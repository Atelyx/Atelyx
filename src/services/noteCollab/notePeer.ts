/**
 * 笔记协作文档 peer（可实例化状态机）：每篇打开的笔记持一个 `Y.Doc`（根 `Y.Text` = 正文 Markdown），
 * 负责收发协作帧、维护基线标签与磁盘基线、把内容变更落到共享基线上。
 *
 * 数据正确性依赖三条不变量：
 * 1. **基线一致**：文档的 clientID=1 struct 流恒等于其当前基线文本的确定性 seed
 *    （`baselineSeedUpdate`，固定 clientID 1）——同文本各端字节一致，合并幂等。
 * 2. **跨基线不合并**：`note-sync` 帧携带发送方基线标签，标签不同则不应用该帧，改走基线再协商。
 *    共享时钟空间被不同文本复用会让 Yjs 按本地状态向量截断对端 struct 内容（尾部混入异文本
 *    或静默分歧），因此异基线状态永不进入同一文档。
 * 3. **标签即真相**：`(seq, author, id)` 与基线正文一一对应；本端合并结果若与采纳的通告文本不同，
 *    立即以更高 `seq` 发布新基线，使房间收敛到同一 seed。
 *
 * 内容变更分两类：
 * - **编辑**（用户输入、撤销重做、外部改盘、回滚、冲突处置）：以 `最近落盘文本` 为共同祖先做
 *   `merge3`，再按 `diffHunks` 差量落到共享基线上——整篇重写会让同一文本以不同客户端并存而重复。
 * - **采纳**（对端通告更高序基线）：`merge3` 后整体重建为 `seed(合并文本)`，使各端基线字节一致。
 *
 * 合并精度（明确的取舍）：`merge3` 只在**区间不重叠**时两侧都保留；一个 hunk 的替换文本是原子整体
 * （无法按子区间拆分），因此两侧 hunk 区间真正重叠时按序号高者整块取胜、败方该 hunk 改动被丢弃，
 * 不做内容拼接（拼接会把同一段内容并排留下，正是本模块要消除的症状）。落在对方区间端点上的插入不属重叠。
 *
 * 磁盘基线语义：`lastFlushed` = 最近确认已落盘的正文，只由 `markDiskWrite` 推进；它同时是三方合并的
 * 共同祖先，因此**已存在文档**不会把未落盘内容当成已落盘（新建文档时以调用方传入的正文为起点，
 * 调用方保证它是磁盘最新正文或本端即将落盘、已按差量写回文档的正文）。
 */
import * as Y from "yjs";
import { Awareness, applyAwarenessUpdate, encodeAwarenessUpdate } from "y-protocols/awareness";
import { messageYjsSyncStep1, readSyncMessage, writeSyncStep1 } from "y-protocols/sync";
import * as decoding from "lib0/decoding";
import * as encoding from "lib0/encoding";
import {
  baselineIdOf,
  compareTag,
  decodeNoteFrame,
  encodeNoteBaseline,
  encodeNoteSync,
  sameContent,
  type BaselineTag,
} from "./frame";
import { diffHunks, merge3, type TextHunk } from "./textChange";

/** 确定性磁盘基线 seed 客户端 id：各端以同一正文重建基线时 struct 字节一致，合并幂等。 */
const BASELINE_SEED_CLIENT_ID = 1;

/** 远端合入 origin 标记：本端 applyUpdate 用它，doc 'update' 事件据此跳过回发。 */
const REMOTE_ORIGIN = "note-collab-remote";

/** 本端 awareness（光标/选中）广播节流：高频合并，防止每次选区变化刷屏 relay。 */
const AWARE_THROTTLE_MS = 100;

/** 异基线帧触发回通告的最小间隔（防同一文件在收敛窗口内刷屏）。 */
const BASELINE_REPLY_MS = 2000;

/** 无激活编辑面的文档保留上限（超出按最久未触达销毁；重建由基线协议无损收敛）。 */
const MAX_RETAINED_DOCS = 32;

