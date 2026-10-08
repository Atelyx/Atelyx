/**
 * 会话容器跨窗口线：传输双模式——宿主-镜像事件线（主窗口单写者独占写盘链）与 Rust 真源
 * （Rust 持有持久态与写盘链，全窗口薄客户端，主窗口兼执行体）。差分与折叠为纯函数，
 * 事件线传输为 Tauri event，Rust 真源传输为 invoke + Rust 广播，对外 API 同形。
 */
import { invoke } from "@tauri-apps/api/core";
import { emit, emitTo, listen } from "@tauri-apps/api/event";
import { emitPluginEvent } from "@/services/cordis/events";
import { getCurrentWindowLabel } from "@/services/window";
import { CHAT_HISTORY_DIR, CHAT_MESSAGE_EXT } from "@/constants/editorChats";
import type {
  ChatTurnMessage,
  ConversationCompaction,
  EditorChatMessage,
  EditorChatMessageRef,
  EditorChatModelOverride,
  EditorChatSession,
  PendingAttachment,
  ReasoningEffort,
} from "@/types";

/** 宿主窗口 label（主窗口；镜像窗口经 emitTo 定向投递请求）。 */
const HOST_WINDOW_LABEL = "main";
const REQUEST_EVENT = "chat-container-request";
const RESPONSE_EVENT = "chat-container-response";
const DELTA_EVENT = "chat-container-delta";
/** Rust 真源 → 执行体（主窗口）的意图事件（执行 op 定向投递；经 intent_result 回填）。 */
const INTENT_EVENT = "chat-container-intent";

/** 增量广播的合并节奏（流式期间的镜像跟随延迟上限；与 UI 帧率感知平衡）。 */
const DELTA_FLUSH_MS = 100;
/** 镜像基线拉取超时（宿主 = 主窗口托盘驻留，常态即时响应；超时即宿主不可达）。 */
const SNAPSHOT_TIMEOUT_MS = 3000;
/** 自身 opId 的回声抑制保留窗（覆盖 op 响应之后的增量广播回声到达）。 */
const OPID_RETENTION_MS = 30_000;

// ---------- 载荷类型 ----------

/** 宿主容器视图（差分与快照的状态面；镜像本地的对应字段同形）。 */
export interface ChatContainerView {
  sessions: EditorChatSession[];
  streaming: boolean;
  compacting: string | null;
  persistError: { message: string; at: number } | null;
  modelOverride: EditorChatModelOverride | null;
  effortOverride: ReasoningEffort | null;
  sessionVaultKey: string;
}

/** 会话元数据增量片段（按 id 折叠；null = 清除——会话字段以 undefined 表示「无」，线上需显式区分）。 */
export interface ChatSessionMetaFragment {
  id: string;
  file?: string;
  title?: string | null;
  agentId?: string | null;
  compaction?: ConversationCompaction | null;
  createdAt?: number;
  updatedAt?: number;
  removed?: boolean;
}

/** 消息增量：upserts 按 id 原位替换或末尾追加；keepCount = 截断为前 N 条（回滚/重新生成）。 */
export interface ChatMessageDelta {
  sessionId: string;
  upserts: EditorChatMessage[];
  keepCount?: number;
}

/** 容器全局面状态增量（字段级，仅携带变化项）。 */
export interface ChatContainerStatusFragment {
  streaming?: boolean;
  compacting?: string | null;
  persistError?: { message: string; at: number } | null;
  modelOverride?: EditorChatModelOverride | null;
  effortOverride?: ReasoningEffort | null;
  sessionVaultKey?: string;
}

/** 一次差分产出的增量片段（seq 由宿主在广播时统一分配）。 */
export interface ChatDeltaFragments {
  metas: ChatSessionMetaFragment[];
  messages: ChatMessageDelta[];
  status: ChatContainerStatusFragment | null;
  /** 插件事件载荷（chat:sessions-changed）的会话 id 分组。 */
  messageSessionIds: string[];
  metaSessionIds: string[];
  deletedIds: string[];
}

/** 宿主 → 全部窗口的增量广播（seq 单调；镜像仅应用 seq 更大者）。 */
export interface ChatContainerDelta extends ChatDeltaFragments {
  seq: number;
  /** 引起本批变更的镜像 opId（宿主自身改动 = 空）；镜像据此抑制自身回声的插件事件转发。 */
  opOwners: string[];
}

/** 镜像 boot 拉取的容器基线快照（seq = 已广播的最新一拍的序号）。 */
export interface ChatContainerSnapshot {
  seq: number;
  sessionVaultKey: string;
  sessions: EditorChatSession[];
  streaming: boolean;
  compacting: string | null;
  persistError: { message: string; at: number } | null;
  modelOverride: EditorChatModelOverride | null;
  effortOverride: ReasoningEffort | null;
}

/** 镜像 → 宿主的写意图（全部容器变更都经宿主真源应用）。 */
export type ChatContainerOp =
  | {
      kind: "send";
      /** 镜像侧激活会话（null = 新对话态，宿主按 forcedSessionId 新建）。 */
      activeSessionId: string | null;
      /** 新会话 id（镜像先行确定，附件临时目录归属与宿主落盘一致）。 */
      forcedSessionId: string;
      /** 镜像侧新对话态的待用 Agent（随会话创建固化）。 */
      draftAgentId: string | undefined;
      content: string;
      refs: EditorChatMessageRef[];
      /** 附件已由镜像落临时区（file 就位、无 blob），字节不过事件线。 */
      pendings: PendingAttachment[];
    }
  | { kind: "regenerate"; sessionId: string }
  | { kind: "stop" }
  | { kind: "compact"; sessionId: string }
  | { kind: "rename"; sessionId: string }
  | { kind: "rollback"; sessionId: string; messageId: string }
  | { kind: "delete"; sessionId: string }
  | { kind: "setAgentId"; sessionId: string; agentId: string | undefined }
  | { kind: "setModelOverride"; ov: EditorChatModelOverride | null }
  | { kind: "setEffortOverride"; effort: ReasoningEffort | null }
  | { kind: "import"; messages: ChatTurnMessage[]; opts?: { title?: string; agentId?: string } }
  | { kind: "append"; sessionId: string; messages: ChatTurnMessage[] }
  | { kind: "create"; opts?: { title?: string; agentId?: string } }
  | { kind: "setTitle"; sessionId: string; title: string }
  | { kind: "deleteExternal"; sessionId: string };

