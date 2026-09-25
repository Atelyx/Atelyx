/**
 * 协作运行时（DocHost 的 store 门面）：连接策略 + 当前空间在线设备列表。
 *
 * 协作只存在于协作空间（服务端真源）：配置（开关/昵称/颜色）来自 settingsStore 应用级配置，
 * 连接目标按激活仓库身份解析（utils/collabHost 的 resolveCollabTarget）——协作空间身份按
 * spaceId 入房、令牌随连接触取；个人仓库/未进仓不连接。切换仓库身份断开旧连接（bye + 重连）。
 * 传输/入站路由/出站咽喉归内核 DocHost（services/collab/docHost.ts）：本 store 只做
 * 「何时连/断/换房」策略与 presence 节流调度、peers/myPeerId 状态镜像；画布/笔记/表格域的协作
 * 接线（消息通道/重连/拆卸/presence 合并/广播注入）随插件启停经各域 `register*CollabWiring`
 * 注册到 collabHost 注册表。本 store 不 import 任何域 store。
 */
import { create } from "zustand";
import {
  connectTransport,
  disconnectTransport,
  sendTransportMessage,
  sendTransportPresence,
  type CollabChannel,
} from "@/services/collab/docHost";
import { useAppStore } from "@/stores/appStore";
import { getAppVersion } from "@/services/app";
import { getToken } from "@/services/space/auth";
// 空间传输工厂随本模块加载注册进传输注册表（连接按身份选中 space 工厂）
import { spaceWsUrl } from "@/services/collab/spaceTransport";
import {
  mergeCollabPresence,
  resolveCollabTarget,
  runCollabReconnects,
  runCollabTeardowns,
  type CollabTarget,
} from "@/utils/collabHost";
import { getPluginNotificationAccess } from "@/services/cordis/access";
import type { CollabMyPeer, CollabPeer, CollabPresence } from "@/types";

/** 本端 presence 广播节流（选中高频变化合并，不刷屏传输层）。 */
const BROADCAST_THROTTLE_MS = 100;
/** 传输侧缺帧提示（resync）合并窗口：一波缺帧只做一次全量重握手，其余由周期反熵兜底。 */
const RESYNC_COALESCE_MS = 3_000;

export interface CollabInitConfig {
  enabled: boolean;
  nickname: string;
  color: string;
  deviceName: string;
}

interface CollabStoreState {
  /** 是否已连接协作传输（连接中/断线重连 = false）。 */
  connected: boolean;
  /** 当前空间在线设备列表。 */
  peers: CollabPeer[];
  /** 本连接在房间内的 peerId（hello-ack 分配；锁主判定/过滤自己用，未连接 = null）。 */
  myPeerId: number | null;
  /** 活跃的插件协作声明数（插件经 ctx.collab.acquire 声明；> 0 时宿主为本窗口维持协作连接）。 */
  pluginDemand: number;

  /** 应用启动时调用：载入配置并建立连接（无配置 = 不连）。 */
  init: (cfg: CollabInitConfig) => void;
  /** 设置变更（开关/昵称/颜色）：重建连接。 */
  applyConfig: (patch: Partial<Omit<CollabInitConfig, "deviceName">>) => void;
  /**
   * 插件声明需要协作通道：计数 +1 并返回释放函数（幂等，重复调用为 no-op）。
   * 计数变化由 panelStore 订阅，重评估本窗口是否维持协作连接；插件停用/卸载随 fiber 撤销释放。
   */
  retainPluginDemand(): () => void;
  /** 上报当前打开的笔记（协作 presence，view=note）：对端据此在笔记头显示「正在编辑」协作者列表。
   *  传 null = 离开笔记（清远端高亮），与表格 presence 共用节流通道（后上报者生效）。 */
  notePresence: (file: string | null) => void;
  /** 断开连接并停止广播（应用退出）。 */
  dispose: () => void;
}

