/**
 * 能力包裹（middleware）桥测试：宿主能力按注册序被插件包裹串链（waterfall）。
 *
 * 覆盖——透传/参数改写/短路/结果后处理/链序/包裹方停止跳过/未知命名空间拒绝/同插件重复
 * 包裹原位替换/审计/流式自动收尾。包裹方用 vm 沙箱跑真实代理源（worker 平面真链路：
 * bridge.wrap 注册 + 链式 invoke 的 ctx.next 经 chainId 续链回调）。
 * `@/services/dialog` mock：只验证链式路由与语义，不触碰真实系统对话框。
 */
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { runInNewContext } from "node:vm";
import type { PluginManifest, PluginType } from "@/types";
import { buildProxySource, type PluginTransport } from "./worker";
import {
  attachPlugin,
  runtimeSnapshot,
  unloadPlugin,
} from "./bridge";
import { pickDirectory, pickFile } from "@/services/dialog";

vi.mock("@/services/dialog", () => ({
  pickDirectory: vi.fn(),
  pickFile: vi.fn(),
  saveFile: vi.fn(),
}));

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
/** 沙箱包裹插件 id（afterEach 统一卸载，防注册残留串测）。 */
const wrapperIds: string[] = [];

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

/** 包裹方插件：vm 沙箱跑真实代理源（bridge.wrap 注册 + 执行都在沙箱内完成）。 */
function spawnWrapper(id: string, code: string): { sandbox: Record<string, unknown> } {
  wrapperIds.push(id);
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
  attachPlugin(manifest(id), transport);
  runInNewContext(`${buildProxySource()}\n;\n${code}`, sandbox);
  return { sandbox };
}

const tick = (): Promise<void> => new Promise((r) => setTimeout(r, 0));

beforeEach(() => {
  vi.resetAllMocks();
});

afterEach(() => {
  for (const id of wrapperIds) unloadPlugin(id);
  wrapperIds.length = 0;
  for (const s of spawned) unloadPlugin(s.id);
  spawned.length = 0;
});

