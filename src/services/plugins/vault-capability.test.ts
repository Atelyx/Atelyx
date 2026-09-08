/**
 * vault 能力（仓库文件读取）桥路由测试（services/plugins/bridge 注册的宿主 `vault` 命名空间）。
 *
 * 覆盖——六方法路由与参数透传、参数类型错误/未知方法、审计、流式自动收尾，以及
 * 代理源 ↔ 真实桥 ↔ vault 服务的端到端回环（vm 沙箱驱动，模拟 worker 平面真链路）。
 * `@/services/vault` 整体 mock：测试只验证桥的路由与透传，不触碰真实 IPC。
 */
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { runInNewContext } from "node:vm";
import type {
  FileTreeNode,
  GlobVaultResult,
  GrepVaultResult,
  ListDirResult,
  PluginManifest,
  PluginType,
  ReadWindowResult,
} from "@/types";
import { buildProxySource, type PluginTransport } from "./worker";
import { attachPlugin, hostCapabilityNames, runtimeSnapshot, unloadPlugin } from "./bridge";
import { listVaultTree } from "@/services/vault";
import {
  globVault,
  grepVault,
  listVaultDir,
  readVaultFile,
  readVaultFileWindow,
} from "@/services/vault/aiFiles";

vi.mock("@/services/vault", () => ({
  listVaultTree: vi.fn(),
}));
vi.mock("@/services/vault/aiFiles", () => ({
  globVault: vi.fn(),
  grepVault: vi.fn(),
  listVaultDir: vi.fn(),
  readVaultFile: vi.fn(),
  readVaultFileWindow: vi.fn(),
}));

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

beforeEach(() => {
  vi.resetAllMocks();
});

afterEach(() => {
  for (const s of spawned) unloadPlugin(s.id);
  spawned.length = 0;
});