/** 房间基线序号记忆上限（只存序号不存文本，超出淘汰最旧）。 */
const MAX_ROOM_SEQ_ENTRIES = 1024;

/** 以固定 seed 客户端把正文编码成一个确定的 Yjs update（导出供测试锁定幂等契约）。 */
export function baselineSeedUpdate(text: string): Uint8Array {
  const seed = new Y.Doc();
  seed.clientID = BASELINE_SEED_CLIENT_ID;
  seed.getText("text").insert(0, text);
  const update = Y.encodeStateAsUpdate(seed);
  seed.destroy();
  return update;
}

/** 远端合入作者（历史按操作人署名用：协作对端经 relay 广播的内容变化，作者 = 发送端身份）。 */
export interface NoteRemoteAuthor {
  id: string;
  name: string;
  device: string;
}

/** 本地 awareness 身份（昵称与用户色）。 */
export interface NoteIdentity {
  name: string;
  color: string;
}

/** 文档实例（协作文档的形状；ytext/awareness 供编辑面绑定）。 */
export interface NoteDocInstance {
  file: string;
  ydoc: Y.Doc;
  ytext: Y.Text;
  awareness: Awareness;
}

/** 基线标签的可变引用：文档重建后更新广播需读到新标签。 */
interface TagRef {
  tag: BaselineTag;
}

/** peer 依赖：出站广播、文档重建通知、本端身份。 */
export interface NotePeerDeps {
  /** 广播 `note-sync` 帧（未连接时由接线侧丢弃）。 */
  send: (file: string, payload: Uint8Array) => void;
  /** 广播 `note-aware` 原始 awareness 更新（未连接时由接线侧丢弃）。 */
  sendAwareness: (file: string, payload: Uint8Array) => void;
  /** 文档被整体重建后通知调用方刷新绑定（服务不 import store，靠回调反哺）。 */
  onDocRebuilt: (file: string, doc: NoteDocInstance) => void;
  /** 本会话稳定对端身份 id（基线标签的 `author`：全序破平局）。 */
  author: string;
}

/** 笔记协作 peer 接口（可实例化状态机的对外面）。 */
export interface NotePeer {
  /** 打开/复用文档（引用计数 +1）。复用即按磁盘正文并入/收敛既有内容（不是不动内容）。
   *  `text` 应为磁盘最新正文（调用方保证直读盘，不用可能滞后的内容缓存）。 */
  open(file: string, text: string, identity: NoteIdentity): NoteDocInstance;
  /** 释放引用（归零保留文档：远端状态与 CRDT 真值留内存，供下次打开与房间收敛）。 */
  release(file: string): void;
  /** 内容收敛：把目标正文按最小差量落到共享基线上（等价于把目标文本编辑出来）。 */
  syncBody(file: string, bodyLF: string): void;
  /** 登记「该正文已落盘」：推进三方合并的共同祖先。 */
  markDiskWrite(file: string, bodyLF: string): void;
  /** 处理入站帧（peerId = 来源连接，用于记录实证不兼容的对端）。 */
  receive(
    file: string,
    payload: Uint8Array,
    peerId: number,
    remoteAuthor?: NoteRemoteAuthor,
  ): void;
  /** 合入远端 awareness（只应用不回发）。 */
  applyRemoteAwareness(file: string, payload: Uint8Array): void;
  /** 对全部激活文档重发 syncStep1（重连/relay 缺帧/周期反熵）。 */
  resyncActive(): void;
  /** 当前是否正在应用远端 Yjs update（编辑面据此区分远端合入与本地编辑）。 */
  isRemoteApplying(): boolean;
  /** 最近一次远端合入作者（无 = null）。 */
  getLastRemoteAuthor(file: string): NoteRemoteAuthor | null;
  /** 文档正文真值（无该文件文档 = null）。 */
  getText(file: string): string | null;
  /** 当前基线标签（诊断/测试用）。 */
  getTag(file: string): BaselineTag | null;
  /** 销毁单文件文档（文件改名/移动/删除：路径即身份，防同名新文件串内容）。 */
  destroyDoc(file: string): void;
  /** 销毁某目录前缀下的全部文档（文件夹改名/移动：目录内笔记的路径身份一并失效）。 */
  disposeDocsUnder(dir: string): void;
  /** 销毁全部文档（切仓库/协作停用）。 */
  destroyAll(): void;
}

