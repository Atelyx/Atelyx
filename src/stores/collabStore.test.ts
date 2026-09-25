/**
 * 协作连接策略测试（stores/collabStore.ts）：用假 WebSocket 驱动真实连接流程，锁定两条行为——
 * 1. 连接建立后各域重连钩子只跑一次（`onStatusChange(true)`），不因连接建立本身重复触发；
 * 2. 服务端缺帧提示（`resync`）在合并窗口内只触发一次重连补齐（防「重握手大帧 → 更慢 → 再下发」自激）。
 * 协作只存在于协作空间：连接目标按 space 身份解析，个人仓库/未进仓不连接（一并锁定）。
 * 传输层帧序与分发由 `services/collab/spaceTransport.test.ts` 覆盖，本文件只测 store 策略。
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

vi.mock("@/services/space/auth", () => ({
  getToken: vi.fn(async () => "tok-123"),
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

/** 进入协作空间（置 space 身份）并建立连接，返回假 socket（`init` 内有 await 版本号，故等待实例出现）。 */
async function connectSpace(): Promise<FakeWebSocket> {
  app.useAppStore.setState({
    vaultIdentity: { kind: "space", serverUrl: "http://s:11224", spaceId: "sp1" },
  });
  collab.useCollabStore.getState().init({
    enabled: true,
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
  it("连接建立触发一次重连钩子，首帧为带 spaceId 的 hello", async () => {
    const socket = await connectSpace();
    expect(reconnects).toBe(1);
    expect(JSON.parse(socket.sent[0])).toEqual({
      type: "hello",
      spaceId: "sp1",
      token: "tok-123",
      nickname: "甲",
      color: "#123456",
      deviceName: "机器甲",
    });
  });

  it("个人仓库/未进仓不连接（协作只存在于协作空间）", async () => {
    app.useAppStore.setState({
      vaultRoot: "E:/vault-1",
      vaultIdentity: { kind: "local", root: "E:/vault-1" },
    });
    collab.useCollabStore.getState().init({
      enabled: true,
      nickname: "甲",
      color: "",
      deviceName: "机器甲",
    });
    await new Promise((r) => setTimeout(r, 0));
    expect(FakeWebSocket.instances).toHaveLength(0);
    expect(collab.useCollabStore.getState().connected).toBe(false);
  });

  it("缺帧提示在合并窗口内只触发一次重连补齐", async () => {
    const socket = await connectSpace();
    const base = reconnects; // 连接建立那次已计入，取基线
    socket.emit({ type: "resync" });
    socket.emit({ type: "resync" });
    socket.emit({ type: "resync" });
    expect(reconnects).toBe(base + 1);
  });

  it("换连接后合并窗口复位：新连接的首个缺帧提示立即生效", async () => {
    const first = await connectSpace();
    first.emit({ type: "resync" });
    const afterFirst = reconnects;
    // 重新 applyConfig（昵称变更）→ 换连接；旧连接 3s 窗口不得吞掉新连接的首个 resync
    collab.useCollabStore.getState().applyConfig({ nickname: "乙" });
    await vi.waitFor(() => expect(FakeWebSocket.instances.length).toBe(2));
    const second = FakeWebSocket.instances[1];
    second.open();
    const afterConnect = reconnects;
    second.emit({ type: "resync" });
    expect(reconnects).toBe(afterConnect + 1);
    expect(afterFirst).toBeLessThan(afterConnect); // 新连接建立确实又跑了一次重连钩子
  });

  it("切回个人仓库断开连接且不再新建", async () => {
    await connectSpace();
    app.useAppStore.setState({ vaultIdentity: { kind: "local", root: "E:/v" } });
    await new Promise((r) => setTimeout(r, 0));
    expect(FakeWebSocket.instances).toHaveLength(1);
    expect(collab.useCollabStore.getState().connected).toBe(false);
    expect(collab.useCollabStore.getState().peers).toEqual([]);
  });

  it("令牌挂起期间 dispose：旧建立请求恢复后不建连", async () => {
    const auth = await import("@/services/space/auth");
    vi.mocked(auth.getToken).mockClear();
    let release!: (token: string) => void;
    vi.mocked(auth.getToken).mockImplementationOnce(
      () => new Promise<string>((resolve) => { release = resolve; }),
    );
    app.useAppStore.setState({
      vaultIdentity: { kind: "space", serverUrl: "http://s:11224", spaceId: "sp1" },
    });
    collab.useCollabStore.getState().init({
      enabled: true,
      nickname: "甲",
      color: "#123456",
      deviceName: "机器甲",
    });
    await vi.waitFor(() => expect(auth.getToken).toHaveBeenCalledTimes(1));
    collab.useCollabStore.getState().dispose();
    release("tok-late");
    await new Promise((r) => setTimeout(r, 0));
    expect(FakeWebSocket.instances).toHaveLength(0);
  });

  it("令牌挂起期间 applyConfig：旧请求作废，连接来自新请求", async () => {
    const auth = await import("@/services/space/auth");
    vi.mocked(auth.getToken).mockClear();
    let release!: (token: string) => void;
    vi.mocked(auth.getToken).mockImplementationOnce(
      () => new Promise<string>((resolve) => { release = resolve; }),
    );
    app.useAppStore.setState({
      vaultIdentity: { kind: "space", serverUrl: "http://s:11224", spaceId: "sp1" },
    });
    collab.useCollabStore.getState().init({
      enabled: true,
      nickname: "甲",
      color: "#123456",
      deviceName: "机器甲",
    });
    await vi.waitFor(() => expect(auth.getToken).toHaveBeenCalledTimes(1));
    // 挂起期间改配置 → 新建立请求（第二次 getToken 正常返回并建连）
    collab.useCollabStore.getState().applyConfig({ nickname: "乙" });
    await vi.waitFor(() => expect(FakeWebSocket.instances.length).toBe(1));
    release("tok-late");
    await new Promise((r) => setTimeout(r, 0));
    // 旧请求恢复后不再建连，存量连接是按新配置建立的
    expect(FakeWebSocket.instances).toHaveLength(1);
    FakeWebSocket.instances[0].open();
    expect(JSON.parse(FakeWebSocket.instances[0].sent[0])).toMatchObject({ nickname: "乙" });
  });

  it("服务端 error 帧：经通知注入点提示用户可见，同因随重连重放不刷屏", async () => {
    const access = await import("@/services/cordis/access");
    const notify = vi.fn(() => "n");
    access.setPluginNotificationAccess({ notify, dismiss: () => {} });
    const socket = await connectSpace();
    socket.emit({ type: "error", message: "无效令牌" });
    socket.emit({ type: "error", message: "无效令牌" });
    expect(notify).toHaveBeenCalledTimes(1);
    expect(notify).toHaveBeenCalledWith(
      expect.objectContaining({
        level: "error",
        message: expect.stringContaining("无效令牌"),
      }),
    );
  });
});

describe("插件消息发送与本端身份", () => {
  it("sendPluginMessage 出站：广播/单播帧正确，返回是否已投递", async () => {
    const socket = await connectSpace();
    expect(collab.sendPluginMessage("comfyui.remote", { cmd: "start" })).toBe(true);
    expect(collab.sendPluginMessage("comfyui.remote", { cmd: "stop" }, 9)).toBe(true);
    expect(socket.sent.slice(1).map((s) => JSON.parse(s))).toEqual([
      { type: "plugin-msg", channel: "comfyui.remote", payload: { cmd: "start" } },
      { type: "plugin-msg", channel: "comfyui.remote", payload: { cmd: "stop" }, targetPeerId: 9 },
    ]);
  });

  it("sendPluginMessage 断开时返回 false（消息未发出，不静默）", async () => {
    const socket = await connectSpace();
    socket.close();
    expect(collab.sendPluginMessage("comfyui.remote", {})).toBe(false);
  });

  it("getMyPeerInfo：未 init 空身份，连接后 peerId 来自 hello-ack", async () => {
    expect(collab.getMyPeerInfo()).toEqual({
      peerId: null,
      nickname: "用户",
      color: "",
      deviceName: "",
    });
    const socket = await connectSpace();
    socket.emit({ type: "hello-ack", peerId: 7 });
    expect(collab.getMyPeerInfo()).toMatchObject({
      peerId: 7,
      nickname: "甲",
      deviceName: "机器甲",
    });
  });
});

describe("插件协作意愿声明", () => {
  it("retain 计数递增，释放递减；多次声明按计数合并", () => {
    const releaseA = collab.useCollabStore.getState().retainPluginDemand();
    const releaseB = collab.useCollabStore.getState().retainPluginDemand();
    expect(collab.useCollabStore.getState().pluginDemand).toBe(2);
    releaseA();
    expect(collab.useCollabStore.getState().pluginDemand).toBe(1);
    releaseB();
    expect(collab.useCollabStore.getState().pluginDemand).toBe(0);
  });

  it("释放函数幂等：重复调用不产生负数或额外递减", () => {
    const release = collab.useCollabStore.getState().retainPluginDemand();
    expect(collab.useCollabStore.getState().pluginDemand).toBe(1);
    release();
    release();
    expect(collab.useCollabStore.getState().pluginDemand).toBe(0);
  });

  it("dispose 只断连接不清意愿：插件仍在运行时声明保持有效", async () => {
    await connectSpace();
    const release = collab.useCollabStore.getState().retainPluginDemand();
    collab.useCollabStore.getState().dispose();
    expect(collab.useCollabStore.getState().connected).toBe(false);
    expect(collab.useCollabStore.getState().pluginDemand).toBe(1);
    release();
    expect(collab.useCollabStore.getState().pluginDemand).toBe(0);
  });
});
