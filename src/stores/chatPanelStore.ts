import { create } from "zustand";
import {
  listChatSessions,
  writeChatSessionMeta,
  deleteChatSessionMeta,
  writeEditorChatsMeta,
  readEditorChatsMeta,
  readChatMessages,
  writeChatMessages,
  appendChatMessages,
  deleteChatMessages,
} from "@/services/metadata";
import {
  foldChatDelta,
  flushChatContainerTruth,
  foldChatContainerExternal,
  initChatContainerTransport,
  isChatContainerHost,
  isRustTruthMode,
  requestChatContainerSnapshot,
  sendChatContainerOp,
  stripPendingsForWire,
  type ChatContainerOp,
  type ChatContainerOpResponse,
  type ChatContainerSnapshot,
  type ChatContainerView,
  type ChatDeltaFragments,
  type ChatOpOutcome,
} from "@/services/chatContainerWire";
import { identityKeyOf } from "@/services/content/factory";
import { abortAutoTitle } from "@/services/ai/autoTitle";
import { resolveMessageAttachments } from "@/services/ai/client";
import {
  cleanupSessionTempAttachments,
  createMessageAttachmentReader,
  writeTempAttachment,
} from "@/services/tempAttachment";
import { deleteAgentTodos } from "@/services/vault/agentTodos";
import { CHAT_UNAVAILABLE_TEXT, ERROR_PREFIX } from "@/constants/chat";
import { BUILTIN_AGENT_CHAT_ID } from "@/constants/agents";
import { emitPluginEvent } from "@/services/cordis/events";
import { prefix, scanMentionHits } from "@/utils/text";
import { createBackoffRetry } from "@/utils/backoff";
import { coalesceAgentSteps, fillAssistantReplyText } from "@/utils/agentSteps";
import { nextCompactionBoundary, splitByCompaction } from "@/utils/compaction";
import { createPersistController } from "@/utils/persist";
import { getChatRuntime } from "@/utils/chatRuntimeHost";
import { useSettingsStore } from "./settingsStore";
import { useAppStore } from "./appStore";
import { useVaultStore } from "./vaultStore";
import { useNotificationStore } from "./notificationStore";
import {
  EDITOR_CHATS_META_SCHEMA,
  CHAT_HISTORY_DIR,
  CHAT_MESSAGE_EXT,
  CHAT_META_EXT,
} from "@/constants/editorChats";
import type {
  Attachment,
  ChatNamingTarget,
  ChatRuntime,
  ChatTurnMessage,
  ChatTurnSink,
  ChatTurnTarget,
  ConversationCompaction,
  EditorChatMessage,
  EditorChatMessageRef,
  EditorChatModelOverride,
  EditorChatSession,
  ChatMetaFile,
  NoteRewriteRequest,
  PendingAttachment,
  ProviderConfig,
  ReasoningEffort,
} from "@/types";

/**
 * AI 对话面板会话状态（跨窗口宿主-镜像模型）：主窗口 = 宿主单写者（容器态与写盘链独占），
 * 撕裂窗口 = 镜像薄客户端（基线快照 + op 转发 + 增量折叠，见 services/chatContainerWire），
 * 激活会话/草稿/输入队列每窗口本地。存储：每会话一个消息 `.jsonl`（JSON Lines，追加式写）+ 可选
 * `.meta.json` 元数据侧车；会话清单 = 扫目录（无整文件索引），磁盘为真相（重进仓库/切回面板读盘刷新，
 * 外部与跨设备变更不实时互见）；面板级覆盖存 `.atelyx/editor-chats-meta.json`。协作空间内同一套读写
 * 签名经 services/metadata 分发到 user meta（`chat/messages/<id>` 等），`file` 仍按本地路径约定回填。
 * 笔记上下文两条路径：#引用（手动拖入，发送时按路径块注入）+ 当前打开笔记尾部上下文块（runExchange
 * 注入，ephemeral 不落盘）。与画布对话的差异：编排由对话核心能力承担（`stores/chatTurn.ts`，经
 * `utils/chatRuntimeHost` 取用），本 store 只管容器、落盘与面板态；错误占位沿用 `[错误]` 前缀过滤
 * （ERROR_PREFIX）；持久化 debounce 500ms，消息纯增长只追加新增记录。
 */

interface ChatPanelState {
  sessions: EditorChatSession[];
  activeSessionId: string | null;
  /** 面板是否正在流式回复（全局单一，与当前激活会话对应）。 */
  streaming: boolean;
  /** 发送预检进行中（附件落盘 + 会话创建 + 消息入容器）：预检完成前防重入双发；
   *  预检完成即复位，此后由 streaming 守卫接力。 */
  sending: boolean;
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
  /** 压缩中的会话 id（null = 无）。压缩为独立模型请求、与流式共用一个中止句柄，故守卫是全局的；
   *  记录 id 只为把转圈/禁用态显示在真正压缩的那个会话上（切到别的会话不误报）。 */
  compacting: string | null;
  loaded: boolean;
  /** 内存会话所属仓库的身份键（identityKeyOf 序列化，load 时记录；flush/写盘前校验归属，
   *  防跨仓库搞混——空间切换 vaultRoot 恒 null，root 比对失效）。 */
  sessionVaultKey: string;
  /** 最近一次写盘失败（null = 无；失败可见不静默，退避重试成功后清空；at 供状态条显示最新错误时间）。 */
  persistError: { message: string; at: number } | null;

  /** 进仓库时加载：读盘历史会话 + 进入新对话态（默认显示新的空对话，不恢复上次激活会话）。
   *  目标身份取调用时的激活仓库身份键；`force`：真实仓库切换时传 true——绕过「已加载该身份」幂等守卫强制重读盘。 */
  load: (force?: boolean) => Promise<void>;
  /** 切到新对话态（activeSessionId = null，不创建空会话对象）——发送首条消息时才真正创建会话。 */
  newSession: () => void;
  /** 切换到历史会话（内存 updatedAt 置顶排序；「最近使用」不持久化，重启后按最近对话排序）。 */
  openSession: (id: string) => void;
  /** 删除会话（删的是当前激活会话时回落新对话态；同时删其消息 .jsonl 与元数据侧车）。 */
  deleteSession: (id: string) => void;
  /** 把插件侧消息登记为新面板会话（ctx.chat.importSession 数据源）：消息经校验转换，落盘走既有防抖链。
   *  不改变面板当前激活会话；title 缺省按首条 user 消息派生。校验失败抛错（角色/内容/附件引用）。 */
  importSession: (
    messages: ChatTurnMessage[],
    opts?: { title?: string; agentId?: string },
  ) => Promise<{ id: string }>;
  /** 向既有面板会话追加插件侧消息（ctx.chat.appendMessages 数据源）：会话不存在即抛错。 */
  appendMessages: (sessionId: string, messages: ChatTurnMessage[]) => Promise<void>;
  /** 面板会话清单（同源只读；id + 标题 + 最近活动时间，按 updatedAt 降序）。
   *  store 未加载时先读盘（同 importSession）。 */
  listSessions: () => Promise<Array<{ id: string; title?: string; updatedAt: number }>>;
  /** 打开面板会话读取全部消息与元数据（同源；会话不存在即抛错）。
   *  与上面的 openSession（激活面板会话）不同：本动作只读，不动激活态。 */
  readSession: (sessionId: string) => Promise<{
    id: string;
    title?: string;
    agentId?: string;
    compaction?: ConversationCompaction;
    messages: ChatTurnMessage[];
  }>;
  /** 新建空面板会话（同源；返回 id；不改面板激活会话）。首条消息落盘时 .jsonl 才实际出现。 */
  createSession: (opts?: { title?: string; agentId?: string }) => Promise<{ id: string }>;
  /** 写面板会话标题（同源；会话不存在即抛错）。 */
  setSessionTitle: (sessionId: string, title: string) => Promise<void>;
  /** 删除面板会话（同源；连带消息 .jsonl / 元数据侧车 / 任务清单侧车）。 */
  deleteSessionExternal: (sessionId: string) => Promise<void>;
  /** 发送消息到当前激活会话（refs = 输入框内的 #引用笔记，发送时就地替换注入路径块；
   *  pendings = 待发送托盘附件：字节落仓库临时区后以路径引用随消息持久化，图片走 vision、
   *  文本类注入内容。附件落盘失败返回 false 且不产生消息（输入与托盘原样保留），
   *  其余失败路径（无模型等）与既有语义一致。 */
  send: (content: string, refs?: EditorChatMessageRef[], pendings?: PendingAttachment[]) => Promise<boolean>;
  /** 重新生成最后一条回复：移除最后 assistant、按 refs 重建最后一条 user 消息注入后重发（同画布 regenerate 语义）。 */
  regenerate: () => Promise<void>;
  /**
   * 手动压缩当前会话：把当前全部消息交给模型总结成检查点，之后重建请求历史时全部历史由该摘要代替
   * （消息本体不动，仅其后的新消息按原文追加）。失败/无可压缩/已覆盖末尾均只提示，不写注解。
   */
  compactSession: () => Promise<void>;
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
  /** 立即落盘并返回写盘 Promise（可等待——切换仓库前必须先等旧会话写完，防写进新仓库）。
   * 归属校验按身份键：当前激活身份键与内存会话所属身份键（sessionVaultKey）不一致则跳过（防跨仓库污染）。
   * 无本地改动（dirty=false）也跳过（外部删除会话文件后切仓库不写回覆盖）。 */
  flush: () => Promise<void>;
}

let abortController: AbortController | null = null;
/** 会话是否有本地改动（新建/切换/删除/发送/设置变化置 true；写盘成功后清）。
 * 脏门控：未改动不写盘——外部删除会话文件后切仓库，flush 不再把内存副本写回（覆盖删除）。 */
