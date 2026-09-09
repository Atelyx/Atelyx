/**
 * 桥宿主能力注册表测试（services/plugins/bridge）。
 *
 * 用最小 Worker mock 驱动消息流：loadPlugin 后直接向 onmessage 注入插件的 call/reply/stream 帧，
 * 验证——宿主命名空间路由、插件命名空间跨插件中转、流式双向转发、命名空间冲突拒绝、贡献注册与清理。
 * 不执行真实 blob 代理源码（无 DOM/worker 全局），只验证宿主侧路由逻辑。
 */
import { describe, it, expect, afterEach } from "vitest";
import type { PluginManifest, PluginType } from "@/types";
import type { PluginTransport } from "./worker";
import {
  attachPlugin,
  callPluginContributionFn,
  emitPluginEvent,
  getPluginCanvasAccess,
  getPluginCollabAccess,
  getPluginTableRuntimeAccess,
  getPluginVaultWriteAccess,
  getSettingsAccess,
  listPluginContributions,
  pluginCapabilitiesByOwner,
  pluginCapabilityOwner,
  registerHostCapability,
  runtimeSnapshot,
  setPluginCanvasAccess,
  setPluginCollabAccess,
  setPluginEventForwarder,
  setPluginTableRuntimeAccess,
  setPluginVaultWriteAccess,
  setSettingsAccess,
  unloadPlugin,
} from "./bridge";

/** 最小传输 mock：记录 post、广播 onMessage（测试可注入消息）、可触发 onCrash。 */
class FakeTransport implements PluginTransport {
  posted: unknown[] = [];
  private handlers: Array<(m: unknown) => void> = [];
  private crashHandlers: Array<(m: string) => void> = [];
  post(message: unknown): void {
    this.posted.push(message);
  }
  onMessage(handler: (m: unknown) => void): () => void {
    this.handlers.push(handler);
    return () => {
      this.handlers = this.handlers.filter((h) => h !== handler);
    };
  }
  onCrash(handler: (m: string) => void): () => void {
    this.crashHandlers.push(handler);
    return () => {
      this.crashHandlers = this.crashHandlers.filter((h) => h !== handler);
    };
  }
  receive(message: unknown): void {
    for (const h of this.handlers) h(message);
  }
  crash(message: string): void {
    for (const h of this.crashHandlers) h(message);
  }
  dispose(): void {}
}

interface Spawned {
  id: string;
  transport: FakeTransport;
}
const spawned: Spawned[] = [];

const manifest = (id: string): PluginManifest => ({
  schemaVersion: 2,
  id,
  name: id,
  version: "1.0.0",
  type: "tool" as PluginType,
  main: "plugin.js",
});

/** 以 fake transport 接入桥并返回，用于注入消息。 */
function spawnPlugin(id: string): { entryId: string; transport: FakeTransport } {
  const transport = new FakeTransport();
  attachPlugin(manifest(id), transport);
  spawned.push({ id, transport });
  return { entryId: id, transport };
}

const tick = (): Promise<void> => new Promise((r) => setTimeout(r, 0));

afterEach(() => {
  for (const s of spawned) unloadPlugin(s.id);
  spawned.length = 0;
});

