/**
 * 协作中转客户端传输测试（services/collab/relay.ts）：用假 WebSocket 驱动，锁定两条协议契约——
 * 1. `onopen` 内 **hello 必须是首帧**：连接态回调会同步触发各域重连钩子（可能立刻广播 note-sync），
 *    若 hello 后发会被中转按「首条消息须为 hello」拒绝（整条连接不可用）；
 * 2. 服务端帧分发：`resync` → `onResync`、`note-sync`/`note-aware` 原样透传、非法 JSON 帧被忽略。
 */
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";

type RelayMod = typeof import("./relay");

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

  /** 模拟无法解析的服务端帧。 */
  emitRaw(data: string): void {
    this.onmessage?.({ data });
  }
}

let mod: RelayMod;

beforeEach(async () => {
  vi.resetModules();
  FakeWebSocket.instances = [];
  vi.stubGlobal("WebSocket", FakeWebSocket);
  // relay 客户端在浏览器上下文用 window.setInterval/setTimeout（心跳与重连退避）；
  // node 测试环境无 window，桩掉计时器使测试不依赖真实时间
  vi.stubGlobal("window", {
    setInterval: () => 1,
    clearInterval: () => {},
    setTimeout: () => 2,
    clearTimeout: () => {},
  });
  mod = await import("./relay");
});

afterEach(() => {
  vi.unstubAllGlobals();
});

interface Harness {
  handle: ReturnType<RelayMod["connectCollabRelay"]>;
  socket: FakeWebSocket;
  channels: Array<[number, string, string, unknown]>;
  stats: { resyncs: number };
  errors: string[];
  statuses: boolean[];
}

function connect(onStatusChange?: (connected: boolean) => void): Harness {
  const channels: Array<[number, string, string, unknown]> = [];
  const errors: string[] = [];
  const statuses: boolean[] = [];
  const stats = { resyncs: 0 };
  const handle = mod.connectCollabRelay({
    url: "ws://relay/ws",
    hello: { vaultId: "v", nickname: "n", color: "#000", deviceName: "d" },
    onHelloAck: () => {},
    onPeers: () => {},
    onPeerPresence: () => {},
    onTablePatch: () => {},
    onCanvasPatch: () => {},
    onNoteSync: (peerId, file, payload) => channels.push([peerId, "note-sync", file, payload]),
    onNoteAware: (peerId, file, payload) => channels.push([peerId, "note-aware", file, payload]),
    onPluginMsg: (peerId, channel, payload) => channels.push([peerId, "plugin-msg", channel, payload]),
    onResync: () => {
      stats.resyncs += 1;
    },
    onServerError: (message) => errors.push(message),
    onStatusChange: (connected) => {
      statuses.push(connected);
      onStatusChange?.(connected);
    },
  });
  const socket = FakeWebSocket.instances[FakeWebSocket.instances.length - 1];
  return { handle, socket, channels, errors, statuses, stats };
}

describe("relay 客户端握手帧序", () => {
  it("hello 先于连接态回调触发的同步帧（否则被中转拒绝首条非 hello）", () => {
    const ref: { handle: Harness["handle"] | null } = { handle: null };
    const h = connect((connected) => {
      // 复刻 collabStore 的行为：连接成功即重新握手，立刻发出 note-sync 帧
      if (connected) ref.handle?.sendNoteSync("notes/a.md", "AAA=");
    });
    ref.handle = h.handle;
    h.socket.open();

    expect(h.socket.sent).toHaveLength(2);
    expect(JSON.parse(h.socket.sent[0]).type).toBe("hello");
    expect(JSON.parse(h.socket.sent[1]).type).toBe("note-sync");
    expect(h.statuses).toEqual([false, true]);
  });

  it("hello 携带房间与身份字段（首帧即可入房）", () => {
    const h = connect();
    h.socket.open();
    expect(JSON.parse(h.socket.sent[0])).toMatchObject({
      type: "hello",
      vaultId: "v",
      nickname: "n",
      color: "#000",
      deviceName: "d",
    });
  });
});