let dirty = false;
/** 需要重写消息 .jsonl 的会话 id 集合（发送/流式结束时标记；persistNow 统一写盘后清空）。 */
const dirtyMessageFiles = new Set<string>();
/** 需要重写元数据侧车（.meta.json：title/agentId/compaction）的会话 id 集合（改名/换 Agent/压缩时标记）。 */
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

/** 已判定「不是文本」的附件引用（二进制附件）：补读/发送前跳过；引用是仓库相对路径，切仓库时清空。 */
const nonTextAttachmentRefs = new Set<string>();

/** 已提示过的「消息附件读回失败」引用：同一附件不重复弹（否则每次发送都弹一次）。随进仓加载清空。 */
const reportedAttachmentReadFailures = new Set<string>();

/** 消息附件补齐读取器（发送前按引用读回）：非文本（二进制）返回空串不算失败（预期不注入模型）；
 *  读不到通知一次并返回空串，该附件不进请求、其余附件与对话照常（按附件粒度降级）。 */
const readMessageAttachment = createMessageAttachmentReader((ref, error) => {
  console.error("面板消息附件内容读回失败，本次不发送该附件", ref, error);
  if (reportedAttachmentReadFailures.has(ref)) return;
  reportedAttachmentReadFailures.add(ref);
  useNotificationStore.getState().notify({
    level: "warning",
    message: `附件「${ref.split("/").pop() ?? ref}」读取失败，本次未发送；请重新添加或移除该附件`,
  });
}, nonTextAttachmentRefs);

/**
 * 会话消息附件水合：附件内容（图片缩略/文本正文）是运行时缓存不落盘，重开软件后按引用读回并回填，
 * 历史气泡的图片才能恢复显示（否则退化为文件名 chip）。
 * 只补缺 payload 的附件；「不是文本」负缓存跳过；读失败留日志不重试（下次打开会话再试）。
 * 仅水合当前打开的会话（全量历史读一遍是无谓 I/O）；回填按消息 id + 附件下标核对引用，
 * 期间消息列表变化不会套错内容。
 */
async function hydrateSessionAttachments(sessionId: string): Promise<void> {
  const session = useChatPanelStore.getState().sessions.find((s) => s.id === sessionId);
  if (!session) return;
  const targets: Array<{ messageId: string; index: number; ref: string; kind: "image" | "file" }> = [];
  for (const m of session.messages) {
    (m.attachments ?? []).forEach((a, i) => {
      if (a.file && !a.payload && !nonTextAttachmentRefs.has(a.file)) {
        targets.push({ messageId: m.id, index: i, ref: a.file, kind: a.kind });
      }
    });
  }
  if (targets.length === 0) return;
  const payloads = new Map<string, string>();
  await Promise.all(
    targets.map(async (t) => {
      const payload = await readMessageAttachment(t.ref, t.kind);
      if (payload) payloads.set(`${t.messageId}:${t.index}`, payload);
    }),
  );
  if (payloads.size === 0) return;
  useChatPanelStore.setState((state) => {
    let changed = false;
    const sessions = state.sessions.map((s) => {
      if (s.id !== sessionId) return s;
      const messages = s.messages.map((m) => {
        const atts = m.attachments;
        if (!atts?.length) return m;
        let filled = false;
        const attachments = atts.map((a, i) => {
          if (a.payload) return a;
          // 按同下标取读回结果并核对引用，防期间列表变化套错内容
          const candidate = payloads.get(`${m.id}:${i}`);
          if (!candidate) return a;
          filled = true;
          return { ...a, payload: candidate };
        });
        if (!filled) return m;
        changed = true;
        return { ...m, attachments };
      });
      if (!changed) return s;
      return { ...s, messages };
    });
    if (!changed) return state;
    return { sessions };
  });
}

/** 当前激活仓库的身份键（identityKeyOf，未激活 = "none"）：load/flush 的归属判别统一用键而非 root
 * （空间模式 vaultRoot 恒 null，root 比对无法区分空间 A/B）。 */
function activeIdentityKey(): string {
  return identityKeyOf(useAppStore.getState().vaultIdentity);
}

/** 防抖持久化控制器：timer 管理 + 代数防吞统一在此；定时写盘与 flush 同签名（归属校验在 persistNow 内按身份键做）。 */
const persistCtl = createPersistController({
  persist: () => persistNow(),
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
 * 只写持久化字段：id/createdAt 稳定持久化，refs（#引用）/steps（含工具步）/attachments
 * （附件按 `file` 引用持久化，`payload` 是运行时缓存、落盘剥离——base64 图片会把历史文件撑到几十 MB）
 * 结构化持久化，重开会话完整恢复。
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
        ...(m.attachments?.length
          ? { attachments: m.attachments.map(({ payload: _payload, ...rest }) => rest) }
          : {}),
        createdAt: m.createdAt,
      })
    )
    .join("\n");
}

/**
 * 插件侧消息 → 面板消息（ctx.chat.importSession/appendMessages 的共享转换）：逐条校验，
 * 失败抛错不静默——角色限 user/assistant、content 须为字符串、附件仅接受带 `file` 引用的
 * （payload 是运行时缓存不落盘，重开会话按引用读回，纯内联附件落盘即丢内容）；id 撞车或
 * 缺失时重生成（`reservedIds` = 既有占用：追加场景传目标会话现有消息 id，保证**会话内**
 * 唯一——压缩注解锚点与前端 key 都按它定位，跨调用批次的撞车同样要避开）；createdAt
 * 按序派生保证时序。
 */