/** op 的应用产出（value = 对应容器面动作的返回值；createdSessionId 供镜像回落自身激活态）。 */
export interface ChatOpOutcome {
  value?: unknown;
  createdSessionId?: string;
}

export type ChatContainerResponse =
  | { requestId: string; kind: "snapshot"; snapshot: ChatContainerSnapshot }
  | {
      requestId: string;
      kind: "op";
      ok: true;
      value?: unknown;
      createdSessionId?: string;
      fragments: ChatDeltaFragments;
    }
  | { requestId: string; kind: "op"; ok: false; error: string };

/** op 响应（sendChatContainerOp 的解析结果）。 */
export type ChatContainerOpResponse = Extract<ChatContainerResponse, { kind: "op" }>;

type ChatContainerRequest =
  | { kind: "snapshot"; requestId: string; from: string }
  | { kind: "op"; requestId: string; from: string; op: ChatContainerOp };

// ---------- 角色判定 ----------

/** 本窗口在容器体系中的角色：主窗口 = 宿主（含非 Tauri 环境降级，行为与单窗口一致）。 */
export function isChatContainerHost(): boolean {
  try {
    return getCurrentWindowLabel() === HOST_WINDOW_LABEL;
  } catch {
    return true;
  }
}

function currentWindowLabel(): string {
  try {
    return getCurrentWindowLabel();
  } catch {
    return "";
  }
}

// ---------- 纯函数：进线载荷整备 / 差分 / 折叠 ----------

/** 剥离附件运行时缓存（payload 只活在各窗口本地，镜像自行按引用水合）。 */
export function stripMessageForWire(m: EditorChatMessage): EditorChatMessage {
  if (!m.attachments?.length) return m;
  return { ...m, attachments: m.attachments.map(({ payload: _payload, ...rest }) => rest) };
}

/**
 * 剥离托盘附件的运行时字节与预览缓存（镜像 send op 专用）：字节已落仓库临时区（file 就位，
 * blob 不可 JSON 序列化），payload 可达数 MB 且宿主按引用水合，均不过事件线。
 * import/append op 不做此剥离——契约附件的 payload 是无 file 引用时的内容载体，剥离即丢内容。
 */
export function stripPendingsForWire(list: PendingAttachment[]): PendingAttachment[] {
  return list.map(({ blob: _blob, payload: _payload, ...rest }) => rest);
}

function stripSessionForWire(s: EditorChatSession): EditorChatSession {
  return { ...s, messages: s.messages.map(stripMessageForWire) };
}

/** 内容级相同判定（引用不同但剥缓存后一致 = 运行时缓存回填噪声，不进增量）。 */
function sameMessageContent(a: EditorChatMessage, b: EditorChatMessage): boolean {
  return JSON.stringify(stripMessageForWire(a)) === JSON.stringify(stripMessageForWire(b));
}

function metaFragmentOf(s: EditorChatSession): ChatSessionMetaFragment {
  return {
    id: s.id,
    file: s.file,
    title: s.title ?? null,
    agentId: s.agentId ?? null,
    compaction: s.compaction ?? null,
    createdAt: s.createdAt,
    updatedAt: s.updatedAt,
  };
}

function diffMessages(
  sessionId: string,
  prevMessages: EditorChatMessage[],
  nextMessages: EditorChatMessage[],
): ChatMessageDelta | null {
  const upserts: EditorChatMessage[] = [];
  let keepCount: number | undefined;
  const common = Math.min(prevMessages.length, nextMessages.length);
  for (let i = 0; i < common; i++) {
    if (prevMessages[i] !== nextMessages[i] && !sameMessageContent(prevMessages[i], nextMessages[i])) {
      upserts.push(stripMessageForWire(nextMessages[i]));
    }
  }
  if (nextMessages.length < prevMessages.length) {
    keepCount = nextMessages.length;
  } else {
    for (let i = common; i < nextMessages.length; i++) {
      upserts.push(stripMessageForWire(nextMessages[i]));
    }
  }
  if (upserts.length === 0 && keepCount === undefined) return null;
  return { sessionId, upserts, ...(keepCount !== undefined ? { keepCount } : {}) };
}

/** 相邻容器状态 → 增量片段（纯函数；updatedAt 单独变化不产出——每窗口「最近使用」置顶不进容器广播）。 */
export function computeChatDeltaFragments(
  prev: ChatContainerView,
  next: ChatContainerView,
): ChatDeltaFragments {
  const metas: ChatSessionMetaFragment[] = [];
  const messages: ChatMessageDelta[] = [];
  const messageSessionIds: string[] = [];
  const metaSessionIds: string[] = [];
  const deletedIds: string[] = [];
  const prevById = new Map(prev.sessions.map((s) => [s.id, s]));
  const nextIds = new Set(next.sessions.map((s) => s.id));
  for (const s of prev.sessions) {
    if (!nextIds.has(s.id)) {
      metas.push({ id: s.id, removed: true });
      deletedIds.push(s.id);
    }
  }
  for (const s of next.sessions) {
    const p = prevById.get(s.id);
    if (!p) {
      metas.push(metaFragmentOf(s));
      metaSessionIds.push(s.id);
      messages.push({ sessionId: s.id, upserts: s.messages.map(stripMessageForWire) });
      messageSessionIds.push(s.id);
      continue;
    }
    const metaChanged =
      p.title !== s.title ||
      p.agentId !== s.agentId ||
      p.compaction !== s.compaction ||
      p.createdAt !== s.createdAt;
    const msgDelta = diffMessages(s.id, p.messages, s.messages);
    if (metaChanged || msgDelta) {
      metas.push(metaFragmentOf(s));
      metaSessionIds.push(s.id);
    }
    if (msgDelta) {
      messages.push(msgDelta);
      messageSessionIds.push(s.id);
    }
  }
  const status = statusFragment(prev, next);
  return { metas, messages, status, messageSessionIds, metaSessionIds, deletedIds };
}