describe("能力注册表路由", () => {
  it("插件注册反向域名命名空间；冲突（同名/宿主保留/无点）拒绝", async () => {
    const a = spawnPlugin("com.test.a");
    a.transport.receive({
      kind: "call",
      seq: 1,
      method: "registerCapability",
      args: [{ namespace: "com.a.db", methodIds: { query: "f1", insert: "f2" } }],
    });
    await tick();
    expect(pluginCapabilityOwner("com.a.db")).toBe("com.test.a");
    expect(pluginCapabilitiesByOwner("com.test.a")).toEqual(["com.a.db"]);
    expect(a.transport.posted).toContainEqual({ kind: "reply", seq: 1, ok: true, result: true });

    // 同名冲突
    const b = spawnPlugin("com.test.b");
    b.transport.receive({
      kind: "call",
      seq: 1,
      method: "registerCapability",
      args: [{ namespace: "com.a.db", methodIds: { x: "f3" } }],
    });
    await tick();
    expect(b.transport.posted).toContainEqual({
      kind: "reply",
      seq: 1,
      ok: false,
      error: "命名空间 com.a.db 已被插件 com.test.a 占用",
    });

    // 宿主命名空间保留 + 无点拒绝
    b.transport.receive({
      kind: "call",
      seq: 2,
      method: "registerCapability",
      args: [{ namespace: "state", methodIds: { x: "f3" } }],
    });
    await tick();
    expect(b.transport.posted).toContainEqual({ kind: "reply", seq: 2, ok: false, error: "命名空间 state 为宿主保留" });
    b.transport.receive({
      kind: "call",
      seq: 3,
      method: "registerCapability",
      args: [{ namespace: "nodot", methodIds: { x: "f3" } }],
    });
    await tick();
    expect(b.transport.posted.some((m) => (m as { ok?: boolean }).ok === false)).toBe(true);
  });

  it("宿主命名空间调用：经 registerHostCapability 处理器执行并回包", async () => {
    registerHostCapability("testhost", async (method, args, ctx) => ({
      method,
      pluginId: ctx.pluginId,
      arg: args[0],
    }));
    const b = spawnPlugin("com.test.b");
    b.transport.receive({ kind: "call", seq: 5, method: "call", args: ["testhost", "ping", ["x"]] });
    await tick();
    expect(b.transport.posted).toContainEqual({
      kind: "reply",
      seq: 5,
      ok: true,
      result: { method: "ping", pluginId: "com.test.b", arg: "x" },
    });
  });

  it("未知能力报错", async () => {
    const b = spawnPlugin("com.test.b");
    b.transport.receive({ kind: "call", seq: 6, method: "call", args: ["nope.method", "query", []] });
    await tick();
    expect(b.transport.posted).toContainEqual({ kind: "reply", seq: 6, ok: false, error: "能力 nope.method 不存在" });
  });
});

describe("跨插件调用与流式", () => {
  it("非流式跨插件调用：宿主向提供者 invoke，回包转发给调用方", async () => {
    const a = spawnPlugin("com.test.a");
    a.transport.receive({
      kind: "call",
      seq: 1,
      method: "registerCapability",
      args: [{ namespace: "com.a.db", methodIds: { query: "f1" } }],
    });
    await tick();

    const b = spawnPlugin("com.test.b");
    b.transport.receive({ kind: "call", seq: 10, method: "call", args: ["com.a.db", "query", [42]] });
    await tick();
    // 宿主向 A 发 invoke（含 fnId f1）
    const invoke = a.transport.posted.find((m) => (m as { kind?: string }).kind === "invoke") as {
      seq: number;
      fnId: string;
      args: unknown[];
    };
    expect(invoke.fnId).toBe("f1");
    expect(invoke.args).toEqual([42]);
    // 模拟 A 的 reply
    a.transport.receive({ kind: "reply", seq: invoke.seq, ok: true, result: "done" });
    await tick();
    expect(b.transport.posted).toContainEqual({ kind: "reply", seq: 10, ok: true, result: "done" });
  });

  it("流式跨插件调用：提供者 chunk/end 帧经宿主转发给调用方（seq 映射）", async () => {
    const a = spawnPlugin("com.test.a");
    a.transport.receive({
      kind: "call",
      seq: 1,
      method: "registerCapability",
      args: [{ namespace: "com.a.stream", methodIds: { pull: "f1" } }],
    });
    await tick();

    const b = spawnPlugin("com.test.b");
    b.transport.receive({
      kind: "call",
      seq: 20,
      method: "call",
      args: ["com.a.stream", "pull", [], { stream: true }],
    });
    await tick();
    const invoke = a.transport.posted.find((m) => (m as { kind?: string }).kind === "invoke") as {
      seq: number;
      stream?: boolean;
    };
    expect(invoke.stream).toBe(true);
    // 模拟 A 推 chunk → end
    a.transport.receive({ kind: "stream", seq: invoke.seq, event: "chunk", data: "x1" });
    a.transport.receive({ kind: "stream", seq: invoke.seq, event: "chunk", data: "x2" });
    a.transport.receive({ kind: "stream", seq: invoke.seq, event: "end", data: "done" });
    await tick();
    expect(b.transport.posted).toContainEqual({ kind: "stream", seq: 20, event: "chunk", data: "x1" });
    expect(b.transport.posted).toContainEqual({ kind: "stream", seq: 20, event: "chunk", data: "x2" });
    expect(b.transport.posted).toContainEqual({ kind: "stream", seq: 20, event: "end", data: "done" });
  });
});