function toPanelMessages(
  messages: ChatTurnMessage[],
  baseTime: number,
  reservedIds?: Iterable<string>,
): EditorChatMessage[] {
  if (!Array.isArray(messages) || messages.length === 0) {
    throw new Error("会话消息须为非空数组");
  }
  const usedIds = new Set<string>(reservedIds);
  return messages.map((m, i) => {
    const label = `第 ${i + 1} 条消息`;
    if (!m || typeof m !== "object") throw new Error(`${label}形状非法`);
    if (m.role !== "user" && m.role !== "assistant") {
      throw new Error(`${label} role 只支持 user/assistant`);
    }
    if (typeof m.content !== "string") throw new Error(`${label} content 须为字符串`);
    let id = typeof m.id === "string" && m.id !== "" && !usedIds.has(m.id) ? m.id : crypto.randomUUID();
    while (usedIds.has(id)) id = crypto.randomUUID();
    usedIds.add(id);
    if (m.attachments?.length) {
      for (const a of m.attachments) {
        if (!a || typeof a.file !== "string" || a.file === "") {
          throw new Error(`${label}附件缺少 file 引用（内联附件无法持久化，不支持导入）`);
        }
      }
    }
    return {
      id,
      role: m.role,
      content: m.content,
      ...(typeof m.displayContent === "string" && m.displayContent !== "" ? { displayContent: m.displayContent } : {}),
      ...(Array.isArray(m.steps) && m.steps.length ? { steps: coalesceAgentSteps(m.steps) } : {}),
      ...(m.attachments?.length
        ? { attachments: m.attachments.map(({ payload: _payload, ...rest }) => rest) }
        : {}),
      createdAt: baseTime + i,
    };
  });
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
        ...(Array.isArray(raw.attachments) ? { attachments: raw.attachments } : {}),
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
 * #引用 注入（send/regenerate 共用）：.md 引用只发文件路径——#标签 文本保留原位，
 * 消息开头拼「引用文件」路径块（模型用 read_file 按需读取正文，不把笔记全文打进每条消息）。
 * #标签 被手动删掉时跳过（扫描不到标签 = 该引用下沉丢弃，不记 refs）。
 */
async function injectNoteRefs(
  text: string,
  refs: { file: string; label: string }[],
): Promise<{ text: string; injectedFiles: string[] }> {
  const injectedFiles: string[] = [];
  if (!refs.length) return { text, injectedFiles };
  // 触发符双兼容：新输入为 #标签，旧消息（重新生成基底）固化的是 @标签
  const hits = scanMentionHits(
    text,
    refs.flatMap((r) => [
      { nodeId: r.file, text: `#${r.label}` },
      { nodeId: r.file, text: `@${r.label}` },
    ]),
  );
  const hitFiles = new Set(hits.map((h) => h.mention.nodeId));
  // 按 #/@标签 出现顺序去重（同文件重复引用只出一条路径）
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

/** 面板会话的命名目标（轮末自动命名与手动重新命名共用）。 */
function sessionNamingTarget(sessionId: string): ChatNamingTarget {
  return {
    getMessages: () => {
      const s = useChatPanelStore.getState().sessions.find((x) => x.id === sessionId);
      // 叙述-only 消息（content 为空、正文在 steps）回填正文，供话题命名摘要
      return (s?.messages ?? []).map(fillAssistantReplyText);
    },
    isNamed: () => autoNamedSessions.has(sessionId),
    applyTitle: (title) => applySessionTitle(sessionId, title),
  };
}

/**
 * 话题自动命名：一轮对话完成后为会话生成话题标题（轮次结束由对话核心能力触发）。
 * - 命名管线走对话核心能力（模型解析/延迟/超时与画布共用），失败含被 abortAutoTitle 中止
 * - 失败不登记——降级保留首条消息前缀，下轮对话完成/进入仓库时自动重试
 * - 命名只改侧车 title（消息 .jsonl 文件名 = 会话 id，不随标题变）
 * - fire-and-forget：对话能力未启用/无可用模型/命名失败一律降级保留占位标题，不阻塞对话
 */
async function autoNameSession(sessionId: string): Promise<void> {
  const runtime = getChatRuntime();
  if (!runtime) return;
  await runtime.autoName(sessionNamingTarget(sessionId), sessionId);
}

/** debounce 500ms 写盘（读最新 state；`messageSessionId` = 本次改动涉及的会话，其消息 .jsonl 需重写）。
 *  Rust 真源模式空操作：落盘脏标记由真源在应用变更时自行登记，写链（防抖+退避）在 Rust 侧。 */
function schedulePersist(messageSessionId?: string) {
  if (isRustTruthMode()) return;
  if (messageSessionId) dirtyMessageFiles.add(messageSessionId);
  dirty = true;
  persistCtl.schedule();
}

/** 会话元数据侧车（title/agentId）脏标记 + 调度写盘（Rust 真源模式空操作，同上）。 */
function markMetaDirty(sessionId: string) {
  if (isRustTruthMode()) return;
  dirtyMetaSessions.add(sessionId);
  dirty = true;
  persistCtl.schedule();
}

/** 面板级覆盖（editor-chats-meta.json）脏标记 + 调度写盘（Rust 真源模式空操作，同上）。 */
function markOverridesDirty() {
  if (isRustTruthMode()) return;
  overridesDirty = true;
  dirty = true;
  persistCtl.schedule();
}

// ===== 跨窗口角色与容器面（Rust 真源 / 宿主-镜像双模式）=====
// Rust 真源模式：Rust 持有持久态与写盘链，全窗口薄客户端（快照/op 走 invoke，增量折叠
// Rust 广播），主窗口兼执行体（意图执行 + 编排变更提交，见 services/chatContainerWire）。
// 事件线模式（Rust 不可达降级）：主窗口 = 宿主单写者（容器态与写盘链独占），撕裂窗口 =
// 镜像薄客户端（基线快照 + op 转发 + 增量折叠）。镜像窗口不持有写盘链：容器态来自
// 快照/增量折叠，改动经 op 转发。

/** 本窗口角色：主窗口 = 宿主/执行体候选（含非 Tauri 环境降级，行为与单窗口一致）。 */
const isContainerHost = isChatContainerHost();

/** 执行体意图执行进行中：意图 op 在本窗口落地时容器动作按本地路径运行。执行体是容器动作的
 * 本地执行者，若经模式分派再转发，执行 op 会再产意图成回环（stop/append/import 等）。 */
let executingIntentOp = false;

/** 本地模式宿主判定：事件线模式主窗口本地动作；Rust 真源模式全部窗口走转发/薄客户端路径，
 * 惟执行体意图执行期间按本地路径运行（见 executingIntentOp）。 */
function isLocalModeHost(): boolean {
  return isContainerHost && (!isRustTruthMode() || executingIntentOp);
}

/** 宿主容器的差分视图（wire 据此产出增量）。 */
function containerView(state: ChatPanelState): ChatContainerView {
  return {
    sessions: state.sessions,
    streaming: state.streaming,
    compacting: state.compacting,
    persistError: state.persistError,
    modelOverride: state.modelOverride,
    effortOverride: state.effortOverride,
    sessionVaultKey: state.sessionVaultKey,
  };
}

/** 镜像容器基线整体替换（激活会话失效回落新对话态，与对账语义一致）。 */
function applyContainerSnapshot(snapshot: ChatContainerSnapshot): void {
  useChatPanelStore.setState((current) => ({
    sessions: snapshot.sessions,
    streaming: snapshot.streaming,
    compacting: snapshot.compacting,
    persistError: snapshot.persistError,
    modelOverride: snapshot.modelOverride,
    effortOverride: snapshot.effortOverride,
    sessionVaultKey: snapshot.sessionVaultKey,
    loaded: true,
    error: null,
    activeSessionId:
      current.activeSessionId !== null && snapshot.sessions.some((s) => s.id === current.activeSessionId)
        ? current.activeSessionId
        : null,
  }));
}

/** 镜像增量折叠（快照/op 响应/广播增量共用折叠；status 字段级合并）。 */
function applyContainerFragments(fragments: ChatDeltaFragments, opts: { notifyPlugins: boolean }): void {
  useChatPanelStore.setState((current) => {
    const sessions = foldChatDelta(current.sessions, fragments);
    const activeSessionId =
      current.activeSessionId !== null && sessions.some((s) => s.id === current.activeSessionId)
        ? current.activeSessionId
        : null;
    const status = fragments.status;
    return {
      sessions,
      activeSessionId,
      ...(status
        ? {
            ...(status.streaming !== undefined ? { streaming: status.streaming } : {}),
            ...(status.compacting !== undefined ? { compacting: status.compacting } : {}),
            ...(status.persistError !== undefined ? { persistError: status.persistError } : {}),
            ...(status.modelOverride !== undefined ? { modelOverride: status.modelOverride } : {}),
            ...(status.effortOverride !== undefined ? { effortOverride: status.effortOverride } : {}),
            ...(status.sessionVaultKey !== undefined ? { sessionVaultKey: status.sessionVaultKey } : {}),
          }
        : {}),
    };
  });
  if (
    opts.notifyPlugins &&
    (fragments.messageSessionIds.length > 0 ||
      fragments.metaSessionIds.length > 0 ||
      fragments.deletedIds.length > 0)
  ) {
    // 本窗口插件订阅方：变更非本窗口引发时转发（自身回声由 wire 的 opOwners 抑制）
    emitPluginEvent("chat:sessions-changed", {
      messages: fragments.messageSessionIds,
      metas: fragments.metaSessionIds,
      deleted: fragments.deletedIds,
    });
  }
}

/**
 * op 响应的软失败解包（regenerate/compact/rename 等以 {ok, error?} 作为 value 约定）：
 * 硬失败（ok=false，镜像层以拒绝表达）返回空对象，交由调用方 catch 统一落 error 态。
 */
function opValueResult(response: ChatContainerOpResponse): { ok?: boolean; error?: string } {
  return response.ok ? ((response.value ?? {}) as { ok?: boolean; error?: string }) : {};
}

// ===== 失败重试（指数退避）=====

/** 写盘失败重试的指数退避序列（500ms→2s→8s→30s 封顶）：持久性故障（服务端不可达/磁盘只读）
 *  期间固定短间隔重试会让写盘请求与错误通知刷屏；成功后归零恢复即时性。 */
const PERSIST_RETRY_DELAYS_MS = [500, 2000, 8000, 30000];
const persistRetry = createBackoffRetry({
  delaysMs: PERSIST_RETRY_DELAYS_MS,
  // 重试走完整 persistNow：loaded/归属守卫与脏集合照常生效
  onRetry: () => void persistNow(),
});

/** 失败后安排下一轮退避重试。 */
function schedulePersistRetry(): void {
  persistRetry.schedule();
}

/** 清退避重试（load 切仓库时调用：persistNow 的归属校验是兜底，清 timer 免旧仓库重试空转）。 */
function cancelPersistRetry(): void {
  persistRetry.reset();
}

/**
 * 写盘。归属校验按身份键：当前激活身份键与内存会话所属身份键（sessionVaultKey）不一致 →
 * 不写（防跨仓库搞混——切仓库前 flush 时身份尚未切换，校验通过落旧仓库；
 * 切换完成后的迟到写盘身份不匹配被丢弃）。Rust 真源模式下整个写盘链在真源侧，此处不达。
 */
async function persistNow(): Promise<void> {
  if (isRustTruthMode()) return;
  const versionAtStart = persistCtl.version;
  let persistFailed = false;
  // 守卫：load 完成前（loaded=false，store 仍是初始空态）不落盘——
  // React 18 StrictMode 开发模式双挂载会在 load 完成前触发卸载 flush，
  // 若此时写盘会用空 sessions 覆盖磁盘真实历史（实测：退出重进历史丢失）
  if (!useChatPanelStore.getState().loaded) return;
  // 仓库归属校验（身份键）：内存会话属于其他仓库 → 不写
  if (activeIdentityKey() !== useChatPanelStore.getState().sessionVaultKey) {
    return;
  }
  const { sessions } = useChatPanelStore.getState();
  // 1) 写脏会话的消息 .jsonl。写成功才移除——失败保留待下次 debounce/flush 重试（防消息只存在于内存而 .jsonl 丢失）；
  //    写盘期间并发 schedulePersist 新标记的会话不在本次快照，保留由下一轮再写（防误清）。
  //    追加式：纯增长只追加新增记录（基线引用逐一相同）；流式中途落盘/截断/基线缺失 → 全量重写（幂等）。
  //    宿主-镜像模型下镜像 op 可在写在途时改写同一会话：写后按引用复核，数组已变则标记保留
  //    由下一轮按新基线追平增量（陈旧快照吞标记 = 在途变更静默丢失）。
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
      const settled = () => {
        // 基线推进到本快照（磁盘真相，失败保留旧值）；标记仅在内存数组未再变化时清除
        messageBaseline.set(id, s.messages);
        if (useChatPanelStore.getState().sessions.find((x) => x.id === id)?.messages === s.messages) {
          dirtyMessageFiles.delete(id);
        }
      };
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
        settled();
      } catch {
        // 追加失败（含外部删文件导致文件缺失）：回落全量重写（幂等，重建历史/防追加重复）；
        // 仍失败保留脏待下次重试 + 基线清除
        try {
          await writeChatMessages(s.file, serializeChatMessages(s.messages));
          settled();
        } catch (e2) {
          messageBaseline.delete(id);
          console.error("保存会话消息失败", e2);
          persistFailed = true;
        }
      }
    }),
  );
  // 2) 写脏会话的元数据侧车（.meta.json：title/agentId）。写成功才移除——失败保留待下次重试。
  //    与消息同款在途复核：会话对象在写在途时被替换（标题/Agent/注解/消息变化）则标记保留，
  //    下一轮幂等重写。
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
          ...(s.compaction ? { compaction: s.compaction } : {}),
        });
        if (useChatPanelStore.getState().sessions.find((x) => x.id === id) === s) {
          dirtyMetaSessions.delete(id);
        }
      } catch (e) {
        console.error("保存会话元数据失败", e);
        persistFailed = true;
      }
    }),
  );
  // 3) 面板级覆盖变化时写 .atelyx/editor-chats-meta.json（设备偏好，不跨设备传播；写成功才清标记）。
  //    载荷取写盘时点的最新覆盖（入参快照可能已在途过期）；写在途又有覆盖变化（引用不同）则
  //    标记保留由下一轮再写。
  if (overridesDirty) {
    const current = useChatPanelStore.getState();
    const metaFile: ChatMetaFile = {
      schema: EDITOR_CHATS_META_SCHEMA,
      modelOverride: current.modelOverride,
      effortOverride: current.effortOverride,
    };
    try {
      await writeEditorChatsMeta(metaFile);
      const latest = useChatPanelStore.getState();
      if (latest.modelOverride === current.modelOverride && latest.effortOverride === current.effortOverride) {
        overridesDirty = false;
      }
    } catch (e) {
      console.error("保存面板覆盖失败", e);
      persistFailed = true;
    }
  }
  // 写盘期间若又有新变更（schedule 已置 dirty + 挂新 timer），保留 dirty 由下一轮再写，
  // 防成功回调吞掉新编辑（消息/侧车/覆盖各有脏集合保护，dirty 仅作 flush 总门）
  if (persistCtl.version === versionAtStart) dirty = false;
  // 失败必须持续可见：状态条记录最新一次失败（含时间），并按指数退避安排自动重试；
  // 成功落盘后清状态条、退避归零（恢复「失败后首 500ms 即重试」的即时性）
  if (persistFailed) {
    schedulePersistRetry();
    useChatPanelStore.setState({
      persistError: { message: "会话保存失败，将自动重试", at: Date.now() },
    });
  } else {
    // 只归零退避进度（在途重试重跑 persistNow 幂等，照常触发）
    persistRetry.resetProgress();
    if (useChatPanelStore.getState().persistError) {
      useChatPanelStore.setState({ persistError: null });
    }
  }
}

