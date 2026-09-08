/**
 * 协作传输注册表测试（services/collab/transport.ts）。
 * 覆盖工厂注册/覆盖/未注册报错、connect 路由到对应工厂、连通性测试路由与降级报错。
 * relay 工厂的真实连接走 WebSocket（node 环境不直测，映射语义由 collabStore 回归锚点覆盖）。
 */
import { describe, it, expect, beforeEach, vi } from "vitest";
import type {
  CollabTransportFactory,
  CollabTransportHandle,
  CollabTransportOptions,
} from "./transport";

// 注册表为模块级单例：每测试重置模块以隔离注册状态
type TransportMod = typeof import("./transport");
let mod: TransportMod;

beforeEach(async () => {
  vi.resetModules();
  mod = await import("./transport");
});

const fakeOptions = (): CollabTransportOptions => ({
  url: "ws://relay/ws",
  hello: { vaultId: "v", nickname: "n", color: "#000", deviceName: "d" },
  onHelloAck: () => {},
  onPeers: () => {},
  onPeerPresence: () => {},
  onChannelMessage: () => {},
  onServerError: () => {},
  onStatusChange: () => {},
});

function fakeHandle(): CollabTransportHandle {
  return {
    sendPresence: () => {},
    sendMessage: () => {},
    sendBye: () => {},
    disconnect: () => {},
  };
}

function fakeFactory(name = "fake"): CollabTransportFactory & {
  connect: ReturnType<typeof vi.fn>;
  testConnection: ReturnType<typeof vi.fn>;
} {
  return {
    name,
    connect: vi.fn(() => fakeHandle()),
    testConnection: vi.fn(async () => ({ ok: true, message: "ok" })),
  };
}

describe("传输注册表", () => {
  it("注册后 connect 路由到对应工厂并透传 options", () => {
    const f = fakeFactory();
    mod.registerCollabTransport(f);
    const opts = fakeOptions();
    const handle = mod.connectCollabTransport("fake", opts);
    expect(f.connect).toHaveBeenCalledWith(opts);
    expect(handle).toBeDefined();
  });

  it("未注册 connect 抛错", () => {
    expect(() => mod.connectCollabTransport("nope", fakeOptions())).toThrow("协作传输未注册");
  });

  it("同 name 覆盖注册：后注册者生效", () => {
    const first = fakeFactory("dup");
    const second = fakeFactory("dup");
    mod.registerCollabTransport(first);
    mod.registerCollabTransport(second);
    mod.connectCollabTransport("dup", fakeOptions());
    expect(first.connect).not.toHaveBeenCalled();
    expect(second.connect).toHaveBeenCalledTimes(1);
  });

  it("testCollabTransport 路由到工厂 testConnection", async () => {
    const f = fakeFactory();
    mod.registerCollabTransport(f);
    const result = await mod.testCollabTransport("fake", "ws://x/ws");
    expect(f.testConnection).toHaveBeenCalledWith("ws://x/ws");
    expect(result.ok).toBe(true);
  });

  it("testCollabTransport：未注册/无 testConnection 报错", () => {
    // testCollabTransport 未注册/无 testConnection 均为同步抛错
    expect(() => mod.testCollabTransport("nope", "ws://x/ws")).toThrow("协作传输未注册");
    const noTest: CollabTransportFactory = { name: "notest", connect: vi.fn(() => fakeHandle()) };
    mod.registerCollabTransport(noTest);
    expect(() => mod.testCollabTransport("notest", "ws://x/ws")).toThrow("传输不支持连接测试");
  });

  it("默认 relay 工厂在模块加载时注册（import relay 即可用）", async () => {
    await import("./relay"); // 触发 relay.ts 模块加载时的 registerCollabTransport
    const handle = mod.connectCollabTransport("relay", fakeOptions());
    expect(handle).toBeDefined();
    handle.disconnect();
  });
});
