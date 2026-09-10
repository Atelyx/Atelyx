/**
 * 协作运行时（DocHost 的 store 门面）：连接策略 + 同仓库在线用户列表。
 *
 * 配置（开关/地址/昵称/颜色）来自 settingsStore 应用级配置；房间按 appStore.vaultId 划分，
 * 切仓库换房（bye + 重连新 hello）。传输/入站路由/出站咽喉归内核 DocHost
 * （services/collab/docHost.ts）：本 store 只做「何时连/断/换房」策略与 presence 节流调度、
 * peers/myPeerId 状态镜像；画布/笔记/表格域的协作接线（消息通道/重连/拆卸/presence 合并/
 * 广播注入）随插件启停经各域 `register*CollabWiring` 注册到 collabHost 注册表。本 store 不 import 任何域 store。
 */
import { create } from "zustand";
import {
  connectTransport,
  disconnectTransport,
  sendTransportMessage,
  sendTransportPresence,
  testTransport,
  type CollabChannel,
} from "@/services/collab/docHost";
import { useAppStore } from "@/stores/appStore";
import { getAppVersion } from "@/services/app";
import {
  mergeCollabPresence,
  runCollabReconnects,
  runCollabTeardowns,
} from "@/utils/collabHost";
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

// ===== 域接线注册表（画布/笔记/表格域经此自注册路由/重连/拆卸/presence 合并；宿主保持域无关） =====
// 注册表本体在 utils/collabHost（纯数据容器可直测）；本模块 re-export 注册 API 供域接线调用，
// 只做查表分发与合并，不 import 任何域 store/service（传输层不透明透传原始载荷）。
export {
  registerCollabChannel,
  registerCollabPresenceProvider,
  registerCollabReconnect,
  registerCollabTeardown,
} from "@/utils/collabHost";

/** 可透传的发送通道（单源：继承自 DocHost/传输层 CollabChannel）。 */
export type CollabSendChannel = CollabChannel;

/** 域发送 sink（经 DocHost 出站咽喉：断开时静默丢弃；重连后自动指向新连接）。 */
export function collabSendSink(
  channel: CollabSendChannel,
): (file: string, payload: unknown) => void {
  return (file, payload) => sendTransportMessage(channel, file, payload);
}

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
  disconnectTransport();
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
  try {
    connectTransport({
      name: "relay",
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
    // 服务端 error 帧（协议异常/房间拒绝）：协作是尽力而为的辅助能力，仅记录不打断使用
    onServerError: (message) => console.warn("协作中转错误：", message),
    onStatusChange: (connected) => {
      useCollabStore.setState({ connected });
      // 连接建立后补发一次当前 presence：重连/进房间时本端选中立即可见，
      // 否则要等用户下一次选中变化才广播（hello 已先发，同 TCP FIFO 保证先入房）
      if (connected) {
        // 各域重连回调（表格/画布 presence 补发、笔记重新握手等；域经 registerCollabReconnect
        // 自注册，接线顺序保证画布打开时画布 presence 覆盖表格槽）
        runCollabReconnects();
      }
    },
  });
  } catch (e) {
    // 传输未注册/建连失败：协作是尽力而为的辅助能力，记录后保持未连接（后续 applyConfig/切仓库重试）
    console.warn("协作连接建立失败：", e instanceof Error ? e.message : String(e));
  }
}

function schedulePresenceBroadcast(presence: CollabPresence): void {
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
  };
  // 域 presence provider 依次合并（画布锁/流式等跨视图保活经注册表接入，宿主域无关）
  pendingPresence = mergeCollabPresence(merged);
  if (broadcastTimer !== null) return;
  broadcastTimer = window.setTimeout(() => {
    broadcastTimer = null;
    if (pendingPresence) sendTransportPresence(pendingPresence);
    pendingPresence = null;
  }, BROADCAST_THROTTLE_MS);
}

/** 插件经协作能力上报本端 presence（view/file；selection 无——插件不做选中联动）。
 *  内部合并当前画布锁/流式与打开文件清单，保持 presence 载荷完整（同表/画布订阅同通道）。 */
export function publishPluginPresence(view: string | null, file: string | null): void {
  schedulePresenceBroadcast({ file, selection: null, view });
}

/** 域 presence 上报（画布/表格域经注册表接线自用；内部合并打开文件清单与各 provider）。 */
export function publishCollabPresence(base: CollabPresence): void {
  schedulePresenceBroadcast(base);
}

// 切仓库（vaultId 变化）→ 换房间重连；无仓库（回启动页）→ 断开。
// 注册推迟到 init（防循环 import 链中模块未完成初始化即调用 store）
function ensureSubscriptions(): void {
  if (subscribed) return;
  subscribed = true;
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
    // 各域广播钩子由域接线注入（ensure*CollabWiring，经 collabSendSink → DocHost 出站咽喉，
    // 重连后自动指向新连接，无需重注入）
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
    return testTransport("relay", url);
  },

  notePresence: (file) =>
    schedulePresenceBroadcast({ file, selection: null, view: file ? "note" : null }),

  dispose: () => {
    disconnectTransport();
    // 作废等待中的建立请求（await 版本号期间 dispose 可能已执行，防幽灵重连）
    connSeq++;
    // 域拆卸钩子（表格/画布/笔记的广播钩子与文档清理经注册表；出站咽喉断开后自然 no-op）
    runCollabTeardowns();
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