/**
 * 解析当前对话的 provider/model（走对话核心能力的解析，与画布/插件同源）：
 * - 面板覆盖 {providerId, model} → 跟随仓库默认（默认模型反查所属供应商）
 * 覆盖供应商已删：清空失效覆盖（含持久化）并提示，本次不发送（不静默回落默认）。
 * 失败返回错误串（宿主 action 负责落 error 态；镜像 op 转发路径经响应回传镜像，不落宿主 error）。
 */
function resolveProviderModel(runtime: ChatRuntime): {
  ok: true;
  provider: ProviderConfig;
  model: string;
  reasoningEffort?: ReasoningEffort;
} | {
  ok: false;
  error: string;
} {
  const ov = useChatPanelStore.getState().modelOverride;
  const resolved = runtime.resolveTarget(ov);
  if (!resolved.ok) {
    if (resolved.reason === "provider-missing") {
      // provider-missing 只可能在 ov 非空时返回（见 resolveChatTarget）；清空失效覆盖并说明已恢复跟随默认
      useChatPanelStore.getState().setModelOverride(null);
      return { ok: false, error: `${resolved.error}（已恢复跟随默认）` };
    }
    return { ok: false, error: resolved.error };
  }
  // 推理等级为面板级独立覆盖（与模型覆盖正交）；缺省 = 不指定（跟随默认，不下发 reasoning_effort）
  return {
    ok: true,
    provider: resolved.provider,
    model: resolved.model,
    reasoningEffort: useChatPanelStore.getState().effortOverride ?? undefined,
  };
}

/** 占位消息写入：按会话定位并 patch（会话已删 = 丢弃，防孤儿消息复活）。 */
function patchSessionMessage(
  sessionId: string,
  messageId: string,
  patch: (m: EditorChatMessage) => EditorChatMessage | null,
): void {
  useChatPanelStore.setState((state) => ({
    sessions: state.sessions.map((s) =>
      s.id !== sessionId
        ? s
        : {
            ...s,
            messages: s.messages.flatMap((m) => {
              if (m.id !== messageId) return [m];
              const next = patch(m);
              return next ? [next] : [];
            }),
          }
    ),
  }));
}

/** 托盘附件字节落仓库临时区（发送预检段；宿主发送与镜像 op 预处理共用，blob 不过跨窗口事件线）：
 *  成功后引用原地写回 pending.file。失败可见（通知）且不产生半发消息——调用方保留草稿原样返回。 */
async function materializePendings(
  pendings: PendingAttachment[],
  sessionKey: string,
): Promise<{ ok: true } | { ok: false }> {
  for (const p of pendings) {
    try {
      if (!p.file) {
        if (!p.blob) {
          // 托盘数据不完整（无引用也无字节）：不可达路径，留痕不静默丢弃
          console.warn("跳过无内容也无引用的托盘附件", p.id, p.filename);
          continue;
        }
        const ref = await writeTempAttachment("session", sessionKey, p.filename ?? "attachment", p.blob);
        p.file = ref;
      }
    } catch (e) {
      console.error("附件写入临时区失败", p.filename, e);
      const reason = e instanceof Error && e.message ? `：${e.message}` : "";
      useNotificationStore.getState().notify({
        level: "error",
        message: `附件「${p.filename ?? ""}」写入失败，消息未发送${reason}`,
      });
      return { ok: false };
    }
  }
  return { ok: true };
}

/**
 * 发送执行体（宿主 action 与镜像 op 共用）：预检（运行时/模型解析/附件落临时区）→ 会话创建
 * （新对话态时）→ 追加 user 消息与占位 → 交给 runExchange 轮转（fire-and-forget）。
 * managePanelState = 是否管理宿主面板自身的激活会话与草稿（镜像 op 不动宿主本地视图态，
 * 镜像侧由 op 响应的 createdSessionId 回落自身激活态）。
 */
async function runSend(params: {
  content: string;
  refs: EditorChatMessageRef[];
  pendings: PendingAttachment[];
  activeSessionId: string | null;
  draftAgentId: string | undefined;
  forcedSessionId?: string;
  managePanelState: boolean;
}): Promise<{ ok: boolean; error?: string; createdSessionId?: string }> {
  const trimmed = params.content.trim();
  if (
    (!trimmed && params.pendings.length === 0) ||
    useChatPanelStore.getState().streaming ||
    useChatPanelStore.getState().compacting ||
    useChatPanelStore.getState().sending
  ) {
    return { ok: false };
  }
  useChatPanelStore.setState({ sending: true });
  try {
    // 对话能力未启用（对话核心插件停用）：如实提示，不进入本轮（不创建空会话）
    const runtime = getChatRuntime();
    if (!runtime) return { ok: false, error: CHAT_UNAVAILABLE_TEXT };
    const resolved = resolveProviderModel(runtime);
    if (!resolved.ok) return { ok: false, error: resolved.error };

    // 确保有激活会话：新对话态（null）时创建会话并激活——标题/消息 .jsonl 路径在首条消息确定；
    // 新对话态选好的 draft Agent 随会话创建固化，随后清空。
    // 会话 id 先于附件落盘确定（附件临时目录按会话 id 归属）；镜像 op 的 forcedSessionId
    // 由镜像先行确定，保证其附件临时目录归属与宿主侧会话一致。
    // 新会话在附件全部落盘成功后才登记，落盘失败不残留无消息的空会话（孤儿临时目录由进仓兜底回收）。
    const existing = useChatPanelStore.getState().sessions.find((s) => s.id === params.activeSessionId) ?? null;
    const sessionId = existing?.id ?? params.forcedSessionId ?? crypto.randomUUID();

    // 附件字节落仓库临时区（`.atelyx/temp/<会话 key>/`）：消息只持久化路径引用
    const materialized = await materializePendings(params.pendings, sessionId);
    if (!materialized.ok) return { ok: false };
    const attachments: Attachment[] = [];
    for (const p of params.pendings) {
      if (!p.file) continue; // 预处理已跳过（无内容也无引用）
      attachments.push({
        kind: p.kind,
        payload: p.payload,
        mime: p.mime,
        filename: p.filename,
        file: p.file,
      });
    }

    let active = existing;
    const created = !active;
    if (!active) {
      const now = Date.now();
      active = {
        id: sessionId,
        title: prefix(trimmed, 16),
        file: chatMessageFilePath(sessionId),
        agentId: params.draftAgentId,
        messages: [],
        createdAt: now,
        updatedAt: now,
      };
      useChatPanelStore.setState({
        sessions: [...useChatPanelStore.getState().sessions, active],
        ...(params.managePanelState
          ? { activeSessionId: active.id, draftAgentId: undefined, error: null }
          : {}),
      });
      // 新会话：元数据侧车随首条消息落盘（新建 = 新文件，多设备并发创建互不覆盖）
      markMetaDirty(active.id);
    }

    // #引用（手动拖入）：只发文件路径——#标签 保留原位，消息开头拼「引用文件」路径块
    // （模型用 read_file 读取正文，不整文打进消息）。标签被用户手动删掉/文件缺失时跳过（扫描不到标签 = 该引用下沉丢弃，不记 refs）。
    const { text: finalContent, injectedFiles } = await injectNoteRefs(trimmed, params.refs);
    const injectedRefs: EditorChatMessageRef[] = injectedFiles
      .map((f) => params.refs.find((r) => r.file === f))
      .filter((r): r is EditorChatMessageRef => !!r);

    const userMsg: EditorChatMessage = {
      id: crypto.randomUUID(),
      role: "user",
      // 气泡显示原始输入；content 含「引用文件」路径块（与画布 displayContent 分离语义一致）
      content: finalContent,
      displayContent: trimmed,
      refs: injectedRefs.length ? injectedRefs : undefined,
      ...(attachments.length ? { attachments } : {}),
      createdAt: Date.now(),
    };

    // 预检到此全部完成：runExchange 内先置 streaming=true（守卫接力防双发），轮转本身
    // 不等待——send 及时返回让调用方清草稿，流式期间的输入不受轮末清空波及。
    void runExchange(
      active,
      userMsg,
      runtime,
      { provider: resolved.provider, model: resolved.model },
      resolved.reasoningEffort,
    ).catch((e) => {
      // 编排侧异常已被 runChatTurn 兜底交回 sink；此处只兜 runExchange 自身早退
      //（附件读回等已在内部消化），复位流式态防「转圈到天荒地老」
      console.error("面板对话轮编排失败", e);
      useChatPanelStore.setState({ streaming: false });
    });
    return created ? { ok: true, createdSessionId: sessionId } : { ok: true };
  } finally {
    useChatPanelStore.setState({ sending: false });
  }
}

