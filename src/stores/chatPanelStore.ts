import { create } from "zustand";
import {
  listChatSessions,
  readChatSessionMeta,
  writeChatSessionMeta,
  deleteChatSessionMeta,
  readEditorChatsMeta,
  writeEditorChatsMeta,
  readChatMessages,
  writeChatMessages,
  appendChatMessages,
  deleteChatMessages,
} from "@/services/vault";
import { toLlmMessages } from "@/services/ai/client";
import { abortAutoTitle } from "@/services/ai/autoTitle";
import { emitPluginEvent } from "@/services/cordis/events";
import { ERROR_PREFIX, TIMEOUT_ERROR_TEXT } from "@/constants/chat";
import { BUILTIN_AGENT_CHAT_ID } from "@/constants/agents";
import {
  runStreamExchange,
  decideCleanup,
  runAutoNaming,
} from "./streaming";
import { runAgentTools, assembleAgentSystemPrompt } from "@/services/ai/tools";
import { runSearch } from "@/services/search";
import { readHistoryForAgent, recordAgentFileWrite } from "@/services/history";
import {
  appendVaultFile,
  editVaultFile,
  globVault,
  grepVault,
  listVaultDir,
  readVaultFileWindow,
  writeVaultFile,
} from "@/services/vault/aiFiles";
import {
  currentTodosBlock,
  deleteAgentTodos,
  readAgentTodos,
  writeAgentTodos,
} from "@/services/vault/agentTodos";
import { fetchWeb } from "@/services/web";
import { prefix, scanMentionHits } from "@/utils/text";
import { appendNarration, appendReasoning, coalesceAgentSteps, fillAssistantReplyText, finalizeReplyText, mergeToolRuns } from "@/utils/agentSteps";
import { createPersistController } from "@/utils/persist";
import { useSettingsStore } from "./settingsStore";
import { useAppStore } from "./appStore";
import { useVaultStore } from "./vaultStore";
import {
  EDITOR_CHATS_META_SCHEMA,
  CHAT_HISTORY_DIR,
  CHAT_MESSAGE_EXT,
  CHAT_META_EXT,
} from "@/constants/editorChats";
import type {
  EditorChatMessage,
  EditorChatMessageRef,
  EditorChatModelOverride,
  EditorChatSession,
  ChatMetaFile,
  NoteRewriteRequest,
  ProviderConfig,
  ReasoningEffort,
  ToolSchema,
} from "@/types";

/**
 * AI 对话面板会话状态。
 *
 * 单一全局历史：以 `.atelyx/对话历史/` 文件夹为真相——每会话一个消息 `.jsonl`
 * （JSON Lines：一行一条消息记录，追加式写）+ 可选 `.meta.json` 元数据侧车（title/agentId）；
 * 会话清单 = 扫目录（无整文件索引），切换笔记不切换会话；面板级覆盖存 `.atelyx/editor-chats-meta.json`。
 * 笔记上下文两条路径：@引用（手动拖入，发送时就地替换注入）+ 当前打开笔记尾部上下文块（runExchange 注入，ephemeral 不落盘）。
 * 多设备共享同一仓库文件夹时，新建/删除/改名/消息经 watcher 内容比对合并实时互见（见 applyExternalChatChange）。
 *
 * 与画布对话（canvasStore.runStream）的差异：
 * - 面板一次只流式一个会话（单输入框）；工具循环与画布共用 runStreamExchange（Agent 模式开关控制）
 * - 错误占位沿用 `[错误]` 前缀过滤约定（ERROR_PREFIX，见 constants/chat.ts）
 * - provider/model 解析：`settingsStore.resolveChatTarget(modelOverride)`（面板覆盖 → 跟随仓库默认，与画布同源）
 * - 持久化 debounce 500ms：消息纯增长只追加新增记录（每记录一行 JSON），元数据/覆盖变化写对应小文件；
 *   自写与外部写经 watcher 内容比对判别（无时间窗误判）
 */

interface ChatPanelState {
  sessions: EditorChatSession[];
  activeSessionId: string | null;
  /** 面板是否正在流式回复（全局单一，与当前激活会话对应）。 */
  streaming: boolean;
  /** 面板级模型覆盖（优先于仓库默认模型；null = 跟随仓库默认）。 */
  modelOverride: EditorChatModelOverride | null;
  /** 面板级推理等级覆盖（null = 不指定/跟随默认；与模型覆盖正交，跟随仓库默认时也可单独设置；持久化 editor-chats.json）。 */
  effortOverride: ReasoningEffort | null;
  /** 拖入输入框的笔记引用队列（文件面板拖拽笔记到 AI 对话输入框，组件消费后清空）。 */
  pendingMentions: EditorChatMessageRef[];
  /** 新对话态（无激活会话）的待用 Agent：默认预置「对话」，发送首条消息创建会话时固化进会话；新建会话/切仓库时重置为默认。 */
  draftAgentId: string | undefined;
  /** 笔记划词改写请求队列（NoteEditor 划词右键确认后入队；AiChatPanel 消费后清空）。 */
  pendingRewrites: NoteRewriteRequest[];
  /** 面板内联错误提示（未配置模型/发送失败等）。 */
  error: string | null;
  loaded: boolean;
  /** 当前内存会话所属的仓库 ID（load 时记录；flush 写盘前校验归属，防跨仓库搞混）。 */
  sessionVaultId: string | null;