describe("vault 能力路由", () => {
  it("readFile：路由到 readVaultFile 并回包", async () => {
    vi.mocked(readVaultFile).mockResolvedValue("内容");
    const b = spawnPlugin("com.test.v1");
    b.transport.receive({ kind: "call", seq: 1, method: "call", args: ["vault", "readFile", ["a.md"]] });
    await tick();
    expect(readVaultFile).toHaveBeenCalledWith("a.md");
    expect(b.transport.posted).toContainEqual({ kind: "reply", seq: 1, ok: true, result: "内容" });
  });

  it("readFileWindow：文件与选项透传", async () => {
    const win: ReadWindowResult = { lines: [{ number: 1, text: "l1" }], totalLines: 10, truncated: false };
    vi.mocked(readVaultFileWindow).mockResolvedValue(win);
    const b = spawnPlugin("com.test.v2");
    b.transport.receive({
      kind: "call",
      seq: 2,
      method: "call",
      args: ["vault", "readFileWindow", ["a.md", { offset: 5, limit: 3 }]],
    });
    await tick();
    expect(readVaultFileWindow).toHaveBeenCalledWith("a.md", { offset: 5, limit: 3 });
    expect(b.transport.posted).toContainEqual({ kind: "reply", seq: 2, ok: true, result: win });
  });

  it("listFiles：返回仓库文件树", async () => {
    const tree: FileTreeNode[] = [{ name: "a.md", path: "a.md", isDir: false, updatedAt: 0, children: [] }];
    vi.mocked(listVaultTree).mockResolvedValue(tree);
    const b = spawnPlugin("com.test.v3");
    b.transport.receive({ kind: "call", seq: 3, method: "call", args: ["vault", "listFiles", []] });
    await tick();
    expect(listVaultTree).toHaveBeenCalledWith();
    expect(b.transport.posted).toContainEqual({ kind: "reply", seq: 3, ok: true, result: tree });
  });

  it("listDir：目录路径透传（省略 = 仓库根）", async () => {
    const dir: ListDirResult = { entries: [{ name: "a.md", kind: "file" }], total: 1, capped: false };
    vi.mocked(listVaultDir).mockResolvedValue(dir);
    const b = spawnPlugin("com.test.v4");
    b.transport.receive({ kind: "call", seq: 4, method: "call", args: ["vault", "listDir", ["notes"]] });
    await tick();
    expect(listVaultDir).toHaveBeenCalledWith("notes");
    expect(b.transport.posted).toContainEqual({ kind: "reply", seq: 4, ok: true, result: dir });
    b.transport.receive({ kind: "call", seq: 5, method: "call", args: ["vault", "listDir", []] });
    await tick();
    expect(listVaultDir).toHaveBeenCalledWith(undefined);
  });

  it("glob / grep：模式与选项透传", async () => {
    const glob: GlobVaultResult = { root: "", paths: ["a.md"], total: 1, capped: false };
    vi.mocked(globVault).mockResolvedValue(glob);
    const b = spawnPlugin("com.test.v5");
    b.transport.receive({
      kind: "call",
      seq: 6,
      method: "call",
      args: ["vault", "glob", ["**/*.md", { path: "notes" }]],
    });
    await tick();
    expect(globVault).toHaveBeenCalledWith("**/*.md", { path: "notes" });
    expect(b.transport.posted).toContainEqual({ kind: "reply", seq: 6, ok: true, result: glob });

    const grep: GrepVaultResult = {
      matches: [{ path: "a.md", lineNumber: 1, line: "hi" }],
      total: 1,
      capped: false,
    };
    vi.mocked(grepVault).mockResolvedValue(grep);
    b.transport.receive({
      kind: "call",
      seq: 7,
      method: "call",
      args: ["vault", "grep", ["TODO", { include: "*.md" }]],
    });
    await tick();
    expect(grepVault).toHaveBeenCalledWith("TODO", { include: "*.md" });
    expect(b.transport.posted).toContainEqual({ kind: "reply", seq: 7, ok: true, result: grep });
  });

  it("参数类型错误与未知方法：友好 error reply", async () => {
    const b = spawnPlugin("com.test.v6");
    b.transport.receive({ kind: "call", seq: 10, method: "call", args: ["vault", "readFile", [123]] });
    await tick();
    expect(b.transport.posted).toContainEqual({
      kind: "reply",
      seq: 10,
      ok: false,
      error: "vault.readFile 需要相对仓库根的文件路径",
    });
    b.transport.receive({ kind: "call", seq: 11, method: "call", args: ["vault", "nonexistent", []] });
    await tick();
    expect(b.transport.posted).toContainEqual({ kind: "reply", seq: 11, ok: false, error: "vault 无方法 nonexistent" });
  });

  it("审计记录 vault 命名空间；注册表含 vault", async () => {
    const b = spawnPlugin("com.test.v7");
    b.transport.receive({ kind: "call", seq: 12, method: "call", args: ["vault", "listFiles", []] });
    await tick();
    expect(runtimeSnapshot().find((e) => e.id === "com.test.v7")?.used).toContain("vault");
    expect(hostCapabilityNames()).toContain("vault");
  });

  it("流式调用非流式方法：分发器补单个 end(result)", async () => {
    vi.mocked(readVaultFile).mockResolvedValue("内容");
    const b = spawnPlugin("com.test.v8");
    b.transport.receive({
      kind: "call",
      seq: 13,
      method: "call",
      args: ["vault", "readFile", ["a.md"], { stream: true }],
    });
    await tick();
    const frames = b.transport.posted.filter((m) => (m as { kind?: string }).kind === "stream") as Array<{
      event: string;
      data?: unknown;
    }>;
    expect(frames.map((f) => f.event)).toEqual(["end"]);
    expect(frames[0].data).toBe("内容");
  });
});

describe("端到端回环（代理源 ↔ 真实桥 ↔ vault 服务）", () => {
  it("插件顶层 bridge.call(\"vault\", \"readFile\") 全链路返回", async () => {
    vi.mocked(readVaultFile).mockResolvedValue("hello vault");
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
        bridge.call("vault", "readFile", ["a.md"]).then(function(r){ self.__result = r; },
          function(e){ self.__error = (e && e.message) || String(e); });
      `;
      runInNewContext(`${buildProxySource()}\n;\n${pluginCode}`, sandbox);
      await tick();
      await tick();
      expect(readVaultFile).toHaveBeenCalledWith("a.md");
      expect(sandbox.__result).toBe("hello vault");
      expect(sandbox.__error).toBeUndefined();
    } finally {
      unloadPlugin("com.test.e2e");
    }
  });
});