/**
 * 重新生成执行体（宿主 action 与镜像 op 共用，按显式会话 id）：移除最后 assistant、按 refs 重建
 * 最后一条 user 消息注入后重发（同画布 regenerate 语义）。无可重建目标静默失败（与既有行为一致），
 * 可操作失败经返回串由调用方落宿主/镜像各自的 error 态。
 */
async function regenerateSession(sessionId: string | null): Promise<{ ok: boolean; error?: string }> {
  const s = useChatPanelStore.getState();
  if (s.streaming || s.compacting) return { ok: false };
  const session = s.sessions.find((x) => x.id === sessionId);
  if (!session) return { ok: false };
  const runtime = getChatRuntime();
  if (!runtime) return { ok: false, error: CHAT_UNAVAILABLE_TEXT };
  const list = session.messages;
  let lastUserIdx = -1;
  let lastAsstIdx = -1;
  for (let i = list.length - 1; i >= 0; i--) {
    if (lastAsstIdx < 0 && list[i].role === "assistant") lastAsstIdx = i;
    if (lastUserIdx < 0 && list[i].role === "user") lastUserIdx = i;
    if (lastUserIdx >= 0 && lastAsstIdx >= 0) break;
  }
  if (lastUserIdx < 0) return { ok: false };

  const resolved = resolveProviderModel(runtime);
  if (!resolved.ok) return { ok: false, error: resolved.error };

  const userMsg = list[lastUserIdx];
  // 重建引用：以原始输入（displayContent）为基底重拼「引用文件」路径块（同 send 语义）。
  // 不能以 userMsg.content（已含上次路径块）为基底——上次的路径块会把 #/@标签 位置
  // 整体推移，displayContent 的命中索引套在 content 上会错位（多个引用时尤甚）；
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
    useChatPanelStore.setState({
      sessions: useChatPanelStore.getState().sessions.map((x) =>
        x.id === session.id ? { ...x, messages: base, updatedAt: Date.now() } : x
      ),
    });
    schedulePersist(session.id);
  }
  await runExchange(
    { ...session, messages: base },
    { ...userMsg, content: rebuiltContent },
    runtime,
    { provider: resolved.provider, model: resolved.model },
    resolved.reasoningEffort,
  );
  return { ok: true };
}

/**
 * 手动压缩执行体（宿主 action 与镜像 op 共用，按显式会话 id）：与流式/重复压缩互斥。
 * 结果提示经返回串交调用方落各自 error 态（用户主动停止静默）。
 */
async function compactSessionById(sessionId: string | null): Promise<{ ok: boolean; error?: string }> {
  const s = useChatPanelStore.getState();
  if (s.streaming || s.compacting) return { ok: false };
  const session = s.sessions.find((x) => x.id === sessionId);
  if (!session) return { ok: false };
  const bound = nextCompactionBoundary(session.messages, session.compaction);
  if (!bound) {
    return {
      ok: false,
      error: session.compaction ? "已压缩到对话末尾，没有新增内容" : "没有可压缩的对话（需要至少一轮问答）",
    };
  }
  // 对话能力未启用（对话核心插件停用）：压缩属对话能力，如实提示
  const runtime = getChatRuntime();
  if (!runtime) return { ok: false, error: CHAT_UNAVAILABLE_TEXT };
  const resolved = resolveProviderModel(runtime);
  if (!resolved.ok) return { ok: false, error: resolved.error };
  // 控制器与 compacting 置位须在任何 await 之前，否则互斥守卫存在可插入的窗口
  const controller = new AbortController();
  abortController = controller;
  useChatPanelStore.setState({ error: null, compacting: session.id });
  try {
    // 摘要生成走对话核心能力（旧摘要随新内容一并重新总结，不照抄）；注解写回与锚点复核留在本 store
    const result = await runtime.compact({
      target: { provider: resolved.provider, model: resolved.model },
      messages: session.messages,
      ...(session.compaction ? { compaction: session.compaction } : {}),
      upToMessageId: bound.upToMessageId,
      ...(session.agentId !== undefined ? { agentId: session.agentId } : {}),
      signal: controller.signal,
    });
    if (!result.ok) {
      // aborted = 用户主动停止，静默收尾；其余给出可重试提示
      return result.aborted ? { ok: false } : { ok: false, error: result.message };
    }
    // 会话可能已被删除：写回前复核；锚点消息若已被回滚截断则丢弃本次结果
    // （写入死注解只会让下次请求白白退回全历史，静默浪费一次调用）
    const cur = useChatPanelStore.getState().sessions.find((x) => x.id === session.id);
    if (!cur) return { ok: false };
    if (!cur.messages.some((m) => m.id === bound.upToMessageId)) return { ok: false };
    useChatPanelStore.setState({
      sessions: useChatPanelStore.getState().sessions.map((x) =>
        x.id === session.id
          ? {
              ...x,
              compaction: {
                summary: result.summary,
                upToMessageId: bound.upToMessageId,
                messageCount: bound.messageCount,
                createdAt: Date.now(),
                providerId: result.providerId,
                model: result.model,
              },
            }
          : x,
      ),
    });
    // 注解在元数据侧车（.meta.json），随既有 debounce 写盘
    markMetaDirty(session.id);
    return { ok: true };
  } finally {
    if (abortController === controller) abortController = null;
    useChatPanelStore.setState({ compacting: null });
  }
}

/**
 * 重新命名执行体（宿主 action 与镜像 op 共用，按显式会话 id）：全量会话记录、立即请求、
 * 不受「话题自动命名」开关限制；结果反馈经返回串交调用方落各自 error 态（跳过静默）。
 */
async function renameSessionById(sessionId: string | null): Promise<{ ok: boolean; error?: string }> {
  const s = useChatPanelStore.getState();
  if (!sessionId || s.streaming || !s.sessions.some((x) => x.id === sessionId)) return { ok: false };
  const runtime = getChatRuntime();
  if (!runtime) return { ok: false, error: CHAT_UNAVAILABLE_TEXT };
  // 手动接管：中止本会话在途的自动命名请求（防同一会话重复请求；延迟中未发出的由 isNamed 二次校验兜底跳过）
  abortAutoTitle(sessionId);
  // 重新命名：全量会话记录（不截断）、立即请求（无 3s 防限流延迟——用户主动点击期待即时反馈）
  const result = await runtime.autoName(
    {
      getMessages: () => {
        const cur = useChatPanelStore.getState().sessions.find((x) => x.id === sessionId);
        return cur?.messages ?? [];
      },
      isNamed: () => false,
      applyTitle: (title) => applySessionTitle(sessionId, title),
    },
    sessionId,
    { delayMs: 0, maxChars: Infinity, ignoreToggle: true },
  );
  if (result === "ok") return { ok: true };
  const hasNamingConfig = !!useSettingsStore.getState().resolveAutoNamingModel(true);
  if (!hasNamingConfig) return { ok: false, error: "话题命名不可用：未配置默认模型或话题命名模型" };
  if (result === "failed") return { ok: false, error: "话题命名失败，请稍后重试" };
  return { ok: false };
}

/** 回滚执行体（宿主 action 与镜像 op 共用，按显式会话 id）：截断到指定 AI 回复（含），静默守卫。 */
function rollbackSession(sessionId: string | null, messageId: string): void {
  const s = useChatPanelStore.getState();
  if (!sessionId || s.streaming || s.compacting) return;
  const session = s.sessions.find((x) => x.id === sessionId);
  if (!session) return;
  const idx = session.messages.findIndex((m) => m.id === messageId);
  if (idx < 0 || idx === session.messages.length - 1) return;
  useChatPanelStore.setState({
    sessions: s.sessions.map((x) =>
      x.id === sessionId
        ? { ...x, messages: session.messages.slice(0, idx + 1), updatedAt: Date.now() }
        : x
    ),
  });
  schedulePersist(sessionId);
}

/** 会话级 Agent 设置执行体（宿主 action 激活分支与镜像 op 共用）。 */
function setSessionAgent(sessionId: string, agentId: string | undefined): void {
  useChatPanelStore.setState((state) => ({
    sessions: state.sessions.map((s) => (s.id === sessionId ? { ...s, agentId } : s)),
  }));
  // 会话级 Agent 变化写元数据侧车
  markMetaDirty(sessionId);
}

/**
 * 执行一轮流式对话（send 与 regenerate 共用）：
 * 追加 user 消息 + 空占位 assistant → 交给对话核心能力跑本轮（提示词/工具/流式/收尾/命名）→
 * 产出经写入器写回会话容器（本函数只管容器与落盘，编排在核心）。
 * 对话能力未启用时调用方在进入前拦下（见 send/regenerate）。
 */