  /** 进仓库时加载：读盘历史会话 + 进入新对话态（默认显示新的空对话，不恢复上次激活会话）。vaultId = 当前仓库稳定 ID。
   * `force`：真实仓库切换时传 true——绕过「已加载该仓库」幂等守卫强制重读盘（防 sessionVaultId 巧合等于目标时把切换当冗余跳过，面板停留在旧会话）。 */
  load: (vaultId: string | null, force?: boolean) => Promise<void>;
  /** 切到新对话态（activeSessionId = null，不创建空会话对象）——发送首条消息时才真正创建会话。 */
  newSession: () => void;
  /** 切换到历史会话（内存 updatedAt 置顶排序；「最近使用」不持久化，重启后按最近对话排序）。 */
  openSession: (id: string) => void;
  /** 删除会话（删的是当前激活会话时回落新对话态；同时删其消息 .jsonl 与元数据侧车，删除经 watcher 跨设备传播）。 */
  deleteSession: (id: string) => void;
  /** 发送消息到当前激活会话（refs = 输入框内的 @引用笔记，发送时就地替换注入笔记全文）。 */
  send: (content: string, refs?: EditorChatMessageRef[]) => Promise<void>;
  /** 重新生成最后一条回复：移除最后 assistant、按 refs 重建最后一条 user 消息注入后重发（同画布 regenerate 语义）。 */
  regenerate: () => Promise<void>;
  /** 手动重新命名当前会话（按全部会话记录请求命名，立即发出无防限流延迟；失败 error 提示）。 */
  renameSession: () => Promise<void>;
  /** 回到此处：截断到指定 AI 回复（含），之后的消息移除，在此处继续对话。 */
  rollbackTo: (messageId: string) => void;
  /** 中止当前流式回复（空回复自动移除占位）。 */
  stop: () => void;
  /** 拖入的笔记引用入队（FileExplorerPanel 拖拽笔记到 AI 对话输入框时调用）。 */
  queueMention: (ref: EditorChatMessageRef) => void;
  /** 清空待消费的笔记引用队列（AiChatPanel 消费后调用）。 */
  clearPendingMentions: () => void;
  /** 笔记划词改写请求入队（NoteEditor 划词右键确认后调用）。 */
  queueNoteRewrite: (req: NoteRewriteRequest) => void;
  /** 清空待消费的划词改写队列（AiChatPanel 消费后调用）。 */
  clearPendingRewrites: () => void;
  /** 设置面板当前 Agent（undefined = 缺省「对话」）：有激活会话写会话元数据侧车并持久化；新对话态存 draft，发送首条消息时固化。 */
  setAgentId: (id: string | undefined) => void;
  /** 设置面板级模型覆盖（null = 跟随仓库默认；持久化 editor-chats-meta.json）。 */
  setModelOverride: (ov: EditorChatModelOverride | null) => void;
  /** 设置面板级推理等级覆盖（null = 不指定/跟随默认；持久化 editor-chats-meta.json）。 */
  setEffortOverride: (effort: ReasoningEffort | null) => void;
  /** 清除面板内联错误。 */
  clearError: () => void;
  /** watcher 收到 `.atelyx/对话历史/` 文件事件：内容比对合并（新会话/新消息/改名/删除跨设备实时互见；幂等、不置脏）。 */
  applyExternalChatChange: (file: string) => void;
  /** 立即落盘并返回写盘 Promise（可等待——切换仓库前必须先等旧会话写完，防写进新仓库）。
   * `vaultId` = 期望写入的仓库 ID：与内存会话所属仓库（sessionVaultId）不匹配则跳过（防跨仓库污染）。
   * 无本地改动（dirty=false）也跳过（外部删除会话文件后切仓库不写回覆盖）。 */
  flush: (vaultId: string | null) => Promise<void>;
}

let abortController: AbortController | null = null;
/** 会话是否有本地改动（新建/切换/删除/发送/设置变化置 true；写盘成功后清）。
 * 脏门控：未改动不写盘——外部删除会话文件后切仓库，flush 不再把内存副本写回（覆盖删除）。 */
let dirty = false;
/** 需要重写消息 .jsonl 的会话 id 集合（发送/流式结束时标记；persistNow 统一写盘后清空）。 */
const dirtyMessageFiles = new Set<string>();
/** 需要重写元数据侧车（.meta.json：title/agentId）的会话 id 集合（改名/换 Agent 时标记）。 */
const dirtyMetaSessions = new Set<string>();
/** 面板级覆盖（editor-chats-meta.json）是否有本地改动（setModelOverride/setEffortOverride 标记）。 */
let overridesDirty = false;
/**
 * 各会话消息 .jsonl 的追加式基线：上次写盘时的消息数组引用。
 * 纯增长（旧消息引用逐一相同）→ 只追加新增记录（省全量重拼与 IPC 载荷）；
 * 流式中途落盘（消息引用变化/截断）→ 全量重写（幂等）。基线只在写成功后推进，
 * 失败清除——下次重试全量重写，防追加重复。外部合并（applyExternalMessages）后清除，
 * 下次写盘全量重写收敛（防追加丢对端已落盘内容）。
 */
const messageBaseline = new Map<string, EditorChatMessage[]>();

/** 防抖持久化控制器：timer 管理 + 代数防吞统一在此；extra = flush 传入的期望仓库 ID（定时写盘不传）。 */
const persistCtl = createPersistController<string | null>({
  persist: persistNow,
});

// ===== 会话消息正文 + 元数据侧车（.atelyx/对话历史/<会话 id>.jsonl|.meta.json）=====

/** 会话消息正文 .jsonl 相对路径（文件名 = 会话 id：LLM 自动命名改标题不影响文件名，无需改名）。 */
function chatMessageFilePath(sessionId: string): string {
  return `${CHAT_HISTORY_DIR}/${sessionId}${CHAT_MESSAGE_EXT}`;
}

/** 会话元数据侧车 .meta.json 相对路径（文件名 = 会话 id）。 */
function chatMetaFilePath(sessionId: string): string {
  return `${CHAT_HISTORY_DIR}/${sessionId}${CHAT_META_EXT}`;
}

/**
 * 序列化会话消息 → JSONL 文本（一行一条消息记录，紧凑 JSON）。
 * 只写持久化字段：id/createdAt 稳定持久化，refs（@引用）/steps（含工具步）结构化持久化，
 * 重开会话完整恢复。
 */
function serializeChatMessages(messages: EditorChatMessage[]): string {
  return messages
    .map((m) =>
      JSON.stringify({
        id: m.id,
        role: m.role,
        content: m.content,
        ...(m.displayContent ? { displayContent: m.displayContent } : {}),
        ...(m.refs?.length ? { refs: m.refs } : {}),
        ...(m.steps?.length ? { steps: m.steps } : {}),
        createdAt: m.createdAt,
      })
    )
    .join("\n");
}

/**
 * 解析会话消息 .jsonl → 消息（逐行 JSON.parse，损坏行跳过——降级不阻塞会话恢复）。
 * id/createdAt/refs/steps 直接用存储值（消息 .jsonl 是记录而非转写，恢复不重新生成 id）。
 */
function parseChatMessages(jsonl: string): EditorChatMessage[] {
  const messages: EditorChatMessage[] = [];
  for (const line of jsonl.split("\n")) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    try {
      const raw = JSON.parse(trimmed) as Partial<EditorChatMessage>;
      if (typeof raw.role !== "string" || typeof raw.content !== "string") continue;
      messages.push({
        id: typeof raw.id === "string" ? raw.id : crypto.randomUUID(),
        role: raw.role as EditorChatMessage["role"],
        content: raw.content,
        ...(typeof raw.displayContent === "string" ? { displayContent: raw.displayContent } : {}),
        ...(Array.isArray(raw.refs) ? { refs: raw.refs } : {}),
        ...(Array.isArray(raw.steps) ? { steps: coalesceAgentSteps(raw.steps) } : {}),
        ...(typeof raw.createdAt === "number"
          ? { createdAt: raw.createdAt }
          : { createdAt: messages.length }),
      });
    } catch {
      // 损坏行：跳过（降级不阻塞会话恢复）
    }
  }
  return messages;
}

/** 已完成 LLM 自动命名的会话 id（一次会话只命名一次；load 切仓库时清空）。 */
const autoNamedSessions = new Set<string>();

/**
 * @引用 注入（send/regenerate 共用）：.md 引用只发文件路径——@标签 文本保留原位，
 * 消息开头拼「引用文件」路径块（模型用 read_file 按需读取正文，不把笔记全文打进每条消息）。
 * @标签 被手动删掉时跳过（扫描不到标签 = 该引用下沉丢弃，不记 refs）。
 */
