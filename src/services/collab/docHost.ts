/**
 * 协作文档宿主 DocHost（内核，域无关）：持有活动传输句柄，入站帧统一路由到 `utils/collabHost` 注册表（各域 handler 自注册）。
 * 出站经 `sendTransportMessage` 咽喉（断开静默丢弃）；帧被裁剪回调 `onResync`，由调用方按域补齐——笔记域重新握手索取全量状态，补丁域只补发 presence（陈旧补丁由后续补丁与保存合并收敛）。
 * 文档实例的生命周期（引用计数/创建/重建/销毁）由各领域服务自持（如 noteDoc 的 per-file Y.Doc）。
 */
import { dispatchCollabChannel, dispatchCollabMetaChanged, dispatchCollabRenamed } from "@/utils/collabHost";
import type { CollabHello, CollabPresence } from "@/types";
import {
  connectCollabTransport,
  type CollabChannel,
  type CollabTransportHandle,
  type CollabTransportOptions,
} from "./transport";

export type { CollabChannel } from "./transport";

/** 活动传输句柄（重连时替换；断开 = null）。 */
let activeTransport: CollabTransportHandle | null = null;

/** 活动连接的建连目标（url + hello）：识别「目标未变的重复建连请求」，调用方据此跳过拆建。 */
let activeTarget: { url: string; hello: CollabHello } | null = null;

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
  extends Omit<CollabTransportOptions, "onChannelMessage" | "onMetaChanged" | "onRenamed"> {
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
  activeTarget = { url: req.url, hello: req.hello };
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
    onRenamed: dispatchCollabRenamed,
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
  activeTarget = null;
}

/** 活动连接（在途握手或已连）是否即为目标连接：url 与 hello 全字段一致 = 同一目标。
 *  启动期协作宿主初始化被高频重复触发，目标未变时调用方应跳过拆建——每次拆建都会
 *  杀掉上一条握手中的连接（服务端只见 hello 前的 Close 帧），无谓重连还伴随状态抖动。 */
export function transportMatchesConnection(url: string, hello: CollabHello): boolean {
  if (!activeTransport || activeTransport.isClosed() || !activeTarget) return false;
  if (activeTarget.url !== url) return false;
  const keys = new Set([...Object.keys(activeTarget.hello), ...Object.keys(hello)]);
  for (const key of keys) {
    if (activeTarget.hello[key as keyof CollabHello] !== hello[key as keyof CollabHello]) {
      return false;
    }
  }
  return true;
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

