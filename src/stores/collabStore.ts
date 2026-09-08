/**
 * 协作 presence 运行时状态：本端与 collab-relay 的连接管理 + 同仓库在线用户列表。
 *
 * 配置（开关/地址/昵称/颜色）来自 settingsStore 应用级配置；房间按 appStore.vaultId 划分，
 * 切仓库换房（bye + 重连新 hello）；表格选中变化经 `useTableStore` 订阅节流广播
 * （100ms），peers 供表格视图渲染远端选中高亮。画布/笔记 presence 后续同通道扩展。
 */
import { create } from "zustand";
import {
  connectCollabRelay,
  testRelayConnection,
  type CollabRelayHandle,
} from "@/services/collab/relay";
import {
  receiveAwareness,
  receiveSyncMessage,
  resyncAllNoteDocs,
  setNoteCollabBroadcast,
} from "@/services/noteCollab/noteDoc";
import { base64ToBytes, bytesToBase64 } from "@/utils/base64";
import { useAppStore } from "@/stores/appStore";
import { useTableStore } from "@/stores/tableStore";
import { useCanvasStore } from "@/stores/canvasStore";
import { useNoteCollabStore } from "@/stores/noteCollabStore";
import { getAppVersion } from "@/services/app";
import type { CollabPeer, CollabPresence, RelayTestResult } from "@/types";

/** 本端 presence 广播节流（选中高频变化合并，不刷屏 relay）。 */
const BROADCAST_THROTTLE_MS = 100;

export interface CollabInitConfig {
  enabled: boolean;
  url: string;
  nickname: string;
  color: string;
  deviceName: string;
}

interface CollabStoreState {
  /** 是否已连接 relay（连接中/断线重连 = false）。 */
  connected: boolean;
  /** 当前房间（同仓库 vaultId）在线用户列表。 */
  peers: CollabPeer[];
  /** 本连接在房间内的 peerId（hello-ack 分配；锁主判定/过滤自己用，未连接 = null）。 */
  myPeerId: number | null;

  /** 应用启动时调用：载入配置并建立连接（无配置 = 不连）。 */
  init: (cfg: CollabInitConfig) => void;
  /** 设置变更（开关/地址/昵称/颜色）：重建连接。 */
  applyConfig: (patch: Partial<Omit<CollabInitConfig, "deviceName">>) => void;
  /** 检查中转连通性（设置页「检查连接」）：按输入地址一次性探测 relay，不影响常驻连接。 */
  testConnection: (rawUrl: string) => Promise<RelayTestResult>;
  /**
   * 上报当前打开的笔记（协作 presence，view=note）：对端据此在笔记头显示「正在编辑」协作者列表。
   * 传 null = 离开笔记（清远端高亮），与表格 presence 共用节流通道（后上报者生效）。
   */
  notePresence: (file: string | null) => void;
  /** 断开连接并停止广播（应用退出）。 */
  dispose: () => void;
}

/** 当前运行时配置（init/applyConfig 更新；连接按它建立）。 */
let runtimeCfg: CollabInitConfig | null = null;
let handle: CollabRelayHandle | null = null;
let currentVaultId: string | null = null;
/** 本连接在房间内的 peerId（hello-ack 分配；据此把自己过滤出 peers，防自己出现在在线列表/高亮）。 */
let myPeerId: number | null = null;
/** 节流广播暂存（节流窗口内的最新 presence）。 */
let pendingPresence: CollabPresence | null = null;
let broadcastTimer: number | null = null;
/** 表格/appStore 订阅只注册一次（init 时，确保各 store 模块已完成初始化——防循环 import 未完成期调用）。 */
let subscribed = false;
/** 应用版本号（运行期不变）：随 hello 上报协作房间展示各成员版本；首次读取后缓存，读取失败降级 undefined。 */
let appVersionPromise: Promise<string | undefined> | null = null;
function appVersionOnce(): Promise<string | undefined> {
  appVersionPromise ??= getAppVersion().catch(() => undefined);
  return appVersionPromise;
}
// 模块加载即预热：首连 await 版本号直接拿已解析值，消除建连被 IPC 阻塞的窗口（失败静默降级 undefined）
void appVersionOnce();
/** 连接建立序号：快速连续 applyConfig/切仓库时，await 版本号期间可能交错两次建立请求，
 *  后一次须作废前一次（否则旧连接泄漏无人管理）。 */
let connSeq = 0;