async function runExchange(
  active: EditorChatSession,
  userMsg: EditorChatMessage,
  runtime: ChatRuntime,
  target: ChatTurnTarget,
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
  schedulePersist(active.id);

  // 附件不随消息内嵌（只持久化 `file` 引用）：发轮前按引用读回（压缩注解之外的消息才读），
  // 读到的内容回填消息缓存，同会话后续发送不重复读盘。按附件粒度降级：单个附件读不到只丢该附件
  // 不进请求（读取器内部提示），其余附件与对话照常；读回整段失败只记日志（历史原样发，附件缺内容）。
  // 切分只限定附件读回范围——runTurn 收到的历史必须是全量（其内部按注解定位锚点，
  // 预切分的列表找不到锚点会让压缩摘要静默失效）。
  try {
    const { kept } = splitByCompaction(updated.messages, active.compaction);
    const resolvedHistory = await resolveMessageAttachments(kept, (att) =>
      readMessageAttachment(att.file as string, att.kind),
    );
    if (resolvedHistory !== kept) {
      // 只回填附件缓存（整体替换会话数组会把 await 期间到达的外部合并回退成旧快照）
      useChatPanelStore.setState((state) => {
        const resolvedById = new Map(resolvedHistory.map((m) => [m.id, m]));
        let changed = false;
        const sessions = state.sessions.map((s) => {
          if (s.id !== active!.id) return s;
          const merged = s.messages.map((m) => {
            const resolvedMsg = resolvedById.get(m.id);
            const atts = m.attachments;
            if (!resolvedMsg || !atts?.length) return m;
            let filled = false;
            const attachments = atts.map((a, i) => {
              if (a.payload || !a.file) return a;
              // 按同下标取补齐结果（resolve 保序）并核对引用，防期间列表变化套错内容
              const candidate = resolvedMsg.attachments?.[i];
              if (!candidate?.payload || candidate.file !== a.file) return a;
              filled = true;
              return { ...a, payload: candidate.payload };
            });
            if (!filled) return m;
            changed = true;
            return { ...m, attachments };
          });
          if (!changed) return s;
          return { ...s, messages: merged };
        });
        if (!changed) return state;
        return { sessions };
      });
    }
  } catch (e) {
    console.error("面板消息附件读回失败，附件不进本轮请求", e);
  }

  const controller = new AbortController();
  abortController = controller;

  // 写入器：核心把本轮产出交回会话容器；存在性守卫（会话已删）与落盘调度在此处理
  const sink: ChatTurnSink = {
    update: ({ content, steps }) => {
      patchSessionMessage(active.id, asstMsg.id, (m) => ({ ...m, content, steps }));
    },
    notice: (message) => useChatPanelStore.setState({ error: message }),
    finish: (result) => {
      if (result.removed) {
        // 空回复：移除占位，避免残留空气泡
        patchSessionMessage(active.id, asstMsg.id, () => null);
      } else {
        patchSessionMessage(active.id, asstMsg.id, (m) => ({
          ...m,
          content: result.content,
          steps: result.steps,
        }));
      }
      useChatPanelStore.setState({ streaming: false });
      schedulePersist(active.id);
      if (abortController === controller) abortController = null;
    },
    fail: (err) => {
      // 不静默降级：占位写入 [错误]（下次请求历史过滤，不污染上下文）
      patchSessionMessage(active.id, asstMsg.id, (m) => ({
        ...m,
        content: m.content || `${ERROR_PREFIX} ${err.message}`,
      }));
      useChatPanelStore.setState({ streaming: false });
      schedulePersist(active.id);
      if (abortController === controller) abortController = null;
    },
  };

  await runtime.runTurn({
    targetId: active.id,
    target,
    // 全量历史（含刚追加的 user 消息）：消息本体即中性字段，核心内部按压缩注解切分
    history: updated.messages,
    ...(reasoningEffort ? { reasoningEffort } : {}),
    ...(updated.agentId !== undefined ? { agentId: updated.agentId } : {}),
    ...(active.compaction ? { compaction: active.compaction } : {}),
    // 面板把当前打开的笔记当隐式上下文（画布对话节点用显式 #引用/连边，不开此开关）
    includeCurrentNote: true,
    signal: controller.signal,
    sink,
    naming: sessionNamingTarget(active.id),
  });
}

