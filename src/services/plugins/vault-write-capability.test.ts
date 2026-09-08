/**
 * vault 写能力（仓库文件写入）桥路由测试（services/plugins/bridge 注册的宿主 `vault` 命名空间写方法）。
 *
 * 覆盖——八个写方法的路由与参数透传、参数类型错误/未知方法/未接线报错、审计、流式自动收尾，
 * 以及代理源 ↔ 真实桥 ↔ 注入访问对象的端到端回环（vm 沙箱驱动，模拟 worker 平面真链路）。
 * 写方法走 pluginStore 注入的访问对象（PluginVaultWriteAccess），测试注入 fake，不触碰真实 IPC。
 */
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { runInNewContext } from "node:vm";
import type { PluginManifest, PluginType } from "@/types";
import { buildProxySource, type PluginTransport } from "./worker";
import {
  attachPlugin,
  hostCapabilityNames,
  runtimeSnapshot,
  setPluginVaultWriteAccess,
  unloadPlugin,
  type PluginVaultWriteAccess,
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

/** 每测试注入的 fake 写访问（全部方法 vi.fn 记录调用）。 */
function makeFakeAccess(): PluginVaultWriteAccess {
  return {
    writeFile: vi.fn(async () => ({ ok: true, summary: "written" })),
    editFile: vi.fn(async () => ({ ok: true, summary: "edited" })),
    appendFile: vi.fn(async () => ({ ok: true, summary: "appended" })),
    renameFile: vi.fn(async () => ({ ok: true, summary: "renamed", actualPath: "b.md" })),
    moveFile: vi.fn(async () => ({ ok: true, summary: "moved", actualPath: "dir/b.md" })),
    deleteFile: vi.fn(async () => ({ ok: true, summary: "deleted" })),
    deleteDir: vi.fn(async () => ({ ok: true, summary: "deleted dir", needsConfirm: false, itemCount: 0 })),
    createFolder: vi.fn(async () => ({ ok: true, summary: "created", path: "notes" })),
  };
}

let fake: PluginVaultWriteAccess;

beforeEach(() => {
  fake = makeFakeAccess();
  setPluginVaultWriteAccess(fake);
});

afterEach(() => {
  setPluginVaultWriteAccess(null);
  for (const s of spawned) unloadPlugin(s.id);
  spawned.length = 0;
});

describe("vault 写能力路由", () => {
  it("writeFile：参数透传并回包", async () => {
    const b = spawnPlugin("com.test.w1");
    b.transport.receive({ kind: "call", seq: 1, method: "call", args: ["vault", "writeFile", ["a.md", "hello"]] });
    await tick();
    expect(fake.writeFile).toHaveBeenCalledWith("a.md", "hello");
    expect(b.transport.posted).toContainEqual({
      kind: "reply",
      seq: 1,
      ok: true,
      result: { ok: true, summary: "written" },
    });
  });

  it("editFile：文件与编辑项数组透传", async () => {
    const b = spawnPlugin("com.test.w2");
    const edits = [{ oldText: "a", newText: "b" }];
    b.transport.receive({ kind: "call", seq: 2, method: "call", args: ["vault", "editFile", ["a.md", edits]] });
    await tick();
    expect(fake.editFile).toHaveBeenCalledWith("a.md", edits);
    expect(b.transport.posted).toContainEqual({
      kind: "reply",
      seq: 2,
      ok: true,
      result: { ok: true, summary: "edited" },
    });
  });

  it("appendFile / createFolder：参数透传并回包", async () => {
    const b = spawnPlugin("com.test.w3");
    b.transport.receive({ kind: "call", seq: 3, method: "call", args: ["vault", "appendFile", ["a.md", "more"]] });
    await tick();
    expect(fake.appendFile).toHaveBeenCalledWith("a.md", "more");
    expect(b.transport.posted).toContainEqual({
      kind: "reply",
      seq: 3,
      ok: true,
      result: { ok: true, summary: "appended" },
    });
    b.transport.receive({ kind: "call", seq: 4, method: "call", args: ["vault", "createFolder", ["notes"]] });
    await tick();
    expect(fake.createFolder).toHaveBeenCalledWith("notes");
    expect(b.transport.posted).toContainEqual({
      kind: "reply",
      seq: 4,
      ok: true,
      result: { ok: true, summary: "created", path: "notes" },
    });
  });

  it("renameFile / moveFile：旧路径与目标透传（actualPath 回包）", async () => {
    const b = spawnPlugin("com.test.w4");
    b.transport.receive({ kind: "call", seq: 5, method: "call", args: ["vault", "renameFile", ["a.md", "b.md"]] });
    await tick();
    expect(fake.renameFile).toHaveBeenCalledWith("a.md", "b.md");
    expect(b.transport.posted).toContainEqual({
      kind: "reply",
      seq: 5,
      ok: true,
      result: { ok: true, summary: "renamed", actualPath: "b.md" },
    });
    b.transport.receive({ kind: "call", seq: 6, method: "call", args: ["vault", "moveFile", ["a.md", "dir"]] });
    await tick();
    expect(fake.moveFile).toHaveBeenCalledWith("a.md", "dir");
    expect(b.transport.posted).toContainEqual({
      kind: "reply",
      seq: 6,
      ok: true,
      result: { ok: true, summary: "moved", actualPath: "dir/b.md" },
    });
  });

  it("deleteFile / deleteDir：路径与 force 透传（needsConfirm 形态透传）", async () => {
    const b = spawnPlugin("com.test.w5");
    b.transport.receive({ kind: "call", seq: 7, method: "call", args: ["vault", "deleteFile", ["a.md"]] });
    await tick();
    expect(fake.deleteFile).toHaveBeenCalledWith("a.md");
    expect(b.transport.posted).toContainEqual({
      kind: "reply",
      seq: 7,
      ok: true,
      result: { ok: true, summary: "deleted" },
    });
    b.transport.receive({ kind: "call", seq: 8, method: "call", args: ["vault", "deleteDir", ["notes", true]] });
    await tick();
    expect(fake.deleteDir).toHaveBeenCalledWith("notes", true);
    expect(b.transport.posted).toContainEqual({
      kind: "reply",
      seq: 8,
      ok: true,
      result: { ok: true, summary: "deleted dir", needsConfirm: false, itemCount: 0 },
    });
    b.transport.receive({ kind: "call", seq: 9, method: "call", args: ["vault", "deleteDir", ["notes"]] });
    await tick();
    expect(fake.deleteDir).toHaveBeenCalledWith("notes", undefined);
  });

  it("参数类型错误：友好 error reply", async () => {
    const b = spawnPlugin("com.test.w6");
    b.transport.receive({ kind: "call", seq: 10, method: "call", args: ["vault", "writeFile", [123, "x"]] });
    await tick();
    expect(b.transport.posted).toContainEqual({
      kind: "reply",
      seq: 10,
      ok: false,
      error: "vault.writeFile 需要文件路径与内容字符串",
    });
    b.transport.receive({ kind: "call", seq: 11, method: "call", args: ["vault", "editFile", ["a.md", "not-an-array"]] });
    await tick();
    expect(b.transport.posted).toContainEqual({
      kind: "reply",
      seq: 11,
      ok: false,
      error: "vault.editFile 需要文件路径与编辑项数组 [{ oldText, newText }]",
    });
    b.transport.receive({
      kind: "call",
      seq: 12,
      method: "call",
      args: ["vault", "editFile", ["a.md", [{ oldText: 1, newText: "b" }]]],
    });
    await tick();
    expect(b.transport.posted).toContainEqual({
      kind: "reply",
      seq: 12,
      ok: false,
      error: "vault.editFile 需要文件路径与编辑项数组 [{ oldText, newText }]",
    });
    b.transport.receive({ kind: "call", seq: 13, method: "call", args: ["vault", "deleteDir", ["notes", "yes"]] });
    await tick();
    expect(b.transport.posted).toContainEqual({
      kind: "reply",
      seq: 13,
      ok: false,
      error: "vault.deleteDir 的 force 需要布尔值",
    });
  });

  it("未接线（未打开仓库）：仓库写能力未就绪", async () => {
    setPluginVaultWriteAccess(null);
    const b = spawnPlugin("com.test.w7");
    b.transport.receive({ kind: "call", seq: 14, method: "call", args: ["vault", "writeFile", ["a.md", "x"]] });
    await tick();
    expect(b.transport.posted).toContainEqual({ kind: "reply", seq: 14, ok: false, error: "仓库写能力未就绪" });
  });

  it("未知方法：vault 无方法", async () => {
    const b = spawnPlugin("com.test.w8");
    b.transport.receive({ kind: "call", seq: 15, method: "call", args: ["vault", "nonexistent", []] });
    await tick();
    expect(b.transport.posted).toContainEqual({ kind: "reply", seq: 15, ok: false, error: "vault 无方法 nonexistent" });
  });

  it("审计记录 vault 命名空间；注册表含 vault", async () => {
    const b = spawnPlugin("com.test.w9");
    b.transport.receive({ kind: "call", seq: 16, method: "call", args: ["vault", "writeFile", ["a.md", "x"]] });
    await tick();
    expect(runtimeSnapshot().find((e) => e.id === "com.test.w9")?.used).toContain("vault");
    expect(hostCapabilityNames()).toContain("vault");
  });

  it("流式调用非流式写方法：分发器补单个 end(result)", async () => {
    const b = spawnPlugin("com.test.w10");
    b.transport.receive({
      kind: "call",
      seq: 17,
      method: "call",
      args: ["vault", "writeFile", ["a.md", "x"], { stream: true }],
    });
    await tick();
    const frames = b.transport.posted.filter((m) => (m as { kind?: string }).kind === "stream") as Array<{
      event: string;
      data?: unknown;
    }>;
    expect(frames.map((f) => f.event)).toEqual(["end"]);
    expect(frames[0].data).toEqual({ ok: true, summary: "written" });
  });
});

describe("端到端回环（代理源 ↔ 真实桥 ↔ 注入访问）", () => {
  it("插件顶层 bridge.call(\"vault\", \"writeFile\") 全链路返回", async () => {
    const holder: { host: ((m: unknown) => void) | null } = { host: null };
    const sandbox: Record<string, unknown> = {};
    let toPlugin: ((m: unknown) => void) | null = null;
    // 沙箱（= worker 全局）：proxy 注册 message 监听 + postMessage 出站。
    sandbox.addEventListener = (_t: string, h: (e: { data: unknown }) => void) => {
      toPlugin = (m: unknown) => h({ data: m });
    };
    sandbox.postMessage = (m: unknown) => {
      holder.host?.(m); // 插件 → 宿主
    };
    sandbox.self = sandbox;
    sandbox.Promise = Promise;
    sandbox.setTimeout = setTimeout;
    sandbox.console = console;
    const transport: PluginTransport = {
      post: (m) => toPlugin?.(m), // 宿主 → 插件
      onMessage: (h) => {
        holder.host = h;
        return () => {
          holder.host = null;
        };
      },
      dispose: () => {},
    };
    attachPlugin(manifest("com.test.e2e"), transport);
    try {
      const pluginCode = `
        bridge.call("vault", "writeFile", ["a.md", "hi"]).then(function(r){ self.__result = r; },
          function(e){ self.__error = (e && e.message) || String(e); });
      `;
      runInNewContext(`${buildProxySource()}\n;\n${pluginCode}`, sandbox);
      await tick();
      await tick();
      expect(fake.writeFile).toHaveBeenCalledWith("a.md", "hi");
      expect(sandbox.__result).toEqual({ ok: true, summary: "written" });
      expect(sandbox.__error).toBeUndefined();
    } finally {
      unloadPlugin("com.test.e2e");
    }
  });
});