/** 随机分配身份色（未配置时；与强调色体系一致的暖色系，避免刺眼）。设置页「随机」按钮复用。 */
export function randomPeerColor(): string {
  const palette = ["#e06c75", "#61afef", "#98c379", "#e5c07b", "#c678dd", "#56b6c2", "#d19a66"];
  return palette[Math.floor(Math.random() * palette.length)];
}

/** 中转地址规范化：补协议（ws://）与 /ws 路径（relay 唯一路由），
 *  如 `192.168.1.10:17701` → `ws://192.168.1.10:17701/ws`；空/无法解析的输入原样返回。 */
export function normalizeRelayUrl(raw: string): string {
  const input = raw.trim();
  if (!input) return "";
  const withProto = /^wss?:\/\//i.test(input) ? input : `ws://${input}`;
  try {
    const u = new URL(withProto);
    return `${u.protocol}//${u.host}/ws`;
  } catch {
    return withProto;
  }
}

async function establishConnection(): Promise<void> {
  // 先发 bye 再断开（切仓库换房）：relay 收到 bye 立即踢出，否则旧 peer 要等 30s 心跳
  // 超时才消失，期间对端列表可见幽灵用户（dispose 路径同样先 bye，见 dispose）
  handle?.sendBye();
  handle?.disconnect();
  handle = null;
  // 断线/重连期间清空在线列表与身份（残留旧 peers 会误导远端高亮）
  myPeerId = null;
  // 丢弃节流窗口内未发出的陈旧 presence（切仓库后旧文件的选中不得发进新房间）
  pendingPresence = null;
  useCollabStore.setState({ connected: false, peers: [] });
  // 序号须先于早退判断递增：await 版本号期间若有禁用协作/地址清空/回启动页等早退调用，
  // 也必须作废在途请求——否则旧请求恢复后仍用已失效配置建连（幽灵连接 / 发出 vaultId:null）
  const seq = ++connSeq;
  const cfg = runtimeCfg;
  if (!cfg?.enabled || !cfg.url || !currentVaultId) return;
  // 应用版本随 hello 上报（协作房间展示各成员版本）；版本运行期不变，仅首次真实读取，失败降级省略
  const version = await appVersionOnce();
  if (seq !== connSeq) return; // 期间有更新的连接请求（applyConfig/切仓库/早退），放弃本次
  handle = connectCollabRelay({
    url: cfg.url,
    hello: {
      vaultId: currentVaultId,
      nickname: cfg.nickname || cfg.deviceName || "用户",
      color: cfg.color || randomPeerColor(),
      deviceName: cfg.deviceName,
      version,
    },
    onHelloAck: (peerId) => {
      myPeerId = peerId;
      // hello-ack 先于 peers 帧到达（relay 端保证）：立即过滤已收快照里的自己 + 暴露本端 peerId
      useCollabStore.setState((s) => ({
        myPeerId: peerId,
        peers: s.peers.filter((p) => p.peerId !== peerId),
      }));
    },
    onPeers: (peers) =>
      useCollabStore.setState({ peers: peers.filter((p) => p.peerId !== myPeerId) }),
    onPeerPresence: (peerId, presence) => {
      if (peerId === myPeerId) return;
      useCollabStore.setState((s) => ({
        peers: s.peers.map((p) => (p.peerId === peerId ? { ...p, presence } : p)),
      }));
    },
    // 表格内容补丁实时广播接收：只应用当前打开的表格（applyRemotePatch 内部按 file + 表 id 守卫）
    onTablePatch: (peerId, file, patch) => {
      if (peerId === myPeerId) return;
      useTableStore.getState().applyRemotePatch(file, patch);
    },
    // 画布内容补丁实时广播接收：只应用当前打开的画布（applyRemoteCanvasPatch 内部按 file + id 守卫）
    onCanvasPatch: (peerId, file, patch) => {
      if (peerId === myPeerId) return;
      useCanvasStore.getState().applyRemoteCanvasPatch(file, patch);
    },
    // 笔记 Yjs 同步 / awareness 接收：解码后只合入本端已打开（注册表存在）的笔记（noteDoc 内部守卫）
    onNoteSync: (peerId, file, payload) => {
      try {
        // 解析发送方身份（历史按操作人署名用：远端合入内容署名发送端而非本端用户）。
        // peers 快照可能已更新/对端离线，查不到时缺省 null（历史回退本端署名）。
        const peer = useCollabStore.getState().peers.find((p) => p.peerId === peerId);
        receiveSyncMessage(
          file,
          base64ToBytes(payload),
          peer ? { id: `peer-${peerId}`, name: peer.nickname, device: peer.deviceName } : undefined,
        );
      } catch {
        console.warn("笔记协作同步消息解码失败", file);
      }
    },
    onNoteAware: (_peerId, file, payload) => {
      try {
        receiveAwareness(file, base64ToBytes(payload));
      } catch {
        console.warn("笔记协作 awareness 解码失败", file);
      }
    },
    // 服务端 error 帧（协议异常/房间拒绝）：协作是尽力而为的辅助能力，仅记录不打断使用
    onServerError: (message) => console.warn("协作中转错误：", message),
    onStatusChange: (connected) => {
      useCollabStore.setState({ connected });
      // 连接建立后补发一次当前 presence：重连/进房间时本端选中立即可见，
      // 否则要等用户下一次选中变化才广播（hello 已先发，同 TCP FIFO 保证先入房）
      if (connected) {
        const ts = useTableStore.getState();
        schedulePresenceBroadcast({ file: ts.tableFile, selection: ts.selection, view: ts.view });
        // 有画布打开时补发画布 presence（覆盖 table 槽——画布为主工作区；锁/流式经
        // schedulePresenceBroadcast 跨视图合并，重连后立即恢复对端只见的编辑锁）
        const cs = useCanvasStore.getState();
        if (cs.canvasFile) {
          schedulePresenceBroadcast({
            file: cs.canvasFile,
            selection: cs.selectedNodeId ? { kind: "node", nodeId: cs.selectedNodeId } : null,
            view: "canvas",
          });
        }
        // 重连后重新握手已打开的协作文档（上次连接断开的对端需重新拿全量状态）
        resyncAllNoteDocs();
      }
    },
  });
}

