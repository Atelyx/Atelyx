/**
 * 协作文档宿主 DocHost（内核，域无关）：协作传输的活动句柄与入站路由。
 *
 * - 组合传输注册表（transport.ts，空间工厂由 spaceTransport.ts 注册）与域接线注册表（utils/collabHost）：
 *   活动传输句柄由本模块持有（collabStore 是 store 门面，经 connectTransport/send* 管理）；
 * - 入站统一路由到 collabHost 注册表：peer 频道消息走通道注册表，服务端单向 meta-changed 帧
 *   走团队 meta 变更注册表（各域 handler 自注册）；
 * - 出站经 send* 咽喉（断开时静默丢弃）；
 * - 插件帧序号（可靠补投的对账基准）由本模块跨连接维护：连接替换/断开前从旧句柄捕获，
 *   换房（hello.spaceId 变化）序号空间失效即重置；
 * - 传输侧报告接收队列过慢（帧被裁剪）时回调 `onResync`，由调用方按域补齐
 *   （笔记域重新握手索取全量状态；补丁域只补发 presence，陈旧补丁由后续补丁与下次保存的
 *   合并落盘收敛）。
 *
 * 文档实例的生命周期（引用计数/创建/重建/销毁）由各领域服务自持（如 noteDoc 的 per-file Y.Doc）。
 */
import { dispatchCollabChannel, dispatchCollabMetaChanged } from "@/utils/collabHost";
import type { CollabPresence } from "@/types";
import {
  connectCollabTransport,
  type CollabChannel,
  type CollabTransportHandle,
  type CollabTransportOptions,
} from "./transport";

export type { CollabChannel } from "./transport";

/** 活动传输句柄（重连时替换；断开 = null）。 */
let activeTransport: CollabTransportHandle | null = null;

/** 房间级插件帧序号（插件可靠补投的对账基准，跨连接延续）：连接替换前从旧句柄捕获；
 *  换房（hello 的 spaceId 变化）序号空间失效即重置。 */
let pluginLastSeq: number | null = null;
/** 上一次连接的房间（hello.spaceId）：判断序号空间是否延续。 */
let lastSpaceId: string | undefined;

/** 从活动句柄捕获插件帧序号（旧句柄失效前取回对账基准；取不到保持原值）。 */
function capturePluginSeq(): void {
  const seq = activeTransport?.pluginSeq() ?? null;
  if (seq !== null && (pluginLastSeq === null || seq > pluginLastSeq)) pluginLastSeq = seq;
}

/** 建连请求（传输名 + 连接参数；入站路由由 DocHost 内部接 collabHost 注册表，调用方不必提供）。 */
export interface ConnectTransportRequest
  extends Omit<CollabTransportOptions, "onChannelMessage" | "onMetaChanged"> {
  name: string;
}

/** 建立连接（先 bye + 断开旧连接，再按名连新传输；未注册传输抛错）。
 *  建连失败时活动句柄置空（不残留已断开的旧句柄），由调用方记录降级。 */
export function connectTransport(req: ConnectTransportRequest): void {
  capturePluginSeq();
  if (req.hello.spaceId !== lastSpaceId) {
    pluginLastSeq = null;
    lastSpaceId = req.hello.spaceId;
  }
  activeTransport?.sendBye();
  activeTransport?.disconnect();
  activeTransport = null;
  activeTransport = connectCollabTransport(req.name, {
    url: req.url,
    hello: req.hello,
    refreshHello: req.refreshHello,
    pluginLastSeq,
    onHelloAck: req.onHelloAck,
    onPeers: req.onPeers,
    onPeerPresence: req.onPeerPresence,
    onChannelMessage: (peerId, channel, file, payload) =>
      dispatchCollabChannel(channel, peerId, file, payload),
    onMetaChanged: dispatchCollabMetaChanged,
    onResync: req.onResync,
    onServerError: req.onServerError,
    onStatusChange: req.onStatusChange,
  });
}

/** 断开并清空活动传输（不再重连）。 */
export function disconnectTransport(): void {
  capturePluginSeq();
  activeTransport?.sendBye();
  activeTransport?.disconnect();
  activeTransport = null;
}

/** 出站咽喉：按频道透传（断开时静默丢弃）。返回是否已投递到传输层（调用方据此感知未连接）。
 *  `plugin-msg` 的 file 槽 = 插件频道名，targetPeerId 有值 = 定向单播（其余频道忽略）。 */
export function sendTransportMessage(
  channel: CollabChannel,
  file: string,
  payload: unknown,
  targetPeerId?: number,
): boolean {
  return activeTransport?.sendMessage(channel, file, payload, targetPeerId) ?? false;
}

/** 出站 presence（断开时静默丢弃）。 */
export function sendTransportPresence(presence: CollabPresence): void {
  activeTransport?.sendPresence(presence);
}

