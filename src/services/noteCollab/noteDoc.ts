/**
 * 笔记协作文档（Yjs）生命周期与每文件单例注册表。
 *
 * 每篇打开/编辑的笔记持一个独立 `Y.Doc`（根键 "text" = `Y.Text` 全文 Markdown）。
 * - 打开/进入编辑：以磁盘正文初始化 `Y.Doc`（快照基线，仅当无激活编辑器时重置）。
 * - 编辑：`y-codemirror.next` 绑定 `Y.Text`；远端 update 经 `receiveSyncMessage` 合入。
 * - 落盘：收敛后全文（`flushNoteDoc`）写回 `.md` 真源（调用方负责）。
 * - awareness：本地光标/选中/身份经 `y-codemirror.next` 更新；远端状态用于渲染用户色光标/选中。
 *
 * 多面板打开同一笔记共享同一 `Y.Doc` 实例（per-file 单例引用计数），避免多 doc 分叉。
 * 本模块为纯数据/同步层：网络收发经 collabStore 注入的广播钩子完成，不直连 relay。
 *
 * # 磁盘基线收敛（防重开翻倍）
 *
 * 幂等仅在所有对端把全文放进同一个 seed 基线（固定 `clientID=1`、确定性字节）时成立。
 * 重开对端把「已合并磁盘全文」塞进新的 seed 基线，与「在线端旧基线 + 真实 cid 编辑」的
 * 拓扑相加即翻倍。机制：
 * - 每 entry 记录权威 `diskBaseline` 与最近落盘 `lastFlushed`（clean = doc 文本 == lastFlushed）。
 * - 重开（bindNoteDoc viewCount 0）：以磁盘重建 doc（client1=disk），先广播 `BASELINE_RESET(disk)`
 *   再发 syncStep1，让房间先行收敛到磁盘权威基线后（有序广播）再握手换状态，避免翻倍。
 * - 收到 `BASELINE_RESET(diskR)`：与本地 diskBaseline 相同 → no-op（防环）；不同 → 收敛：
 *   空基线（diskR=""）且本地有内容 → 挂起保留本地（真实清空走增量，防空缓存/加载窗口空态误广播反复清空在线端）；
 *   clean（无未落盘编辑）→ 整体重建 doc 为 client1=diskR（destroy + 新实例，编辑器随 binding
 *   引用变化自动重绑）；dirty（正在输入）→ 挂起 pendingBaseline，实时编辑照常，待落盘
 *   （markNoteDiskWrite）后以「较新的磁盘文本」收敛，不丢最后协作者版本。
 * - 收敛/重建后重发 RESET + syncStep1 与房间重新收敛；同基线 exchange 幂等（确定性 seed）。
 *
 * 残余说明：本机制依赖「RESET 在握手之前先到」的有序广播（relay 按发送顺序转发）。极端场景
 * 「A 正在输入的同一瞬间 B 重开」，A 挂起期间仍以旧拓扑广播增量，B（新基线）合入可能位置失真；
 * 该场景稀薄，且待 A 落盘后经 markNoteDiskWrite 以最新磁盘文本收敛自愈，不静默覆盖磁盘。
 */
import * as Y from "yjs";
import { Awareness } from "y-protocols/awareness";
import {
  applyAwarenessUpdate,
  encodeAwarenessUpdate,
} from "y-protocols/awareness";
import {
  messageYjsSyncStep1,
  readSyncMessage,
  writeSyncStep1,
} from "y-protocols/sync";
import * as encoding from "lib0/encoding";
import * as decoding from "lib0/decoding";

/** 远端合入 origin 标记：本端 applyUpdate 用它，doc 'update' 事件据此对应用跳过回发。 */
const REMOTE_ORIGIN = "note-collab-remote";

/** 本端 awareness（光标/选中）广播节流：高频合并，防止每次选区变化刷屏 relay。 */
const AWARE_THROTTLE_MS = 100;

/**
 * 确定性磁盘基线 seed 客户端 id：所有对端以同一 clientID 重建磁盘基线 → 合并幂等不重复。
 * 若各对端各自用随机 clientID 插入同一正文，合并时两条 struct 并存 → 内容翻倍、
 * 相对位置错乱（光标互见失效）、本端落盘与对端相悖（误报外部覆盖）。
 */
const BASELINE_SEED_CLIENT_ID = 1;

/** 消息 opcode：磁盘基线重置通告（文本为权威基线全文，UTF-8 varString）。 */
const MESSAGE_BASELINE_RESET = 0x42;

