/**
 * 协作连接策略测试（stores/collabStore.ts）：用假 WebSocket 驱动真实连接流程，锁定两条行为——
 * 1. 连接建立后各域重连钩子只跑一次（`onStatusChange(true)`），不因连接建立本身重复触发；
 * 2. 服务端缺帧提示（`resync`）在合并窗口内只触发一次重连补齐（防「重握手大帧 → 更慢 → 再下发」自激）。
 * 传输层帧序与分发由 `services/collab/relay.test.ts` 覆盖，本文件只测 store 策略。
 */
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";

type CollabStore = typeof import("./collabStore");
type AppStore = typeof import("./appStore");
type CollabHost = typeof import("@/utils/collabHost");

/** 假 WebSocket：捕获出站帧、暴露入站入口。 */
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

  open(): void {
    this.onopen?.();
  }

  emit(payload: unknown): void {
    this.onmessage?.({ data: JSON.stringify(payload) });
  }
}

vi.mock("@tauri-apps/api/core", () => ({
  invoke: async () => "",
}));

let collab: CollabStore;
let app: AppStore;
let collabHost: CollabHost;
let reconnects = 0;
let offReconnect: (() => void) | null = null;

beforeEach(async () => {
  vi.resetModules();
  FakeWebSocket.instances = [];
  reconnects = 0;
  vi.stubGlobal("WebSocket", FakeWebSocket);
  // 连接策略用到 window.setTimeout（presence 节流）与 window.setInterval（心跳）；node 环境需桩掉
  vi.stubGlobal("window", {
    setInterval: () => 1,
    clearInterval: () => {},
    setTimeout: () => 2,
    clearTimeout: () => {},
  });
  app = await import("./appStore");
  collabHost = await import("@/utils/collabHost");
  offReconnect = collabHost.registerCollabReconnect(() => {
    reconnects += 1;
  });
  collab = await import("./collabStore");
});

afterEach(() => {
  offReconnect?.();
  vi.unstubAllGlobals();
});

/** 建立连接并返回假 socket（`init` 内有 await 版本号，故等待实例出现）。 */
async function connect(): Promise<FakeWebSocket> {
  app.useAppStore.setState({ vaultId: "vault-1" });
  collab.useCollabStore.getState().init({
    enabled: true,
    url: "ws://relay/ws",
    nickname: "甲",
    color: "#123456",
    deviceName: "机器甲",
  });
  await vi.waitFor(() => expect(FakeWebSocket.instances.length).toBe(1));
  const socket = FakeWebSocket.instances[0];
  socket.open();
  return socket;
}

describe("连接与重连补齐", () => {
  it("连接建立触发一次重连钩子，首帧为 hello", async () => {
    const socket = await connect();
    expect(reconnects).toBe(1);
    expect(JSON.parse(socket.sent[0]).type).toBe("hello");
  });

  it("缺帧提示在合并窗口内只触发一次重连补齐", async () => {
    const socket = await connect();
    const base = reconnects; // 连接建立那次已计入，取基线
    socket.emit({ type: "resync" });
    socket.emit({ type: "resync" });
    socket.emit({ type: "resync" });
    expect(reconnects).toBe(base + 1);
  });

  it("换连接后合并窗口复位：新连接的首个缺帧提示立即生效", async () => {
    const first = await connect();
    first.emit({ type: "resync" });
    const afterFirst = reconnects;
    // 重新 applyConfig（地址变更）→ 换连接；旧连接 3s 窗口不得吞掉新连接的首个 resync
    collab.useCollabStore.getState().applyConfig({ url: "ws://relay-2/ws" });
    await vi.waitFor(() => expect(FakeWebSocket.instances.length).toBe(2));
    const second = FakeWebSocket.instances[1];
    second.open();
    const afterConnect = reconnects;
    second.emit({ type: "resync" });
    expect(reconnects).toBe(afterConnect + 1);
    expect(afterFirst).toBeLessThan(afterConnect); // 新连接建立确实又跑了一次重连钩子
  });
});
