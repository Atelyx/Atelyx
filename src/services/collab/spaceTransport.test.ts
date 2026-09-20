/**
 * 协作空间传输测试（services/collab/spaceTransport.ts）：用假 WebSocket 驱动，锁定 /ws/space
 * 契约——hello 首帧形状（spaceId/token/身份字段，camelCase 与服务端 serde 对齐）、hello-ack
 * 分配 peerId、频道消息入站分发、出站帧形状（sendMessage/sendBye）、error 帧上报、断线状态可见。
 * 连接机制（心跳/重连）由共用帧泵（framePump.ts）承载，此处不重复覆盖。
 */
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import type { CollabHello } from "@/types";

type SpaceMod = typeof import("./spaceTransport");

/** 假 WebSocket：捕获出站帧、暴露入站入口（onopen/onmessage）。 */
class FakeWebSocket {
  static instances: FakeWebSocket[] = [];
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSING = 2;
  static readonly CLOSED = 3;
  readyState = FakeWebSocket.OPEN;
  sent: string[] = [];
  onopen: (() => void) | null = null;
  onmessage: ((e: { data: string }) => void) | null = null;
  onerror: (() => void) | null = null;
  onclose: (() => void) | null = null;

  constructor(public url: string) {
    FakeWebSocket.instances.push(this);
  }

  send(data: string): void {
    this.sent.push(data);
  }

  close(): void {
    this.onclose?.();
  }

  /** 模拟连接建立。 */
  open(): void {
    this.onopen?.();
  }

  /** 模拟服务端帧。 */
  emit(payload: unknown): void {
    this.onmessage?.({ data: JSON.stringify(payload) });
  }
}

let mod: SpaceMod;
/** 被捕获的重连计时器回调（桩不自动触发，测试手动点火控制时序）。 */
let retryTimers: Array<() => void> = [];

beforeEach(async () => {
  vi.resetModules();
  FakeWebSocket.instances = [];
  retryTimers = [];
  vi.stubGlobal("WebSocket", FakeWebSocket);
  // 帧泵在浏览器上下文用 window.setInterval/setTimeout（心跳与重连退避）；
  // node 测试环境无 window，桩掉计时器使测试不依赖真实时间；
  // 重连计时器捕获回调（不自动触发），重连时序由测试手动驱动
  vi.stubGlobal("window", {
    setInterval: () => 1,
    clearInterval: () => {},
    setTimeout: (fn: () => void) => {
      retryTimers.push(fn);
      return retryTimers.length;
    },
    clearTimeout: () => {},
  });
  mod = await import("./spaceTransport");
});

afterEach(() => {
  vi.unstubAllGlobals();
});

interface Harness {
  handle: ReturnType<typeof mod.spaceCollabTransport.connect>;
  socket: FakeWebSocket;
  channels: Array<[number, string, string, unknown]>;
  statuses: boolean[];
  errors: string[];
  acks: number[];
}

const spaceHello = {
  spaceId: "sp1",
  token: "tok-123",
  nickname: "n",
  color: "#000",
  deviceName: "d",
  version: "1.0.0",
};

function connect(refreshHello?: () => Promise<CollabHello | null>): Harness {
  const channels: Array<[number, string, string, unknown]> = [];
  const statuses: boolean[] = [];
  const errors: string[] = [];
  const acks: number[] = [];
  const handle = mod.spaceCollabTransport.connect({
    url: "ws://server:11224/ws/space",
    hello: { ...spaceHello },
    refreshHello,
    onHelloAck: (peerId) => acks.push(peerId),
    onPeers: () => {},
    onPeerPresence: () => {},
    onChannelMessage: (peerId, channel, file, payload) =>
      channels.push([peerId, channel, file, payload]),
    onResync: () => {},
    onServerError: (message) => errors.push(message),
    onStatusChange: (connected) => statuses.push(connected),
  });
  const socket = FakeWebSocket.instances[FakeWebSocket.instances.length - 1];
  return { handle, socket, channels, statuses, errors, acks };
}

describe("spaceWsUrl", () => {
  it("http → ws 并拼接 /ws/space", () => {
    expect(mod.spaceWsUrl("http://192.168.1.10:11224")).toBe("ws://192.168.1.10:11224/ws/space");
  });

  it("https → wss 并拼接 /ws/space", () => {
    expect(mod.spaceWsUrl("https://server.example.com")).toBe("wss://server.example.com/ws/space");
  });

  it("尾斜杠被去除后再拼接（单/多斜杠同形）", () => {
    expect(mod.spaceWsUrl("http://h:1/")).toBe("ws://h:1/ws/space");
    expect(mod.spaceWsUrl("https://h/")).toBe("wss://h/ws/space");
    expect(mod.spaceWsUrl("http://h/")).toBe("ws://h/ws/space");
  });

  it("已是 ws 协议的输入保持协议；空串返回空串", () => {
    expect(mod.spaceWsUrl("ws://h:1/")).toBe("ws://h:1/ws/space");
    expect(mod.spaceWsUrl("wss://h")).toBe("wss://h/ws/space");
    expect(mod.spaceWsUrl("")).toBe("");
  });
});