/** 当前运行时配置（init/applyConfig 更新；连接按它建立）。 */
let runtimeCfg: CollabInitConfig | null = null;
/** 本连接在房间内的 peerId（hello-ack 分配；据此把自己过滤出 peers，防自己出现在在线列表/高亮）。 */
let myPeerId: number | null = null;
/** 节流广播暂存（节流窗口内的最新 presence）。 */
let pendingPresence: CollabPresence | null = null;
let broadcastTimer: number | null = null;
/** 最近一次按服务端缺帧提示做全量重握手的时刻（合并窗口内忽略后续提示）。 */
let lastResyncAt = 0;
/** appStore 订阅只注册一次（init 时，确保各 store 模块已完成初始化——防循环 import 未完成期调用）。 */
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
/** 活跃的插件协作声明数（retainPluginDemand 维护；与连接状态无关，dispose 不清）。 */
let pluginDemandCount = 0;

/** 服务端 error 帧的用户可见提示合并窗口：鉴权拒绝等错误会随每次重连重放，同因提示不刷屏。 */
const SERVER_ERROR_NOTIFY_COALESCE_MS = 60_000;
let lastServerErrorAt = 0;
let lastServerErrorMessage = "";

/** 按当前身份与配置解析连接目标（建连与重连刷新共用）：space = space 工厂按 spaceId 入房，
 *  每次解析触取登录令牌——身份/令牌/配置变化自动生效；local/无身份/协作关闭 = 不连接（null）。 */
async function resolveCurrentTarget(): Promise<CollabTarget | null> {
  const cfg = runtimeCfg;
  if (!cfg) return null;
  // 应用版本随 hello 上报（协作房间展示各成员版本）；版本运行期不变，仅首次真实读取，失败降级省略
  const version = await appVersionOnce();
  return resolveCollabTarget({
    identity: useAppStore.getState().vaultIdentity ?? null,
    collab: {
      enabled: cfg.enabled,
      nickname: cfg.nickname || cfg.deviceName || "用户",
      color: cfg.color || randomPeerColor(),
      deviceName: cfg.deviceName,
      version,
    },
    getToken,
    spaceWsUrl,
  });
}

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

/** 插件通用消息发送（`plugin-msg` 通道；传输层 file 槽承载插件频道名）。返回是否已投递到传输层
 *  （未连接/断开 = false，调用方据此感知消息未发出——协作是尽力而为，不静默）。to 指定 = 定向单播只发该 peer。 */
export function sendPluginMessage(channel: string, payload: unknown, to?: number): boolean {
  return sendTransportMessage("plugin-msg", channel, payload, to);
}

/** 本端身份（peerId 未连接 = null；昵称/颜色/设备名取当前运行时配置，未 init 为空身份）。
 *  与 peers() 对称，供插件展示「我是谁」/作远程命令的本机标识。 */
export function getMyPeerInfo(): CollabMyPeer {
  return {
    peerId: myPeerId,
    nickname: runtimeCfg?.nickname || runtimeCfg?.deviceName || "用户",
    color: runtimeCfg?.color ?? "",
    deviceName: runtimeCfg?.deviceName ?? "",
  };
}

/** 随机分配身份色（未配置时；与强调色体系一致的暖色系，避免刺眼）。设置页「随机」按钮复用。 */
export function randomPeerColor(): string {
  const palette = ["#e06c75", "#61afef", "#98c379", "#e5c07b", "#c678dd", "#56b6c2", "#d19a66"];
  return palette[Math.floor(Math.random() * palette.length)];
}