/**
 * 以固定 seed 客户端把磁盘正文编码成一个确定的 Yjs update：
 * 各对端对同一正文生成字节相同的基线 struct，`applyUpdate` 合并幂等，无重复。
 * 导出仅供测试锁定幂等契约（同文本不翻倍；异文本不产生重复）。
 */
export function baselineSeedUpdate(text: string): Uint8Array {
  const seed = new Y.Doc();
  seed.clientID = BASELINE_SEED_CLIENT_ID;
  seed.getText("text").insert(0, text);
  const update = Y.encodeStateAsUpdate(seed);
  seed.destroy();
  return update;
}

export interface NoteDoc {
  /** 仓库相对路径。 */
  file: string;
  ydoc: Y.Doc;
  ytext: Y.Text;
  awareness: Awareness;
}

/** 网络广播钩子（由 collabStore 注入；未启用协作时为 null）。 */
export interface NoteCollabBroadcast {
  /** 广播 Yjs 同步消息（syncStep1/2 或增量 update，已按 y-protocols 协议编码）。 */
  sendSyncMessage: (file: string, payload: Uint8Array) => void;
  /** 广播 awareness 更新（y-protocols 编码）。 */
  sendAwareness: (file: string, payload: Uint8Array) => void;
}

let broadcast: NoteCollabBroadcast | null = null;

/** doc 实例被整体重建后通知 store 刷新 binding（service 不 import store，靠回调反哺）。 */
let onBindingRefresh: ((file: string, doc: NoteDoc) => void) | null = null;

/** collabStore 注入/解除网络广播钩子（协作开关切换时调用）。 */
export function setNoteCollabBroadcast(b: NoteCollabBroadcast | null): void {
  broadcast = b;
}

/** 注册 doc 重建后的 binding 刷新回调（noteCollabStore 注入，用于更新 bindings[file] 触发编辑器重绑）。 */
export function setNoteCollabBindingRefresh(
  fn: ((file: string, doc: NoteDoc) => void) | null,
): void {
  onBindingRefresh = fn;
}

/** 每文件单例注册表（Y.Doc 生命周期）。 */
interface Entry {
  doc: NoteDoc;
  viewCount: number;
  /** 权威磁盘基线文本（本端当前持有的 seed 基线）。 */
  diskBaseline: string;
  /** 最近一次落盘/seed 的文本（clean = doc 文本 == lastFlushed）。 */
  lastFlushed: string;
  /** 收到更新的权威基线但因 dirty 挂起，待 clean 后收敛。 */
  pendingBaseline: string | null;
  /** 最近一次远端合入的作者（collabStore 从 peers 解析注入；历史按操作人署名用，无远端合入 = null）。 */
  lastRemoteAuthor: NoteRemoteAuthor | null;
  /** 销毁/重建 doc 前清理其挂起定时器与 awareness（防切仓库/重建后旧 timer 把残留 awareness 发进新房间）。 */
  cleanup: () => void;
}

/** 远端合入作者（历史按操作人署名用：协作对端经 relay 广播的内容变化，作者 = 发送端身份）。 */
export interface NoteRemoteAuthor {
  id: string;
  name: string;
  device: string;
}

/** 远端 Yjs update 应用深度（同步窗口内 >0）：y-codemirror 在 yjs 事务内同步回写 CM，
 *  onBodyChange→handleChange 与远端应用同栈执行，据此区分「远端合入」与「本地编辑」。 */
let remoteApplyDepth = 0;

/** 当前是否正在应用远端 Yjs update（协作回环/对端收敛时同步调用栈内为 true）。 */
export function isRemoteNoteApplyActive(): boolean {
  return remoteApplyDepth > 0;
}

/** 取某文件最近一次远端合入的作者（无 = null）。 */
export function getLastRemoteAuthor(file: string): NoteRemoteAuthor | null {
  return entries.get(file)?.lastRemoteAuthor ?? null;
}

const entries = new Map<string, Entry>();

/** 为本地文档绑定 awareness 身份（昵称/用户色；collabStore 打开协作笔记时调用，可幂等重设）。 */
export function setNoteCollabIdentity(file: string, user: { name: string; color: string }): void {
  const e = entries.get(file);
  if (!e) return;
  e.doc.awareness.setLocalStateField("user", {
    name: user.name,
    color: user.color,
    colorLight: `${user.color}33`,
  });
}