async function injectNoteRefs(
  text: string,
  refs: { file: string; label: string }[],
): Promise<{ text: string; injectedFiles: string[] }> {
  const injectedFiles: string[] = [];
  if (!refs.length) return { text, injectedFiles };
  const hits = scanMentionHits(
    text,
    refs.map((r) => ({ nodeId: r.file, text: `@${r.label}` })),
  );
  const hitFiles = new Set(hits.map((h) => h.mention.nodeId));
  // 按 @标签 出现顺序去重（同文件重复引用只出一条路径）
  const seen = new Set<string>();
  const active: { file: string; label: string }[] = [];
  for (const r of refs) {
    if (hitFiles.has(r.file) && !seen.has(r.file)) {
      seen.add(r.file);
      active.push(r);
    }
  }
  if (!active.length) return { text, injectedFiles };
  // 目录引用加 / 后缀标注（引导见 FILE_REFERENCE_PROMPT 的目录句）；树中不存在按文件原样
  const pathKind = useVaultStore.getState().pathKind;
  const fileBlock = `[引用文件：\n${active
    .map((r) => (pathKind(r.file) === "dir" ? `${r.file}/` : r.file))
    .join("\n")}]\n\n`;
  injectedFiles.push(...active.map((r) => r.file));
  return { text: `${fileBlock}${text}`, injectedFiles };
}

/** 命名成功写回：登记防重复命名 + 更新会话 title + 落盘元数据侧车（自动命名与重新命名共用）。 */
function applySessionTitle(sessionId: string, title: string): void {
  const latest = useChatPanelStore.getState();
  if (!latest.sessions.some((x) => x.id === sessionId)) return;
  // 成功才登记：失败下轮重试；成功后消息 .jsonl 文件名 = 会话 id，不随标题变——命名只改侧车 title（随既有 debounce 写盘）
  autoNamedSessions.add(sessionId);
  useChatPanelStore.setState({
    sessions: latest.sessions.map((x) => (x.id === sessionId ? { ...x, title } : x)),
  });
  markMetaDirty(sessionId);
}

/**
 * LLM 话题自动命名：一轮对话完成后为会话生成话题标题。
 * - 统一走 streaming.ts 的公共命名管线（模型解析/延迟/超时与画布共用）
 * - 失败（含被 abortAutoTitle 中止）不登记——降级保留首条消息前缀，下轮对话完成/进入仓库时自动重试
 * - 命名只改侧车 title（消息 .jsonl 文件名 = 会话 id，不随标题变）
 * - fire-and-forget：关闭/无可用模型/命名失败降级保留占位标题，不阻塞对话
 */
async function autoNameSession(sessionId: string): Promise<void> {
  await runAutoNaming(
    {
      getMessages: () => {
        const s = useChatPanelStore.getState().sessions.find((x) => x.id === sessionId);
        // 叙述-only 消息（content 为空、正文在 steps）回填正文，供话题命名摘要
        return (s?.messages ?? []).map(fillAssistantReplyText);
      },
      isNamed: () => autoNamedSessions.has(sessionId),
      applyTitle: (title) => applySessionTitle(sessionId, title),
    },
    // key = 会话 id：发送新消息/手动接管时只中止本会话的命名请求（不误伤其他会话）
    { key: sessionId },
  );
}

/** debounce 500ms 写盘（读最新 state；`messageSessionId` = 本次改动涉及的会话，其消息 .jsonl 需重写）。 */
function schedulePersist(messageSessionId?: string) {
  if (messageSessionId) dirtyMessageFiles.add(messageSessionId);
  dirty = true;
  persistCtl.schedule();
}

/** 会话元数据侧车（title/agentId）脏标记 + 调度写盘。 */
function markMetaDirty(sessionId: string) {
  dirtyMetaSessions.add(sessionId);
  dirty = true;
  persistCtl.schedule();
}

/** 面板级覆盖（editor-chats-meta.json）脏标记 + 调度写盘。 */
function markOverridesDirty() {
  overridesDirty = true;
  dirty = true;
  persistCtl.schedule();
}

/**
 * 写盘。`guardVaultId`：切仓库前 flush 传入的「期望仓库 ID」——
 * 内存会话属于其他仓库时（sessionVaultId 不匹配）绝不写，防跨仓库污染。
 * 定时写盘不传（sessionVaultId 随 load 已同步为当前仓库）。
 */
async function persistNow(guardVaultId?: string | null): Promise<void> {
  const versionAtStart = persistCtl.version;
  // 守卫：load 完成前（loaded=false，store 仍是初始空态）不落盘——
  // React 18 StrictMode 开发模式双挂载会在 load 完成前触发卸载 flush，
  // 若此时写盘会用空 sessions 覆盖磁盘真实历史（实测：退出重进历史丢失）
  if (!useChatPanelStore.getState().loaded) return;
  // 仓库归属校验：flush 传入的期望仓库 ≠ 内存会话所属仓库 → 不写（防跨仓库搞混）
  if (
    guardVaultId !== undefined &&
    guardVaultId !== useChatPanelStore.getState().sessionVaultId
  ) {
    return;
  }
  const { sessions, modelOverride, effortOverride } = useChatPanelStore.getState();
  // 1) 写脏会话的消息 .jsonl。写成功才移除——失败保留待下次 debounce/flush 重试（防消息只存在于内存而 .jsonl 丢失）；
  //    写盘期间并发 schedulePersist 新标记的会话不在本次快照，保留由下一轮再写（防误清）。
  //    追加式：纯增长只追加新增记录（基线引用逐一相同）；流式中途落盘/截断/基线缺失 → 全量重写（幂等）。
  const pendingIds = [...dirtyMessageFiles];
  await Promise.all(
    pendingIds.map(async (id) => {
      const s = sessions.find((x) => x.id === id);
      if (!s) {
        // 会话已删：删除路径已处理 .jsonl，仅清标记与基线
        dirtyMessageFiles.delete(id);
        messageBaseline.delete(id);
        return;
      }
      const baseline = messageBaseline.get(id);
      try {
        if (
          baseline !== undefined &&
          s.messages.length > baseline.length &&
          baseline.every((m, i) => s.messages[i] === m)
        ) {
          // 纯增长（旧消息引用逐一相同）：只追加新增消息记录（每记录一行 JSON），省全量重拼与 IPC 载荷
          await appendChatMessages(s.file, s.messages.slice(baseline.length));
        } else {
          // 截断/流式中途落盘/基线缺失：全量重写（幂等）
          await writeChatMessages(s.file, serializeChatMessages(s.messages));
        }
        // 基线只在写成功后推进：失败时保留旧值（文件未变），下次重试仍从旧基线追加；
        // 流式中途的全量重写同样以当前数组为基线（最终内容由 onDone 保存覆盖）
        messageBaseline.set(id, s.messages);
        dirtyMessageFiles.delete(id);
      } catch {
        // 追加失败（含外部删文件导致文件缺失）：回落全量重写（幂等，重建历史/防追加重复）；
        // 仍失败保留脏待下次重试 + 基线清除
        try {
          await writeChatMessages(s.file, serializeChatMessages(s.messages));
          messageBaseline.set(id, s.messages);
          dirtyMessageFiles.delete(id);
        } catch (e2) {
          messageBaseline.delete(id);
          console.error("保存会话消息失败", e2);
        }
      }
    }),
  );
  // 2) 写脏会话的元数据侧车（.meta.json：title/agentId）。写成功才移除——失败保留待下次重试。
  //    无整文件索引：多设备并发写互不覆盖，其余设备经 watcher 内容比对合并实时互见。
  const pendingMeta = [...dirtyMetaSessions];
  await Promise.all(
    pendingMeta.map(async (id) => {
      const s = sessions.find((x) => x.id === id);
      if (!s) {
        // 会话已删：删除路径已处理侧车，仅清标记
        dirtyMetaSessions.delete(id);
        return;
      }
      try {
        await writeChatSessionMeta(chatMetaFilePath(id), {
          id: s.id,
          ...(s.title !== undefined ? { title: s.title } : {}),
          ...(s.agentId !== undefined ? { agentId: s.agentId } : {}),
        });
        dirtyMetaSessions.delete(id);
      } catch (e) {
        console.error("保存会话元数据失败", e);
      }
    }),
  );
  // 3) 面板级覆盖变化时写 .atelyx/editor-chats-meta.json（设备偏好，不跨设备传播；写成功才清标记）
  if (overridesDirty) {
    const metaFile: ChatMetaFile = {
      schema: EDITOR_CHATS_META_SCHEMA,
      modelOverride,
      effortOverride,
    };
    try {
      await writeEditorChatsMeta(metaFile);
      overridesDirty = false;
    } catch (e) {
      console.error("保存面板覆盖失败", e);
    }
  }
  // 写盘期间若又有新变更（schedule 已置 dirty + 挂新 timer），保留 dirty 由下一轮再写，
  // 防成功回调吞掉新编辑（消息/侧车/覆盖各有脏集合保护，dirty 仅作 flush 总门）
  if (persistCtl.version === versionAtStart) dirty = false;
}