describe("space 传输握手", () => {
  it("hello 首帧携带空间号与令牌（camelCase 与服务端 serde 对齐）", () => {
    const h = connect();
    h.socket.open();
    expect(h.socket.sent).toHaveLength(1);
    expect(JSON.parse(h.socket.sent[0])).toEqual({
      type: "hello",
      spaceId: "sp1",
      token: "tok-123",
      nickname: "n",
      color: "#000",
      deviceName: "d",
      version: "1.0.0",
    });
  });

  it("hello-ack 分配 peerId；断线 onStatusChange(false) 可见", () => {
    const h = connect();
    h.socket.open();
    h.socket.emit({ type: "hello-ack", peerId: 7 });
    expect(h.acks).toEqual([7]);
    h.socket.close();
    expect(h.statuses).toEqual([false, true, false]);
  });

  it("error 帧（鉴权拒绝等）上报给调用方", () => {
    const h = connect();
    h.socket.open();
    h.socket.emit({ type: "error", message: "无效令牌" });
    expect(h.errors).toEqual(["无效令牌"]);
  });
});

describe("space 传输频道收发", () => {
  it("入站频道消息（note-sync/plugin-msg）分发到 onChannelMessage", () => {
    const h = connect();
    h.socket.open();
    h.socket.emit({ type: "note-sync", peerId: 3, file: "a.md", payload: "AAA=" });
    h.socket.emit({
      type: "plugin-msg",
      peerId: 5,
      channel: "comfyui.remote",
      payload: { cmd: "start" },
    });
    expect(h.channels).toEqual([
      [3, "note-sync", "a.md", "AAA="],
      [5, "plugin-msg", "comfyui.remote", { cmd: "start" }],
    ]);
  });

  it("sendMessage 出站映射：note-sync 原样、plugin-msg 的 file 槽 = 频道名（可定向）", () => {
    const h = connect();
    h.socket.open();
    expect(h.handle.sendMessage("note-sync", "a.md", "AAA=")).toBe(true);
    expect(h.handle.sendMessage("plugin-msg", "comfyui.remote", { cmd: "stop" }, 9)).toBe(true);
    expect(h.socket.sent.slice(1).map((s) => JSON.parse(s))).toEqual([
      { type: "note-sync", file: "a.md", payload: "AAA=" },
      { type: "plugin-msg", channel: "comfyui.remote", payload: { cmd: "stop" }, targetPeerId: 9 },
    ]);
  });

  it("sendBye 帧形状（type: bye）；工厂模块加载即注册进注册表", async () => {
    const h = connect();
    h.socket.open();
    h.handle.sendBye();
    expect(JSON.parse(h.socket.sent[1])).toEqual({ type: "bye" });

    const { connectCollabTransport } = await import("./transport");
    const registered = connectCollabTransport("space", {
      url: "ws://server:11224/ws/space",
      hello: { ...spaceHello },
      onHelloAck: () => {},
      onPeers: () => {},
      onPeerPresence: () => {},
      onChannelMessage: () => {},
      onResync: () => {},
      onServerError: () => {},
      onStatusChange: () => {},
    });
    expect(registered).toBeDefined();
    registered.disconnect();
  });
});

describe("断线重连刷新 hello", () => {
  it("重连前调用 refreshHello，新 hello 随重连生效", async () => {
    const refresh = vi.fn(async (): Promise<CollabHello | null> => ({
      ...spaceHello,
      token: "tok-new",
      nickname: "n2",
    }));
    const h = connect(refresh);
    h.socket.open();
    h.socket.close(); // 调度重连（计时器被捕获，未触发）
    retryTimers[0]();
    await vi.waitFor(() => expect(FakeWebSocket.instances.length).toBe(2));
    expect(refresh).toHaveBeenCalledTimes(1);
    FakeWebSocket.instances[1].open();
    expect(JSON.parse(FakeWebSocket.instances[1].sent[0])).toMatchObject({
      type: "hello",
      token: "tok-new",
      nickname: "n2",
    });
  });

  it("refreshHello 返回 null：放弃重连并正常收尾（不再建连、不再调度）", async () => {
    const refresh = vi.fn(async (): Promise<CollabHello | null> => null);
    const h = connect(refresh);
    h.socket.open();
    h.socket.close();
    retryTimers[0]();
    await new Promise((r) => setTimeout(r, 0));
    expect(refresh).toHaveBeenCalledTimes(1);
    expect(FakeWebSocket.instances).toHaveLength(1);
    // 放弃后无新的重连调度（计时器回调数不增）
    expect(retryTimers).toHaveLength(1);
  });
});
