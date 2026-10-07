/**
 * 会话容器跨窗口线（宿主-镜像模型）：宿主 = 主窗口单写者（独占写盘链），镜像窗口经
 * seq 戳快照拉基线、增量折叠跟随、op 转发写意图。差分与折叠为纯函数，传输为 Tauri event。
 */
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
    mirrorHandlers?.applyFragments(r.fragments, { notifyPlugins: false });
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
  if (d.seq <= mirrorSeq) return;
  mirrorSeq = d.seq;
  const ownEcho = d.opOwners.some((id) => mirrorOpIds.has(id));
  mirrorHandlers?.applyFragments(d, { notifyPlugins: !ownEcho });
}

function rememberMirrorOpId(opId: string): void {
  const now = Date.now();
  for (const [id, at] of mirrorOpIds) {
    if (now - at > OPID_RETENTION_MS) mirrorOpIds.delete(id);
  }
  mirrorOpIds.set(opId, now);
}

/** 拉取容器基线快照（超时拒绝；seq 基线在此登记）。 */
export function requestChatContainerSnapshot(): Promise<ChatContainerSnapshot> {
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

/** 转发写意图到宿主（响应片段由镜像层先行折叠；不设超时——压缩/命名等模型请求耗时不可上限）。 */
export function sendChatContainerOp(op: ChatContainerOp): Promise<ChatContainerOpResponse> {
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
