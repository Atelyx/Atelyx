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

describe("重连/拆卸钩子按注册序运行", () => {
  it("runCollabReconnects 按注册序逐个调", () => {
    const order: string[] = [];
    host.registerCollabReconnect(() => order.push("first"));
    host.registerCollabReconnect(() => order.push("second"));
    host.runCollabReconnects();
    expect(order).toEqual(["first", "second"]);
  });

  it("runCollabTeardowns 按注册序逐个调", () => {
    const order: string[] = [];
    host.registerCollabTeardown(() => order.push("first"));
    host.registerCollabTeardown(() => order.push("second"));
    host.runCollabTeardowns();
    expect(order).toEqual(["first", "second"]);
  });
});