function statusFragment(prev: ChatContainerView, next: ChatContainerView): ChatContainerStatusFragment | null {
  const out: ChatContainerStatusFragment = {};
  let changed = false;
  if (prev.streaming !== next.streaming) {
    out.streaming = next.streaming;
    changed = true;
  }
  if (prev.compacting !== next.compacting) {
    out.compacting = next.compacting;
    changed = true;
  }
  if (prev.persistError !== next.persistError) {
    out.persistError = next.persistError;
    changed = true;
  }
  if (prev.modelOverride !== next.modelOverride) {
    out.modelOverride = next.modelOverride;
    changed = true;
  }
  if (prev.effortOverride !== next.effortOverride) {
    out.effortOverride = next.effortOverride;
    changed = true;
  }
  if (prev.sessionVaultKey !== next.sessionVaultKey) {
    out.sessionVaultKey = next.sessionVaultKey;
    changed = true;
  }
  return changed ? out : null;
}

/** 折叠时回收既有消息的附件运行时缓存：进线载荷已剥离 payload，同 file 引用的本地缓存不因折叠丢失。 */
function adoptRuntimePayloads(incoming: EditorChatMessage, existing?: EditorChatMessage): EditorChatMessage {
  if (!existing?.attachments?.length || !incoming.attachments?.length) return incoming;
  const payloadByFile = new Map<string, string>();
  for (const a of existing.attachments) {
    if (a.file && a.payload) payloadByFile.set(a.file, a.payload);
  }
  let adopted = false;
  const attachments = incoming.attachments.map((a) => {
    const payload = a.file ? payloadByFile.get(a.file) : undefined;
    if (!payload || a.payload) return a;
    adopted = true;
    return { ...a, payload };
  });
  return adopted ? { ...incoming, attachments } : incoming;
}

/**
 * 增量折叠进会话数组（纯函数，幂等——同一片段重复应用结果一致）：元数据按 id 合并（removed 移除、
 * 未知 id 落占位），消息按 id 原位替换或末尾追加后按 keepCount 截断。
 * 批次内移除主导：同一批片段对同一会话既有创建/更新又有移除（op 创建后会话被删、合并批次跨两次
 * 状态变化）时按移除收敛——移除后不再落该会话的元数据 upsert 与消息增量，防空壳占位复活。
 */
export function foldChatDelta(sessions: EditorChatSession[], f: ChatDeltaFragments): EditorChatSession[] {
  let next = sessions;
  const removedIds = new Set(f.metas.filter((m) => m.removed).map((m) => m.id));
  for (const meta of f.metas) {
    if (meta.removed || removedIds.has(meta.id)) {
      next = next.filter((s) => s.id !== meta.id);
      continue;
    }
    const existing = next.find((s) => s.id === meta.id);
    if (!existing) {
      next = [
        ...next,
        {
          id: meta.id,
          file: meta.file ?? `${CHAT_HISTORY_DIR}/${meta.id}${CHAT_MESSAGE_EXT}`,
          messages: [],
          createdAt: meta.createdAt ?? 0,
          updatedAt: meta.updatedAt ?? 0,
          ...(meta.title ? { title: meta.title } : {}),
          ...(meta.agentId ? { agentId: meta.agentId } : {}),
          ...(meta.compaction ? { compaction: meta.compaction } : {}),
        },
      ];
      continue;
    }
    next = next.map((s) =>
      s.id !== meta.id
        ? s
        : {
            ...s,
            ...(meta.title !== undefined ? { title: meta.title ?? undefined } : {}),
            ...(meta.agentId !== undefined ? { agentId: meta.agentId ?? undefined } : {}),
            ...(meta.compaction !== undefined ? { compaction: meta.compaction ?? undefined } : {}),
            ...(meta.file !== undefined ? { file: meta.file } : {}),
            createdAt: meta.createdAt ?? s.createdAt,
            updatedAt: meta.updatedAt ?? s.updatedAt,
          },
    );
  }
  for (const md of f.messages) {
    if (removedIds.has(md.sessionId)) continue;
    if (!next.some((s) => s.id === md.sessionId)) {
      next = [
        ...next,
        {
          id: md.sessionId,
          file: `${CHAT_HISTORY_DIR}/${md.sessionId}${CHAT_MESSAGE_EXT}`,
          messages: [],
          createdAt: 0,
          updatedAt: 0,
        },
      ];
    }
    next = next.map((s) => {
      if (s.id !== md.sessionId) return s;
      let messages = s.messages;
      for (const up of md.upserts) {
        const idx = messages.findIndex((m) => m.id === up.id);
        const merged = adoptRuntimePayloads(up, idx >= 0 ? messages[idx] : undefined);
        messages = idx >= 0 ? messages.map((m, i) => (i === idx ? merged : m)) : [...messages, merged];
      }
      if (md.keepCount !== undefined) messages = messages.slice(0, md.keepCount);
      return { ...s, messages };
    });
  }
  return next;
}

// ---------- 宿主侧 ----------

