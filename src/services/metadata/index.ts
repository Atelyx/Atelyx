/**
 * 元数据双源分发层：`.atelyx` 元数据与配置的读写按激活仓库身份分发。
 *
 * 个人仓库（local）→ 既有本地 Tauri 命令（services/vault 的函数面原样直通）；
 * 协作空间（space）→ 服务端 meta 分组（services/space/client）：
 * - team 层（space meta，团队共享）：排序 `sort` / 排除夹 `exclusions` /
 *   文件夹颜色 `folder-colors` / 提示词标记 `prompt-notes` / Agent 配置 `agents`（后两者只读）；
 * - user 层（meta/me，个人）：对话历史 `chat/messages/<id>`、`chat/sessions/<id>`、
 *   `chat/editor-meta`、日历 `calendar`、待办 `todos/<encodeURIComponent(id)>`；
 * - 供应商/搜索源/默认模型等配置本体 → 本机 global.json `spaceConfigs`（services/global
 *   的 `patchSpaceConfig`，Rust 侧按 serverKey 字段级合并）；API key 仍只进 keychain。
 *
 * 空间写失败与被拒操作（只读团队层写入）都经 cordis access 注入点弹通知，不静默；
 * 配置补丁路径例外——该路径的错误由 settingsStore 的写盘入口统一通知，避免双重弹窗。
 * 读改写场景（对话追加）在写前校验激活身份未变，切换后的在途写直接丢弃。
 */
import { getActiveVaultIdentity, identityKeyOf } from "@/services/content/factory";
import { createSpaceClient, type SpaceClient } from "@/services/space/client";
import { getToken } from "@/services/space/auth";
import {
  patchSpaceConfig,
  readGlobalConfig,
  spaceKey as globalSpaceKey,
} from "@/services/global";
import {
  readVaultConfig as readLocalVaultConfig,
  patchVaultConfig as patchLocalVaultConfig,
  readPromptNotes as readLocalPromptNotes,
  writePromptNotes as writeLocalPromptNotes,
  readAgents as readLocalAgents,
  writeAgents as writeLocalAgents,
  readFolderColors as readLocalFolderColors,
  writeFolderColors as writeLocalFolderColors,
  listChatSessions as listLocalChatSessions,
  readChatSessionMeta as readLocalChatSessionMeta,
  writeChatSessionMeta as writeLocalChatSessionMeta,
  deleteChatSessionMeta as deleteLocalChatSessionMeta,
  readEditorChatsMeta as readLocalEditorChatsMeta,
  writeEditorChatsMeta as writeLocalEditorChatsMeta,
  readChatMessages as readLocalChatMessages,
  writeChatMessages as writeLocalChatMessages,
  appendChatMessages as appendLocalChatMessages,
  deleteChatMessages as deleteLocalChatMessages,
} from "@/services/vault";
import { readVaultFile, writeVaultFile } from "@/services/vault/aiFiles";
import { deleteAttachment } from "@/services/vault";
import {
  CHAT_HISTORY_DIR,
  CHAT_MESSAGE_EXT,
  CHAT_META_EXT,
  EDITOR_CHATS_META_SCHEMA,
} from "@/constants/editorChats";
import { CALENDAR_FILE } from "@/constants/calendar";
import { getPluginNotificationAccess } from "@/services/cordis/access";
import type {
  AgentConfig,
  ChatMetaFile,
  ChatSessionMeta,
  ChatSessionRow,
  EditorChatMessage,
  VaultConfig,
  VaultConfigRead,
} from "@/types";

// ===== 空间 meta 键名（team 层 + user 层）=====

const TEAM_SORT = "sort";
const TEAM_EXCLUSIONS = "exclusions";
const TEAM_FOLDER_COLORS = "folder-colors";
const TEAM_PROMPT_NOTES = "prompt-notes";
const TEAM_AGENTS = "agents";
const MY_CALENDAR = "calendar";
const MY_EDITOR_META = "chat/editor-meta";
const MY_CHAT_MESSAGES_PREFIX = "chat/messages/";
const MY_CHAT_SESSIONS_PREFIX = "chat/sessions/";
const MY_TODOS_PREFIX = "todos/";

