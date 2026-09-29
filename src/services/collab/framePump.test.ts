/**
 * 帧泵测试（services/collab/framePump.ts）：插件消息二进制帧编解码往返、seq 对账
 * （缺口上报 onResync、房间序号空间重置作废旧基线）、hello-ack 后的补投请求帧序。
 * 连接机制（心跳/重连）由 spaceTransport.test.ts 覆盖，本文件只测帧面与对账。
 */
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { encodePluginBinaryFrame, decodePluginBinaryFrame } from "./framePump";
import type { CollabTransportOptions } from "./transport";

/** 假 WebSocket：捕获出站帧（文本/二进制）、暴露入站入口。 */
class FakeWebSocket {
  static instances: FakeWebSocket[] = [];
  static readonly OPEN = 1;
  readyState = FakeWebSocket.OPEN;
  binaryType = "blob";
  sent: Array<string | Uint8Array> = [];
  onopen: (() => void) | null = null;
  onmessage: ((e: { data: string | ArrayBuffer }) => void) | null = null;
  onerror: (() => void) | null = null;
  onclose: (() => void) | null = null;

  constructor(public url: string) {
    FakeWebSocket.instances.push(this);
  }

  send(data: string | Uint8Array): void {
    this.sent.push(data);
  }

  close(): void {
    this.onclose?.();
  }

  open(): void {
    this.onopen?.();
  }

  /** 模拟服务端文本帧入站。 */
  emitText(payload: unknown): void {
    this.onmessage?.({ data: JSON.stringify(payload) });
  }

  /** 模拟服务端二进制帧入站（复制进独立 ArrayBuffer，模拟浏览器交付语义）。 */
  emitBinary(bytes: Uint8Array): void {
    this.onmessage?.({ data: bytes.slice().buffer });
  }

  textFrames(): unknown[] {
    return this.sent.filter((f): f is string => typeof f === "string").map((f) => JSON.parse(f));
  }

  binaryFrames(): Uint8Array[] {
    return this.sent.filter((f): f is Uint8Array => typeof f !== "string");
  }
}

/** 按服务端下行格式构建二进制插件帧（回填 seq/sender，不带定向目标）。 */
function serverBinaryFrame(
  channel: string,
  payload: Uint8Array,
  seq: number,
  sender: number,
): Uint8Array {
  const name = new TextEncoder().encode(channel);
  const frame = new Uint8Array(4 + name.length + 16 + payload.length);
  const view = new DataView(frame.buffer);
  frame[0] = 1;
  view.setUint16(2, name.length, true);
  frame.set(name, 4);
  view.setBigUint64(4 + name.length, BigInt(seq), true);
  view.setBigUint64(12 + name.length, BigInt(sender), true);
  frame.set(payload, 20 + name.length);
  return frame;
}

let socket: FakeWebSocket;
let channelMessages: Array<[number, string, unknown]>;
let resyncs: number;
/** setTimeout 桩的待触发队列（超时兜底由测试手动点火）。 */
let pendingTimeouts: Map<number, () => void>;
let timeoutSeq = 0;

function fireTimeout(id: number): void {
  const fn = pendingTimeouts.get(id);
  pendingTimeouts.delete(id);
  fn?.();
}

function connect(opts?: { pluginLastSeq?: number | null }) {
  const request: CollabTransportOptions = {
    url: "ws://s/ws/space",
    hello: { spaceId: "sp1", token: "tok", nickname: "n", color: "#000", deviceName: "d" },
    pluginLastSeq: opts?.pluginLastSeq,
    onHelloAck: () => {},
    onPeers: () => {},
    onPeerPresence: () => {},
    onChannelMessage: (peerId, channel, file, payload) => {
      if (channel === "plugin-msg") channelMessages.push([peerId, file, payload]);
    },
    onMetaChanged: () => {},
    onResync: () => {
      resyncs += 1;
    },
    onServerError: () => {},
    onStatusChange: () => {},
  };
  // 空间工厂即 connectChannelPump（模块加载已注册），经注册表建立连接
  return import("./spaceTransport").then((mod) => mod.spaceCollabTransport.connect(request));
}

