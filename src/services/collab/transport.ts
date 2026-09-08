/**
 * 协作传输接口（内核，域无关）：传输 = 连接/房间/presence/频道收发的可注册后端。
 * 默认内建 relay 实现（relay.ts 的 `collabRelayTransport` 工厂，模块加载即注册）；
 * 换协作后端 = 注册新 factory（connect 返回同接口 handle），画布/笔记/表格零改动。
 */
import type { CollabHello, CollabPeer, CollabPresence, RelayTestResult } from "@/types";

/** 透传频道（relay 各消息类型；新传输可按需扩展）。 */
export type CollabChannel = "note-sync" | "note-aware" | "canvas-patch" | "table-patch";

export interface CollabTransportHandle {
  /** 上报本端 presence（调用方自行节流）。 */
  sendPresence(presence: CollabPresence): void;
  /** 按频道透传一条消息（relay 不透明转发；断开时静默丢弃）。 */
  sendMessage(channel: CollabChannel, file: string, payload: unknown): void;
  /** 主动离开房间（切仓库/关闭应用）。 */
  sendBye(): void;
  /** 断开连接且不再重连。 */
  disconnect(): void;
}

export interface CollabTransportOptions {
  url: string;
  hello: CollabHello;
  /** 收到 hello-ack（分配的 peerId）——据此把自己过滤出 peers 列表。 */
  onHelloAck(peerId: number): void;
  onPeers(peers: CollabPeer[]): void;
  onPeerPresence(peerId: number, presence: CollabPresence): void;
  /** 频道消息入站（payload 不透明，解码归接收域；peerId 供发送方身份解析）。 */
  onChannelMessage(peerId: number, channel: CollabChannel, file: string, payload: unknown): void;
  onServerError(message: string): void;
  onStatusChange(connected: boolean): void;
}

export interface CollabTransportFactory {
  name: string;
  connect(opts: CollabTransportOptions): CollabTransportHandle;
  /** 可选：一次性连通性测试（设置页「检查连接」用）。 */
  testConnection?(url: string): Promise<RelayTestResult>;
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

/** 按名测试连通性（传输不支持或无 testConnection 时报错）。 */
export function testCollabTransport(name: string, url: string): Promise<RelayTestResult> {
  const factory = collabTransports.get(name);
  if (!factory) throw new Error(`协作传输未注册：${name}`);
  if (!factory.testConnection) throw new Error(`传输不支持连接测试：${name}`);
  return factory.testConnection(url);
}