describe("通用扩展点注册", () => {
  it("registerContribution 收集/查询，卸载后清除", async () => {
    const a = spawnPlugin("com.test.a");
    a.transport.receive({
      kind: "call",
      seq: 1,
      method: "registerContribution",
      args: [{ point: "toolbar:button", id: "b1", payload: { label: "hi" } }],
    });
    await tick();
    expect(listPluginContributions("toolbar:button")).toHaveLength(1);
    expect(listPluginContributions("toolbar:button")[0]).toMatchObject({
      pluginId: "com.test.a",
      point: "toolbar:button",
      id: "b1",
      payload: { label: "hi" },
    });
    unloadPlugin("com.test.a");
    expect(listPluginContributions("toolbar:button")).toHaveLength(0);
  });
});

describe("生命周期清理", () => {
  it("卸载清空能力与贡献，运行时快照移除条目", async () => {
    const a = spawnPlugin("com.test.a");
    a.transport.receive({
      kind: "call",
      seq: 1,
      method: "registerCapability",
      args: [{ namespace: "com.a.db", methodIds: { query: "f1" } }],
    });
    await tick();
    expect(runtimeSnapshot().map((e) => e.id)).toContain("com.test.a");
    unloadPlugin("com.test.a");
    expect(runtimeSnapshot().map((e) => e.id)).not.toContain("com.test.a");
    expect(pluginCapabilityOwner("com.a.db")).toBeUndefined();
  });
});

describe("贡献载荷函数调用", () => {
  it("callPluginContributionFn 对已卸载插件报错", async () => {
    await expect(callPluginContributionFn("com.test.a", { $fn: "f1" }, [])).rejects.toThrow("插件未运行");
  });
});