beforeEach(() => {
  vi.stubGlobal("WebSocket", FakeWebSocket);
  pendingTimeouts = new Map();
  vi.stubGlobal("window", {
    setInterval: () => 1,
    clearInterval: () => {},
    setTimeout: (fn: () => void) => {
      pendingTimeouts.set(++timeoutSeq, fn);
      return timeoutSeq;
    },
    clearTimeout: (id: number) => pendingTimeouts.delete(id),
  });
  FakeWebSocket.instances = [];
  channelMessages = [];
  resyncs = 0;
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("插件消息二进制帧", () => {
  it("编解码往返：channel/payload/seq/sender/target 原样还原（payload 不经转码）", () => {
    const payload = new Uint8Array([0, 1, 2, 250, 251, 255]);
    const encoded = encodePluginBinaryFrame("com.x.wb:sync", payload, 9);
    const decoded = decodePluginBinaryFrame(encoded.slice().buffer);
    expect(decoded).not.toBeNull();
    expect(decoded!.channel).toBe("com.x.wb:sync");
    expect(Array.from(decoded!.payload)).toEqual(Array.from(payload));
    expect(decoded!.targetPeerId).toBe(9);
    expect(decoded!.seq).toBe(0);
    expect(decoded!.senderPeerId).toBe(0);
  });

  it("畸形帧恒拒：未知 kind、截断帧返回 null", () => {
    const encoded = encodePluginBinaryFrame("ch", new Uint8Array([1, 2, 3]));
    const badKind = encoded.slice();
    badKind[0] = 9;
    expect(decodePluginBinaryFrame(badKind.slice().buffer)).toBeNull();
    expect(decodePluginBinaryFrame(encoded.slice(0, 3).buffer)).toBeNull();
  });

  it("入站二进制帧：载荷以 Uint8Array 透传到 plugin-msg 通道，peerId 取自帧头 sender", async () => {
    const handle = await connect();
    socket = FakeWebSocket.instances[0];
    socket.open();
    socket.emitText({ type: "hello-ack", peerId: 5 });
    socket.emitBinary(serverBinaryFrame("com.x.wb:sync", new Uint8Array([9, 9]), 3, 12));
    expect(channelMessages).toEqual([[12, "com.x.wb:sync", new Uint8Array([9, 9])]]);
    handle.disconnect();
  });

  it("出站二进制载荷走二进制帧（kind=1），JSON 载荷仍走文本帧，混发互不干扰", async () => {
    const handle = await connect();
    socket = FakeWebSocket.instances[0];
    socket.open();
    handle.sendMessage("plugin-msg", "ch", new Uint8Array([1, 2, 3]));
    handle.sendMessage("plugin-msg", "ch", { text: 1 });
    expect(socket.binaryFrames()).toHaveLength(1);
    expect(socket.binaryFrames()[0][0]).toBe(1);
    const texts = socket.textFrames().filter((f) => (f as { type: string }).type === "plugin-msg");
    expect(texts).toEqual([{ type: "plugin-msg", channel: "ch", payload: { text: 1 } }]);
    handle.disconnect();
  });
});

describe("插件帧序号对账", () => {
  it("hello-ack 后按 pluginLastSeq 发补投请求；无基线不发", async () => {
    const handle = await connect({ pluginLastSeq: 7 });
    socket = FakeWebSocket.instances[0];
    socket.open();
    socket.emitText({ type: "hello-ack", peerId: 5 });
    const texts = socket.textFrames();
    expect(texts[0]).toMatchObject({ type: "hello" });
    expect(texts[1]).toEqual({ type: "plugin-replay", after: 7 });
    handle.disconnect();

    FakeWebSocket.instances = [];
    const fresh = await connect();
    socket = FakeWebSocket.instances[0];
    socket.open();
    socket.emitText({ type: "hello-ack", peerId: 6 });
    expect(socket.textFrames().filter((f) => (f as { type: string }).type === "plugin-replay")).toEqual([]);
    fresh.disconnect();
  });

  it("入站 seq 单调推进；缺口（缓存已逐出）上报 onResync", async () => {
    const handle = await connect();
    socket = FakeWebSocket.instances[0];
    socket.open();
    socket.emitText({ type: "hello-ack", peerId: 5 });
    socket.emitText({ type: "plugin-msg", peerId: 3, channel: "a:b", payload: 1, seq: 8 });
    expect(resyncs).toBe(0);
    socket.emitText({ type: "plugin-msg", peerId: 3, channel: "a:b", payload: 2, seq: 9 });
    expect(resyncs).toBe(0);
    socket.emitText({ type: "plugin-msg", peerId: 3, channel: "a:b", payload: 3, seq: 13 });
    expect(resyncs).toBe(1);
    // 二进制帧同样参与对账
    socket.emitBinary(serverBinaryFrame("a:b", new Uint8Array([1]), 14, 3));
    expect(resyncs).toBe(1);
    socket.emitBinary(serverBinaryFrame("a:b", new Uint8Array([1]), 20, 3));
    expect(resyncs).toBe(2);
    handle.disconnect();
  });

  it("hello-ack 序号头小于本地基线 = 房间序号空间重置：基线归零并按 after=0 补回新房间帧", async () => {
    const handle = await connect({ pluginLastSeq: 50 });
    socket = FakeWebSocket.instances[0];
    socket.open();
    socket.emitText({ type: "hello-ack", peerId: 5, pluginSeq: 3 });
    expect(socket.textFrames().filter((f) => (f as { type: string }).type === "plugin-replay")).toEqual([
      { type: "plugin-replay", after: 0 },
    ]);
    // 重建房间离线期间已广播的帧（seq 1..3）经补投按序取回，不误报缺口
    socket.emitText({ type: "plugin-msg", peerId: 3, channel: "a:b", payload: 1, seq: 1 });
    socket.emitText({ type: "plugin-msg", peerId: 3, channel: "a:b", payload: 3, seq: 3 });
    socket.emitText({ type: "plugin-msg", peerId: 3, channel: "a:b", payload: 2, seq: 2 });
    expect(resyncs).toBe(0);
    expect(channelMessages.map((m) => m[2])).toEqual([1, 2, 3]);
    handle.disconnect();
  });

  it("补投窗口内直播帧抢先到达：入缓冲等待，补投帧到达后整体按序投递且不误报缺口", async () => {
    const handle = await connect({ pluginLastSeq: 10 });
    socket = FakeWebSocket.instances[0];
    socket.open();
    socket.emitText({ type: "hello-ack", peerId: 5, pluginSeq: 13 });
    expect(socket.textFrames().filter((f) => (f as { type: string }).type === "plugin-replay")).toEqual([
      { type: "plugin-replay", after: 10 },
    ]);
    // 直播帧 seq 14 在服务端处理补投请求前先到达（应入缓冲，不报缺口、不投递）
    socket.emitText({ type: "plugin-msg", peerId: 3, channel: "a:b", payload: 14, seq: 14 });
    expect(channelMessages).toEqual([]);
    expect(resyncs).toBe(0);
    // 补投帧 11..13 到达：与缓冲的 14 合并按序投递
    socket.emitText({ type: "plugin-msg", peerId: 3, channel: "a:b", payload: 11, seq: 11 });
    socket.emitText({ type: "plugin-msg", peerId: 3, channel: "a:b", payload: 12, seq: 12 });
    socket.emitText({ type: "plugin-msg", peerId: 3, channel: "a:b", payload: 13, seq: 13 });
    expect(channelMessages.map((m) => m[2])).toEqual([11, 12, 13, 14]);
    expect(resyncs).toBe(0);
    handle.disconnect();
  });

  it("补投窗口内重复帧丢弃；无序号单播帧尽力而为直投", async () => {
    const handle = await connect({ pluginLastSeq: 10 });
    socket = FakeWebSocket.instances[0];
    socket.open();
    socket.emitText({ type: "hello-ack", peerId: 5, pluginSeq: 11 });
    // 已投递过的 seq 10（重复）丢弃；无 seq 单播直投；补投帧 11 到达后冲刷退出缓冲
    socket.emitText({ type: "plugin-msg", peerId: 3, channel: "a:b", payload: "dup", seq: 10 });
    socket.emitText({ type: "plugin-msg", peerId: 3, channel: "a:b", payload: "unicast" });
    socket.emitText({ type: "plugin-msg", peerId: 3, channel: "a:b", payload: 11, seq: 11 });
    expect(channelMessages.map((m) => m[2])).toEqual(["unicast", 11]);
    expect(resyncs).toBe(0);
    handle.disconnect();
  });

  it("补投超时（缓存缺口补不齐）：上报 onResync 后按 seq 升序尽力投递剩余帧", async () => {
    const handle = await connect({ pluginLastSeq: 10 });
    socket = FakeWebSocket.instances[0];
    socket.open();
    socket.emitText({ type: "hello-ack", peerId: 5, pluginSeq: 13 });
    // 补投只回放得到 12..13（11 已被缓存逐出）：expected=11 一直等不到，直播 14 也积压
    socket.emitText({ type: "plugin-msg", peerId: 3, channel: "a:b", payload: 12, seq: 12 });
    socket.emitText({ type: "plugin-msg", peerId: 3, channel: "a:b", payload: 14, seq: 14 });
    expect(channelMessages).toEqual([]);
    // 超时兜底点火：缺口告警 + 剩余帧按 seq 升序投递
    expect(pendingTimeouts.size).toBe(1);
    fireTimeout([...pendingTimeouts.keys()][0]);
    expect(resyncs).toBe(1);
    expect(channelMessages.map((m) => m[2])).toEqual([12, 14]);
    handle.disconnect();
  });

  it("二进制单播下行 seq=0 是无序号哨兵：不毒化空基线、不误报缺口", async () => {
    const handle = await connect();
    socket = FakeWebSocket.instances[0];
    socket.open();
    socket.emitText({ type: "hello-ack", peerId: 5 });
    // 先到一条二进制单播（服务端哨兵 seq=0），基线须保持 null
    socket.emitBinary(serverBinaryFrame("a:b", new Uint8Array([1]), 0, 3));
    expect(channelMessages.map((m) => m[2])).toEqual([new Uint8Array([1])]);
    // 后续广播帧相对空基线不算缺口
    socket.emitText({ type: "plugin-msg", peerId: 3, channel: "a:b", payload: 8, seq: 8 });
    expect(resyncs).toBe(0);
    handle.disconnect();
  });

  it("基线已到序号头（静音房间重连）：不发补投请求、不进缓冲模式，直播帧直通无虚假告警", async () => {
    const handle = await connect({ pluginLastSeq: 10 });
    socket = FakeWebSocket.instances[0];
    socket.open();
    socket.emitText({ type: "hello-ack", peerId: 5, pluginSeq: 10 });
    expect(socket.textFrames().filter((f) => (f as { type: string }).type === "plugin-replay")).toEqual([]);
    expect(pendingTimeouts.size).toBe(0);
    // 直播帧按序直通投递
    socket.emitText({ type: "plugin-msg", peerId: 3, channel: "a:b", payload: 11, seq: 11 });
    expect(channelMessages.map((m) => m[2])).toEqual([11]);
    expect(resyncs).toBe(0);
    handle.disconnect();
  });

  it("旧服务端（无 seq 字段）帧跳过对账，收发行为不变", async () => {
    const handle = await connect({ pluginLastSeq: 7 });
    socket = FakeWebSocket.instances[0];
    socket.open();
    socket.emitText({ type: "hello-ack", peerId: 5 });
    socket.emitText({ type: "plugin-msg", peerId: 3, channel: "a:b", payload: 1 });
    expect(resyncs).toBe(0);
    expect(channelMessages).toEqual([[3, "a:b", 1]]);
    handle.disconnect();
  });
});