/** 本地待办清单目录（与本地对话历史的隐藏目录约定一致，文件树/watcher 均排除）。 */
const TODOS_DIR = ".atelyx/todos";

// ===== 身份与客户端 =====

function spaceIdentity(): { serverUrl: string; spaceId: string } | null {
  const id = getActiveVaultIdentity();
  return id?.kind === "space" ? { serverUrl: id.serverUrl, spaceId: id.spaceId } : null;
}

function clientFor(serverUrl: string): SpaceClient {
  return createSpaceClient(serverUrl, () => getToken(serverUrl));
}

// ===== JSON 与通知辅助 =====

/** JSON 字符串 → 值（缺失/损坏回落默认：手改/传输损坏不阻塞读取，与本地文件读同策略）。 */
function parseJson<T>(raw: string | undefined, fallback: T): T {
  if (raw === undefined) return fallback;
  try {
    return JSON.parse(raw) as T;
  } catch {
    return fallback;
  }
}

let lastNotifiedMessage = "";
let lastNotifiedAt = 0;

/** 空间侧写失败的用户可见通知（同因 10 秒内不重复：防抖重试场景不刷屏）。
 * 经 cordis access 注入点（services 不 import stores）；未接线时仅靠抛错由调用方记日志。 */
function notifySpaceFailure(message: string): void {
  const now = Date.now();
  if (message === lastNotifiedMessage && now - lastNotifiedAt < 60_000) return;
  lastNotifiedMessage = message;
  lastNotifiedAt = now;
  getPluginNotificationAccess()?.notify({ level: "error", message });
}

