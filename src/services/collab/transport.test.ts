/**
 * 协作传输注册表测试（services/collab/transport.ts）。
 * 覆盖工厂注册/覆盖/未注册报错、connect 路由到对应工厂。
 * 空间工厂的真实连接走 WebSocket（node 环境不直测，帧分发与握手帧序由 spaceTransport.test.ts 覆盖）。
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
  url: "ws://server:11224/ws/space",
  hello: { spaceId: "sp1", token: "tok", nickname: "n", color: "#000", deviceName: "d" },
  onHelloAck: () => {},
  onPeers: () => {},
  onPeerPresence: () => {},
  onChannelMessage: () => {},
  onResync: () => {},
  onServerError: () => {},
  onStatusChange: () => {},
});

function fakeHandle(): CollabTransportHandle {
  return {
    sendPresence: () => {},
    sendMessage: () => true,
    sendBye: () => {},
    disconnect: () => {},
  };
}

function fakeFactory(name = "fake"): CollabTransportFactory & {
  connect: ReturnType<typeof vi.fn>;
} {
  return {
    name,
    connect: vi.fn(() => fakeHandle()),
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

});
