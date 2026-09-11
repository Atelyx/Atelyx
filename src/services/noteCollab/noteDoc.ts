/**
 * 笔记协作接线（模块级单例）：把 `notePeer` 的可实例化状态机接到应用的广播与重建回调上，
 * 对上层（`stores/noteCollabStore`）暴露稳定的函数面。
 *
 * 网络收发经 `noteCollabStore` 接线注入的广播钩子完成，本模块不直连 relay；
 * 文档被整体重建（采纳对端基线）时经 `onBindingRefresh` 通知 store 刷新绑定，
 * 使编辑面随 ytext/awareness 引用变化重绑。
 */
import {
  baselineSeedUpdate,
  createNotePeer,
  type NoteDocInstance,
  type NoteIdentity,
  type NotePeer,
  type NoteRemoteAuthor,
} from "./notePeer";

export { baselineSeedUpdate };
export type { NoteIdentity, NoteRemoteAuthor };
/** 协作文档实例（ytext/awareness 供编辑面绑定）。 */
export type NoteDoc = NoteDocInstance;

/** 网络广播钩子（由 collabStore 注入；未启用协作时为 null）。 */
export interface NoteCollabBroadcast {
  /** 广播 y-protocols 同步帧（基线标签 + 内嵌同步消息）。 */
  sendSyncMessage: (file: string, payload: Uint8Array) => void;
  /** 广播 awareness 更新（y-protocols 编码）。 */
  sendAwareness: (file: string, payload: Uint8Array) => void;
}

let broadcast: NoteCollabBroadcast | null = null;

/** doc 实例被整体重建后通知 store 刷新 binding（service 不 import store，靠回调反哺）。 */
let onBindingRefresh: ((file: string, doc: NoteDoc) => void) | null = null;

/** 本会话稳定对端身份 id：基线标签按全序比较需要唯一值（同序号时才比对它）。 */
const PEER_AUTHOR = `peer-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;

const peer: NotePeer = createNotePeer({
  send: (file, payload) => broadcast?.sendSyncMessage(file, payload),
  sendAwareness: (file, payload) => broadcast?.sendAwareness(file, payload),
  onDocRebuilt: (file, doc) => onBindingRefresh?.(file, doc),
  author: PEER_AUTHOR,
});

/** collabStore 注入/解除网络广播钩子（协作开关切换时调用）。 */
export function setNoteCollabBroadcast(hooks: NoteCollabBroadcast | null): void {
  broadcast = hooks;
}

/** 注册 doc 重建后的 binding 刷新回调（noteCollabStore 注入，用于更新 bindings[file] 触发编辑器重绑）。 */
export function setNoteCollabBindingRefresh(
  fn: ((file: string, doc: NoteDoc) => void) | null,
): void {
  onBindingRefresh = fn;
}

/** 打开并绑定笔记文档：有激活编辑面时复用现有文档；否则以磁盘正文 text 建基线（调用方保证 LF 归一化、且为磁盘最新内容）。 */
export function bindNoteDoc(file: string, text: string, identity: NoteIdentity): NoteDoc {
  return peer.open(file, text, identity);
}

/** 释放一个引用（多编辑面各释放一次）；归零后文档留内存继续参与房间收敛。 */
export function unbindNoteDoc(file: string): void {
  peer.release(file);
}

/** 正文收敛：把目标正文按最小差量落到共享基线上（源码模式/撤销重做/外部改盘/回滚共用）。 */
export function applyLocalBody(file: string, bodyLF: string): void {
  peer.syncBody(file, bodyLF);
}

/** 协作态本端落盘完成登记：推进三方合并的共同祖先。 */
export function markNoteDiskWrite(file: string, bodyLF: string): void {
  peer.markDiskWrite(file, bodyLF);
}

/** 合入远端帧（peerId = 来源连接；异基线帧只回通告、不合并）。 */
export function receiveSyncMessage(
  file: string,
  payload: Uint8Array,
  peerId: number,
  remoteAuthor?: NoteRemoteAuthor,
): void {
  peer.receive(file, payload, peerId, remoteAuthor);
}

/** 合入远端 awareness 更新（只应用不回发，防回环）。 */
export function receiveAwareness(file: string, payload: Uint8Array): void {
  peer.applyRemoteAwareness(file, payload);
}

/** 当前是否正在应用远端 Yjs update（协作回环/对端收敛时同步调用栈内为 true）。 */
export function isRemoteNoteApplyActive(): boolean {
  return peer.isRemoteApplying();
}

/** 取某文件最近一次远端合入的作者（无 = null）。 */
export function getLastRemoteAuthor(file: string): NoteRemoteAuthor | null {
  return peer.getLastRemoteAuthor(file);
}

/** 销毁单文件文档（文件改名/移动/删除：路径即身份，防同名新文件串内容）。 */
export function destroyNoteDoc(file: string): void {
  peer.destroyDoc(file);
}

/** 销毁某目录前缀下的全部文档（文件夹改名/移动：目录内笔记的路径身份一并失效）。 */
export function disposeNoteDocsUnder(dir: string): void {
  peer.disposeDocsUnder(dir);
}

/** 全部销毁（应用退出/切仓库清空协作上下文）。 */
export function destroyAllNoteDocs(): void {
  peer.destroyAll();
}

/** 重连/relay 缺帧/周期反熵：对所有激活文档重发 syncStep1 索取对端全量状态。 */
export function resyncAllNoteDocs(): void {
  peer.resyncActive();
}
