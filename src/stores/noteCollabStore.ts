/**
 * 笔记协作运行时：每笔记 `Y.Doc` 生命周期的单例编排（组件不直连 service 的桥）。
 *
 * 笔记编辑会话（协作态）经本 store 绑定/解绑协作文档，并把绑定对象（ytext + awareness）
 * 以会话状态下发给编辑面做 y-codemirror 绑定；保存仍走 noteStore（收敛后全文写盘）。
 * 本 store 只做生命周期与身份登记；网络收发经 `registerNoteCollabWiring` 注册到协作宿主
 * （collabStore 通道注册表 + 发送 sink 注入，见下方接线）。
 *
 * 多编辑面打开同一笔记共享同一 `Y.Doc`（底层 noteDoc 引用计数），防多 doc 分叉。
 */
import { create } from "zustand";
import type { NoteEditorBinding } from "@/types";
import {
  applyLocalBody,
  bindNoteDoc,
  destroyAllNoteDocs,
  destroyNoteDoc,
  disposeNoteDocsUnder,
  getLastRemoteAuthor,
  isRemoteNoteApplyActive,
  markNoteDiskWrite,
  receiveAwareness,
  receiveSyncMessage,
  resyncAllNoteDocs,
  setNoteCollabBroadcast,
  setNoteCollabBindingRefresh,
  unbindNoteDoc,
  type NoteIdentity,
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

/** 周期反熵：激活文档定时重发握手，补齐 relay 丢帧或漏收造成的分歧。 */
const ANTI_ENTROPY_MS = 10_000;

/** 笔记域协作接线（builtin.note 载荷调用，随插件启停）：
 *  注册 note-sync/note-aware 通道 handler（relay 载荷为不透明 base64，解码与作者解析归本域）、
 *  重连重握手、周期反熵、拆卸清理与广播钩子注入（经 collabSendSink 惰性读宿主 handle：断开时 no-op、
 *  重连后自动指向新连接，无需重注入）；返回撤销函数（停用/卸载时撤销注册 + 复位广播钩子 + 清空协作文档）。
 *  依赖方向：collabStore 不 import 本模块，本模块经注册表单向回注（无环）。 */
export function registerNoteCollabWiring(): () => void {
  const offs: Array<() => void> = [];
  offs.push(
    registerCollabChannel("note-sync", (peerId, file, payload) => {
      try {
        // 解析发送方身份（历史按操作人署名用：远端合入内容署名发送端而非本端用户）；
        // peers 快照可能已更新/对端离线，查不到时缺省 null（历史回退本端署名）。
        const peer = useCollabStore.getState().peers.find((p) => p.peerId === peerId);
        receiveSyncMessage(
          file,
          base64ToBytes(payload as string),
          peerId,
          peer ? { id: `peer-${peerId}`, name: peer.nickname, device: peer.deviceName } : undefined,
        );
      } catch {
        console.warn("笔记协作同步消息解码失败", file);
      }
    }),
  );
  offs.push(
    registerCollabChannel("note-aware", (_peerId, file, payload) => {
      try {
        receiveAwareness(file, base64ToBytes(payload as string));
      } catch {
        console.warn("笔记协作 awareness 解码失败", file);
      }
    }),
  );
  // 重连后对激活文档重发 syncStep1 重新握手（对端需重新拿全量状态收敛）
  offs.push(registerCollabReconnect(() => resyncAllNoteDocs()));
  // 周期反熵：relay 广播裁剪（Lagged）等造成的缺帧由定时握手兜底补齐。
  // 用宿主全局 setInterval 而非 window.*：该接线在 node 测试环境同样被挂载与拆卸
  const entropyTimer = setInterval(() => resyncAllNoteDocs(), ANTI_ENTROPY_MS);
  offs.push(() => clearInterval(entropyTimer));
  // 拆卸：清空全部协作文档与绑定（Y.Doc/awareness 随销毁释放观察者与定时器）
  offs.push(registerCollabTeardown(() => useNoteCollabStore.getState().clear()));
  setNoteCollabBroadcast({
    sendSyncMessage: (file, payload) => collabSendSink("note-sync")(file, bytesToBase64(payload)),
    sendAwareness: (file, payload) => collabSendSink("note-aware")(file, bytesToBase64(payload)),
  });
  offs.push(() => setNoteCollabBroadcast(null));
  // 撤销 = 笔记退出协作：清空协作文档（与 collab 关闭的拆卸路径幂等重叠）
  offs.push(() => useNoteCollabStore.getState().clear());
  return () => {
    for (const off of offs) off();
  };
}

interface NoteCollabState {
  /** 当前已绑定的协作文档（file → binding）。 */
  bindings: Record<string, NoteEditorBinding>;
  /**
   * 绑定笔记协作文档：以磁盘正文 textLF 为基线（有激活编辑面时复用现有文档），登记身份，返回 binding。
   * 幂等：同 file 已有激活文档时复用（多编辑面共享），不重复建 doc。
   */
  bind: (file: string, textLF: string, identity: NoteIdentity) => NoteEditorBinding;
  /** 解绑：释放一个引用（多编辑面各释放一次）；协作文档仍留内存保留远端状态。 */
  unbind: (file: string) => void;
  /** 协作态本地正文同步（源码模式/撤销重做/外部改盘等整篇入口）：按差量写回该笔记的 ytext。 */
  syncLocalBody: (file: string, bodyLF: string) => void;
  /** 协作态落盘完成通知：以落盘正文推进磁盘基线（三方合并的共同祖先）。 */
  notifyNoteDiskWrite: (file: string, bodyLF: string) => void;
  /** 当前是否正在应用远端 Yjs update（会话据此区分「远端合入」与「本地编辑」，历史按操作人署名）。 */
  isRemoteApplying: () => boolean;
  /** 最近一次远端合入的作者（历史按操作人署名用；无 = null）。 */
  lastRemoteAuthor: (file: string) => NoteRemoteAuthor | null;
  /** 文件改名/移动/删除：销毁该路径文档（路径即身份，防同名新文件串内容）。 */
  disposeDoc: (file: string) => void;
  /** 文件夹改名/移动：销毁该目录前缀下的全部文档（目录内笔记路径身份一并失效）。 */
  disposeDocsUnder: (dir: string) => void;
  /** 应用退出/切仓库：清空全部协作文档上下文。 */
  clear: () => void;
}

export const useNoteCollabStore = create<NoteCollabState>((set) => ({
  bindings: {},

  bind: (file, textLF, identity) => {
    const doc = bindNoteDoc(file, textLF, identity);
    const binding: NoteEditorBinding = { ytext: doc.ytext, awareness: doc.awareness };
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

  notifyNoteDiskWrite: (file, bodyLF) => {
    markNoteDiskWrite(file, bodyLF);
  },

  isRemoteApplying: () => isRemoteNoteApplyActive(),

  lastRemoteAuthor: (file) => getLastRemoteAuthor(file),

  disposeDoc: (file) => {
    // 先摘绑定（编辑面随即重绑到 null/新文档），再销毁文档：销毁本身延后一拍，避免销毁瞬间编辑面仍引用旧文档
    set((s) => {
      if (!(file in s.bindings)) return s;
      const bindings = { ...s.bindings };
      delete bindings[file];
      return { bindings };
    });
    destroyNoteDoc(file);
  },

  disposeDocsUnder: (dir) => {
    const prefix = `${dir}/`;
    set((s) => {
      const keys = Object.keys(s.bindings).filter((file) => file.startsWith(prefix));
      if (keys.length === 0) return s;
      const bindings = { ...s.bindings };
      for (const file of keys) delete bindings[file];
      return { bindings };
    });
    disposeNoteDocsUnder(dir);
  },

  clear: () => {
    destroyAllNoteDocs();
    set({ bindings: {} });
  },
}));

// doc 被整体重建（采纳对端基线）后刷新 binding（新 ytext/awareness），触发编辑面随 collab 引用变化重绑。
// 只刷新「当前仍有编辑面」的文件：无编辑面的保留文档重建不该凭空生出绑定（会让下次打开跳过磁盘/CRDT 对齐）。
setNoteCollabBindingRefresh((file, doc) => {
  useNoteCollabStore.setState((s) =>
    file in s.bindings
      ? { bindings: { ...s.bindings, [file]: { ytext: doc.ytext, awareness: doc.awareness } } }
      : s,
  );
});
