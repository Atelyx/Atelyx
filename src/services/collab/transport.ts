/**
 * 协作传输接口（内核，域无关）：传输 = 连接/房间/presence/频道收发的可注册后端。
 * 空间传输工厂（spaceTransport.ts 的 `spaceCollabTransport`，模块加载即注册）；
 * 换协作后端 = 注册新 factory（connect 返回同接口 handle），画布/笔记/表格零改动。
 */
import type { CollabHello, CollabPeer, CollabPresence } from "@/types";

/** 透传频道（传输层消息类型；新传输可按需扩展）。`plugin-msg` 为插件通用消息通道：
 *  file 槽承载插件频道名，payload 为任意 JSON，可选 targetPeerId 定向单播。 */
export type CollabChannel =
  | "note-sync"
  | "note-aware"
  | "canvas-patch"
  | "table-patch"
  | "plugin-msg";

export interface CollabTransportHandle {
  /** 上报本端 presence（调用方自行节流）。 */
  sendPresence(presence: CollabPresence): void;
  /** 按频道透传一条消息（传输层不透明转发；断开时静默丢弃）。返回是否已投递到传输层。
   *  `plugin-msg` 的 file 槽 = 插件频道名，targetPeerId 有值 = 定向单播（其余频道忽略）。 */
  sendMessage(
    channel: CollabChannel,
    file: string,
    payload: unknown,
    targetPeerId?: number,
  ): boolean;
  /** 主动离开房间（切仓库/关闭应用）。 */
  sendBye(): void;
  /** 断开连接且不再重连。 */
  disconnect(): void;
}

export interface CollabTransportOptions {
  url: string;
  hello: CollabHello;
  /** 重连前刷新 hello（身份/令牌/配置变化后自动生效）；返回 null = 放弃重连并正常收尾。
   *  可选：不提供 = 重连沿用构造时的 hello。 */
  refreshHello?: () => Promise<CollabHello | null>;
  /** 收到 hello-ack（分配的 peerId）——据此把自己过滤出 peers 列表。 */
  onHelloAck(peerId: number): void;
  onPeers(peers: CollabPeer[]): void;
  onPeerPresence(peerId: number, presence: CollabPresence): void;
  /** 频道消息入站（payload 不透明，解码归接收域；peerId 供发送方身份解析）。 */
  onChannelMessage(peerId: number, channel: CollabChannel, file: string, payload: unknown): void;
  /** 传输侧提示本端接收队列过慢（帧被裁剪）：调用方需重新握手补齐（笔记域索取全量状态）。 */
  onResync(): void;
  onServerError(message: string): void;
  onStatusChange(connected: boolean): void;
}

export interface CollabTransportFactory {
  name: string;
  connect(opts: CollabTransportOptions): CollabTransportHandle;
}

const collabTransports = new Map<string, CollabTransportFactory>();

/** 注册传输工厂（幂等覆盖：同 name 后注册者生效）。 */
export function registerCollabTransport(factory: CollabTransportFactory): void {
  collabTransports.set(factory.name, factory);
}

/** 按名建立连接（未注册报错）。 */
export function connectCollabTransport(
  name: string,
  opts: CollabTransportOptions,
): CollabTransportHandle {
  const factory = collabTransports.get(name);
  if (!factory) throw new Error(`协作传输未注册：${name}`);
  return factory.connect(opts);
}