/**
 * watcher 消息事件（`.atelyx/对话历史/<id>.jsonl`）：内容比对合并——
 * 磁盘含内存未知消息 id → 并入（磁盘顺序为基底、内存独有消息补尾部）+ 清基线（下次全量重写收敛）；
 * 磁盘 ⊆ 内存 → 自写回波，跳过；文件已删 → 移除会话（删除跨设备传播，本端进行中/未落盘会话保留）；
 * 会话不在内存 → 新会话（读侧车元数据后加入）。
 */
async function applyExternalMessages(id: string, file: string): Promise<void> {
  const jsonl = await readChatMessages(file).catch(() => null);
  const state = useChatPanelStore.getState();
  const idx = state.sessions.findIndex((s) => s.id === id);
  if (jsonl === null) {
    // 文件已删（外部删除）：本端进行中（流式）/未落盘（脏消息）的会话保留，防误删本地工作；
    // 其余移除——删除经此跨设备传播
    if (idx >= 0) {
      const active = state.activeSessionId;
      if ((state.streaming && active === id) || dirtyMessageFiles.has(id)) return;
      dirtyMetaSessions.delete(id);
      messageBaseline.delete(id);
      // 函数式更新：防与其他 watcher 事件的并发 setState 相互覆盖（丢弃另一事件的合并）
      useChatPanelStore.setState((st) => ({
        sessions: st.sessions.filter((x) => x.id !== id),
      }));
    }
    return;
  }
  const diskMessages = parseChatMessages(jsonl);
  if (idx < 0) {
    // 新会话：读侧车元数据（失败缺省）后加入内存（不置脏——磁盘已是权威，不写回）
    const meta = await readChatSessionMeta(chatMetaFilePath(id)).catch(() => null);
    messageBaseline.set(id, diskMessages);
    useChatPanelStore.setState((st) => ({
      sessions: [
        ...st.sessions,
        {
          id,
          ...(meta?.title !== undefined ? { title: meta.title } : {}),
          ...(meta?.agentId !== undefined ? { agentId: meta.agentId } : {}),
          file,
          createdAt: diskMessages[0]?.createdAt ?? 0,
          updatedAt: diskMessages[diskMessages.length - 1]?.createdAt ?? 0,
          messages: diskMessages,
        },
      ],
    }));
    return;
  }
  const current = state.sessions[idx];
  const hasNew = diskMessages.some(
    (m) => !current.messages.some((cm) => cm.id === m.id),
  );
  if (!hasNew) return; // 自写回波或磁盘 ⊆ 内存：无新内容
  const diskIds = new Set(diskMessages.map((m) => m.id));
  // 并入对端消息后基线失效：下次写盘全量重写，防追加丢对端内容
  messageBaseline.delete(id);
  // 函数式更新 + 最新内存作基底：防与其他 watcher 事件/流式的并发 setState 互相覆盖——
  // 内存独有消息必须从最新 state 取，否则会把等待期间新流出的 token 一并丢掉
  useChatPanelStore.setState((st) => {
    const cur = st.sessions.find((s) => s.id === id);
    if (!cur) return {};
    const merged = [
      ...diskMessages,
      ...cur.messages.filter((m) => !diskIds.has(m.id)),
    ];
    return {
      sessions: st.sessions.map((s) =>
        s.id === id
          ? {
              ...s,
              messages: merged,
              updatedAt: merged[merged.length - 1]?.createdAt ?? s.updatedAt,
            }
          : s
      ),
    };
  });
}

/** watcher 元数据事件（`.atelyx/对话历史/<id>.meta.json`）：磁盘侧车 title/agentId 并入内存（会话级 LWW，最后写者胜）。 */
async function applyExternalMeta(id: string): Promise<void> {
  const meta = await readChatSessionMeta(chatMetaFilePath(id)).catch(() => null);
  if (!meta || meta.id !== id) return;
  const state = useChatPanelStore.getState();
  if (!state.sessions.some((s) => s.id === id)) return; // .jsonl 事件会负责加会话
  useChatPanelStore.setState((st) => ({
    sessions: st.sessions.map((s) =>
      s.id === id
        ? {
            ...s,
            ...(meta.title !== undefined ? { title: meta.title } : {}),
            ...(meta.agentId !== undefined ? { agentId: meta.agentId } : {}),
          }
        : s
    ),
  }));
}

/**
 * 解析当前对话的 provider/model（画布/面板同源实现，见 settingsStore.resolveChatTarget）：
 * - 面板覆盖 {providerId, model} → 跟随仓库默认（默认模型反查所属供应商）
 * 覆盖供应商已删：清空失效覆盖（含持久化）并提示，本次不发送（不静默回落默认）。
 * 失败已写 error，返回 null。
 */
function resolveProviderModel(): {
  provider: ProviderConfig;
  model: string;
  reasoningEffort?: ReasoningEffort;
} | null {
  const ov = useChatPanelStore.getState().modelOverride;
  const resolved = useSettingsStore.getState().resolveChatTarget(ov);
  if (!resolved.ok) {
    if (resolved.reason === "provider-missing") {
      // provider-missing 只可能在 ov 非空时返回（见 resolveChatTarget）；清空失效覆盖并说明已恢复跟随默认
      useChatPanelStore.getState().setModelOverride(null);
      useChatPanelStore.setState({ error: `${resolved.error}（已恢复跟随默认）` });
      return null;
    }
    useChatPanelStore.setState({ error: resolved.error });
    return null;
  }
  // 推理等级为面板级独立覆盖（与模型覆盖正交）；缺省 = 不指定（跟随默认，不下发 reasoning_effort）
  return {
    provider: resolved.provider,
    model: resolved.model,
    reasoningEffort: useChatPanelStore.getState().effortOverride ?? undefined,
  };
}