function schedulePresenceBroadcast(presence: CollabPresence): void {
  // 画布锁/流式跨视图保活：无论当前 view 槽（table/note/canvas）为何，都合并 canvas 的
  // 独占编辑锁与生成中节点——用户在看表格/笔记期间其画布锁仍对端可见，对话节点持续只读
  const cs = useCanvasStore.getState();
  const lockedNodes = Object.entries(cs.lockedConversations).map(([id, since]) => ({ id, since }));
  const streamingNodeIds = Object.entries(cs.streamingByConv)
    .filter(([, v]) => v)
    .map(([id]) => id);
  // 打开文件清单（跨视图保活：画布/笔记/表格可同时打开，聚焦文件置顶，供「协作房间」面板展示）
  const as = useAppStore.getState();
  const openFiles: CollabPresence["openFiles"] = [];
  const focusView: "canvas" | "note" | "table" =
    presence.view === "canvas" ? "canvas" : presence.view === "note" ? "note" : "table";
  if (presence.file) openFiles.push({ file: presence.file, view: focusView });
  const others: Array<[string | null, "canvas" | "note" | "table"]> = [
    [as.currentCanvasFile, "canvas"],
    [as.currentNoteFile, "note"],
    [as.currentTableFile, "table"],
  ];
  for (const [file, view] of others) {
    if (file && !openFiles.some((o) => o.file === file)) openFiles.push({ file, view });
  }
  const merged: CollabPresence = {
    ...presence,
    ...(openFiles.length ? { openFiles } : {}),
    ...(lockedNodes.length ? { lockedNodes } : {}),
    ...(streamingNodeIds.length ? { streamingNodeIds } : {}),
  };
  pendingPresence = merged;
  if (broadcastTimer !== null) return;
  broadcastTimer = window.setTimeout(() => {
    broadcastTimer = null;
    if (handle && pendingPresence) handle.sendPresence(pendingPresence);
    pendingPresence = null;
  }, BROADCAST_THROTTLE_MS);
}

/** 插件经协作能力上报本端 presence（view/file；selection 无——插件不做选中联动）。
 *  内部合并当前画布锁/流式与打开文件清单，保持 presence 载荷完整（同表/画布订阅同通道）。 */
export function publishPluginPresence(view: string | null, file: string | null): void {
  schedulePresenceBroadcast({ file, selection: null, view });
}