describe("流式收尾契约（高危路径）", () => {
  it("宿主流式：handler 自行 end 后分发器不重复补 end", async () => {
    registerHostCapability("teststream", async (_m, _a, ctx) => {
      const s = ctx.stream!;
      s.chunk("a");
      s.end("done");
      return undefined;
    });
    const b = spawnPlugin("com.test.b");
    b.transport.receive({ kind: "call", seq: 30, method: "call", args: ["teststream", "pull", [], { stream: true }] });
    await tick();
    const frames = b.transport.posted.filter((m) => (m as { kind?: string }).kind === "stream") as Array<{
      event: string;
      data?: unknown;
    }>;
    expect(frames.map((f) => f.event)).toEqual(["chunk", "end"]);
    expect(frames[1].data).toBe("done");
  });

  it("宿主流式：非流式 handler 被流式调用时补单个 end(result)", async () => {
    registerHostCapability("testplain", async () => "result-x");
    const b = spawnPlugin("com.test.b");
    b.transport.receive({ kind: "call", seq: 31, method: "call", args: ["testplain", "get", [], { stream: true }] });
    await tick();
    const frames = b.transport.posted.filter((m) => (m as { kind?: string }).kind === "stream") as Array<{
      event: string;
      data?: unknown;
    }>;
    expect(frames.map((f) => f.event)).toEqual(["end"]);
    expect(frames[0].data).toBe("result-x");
  });

  it("流式调用出错发 stream error 帧（不挂起）", async () => {
    const b = spawnPlugin("com.test.b");
    b.transport.receive({ kind: "call", seq: 32, method: "call", args: ["nope.method", "get", [], { stream: true }] });
    await tick();
    expect(b.transport.posted).toContainEqual({
      kind: "stream",
      seq: 32,
      event: "error",
      data: "能力 nope.method 不存在",
    });
  });

  it("跨插件流式：提供方未调 end 时尾随 reply 转发为 end", async () => {
    const a = spawnPlugin("com.test.a");
    a.transport.receive({
      kind: "call",
      seq: 1,
      method: "registerCapability",
      args: [{ namespace: "com.a.plain", methodIds: { get: "f1" } }],
    });
    await tick();
    const b = spawnPlugin("com.test.b");
    b.transport.receive({ kind: "call", seq: 40, method: "call", args: ["com.a.plain", "get", [], { stream: true }] });
    await tick();
    const invoke = a.transport.posted.find((m) => (m as { kind?: string }).kind === "invoke") as { seq: number };
    // 提供方只回 reply（未走 stream 帧）：宿主应转发为 end，调用方不挂起。
    a.transport.receive({ kind: "reply", seq: invoke.seq, ok: true, result: "plain-result" });
    await tick();
    expect(b.transport.posted).toContainEqual({ kind: "stream", seq: 40, event: "end", data: "plain-result" });
  });

  it("跨插件流式：提供方卸载后调用方收到 error 帧", async () => {
    const a = spawnPlugin("com.test.a");
    a.transport.receive({
      kind: "call",
      seq: 1,
      method: "registerCapability",
      args: [{ namespace: "com.a.stream2", methodIds: { pull: "f1" } }],
    });
    await tick();
    const b = spawnPlugin("com.test.b");
    b.transport.receive({ kind: "call", seq: 41, method: "call", args: ["com.a.stream2", "pull", [], { stream: true }] });
    await tick();
    unloadPlugin("com.test.a");
    await tick();
    expect(
      b.transport.posted.some(
        (m) => (m as { kind?: string }).kind === "stream" && (m as { event?: string }).event === "error",
      ),
    ).toBe(true);
  });

  it("跨插件流式：两个提供方并发流式（invokeSeq 相同）经复合键不错发", async () => {
    const a = spawnPlugin("com.test.a");
    a.transport.receive({
      kind: "call",
      seq: 1,
      method: "registerCapability",
      args: [{ namespace: "com.a.sa", methodIds: { pull: "f1" } }],
    });
    const b = spawnPlugin("com.test.b");
    b.transport.receive({
      kind: "call",
      seq: 1,
      method: "registerCapability",
      args: [{ namespace: "com.b.sb", methodIds: { pull: "f1" } }],
    });
    await tick();
    const ca = spawnPlugin("com.test.ca");
    const cb = spawnPlugin("com.test.cb");
    ca.transport.receive({ kind: "call", seq: 50, method: "call", args: ["com.a.sa", "pull", [], { stream: true }] });
    cb.transport.receive({ kind: "call", seq: 51, method: "call", args: ["com.b.sb", "pull", [], { stream: true }] });
    await tick();
    // 两个提供方各自的 invoke seq 都是 1——复合键之前会全局覆盖错发。
    const invokeA = a.transport.posted.find((m) => (m as { kind?: string }).kind === "invoke") as { seq: number };
    const invokeB = b.transport.posted.find((m) => (m as { kind?: string }).kind === "invoke") as { seq: number };
    expect(invokeA.seq).toBe(1);
    expect(invokeB.seq).toBe(1);
    // A 的 chunk 只到 ca，B 的 chunk 只到 cb。
    a.transport.receive({ kind: "stream", seq: invokeA.seq, event: "chunk", data: "A1" });
    b.transport.receive({ kind: "stream", seq: invokeB.seq, event: "chunk", data: "B1" });
    await tick();
    expect(ca.transport.posted).toContainEqual({ kind: "stream", seq: 50, event: "chunk", data: "A1" });
    expect(
      ca.transport.posted.some((m) => (m as { kind?: string }).kind === "stream" && (m as { data?: unknown }).data === "B1"),
    ).toBe(false);
    expect(cb.transport.posted).toContainEqual({ kind: "stream", seq: 51, event: "chunk", data: "B1" });
    expect(
      cb.transport.posted.some((m) => (m as { kind?: string }).kind === "stream" && (m as { data?: unknown }).data === "A1"),
    ).toBe(false);
    // A 的 end 只清 A 的 relay，B 的流不受影响。
    a.transport.receive({ kind: "stream", seq: invokeA.seq, event: "end", data: "doneA" });
    b.transport.receive({ kind: "stream", seq: invokeB.seq, event: "chunk", data: "B2" });
    b.transport.receive({ kind: "stream", seq: invokeB.seq, event: "end", data: "doneB" });
    await tick();
    expect(ca.transport.posted).toContainEqual({ kind: "stream", seq: 50, event: "end", data: "doneA" });
    expect(cb.transport.posted).toContainEqual({ kind: "stream", seq: 51, event: "chunk", data: "B2" });
    expect(cb.transport.posted).toContainEqual({ kind: "stream", seq: 51, event: "end", data: "doneB" });
  });
});