/**
 * 当前打开笔记的尾部上下文块：随请求折叠进末条 user 消息线文（ephemeral，不入会话存储）。
 * 引导模型视相关性用 read_file 读取（read_file 随 Agent 勾选在名册内时生效；关闭后模型无读取能力）。
 */
function currentNoteContextBlock(file: string, title: string): string {
  const label = title ? `（${title}）` : "";
  return [
    "<context>",
    `用户当前打开的笔记：\`${file}\`${label}。若与本次对话相关，用 read_file 工具按此路径读取正文；读取之前不要声称已查看过该文件。`,
    "</context>",
  ].join("\n");
}

/**
 * 执行一轮流式对话（send 与 regenerate 共用）：
 * 追加 user 消息 + 空占位 assistant → SSE 流式写入（content/思考双通道 rAF 合并）→ 清理。
 * 空闲超时/错误占位/空回复移除与画布 runStream 语义一致。
 */
async function runExchange(
  active: EditorChatSession,
  userMsg: EditorChatMessage,
  provider: ProviderConfig,
  model: string,
  reasoningEffort?: ReasoningEffort,
): Promise<void> {
  const now = Date.now();
  const title = active.title ?? prefix(userMsg.displayContent ?? userMsg.content, 16);
  const asstMsg: EditorChatMessage = {
    id: crypto.randomUUID(),
    role: "assistant",
    content: "",
    createdAt: now + 1,
  };
  const updated: EditorChatSession = {
    ...active,
    title,
    messages: [...active.messages, userMsg, asstMsg],
    updatedAt: now + 1,
  };
  useChatPanelStore.setState({
    sessions: useChatPanelStore.getState().sessions.map((s) =>
      s.id === active.id ? updated : s
    ),
    streaming: true,
    error: null,
  });
  // 领域事件（状态提交后发出，订阅方读到的是已含本 user 消息的最新会话）：会话请求开始 + 用户消息。
  emitPluginEvent("chat:started", { sessionId: active.id });
  emitPluginEvent("chat:message", { sessionId: active.id, role: "user", content: userMsg.displayContent ?? userMsg.content });
  schedulePersist(active.id);

  const controller = new AbortController();
  abortController = controller;

  // 系统提示词 + 工具：按 Agent 实时解析（配置在 设置 → Agent，引用已注册提示词笔记实时读正文注入）。
  // 缺省（未选 Agent）= 预置「对话」；Agent 缺失（已删）降级为普通对话。
  const agentReq = await useSettingsStore
    .getState()
    .resolveAgentRequest(updated.agentId);
  let systemPrompt: string | undefined;
  let tools: ToolSchema[] = [];
  if (agentReq) {
    systemPrompt = agentReq.systemPrompt;
    tools = agentReq.tools;
    if (agentReq.skippedWebSearch) {
      useChatPanelStore.setState({
        error: "未配置搜索源（设置 → 联网搜索），本次对话未启用联网搜索",
      });
    }
  }

  // 历史含刚追加的 user 消息；叙述-only 消息（content 为空、正文在 steps）先回填 content
  // （否则空 content 发给部分端点返回 400）；过滤错误占位防污染上下文，system 提示词置首
  // （与画布 runStream 同语义）
  const apiHistory = [...active.messages, userMsg]
    .map(fillAssistantReplyText)
    .filter(
      (m) => !(m.role === "assistant" && m.content.startsWith(ERROR_PREFIX)),
    );

  // 系统提示词：Agent 提示词 + 引用文件读取引导（工具含 read_file 时追加「@引用 文件用 read_file 读取」）
  // 易变上下文（当前笔记/任务清单）走尾部 user 消息块，不进系统提示词（保前缀缓存命中）
  const systemText = assembleAgentSystemPrompt(systemPrompt, tools);

  const apiMessages = [
    ...(systemText ? [{ role: "system" as const, text: systemText }] : []),
    ...toLlmMessages(apiHistory),
  ];
  // 当前打开笔记以尾部上下文块折叠进末条 user 消息线文：仅当名册含 read_file 时才注入
  // （模型才有能力读取，避免教唆用不存在的工具）；块不落历史（ephemeral），历史逐字节稳定
  // 复现 → 前缀缓存命中至该消息原文，仅块本身 token 不命中；regenerate 同经此处 → 每次请求按当下打开
  // 的笔记注入（无笔记则按条件不注入）。
  const { currentNoteFile, currentNoteTitle } = useAppStore.getState();
  if (currentNoteFile && tools.some((t) => t.name === "read_file")) {
    const last = apiMessages[apiMessages.length - 1];
    last.text += `\n\n${currentNoteContextBlock(currentNoteFile, currentNoteTitle)}`;
  }
  // 当前任务清单（todo_write 工具开启时）同以尾部上下文块带出：清单不变则逐字稳定 → 前缀缓存命中；
  // 清单变化（模型更新过 todo）只影响尾块 token，不打断系统前缀缓存
  const todosBlock = tools.some((t) => t.name === "todo_write")
    ? currentTodosBlock(await readAgentTodos(active.id))
    : "";
  if (todosBlock) {
    const last = apiMessages[apiMessages.length - 1];
    last.text += `\n\n${todosBlock}`;
  }

  await runStreamExchange({
    provider,
    model,
    ...(reasoningEffort ? { reasoningEffort } : {}),
    apiMessages,
    ...(tools.length ? { tools } : {}),
    signal: controller.signal,
    applyBatch: ({ content, reasoning }) => {
      useChatPanelStore.setState((state) => ({
        sessions: state.sessions.map((s) =>
          s.id === active.id
            ? {
                ...s,
                messages: s.messages.map((m) =>
                  m.id === asstMsg.id
                    ? {
                        ...m,
                        ...(content ? { content: m.content + content } : {}),
                        // 思考增量流入 steps（最后思考步拼接 / 工具轮之间自然分隔）
                        ...(reasoning
                          ? { steps: appendReasoning(m.steps ?? [], reasoning) }
                          : {}),
                      }
                    : m
                ),
              }
            : s
        ),
      }));
    },
    // 工具调用过程可视化：全量累积 runs 合并进占位消息 steps（思考→工具交错，工具步随消息 .jsonl 记录落盘）
    onToolRuns: (runs) => {
      useChatPanelStore.setState((state) => ({
        sessions: state.sessions.map((s) =>
          s.id === active.id
            ? {
                ...s,
                messages: s.messages.map((m) =>
                  m.id === asstMsg.id
                    ? { ...m, steps: mergeToolRuns(m.steps ?? [], runs) }
                    : m
                ),
              }
            : s
        ),
      }));
    },
    // 工具轮叙述正文进 steps（渲染为该步的「思考行」）
    onNarration: (text) => {
      useChatPanelStore.setState((state) => ({
        sessions: state.sessions.map((s) =>
          s.id === active.id
            ? {
                ...s,
                messages: s.messages.map((m) =>
                  m.id === asstMsg.id
                    ? { ...m, steps: appendNarration(m.steps ?? [], text) }
                    : m
                ),
              }
            : s
        ),
      }));
    },
    onError: (err) => {
      // 不静默降级：占位写入 [错误]（下次请求历史过滤，不污染上下文）
      useChatPanelStore.setState((state) => ({
        sessions: state.sessions.map((s) =>
          s.id === active.id
            ? {
                ...s,
                messages: s.messages.map((m) =>
                  m.id === asstMsg.id
                    ? { ...m, content: m.content || `${ERROR_PREFIX} ${err.message}` }
                    : m
                ),
              }
            : s
        ),
        streaming: false,
      }));
      schedulePersist(active.id);
      void autoNameSession(active.id);
      // 会话轮次结束（出错收敛也通知订阅方，与 正常/中止 语义一致）。
      emitPluginEvent("chat:finished", { sessionId: active.id });
      abortController = null;
    },
    onDone: ({ content, reasoning, timedOut, truncated, promoteNarration }) => {
      // 空回复移除占位；超时且回答未产出写超时降级（保留思考）；否则正常保留。
      // 用占位消息的实际 content/steps 判定（叙述提升/工具步骤已在其内），而非引擎 totals
      const m = useChatPanelStore
        .getState()
        .sessions.find((s) => s.id === active.id)
        ?.messages.find((mm) => mm.id === asstMsg.id);
      // onDone 最终化：最终回答轮叙述提升进 content + 输出上限截断提示（画布/面板共用）
      const finalized = finalizeReplyText({
        content: m?.content ?? content,
        steps: m?.steps ?? [],
        promoteNarration,
        truncated,
      });
      const decision = decideCleanup(
        finalized.content,
        reasoning,
        timedOut,
        finalized.steps.length > 0,
      );
      useChatPanelStore.setState((state) => {
        const sess = state.sessions.find((s) => s.id === active.id);
        if (!sess) return { streaming: false };
        let messages = sess.messages;
        if (decision.kind === "timeout-error") {
          messages = messages.map((m) =>
            m.id === asstMsg.id
              ? { ...m, content: `${ERROR_PREFIX} ${TIMEOUT_ERROR_TEXT}` }
              : m
          );
        } else if (decision.kind === "remove") {
          messages = messages.filter((m) => m.id !== asstMsg.id);
        } else {
          messages = messages.map((m) =>
            m.id === asstMsg.id
              ? { ...m, content: finalized.content, steps: finalized.steps }
              : m
          );
        }
        return {
          sessions: state.sessions.map((s) =>
            s.id === active.id ? { ...s, messages } : s
          ),
          streaming: false,
        };
      });
      schedulePersist(active.id);
      void autoNameSession(active.id);
      // 领域事件：assistant 完成消息（保留分支才有最终内容；超时/移除分支不入列）+ 会话轮次结束。
      if (decision.kind !== "timeout-error" && decision.kind !== "remove" && finalized.content) {
        emitPluginEvent("chat:message", { sessionId: active.id, role: "assistant", content: finalized.content });
      }
      emitPluginEvent("chat:finished", { sessionId: active.id });
      abortController = null;
    },
    executeTools: (calls) =>
      // 公共工具执行器（画布/面板共用）；面板无画布上下文，不建产物节点
      runAgentTools(calls, {
        signal: controller.signal,
        capabilities: {
          search: (query) => runSearch(useSettingsStore.getState().searchConfig, query),
          readFile: (path, opts) => readVaultFileWindow(path, opts),
          glob: (pattern, opts) => globVault(pattern, opts),
          grep: (pattern, opts) => grepVault(pattern, opts),
          listDir: (dir) => listVaultDir(dir),
          renameFile: (oldPath, newName) => useVaultStore.getState().renameFile(oldPath, newName),
          moveFile: (oldPath, targetDir) => useVaultStore.getState().moveFile(oldPath, targetDir),
          deleteFile: (path) => useVaultStore.getState().deleteFile(path),
          deleteDir: (dir, force) =>
            useVaultStore.getState().deleteFolder(dir, force).then((r) => ({
              ok: r.deleted,
              summary: r.deleted
                ? `已删除目录「${dir}」`
                : r.needsConfirm
                  ? `目录非空（${r.itemCount} 项）`
                  : "删除目录失败",
              needsConfirm: r.needsConfirm,
              itemCount: r.itemCount,
            })),
          readHistory: (path, opts) => readHistoryForAgent(path, opts),
          appendFile: (path, content) =>
            appendVaultFile(path, content).then((res) => {
              if (res.ok) void recordAgentFileWrite(path);
              return res;
            }),
          writeTodos: (todos) => writeAgentTodos(active.id, todos),
          writeFile: (path, content) => writeVaultFile(path, content).then(() => {
            // Agent 协作历史：AI 写文件以 Agent 身份记入对应 kind 的历史（fire-and-forget）
            void recordAgentFileWrite(path, content);
            return { ok: true, summary: `已写入「${path}」` };
          }),
          editFile: (path, edits) => editVaultFile(path, edits).then((res) => {
            if (res.ok) void recordAgentFileWrite(path);
            return res;
          }),
          fetchUrl: fetchWeb,
        },
      }),
  });
}