function createDoc(file: string, text: string): { doc: NoteDoc; cleanup: () => void } {
  const ydoc = new Y.Doc();
  const ytext = ydoc.getText("text");
  // 以确定性 seed 客户端重建磁盘基线（而非本端随机 clientID 直接 insert）——
  // 保证所有对端基线 struct 一致，合并不重复（见 baselineSeedUpdate 注释）。
  Y.applyUpdate(ydoc, baselineSeedUpdate(text));
  const awareness = new Awareness(ydoc);

  // 本端增量 update 广播：远端合入（origin=REMOTE_ORIGIN）不回发，防回环；本地编辑/初始状态均发
  ydoc.on("update", (update: Uint8Array, origin: unknown) => {
    if (origin === REMOTE_ORIGIN) return;
    if (broadcast) {
      broadcast.sendSyncMessage(file, writeUpdateHeader(update));
    }
  });

  // 本端 awareness（光标/选中/身份）变更 → 节流编码广播（光标/选中高频变化合并，防刷屏 relay）；
  // 远端应用（origin=remote）不再回发
  let awarePending: { payload: Uint8Array } | null = null;
  let awareTimer: ReturnType<typeof setTimeout> | null = null;
  awareness.on(
    "update",
    (
      { added, updated, removed }: { added: number[]; updated: number[]; removed: number[] },
      origin: unknown,
    ) => {
      if (origin === "remote") return;
      const changed = [...added, ...updated, ...removed];
      if (changed.length === 0 || !broadcast) return;
      awarePending = { payload: encodeAwarenessUpdate(awareness, changed) };
      if (awareTimer !== null) return;
      awareTimer = setTimeout(() => {
        awareTimer = null;
        if (awarePending && broadcast) {
          broadcast.sendAwareness(file, awarePending.payload);
        }
        awarePending = null;
      }, AWARE_THROTTLE_MS);
    },
  );

  const doc: NoteDoc = {
    file,
    ydoc,
    ytext,
    awareness,
  };
  // 销毁时清理：awareness 定时器/待发状态 + awareness 本身（其 'update' 监听随之释放）
  const cleanup = () => {
    if (awareTimer !== null) {
      clearTimeout(awareTimer);
      awareTimer = null;
    }
    awarePending = null;
    awareness.destroy();
  };
  return { doc, cleanup };
}

/** y-protocols sync 的 `update` 消息编码 = 消息类型头 + update bytes。 */
function writeUpdateHeader(update: Uint8Array): Uint8Array {
  const encoder = encoding.createEncoder();
  encoding.writeVarUint(encoder, 2); // messageYjsUpdate
  encoding.writeVarUint8Array(encoder, update);
  return encoding.toUint8Array(encoder);
}

/** 磁盘基线重置通告消息 = MESSAGE_BASELINE_RESET + 权威基线全文（UTF-8）。 */
function writeBaselineResetHeader(text: string): Uint8Array {
  const encoder = encoding.createEncoder();
  encoding.writeVarUint(encoder, MESSAGE_BASELINE_RESET);
  encoding.writeVarString(encoder, text);
  return encoding.toUint8Array(encoder);
}

/** 整体重建 entry 的 doc 到权威基线 baselineText（调用方保证 clean 或已决定重建）。 */
function rebuildEntryToBaseline(e: Entry, baselineText: string): void {
  e.cleanup();
  e.doc.ydoc.destroy();
  const { doc, cleanup } = createDoc(e.doc.file, baselineText);
  e.doc = doc;
  e.cleanup = cleanup;
  e.diskBaseline = baselineText;
  e.lastFlushed = baselineText;
  e.pendingBaseline = null;
  e.lastRemoteAuthor = null;
  // store 侧刷新 binding → NoteEditor 重 render → MarkdownEditor 随 collab 引用变化自动重绑
  onBindingRefresh?.(e.doc.file, doc);
  // 重建后与房间重新收敛（确定性 seed 幂等）
  if (broadcast) {
    broadcast.sendSyncMessage(e.doc.file, writeBaselineResetHeader(baselineText));
    const encoder = encoding.createEncoder();
    writeSyncStep1(encoder, e.doc.ydoc);
    broadcast.sendSyncMessage(e.doc.file, encoding.toUint8Array(encoder));
  }
}

/**
 * 处理磁盘基线重置通告：收敛到磁盘权威基线。
 * - 与本地相同 → no-op（幂等防环）。
 * - clean → 整体重建到通告基线；dirty（正在输入）→ 挂起 pendingBaseline，实时编辑不受影响，
 *   待落盘（markNoteDiskWrite）后以「较新的磁盘文本」收敛（不丢最后协作者版本）。
 */