export interface ChatContainerHostHandlers {
  /** 当前容器视图（快照响应与差分基准）。 */
  getView(): ChatContainerView;
  /** 订阅容器状态变化（每次 setState 同步回调；差分基准由本模块自持）。 */
  subscribe(listener: () => void): () => void;
  /** 应用镜像 op（复用宿主容器动作；抛错 = op 失败，错误信息随响应回传镜像）。 */
  applyOp(op: ChatContainerOp): Promise<ChatOpOutcome>;
}

interface PendingEntry {
  fragments: ChatDeltaFragments;
  owner: string | null;
}

let hostHandlers: ChatContainerHostHandlers | null = null;
let hostLastView: ChatContainerView | null = null;
let hostSeq = 0;
let hostQueue: PendingEntry[] = [];
let hostFlushTimer: ReturnType<typeof setTimeout> | null = null;
/** 镜像 op 应用期间置位：期间的全部状态变化归属该 op（响应片段与广播回声抑制据此判定）。 */
let hostOpOwner: string | null = null;

/** 安装宿主（每窗口一次；请求监听 + 状态订阅差分）。 */
export function installChatContainerHost(handlers: ChatContainerHostHandlers): void {
  if (hostHandlers) return;
  hostHandlers = handlers;
  hostLastView = handlers.getView();
  void listen<ChatContainerRequest>(REQUEST_EVENT, (e) => {
    void handleContainerRequest(e.payload);
  });
  handlers.subscribe(() => {
    const next = handlers.getView();
    if (!hostLastView) {
      hostLastView = next;
      return;
    }
    const fragments = computeChatDeltaFragments(hostLastView, next);
    hostLastView = next;
    queueHostDelta(fragments, hostOpOwner);
  });
}

function isFragmentsEmpty(f: ChatDeltaFragments): boolean {
  return f.metas.length === 0 && f.messages.length === 0 && f.status === null;
}

function queueHostDelta(fragments: ChatDeltaFragments, owner: string | null): void {
  if (isFragmentsEmpty(fragments)) return;
  hostQueue.push({ fragments, owner });
  if (!hostFlushTimer) {
    hostFlushTimer = setTimeout(flushHostDelta, DELTA_FLUSH_MS);
  }
}

function mergeFragments(list: ChatDeltaFragments[]): ChatDeltaFragments {
  const metas: ChatSessionMetaFragment[] = [];
  const messages: ChatMessageDelta[] = [];
  const messageSessionIds = new Set<string>();
  const metaSessionIds = new Set<string>();
  const deletedIds = new Set<string>();
  let status: ChatContainerStatusFragment | null = null;
  for (const f of list) {
    metas.push(...f.metas);
    messages.push(...f.messages);
    f.messageSessionIds.forEach((id) => messageSessionIds.add(id));
    f.metaSessionIds.forEach((id) => metaSessionIds.add(id));
    f.deletedIds.forEach((id) => deletedIds.add(id));
    if (f.status) status = { ...(status ?? {}), ...f.status };
  }
  return {
    metas,
    messages,
    status,
    messageSessionIds: [...messageSessionIds],
    metaSessionIds: [...metaSessionIds],
    deletedIds: [...deletedIds],
  };
}

function flushHostDelta(): void {
  hostFlushTimer = null;
  if (hostQueue.length === 0) return;
  const entries = hostQueue;
  hostQueue = [];
  const merged = mergeFragments(entries.map((e) => e.fragments));
  const opOwners = [...new Set(entries.map((e) => e.owner).filter((o): o is string => o !== null))];
  hostSeq += 1;
  const delta: ChatContainerDelta = { seq: hostSeq, opOwners, ...merged };
  void emit(DELTA_EVENT, delta);
  // 插件事件只对镜像引发的变更发（宿主自身变更 = 本窗口写入方自知，与既有 origin 语义一致）
  if (
    opOwners.length > 0 &&
    (merged.messageSessionIds.length > 0 || merged.metaSessionIds.length > 0 || merged.deletedIds.length > 0)
  ) {
    emitPluginEvent("chat:sessions-changed", {
      messages: merged.messageSessionIds,
      metas: merged.metaSessionIds,
      deleted: merged.deletedIds,
    });
  }
}

async function handleContainerRequest(req: ChatContainerRequest): Promise<void> {
  if (!hostHandlers) return;
  if (req.kind === "snapshot") {
    const view = hostHandlers.getView();
    const response: ChatContainerResponse = {
      requestId: req.requestId,
      kind: "snapshot",
      snapshot: {
        seq: hostSeq,
        sessionVaultKey: view.sessionVaultKey,
        sessions: view.sessions.map(stripSessionForWire),
        streaming: view.streaming,
        compacting: view.compacting,
        persistError: view.persistError,
        modelOverride: view.modelOverride,
        effortOverride: view.effortOverride,
      },
    };
    await emitTo(req.from, RESPONSE_EVENT, response);
    return;
  }
  // op 应用前冲刷在途增量：镜像按宿主状态推进序折叠——pre-op 变更先以广播增量到达（事件按
  // 发射序投递），响应片段才恰为 op 应用窗口（mark 恒 0）。否则 op 窗口片段先于先前未冲刷的
  // 广播到达镜像，追加类增量按「末尾追加」折叠会错序（镜像内消息顺序 ≠ 宿主真源）。
  flushHostDelta();
  const mark = hostQueue.length;
  hostOpOwner = req.requestId;
  let response: ChatContainerResponse;
  try {
    const outcome = await hostHandlers.applyOp(req.op);
    response = {
      requestId: req.requestId,
      kind: "op",
      ok: true,
      value: outcome.value,
      ...(outcome.createdSessionId !== undefined ? { createdSessionId: outcome.createdSessionId } : {}),
      fragments: mergeFragments(hostQueue.slice(mark).map((e) => e.fragments)),
    };
  } catch (e) {
    response = { requestId: req.requestId, kind: "op", ok: false, error: e instanceof Error ? e.message : String(e) };
  } finally {
    hostOpOwner = null;
  }
  await emitTo(req.from, RESPONSE_EVENT, response);
}