function errorMessage(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

// ===== 空间 meta 读写原语 =====

async function getTeamValues(identity: { serverUrl: string; spaceId: string }): Promise<Record<string, string>> {
  const values = await clientFor(identity.serverUrl).meta.getSpaceMeta(identity.spaceId);
  return values.values ?? {};
}

async function getMyValues(identity: { serverUrl: string; spaceId: string }): Promise<Record<string, string>> {
  const values = await clientFor(identity.serverUrl).meta.getMyMeta(identity.spaceId);
  return values.values ?? {};
}

/** PATCH 单个 team meta 键（value = null 删除）。失败通知用户后重抛。 */
async function patchTeamValue(
  identity: { serverUrl: string; spaceId: string },
  key: string,
  value: string | null,
): Promise<void> {
  const client = clientFor(identity.serverUrl);
  try {
    if (value === null) {
      await client.meta.deleteSpaceMeta(identity.spaceId, key);
    } else {
      await client.meta.patchSpaceMeta(identity.spaceId, { values: { [key]: value } });
    }
  } catch (e) {
    notifySpaceFailure(`协作空间元数据保存失败：${errorMessage(e)}`);
    throw e;
  }
}

/** PATCH 单个 user meta 键（value = null 删除）。失败通知用户后重抛。 */
async function patchMyValue(
  identity: { serverUrl: string; spaceId: string },
  key: string,
  value: string | null,
): Promise<void> {
  const client = clientFor(identity.serverUrl);
  try {
    if (value === null) {
      await client.meta.deleteMyMeta(identity.spaceId, key);
    } else {
      await client.meta.patchMyMeta(identity.spaceId, { values: { [key]: value } });
    }
  } catch (e) {
    notifySpaceFailure(`协作空间数据保存失败：${errorMessage(e)}`);
    throw e;
  }
}

/** 删除 user meta 键（幂等；键不存在与其他删除失败同语义如实抛出，调用方按幂等兜底）。 */
async function deleteMyValue(
  identity: { serverUrl: string; spaceId: string },
  key: string,
): Promise<void> {
  const client = clientFor(identity.serverUrl);
  try {
    await client.meta.deleteMyMeta(identity.spaceId, key);
  } catch (e) {
    // 服务端对不存在的键返回 404：与本地「删除幂等」契约对齐，视为成功
    const status = (e as { status?: number }).status;
    if (status === 404) return;
    notifySpaceFailure(`协作空间数据删除失败：${errorMessage(e)}`);
    throw e;
  }
}

/** 只读团队层写拒绝：通知可见 + 抛错（提示词/Agent 在空间内由团队统一维护）。 */
function rejectTeamWrite(): never {
  const message = "协作空间内由团队统一维护，暂不可修改";
  getPluginNotificationAccess()?.notify({ level: "warning", message });
  throw new Error(message);
}

// ===== 仓库级配置（providers/model 等本体 + sort/exclusions）=====

/**
 * 读仓库级配置：local = `.atelyx/config.json`；
 * space = 本机 global.json `spaceConfigs`（配置本体，缺失 = 默认配置）+ 服务端 team meta
 * （sort/exclusions 团队共享）合并。corruptBackup 仅本地路径有语义。
 */
export async function readVaultConfig(): Promise<VaultConfigRead> {
  const identity = spaceIdentity();
  if (!identity) return readLocalVaultConfig();
  const { config: global } = await readGlobalConfig();
  const base = (global.spaceConfigs?.[globalSpaceKey(identity.serverUrl, identity.spaceId)] ??
    {}) as VaultConfig;
  const team = await getTeamValues(identity);
  const config: VaultConfig = { ...base };
  if (team[TEAM_SORT] !== undefined) {
    config.fileExplorerSort = parseJson(team[TEAM_SORT], config.fileExplorerSort);
  }
  if (team[TEAM_EXCLUSIONS] !== undefined) {
    config.excludeFolders = parseJson<string[]>(team[TEAM_EXCLUSIONS], []);
  }
  return { config, corruptBackup: null };
}

/**
 * 字段级合并补丁写仓库级配置。local = `vault_config_patch`（Rust 按字段合并）；
 * space 按归属拆分：fileExplorerSort/excludeFolders → team meta（sort/exclusions 键，
 * `null` = 删键），attachmentFolder 空间内不适用（无本地附件目录，静默丢弃），
 * 其余字段 → `space_config_patch`（global.json `spaceConfigs` 按 serverKey 字段级合并）。
 * 返回损坏备份文件名（仅本地路径可能非空；空间配置损坏由 global 读路径备份提示）。
 */
export async function patchVaultConfig(patch: Record<string, unknown>): Promise<string | null> {
  const identity = spaceIdentity();
  if (!identity) return patchLocalVaultConfig(patch);
  const metaValues: Record<string, string> = {};
  const metaDeletes: string[] = [];
  const rest: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(patch)) {
    if (key === "fileExplorerSort") {
      if (value === null) metaDeletes.push(TEAM_SORT);
      else metaValues[TEAM_SORT] = JSON.stringify(value);
    } else if (key === "excludeFolders") {
      if (value === null) metaDeletes.push(TEAM_EXCLUSIONS);
      else metaValues[TEAM_EXCLUSIONS] = JSON.stringify(value);
    } else if (key === "attachmentFolder") {
      // 空间无本地附件目录，该设定不适用：丢弃不报错（读侧同样不会有值）
      continue;
    } else {
      rest[key] = value;
    }
  }
  for (const key of metaDeletes) {
    // 删除走无通知原语：本路径（配置补丁）的错误由 settingsStore 的写盘入口统一通知，
    // patchTeamValue 会再弹一次造成双重通知
    await clientFor(identity.serverUrl).meta.deleteSpaceMeta(identity.spaceId, key);
  }
  if (Object.keys(metaValues).length) {
    await clientFor(identity.serverUrl).meta.patchSpaceMeta(identity.spaceId, { values: metaValues });
  }
  if (Object.keys(rest).length) {
    await patchSpaceConfig(globalSpaceKey(identity.serverUrl, identity.spaceId), rest);
  }
  return null;
}

// ===== 提示词标记 / Agent / 文件夹颜色 =====

/** 读系统提示词标记：local = `.atelyx/prompt-notes.json`；space = team meta `prompt-notes`。 */
export async function readPromptNotes(): Promise<string[]> {
  const identity = spaceIdentity();
  if (!identity) return readLocalPromptNotes();
  const team = await getTeamValues(identity);
  return parseJson<string[]>(team[TEAM_PROMPT_NOTES], []);
}