function handleBaselineReset(file: string, diskText: string): void {
  const e = entries.get(file);
  if (!e) return;
  if (e.diskBaseline === diskText) return;
  // 空基线且本地有非空内容：不整体重建为空（挂起，保留本地内容）。
  // 空基线来自对端「空缓存/加载窗口空态」的误广播（打开被写空的笔记即广播，见 bindNoteDoc），
  // 无条件以空重建会把在线端反复清空（共享仓库下逐端传染、历史反复出现空版本）；
  // 真实清空由用户编辑产生的增量 update（ytext 删除）合入传播，不由全量空基线负责。
  // 挂起后待 markNoteDiskWrite 以「较新的磁盘文本」收敛（不丢本地内容）。
  if (diskText === "" && e.doc.ytext.toString() !== "") {
    e.pendingBaseline = diskText;
    return;
  }
  if (e.doc.ytext.toString() !== e.lastFlushed) {
    // 正在输入（未落盘差异）：挂起；若本地落盘推进了磁盘，则以更新文本收敛而非旧通告
    e.pendingBaseline = diskText;
    return;
  }
  rebuildEntryToBaseline(e, diskText);
}

/**
 * 打开并绑定笔记文档：有激活编辑器（多面板共享）时复用现有 doc；
 * 无激活时以磁盘正文 text 重置基线（快照）后返回。调用方应在编辑器挂载时调用（refCount++）。
 * 传入的 text 为 LF 规范化后的全文（调用方保证）。
 * 重开时先广播 BASELINE_RESET 让房间收敛到磁盘权威基线，再发 syncStep1 握手。
 */
export function bindNoteDoc(file: string, text: string): NoteDoc {
  const existing = entries.get(file);
  if (existing && existing.viewCount > 0) {
    existing.viewCount += 1;
    return existing.doc;
  }
  // 无激活编辑器：清理旧 doc（其挂起定时器/awareness/观察者随之释放），以磁盘基线重建
  existing?.cleanup();
  existing?.doc.ydoc.destroy();
  entries.delete(file);
  const { doc, cleanup } = createDoc(file, text);
  entries.set(file, {
    doc,
    viewCount: 1,
    cleanup,
    diskBaseline: text,
    lastFlushed: text,
    pendingBaseline: null,
    lastRemoteAuthor: null,
  });
  // 先广播磁盘权威基线（对端据此收敛，防重开塞入的新 seed 基线翻倍），再握手
  broadcast?.sendSyncMessage(file, writeBaselineResetHeader(text));
  const encoder = encoding.createEncoder();
  writeSyncStep1(encoder, doc.ydoc);
  broadcast?.sendSyncMessage(file, encoding.toUint8Array(encoder));
  return doc;
}

/** 编辑器卸载时释放（refCount--）；归零仍在注册表（内存开销小，保留远端状态），下次打开重置基线。 */
export function unbindNoteDoc(file: string): void {
  const e = entries.get(file);
  if (!e) return;
  e.viewCount = Math.max(0, e.viewCount - 1);
}

/** 协作态本端落盘完成登记：以当前 ytext（正文）推进 lastFlushed；若既有挂起基线 → 以最新磁盘权威收敛（不丢最后协作者版本）。 */
export function markNoteDiskWrite(file: string): void {
  const e = entries.get(file);
  if (!e) return;
  // 只记正文（ytext）而非调用方全文——磁盘含 frontmatter/CRLF，ytext 仅 LF 正文，不能直接比
  e.lastFlushed = e.doc.ytext.toString();
  const pending = e.pendingBaseline;
  // 空基线也可能作为挂起值（handleBaselineReset 空保护挂起 ""）——必须用 null 判空，
  // 否则挂起的空基线永远无法收敛清理、残留阻塞后续基线收敛
  if (pending === null) return;
  // 本地刚落盘文本若已推进（≠ 挂起旧通告），以本地更新文本为权威（保留在线端输入）；否则以挂起通告为权威
  const target = e.lastFlushed !== pending ? e.lastFlushed : pending;
  if (e.doc.ytext.toString() === target) {
    e.pendingBaseline = null;
    rebuildEntryToBaseline(e, target);
  }
  // 仍 dirty（落盘后又输入）：保留 pendingBaseline，下次落盘再收敛（不吞未落盘输入）
}

