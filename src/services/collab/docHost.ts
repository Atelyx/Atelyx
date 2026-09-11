/**
 * 协作文档宿主 DocHost（内核，域无关）：协作传输的活动句柄与入站路由。
 *
 * - 组合传输注册表（transport.ts，默认内建 relay）与域接线注册表（utils/collabHost）：
 *   活动传输句柄由本模块持有（collabStore 是 store 门面，经 connectTransport/send* 管理）；
 * - 入站频道消息统一路由到 collabHost 通道注册表（各域 handler 自注册）；
 * - 出站经 send* 咽喉（断开时静默丢弃）；
 * - 传输侧报告接收队列过慢（帧被裁剪）时回调 `onResync`，由调用方按域补齐
 *   （笔记域重新握手索取全量状态；补丁域只补发 presence，陈旧补丁由后续补丁与下次保存的
 *   乐观锁三方合并收敛）。
 *
 * 文档实例的生命周期（引用计数/创建/重建/销毁）由各领域服务自持（如 noteDoc 的 per-file Y.Doc）。
 */
import { dispatchCollabChannel } from "@/utils/collabHost";
import type { CollabPresence, RelayTestResult } from "@/types";
import {
  connectCollabTransport,
  testCollabTransport,
  type CollabChannel,
  type CollabTransportHandle,
  type CollabTransportOptions,
} from "./transport";
// 装配默认内建传输：relay.ts 模块加载时注册 collabRelayTransport（无环——relay 只依赖 transport）
import "@/services/collab/relay";

export type { CollabChannel } from "./transport";

/** 活动传输句柄（重连时替换；断开 = null）。 */
let activeTransport: CollabTransportHandle | null = null;

/** 建连请求（传输名 + 连接参数；onChannelMessage 由 DocHost 内部接通道注册表，调用方不必提供）。 */
export interface ConnectTransportRequest
  extends Omit<CollabTransportOptions, "onChannelMessage"> {
  name: string;
}

/** 建立连接（先 bye + 断开旧连接，再按名连新传输；未注册传输抛错）。
 *  建连失败时活动句柄置空（不残留已断开的旧句柄），由调用方记录降级。 */
export function connectTransport(req: ConnectTransportRequest): void {
  activeTransport?.sendBye();
  activeTransport?.disconnect();
  activeTransport = null;
  activeTransport = connectCollabTransport(req.name, {
    url: req.url,
    hello: req.hello,
    onHelloAck: req.onHelloAck,
    onPeers: req.onPeers,
    onPeerPresence: req.onPeerPresence,
    onChannelMessage: (peerId, channel, file, payload) =>
      dispatchCollabChannel(channel, peerId, file, payload),
    onResync: req.onResync,
    onServerError: req.onServerError,
    onStatusChange: req.onStatusChange,
  });
}

/** 断开并清空活动传输（不再重连）。 */
export function disconnectTransport(): void {
  activeTransport?.sendBye();
  activeTransport?.disconnect();
  activeTransport = null;
}

/** 出站咽喉：按频道透传（断开时静默丢弃）。 */
export function sendTransportMessage(channel: CollabChannel, file: string, payload: unknown): void {
  activeTransport?.sendMessage(channel, file, payload);
}

/** 出站 presence（断开时静默丢弃）。 */
export function sendTransportPresence(presence: CollabPresence): void {
  activeTransport?.sendPresence(presence);
}

/** 按名测试传输连通性（设置页「检查连接」用）。 */
export function testTransport(name: string, url: string): Promise<RelayTestResult> {
  return testCollabTransport(name, url);
}

