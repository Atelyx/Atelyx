/**
 * canvas 能力（当前画布：读快照 + 节点/边写操作）桥路由测试（services/plugins/bridge 注册的宿主 `canvas` 命名空间）。
 *
 * 覆盖——方法路由与参数透传、参数类型错误/未知方法、未接线错误、审计、流式自动收尾。
 * store 数据源经 `setPluginCanvasAccess` 注入假实现（bridge 不 import store），只验证桥的路由与校验。
 */
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { runInNewContext } from "node:vm";
import type { PluginCanvasSnapshot, PluginManifest, PluginType } from "@/types";
import type { PluginTransport } from "./worker";
import { buildProxySource } from "./worker";
import {
  attachPlugin,
  hostCapabilityNames,
  runtimeSnapshot,
  setPluginCanvasAccess,
  unloadPlugin,
  type PluginCanvasAccess,
} from "./bridge";

/** 最小传输 mock：记录 post、广播 onMessage（测试可注入消息）。 */
class FakeTransport implements PluginTransport {
  posted: unknown[] = [];
  private handlers: Array<(m: unknown) => void> = [];
  post(message: unknown): void {
    this.posted.push(message);
  }
  onMessage(handler: (m: unknown) => void): () => void {
    this.handlers.push(handler);
    return () => {
      this.handlers = this.handlers.filter((h) => h !== handler);
    };
  }
  receive(message: unknown): void {
    for (const h of this.handlers) h(message);
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

function spawnPlugin(id: string): Spawned {
  const transport = new FakeTransport();
  attachPlugin(manifest(id), transport);
  spawned.push({ id, transport });
  return { id, transport };
}

const tick = (): Promise<void> => new Promise((r) => setTimeout(r, 0));

/** 假画布访问：各方法 vi.fn，默认返回可辨识值。 */
function makeAccess(): PluginCanvasAccess {
  return {
    snapshot: vi.fn(() => ({ canvasFile: "a.atlx", canvasTitle: "画布", nodes: [], edges: [], selectedNodeId: null })),
    addNode: vi.fn(() => "node-1"),
    updateNode: vi.fn(),
    moveNode: vi.fn(),
    deleteNode: vi.fn(),
    addEdge: vi.fn(() => "edge-1"),
    deleteEdge: vi.fn(),
    selectNode: vi.fn(),
  };
}

beforeEach(() => {
  vi.resetAllMocks();
  setPluginCanvasAccess(makeAccess());
});

afterEach(() => {
  setPluginCanvasAccess(null);
  for (const s of spawned) unloadPlugin(s.id);
  spawned.length = 0;
});

describe("canvas 能力路由", () => {
  it("snapshot：透传并回包", async () => {
    const snap: PluginCanvasSnapshot = {
      canvasFile: "a.atlx",
      canvasTitle: "画布",
      nodes: [],
      edges: [],
      selectedNodeId: null,
    };
    const access = makeAccess();
    access.snapshot = vi.fn(() => snap);
    setPluginCanvasAccess(access);
    const b = spawnPlugin("com.test.cv1");
    b.transport.receive({ kind: "call", seq: 1, method: "call", args: ["canvas", "snapshot", []] });
    await tick();
    expect(access.snapshot).toHaveBeenCalledWith();
    expect(b.transport.posted).toContainEqual({ kind: "reply", seq: 1, ok: true, result: snap });
  });

  it("addNode：参数透传并回传创建的节点 id", async () => {
    const access = makeAccess();
    setPluginCanvasAccess(access);
    const b = spawnPlugin("com.test.cv2");
    b.transport.receive({
      kind: "call",
      seq: 2,
      method: "call",
      args: ["canvas", "addNode", [{ type: "text", position: { x: 1, y: 2 }, data: { title: "t" } }]],
    });
    await tick();
    expect(access.addNode).toHaveBeenCalledWith({ type: "text", position: { x: 1, y: 2 }, data: { title: "t" } });
    expect(b.transport.posted).toContainEqual({ kind: "reply", seq: 2, ok: true, result: "node-1" });
  });

  it("updateNode / moveNode / deleteNode：参数透传", async () => {
    const access = makeAccess();
    setPluginCanvasAccess(access);
    const b = spawnPlugin("com.test.cv3");
    b.transport.receive({
      kind: "call",
      seq: 3,
      method: "call",
      args: ["canvas", "updateNode", ["n1", { title: "x" }]],
    });
    await tick();
    expect(access.updateNode).toHaveBeenCalledWith("n1", { title: "x" });
    b.transport.receive({ kind: "call", seq: 4, method: "call", args: ["canvas", "moveNode", ["n1", { x: 9, y: 8 }]] });
    await tick();
    expect(access.moveNode).toHaveBeenCalledWith("n1", { x: 9, y: 8 });
    b.transport.receive({ kind: "call", seq: 5, method: "call", args: ["canvas", "deleteNode", ["n1"]] });
    await tick();
    expect(access.deleteNode).toHaveBeenCalledWith("n1");
  });

  it("addEdge：参数透传并回传创建的边 id；deleteEdge / selectNode 透传", async () => {
    const access = makeAccess();
    setPluginCanvasAccess(access);
    const b = spawnPlugin("com.test.cv4");
    b.transport.receive({
      kind: "call",
      seq: 6,
      method: "call",
      args: ["canvas", "addEdge", [{ source: "a", target: "b", directed: true }]],
    });
    await tick();
    expect(access.addEdge).toHaveBeenCalledWith({ source: "a", target: "b", directed: true });
    expect(b.transport.posted).toContainEqual({ kind: "reply", seq: 6, ok: true, result: "edge-1" });
    b.transport.receive({ kind: "call", seq: 7, method: "call", args: ["canvas", "deleteEdge", ["e1"]] });
    await tick();
    expect(access.deleteEdge).toHaveBeenCalledWith("e1");
    b.transport.receive({ kind: "call", seq: 8, method: "call", args: ["canvas", "selectNode", ["n1"]] });
    await tick();
    expect(access.selectNode).toHaveBeenCalledWith("n1");
  });

  it("参数类型错误与未知方法：友好 error reply", async () => {
    const access = makeAccess();
    setPluginCanvasAccess(access);
    const b = spawnPlugin("com.test.cv5");
    b.transport.receive({ kind: "call", seq: 10, method: "call", args: ["canvas", "addNode", [{ type: "text" }]] });
    await tick();
    expect(b.transport.posted).toContainEqual({
      kind: "reply",
      seq: 10,
      ok: false,
      error: "canvas.addNode 需要 { type, position, data? }",
    });
    b.transport.receive({ kind: "call", seq: 11, method: "call", args: ["canvas", "updateNode", ["n1", "not-object"]] });
    await tick();
    expect(b.transport.posted).toContainEqual({
      kind: "reply",
      seq: 11,
      ok: false,
      error: "canvas.updateNode 需要 data 补丁对象",
    });
    b.transport.receive({ kind: "call", seq: 12, method: "call", args: ["canvas", "nonexistent", []] });
    await tick();
    expect(b.transport.posted).toContainEqual({ kind: "reply", seq: 12, ok: false, error: "canvas 无方法 nonexistent" });
  });

  it("未接线：友好 error reply", async () => {
    setPluginCanvasAccess(null);
    const b = spawnPlugin("com.test.cv6");
    b.transport.receive({ kind: "call", seq: 13, method: "call", args: ["canvas", "snapshot", []] });
    await tick();
    expect(b.transport.posted).toContainEqual({ kind: "reply", seq: 13, ok: false, error: "画布能力未就绪" });
  });

  it("审计记录 canvas 命名空间；注册表含 canvas", async () => {
    const b = spawnPlugin("com.test.cv7");
    b.transport.receive({ kind: "call", seq: 14, method: "call", args: ["canvas", "snapshot", []] });
    await tick();
    expect(runtimeSnapshot().find((e) => e.id === "com.test.cv7")?.used).toContain("canvas");
    expect(hostCapabilityNames()).toContain("canvas");
  });

  it("流式调用非流式方法：分发器补单个 end(result)", async () => {
    const b = spawnPlugin("com.test.cv8");
    b.transport.receive({
      kind: "call",
      seq: 15,
      method: "call",
      args: ["canvas", "snapshot", [], { stream: true }],
    });
    await tick();
    const frames = b.transport.posted.filter((m) => (m as { kind?: string }).kind === "stream") as Array<{
      event: string;
      data?: unknown;
    }>;
    expect(frames.map((f) => f.event)).toEqual(["end"]);
  });
});

describe("端到端回环（代理源 ↔ 真实桥 ↔ 注入画布访问）", () => {
  it("插件顶层 bridge.call(\"canvas\", \"snapshot\") 全链路返回", async () => {
    const snap: PluginCanvasSnapshot = {
      canvasFile: "a.atlx",
      canvasTitle: "画布",
      nodes: [{ id: "n1", type: "text", x: 0, y: 0, data: { title: "t" } }],
      edges: [],
      selectedNodeId: null,
    };
    const access = makeAccess();
    access.snapshot = vi.fn(() => snap);
    setPluginCanvasAccess(access);
    const holder: { host: ((m: unknown) => void) | null } = { host: null };
    const sandbox: Record<string, unknown> = {};
    let toPlugin: ((m: unknown) => void) | null = null;
    sandbox.addEventListener = (_t: string, h: (e: { data: unknown }) => void) => {
      toPlugin = (m: unknown) => h({ data: m });
    };
    sandbox.postMessage = (m: unknown) => {
      holder.host?.(m);
    };
    sandbox.self = sandbox;
    sandbox.Promise = Promise;
    sandbox.setTimeout = setTimeout;
    sandbox.console = console;
    const transport: PluginTransport = {
      post: (m) => toPlugin?.(m),
      onMessage: (h) => {
        holder.host = h;
        return () => {
          holder.host = null;
        };
      },
      dispose: () => {},
    };
    attachPlugin(manifest("com.test.cv-e2e"), transport);
    try {
      const pluginCode = `
        bridge.call("canvas", "snapshot", []).then(function(r){ self.__result = r; },
          function(e){ self.__error = (e && e.message) || String(e); });
      `;
      runInNewContext(`${buildProxySource()}\n;\n${pluginCode}`, sandbox);
      await tick();
      await tick();
      expect(access.snapshot).toHaveBeenCalledWith();
      expect(sandbox.__result).toEqual(snap);
      expect(sandbox.__error).toBeUndefined();
    } finally {
      unloadPlugin("com.test.cv-e2e");
    }
  });
});