/**
 * 协作态本地正文同步：把调用方 content 的正文（LF）写回该笔记的 `Y.Text`。
 * 源码模式编辑只走 content（不经 yCollab 绑定），不写回 ytext 会让 ytext 陈旧——切回实时预览
 * 时 MarkdownEditor 以陈旧 ytext 为编辑模型源并回传 content，源码编辑被回退。实时预览编辑的
 * ytext 已由 yCollab 同步，`toString` 一致即 no-op（无回环）；写回触发 ytext update 广播，对端实时可见。
 * **整篇替换语义（本地最新者胜）**：对端若有未收敛编辑会被整篇覆盖——属刻意取舍（挂载分歧
 * 「脏→本地胜」处置用），非增量合并。
 */
export function applyLocalBody(file: string, bodyLF: string): void {
  const e = entries.get(file);
  if (!e) return;
  const y = e.doc.ytext;
  if (y.toString() === bodyLF) return;
  y.delete(0, y.length);
  y.insert(0, bodyLF);
}

/**
 * 合入远端 sync 消息。
 * - `MESSAGE_BASELINE_RESET`：解析权威基线全文，收敛到磁盘基线（见 handleBaselineReset）。
 * - 普通 y-protocols 消息：按既有逻辑经 readSyncMessage 合入。本实现依赖「重开端在握手前先广播
 *   BASELINE_RESET（relay 按发送顺序转发，端点先收敛再换状态）」保证不翻倍——即不在普通消息里
 *   做 seed 基线消歧（那需要解析 update 内部 struct，脆弱）。乱序/键入期间到达的异基线内容会
 *   在后续收敛（A 落盘后 markNoteDiskWrite 以最新磁盘收敛，B 再采纳）中自愈，见文件头残余说明。
 * - step1 要求回复 step2（全量状态）——编码输出广播给房间（幂等收敛，对端请求者应用）。
 */
export function receiveSyncMessage(
  file: string,
  payload: Uint8Array,
  remoteAuthor?: NoteRemoteAuthor,
): void {
  const e = entries.get(file);
  if (!e) return;
  // 先识别是否基线重置通告
  let first = -1;
  try {
    const d = decoding.createDecoder(payload);
    first = decoding.readVarUint(d);
  } catch {
    return;
  }
  if (first === MESSAGE_BASELINE_RESET) {
    let diskText = "";
    try {
      const d = decoding.createDecoder(payload);
      decoding.readVarUint(d);
      diskText = decoding.readVarString(d);
    } catch {
      return;
    }
    handleBaselineReset(file, diskText);
    return;
  }
  // 记录本次远端合入作者（历史按操作人署名用）+ 置远端应用标记：
  // y-codemirror 在 yjs 事务内同步回写 CM → onBodyChange→handleChange 与此同栈，
  // NoteEditor 据此区分「远端合入」（内容变化来自协作对端，不署本端用户）。
  e.lastRemoteAuthor = remoteAuthor ?? null;
  remoteApplyDepth++;
  // 普通 y-protocols sync 消息：按标准流程合入（见函数头部说明，不在普通消息做 seed 消歧）
  const decoder = decoding.createDecoder(payload);
  const encoder = encoding.createEncoder();
  let replyNeeded = false;
  try {
    const type = readSyncMessage(decoder, encoder, e.doc.ydoc, REMOTE_ORIGIN, (err) => {
      console.error("笔记协作同步合入失败", err);
    });
    replyNeeded = type === messageYjsSyncStep1;
  } catch {
    // 解析失败的消息（乱序/格式异常）：丢弃，下一帧握手兜底收敛
    return;
  } finally {
    remoteApplyDepth--;
  }
  if (replyNeeded && broadcast) {
    broadcast.sendSyncMessage(file, encoding.toUint8Array(encoder));
  }
}

/** 合入远端 awareness 更新（只应用不回发，防回环；y-remote-selections 监听 change 重绘）。 */
export function receiveAwareness(file: string, payload: Uint8Array): void {
  const e = entries.get(file);
  if (!e) return;
  applyAwarenessUpdate(e.doc.awareness, payload, "remote");
}

/** 全部销毁（应用退出/切仓库清空协作上下文）。 */
export function destroyAllNoteDocs(): void {
  for (const e of entries.values()) {
    e.cleanup();
    e.doc.ydoc.destroy();
  }
  entries.clear();
}

/** 重连后对所有激活协作文档重发 syncStep1（重新握手，索取对端全量状态收敛）。 */
export function resyncAllNoteDocs(): void {
  if (!broadcast) return;
  for (const e of entries.values()) {
    if (e.viewCount <= 0) continue;
    const encoder = encoding.createEncoder();
    writeSyncStep1(encoder, e.doc.ydoc);
    broadcast.sendSyncMessage(e.doc.file, encoding.toUint8Array(encoder));
  }
}