export const useChatPanelStore = create<ChatPanelState>((set, get) => ({
  sessions: [],
  activeSessionId: null,
  streaming: false,
  modelOverride: null,
  effortOverride: null,
  pendingMentions: [],
  draftAgentId: BUILTIN_AGENT_CHAT_ID,
  pendingRewrites: [],
  error: null,
  loaded: false,
  sessionVaultId: null,

  load: async (vaultId, force = false) => {
    // 幂等：内存会话已属于目标仓库（sessionVaultId 为权威）则跳过——防多调用方重复读盘
    // + 二次 load 覆盖进行中会话改动；真实换仓库（sessionVaultId ≠ 目标）必重载；
    // `force`（selectVault 真实切换传入）绕过守卫强制重读盘。
    const st = useChatPanelStore.getState();
    const prevVault = st.sessionVaultId;
    if (!force && st.loaded && prevVault === vaultId) {
      return;
    }
    // 清残留 debounce timer（防旧仓库 timer 写新仓库状态）+ 脏会话标记
    persistCtl.cancel();
    dirtyMessageFiles.clear();
    dirtyMetaSessions.clear();
    messageBaseline.clear();
    autoNamedSessions.clear();
    overridesDirty = false;
    // 真实重载（换仓库/强制）时中止进行中的流式回复：会话即将清空重建，孤儿流只会把增量
    // 写进已消失的会话（静默丢内容），与画布「切仓库中止流」同语义
    if (abortController) {
      abortController.abort();
      abortController = null;
    }
    // 切仓库让路：中止旧仓库进行中的命名请求，防其后台空转/误写
    abortAutoTitle();
    // 真实换仓库：立即清空旧仓库会话（历史列表/当前会话回到新仓库空上下文），
    // 杜绝切换后残留、加载失败残留旧仓库数据被展示（force = 明确切换，必清）
    set({
      pendingMentions: [],
      pendingRewrites: [],
      streaming: false,
      ...(force || prevVault !== vaultId ? { sessions: [], activeSessionId: null } : {}),
    });
    try {
      // 读面板级覆盖（设备偏好）
      const f = await readEditorChatsMeta();
      // 会话清单 = 扫 .atelyx/对话历史/ 目录（无整文件索引）+ 逐个读消息 .jsonl（读失败降级空消息，不阻塞面板）
      const rows = await listChatSessions();
      const sessions: EditorChatSession[] = [];
      for (const row of rows) {
        const messages = await readChatMessages(row.file)
          .then((jsonl) => parseChatMessages(jsonl))
          .catch(() => []);
        // 追加式基线 = 磁盘解析结果（未写盘过的新会话在首次保存时走全量重写）
        messageBaseline.set(row.id, messages);
        sessions.push({
          id: row.id,
          ...(row.meta?.title !== undefined ? { title: row.meta.title } : {}),
          ...(row.meta?.agentId !== undefined ? { agentId: row.meta.agentId } : {}),
          file: row.file,
          createdAt: messages[0]?.createdAt ?? 0,
          updatedAt: messages[messages.length - 1]?.createdAt ?? 0,
          messages,
        });
      }
      // 切仓库竞态守卫：后台填充链与 VaultSwitcher 快速切换并发时，
      // 旧仓库读取结果不得覆盖新仓库的会话（等待期间已切走则丢弃）
      if (useAppStore.getState().vaultId !== vaultId) return;
      // 新仓库干净状态：清脏标记（旧仓库未写完的改动不再写回）
      dirty = false;
      set({
        sessions,
        // 新对话态：打开面板默认是空对话，历史会话从历史浮层手动打开；
        // Agent draft 默认预置「对话」（内存态），切仓库重置
        activeSessionId: null,
        sessionVaultId: vaultId,
        modelOverride: f.modelOverride,
        effortOverride: f.effortOverride ?? null,
        draftAgentId: BUILTIN_AGENT_CHAT_ID,
        loaded: true,
        error: null,
      });
      // 恢复补命名：对最近使用的未命名会话重试（覆盖上次命名被中断/丢失的窗口；仅补一个防请求轰炸）。
      // 未命名判定 = title 仍为空/等于首条 user 消息前缀（命名成功会改变 title，下次 load 不再匹配）
      const unnamed = [...sessions]
        .sort((a, b) => b.updatedAt - a.updatedAt)
        .find((s) => {
          if (autoNamedSessions.has(s.id)) return false;
          const firstUser = s.messages.find((m) => m.role === "user");
          if (!firstUser || !s.messages.some((m) => m.role === "assistant")) return false;
          return !s.title || s.title === prefix(firstUser.displayContent ?? firstUser.content, 16);
        });
      if (unnamed) void autoNameSession(unnamed.id);
    } catch (e) {
      console.error("读取 AI 对话会话失败", e);
      if (useAppStore.getState().vaultId !== vaultId) return;
      set({ loaded: true, error: "读取 AI 对话会话失败" });
    }
  },

  newSession: () => {
    // 已是新对话态（无激活会话）→ 复用；否则切到新对话态。
    // 不创建空 session 对象：空会话天然不进 sessions、不落盘、不出现在历史列表，
    // 发送首条消息时由 send 真正创建会话。draft Agent 随新建会话重置为默认「对话」。
    if (!get().activeSessionId) return;
    set({ activeSessionId: null, draftAgentId: BUILTIN_AGENT_CHAT_ID, error: null });
  },

  openSession: (id) => {
    if (get().activeSessionId === id) return;
    // 切换即「使用」：内存 updatedAt 置顶（UI 最近使用排序）；
    // 「最近使用」不持久化——重启后按最近对话（末条消息时间）排序，避免每次打开都写盘/跨设备重排
    const now = Date.now();
    set({
      sessions: get().sessions.map((s) => (s.id === id ? { ...s, updatedAt: now } : s)),
      activeSessionId: id,
      error: null,
    });
  },

  deleteSession: (id) => {
    const target = get().sessions.find((s) => s.id === id);
    const sessions = get().sessions.filter((s) => s.id !== id);
    dirtyMessageFiles.delete(id); // 不再重写已删会话的消息 .jsonl
    dirtyMetaSessions.delete(id); // 不再重写已删会话的元数据侧车
    messageBaseline.delete(id);
    if (target?.file) {
      // 立即删消息 .jsonl + 元数据侧车（异步，失败仅记日志——删除 = 删文件，跨设备经 watcher 传播）
      void deleteChatMessages(target.file).catch((e) =>
        console.error("删除会话消息文件失败", e),
      );
      void deleteChatSessionMeta(chatMetaFilePath(id)).catch((e) =>
        console.error("删除会话元数据文件失败", e),
      );
      // 顺手清理该会话的任务清单侧车（孤儿清理；best-effort，失败静默）
      void deleteAgentTodos(id).catch(() => {});
    }
    let activeSessionId = get().activeSessionId;
    if (activeSessionId === id) {
      // 删除当前会话 → 回落新对话态（与「默认新空对话」一致，不自动跳到其他历史）
      activeSessionId = null;
    }
    set({ sessions, activeSessionId, error: null });
    // 无整文件索引要写；保留调度以 flush 其余待写项（若有）
    schedulePersist();
  },

  send: async (content, refs = []) => {
    const trimmed = content.trim();
    if (!trimmed || get().streaming) return;

    // 让路：中止当前会话的自动命名请求（防其占用后端槽位与新消息排队；不误伤其他会话）
    const sid = get().activeSessionId;
    if (sid) abortAutoTitle(sid);

    const resolved = resolveProviderModel();
    if (!resolved) return;

    // 确保有激活会话：新对话态（null）时创建会话并激活——标题/消息 .jsonl 路径在首条消息确定；
    // 新对话态选好的 draft Agent 随会话创建固化，随后清空
    let active: EditorChatSession | null =
      get().sessions.find((s) => s.id === get().activeSessionId) ?? null;
    if (!active) {
      const now = Date.now();
      const id = crypto.randomUUID();
      active = {
        id,
        title: prefix(trimmed, 16),
        file: chatMessageFilePath(id),
        agentId: get().draftAgentId,
        messages: [],
        createdAt: now,
        updatedAt: now,
      };
      set({
        sessions: [...get().sessions, active],
        activeSessionId: active.id,
        draftAgentId: undefined,
        error: null,
      });
      // 新会话：元数据侧车随首条消息落盘（新建 = 新文件，多设备并发创建互不覆盖）
      markMetaDirty(active.id);
    }

    // @引用（手动拖入）：只发文件路径——@标签 保留原位，消息开头拼「引用文件」路径块
    // （模型用 read_file 读取正文，不整文打进消息）。标签被用户手动删掉/文件缺失时跳过（扫描不到标签 = 该引用下沉丢弃，不记 refs）。
    const { text: finalContent, injectedFiles } = await injectNoteRefs(trimmed, refs);
    const injectedRefs: EditorChatMessageRef[] = injectedFiles
      .map((f) => refs.find((r) => r.file === f))
      .filter((r): r is EditorChatMessageRef => !!r);

    const userMsg: EditorChatMessage = {
      id: crypto.randomUUID(),
      role: "user",
      // 气泡显示原始输入；content 含「引用文件」路径块（与画布 displayContent 分离语义一致）
      content: finalContent,
      displayContent: trimmed,
      refs: injectedRefs.length ? injectedRefs : undefined,
      createdAt: Date.now(),
    };

    await runExchange(active, userMsg, resolved.provider, resolved.model, resolved.reasoningEffort);
  },

  regenerate: async () => {
    const s = get();
    if (s.streaming) return;
    const session = s.sessions.find((x) => x.id === s.activeSessionId);
    if (!session) return;
    const list = session.messages;
    let lastUserIdx = -1;
    let lastAsstIdx = -1;
    for (let i = list.length - 1; i >= 0; i--) {
      if (lastAsstIdx < 0 && list[i].role === "assistant") lastAsstIdx = i;
      if (lastUserIdx < 0 && list[i].role === "user") lastUserIdx = i;
      if (lastUserIdx >= 0 && lastAsstIdx >= 0) break;
    }
    if (lastUserIdx < 0) return;

    const resolved = resolveProviderModel();
    if (!resolved) return;

    const userMsg = list[lastUserIdx];
    // 重建引用：以原始输入（displayContent）为基底重拼「引用文件」路径块（同 send 语义）。
    // 不能以 userMsg.content（已含上次路径块）为基底——上次的路径块会把 @标签 位置
    // 整体推移，displayContent 的命中索引套在 content 上会错位（多个 @引用时尤甚）；
    // displayContent 缺失/无 refs 跳过。
    let rebuiltContent = userMsg.content;
    if (userMsg.displayContent && userMsg.refs?.length) {
      const { text } = await injectNoteRefs(userMsg.displayContent, userMsg.refs);
      rebuiltContent = text;
    }

    // 移除最后 assistant（在 user 之后）与最后 user（runExchange 重发重建版），一次 set 完成
    const asstToDrop = lastAsstIdx > lastUserIdx ? list[lastAsstIdx].id : null;
    const base = list.filter((m) => m.id !== userMsg.id && m.id !== asstToDrop);
    if (base.length !== list.length) {
      set({
        sessions: get().sessions.map((x) =>
          x.id === session.id ? { ...x, messages: base, updatedAt: Date.now() } : x
        ),
      });
      schedulePersist(session.id);
    }
    await runExchange({ ...session, messages: base }, { ...userMsg, content: rebuiltContent }, resolved.provider, resolved.model, resolved.reasoningEffort);
  },

  renameSession: async () => {
    const s = get();
    const id = s.activeSessionId;
    if (!id || s.streaming || !s.sessions.some((x) => x.id === id)) return;
    // 手动接管：中止本会话在途的自动命名请求（防同一会话重复请求；延迟中未发出的由 isNamed 二次校验兜底跳过）
    abortAutoTitle(id);
    // 重新命名：全量会话记录（不截断）、立即请求（无 3s 防限流延迟——用户主动点击期待即时反馈）、
    // 不受「话题自动命名」开关限制；成功后登记防自动命名重复覆盖
    const result = await runAutoNaming(
      {
        getMessages: () => {
          const cur = useChatPanelStore.getState().sessions.find((x) => x.id === id);
          return cur?.messages ?? [];
        },
        isNamed: () => false,
        applyTitle: (title) => applySessionTitle(id, title),
      },
      { delayMs: 0, maxChars: Infinity, ignoreToggle: true },
    );
    // 反馈：真失败（请求出错）可重试；未配置模型提示配置；跳过（无消息/被中止）静默
    if (result !== "ok") {
      const hasNamingConfig = !!useSettingsStore.getState().resolveAutoNamingModel(true);
      if (!hasNamingConfig) {
        set({ error: "话题命名不可用：未配置默认模型或话题命名模型" });
      } else if (result === "failed") {
        set({ error: "话题命名失败，请稍后重试" });
      }
    }
  },

  rollbackTo: (messageId) => {
    const s = get();
    const id = s.activeSessionId;
    if (!id || s.streaming) return;
    const session = s.sessions.find((x) => x.id === id);
    if (!session) return;
    const idx = session.messages.findIndex((m) => m.id === messageId);
    if (idx < 0 || idx === session.messages.length - 1) return;
    set({
      sessions: s.sessions.map((x) =>
        x.id === id
          ? { ...x, messages: session.messages.slice(0, idx + 1), updatedAt: Date.now() }
          : x
      ),
    });
    schedulePersist(id);
  },

  stop: () => {
    abortController?.abort();
  },

  setAgentId: (id) => {
    const current = get();
    if (current.activeSessionId) {
      set({
        sessions: current.sessions.map((s) =>
          s.id === current.activeSessionId ? { ...s, agentId: id } : s
        ),
      });
      // 会话级 Agent 变化写元数据侧车（跨设备经 watcher 实时传播）
      markMetaDirty(current.activeSessionId);
    } else {
      // 新对话态：暂存待用，发送首条消息创建会话时固化（见 send）
      set({ draftAgentId: id });
    }
  },

  setModelOverride: (ov) => {
    set({ modelOverride: ov });
    markOverridesDirty();
  },

  setEffortOverride: (effort) => {
    set({ effortOverride: effort });
    markOverridesDirty();
  },

  clearError: () => set({ error: null }),

  applyExternalChatChange: (file) => {
    // watcher 收到 .atelyx/对话历史/ 文件事件（消息或元数据侧车）：内容比对合并，
    // 自写与外部写用内容比对判别（无 2s 时间窗误判——多设备同路径互写不误伤）
    if (!useChatPanelStore.getState().loaded) return;
    const isMeta = file.endsWith(CHAT_META_EXT);
    const ext = isMeta ? CHAT_META_EXT : CHAT_MESSAGE_EXT;
    if (!file.endsWith(ext)) return;
    const stem = file.slice(0, -ext.length);
    const id = stem.slice(stem.lastIndexOf("/") + 1);
    if (!id) return;
    if (isMeta) {
      void applyExternalMeta(id);
    } else {
      void applyExternalMessages(id, file);
    }
  },

  queueMention: (ref) => {
    set((state) => ({
      pendingMentions: state.pendingMentions.some((r) => r.file === ref.file)
        ? state.pendingMentions
        : [...state.pendingMentions, ref],
    }));
  },

  clearPendingMentions: () => set({ pendingMentions: [] }),

  queueNoteRewrite: (req) => {
    set((state) => ({
      pendingRewrites: [...state.pendingRewrites, req],
    }));
  },

  clearPendingRewrites: () => set({ pendingRewrites: [] }),

  flush: (vaultId) => {
    // 无本地改动不写盘：外部删除会话文件后切仓库/退出，不把内存副本写回（覆盖删除）
    if (!dirty) return Promise.resolve();
    return persistCtl.flush(vaultId);
  },
}));