async function establishConnection(): Promise<void> {
  // 先发 bye 再断开（切换身份换房）：服务端收到 bye 立即踢出，否则旧 peer 要等 30s 心跳
  // 超时才消失，期间对端列表可见幽灵用户（dispose 路径同样先 bye，见 dispose）
  disconnectTransport();
  // 换连接即重新计时：上一个连接刚接受过 resync 不应吞掉新连接的首个 resync
  lastResyncAt = 0;
  // 断线/重连期间清空在线列表与身份（残留旧 peers 会误导远端高亮）
  myPeerId = null;
  // 丢弃节流窗口内未发出的陈旧 presence（切仓库后旧文件的选中不得发进新房间）
  pendingPresence = null;
  // 最近一次上报基底同理失效：换房后 republishPresence 不得拿旧仓库的聚焦文件成帧
  lastPresenceBase = null;
  useCollabStore.setState({ connected: false, peers: [] });
  // 序号须先于早退判断递增：await 版本号/令牌期间若有禁用协作/切换身份等早退调用，
  // 也必须作废在途请求——否则旧请求恢复后仍用已失效配置建连（幽灵连接 / 发出空房间号）
  const seq = ++connSeq;
  if (!runtimeCfg) return;
  const target = await resolveCurrentTarget();
  // await 版本号/令牌期间有更新的连接请求（applyConfig/切身份/早退）则放弃本次——
  // 不复查会拿已失效的配置建连（幽灵连接/发出空房间号）
  if (seq !== connSeq) return;
  if (!target) return;
  try {
    connectTransport({
      name: target.transport,
      url: target.url,
      hello: target.hello,
      // 断线重连前重解析连接目标：身份/令牌/配置已变自动生效；不可再连（null）= 放弃重连
      refreshHello: async () => {
        const fresh = await resolveCurrentTarget();
        return fresh?.hello ?? null;
      },
      onHelloAck: (peerId) => {
        myPeerId = peerId;
        // hello-ack 先于 peers 帧到达（服务端保证）：立即过滤已收快照里的自己 + 暴露本端 peerId
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
      // 服务端 error 帧（鉴权拒绝/协议异常）：协作是尽力而为的辅助能力，不打断使用，
      // 但必须用户可见（同因错误随重连重放，合并窗口内不刷屏）
      onServerError: (message) => {
        console.warn("协作服务错误：", message);
        const now = Date.now();
        if (
          message === lastServerErrorMessage &&
          now - lastServerErrorAt < SERVER_ERROR_NOTIFY_COALESCE_MS
        ) {
          return;
        }
        lastServerErrorAt = now;
        lastServerErrorMessage = message;
        getPluginNotificationAccess()?.notify({
          level: "error",
          message: `协作连接被服务器拒绝：${message}`,
        });
      },
      // 本连接接收队列被广播裁剪（消费过慢）→ 帧已丢：与重连同款重新握手补齐；
      // 服务端按最小间隔下发，本端再合并一波，防「重握手大帧 → 更慢 → 再下发」自激
      onResync: () => {
        const now = Date.now();
        if (now - lastResyncAt < RESYNC_COALESCE_MS) return;
        lastResyncAt = now;
        runCollabReconnects();
      },
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

/** 最近一次上报的 presence 基底（不含 openFiles 等派生字段）：域内状态变化时据此重发。 */
let lastPresenceBase: CollabPresence | null = null;

function schedulePresenceBroadcast(presence: CollabPresence): void {
  lastPresenceBase = presence;
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

/** 重发最近一次 presence（域内状态变化但聚焦文件/选中未变时用，如笔记编辑面开关）。
 *  未上报过（未连接/未进仓）为 no-op。 */
export function republishPresence(): void {
  if (lastPresenceBase) schedulePresenceBroadcast(lastPresenceBase);
}

/** 域 presence 上报（画布/表格域经注册表接线自用；内部合并打开文件清单与各 provider）。 */
export function publishCollabPresence(base: CollabPresence): void {
  schedulePresenceBroadcast(base);
}

// 切换仓库身份（进仓/回到无仓库/进入或切换协作空间）→ 重建或断开连接；
// 注册推迟到 init（防循环 import 链中模块未完成初始化即调用 store）
function ensureSubscriptions(): void {
  if (subscribed) return;
  subscribed = true;
  // 身份对象变化即重建连接（establishConnection 内部先 bye 再断开；
  // 解析结果为 null 时等价于断开——个人仓库/未进仓无协作）
  useAppStore.subscribe((s, prev) => {
    if (s.vaultIdentity !== prev.vaultIdentity) void establishConnection();
  });
}

export const useCollabStore = create<CollabStoreState>((set) => ({
  connected: false,
  peers: [],
  myPeerId: null,
  pluginDemand: 0,

  retainPluginDemand: () => {
    pluginDemandCount += 1;
    set({ pluginDemand: pluginDemandCount });
    let released = false;
    return () => {
      if (released) return;
      released = true;
      // Math.max 兜底：释放函数逃逸出插件生命周期被多余调用时不把计数打成负数
      pluginDemandCount = Math.max(0, pluginDemandCount - 1);
      set({ pluginDemand: pluginDemandCount });
    };
  },

  init: (cfg) => {
    ensureSubscriptions();
    runtimeCfg = { ...cfg, color: cfg.color || randomPeerColor() };
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
    lastPresenceBase = null;
    set({ connected: false, peers: [], myPeerId: null });
  },
}));