describe("审计（命名空间）", () => {
  it("call 记录宿主与插件命名空间；registerTool 记录 ai", async () => {
    registerHostCapability("testaudit", async () => "ok");
    const a = spawnPlugin("com.test.a");
    a.transport.receive({
      kind: "call",
      seq: 1,
      method: "registerCapability",
      args: [{ namespace: "com.a.x", methodIds: { m: "f1" } }],
    });
    await tick();
    a.transport.receive({ kind: "call", seq: 2, method: "call", args: ["com.a.x", "m", []] });
    await tick();
    const invoke = a.transport.posted.find((m) => (m as { kind?: string }).kind === "invoke") as { seq: number };
    a.transport.receive({ kind: "reply", seq: invoke.seq, ok: true, result: "x" });
    a.transport.receive({ kind: "call", seq: 3, method: "call", args: ["testaudit", "get", []] });
    a.transport.receive({ kind: "call", seq: 4, method: "registerTool", args: [{ name: "t", executeId: "f2" }] });
    await tick();
    const used = runtimeSnapshot().find((e) => e.id === "com.test.a")?.used ?? [];
    expect(used).toContain("com.a.x");
    expect(used).toContain("testaudit");
    expect(used).toContain("ai");
  });
});

describe("访问读口与事件转发（内核接缝）", () => {
  it("注入的访问对象可经读口取回（单一数据源）", () => {
    const canvas = { snapshot: () => ({ canvasFile: null }) };
    const table = { snapshot: () => ({ tableFile: null }) };
    const collab = { peers: () => [] };
    const vaultWrite = { writeFile: async () => ({ ok: true, summary: "x" }) };
    const ai = () => ({ providers: [] });
    setPluginCanvasAccess(canvas as never);
    setPluginTableRuntimeAccess(table as never);
    setPluginCollabAccess(collab as never);
    setPluginVaultWriteAccess(vaultWrite as never);
    setSettingsAccess(ai as never);
    expect(getPluginCanvasAccess()).toBe(canvas);
    expect(getPluginTableRuntimeAccess()).toBe(table);
    expect(getPluginCollabAccess()).toBe(collab);
    expect(getPluginVaultWriteAccess()).toBe(vaultWrite);
    expect(getSettingsAccess()).toBe(ai);
    setPluginCanvasAccess(null);
    setPluginTableRuntimeAccess(null);
    setPluginCollabAccess(null);
    setPluginVaultWriteAccess(null);
    setSettingsAccess(null);
    expect(getPluginCanvasAccess()).toBeNull();
    expect(getPluginTableRuntimeAccess()).toBeNull();
    expect(getPluginCollabAccess()).toBeNull();
    expect(getPluginVaultWriteAccess()).toBeNull();
    expect(getSettingsAccess()).toBeNull();
  });

  it("事件转发钩子：emitPluginEvent 同步转发且不影响插件投递", async () => {
    const forwarded: Array<{ event: string; payload: unknown }> = [];
    setPluginEventForwarder((event, payload) => forwarded.push({ event, payload }));
    const a = spawnPlugin("com.test.fwd");
    a.transport.receive({ kind: "call", seq: 1, method: "subscribe", args: ["canvas:changed"] });
    await tick();
    emitPluginEvent("canvas:changed", { file: "c.atlx" });
    expect(forwarded).toContainEqual({ event: "canvas:changed", payload: { file: "c.atlx" } });
    expect(a.transport.posted).toContainEqual({ kind: "event", event: "canvas:changed", payload: { file: "c.atlx" } });
    setPluginEventForwarder(null);
    forwarded.length = 0;
    emitPluginEvent("canvas:changed", { file: "d.atlx" });
    expect(forwarded).toEqual([]);
  });
});