interface Entry {
  doc: NoteDocInstance;
  tagRef: TagRef;
  /** 当前文档基线的 seed 正文（`tagRef.tag.id` 即它的内容标识，通告与兼容判定据它复核）。 */
  baselineText: string;
  /** 最近确认已落盘的正文（LF）；三方合并的共同祖先。 */
  lastFlushed: string;
  /** 最近一次远端合入的作者（历史署名用；无远端合入 = null）。 */
  lastRemoteAuthor: NoteRemoteAuthor | null;
  /** 本地身份（重建后重设 awareness）。 */
  identity: NoteIdentity;
  /** 激活引用计数：归零即无编辑面，但文档留内存继续参与房间收敛。 */
  refcount: number;
  /** 最近一次被 open/receive 触达的时刻（保留上限淘汰用）。 */
  touchedAt: number;
  /** 已从注册表摘除：不再复用于打开、不再接收帧，且已静音（不再发出任何帧）。 */
  retired: boolean;
  /** 底层文档已销毁（防重复销毁）。 */
  destroyed: boolean;
  /** 掐断出站（retire 时立刻调用：窗口内编辑只落盘、不进 CRDT 广播）。 */
  silence: () => void;
  /** 销毁/重建前清理挂起定时器与 awareness（防残留定时器把旧 awareness 发进新房间）。 */
  cleanup: () => void;
}

/** 出站 y-protocols `update` 消息编码 = 消息类型头 + update 字节。 */
function encodeUpdateMessage(update: Uint8Array): Uint8Array {
  const encoder = encoding.createEncoder();
  encoding.writeVarUint(encoder, 2); // messageYjsUpdate
  encoding.writeVarUint8Array(encoder, update);
  return encoding.toUint8Array(encoder);
}

/** 正文差量 → `Y.Text.applyDelta` 的 delta：未列出的尾部内容自动保留，一次事务只发一个 update。 */
function hunksToDelta(
  hunks: TextHunk[],
): Array<{ retain?: number; delete?: number; insert?: string }> {
  const delta: Array<{ retain?: number; delete?: number; insert?: string }> = [];
  let pos = 0;
  for (const hunk of hunks) {
    if (hunk.at > pos) delta.push({ retain: hunk.at - pos });
    if (hunk.remove > 0) delta.push({ delete: hunk.remove });
    if (hunk.insert) delta.push({ insert: hunk.insert });
    pos = hunk.at + hunk.remove;
  }
  return delta;
}

function applyIdentity(awareness: Awareness, identity: NoteIdentity): void {
  awareness.setLocalStateField("user", {
    name: identity.name,
    color: identity.color,
    colorLight: `${identity.color}33`,
  });
}

