/**
 * 笔记协作运行时：每笔记 `Y.Doc` 生命周期的单例编排（组件不直连 service 的桥）。
 *
 * NoteEditor 打开笔记（协作态）时经本 store 绑定/解绑协作文档，并把绑定对象（ytext + awareness）
 * 以 props 传给 MarkdownEditor 做 y-codemirror 绑定；保存仍走 vaultStore（收敛后全文写盘）。
 * 本 store 只做生命周期与身份登记；网络收发经 `ensureNoteCollabWiring` 注册到协作宿主
 * （collabStore 通道注册表 + 发送 sink 注入，见下方接线）。
 *
 * 多面板打开同一笔记共享同一 `Y.Doc`（底层 noteDoc 引用计数），防多 doc 分叉。
 */
import { create } from "zustand";
import type { Text as YText } from "yjs";
import type { Awareness } from "y-protocols/awareness";
import {
  applyLocalBody,
  bindNoteDoc,
  unbindNoteDoc,
  setNoteCollabIdentity,
  markNoteDiskWrite,
  setNoteCollabBindingRefresh,
  destroyAllNoteDocs,
  getLastRemoteAuthor,
  isRemoteNoteApplyActive,
  receiveAwareness,
  receiveSyncMessage,
  resyncAllNoteDocs,
  setNoteCollabBroadcast,
  type NoteRemoteAuthor,
} from "@/services/noteCollab/noteDoc";
import {
  collabSendSink,
  registerCollabChannel,
  registerCollabReconnect,
  registerCollabTeardown,
  useCollabStore,
} from "@/stores/collabStore";
import { base64ToBytes, bytesToBase64 } from "@/utils/base64";

/** 笔记域协作接线（wireCollabDomains 调用，幂等一次）：
 *  注册 note-sync/note-aware 通道 handler（relay 载荷为不透明 base64，解码与作者解析归本域）、
 *  重连重握手、拆卸清理与广播钩子注入（经 collabSendSink 惰性读宿主 handle：断开时 no-op、
 *  重连后自动指向新连接，无需重注入）。
 *  依赖方向：collabStore 不 import 本模块，本模块经注册表单向回注（无环）。 */
let noteCollabWired = false;
export function ensureNoteCollabWiring(): void {
  if (noteCollabWired) return;
  noteCollabWired = true;
  registerCollabChannel("note-sync", (peerId, file, payload) => {
    try {
      // 解析发送方身份（历史按操作人署名用：远端合入内容署名发送端而非本端用户）；
      // peers 快照可能已更新/对端离线，查不到时缺省 null（历史回退本端署名）。
      const peer = useCollabStore.getState().peers.find((p) => p.peerId === peerId);
      receiveSyncMessage(
        file,
        base64ToBytes(payload as string),
        peer ? { id: `peer-${peerId}`, name: peer.nickname, device: peer.deviceName } : undefined,
      );
    } catch {
      console.warn("笔记协作同步消息解码失败", file);
    }
  });
  registerCollabChannel("note-aware", (_peerId, file, payload) => {
    try {
      receiveAwareness(file, base64ToBytes(payload as string));
    } catch {
      console.warn("笔记协作 awareness 解码失败", file);
    }
  });
  // 重连后对激活文档重发 syncStep1 重新握手（对端需重新拿全量状态收敛）
  registerCollabReconnect(() => resyncAllNoteDocs());
  // 拆卸：清空全部协作文档与绑定（Y.Doc/awareness 随销毁释放观察者与定时器）；
  // 广播钩子保持注入——读取 null handle 自然 no-op，重连后自动生效
  registerCollabTeardown(() => useNoteCollabStore.getState().clear());
  setNoteCollabBroadcast({
    sendSyncMessage: (file, payload) => collabSendSink("note-sync")(file, bytesToBase64(payload)),
    sendAwareness: (file, payload) => collabSendSink("note-aware")(file, bytesToBase64(payload)),
  });
}

/** 可下发给 MarkdownEditor 的协作绑定（纯数据，组件不自撞 service）。 */
export interface NoteCollabBinding {
  ytext: YText;
  awareness: Awareness;
}

export interface NoteCollabIdentity {
  name: string;
  color: string;
}

interface NoteCollabState {
  /** 当前已绑定的协作文档（file → binding）。 */
  bindings: Record<string, NoteCollabBinding>;
  /**
   * 绑定笔记协作文档：以磁盘正文 textLF 为基线（首次/无激活时重置），登记身份，返回 binding。
   * 幂等：同 file 已有激活文档时复用（多面板共享），不重复建 doc。
   */
  bind: (file: string, textLF: string, identity: NoteCollabIdentity) => NoteCollabBinding;
  /** 解绑：释放一个引用（多面板各释放一次）；协作文档仍留注册表保留远端状态。 */
  unbind: (file: string) => void;
  /** 协作态本地正文同步（源码模式编辑走 content 不经 yCollab）：写回该笔记的 ytext，防切回实时预览被陈旧 ytext 回退。 */
  syncLocalBody: (file: string, bodyLF: string) => void;
  /** 协作态落盘完成通知：驱动磁盘基线收敛（重建 doc 的挂起复位）。 */
  notifyNoteDiskWrite: (file: string) => void;
  /** 当前是否正在应用远端 Yjs update（NoteEditor 据此区分「远端合入」与「本地编辑」，历史按操作人署名）。 */
  isRemoteApplying: () => boolean;
  /** 最近一次远端合入的作者（历史按操作人署名用；无 = null）。 */
  lastRemoteAuthor: (file: string) => NoteRemoteAuthor | null;
  /** 应用退出/切仓库：清空全部协作文档上下文。 */
  clear: () => void;
}

export const useNoteCollabStore = create<NoteCollabState>((set) => ({
  bindings: {},

  bind: (file, textLF, identity) => {
    const doc = bindNoteDoc(file, textLF);
    setNoteCollabIdentity(file, identity);
    const binding: NoteCollabBinding = { ytext: doc.ytext, awareness: doc.awareness };
    set((s) => ({ bindings: { ...s.bindings, [file]: binding } }));
    return binding;
  },

  unbind: (file) => {
    unbindNoteDoc(file);
    set((s) => {
      const bindings = { ...s.bindings };
      delete bindings[file];
      return { bindings };
    });
  },

  syncLocalBody: (file, bodyLF) => {
    applyLocalBody(file, bodyLF);
  },

  notifyNoteDiskWrite: (file) => {
    markNoteDiskWrite(file);
  },

  isRemoteApplying: () => isRemoteNoteApplyActive(),

  lastRemoteAuthor: (file) => getLastRemoteAuthor(file),

  clear: () => {
    destroyAllNoteDocs();
    set({ bindings: {} });
  },
}));

// doc 被磁盘基线收敛整体重建后刷新 binding（新 ytext/awareness），触发编辑器随 collab 引用变化重绑。
setNoteCollabBindingRefresh((file, doc) => {
  useNoteCollabStore.setState((s) => ({
    bindings: { ...s.bindings, [file]: { ytext: doc.ytext, awareness: doc.awareness } },
  }));
});