// ---------- 镜像侧 ----------

export interface ChatContainerMirrorHandlers {
  /** 基线快照整体替换（loaded 随之置位）。 */
  applySnapshot(snapshot: ChatContainerSnapshot): void;
  /** 增量折叠（notifyPlugins = 非自身回声时向本窗口插件转发 chat:sessions-changed）。 */
  applyFragments(fragments: ChatDeltaFragments, opts: { notifyPlugins: boolean }): void;
}

let mirrorHandlers: ChatContainerMirrorHandlers | null = null;
let mirrorSeq = 0;
const mirrorPending = new Map<
  string,
  { resolve: (r: ChatContainerResponse) => void; reject: (e: Error) => void }
>();
const mirrorOpIds = new Map<string, number>();

/** 安装镜像（每窗口一次；响应与增量监听）。 */
export function installChatContainerMirror(handlers: ChatContainerMirrorHandlers): void {
  if (mirrorHandlers) return;
  mirrorHandlers = handlers;
  void listen<ChatContainerResponse>(RESPONSE_EVENT, (e) => {
    handleMirrorResponse(e.payload);
  });
  void listen<ChatContainerDelta>(DELTA_EVENT, (e) => {
    handleMirrorDelta(e.payload);
  });
}

function handleMirrorResponse(r: ChatContainerResponse): void {
  const pending = mirrorPending.get(r.requestId);
  if (r.kind === "op" && r.ok && r.fragments) {
    // 自身 op 的响应片段先折叠再决绝：await 返回时读己之写已可见；不转发插件事件（自身变更不自通知）
    foldExternal(() => mirrorHandlers?.applyFragments(r.fragments, { notifyPlugins: false }));
  }
  if (!pending) return;
  mirrorPending.delete(r.requestId);
  if (r.kind === "op" && !r.ok) {
    pending.reject(new Error(r.error));
  } else {
    pending.resolve(r);
  }
}

function handleMirrorDelta(d: ChatContainerDelta): void {
  // 事件线宿主自回声：宿主是 delta 唯一产出方（emit 全窗广播含自身），状态已是权威；
  // 宿主自身变更 opOwners 为空判不出回声，折叠会以通知面重复触发插件事件——整体跳过
  if (!rustTruthEnabled && isChatContainerHost()) return;
  if (d.seq <= mirrorSeq) return;
  mirrorSeq = d.seq;
  if (d.status?.sessionVaultKey !== undefined) rustCurrentRoot = d.status.sessionVaultKey;
  // 执行体自身提交的回声：执行期变更已乐观应用进本地（本地内容 ≥ 广播内容），折叠会回退
  // 正在流式的内容——整体跳过（Rust 真源侧一致性由提交时的差分保证，无需本地对账）
  if (rustTruthEnabled && executorHandlers !== null && d.opOwners.some((id) => executorCommitIds.has(id))) {
    return;
  }
  const ownEcho = d.opOwners.some((id) => mirrorOpIds.has(id));
  foldExternal(() => mirrorHandlers?.applyFragments(d, { notifyPlugins: !ownEcho }));
}

/** 外部来源的容器变更折叠（Rust 广播/快照/事件线响应）：期间执行体的差分提交基准直接推进，不回提交。 */
function foldExternal(fold: () => void): void {
  externalFolding = true;
  try {
    fold();
  } finally {
    externalFolding = false;
  }
}

/** wire 之外的容器变更入点（快照整体替换等）复用同一折叠语义：基准推进不回提交。 */
export function foldChatContainerExternal(fold: () => void): void {
  foldExternal(fold);
}

function rememberMirrorOpId(opId: string): void {
  const now = Date.now();
  for (const [id, at] of mirrorOpIds) {
    if (now - at > OPID_RETENTION_MS) mirrorOpIds.delete(id);
  }
  mirrorOpIds.set(opId, now);
}

/** 拉取容器基线快照（Rust 真源 = invoke 装载命令；事件线 = 向宿主定向请求，超时拒绝）。 */
export function requestChatContainerSnapshot(): Promise<ChatContainerSnapshot> {
  if (rustTruthEnabled) {
    return invoke<ChatContainerSnapshot>("chat_container_load").then((snapshot) => {
      rustCurrentRoot = snapshot.sessionVaultKey;
      mirrorSeq = snapshot.seq;
      // 装载 = 真源按新仓库根重建：执行体在途队列/退避重试/补丁 rev 全部作废
      //（批次基于旧根基线，提交会污染新根；Rust 侧 rev 计数随重建归零）
      executorQueue = [];
      executorCommitRetries = 0;
      if (executorRetryTimer) {
        clearTimeout(executorRetryTimer);
        executorRetryTimer = null;
      }
      executorPatchRevs.clear();
      if (executorFlushTimer) {
        clearTimeout(executorFlushTimer);
        executorFlushTimer = null;
      }
      executorLastView = executorHandlers?.getView() ?? null;
      return snapshot;
    });
  }
  const requestId = crypto.randomUUID();
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      mirrorPending.delete(requestId);
      reject(new Error("会话容器基线拉取超时"));
    }, SNAPSHOT_TIMEOUT_MS);
    mirrorPending.set(requestId, {
      resolve: (r) => {
        clearTimeout(timer);
        if (r.kind === "snapshot") {
          mirrorSeq = r.snapshot.seq;
          resolve(r.snapshot);
        } else {
          reject(new Error("会话容器基线响应类型不符"));
        }
      },
      reject: (e) => {
        clearTimeout(timer);
        reject(e);
      },
    });
    void emitTo(HOST_WINDOW_LABEL, REQUEST_EVENT, {
      kind: "snapshot",
      requestId,
      from: currentWindowLabel(),
    } satisfies ChatContainerRequest);
  });
}