/** 写系统提示词标记：local 原样；space = 只读团队层，通知并拒绝。 */
export async function writePromptNotes(files: string[]): Promise<void> {
  if (spaceIdentity()) rejectTeamWrite();
  return writeLocalPromptNotes(files);
}

/** 读 Agent 配置：local = `.atelyx/agents.json`；space = team meta `agents`。 */
export async function readAgents(): Promise<AgentConfig[]> {
  const identity = spaceIdentity();
  if (!identity) return readLocalAgents();
  const team = await getTeamValues(identity);
  return parseJson<AgentConfig[]>(team[TEAM_AGENTS], []);
}

/** 写 Agent 配置：local 原样；space = 只读团队层，通知并拒绝。 */
export async function writeAgents(agents: AgentConfig[]): Promise<void> {
  if (spaceIdentity()) rejectTeamWrite();
  return writeLocalAgents(agents);
}

/** 读文件夹颜色映射：local = `.atelyx/folder-colors.json`；space = team meta `folder-colors`。 */
export async function readFolderColors(): Promise<Record<string, string>> {
  const identity = spaceIdentity();
  if (!identity) return readLocalFolderColors();
  const team = await getTeamValues(identity);
  return parseJson<Record<string, string>>(team[TEAM_FOLDER_COLORS], {});
}

/** 写文件夹颜色映射：local 原样；space = team meta（无编辑权限时服务端拒绝，通知可见）。 */
export async function writeFolderColors(colors: Record<string, string>): Promise<void> {
  const identity = spaceIdentity();
  if (!identity) return writeLocalFolderColors(colors);
  await patchTeamValue(identity, TEAM_FOLDER_COLORS, JSON.stringify(colors));
}

// ===== AI 对话面板会话（user meta）=====

/** 从消息 .jsonl 路径取会话 id（与本地 Rust 校验同规则：`.atelyx/对话历史/` 前缀 + `.jsonl` 后缀）。 */
function chatSessionIdFromMessageFile(file: string): string {
  const prefix = `${CHAT_HISTORY_DIR}/`;
  if (!file.startsWith(prefix) || !file.endsWith(CHAT_MESSAGE_EXT)) {
    throw new Error(`非法会话消息路径：${file}`);
  }
  return file.slice(prefix.length, -CHAT_MESSAGE_EXT.length);
}

/** 从侧车路径取会话 id（`.atelyx/对话历史/<id>.meta.json`）。 */
function chatSessionIdFromMetaFile(file: string): string {
  const prefix = `${CHAT_HISTORY_DIR}/`;
  if (!file.startsWith(prefix) || !file.endsWith(CHAT_META_EXT)) {
    throw new Error(`非法会话元数据路径：${file}`);
  }
  return file.slice(prefix.length, -CHAT_META_EXT.length);
}

/** 会话清单：local = 扫 `.atelyx/对话历史/`；space = 扫 user meta 消息键
 * （`chat/messages/<id>` 为存在性真相），`file` 仍按本地路径约定回填，消费方无感。 */
export async function listChatSessions(): Promise<ChatSessionRow[]> {
  const identity = spaceIdentity();
  if (!identity) return listLocalChatSessions();
  const values = await getMyValues(identity);
  const rows: ChatSessionRow[] = [];
  for (const key of Object.keys(values)) {
    if (!key.startsWith(MY_CHAT_MESSAGES_PREFIX)) continue;
    const id = key.slice(MY_CHAT_MESSAGES_PREFIX.length);
    if (!id) continue;
    const meta = parseJson<ChatSessionMeta | null>(
      values[`${MY_CHAT_SESSIONS_PREFIX}${id}`],
      null,
    );
    rows.push({
      id,
      file: `${CHAT_HISTORY_DIR}/${id}${CHAT_MESSAGE_EXT}`,
      meta: meta ?? null,
    });
  }
  rows.sort((a, b) => a.id.localeCompare(b.id));
  return rows;
}

/** 读会话元数据侧车（local 原样；space = user meta `chat/sessions/<id>`，缺失/损坏返回 null）。 */
export async function readChatSessionMeta(file: string): Promise<ChatSessionMeta | null> {
  const identity = spaceIdentity();
  if (!identity) return readLocalChatSessionMeta(file);
  const values = await getMyValues(identity);
  return parseJson<ChatSessionMeta | null>(
    values[`${MY_CHAT_SESSIONS_PREFIX}${chatSessionIdFromMetaFile(file)}`],
    null,
  );
}

