/**
 * 协作域接线注册表纯函数测试（utils/collabHost.ts）。
 * 覆盖通道注册/覆盖/未注册静默丢弃与分发传参、presence provider 依次合并、
 * 重连/拆卸钩子按注册序运行。注册表为模块级单例，每测试 vi.resetModules 隔离。
 */
import { describe, it, expect, beforeEach, vi } from "vitest";
import type { CollabPresence } from "@/types";

type CollabHost = typeof import("./collabHost");
let host: CollabHost;

beforeEach(async () => {
  vi.resetModules();
  host = await import("./collabHost");
});

describe("通道注册与分发", () => {
  it("注册后分发：handler 收到 peerId/file/payload", () => {
    const calls: unknown[] = [];
    host.registerCollabChannel("note-sync", (peerId, file, payload) => {
      calls.push([peerId, file, payload]);
    });
    host.dispatchCollabChannel("note-sync", 7, "a.md", "base64");
    expect(calls).toEqual([[7, "a.md", "base64"]]);
  });

  it("未注册通道静默丢弃（不抛错）", () => {
    expect(() => host.dispatchCollabChannel("unknown", 1, "a.md", "x")).not.toThrow();
  });

  it("同通道覆盖注册：后注册者生效", () => {
    const first = vi.fn();
    const second = vi.fn();
    host.registerCollabChannel("canvas-patch", first);
    host.registerCollabChannel("canvas-patch", second);
    host.dispatchCollabChannel("canvas-patch", 1, "c.atlx", {});
    expect(first).not.toHaveBeenCalled();
    expect(second).toHaveBeenCalledTimes(1);
  });
});

describe("presence 合并", () => {
  const base: CollabPresence = { file: "a.md", selection: null, view: "note" };

  it("无 provider：原样返回（同引用）", () => {
    expect(host.mergeCollabPresence(base)).toBe(base);
  });

  it("provider 按注册序依次合并，基础字段保留", () => {
    host.registerCollabPresenceProvider((p) => ({ ...p, lockedNodes: [{ id: "n1", since: 1 }] }));
    host.registerCollabPresenceProvider((p) => ({ ...p, streamingNodeIds: ["n2"] }));
    const merged = host.mergeCollabPresence(base);
    expect(merged.file).toBe("a.md");
    expect(merged.view).toBe("note");
    expect(merged.lockedNodes).toEqual([{ id: "n1", since: 1 }]);
    expect(merged.streamingNodeIds).toEqual(["n2"]);
    expect(base.lockedNodes).toBeUndefined(); // 不污染入参
  });

  it("provider 可为空合并（增量语义：只加自己的字段）", () => {
    host.registerCollabPresenceProvider((p) => p);
    expect(host.mergeCollabPresence(base)).toBe(base);
  });
});

describe("重连/拆卸钩子按序运行", () => {
  it("runCollabReconnects 按 priority 升序（同值按注册序）", () => {
    const order: string[] = [];
    host.registerCollabReconnect(() => order.push("p0"));
    host.registerCollabReconnect(() => order.push("p1"), 1);
    host.registerCollabReconnect(() => order.push("p0b"));
    host.runCollabReconnects();
    expect(order).toEqual(["p0", "p0b", "p1"]);
  });

  it("runCollabTeardowns 按 priority 升序（同值按注册序）", () => {
    const order: string[] = [];
    host.registerCollabTeardown(() => order.push("p1"), 1);
    host.registerCollabTeardown(() => order.push("p0"));
    host.registerCollabTeardown(() => order.push("p2"), 2);
    host.runCollabTeardowns();
    expect(order).toEqual(["p0", "p1", "p2"]);
  });

  it("presence provider 按 priority 升序合并", () => {
    const order: string[] = [];
    host.registerCollabPresenceProvider((p) => {
      order.push("p1");
      return p;
    }, 1);
    host.registerCollabPresenceProvider((p) => {
      order.push("p0");
      return p;
    });
    host.mergeCollabPresence({ file: "a.md", selection: null, view: "note" });
    expect(order).toEqual(["p0", "p1"]);
  });
});

describe("撤销（随插件启停）", () => {
  it("撤销通道注册后不再分发；撤销幂等", () => {
    const fn = vi.fn();
    const off = host.registerCollabChannel("table-patch", fn);
    off();
    off();
    host.dispatchCollabChannel("table-patch", 1, "t.atb", {});
    expect(fn).not.toHaveBeenCalled();
  });

  it("撤销重连/拆卸钩子后不再运行", () => {
    const order: string[] = [];
    const off1 = host.registerCollabReconnect(() => order.push("r1"));
    const off2 = host.registerCollabReconnect(() => order.push("r2"));
    off1();
    host.runCollabReconnects();
    expect(order).toEqual(["r2"]);
    order.length = 0;
    const off3 = host.registerCollabTeardown(() => order.push("t1"));
    const off4 = host.registerCollabTeardown(() => order.push("t2"));
    off4();
    host.runCollabTeardowns();
    expect(order).toEqual(["t1"]);
    off2();
    off3();
  });

  it("撤销 presence provider 后不再合并", () => {
    const fn = vi.fn((p: CollabPresence) => p);
    const off = host.registerCollabPresenceProvider(fn);
    off();
    host.mergeCollabPresence({ file: "a.md", selection: null, view: "note" });
    expect(fn).not.toHaveBeenCalled();
  });

  it("同 priority 下撤销不扰乱其余钩子顺序（按引用守卫）", () => {
    const order: string[] = [];
    const off1 = host.registerCollabReconnect(() => order.push("a"));
    host.registerCollabReconnect(() => order.push("b"));
    off1();
    host.runCollabReconnects();
    expect(order).toEqual(["b"]);
  });
});