/** 转发写意图（Rust 真源 = invoke 真源命令，纯持久 op 直应用、执行 op 经意图转发执行体；
 *  事件线 = 定向发宿主）。响应片段由镜像层先行折叠；不设超时——压缩/命名等模型请求耗时不可上限。 */
export function sendChatContainerOp(op: ChatContainerOp): Promise<ChatContainerOpResponse> {
  if (rustTruthEnabled) return sendChatContainerOpRust(op);
  const requestId = crypto.randomUUID();
  rememberMirrorOpId(requestId);
  return new Promise((resolve, reject) => {
    mirrorPending.set(requestId, {
      resolve: (r) => {
        if (r.kind === "op") {
          resolve(r);
        } else {
          reject(new Error("会话容器 op 响应类型不符"));
        }
      },
      reject,
    });
    void emitTo(HOST_WINDOW_LABEL, REQUEST_EVENT, {
      kind: "op",
      requestId,
      from: currentWindowLabel(),
      op,
    } satisfies ChatContainerRequest);
  });
}

async function sendChatContainerOpRust(op: ChatContainerOp): Promise<ChatContainerOpResponse> {
  const requestId = crypto.randomUUID();
  rememberMirrorOpId(requestId);
  const result = await invoke<{
    value?: unknown;
    createdSessionId?: string;
    fragments: ChatDeltaFragments;
  }>("chat_container_apply", { requestId, expectedRoot: rustCurrentRoot, op });
  const response: ChatContainerOpResponse = {
    requestId,
    kind: "op",
    ok: true,
    ...(result.value !== undefined ? { value: result.value } : {}),
    ...(result.createdSessionId !== undefined ? { createdSessionId: result.createdSessionId } : {}),
    fragments: result.fragments,
  };
  // 读己之写：响应片段先折叠再返回（同事件线镜像语义；执行体意图窗口片段同路径收敛）
  foldExternal(() => mirrorHandlers?.applyFragments(result.fragments, { notifyPlugins: false }));
  return response;
}

// ---------- Rust 真源传输（Rust 持有持久态与写盘链，全窗口薄客户端，主窗口兼执行体）----------

/** Rust 容器真源是否已启用（initChatContainerTransport 按资格与探测评定；未评定 = 事件线模式）。 */
let rustTruthEnabled = false;
/** 已知真源仓库身份键（快照/增量 status 携带更新；op 与变更提交的 expectedRoot）。 */
let rustCurrentRoot = "";

/** Rust 容器真源模式判定（动作分派按此分流：true = 全窗口走 invoke，false = 宿主-镜像事件线）。 */
export function isRustTruthMode(): boolean {
  return rustTruthEnabled;
}