/** 创建 peer 实例：状态全部实例内持有，便于测试在同一进程内驱动多端。 */
export function createNotePeer(deps: NotePeerDeps): NotePeer {
  const entries = new Map<string, Entry>();
  /** 房间各文件已知的最大基线序号（只记序号；本端提案时取更高序，减少一轮协商）。 */
  const roomSeqs = new Map<string, number>();
  /** 各文件最近一次「异基线回通告」的时刻。 */
  const baselineReplyAt = new Map<string, number>();
  /**
   * 实证与本地基线正文不一致的对端（键 = `${peerId}\u0000${file}`）。
   * 通告帧自带基线正文，可逐字符复核：同内容标识但基线正文不同（散列碰撞或伪造）时记入本集合，
   * 之后该对端带此标识的同步帧一律不应用。
   */
  const incompatiblePeers = new Set<string>();
  /** 远端 Yjs update 应用深度（同步窗口内 >0）：编辑面据此区分远端合入与本地编辑。 */
  let remoteApplyDepth = 0;

  function rememberRoomSeq(file: string, seq: number): void {
    if (seq <= (roomSeqs.get(file) ?? 0)) return;
    roomSeqs.delete(file);
    roomSeqs.set(file, seq);
    while (roomSeqs.size > MAX_ROOM_SEQ_ENTRIES) {
      const oldest = roomSeqs.keys().next().value;
      if (oldest === undefined) break;
      roomSeqs.delete(oldest);
    }
  }

  function nextSeq(file: string): number {
    return (roomSeqs.get(file) ?? 0) + 1;
  }

  function sendBaseline(e: Entry): void {
    deps.send(
      e.doc.file,
      encodeNoteBaseline(e.tagRef.tag, e.baselineText, e.doc.ytext.toString()),
    );
  }

  /** 回通告本端基线（限频）：让异基线对端采纳本端，而不是做跨基线合并。 */
  function replyBaseline(e: Entry): void {
    const now = Date.now();
    if (now - (baselineReplyAt.get(e.doc.file) ?? 0) < BASELINE_REPLY_MS) return;
    baselineReplyAt.set(e.doc.file, now);
    sendBaseline(e);
  }

  function sendSyncStep1(e: Entry): void {
    const encoder = encoding.createEncoder();
    writeSyncStep1(encoder, e.doc.ydoc);
    deps.send(e.doc.file, encodeNoteSync(e.tagRef.tag, encoding.toUint8Array(encoder)));
  }

  /** 新建文档实例：以确定性 seed 建基线，装配增量广播与 awareness 节流广播。 */
  function createInstance(
    file: string,
    text: string,
    identity: NoteIdentity,
    tagRef: TagRef,
  ): { doc: NoteDocInstance; cleanup: () => void; silence: () => void } {
    const ydoc = new Y.Doc();
    const ytext = ydoc.getText("text");
    Y.applyUpdate(ydoc, baselineSeedUpdate(text));
    const awareness = new Awareness(ydoc);
    /** 出站闸门：置 false 后本实例不再发出任何帧（同步与 awareness）。 */
    let alive = true;

    ydoc.on("update", (update: Uint8Array, origin: unknown) => {
      if (!alive || origin === REMOTE_ORIGIN) return;
      deps.send(file, encodeNoteSync(tagRef.tag, encodeUpdateMessage(update)));
    });

    /** 待广播的本端 awareness 变更 client 集合：窗口内累积，避免覆盖式赋值丢掉同窗口的其它变更。 */
    const awareClients = new Set<number>();
    let awareTimer: ReturnType<typeof setTimeout> | null = null;
    awareness.on(
      "update",
      (
        { added, updated, removed }: { added: number[]; updated: number[]; removed: number[] },
        origin: unknown,
      ) => {
        if (!alive || origin === "remote") return;
        for (const clientId of [...added, ...updated, ...removed]) awareClients.add(clientId);
        if (awareClients.size === 0 || awareTimer !== null) return;
        awareTimer = setTimeout(() => {
          awareTimer = null;
          if (!alive) return;
          const payload = encodeAwarenessUpdate(awareness, [...awareClients]);
          awareClients.clear();
          deps.sendAwareness(file, payload);
        }, AWARE_THROTTLE_MS);
      },
    );
    // 身份在监听挂载之后写入：监听挂载前的 setLocalState 无人接收，首帧不会带出本端光标身份
    applyIdentity(awareness, identity);

    /** 掐断出站（不动 awareness 实例，文档仍可读可写、编辑面仍可安全引用）。 */
    const silence = () => {
      alive = false;
      if (awareTimer !== null) {
        clearTimeout(awareTimer);
        awareTimer = null;
      }
      awareClients.clear();
    };

    const cleanup = () => {
      // 先关闸：awareness.destroy() 内部会同步 emit 'update'（origin 'local'），
      // 否则又排一个定时器把旧路径的 awareness 发进新房间
      silence();
      awareness.destroy();
    };
    return { doc: { file, ydoc, ytext, awareness }, cleanup, silence };
  }

  function createEntry(file: string, text: string, identity: NoteIdentity): Entry {
    const tagRef: TagRef = {
      tag: { seq: nextSeq(file), author: deps.author, id: baselineIdOf(text) },
    };
    const created = createInstance(file, text, identity, tagRef);
    const entry: Entry = {
      doc: created.doc,
      tagRef,
      baselineText: text,
      lastFlushed: text,
      lastRemoteAuthor: null,
      identity,
      refcount: 0,
      touchedAt: Date.now(),
      retired: false,
      destroyed: false,
      silence: created.silence,
      cleanup: created.cleanup,
    };
    entries.set(file, entry);
    rememberRoomSeq(file, tagRef.tag.seq);
    return entry;
  }

  /** 整体重建 entry 为「文本 M + 标签 tag」：旧实例销毁、awareness 按本地身份重建。 */
  function rebuildEntry(e: Entry, text: string, tag: BaselineTag): void {
    const file = e.doc.file;
    e.cleanup();
    e.doc.ydoc.destroy();
    e.tagRef.tag = tag;
    e.baselineText = text;
    const created = createInstance(file, text, e.identity, e.tagRef);
    e.doc = created.doc;
    e.cleanup = created.cleanup;
    e.silence = created.silence;
    // lastFlushed 不随重建推进：当前文本未必在磁盘上（未落盘内容不得记为已落盘）
    rememberRoomSeq(file, tag.seq);
    deps.onDocRebuilt(file, created.doc);
  }

  /**
   * 销毁底层文档：经宏任务延后一拍再销毁，避开「编辑面解绑/重绑到新文档」与 React 提交之间的窗口。
   * 该窗口内旧文档已静音（见 `retire`），因此窗口内的输入只会经会话保存链落盘、不产生任何出站帧；
   * 彻底消除窗口风险需要能确定「编辑面已重绑」，此处以静音把可观测后果收敛为「不进 CRDT」。
   */
  function scheduleDestroy(e: Entry): void {
    if (!e.retired || e.destroyed) return;
    setTimeout(() => {
      if (!e.retired || e.destroyed) return;
      e.destroyed = true;
      e.cleanup();
      e.doc.ydoc.destroy();
    }, 0);
  }

  /** 从注册表摘除：立刻静音（不再发出任何帧）+ 不再复用/不再接收帧，随后销毁底层文档。 */
  function retire(e: Entry): void {
    const file = e.doc.file;
    if (entries.get(file) === e) entries.delete(file);
    baselineReplyAt.delete(file);
    clearIncompatible(file);
    e.retired = true;
    e.silence();
    scheduleDestroy(e);
  }

  /** 保留上限淘汰：摘除最久未触达且无激活编辑面的文档（重建走基线协议无损收敛）。 */
  function evictIdleDocs(): void {
    if (entries.size <= MAX_RETAINED_DOCS) return;
    const idle = [...entries.values()]
      .filter((e) => e.refcount === 0)
      .sort((a, b) => a.touchedAt - b.touchedAt);
    for (const e of idle) {
      if (entries.size <= MAX_RETAINED_DOCS) break;
      retire(e);
    }
  }

  function applyHunks(e: Entry, hunks: TextHunk[]): void {
    if (hunks.length === 0) return;
    e.doc.ytext.applyDelta(hunksToDelta(hunks));
  }

  function receiveSync(e: Entry, payload: Uint8Array, remoteAuthor?: NoteRemoteAuthor): void {
    e.lastRemoteAuthor = remoteAuthor ?? null;
    remoteApplyDepth++;
    const decoder = decoding.createDecoder(payload);
    const encoder = encoding.createEncoder();
    let replyStep2 = false;
    try {
      replyStep2 =
        readSyncMessage(decoder, encoder, e.doc.ydoc, REMOTE_ORIGIN, (err) => {
          console.error("笔记协作同步合入失败", err);
        }) === messageYjsSyncStep1;
    } catch {
      // 解析失败的消息（乱序/格式异常）：丢弃，下一轮握手兜底收敛
      remoteApplyDepth--;
      return;
    }
    remoteApplyDepth--;
    if (replyStep2) {
      deps.send(e.doc.file, encodeNoteSync(e.tagRef.tag, encoding.toUint8Array(encoder)));
    }
  }

  /**
   * 采纳更高序基线：以最近落盘正文为共同祖先三方合并后整体重建（非重叠两侧改动都保留，同区间取通告方）。
   * - 合并结果 == 通告方当前正文，且其当前正文正是其基线正文（标签可复核）→ 直接采纳其标签，
   *   本端与对端落在同一 seed 上，不再多一轮提案。
   * - 否则以更高序发布合并文本（房间据此采纳同一 seed），避免两文档各自带着不同 seed 继续交换状态。
   */
  function adoptBaseline(e: Entry, tag: BaselineTag, currentText: string): void {
    const current = e.doc.ytext.toString();
    const merged = merge3(e.lastFlushed, current, currentText, "theirs");
    const sameBaseline = merged === currentText && baselineIdOf(currentText) === tag.id;
    const nextTag: BaselineTag = sameBaseline
      ? tag
      : { seq: nextSeq(e.doc.file), author: deps.author, id: baselineIdOf(merged) };
    rebuildEntry(e, merged, nextTag);
    // 基线已替换：此前基于旧基线的「实证不兼容」结论作废，由后续通告重新复核
    clearIncompatible(e.doc.file);
    if (!sameBaseline) sendBaseline(e);
    sendSyncStep1(e);
  }

  function clearIncompatible(file: string): void {
    const suffix = `\u0000${file}`;
    for (const key of [...incompatiblePeers]) {
      if (key.endsWith(suffix)) incompatiblePeers.delete(key);
    }
  }

  function peerKey(peerId: number, file: string): string {
    return `${peerId}\u0000${file}`;
  }

  return {
    open(file, text, identity) {
      const existing = entries.get(file);
      if (existing) {
        existing.refcount += 1;
        existing.touchedAt = Date.now();
        existing.identity = identity;
        applyIdentity(existing.doc.awareness, identity);
        // 保留文档 = CRDT 真值（含对端已广播、本端尚未落盘的内容），磁盘文本按既有外部修改策略并入：
        // 文档无未落盘差异时磁盘即权威；有未落盘差异时非重叠改动并入、同区间以内存为准，
        // 不被旧快照整体回退（回退会让磁盘覆盖房间已收敛的内容）
        const current = existing.doc.ytext.toString();
        const target =
          current === existing.lastFlushed
            ? text
            : merge3(existing.lastFlushed, current, text, "ours");
        if (target !== current) applyHunks(existing, diffHunks(current, target));
        // 打开文本即磁盘当前内容：磁盘基线随之对齐（后续三方合并以它为共同祖先）
        existing.lastFlushed = text;
        return existing.doc;
      }
      const entry = createEntry(file, text, identity);
      entry.refcount = 1;
      evictIdleDocs();
      // 新建文档先通告本地基线（更高序则房间采纳本端），再握手索取对端状态
      sendBaseline(entry);
      sendSyncStep1(entry);
      return entry.doc;
    },

    release(file) {
      const e = entries.get(file);
      if (!e) return;
      e.refcount = Math.max(0, e.refcount - 1);
      e.touchedAt = Date.now();
      evictIdleDocs();
    },

    syncBody(file, bodyLF) {
      const e = entries.get(file);
      if (!e) return;
      const current = e.doc.ytext.toString();
      if (current === bodyLF) return;
      e.touchedAt = Date.now();
      applyHunks(e, diffHunks(current, bodyLF));
    },

    markDiskWrite(file, bodyLF) {
      const e = entries.get(file);
      if (!e) return;
      e.lastFlushed = bodyLF;
    },

    receive(file, payload, peerId, remoteAuthor) {
      const frame = decodeNoteFrame(payload);
      if (!frame) return;
      const e = entries.get(file);
      if (frame.kind === "resync") {
        if (!e) return;
        // 对端消费过慢被裁剪：重发本端握手（对端同样回发，双向补齐缺失增量）
        sendSyncStep1(e);
        return;
      }
      rememberRoomSeq(file, frame.tag.seq);
      if (!e) return;
      e.touchedAt = Date.now();
      const localTag = e.tagRef.tag;
      if (frame.kind === "sync") {
        // 只应用「基线正文一致」的对端状态：异正文基线共享 clientID=1 时钟空间，
        // 合并会按本地状态向量截断对端 struct 内容（尾部混入异文本/静默分歧）。
        // 同步帧只带标识（不带正文），故「同标识异正文」的碰撞/伪造只能由通告帧的正文复核发现。
        if (!sameContent(localTag, frame.tag) || incompatiblePeers.has(peerKey(peerId, file))) {
          replyBaseline(e);
          return;
        }
        receiveSync(e, frame.payload, remoteAuthor);
        return;
      }
      // 基线通告自带基线正文：逐字符复核内容标识，防散列碰撞/伪造把异正文当同基线合并
      if (baselineIdOf(frame.baselineText) !== frame.tag.id) return;
      if (sameContent(localTag, frame.tag)) {
        if (frame.baselineText === e.baselineText) {
          // 同基线正文、不同 seq/author：字节一致的 seed，按普通增量合并即可（无需重建）；
          // 若此前因标识碰撞把它记成不兼容，回一次通告让对方也解除（否则单向永久拉黑）
          if (incompatiblePeers.delete(peerKey(peerId, file))) replyBaseline(e);
          return;
        }
        // 同内容标识但基线正文不同：标识碰撞或伪造。同步帧只带标识（不带正文），故这里是唯一防线：
        // 记入不兼容后其带该标识的同步帧一律不应用。**不回通告**——双方互拉黑时回通告只会形成 2s 互刷，
        // 且真正需要的是「谁知道对方正文」的判定，回通告无从推进；改为一次性告警便于定位。
        const key = peerKey(peerId, file);
        if (!incompatiblePeers.has(key)) {
          incompatiblePeers.add(key);
          console.warn("笔记协作：对端基线标识与本端相同但基线正文不同，已停止合并其状态", file);
        }
        return;
      }
      if (compareTag(frame.tag, localTag) <= 0) {
        replyBaseline(e);
        return;
      }
      adoptBaseline(e, frame.tag, frame.currentText);
    },

    applyRemoteAwareness(file, payload) {
      const e = entries.get(file);
      if (!e) return;
      applyAwarenessUpdate(e.doc.awareness, payload, "remote");
    },

    resyncActive() {
      for (const e of entries.values()) {
        if (e.refcount <= 0) continue;
        sendSyncStep1(e);
      }
    },

    isRemoteApplying() {
      return remoteApplyDepth > 0;
    },

    getLastRemoteAuthor(file) {
      return entries.get(file)?.lastRemoteAuthor ?? null;
    },

    getText(file) {
      return entries.get(file)?.doc.ytext.toString() ?? null;
    },

    getTag(file) {
      return entries.get(file)?.tagRef.tag ?? null;
    },

    destroyDoc(file) {
      const e = entries.get(file);
      if (!e) return;
      retire(e);
    },

    disposeDocsUnder(dir) {
      const prefix = `${dir}/`;
      for (const e of [...entries.values()]) {
        if (e.doc.file.startsWith(prefix)) retire(e);
      }
    },

    destroyAll() {
      for (const e of [...entries.values()]) retire(e);
      entries.clear();
      roomSeqs.clear();
      baselineReplyAt.clear();
      incompatiblePeers.clear();
    },
  };
}