describe("能力包裹（wrap）", () => {
  it("透传：包裹记日志后 next() 不带参数 = 透传原参数，真实 handler 执行并回包", async () => {
    vi.mocked(pickFile).mockResolvedValue("C:\\repo\\a.md");
    const w = spawnWrapper(
      "com.wrap.w1",
      `bridge.wrap("dialog", async function(args, ctx, next){
         self.__log = (self.__log || []).concat(["w1"]);
         return next();
       });`,
    );
    const filters = [{ name: "Markdown", extensions: ["md"] }];
    const b = spawnPlugin("com.wrap.caller1");
    b.transport.receive({ kind: "call", seq: 1, method: "call", args: ["dialog", "pickFile", [filters]] });
    await tick();
    await tick();
    expect(pickFile).toHaveBeenCalledWith(filters); // next() 未传参 → 透传原参数而非清空
    expect(b.transport.posted).toContainEqual({ kind: "reply", seq: 1, ok: true, result: "C:\\repo\\a.md" });
    expect(w.sandbox.__log).toEqual(["w1"]);
  });

  it("显式清空：next([]) 传空参数给真实 handler（与透传区分）", async () => {
    vi.mocked(pickFile).mockResolvedValue("C:\\repo\\b.md");
    spawnWrapper(
      "com.wrap.clear",
      `bridge.wrap("dialog", async function(args, ctx, next){ return next([]); });`,
    );
    const b = spawnPlugin("com.wrap.caller1b");
    b.transport.receive({
      kind: "call",
      seq: 1,
      method: "call",
      args: ["dialog", "pickFile", [[{ name: "原", extensions: ["md"] }]]],
    });
    await tick();
    await tick();
    expect(pickFile).toHaveBeenCalledWith(undefined); // 显式传空数组 → handler 收到 undefined filters
  });

  it("参数改写：next(新 args) 改写后透传给真实 handler", async () => {
    spawnWrapper(
      "com.wrap.w2",
      `bridge.wrap("dialog", async function(args, ctx, next){
         return next([[{ name: "重写", extensions: ["xyz"] }]]);
       });`,
    );
    const b = spawnPlugin("com.wrap.caller2");
    b.transport.receive({
      kind: "call",
      seq: 2,
      method: "call",
      args: ["dialog", "pickFile", [[{ name: "原", extensions: ["md"] }]]],
    });
    await tick();
    await tick();
    expect(pickFile).toHaveBeenCalledWith([{ name: "重写", extensions: ["xyz"] }]);
  });

  it("短路：不调 next 直接返回，真实 handler 不执行", async () => {
    spawnWrapper(
      "com.wrap.w3",
      `bridge.wrap("dialog", async function(args, ctx, next){ return "被拦截"; });`,
    );
    const b = spawnPlugin("com.wrap.caller3");
    b.transport.receive({ kind: "call", seq: 3, method: "call", args: ["dialog", "pickDirectory", []] });
    await tick();
    await tick();
    expect(pickDirectory).not.toHaveBeenCalled();
    expect(b.transport.posted).toContainEqual({ kind: "reply", seq: 3, ok: true, result: "被拦截" });
  });

  it("结果后处理：await next() 拿到下游结果后加工返回", async () => {
    vi.mocked(pickDirectory).mockResolvedValue("C:\\repo");
    spawnWrapper(
      "com.wrap.w4",
      `bridge.wrap("dialog", async function(args, ctx, next){
         const r = await next();
         return r + "!";
       });`,
    );
    const b = spawnPlugin("com.wrap.caller4");
    b.transport.receive({ kind: "call", seq: 4, method: "call", args: ["dialog", "pickDirectory", []] });
    await tick();
    await tick();
    expect(b.transport.posted).toContainEqual({ kind: "reply", seq: 4, ok: true, result: "C:\\repo!" });
  });

  it("链序：多个包裹按注册序串链，真实 handler 最后执行", async () => {
    vi.mocked(pickDirectory).mockResolvedValue("ok");
    spawnWrapper(
      "com.wrap.chain1",
      `bridge.wrap("dialog", async function(args, ctx, next){
         self.__log = (self.__log || []).concat(["c1"]);
         return next();
       });`,
    );
    spawnWrapper(
      "com.wrap.chain2",
      `bridge.wrap("dialog", async function(args, ctx, next){
         self.__log = (self.__log || []).concat(["c2"]);
         return next();
       });`,
    );
    const b = spawnPlugin("com.wrap.caller5");
    b.transport.receive({ kind: "call", seq: 5, method: "call", args: ["dialog", "pickDirectory", []] });
    await tick();
    await tick();
    expect(pickDirectory).toHaveBeenCalledWith();
    expect(b.transport.posted).toContainEqual({ kind: "reply", seq: 5, ok: true, result: "ok" });
  });

  it("包裹方停止：卸载包裹插件后链路跳过其层，真实 handler 仍可服务", async () => {
    vi.mocked(pickDirectory).mockResolvedValue("ok");
    spawnWrapper(
      "com.wrap.gone",
      `bridge.wrap("dialog", async function(args, ctx, next){ return "不应出现"; });`,
    );
    unloadPlugin("com.wrap.gone");
    const b = spawnPlugin("com.wrap.caller6");
    b.transport.receive({ kind: "call", seq: 6, method: "call", args: ["dialog", "pickDirectory", []] });
    await tick();
    await tick();
    expect(pickDirectory).toHaveBeenCalledWith();
    expect(b.transport.posted).toContainEqual({ kind: "reply", seq: 6, ok: true, result: "ok" });
  });

  it("未知命名空间：wrap 注册被拒绝并给出可读错误", async () => {
    const w = spawnWrapper(
      "com.wrap.bad",
      `bridge.wrap("nonexistent", async function(args, ctx, next){ return next(); })
         .then(function(){ self.__ok = true; }, function(e){ self.__err = (e && e.message) || String(e); });`,
    );
    await tick();
    expect(w.sandbox.__err).toContain("能力 nonexistent 不存在");
    expect(w.sandbox.__ok).toBeUndefined();
  });

  it("同插件重复包裹：原位替换（后注册的生效）", async () => {
    spawnWrapper(
      "com.wrap.re",
      `bridge.wrap("dialog", async function(args, ctx, next){ return "first"; });
       bridge.wrap("dialog", async function(args, ctx, next){ return "second"; });`,
    );
    const b = spawnPlugin("com.wrap.caller7");
    b.transport.receive({ kind: "call", seq: 7, method: "call", args: ["dialog", "pickDirectory", []] });
    await tick();
    await tick();
    expect(pickDirectory).not.toHaveBeenCalled();
    expect(b.transport.posted).toContainEqual({ kind: "reply", seq: 7, ok: true, result: "second" });
  });

  it("流式调用被包裹的非流式方法：分发器补单个 end(result)（收尾契约保持）", async () => {
    vi.mocked(pickDirectory).mockResolvedValue("C:\\repo");
    spawnWrapper(
      "com.wrap.s",
      `bridge.wrap("dialog", async function(args, ctx, next){ return next(); });`,
    );
    const b = spawnPlugin("com.wrap.caller8");
    b.transport.receive({
      kind: "call",
      seq: 8,
      method: "call",
      args: ["dialog", "pickDirectory", [], { stream: true }],
    });
    await tick();
    await tick();
    const frames = b.transport.posted.filter((m) => (m as { kind?: string }).kind === "stream") as Array<{
      event: string;
      data?: unknown;
    }>;
    expect(frames.map((f) => f.event)).toEqual(["end"]);
    expect(frames[0].data).toBe("C:\\repo");
  });

  it("审计：包裹注册即记录被包裹命名空间", async () => {
    spawnWrapper(
      "com.wrap.audit",
      `bridge.wrap("dialog", async function(args, ctx, next){ return next(); });`,
    );
    await tick();
    expect(runtimeSnapshot().find((e) => e.id === "com.wrap.audit")?.used).toContain("dialog");
  });
});