/** 写会话元数据侧车（local 原样；space = PATCH user meta `chat/sessions/<id>`）。 */
export async function writeChatSessionMeta(file: string, meta: ChatSessionMeta): Promise<void> {
  const identity = spaceIdentity();
  if (!identity) return writeLocalChatSessionMeta(file, meta);
  await patchMyValue(
    identity,
    `${MY_CHAT_SESSIONS_PREFIX}${chatSessionIdFromMetaFile(file)}`,
    JSON.stringify(meta),
  );
}

/** 删会话元数据侧车（幂等；local 原样；space = 删 user meta 键）。 */
export async function deleteChatSessionMeta(file: string): Promise<void> {
  const identity = spaceIdentity();
  if (!identity) return deleteLocalChatSessionMeta(file);
  await deleteMyValue(identity, `${MY_CHAT_SESSIONS_PREFIX}${chatSessionIdFromMetaFile(file)}`);
}

/** 读面板级覆盖：local = `.atelyx/editor-chats-meta.json`；
 * space = user meta `chat/editor-meta`（schema 不匹配视为空覆盖，与本地读路径同策略）。 */
export async function readEditorChatsMeta(): Promise<ChatMetaFile> {
  const identity = spaceIdentity();
  if (!identity) return readLocalEditorChatsMeta();
  const values = await getMyValues(identity);
  const empty: ChatMetaFile = {
    schema: EDITOR_CHATS_META_SCHEMA,
    modelOverride: null,
    effortOverride: null,
  };
  const parsed = parseJson<ChatMetaFile>(values[MY_EDITOR_META], empty);
  if (parsed.schema !== EDITOR_CHATS_META_SCHEMA) {
    return empty;
  }
  return parsed;
}

/** 写面板级覆盖（local 原样；space = PATCH user meta `chat/editor-meta`）。 */
export async function writeEditorChatsMeta(file: ChatMetaFile): Promise<void> {
  const identity = spaceIdentity();
  if (!identity) return writeLocalEditorChatsMeta(file);
  await patchMyValue(identity, MY_EDITOR_META, JSON.stringify(file));
}

/** 读会话消息正文 JSONL（local 原样；space = user meta `chat/messages/<id>`，缺失报错由调用方降级）。 */
export async function readChatMessages(file: string): Promise<string> {
  const identity = spaceIdentity();
  if (!identity) return readLocalChatMessages(file);
  const values = await getMyValues(identity);
  const key = `${MY_CHAT_MESSAGES_PREFIX}${chatSessionIdFromMessageFile(file)}`;
  if (!(key in values)) {
    throw new Error(`会话消息不存在：${file}`);
  }
  return values[key];
}

/** 写会话消息正文 JSONL 整文件（local 原样；space = PATCH user meta `chat/messages/<id>`）。 */
export async function writeChatMessages(file: string, content: string): Promise<void> {
  const identity = spaceIdentity();
  if (!identity) return writeLocalChatMessages(file, content);
  await patchMyValue(
    identity,
    `${MY_CHAT_MESSAGES_PREFIX}${chatSessionIdFromMessageFile(file)}`,
    content,
  );
}

/**
 * 序列化一条消息记录为紧凑 JSON 行（与本地 .jsonl 行格式同构，读回解析共用一套形状）。
 * 独立于调用方实现：追加是服务端的读改写，无法复用本地命令的 serde 序列化。
 */
function chatRecordLine(m: EditorChatMessage): string {
  return JSON.stringify({
    id: m.id,
    role: m.role,
    content: m.content,
    ...(m.displayContent ? { displayContent: m.displayContent } : {}),
    ...(m.refs?.length ? { refs: m.refs } : {}),
    ...(m.steps?.length ? { steps: m.steps } : {}),
    createdAt: m.createdAt,
  });
}