export const useChatPanelStore = create<ChatPanelState>((set, get) => ({
  sessions: [],
  activeSessionId: null,
  streaming: false,
  sending: false,
  modelOverride: null,
  effortOverride: null,
  pendingMentions: [],
  draftAgentId: BUILTIN_AGENT_CHAT_ID,
  pendingRewrites: [],
  error: null,
  compacting: null,
  loaded: false,
  sessionVaultKey: "",
  persistError: null,

  load: async (force = false) => {
    // 传输初始化（每次装载重评估）：按资格与探测评定 Rust 真源/事件线模式并安装对应角色
    //（薄客户端 + 主窗口执行体，或事件线宿主/镜像）。资格 = 本地仓库身份：容器持久态收口
    // Rust 仅覆盖本地仓库，空间仓库会话存储在服务端，走事件线（宿主写盘链经内容 I/O 分派）。
    await initChatContainerTransport({
      mirror: {
        applySnapshot: applyContainerSnapshot,
        applyFragments: applyContainerFragments,
      },
      ...(isContainerHost
        ? {
            executor: {
              getView: () => containerView(useChatPanelStore.getState()),
              subscribe: (listener) => useChatPanelStore.subscribe(listener),
              executeOp: applyContainerOp,
            },
            host: {
              getView: () => containerView(useChatPanelStore.getState()),
              subscribe: (listener) => useChatPanelStore.subscribe(listener),
              applyOp: applyContainerOp,
            },
          }
        : {}),
      rustTruthEligible: useAppStore.getState().vaultIdentity?.kind === "local",
    });
    // 薄客户端/镜像：load = 向真源拉基线快照（幂等守卫照常，force 重拉）。真源不可达重试
    // 一次后如实报错（loaded 保持 false，重进面板/切仓库可再触发）。
    // 判定与 isLocalModeHost 解耦：执行体意图执行期间装载路径不变（仍是薄客户端拉快照）。
    if (!(isContainerHost && !isRustTruthMode())) {
      if (!force && get().loaded) return;
      // 目标身份 = 调用时激活身份；await 快照期间身份可能切换（新 load 会重评模式并自拉
      // 基线），迟到快照属旧身份，应用即跨仓库污染
      const targetKey = activeIdentityKey();
      let lastError: unknown = null;
      for (let attempt = 0; attempt < 2; attempt++) {
        try {
          const snapshot = await requestChatContainerSnapshot();
          if (activeIdentityKey() !== targetKey) return;
          // 快照装载是外部变更：执行体差分基准随之推进，不得当本地改动回提交真源
          foldChatContainerExternal(() => applyContainerSnapshot(snapshot));
          return;
        } catch (e) {
          lastError = e;
        }
      }
      console.error("会话容器基线拉取失败", lastError);
      set({ error: isRustTruthMode() ? "会话容器暂不可达（真源未响应）" : "会话容器暂不可达（主窗口未响应）" });
      return;
    }
    // 目标身份 = 调用时的激活仓库身份（进仓流程在激活完成后才分发 onVaultEntered）。
    // 幂等：内存会话已属于目标身份（sessionVaultKey 为权威）则跳过——防多调用方重复读盘
    // + 二次 load 覆盖进行中会话改动；真实换仓库（sessionVaultKey ≠ 目标）必重载；
    // `force`（selectVault/selectSpace 真实切换传入）绕过守卫强制重读盘。
    const st = useChatPanelStore.getState();
    const targetKey = activeIdentityKey();
    const prevKey = st.sessionVaultKey;
    if (!force && st.loaded && prevKey === targetKey) {
      return;
    }
    // 清残留 debounce timer（防旧仓库 timer 写新仓库状态）+ 脏会话标记 + 退避重试
    persistCtl.cancel();
    cancelPersistRetry();
    dirtyMessageFiles.clear();
    dirtyMetaSessions.clear();
    messageBaseline.clear();
    autoNamedSessions.clear();
    // 附件负缓存/提示去重按仓库相对路径记账：换仓库后同路径指向不同内容，一并失效
    nonTextAttachmentRefs.clear();
    reportedAttachmentReadFailures.clear();
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
      persistError: null,
      ...(force || prevKey !== targetKey ? { sessions: [], activeSessionId: null } : {}),
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
          ...(row.meta?.compaction ? { compaction: row.meta.compaction } : {}),
          file: row.file,
          createdAt: messages[0]?.createdAt ?? 0,
          updatedAt: messages[messages.length - 1]?.createdAt ?? 0,
          messages,
        });
      }
      // 切仓库竞态守卫：后台填充链与面板快速切换并发时，
      // 旧仓库读取结果不得覆盖新仓库的会话（按身份键比对——空间切换 root 恒 null，root 比对失效）
      if (activeIdentityKey() !== targetKey) return;
      // 新仓库干净状态：清脏标记（旧仓库未写完的改动不再写回）
      dirty = false;
      set({
        sessions,
        // 新对话态：打开面板默认是空对话，历史会话从历史浮层手动打开；
        // Agent draft 默认预置「对话」（内存态），切仓库重置
        activeSessionId: null,
        sessionVaultKey: targetKey,
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
      if (activeIdentityKey() !== targetKey) return;
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
    // 附件内容是运行时缓存（落盘只存引用）：重开软件后首次打开会话按引用读回，恢复图片显示
    void hydrateSessionAttachments(id);
  },

  deleteSession: (id) => {
    // 镜像：删除意图转发宿主（会话列表随增量折叠收回，文件清理在宿主）；激活回落是本窗口
    // 本地状态（宿主增量不管各窗口激活态），先行本地处理
    if (!isLocalModeHost()) {
      const active = get().activeSessionId;
      set(active === id ? { activeSessionId: null, error: null } : { error: null });
      void sendChatContainerOp({ kind: "delete", sessionId: id }).catch((e) =>
        console.error("镜像删除会话转发失败", e),
      );
      return;
    }
    const target = get().sessions.find((s) => s.id === id);
    const sessions = get().sessions.filter((s) => s.id !== id);
    dirtyMessageFiles.delete(id); // 不再重写已删会话的消息 .jsonl
    dirtyMetaSessions.delete(id); // 不再重写已删会话的元数据侧车
    messageBaseline.delete(id);
    if (target?.file) {
      // 立即删消息 .jsonl + 元数据侧车（异步，失败仅记日志——删除 = 删文件）
      void deleteChatSessionMeta(chatMetaFilePath(id)).catch((e) =>
        console.error("删除会话元数据文件失败", e),
      );
      // 顺手清理该会话的任务清单侧车（孤儿清理；best-effort，失败静默）
      void deleteAgentTodos(id).catch(() => {});
      // 回收该会话的未入库附件：等消息 .jsonl 删除尘埃落定后再扫（会话已删 = 引用集合为空，
      // 整目录可清；best-effort——读盘失败保守不删，残留由进仓兜底回收）
      void deleteChatMessages(target.file)
        .catch((e) => console.error("删除会话消息文件失败", e))
        .then(() =>
          cleanupSessionTempAttachments(id, target.file).catch((e) =>
            console.error("回收会话临时附件失败", id, e),
          ),
        );
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

  importSession: async (messages, opts) => {
    // 镜像：留档写意图转发宿主（登记随增量折叠收回），返回新建 id
    if (!isLocalModeHost()) {
      if (!get().loaded) await get().load();
      if (!get().loaded) throw new Error("会话容器未就绪（基线拉取未完成），请重试");
      const response = await sendChatContainerOp({ kind: "import", messages, opts });
      if (!response.ok) throw new Error(response.error);
      return response.value as { id: string };
    }
    // 先确保本仓库会话已读盘：写盘守卫 loaded=false 不落盘，未加载先读（幂等，同仓库跳过）；
    // 读盘竞态被丢弃（切仓库在途）时 loaded 仍为 false——如实抛错，不留一个永不落盘的内存会话
    if (!get().loaded) await get().load();
    if (!get().loaded) throw new Error("会话读盘未完成（仓库切换中），请重试");
    const now = Date.now();
    const panelMessages = toPanelMessages(messages, now);
    const firstUser = panelMessages.find((m) => m.role === "user");
    const derived = firstUser ? prefix(firstUser.displayContent ?? firstUser.content, 16) : "";
    const title = opts?.title ?? (derived !== "" ? derived : undefined);
    const id = crypto.randomUUID();
    const session: EditorChatSession = {
      id,
      ...(title !== undefined ? { title } : {}),
      ...(opts?.agentId !== undefined ? { agentId: opts.agentId } : {}),
      file: chatMessageFilePath(id),
      messages: panelMessages,
      createdAt: now,
      updatedAt: panelMessages[panelMessages.length - 1].createdAt,
    };
    // 不改面板激活会话：登记 = 留档，不打断用户正在进行的对话
    set({ sessions: [...get().sessions, session] });
    schedulePersist(id);
    markMetaDirty(id);
    return { id };
  },

  appendMessages: async (sessionId, messages) => {
    // 镜像：追加写意图转发宿主（去重/时间基线由宿主按真源会话计算）
    if (!isLocalModeHost()) {
      if (!get().loaded) await get().load();
      if (!get().loaded) throw new Error("会话容器未就绪（基线拉取未完成），请重试");
      const response = await sendChatContainerOp({ kind: "append", sessionId, messages });
      if (!response.ok) throw new Error(response.error);
      return;
    }
    if (!get().loaded) await get().load();
    if (!get().loaded) throw new Error("会话读盘未完成（仓库切换中），请重试");
    const session = get().sessions.find((s) => s.id === sessionId);
    if (!session) throw new Error(`会话不存在：${sessionId}`);
    const last = session.messages[session.messages.length - 1];
    const baseTime = Math.max(last ? last.createdAt + 1 : 0, Date.now());
    // 追加去重含既有消息：插件重载后其模块计数器归零，会话 id 却跨重载复用，
    // 不含既有 id 会在同一会话内产生重复消息 id（压缩锚点/前端 key 均按它定位）
    const panelMessages = toPanelMessages(
      messages,
      baseTime,
      session.messages.map((m) => m.id),
    );
    useChatPanelStore.setState((state) => ({
      sessions: state.sessions.map((s) =>
        s.id !== sessionId
          ? s
          : {
              ...s,
              messages: [...s.messages, ...panelMessages],
              updatedAt: panelMessages[panelMessages.length - 1].createdAt,
            },
      ),
    }));
    schedulePersist(sessionId);
  },

  listSessions: async () => {
    if (!get().loaded) await get().load();
    if (!get().loaded) throw new Error("会话读盘未完成（仓库切换中），请重试");
    return get()
      .sessions.map((s) => ({ id: s.id, ...(s.title !== undefined ? { title: s.title } : {}), updatedAt: s.updatedAt }))
      .sort((a, b) => b.updatedAt - a.updatedAt);
  },

  readSession: async (sessionId) => {
    if (!get().loaded) await get().load();
    if (!get().loaded) throw new Error("会话读盘未完成（仓库切换中），请重试");
    // 附件内容是运行时缓存：容器消费方（速问窗口等）无读回通道，读出前先按引用水合；
    // 水合会替换 store 内该会话的消息数组，返回值须在水合完成后重新取会话。
    await hydrateSessionAttachments(sessionId);
    const session = get().sessions.find((s) => s.id === sessionId);
    if (!session) throw new Error(`会话不存在：${sessionId}`);
    return {
      id: session.id,
      ...(session.title !== undefined ? { title: session.title } : {}),
      ...(session.agentId !== undefined ? { agentId: session.agentId } : {}),
      ...(session.compaction ? { compaction: session.compaction } : {}),
      // 面板消息 → 中性消息：面板特有字段（refs/notices/error）不出契约；
      // 附件随契约携带水合后的内容缓存（payload），供容器消费方展示图片缩略
      messages: session.messages.map((m) => ({
        id: m.id,
        role: m.role,
        content: m.content,
        ...(m.displayContent !== undefined ? { displayContent: m.displayContent } : {}),
        ...(m.steps?.length ? { steps: m.steps } : {}),
        ...(m.attachments?.length ? { attachments: m.attachments.map(({ kind, mime, filename, file, payload }) => ({ kind, mime, ...(filename !== undefined ? { filename } : {}), ...(file !== undefined ? { file } : {}), ...(payload !== undefined ? { payload } : {}) })) } : {}),
      })),
    };
  },

  createSession: async (opts) => {
    // 镜像：新建意图转发宿主（不改激活会话的语义由宿主保持），返回新建 id
    if (!isLocalModeHost()) {
      if (!get().loaded) await get().load();
      if (!get().loaded) throw new Error("会话容器未就绪（基线拉取未完成），请重试");
      const response = await sendChatContainerOp({ kind: "create", opts });
      if (!response.ok) throw new Error(response.error);
      return response.value as { id: string };
    }
    if (!get().loaded) await get().load();
    if (!get().loaded) throw new Error("会话读盘未完成（仓库切换中），请重试");
    const now = Date.now();
    const id = crypto.randomUUID();
    const session: EditorChatSession = {
      id,
      ...(opts?.title !== undefined ? { title: opts.title } : {}),
      ...(opts?.agentId !== undefined ? { agentId: opts.agentId } : {}),
      file: chatMessageFilePath(id),
      messages: [],
      createdAt: now,
      updatedAt: now,
    };
    // 不改面板激活会话：新建 = 留出空容器，首条消息落盘时 .jsonl 才实际出现
    set({ sessions: [...get().sessions, session] });
    markMetaDirty(id);
    return { id };
  },

  setSessionTitle: async (sessionId, title) => {
    // 镜像：改名意图转发宿主（同名冲突/存在性校验在宿主真源）
    if (!isLocalModeHost()) {
      if (!get().loaded) await get().load();
      if (!get().loaded) throw new Error("会话容器未就绪（基线拉取未完成），请重试");
      const response = await sendChatContainerOp({ kind: "setTitle", sessionId, title });
      if (!response.ok) throw new Error(response.error);
      return;
    }
    if (!get().loaded) await get().load();
    if (!get().loaded) throw new Error("会话读盘未完成（仓库切换中），请重试");
    const session = get().sessions.find((s) => s.id === sessionId);
    if (!session) throw new Error(`会话不存在：${sessionId}`);
    useChatPanelStore.setState((state) => ({
      sessions: state.sessions.map((s) => (s.id !== sessionId ? s : { ...s, title })),
    }));
    markMetaDirty(sessionId);
  },

  /** 删除面板会话（同源；连带消息 .jsonl / 元数据侧车 / 任务清单侧车，见 deleteSession）。 */
  deleteSessionExternal: async (sessionId: string) => {
    // 镜像：删除意图转发宿主（连带清理在宿主执行）
    if (!isLocalModeHost()) {
      if (!get().loaded) await get().load();
      if (!get().loaded) throw new Error("会话容器未就绪（基线拉取未完成），请重试");
      const response = await sendChatContainerOp({ kind: "deleteExternal", sessionId });
      if (!response.ok) throw new Error(response.error);
      return;
    }
    if (!get().loaded) await get().load();
    if (!get().loaded) throw new Error("会话读盘未完成（仓库切换中），请重试");
    get().deleteSession(sessionId);
  },

  send: async (content, refs = [], pendings = []) => {
    // 镜像：预检在本窗口完成后转发 op（附件字节先落本仓库临时区，blob 不过事件线）；
    // 读己之写 = 响应片段先折叠再返回，新建会话的激活回落同步完成。
    if (!isLocalModeHost()) {
      const trimmed = content.trim();
      if ((!trimmed && pendings.length === 0) || get().streaming || get().compacting || get().sending) {
        return false;
      }
      // Rust 真源模式不占 sending：意图回到本窗口（执行体）执行 runSend，由其 sending 守卫
      // 串行化并发意图；转发期间占位会自挡本窗口意图。事件线镜像无本地执行，占位防双击。
      // 门控进 tried 时捕获：模式在 await 期间翻转时 finally 仍按进入时决定复位，防 sending 卡死
      const gateSending = !isRustTruthMode();
      if (gateSending) set({ sending: true });
      try {
        const sessionKey = get().activeSessionId ?? crypto.randomUUID();
        const materialized = await materializePendings(pendings, sessionKey);
        if (!materialized.ok) return false;
        const response = await sendChatContainerOp({
          kind: "send",
          activeSessionId: get().activeSessionId,
          forcedSessionId: sessionKey,
          draftAgentId: get().draftAgentId,
          content,
          refs,
          pendings: stripPendingsForWire(pendings),
        });
        if (!response.ok) return false;
        const result = (response.value ?? {}) as { ok?: boolean; error?: string };
        if (result.error) set({ error: result.error });
        if (result.ok) {
          // 附件运行时缓存不随容器广播（进线载荷剥离 payload）：新会话按引用立即水合恢复预览
          if (response.createdSessionId) {
            set({ activeSessionId: response.createdSessionId, draftAgentId: undefined });
          }
          const target = response.createdSessionId ?? get().activeSessionId;
          if (target) void hydrateSessionAttachments(target);
        }
        return result.ok ?? false;
      } catch (e) {
        console.error("镜像发送转发失败", e);
        return false;
      } finally {
        if (gateSending) set({ sending: false });
      }
    }
    const result = await runSend({
      content,
      refs,
      pendings,
      activeSessionId: get().activeSessionId,
      draftAgentId: get().draftAgentId,
      managePanelState: true,
    });
    if (!result.ok && result.error) set({ error: result.error });
    return result.ok;
  },

  regenerate: async () => {
    // 镜像：重建意图转发宿主（截断写经宿主真源，防陈旧副本覆盖）；软失败提示随响应回传
    if (!isLocalModeHost()) {
      const id = get().activeSessionId;
      if (!id || get().streaming || get().compacting) return;
      try {
        const result = opValueResult(await sendChatContainerOp({ kind: "regenerate", sessionId: id }));
        if (!result.ok && result.error) set({ error: result.error });
      } catch (e) {
        set({ error: e instanceof Error ? e.message : String(e) });
      }
      return;
    }
    const result = await regenerateSession(get().activeSessionId);
    if (!result.ok && result.error) set({ error: result.error });
  },

  compactSession: async () => {
    // 镜像：压缩意图转发宿主（模型请求在宿主执行；互斥守卫以宿主状态为准，本地守卫只省无效转发）
    if (!isLocalModeHost()) {
      const id = get().activeSessionId;
      if (!id || get().streaming || get().compacting) return;
      try {
        const result = opValueResult(await sendChatContainerOp({ kind: "compact", sessionId: id }));
        if (!result.ok && result.error) set({ error: result.error });
      } catch (e) {
        set({ error: e instanceof Error ? e.message : String(e) });
      }
      return;
    }
    const result = await compactSessionById(get().activeSessionId);
    if (!result.ok && result.error) set({ error: result.error });
  },

  renameSession: async () => {
    // 镜像：命名意图转发宿主（模型请求在宿主执行，标题写回经真源广播）
    if (!isLocalModeHost()) {
      const id = get().activeSessionId;
      if (!id || get().streaming) return;
      try {
        const result = opValueResult(await sendChatContainerOp({ kind: "rename", sessionId: id }));
        if (!result.ok && result.error) set({ error: result.error });
      } catch (e) {
        set({ error: e instanceof Error ? e.message : String(e) });
      }
      return;
    }
    const result = await renameSessionById(get().activeSessionId);
    if (!result.ok && result.error) set({ error: result.error });
  },

  rollbackTo: (messageId) => {
    const s = get();
    const id = s.activeSessionId;
    if (!id || s.streaming || s.compacting) return;
    // 镜像：截断写转发宿主（陈旧副本的整文件重写是跨窗口丢数据的根源，截断只发生在真源）
    if (!isLocalModeHost()) {
      void sendChatContainerOp({ kind: "rollback", sessionId: id, messageId }).catch((e) =>
        console.error("镜像回滚转发失败", e),
      );
      return;
    }
    rollbackSession(id, messageId);
  },

  stop: () => {
    // 镜像：中止意图转发宿主（abortController 持有在宿主；本地流式态随 status 增量收回）
    if (!isLocalModeHost()) {
      void sendChatContainerOp({ kind: "stop" }).catch((e) =>
        console.error("镜像停止转发失败", e),
      );
      return;
    }
    abortController?.abort();
  },

  setAgentId: (id) => {
    const current = get();
    if (!isLocalModeHost()) {
      if (current.activeSessionId) {
        void sendChatContainerOp({
          kind: "setAgentId",
          sessionId: current.activeSessionId,
          agentId: id,
        }).catch((e) => console.error("镜像设置会话 Agent 转发失败", e));
      } else {
        // 新对话态：暂存待用（本窗口本地状态），发送时随 op 固化
        set({ draftAgentId: id });
      }
      return;
    }
    if (current.activeSessionId) {
      setSessionAgent(current.activeSessionId, id);
    } else {
      // 新对话态：暂存待用，发送首条消息创建会话时固化（见 send）
      set({ draftAgentId: id });
    }
  },

  setModelOverride: (ov) => {
    set({ modelOverride: ov });
    // 镜像：覆盖随 op 同步宿主（send 的模型解析在宿主做）；持久化（设备偏好）归宿主，
    // 宿主回声经 status 增量折叠（同值幂等）
    if (!isLocalModeHost()) {
      void sendChatContainerOp({ kind: "setModelOverride", ov }).catch((e) =>
        console.error("镜像模型覆盖转发失败", e),
      );
      return;
    }
    markOverridesDirty();
  },

  setEffortOverride: (effort) => {
    set({ effortOverride: effort });
    if (!isLocalModeHost()) {
      void sendChatContainerOp({ kind: "setEffortOverride", effort }).catch((e) =>
        console.error("镜像推理力度覆盖转发失败", e),
      );
      return;
    }
    markOverridesDirty();
  },

  clearError: () => set({ error: null }),

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

  flush: () => {
    // Rust 真源模式：写盘链在真源侧，flush 请求真源立即写（resolve 不代表全部写成功，失败可见+退避重试）
    if (isRustTruthMode()) return flushChatContainerTruth();
    // 镜像不持有写盘链：容器变更已随 op 落在宿主，退出时无盘可刷
    if (!isLocalModeHost()) return Promise.resolve();
    // 无本地改动不写盘：外部删除会话文件后切仓库/退出，不把内存副本写回（覆盖删除）
    if (!dirty) return Promise.resolve();
    // 归属校验在 persistNow 内按身份键做（当前激活身份 ≠ 内存会话所属身份 → 不写）
    return persistCtl.flush();
  },
}));

// ===== 容器 op 分派与角色安装 =====

/**
 * 容器 op → 容器动作的唯一分派点：镜像写意图在此落回真源执行体（复用上述动作，执行期间
 * 按本地路径运行——见 executingIntentOp），产出随 op 响应回传发起窗口：value = 对应容器面
 * 动作的返回值，createdSessionId 供其回落自身激活态。
 */
async function applyContainerOp(op: ChatContainerOp): Promise<ChatOpOutcome> {
  const outer = executingIntentOp;
  executingIntentOp = true;
  try {
    return await applyContainerOpInner(op);
  } finally {
    executingIntentOp = outer;
  }
}

async function applyContainerOpInner(op: ChatContainerOp): Promise<ChatOpOutcome> {
  const s = useChatPanelStore.getState();
  switch (op.kind) {
    case "send": {
      const result = await runSend({
        content: op.content,
        refs: op.refs,
        pendings: op.pendings,
        activeSessionId: op.activeSessionId,
        draftAgentId: op.draftAgentId,
        forcedSessionId: op.forcedSessionId,
        managePanelState: false,
      });
      return {
        value: result.ok ? { ok: true } : { ok: false, ...(result.error ? { error: result.error } : {}) },
        ...(result.createdSessionId !== undefined ? { createdSessionId: result.createdSessionId } : {}),
      };
    }
    case "regenerate":
      return { value: await regenerateSession(op.sessionId) };
    case "compact":
      return { value: await compactSessionById(op.sessionId) };
    case "rename":
      return { value: await renameSessionById(op.sessionId) };
    case "stop":
      s.stop();
      return {};
    case "delete":
      s.deleteSession(op.sessionId);
      return {};
    case "rollback":
      rollbackSession(op.sessionId, op.messageId);
      return {};
    case "setAgentId":
      setSessionAgent(op.sessionId, op.agentId);
      return {};
    case "setModelOverride":
      s.setModelOverride(op.ov);
      return {};
    case "setEffortOverride":
      s.setEffortOverride(op.effort);
      return {};
    case "import":
      return { value: await s.importSession(op.messages, op.opts) };
    case "append":
      await s.appendMessages(op.sessionId, op.messages);
      return {};
    case "create":
      return { value: await s.createSession(op.opts) };
    case "setTitle":
      await s.setSessionTitle(op.sessionId, op.title);
      return {};
    case "deleteExternal":
      await s.deleteSessionExternal(op.sessionId);
      return {};
  }
}