// 表格打开/选中/视图变化 → 节流广播 presence（file null = 未看表格，清空远端高亮）；
// 切仓库（vaultId 变化）→ 换房间重连；无仓库（回启动页）→ 断开。
// 注册推迟到 init（防循环 import 链中模块未完成初始化即调用 store）
function ensureSubscriptions(): void {
  if (subscribed) return;
  subscribed = true;
  useTableStore.subscribe((s, prev) => {
    if (s.tableFile !== prev.tableFile || s.selection !== prev.selection || s.view !== prev.view) {
      schedulePresenceBroadcast({ file: s.tableFile, selection: s.selection, view: s.view });
    }
  });
  // 画布 presence：打开/切画布（canvasFile）、选中节点、独占编辑锁、流式起止任一变化 → 广播
  // view=canvas（锁/流式经 schedulePresenceBroadcast 跨视图合并，此订阅只负责 view 槽与选中）。
  // messagesByConv 逐 token 更新不在此订阅 → 流式 token 不刷屏 presence（只有流式起止变更）。
  useCanvasStore.subscribe((s, prev) => {
    const changed =
      s.canvasFile !== prev.canvasFile ||
      s.selectedNodeId !== prev.selectedNodeId ||
      s.lockedConversations !== prev.lockedConversations ||
      s.streamingByConv !== prev.streamingByConv;
    if (!changed) return;
    schedulePresenceBroadcast({
      file: s.canvasFile,
      selection: s.selectedNodeId ? { kind: "node", nodeId: s.selectedNodeId } : null,
      view: "canvas",
    });
  });
  useAppStore.subscribe((s, prev) => {
    if (s.vaultId !== prev.vaultId) {
      currentVaultId = s.vaultId;
      void establishConnection();
    }
  });
}

export const useCollabStore = create<CollabStoreState>((set) => ({
  connected: false,
  peers: [],
  myPeerId: null,

  init: (cfg) => {
    ensureSubscriptions();
    runtimeCfg = { ...cfg, color: cfg.color || randomPeerColor() };
    currentVaultId = useAppStore.getState().vaultId;
    // 注入表格补丁广播钩子（tableStore 在 schedulePersist 计算补丁后回调；handle 为模块级，
    // establishConnection 重建连接后闭包自动指向新连接，无需重注入）
    useTableStore.getState().setCollabBroadcast((file, patch) => handle?.sendTablePatch(file, patch));
    // 注入画布补丁广播钩子（canvasStore 在 schedulePersist 计算补丁后回调，同表格）
    useCanvasStore.getState().setCollabBroadcast((file, patch) => handle?.sendCanvasPatch(file, patch));
    // 注入笔记协作广播钩子（Yjs 二进制约经 base64 走 relay 同通道）
    setNoteCollabBroadcast({
      sendSyncMessage: (file, payload) => handle?.sendNoteSync(file, bytesToBase64(payload)),
      sendAwareness: (file, payload) => handle?.sendNoteAware(file, bytesToBase64(payload)),
    });
    void establishConnection();
  },

  applyConfig: (patch) => {
    if (!runtimeCfg) return;
    runtimeCfg = {
      ...runtimeCfg,
      ...patch,
      // 设置页未配置颜色（空串）时保留已分配的随机色，防每次设置变更/重连都换身份色
      color: patch.color || runtimeCfg.color,
    };
    void establishConnection();
  },

  testConnection: async (rawUrl) => {
    const url = normalizeRelayUrl(rawUrl);
    if (!url) return { ok: false, message: "请先填写中转地址" };
    return testRelayConnection(url);
  },

  notePresence: (file) =>
    schedulePresenceBroadcast({ file, selection: null, view: file ? "note" : null }),

  dispose: () => {
    handle?.sendBye();
    handle?.disconnect();
    handle = null;
    // 作废等待中的建立请求（await 版本号期间 dispose 可能已执行，防幽灵重连）
    connSeq++;
    // 解除广播钩子：协作关闭后画布/表格编辑不再走 relay（回落 watcher/磁盘通道）
    useTableStore.getState().setCollabBroadcast(null);
    useCanvasStore.getState().setCollabBroadcast(null);
    // 释放本端画布独占编辑锁（协作关闭后锁声明不再对端可见，内存清空防陈旧）
    useCanvasStore.getState().clearConversationLocks();
    setNoteCollabBroadcast(null);
    // 释放全部协作文档（Y.Doc/awareness 随销毁释放观察者与定时器）
    useNoteCollabStore.getState().clear();
    runtimeCfg = null;
    myPeerId = null;
    if (broadcastTimer !== null) {
      clearTimeout(broadcastTimer);
      broadcastTimer = null;
    }
    pendingPresence = null;
    set({ connected: false, peers: [], myPeerId: null });
  },
}));