/**
 * 追加会话消息（local = 真 OS 追加命令；space = 读改写整文件——user meta 单键无追加端点）。
 * 空间路径在写前校验激活身份未变：读在途期间用户切到别的空间/仓库时，在途追加直接丢弃
 * （内存会话已随切换重载，旧空间的追加内容会被下次写盘全量收敛，不丢用户可见消息）。
 * 消息文件缺失报错（与本地同语义：调用方回落全量重写重建历史）。
 */
export async function appendChatMessages(file: string, records: EditorChatMessage[]): Promise<void> {
  const identity = spaceIdentity();
  if (!identity) return appendLocalChatMessages(file, records);
  const identityAtStart = identityKeyOf(getActiveVaultIdentity());
  const existing = await readChatMessages(file).catch(() => null);
  if (existing === null) {
    throw new Error("会话消息文件缺失，请重写");
  }
  if (identityKeyOf(getActiveVaultIdentity()) !== identityAtStart) return;
  const needsSep = existing.length > 0 && !existing.endsWith("\n");
  const next = `${existing}${needsSep ? "\n" : ""}${records.map(chatRecordLine).join("\n")}\n`;
  await patchMyValue(identity, `${MY_CHAT_MESSAGES_PREFIX}${chatSessionIdFromMessageFile(file)}`, next);
}

/** 删会话消息正文（幂等；local 原样；space = 删 user meta 键）。 */
export async function deleteChatMessages(file: string): Promise<void> {
  const identity = spaceIdentity();
  if (!identity) return deleteLocalChatMessages(file);
  await deleteMyValue(identity, `${MY_CHAT_MESSAGES_PREFIX}${chatSessionIdFromMessageFile(file)}`);
}

// ===== 日历（user meta `calendar`）=====

/** 读日历日程 JSON 原文：local = `.atelyx/calendar.json`（缺失/读失败 = null，调用方降级空日程）；
 * space = user meta `calendar`（缺失 = null）。 */
export async function readCalendarRaw(): Promise<string | null> {
  const identity = spaceIdentity();
  if (!identity) {
    try {
      return await readVaultFile(CALENDAR_FILE);
    } catch {
      return null;
    }
  }
  const values = await getMyValues(identity);
  return values[MY_CALENDAR] ?? null;
}

/** 写日历日程 JSON 原文（local = `.atelyx/calendar.json` 原子写；space = PATCH user meta `calendar`）。 */
export async function writeCalendarRaw(content: string): Promise<void> {
  const identity = spaceIdentity();
  if (!identity) return writeVaultFile(CALENDAR_FILE, content);
  await patchMyValue(identity, MY_CALENDAR, content);
}

// ===== AI 任务清单（user meta `todos/<id>`）=====

function todosPathFor(id: string): string {
  return `${TODOS_DIR}/${encodeURIComponent(id)}.json`;
}

/** 读某会话/画布对话的任务清单 JSON 原文：local = `.atelyx/todos/<id>.json`；
 * space = user meta `todos/<encodeURIComponent(id)>`。缺失/读失败 = null（尽力而为）。 */
export async function readSessionTodosRaw(id: string): Promise<string | null> {
  const identity = spaceIdentity();
  if (!identity) {
    try {
      return await readVaultFile(todosPathFor(id));
    } catch {
      return null;
    }
  }
  const values = await getMyValues(identity);
  return values[`${MY_TODOS_PREFIX}${encodeURIComponent(id)}`] ?? null;
}

/** 整单替换写任务清单 JSON 原文（local 原子写；space = PATCH user meta）。 */
export async function writeSessionTodosRaw(id: string, content: string): Promise<void> {
  const identity = spaceIdentity();
  if (!identity) return writeVaultFile(todosPathFor(id), content);
  await patchMyValue(identity, `${MY_TODOS_PREFIX}${encodeURIComponent(id)}`, content);
}

/** 删任务清单侧车（幂等；local = 删仓库内文件；space = 删 user meta 键）。 */
export async function deleteSessionTodosRaw(id: string): Promise<void> {
  const identity = spaceIdentity();
  if (!identity) return deleteAttachment(todosPathFor(id));
  await deleteMyValue(identity, `${MY_TODOS_PREFIX}${encodeURIComponent(id)}`);
}
