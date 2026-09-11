/**
 * 协作传输宿主测试（services/collab/docHost.ts）：传输工厂查表与连接替换、入站频道消息
 * 路由到领域注册表、出站咽喉断开时静默丢弃、重同步提示透传、连通性测试透传。
 * 文档实例生命周期归各领域服务自持（如 noteDoc），本模块只做句柄与路由。
 */
import { describe, it, expect, beforeEach, vi } from "vitest";
import type { CollabTransportFactory, CollabTransportOptions } from "./transport";

type DocHostMod = typeof import("./docHost");
type TransportMod = typeof import("./transport");
type CollabHostMod = typeof import("@/utils/collabHost");

let host: DocHostMod;
let transport: TransportMod;
let collabHost: CollabHostMod;

const HELLO = { vaultId: "v", nickname: "昵称", color: "#000000", deviceName: "设备" };

/** 补齐连接请求必填回调（本测试只关心入站路由与出站咽喉）。 */
function connectRequest(
  name: string,
  onResync: () => void = () => {},
): Parameters<DocHostMod["connectTransport"]>[0] {
  return {
    name,
    url: "ws://x/ws",
    hello: HELLO,
    onHelloAck: () => {},
    onPeers: () => {},
    onPeerPresence: () => {},
    onResync,
    onServerError: () => {},
    onStatusChange: () => {},
  };
}

/** 假传输：记录连接参数与出站调用（含调用顺序），暴露入站回调抓手。 */
function fakeFactory(name: string) {
  const calls = {
    sendMessage: [] as Array<[string, string, unknown]>,
    sendPresence: 0,
    sendBye: 0,
    disconnect: 0,
    order: [] as string[],
  };
  let options: CollabTransportOptions | null = null;
  const factory: CollabTransportFactory = {
    name,
    connect: (opts) => {
      options = opts;
      return {
        sendPresence: () => {
          calls.sendPresence += 1;
        },
        sendMessage: (channel, file, payload) => {
          calls.sendMessage.push([channel, file, payload]);
        },
        sendBye: () => {
          calls.sendBye += 1;
          calls.order.push("bye");
        },
        disconnect: () => {
          calls.disconnect += 1;
          calls.order.push("disconnect");
        },
      };
    },
    testConnection: async (url) => ({ ok: true, message: `已连接 ${url}` }),
  };
  return { factory, calls, options: () => options };
}

beforeEach(async () => {
  vi.resetModules();
  transport = await import("./transport");
  host = await import("./docHost");
  collabHost = await import("@/utils/collabHost");
});

describe("连接与路由", () => {
  it("未注册传输名：connectTransport 抛错且不触达任何传输工厂", () => {
    const stray = fakeFactory("stray");
    transport.registerCollabTransport(stray.factory);
    expect(() => host.connectTransport(connectRequest("nope"))).toThrow("协作传输未注册：nope");
    // 出站咽喉不得退回既有/其它句柄：帧必须真的没有发出
    host.sendTransportMessage("note-sync", "a.md", "payload");
    expect(stray.calls.sendMessage).toEqual([]);
  });

  it("已注册传输：入站频道消息按注册表分发（channel/peerId/file/payload 原样）", () => {
    const fake = fakeFactory("fake");
    transport.registerCollabTransport(fake.factory);
    const received: Array<[number, string, unknown]> = [];
    collabHost.registerCollabChannel("note-sync", (_peerId, _file, _payload) => {
      received.push([_peerId, _file, _payload]);
    });

    host.connectTransport(connectRequest("fake"));
    fake.options()!.onChannelMessage(7, "note-sync", "notes/a.md", { patch: 1 });

    expect(received).toEqual([[7, "notes/a.md", { patch: 1 }]]);
  });

  it("传输报告接收队列过慢：onResync 透传到调用方", () => {
    const fake = fakeFactory("fake");
    transport.registerCollabTransport(fake.factory);
    let resyncs = 0;
    host.connectTransport(
      connectRequest("fake", () => {
        resyncs += 1;
      }),
    );
    fake.options()!.onResync();
    expect(resyncs).toBe(1);
  });

  it("重复连接：旧句柄先 bye + disconnect，出站改走新句柄", () => {
    const first = fakeFactory("first");
    const second = fakeFactory("second");
    transport.registerCollabTransport(first.factory);
    transport.registerCollabTransport(second.factory);

    host.connectTransport(connectRequest("first"));
    host.sendTransportMessage("note-sync", "a.md", "one");
    host.connectTransport(connectRequest("second"));
    host.sendTransportMessage("note-sync", "a.md", "two");

    expect(first.calls.sendBye).toBe(1);
    expect(first.calls.disconnect).toBe(1);
    // 先 bye（让中转立即踢出，否则旧 peer 要等 30s 心跳超时才消失）再断开
    expect(first.calls.order).toEqual(["bye", "disconnect"]);
    expect(first.calls.sendMessage).toEqual([["note-sync", "a.md", "one"]]);
    expect(second.calls.sendMessage).toEqual([["note-sync", "a.md", "two"]]);
  });

  it("断开后出站静默丢弃（不再触达旧句柄）", () => {
    const fake = fakeFactory("fake");
    transport.registerCollabTransport(fake.factory);
    host.connectTransport(connectRequest("fake"));
    host.sendTransportMessage("note-sync", "a.md", "before");
    host.disconnectTransport();
    host.sendTransportMessage("note-sync", "a.md", "after");
    host.sendTransportPresence({ file: null, selection: null, view: null });

    expect(fake.calls.sendBye).toBe(1);
    expect(fake.calls.disconnect).toBe(1);
    expect(fake.calls.sendMessage).toEqual([["note-sync", "a.md", "before"]]);
    expect(fake.calls.sendPresence).toBe(0);
  });

  it("presence 与连通性测试按名透传", async () => {
    const fake = fakeFactory("fake");
    transport.registerCollabTransport(fake.factory);
    host.connectTransport(connectRequest("fake"));
    host.sendTransportPresence({ file: "a.md", selection: null, view: "note" });
    expect(fake.calls.sendPresence).toBe(1);
    await expect(host.testTransport("fake", "ws://x/ws")).resolves.toEqual({
      ok: true,
      message: "已连接 ws://x/ws",
    });
  });
});