describe("relay 客户端入站分发", () => {
  it("resync → onResync（每帧一次）", () => {
    const h = connect();
    h.socket.open();
    h.socket.emit({ type: "resync" });
    h.socket.emit({ type: "resync" });
    expect(h.stats.resyncs).toBe(2);
  });

  it("note-sync / note-aware 原样透传到对应回调", () => {
    const h = connect();
    h.socket.open();
    h.socket.emit({ type: "note-sync", peerId: 3, file: "a.md", payload: "AAA=" });
    h.socket.emit({ type: "note-aware", peerId: 4, file: "b.md", payload: "BBB=" });
    expect(h.channels).toEqual([
      [3, "note-sync", "a.md", "AAA="],
      [4, "note-aware", "b.md", "BBB="],
    ]);
  });

  it("error 帧上报给调用方", () => {
    const h = connect();
    h.socket.open();
    h.socket.emit({ type: "error", message: "房间拒绝" });
    expect(h.errors).toEqual(["房间拒绝"]);
  });

  it("非法 JSON 帧被忽略且不影响后续帧", () => {
    const h = connect();
    h.socket.open();
    h.socket.emitRaw("{不是 JSON");
    h.socket.emit({ type: "resync" });
    h.socket.emit({ type: "note-sync", peerId: 1, file: "a.md", payload: "P" });
    expect(h.stats.resyncs).toBe(1);
    expect(h.channels).toHaveLength(1);
  });
});

describe("relay 客户端插件消息收发", () => {
  it("sendPluginMsg 出站帧：广播不带 targetPeerId，单播携带；返回已投递", () => {
    const h = connect();
    h.socket.open();
    expect(h.handle.sendPluginMsg("comfyui.remote", { cmd: "start" })).toBe(true);
    expect(h.handle.sendPluginMsg("comfyui.remote", { cmd: "stop" }, 9)).toBe(true);
    expect(h.socket.sent.slice(1).map((s) => JSON.parse(s))).toEqual([
      { type: "plugin-msg", channel: "comfyui.remote", payload: { cmd: "start" } },
      { type: "plugin-msg", channel: "comfyui.remote", payload: { cmd: "stop" }, targetPeerId: 9 },
    ]);
  });

  it("断开后 sendPluginMsg 返回 false 且不发帧", () => {
    const h = connect();
    h.socket.open();
    h.socket.close();
    expect(h.handle.sendPluginMsg("comfyui.remote", {})).toBe(false);
    expect(h.socket.sent.filter((s) => JSON.parse(s).type !== "hello")).toEqual([]);
  });

  it("入站 plugin-msg 原样透传到回调（payload 任意 JSON，不含定向信息）", () => {
    const h = connect();
    h.socket.open();
    h.socket.emit({
      type: "plugin-msg",
      peerId: 5,
      channel: "comfyui.remote",
      payload: { cmd: "start", ts: 1 },
    });
    expect(h.channels).toEqual([[5, "plugin-msg", "comfyui.remote", { cmd: "start", ts: 1 }]]);
  });

  it("既有 send* 返回是否已投递（已连接 = true，断开后 = false）", () => {
    const h = connect();
    h.socket.open();
    expect(h.handle.sendNoteSync("a.md", "AAA=")).toBe(true);
    expect(h.handle.sendTablePatch("t.atb", { ops: [] } as never)).toBe(true);
    expect(h.handle.sendCanvasPatch("c.atlx", { nodes: [] } as never)).toBe(true);
    expect(h.handle.sendNoteAware("a.md", "B")).toBe(true);
    h.socket.close();
    expect(h.handle.sendNoteSync("a.md", "AAA=")).toBe(false);
  });

  it("relay 工厂：sendMessage 分发 plugin-msg（file 槽 = 频道名，透传 target）与入站映射", () => {
    const onChannel: Array<[number, string, string, unknown]> = [];
    const handle = mod.collabRelayTransport.connect({
      url: "ws://relay/ws",
      hello: { vaultId: "v", nickname: "n", color: "#000", deviceName: "d" },
      onHelloAck: () => {},
      onPeers: () => {},
      onPeerPresence: () => {},
      onChannelMessage: (peerId, channel, file, payload) => onChannel.push([peerId, channel, file, payload]),
      onResync: () => {},
      onServerError: () => {},
      onStatusChange: () => {},
    });
    const socket = FakeWebSocket.instances[FakeWebSocket.instances.length - 1];
    socket.open();
    expect(handle.sendMessage("plugin-msg", "comfyui.remote", { cmd: "stop" }, 9)).toBe(true);
    expect(handle.sendMessage("note-sync", "a.md", "AAA=")).toBe(true);
    expect(socket.sent.slice(1).map((s) => JSON.parse(s))).toEqual([
      { type: "plugin-msg", channel: "comfyui.remote", payload: { cmd: "stop" }, targetPeerId: 9 },
      { type: "note-sync", file: "a.md", payload: "AAA=" },
    ]);
    socket.emit({ type: "plugin-msg", peerId: 3, channel: "comfyui.remote", payload: { cmd: "start" } });
    expect(onChannel).toEqual([[3, "plugin-msg", "comfyui.remote", { cmd: "start" }]]);
    handle.disconnect();
  });
});