/** Tauri 运行时判定（vitest/jsdom 无 invoke 通道，恒事件线降级）。 */
function hasTauriRuntime(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

/** 请求 Rust 真源立即写盘并等待本轮完成（resolve 不代表全部写成功；失败可见 + 退避重试）。 */
export function flushChatContainerTruth(): Promise<void> {
  // flush 前同步冲刷执行体在途队列：100ms 节流窗口内的变更先提交进真源再等写盘轮，
  // 防退出时最后一批变更留在前端内存（invoke 按序处理，commit 先于 flush 到达 Rust）
  if (rustTruthEnabled && executorHandlers) flushExecutorCommits();
  return invoke("chat_container_flush");
}

export interface ChatContainerExecutorHandlers {
  /** 当前容器视图（变更提交的差分基准）。 */
  getView(): ChatContainerView;
  /** 订阅容器状态变化（同步回调；编排侧乐观应用经此捕获并提交真源）。 */
  subscribe(listener: () => void): () => void;
  /** 执行意图 op（Rust 转发的执行 op；产出经变更提交回真源，随 intent_result 回填调用方）。 */
  executeOp(op: ChatContainerOp): Promise<ChatOpOutcome>;
}

interface IntentPayload {
  intentId: string;
  requestId: string;
  op: ChatContainerOp;
}

/**
 * 传输初始化（面板 load 时调用）：按资格与探测结果评定传输模式并安装对应角色，本地/空间
 * 仓库切换时模式随装载重评估。Rust 真源模式 = 资格成立（当前激活身份支持容器持久态上收）
 * 且探测可达（旧后端无容器命令则回落）——全窗口薄客户端（快照/op 走 invoke、增量折叠 Rust
 * 广播），主窗口额外安装执行体（意图执行 + 编排变更提交）；否则宿主-镜像事件线（主窗口 =
 * 宿主单写者）。角色监听幂等安装、双模式并存：模式翻转只切换分派标志，不重挂监听。
 */
export async function initChatContainerTransport(handlers: {
  mirror: ChatContainerMirrorHandlers;
  /** 主窗口传入（执行体职责仅主窗口承担）；事件线模式的宿主 handlers。 */
  executor?: ChatContainerExecutorHandlers;
  host?: ChatContainerHostHandlers;
  /** Rust 真源模式资格：容器持久态收口 Rust 仅覆盖本地仓库——空间仓库会话存储在服务端
   * （Rust 无鉴权通道），且 Rust 侧仓库根只跟踪本地激活，空间身份下取根会错指向旧本地仓库。 */
  rustTruthEligible?: boolean;
}): Promise<void> {
  if (!hasTauriRuntime()) return;
  let rustOk = false;
  if (handlers.rustTruthEligible) {
    try {
      await invoke("chat_container_snapshot");
      rustOk = true;
    } catch (e) {
      // 探测失败回落事件线（旧后端无容器命令属预期）；记日志便于区分「后端过旧」与「真源异常」
      console.error("Rust 会话容器真源探测失败，回落宿主-镜像事件线", e);
      rustOk = false;
    }
  }
  rustTruthEnabled = rustOk;
  installChatContainerMirror(handlers.mirror);
  if (isChatContainerHost()) {
    if (rustTruthEnabled && handlers.executor) {
      installChatContainerExecutor(handlers.executor);
    }
    if (!rustTruthEnabled && handlers.host) {
      installChatContainerHost(handlers.host);
    }
  }
}

// ===== 执行体（主窗口）：意图执行 + 编排变更提交 =====

let executorHandlers: ChatContainerExecutorHandlers | null = null;
let executorLastView: ChatContainerView | null = null;
let executorCommitSeq = 0;
let executorFlushTimer: ReturnType<typeof setTimeout> | null = null;
let executorQueue: CommitBatch[] = [];
/** 自身提交的 requestId（回声跳过保留窗，覆盖广播回声到达）。 */
const executorCommitIds = new Map<string, number>();
/** 消息补丁 rev（Rust 门控单调：迟到的旧补丁不得覆盖新内容）。 */
const executorPatchRevs = new Map<string, number>();
/** 外部来源变更折叠进行中标志（foldExternal 维护）：执行体差分提交据此推进基准而不回提交。 */
let externalFolding = false;
/** 自身提交回声的保留窗（同事件线 opId 语义）。 */
const COMMIT_ID_RETENTION_MS = OPID_RETENTION_MS;
/** 提交通道失败的退避序列（有界重试，达上限放弃并报错）。 */
const EXECUTOR_COMMIT_RETRY_DELAYS = [500, 1000, 2000, 4000, 8000];
let executorCommitRetries = 0;
let executorRetryTimer: ReturnType<typeof setTimeout> | null = null;

function installChatContainerExecutor(handlers: ChatContainerExecutorHandlers): void {
  if (executorHandlers) return;
  executorHandlers = handlers;
  executorLastView = handlers.getView();
  void invoke("chat_container_executor_boot").catch((e) => console.error("会话执行体登记失败", e));
  void listen<IntentPayload>(INTENT_EVENT, (e) => {
    void handleExecutorIntent(e.payload);
  });
  handlers.subscribe(queueExecutorReport);
}

async function handleExecutorIntent(intent: IntentPayload): Promise<void> {
  const handlers = executorHandlers;
  if (!handlers) return;
  let result:
    | { status: "ok"; value?: unknown; createdSessionId?: string }
    | { status: "error"; error: string };
  try {
    const outcome = await handlers.executeOp(intent.op);
    result = {
      status: "ok",
      ...(outcome.value !== undefined ? { value: outcome.value } : {}),
      ...(outcome.createdSessionId !== undefined ? { createdSessionId: outcome.createdSessionId } : {}),
    };
  } catch (e) {
    result = { status: "error", error: e instanceof Error ? e.message : String(e) };
  }
  await invoke("chat_container_intent_result", { intentId: intent.intentId, result }).catch((e) =>
    console.error("意图结果回填失败", intent.intentId, e),
  );
}

/** 执行体状态变化 → 差分 → 提交批次（100ms 节流；外部折叠或模式退离期间仅推进基准）。 */
function queueExecutorReport(): void {
  const handlers = executorHandlers;
  if (!handlers) return;
  const next = handlers.getView();
  const prev = executorLastView;
  executorLastView = next;
  // 模式退离（切到空间仓库走事件线）：本地视图属另一真源，基准推进防陈旧差分在模式回归时回放
  if (!prev || externalFolding || !rustTruthEnabled) return;
  const batch = containerDeltaToBatch(prev, next);
  if (isBatchEmpty(batch)) return;
  executorQueue.push(batch);
  if (!executorFlushTimer) {
    executorFlushTimer = setTimeout(flushExecutorCommits, DELTA_FLUSH_MS);
  }
}

function flushExecutorCommits(): void {
  executorFlushTimer = null;
  // 模式退离后在途队列整体丢弃：批次基于旧模式基线，提交会污染退离前的真源仓库
  if (!rustTruthEnabled) {
    executorQueue = [];
    executorCommitRetries = 0;
    return;
  }
  if (executorQueue.length === 0 || !executorHandlers) return;
  const batch = mergeCommitBatches(executorQueue);
  executorQueue = [];
  const requestId = `exec-${++executorCommitSeq}`;
  const now = Date.now();
  for (const [id, at] of executorCommitIds) {
    if (now - at > COMMIT_ID_RETENTION_MS) executorCommitIds.delete(id);
  }
  executorCommitIds.set(requestId, now);
  // 落盘重试链在 Rust 真源侧（防抖 + 退避）；提交通道失败由前端按退避重试——「仓库已切换」
  // 除外（切仓进行中，旧变更随旧仓丢弃，与事件线归属校验同语义）。计数只在链条真正结束时归零：
  // 若在发出前无条件归零，重试自己的 flush 又会清零，退避序列永远停在第 0 档（等同固定间隔无限重试）。
  void invoke("chat_container_commit", { requestId, expectedRoot: rustCurrentRoot, batch })
    .then(() => {
      executorCommitRetries = 0;
    })
    .catch((e) => {
      const message = e instanceof Error ? e.message : String(e);
      if (message.includes("仓库已切换")) {
        executorCommitRetries = 0;
        return;
      }
      console.error("会话容器变更提交失败，将重试", e);
      scheduleExecutorCommitRetry(batch);
    });
}

/** 提交通道失败的退避重试（有界）：批次重新入队错峰再交；连续失败达上限放弃（真源侧落盘重试链不受影响）。 */
function scheduleExecutorCommitRetry(batch: CommitBatch): void {
  if (executorCommitRetries >= EXECUTOR_COMMIT_RETRY_DELAYS.length) {
    console.error("会话容器变更提交重试达上限，本批变更已丢弃", batch);
    return;
  }
  const delay = EXECUTOR_COMMIT_RETRY_DELAYS[executorCommitRetries];
  executorCommitRetries += 1;
  if (executorRetryTimer) clearTimeout(executorRetryTimer);
  executorRetryTimer = setTimeout(() => {
    executorRetryTimer = null;
    if (!rustTruthEnabled) return;
    executorQueue.push(batch);
    flushExecutorCommits();
  }, delay);
}

/** 执行体提交批次（chat_container_commit 的 CommitBatch；字段与 Rust 契约逐一对齐）。 */
interface CommitBatch {
  created: Array<{
    id: string;
    file: string;
    title?: string;
    agentId?: string;
    compaction?: unknown;
    createdAt: number;
    updatedAt: number;
    messages: EditorChatMessage[];
  }>;
  metas: Array<{
    id: string;
    title?: string;
    agentId?: string;
    compaction?: unknown;
    updatedAt?: number;
  }>;
  appends: Array<{ sessionId: string; messages: EditorChatMessage[] }>;
  patches: Array<{
    sessionId: string;
    messageId: string;
    rev: number;
    content?: string;
    steps?: unknown;
  }>;
  truncations: Array<{ sessionId: string; keepCount: number }>;
  status: { streaming?: boolean; compacting?: string | null };
}

function isBatchEmpty(batch: CommitBatch): boolean {
  return (
    batch.created.length === 0 &&
    batch.metas.length === 0 &&
    batch.appends.length === 0 &&
    batch.patches.length === 0 &&
    batch.truncations.length === 0 &&
    batch.status.streaming === undefined &&
    batch.status.compacting === undefined
  );
}

function mergeCommitBatches(list: CommitBatch[]): CommitBatch {
  const merged: CommitBatch = { created: [], metas: [], appends: [], patches: [], truncations: [], status: {} };
  for (const b of list) {
    merged.created.push(...b.created);
    merged.metas.push(...b.metas);
    merged.appends.push(...b.appends);
    merged.patches.push(...b.patches);
    merged.truncations.push(...b.truncations);
    if (b.status.streaming !== undefined) merged.status.streaming = b.status.streaming;
    if (b.status.compacting !== undefined) merged.status.compacting = b.status.compacting;
  }
  return merged;
}

/**
 * 相邻容器视图 → 真源提交批次：差分（updatedAt 单独变化不产出）后按「prev 有无」分派——
 * 消息 prev 已有 = 内容补丁（rev 单调），prev 无 = 末尾追加；会话 prev 无 = 新建登记，
 * prev 有 = 元数据补丁；会话移除不产批次（删除走纯持久 op 直应用）。执行期消息契约 =
 * 仅 content/steps 变化（其他字段只随新建进入真源），补丁按此只携带两字段。
 * 与 Rust commit 的应用顺序（created→metas→appends→patches→truncations→status）和折叠
 * 语义（upserts 先于 keepCount 截断）对齐。
 */
function containerDeltaToBatch(prev: ChatContainerView, next: ChatContainerView): CommitBatch {
  const batch: CommitBatch = { created: [], metas: [], appends: [], patches: [], truncations: [], status: {} };
  const fragments = computeChatDeltaFragments(prev, next);
  const prevById = new Map(prev.sessions.map((s) => [s.id, s]));
  const createdIds = new Set<string>();
  for (const meta of fragments.metas) {
    const p = prevById.get(meta.id);
    const n = next.sessions.find((s) => s.id === meta.id);
    if (meta.removed || !n) continue;
    if (!p) {
      createdIds.add(n.id);
      batch.created.push({
        id: n.id,
        file: n.file,
        ...(n.title !== undefined ? { title: n.title } : {}),
        ...(n.agentId !== undefined ? { agentId: n.agentId } : {}),
        ...(n.compaction !== undefined ? { compaction: n.compaction } : {}),
        createdAt: n.createdAt,
        updatedAt: n.updatedAt,
        messages: n.messages.map(stripMessageForWire),
      });
      continue;
    }
    const patch: CommitBatch["metas"][number] = { id: meta.id };
    if (p.title !== n.title) patch.title = n.title;
    if (p.agentId !== n.agentId) patch.agentId = n.agentId;
    if (p.compaction !== n.compaction) patch.compaction = n.compaction;
    if (p.updatedAt !== n.updatedAt) patch.updatedAt = n.updatedAt;
    if (patch.title !== undefined || patch.agentId !== undefined || patch.compaction !== undefined || patch.updatedAt !== undefined) {
      batch.metas.push(patch);
    }
  }
  for (const md of fragments.messages) {
    // 新建会话的全量消息已随 created 登记，再产 append 会在真源翻倍（import 单步全量
    // setState 即此形状），直接跳过
    if (createdIds.has(md.sessionId)) continue;
    const p = prevById.get(md.sessionId);
    for (const up of md.upserts) {
      const known = p?.messages.some((m) => m.id === up.id) ?? false;
      if (known) {
        const rev = (executorPatchRevs.get(up.id) ?? 0) + 1;
        executorPatchRevs.set(up.id, rev);
        batch.patches.push({
          sessionId: md.sessionId,
          messageId: up.id,
          rev,
          ...(up.content !== undefined ? { content: up.content } : {}),
          ...(up.steps !== undefined ? { steps: up.steps } : {}),
        });
      } else {
        batch.appends.push({ sessionId: md.sessionId, messages: [stripMessageForWire(up)] });
      }
    }
    if (md.keepCount !== undefined) {
      batch.truncations.push({ sessionId: md.sessionId, keepCount: md.keepCount });
    }
  }
  if (fragments.status) {
    if (fragments.status.streaming !== undefined) batch.status.streaming = fragments.status.streaming;
    if (fragments.status.compacting !== undefined) batch.status.compacting = fragments.status.compacting;
  }
  return batch;
}
