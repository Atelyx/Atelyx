/**
 * 通用能力面（dialog/clipboard/window）桥路由测试（services/plugins/bridge 注册的宿主命名空间）。
 *
 * 覆盖——方法路由与参数透传、参数类型错误/未知方法、审计、流式自动收尾。
 * `@/services/dialog`、`@/services/clipboard`、`@/services/window` 整体 mock：
 * 测试只验证桥的路由与透传，不触碰真实 IPC/系统对话框。
 */
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import type { PluginManifest, PluginType } from "@/types";
import type { PluginTransport } from "./worker";
import { attachPlugin, hostCapabilityNames, runtimeSnapshot, unloadPlugin } from "./bridge";
import { pickDirectory, pickFile, saveFile } from "@/services/dialog";
import { copyImageToClipboard, readClipboardText, writeClipboardText } from "@/services/clipboard";
import { closeWindow, minimizeWindow, toggleMaximizeWindow } from "@/services/window";

vi.mock("@/services/dialog", () => ({
  pickDirectory: vi.fn(),
  pickFile: vi.fn(),
  saveFile: vi.fn(),
}));
vi.mock("@/services/clipboard", () => ({
  copyImageToClipboard: vi.fn(),
  readClipboardText: vi.fn(),
  writeClipboardText: vi.fn(),
}));
vi.mock("@/services/window", () => ({
  closeWindow: vi.fn(),
  minimizeWindow: vi.fn(),
  toggleMaximizeWindow: vi.fn(),
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

describe("dialog 能力路由", () => {
  it("pickDirectory：无参透传并回包", async () => {
    vi.mocked(pickDirectory).mockResolvedValue("C:\\repo");
    const b = spawnPlugin("com.test.dlg1");
    b.transport.receive({ kind: "call", seq: 1, method: "call", args: ["dialog", "pickDirectory", []] });
    await tick();
    expect(pickDirectory).toHaveBeenCalledWith();
    expect(b.transport.posted).toContainEqual({ kind: "reply", seq: 1, ok: true, result: "C:\\repo" });
  });

  it("pickFile：过滤器透传；取消（null）原样回包", async () => {
    vi.mocked(pickFile).mockResolvedValue(null);
    const filters = [{ name: "Markdown", extensions: ["md"] }];
    const b = spawnPlugin("com.test.dlg2");
    b.transport.receive({ kind: "call", seq: 2, method: "call", args: ["dialog", "pickFile", [filters]] });
    await tick();
    expect(pickFile).toHaveBeenCalledWith(filters);
    expect(b.transport.posted).toContainEqual({ kind: "reply", seq: 2, ok: true, result: null });
  });

  it("saveFile：选项对象透传（defaultPath + filters）", async () => {
    vi.mocked(saveFile).mockResolvedValue("C:\\repo\\out.md");
    const opts = { defaultPath: "out.md", filters: [{ name: "Markdown", extensions: ["md"] }] };
    const b = spawnPlugin("com.test.dlg3");
    b.transport.receive({ kind: "call", seq: 3, method: "call", args: ["dialog", "saveFile", [opts]] });
    await tick();
    expect(saveFile).toHaveBeenCalledWith(opts);
    expect(b.transport.posted).toContainEqual({ kind: "reply", seq: 3, ok: true, result: "C:\\repo\\out.md" });
  });

  it("参数类型错误与未知方法：友好 error reply", async () => {
    const b = spawnPlugin("com.test.dlg4");
    b.transport.receive({ kind: "call", seq: 10, method: "call", args: ["dialog", "pickFile", ["not-an-array"]] });
    await tick();
    expect(b.transport.posted).toContainEqual({
      kind: "reply",
      seq: 10,
      ok: false,
      error: "dialog.pickFile 需要过滤器数组 [{ name, extensions }]",
    });
    b.transport.receive({ kind: "call", seq: 11, method: "call", args: ["dialog", "nonexistent", []] });
    await tick();
    expect(b.transport.posted).toContainEqual({ kind: "reply", seq: 11, ok: false, error: "dialog 无方法 nonexistent" });
  });
});

describe("clipboard 能力路由", () => {
  it("readText：返回剪贴板文本", async () => {
    vi.mocked(readClipboardText).mockResolvedValue("clip text");
    const b = spawnPlugin("com.test.clp1");
    b.transport.receive({ kind: "call", seq: 20, method: "call", args: ["clipboard", "readText", []] });
    await tick();
    expect(readClipboardText).toHaveBeenCalledWith();
    expect(b.transport.posted).toContainEqual({ kind: "reply", seq: 20, ok: true, result: "clip text" });
  });

  it("writeText / copyImage：参数透传", async () => {
    vi.mocked(writeClipboardText).mockResolvedValue(undefined);
    const b = spawnPlugin("com.test.clp2");
    b.transport.receive({ kind: "call", seq: 21, method: "call", args: ["clipboard", "writeText", ["hello"]] });
    await tick();
    expect(writeClipboardText).toHaveBeenCalledWith("hello");
    expect(b.transport.posted).toContainEqual({ kind: "reply", seq: 21, ok: true, result: true });

    vi.mocked(copyImageToClipboard).mockResolvedValue(undefined);
    b.transport.receive({
      kind: "call",
      seq: 22,
      method: "call",
      args: ["clipboard", "copyImage", ["data:image/png;base64,AAA"]],
    });
    await tick();
    expect(copyImageToClipboard).toHaveBeenCalledWith("data:image/png;base64,AAA");
    expect(b.transport.posted).toContainEqual({ kind: "reply", seq: 22, ok: true, result: true });
  });

  it("writeText 非字符串：error reply", async () => {
    const b = spawnPlugin("com.test.clp3");
    b.transport.receive({ kind: "call", seq: 23, method: "call", args: ["clipboard", "writeText", [123]] });
    await tick();
    expect(b.transport.posted).toContainEqual({
      kind: "reply",
      seq: 23,
      ok: false,
      error: "clipboard.writeText 需要文本",
    });
  });
});

describe("window 能力路由", () => {
  it("minimize / toggleMaximize / close：调用对应 service 并回包", async () => {
    vi.mocked(minimizeWindow).mockResolvedValue(undefined);
    vi.mocked(toggleMaximizeWindow).mockResolvedValue(undefined);
    vi.mocked(closeWindow).mockResolvedValue(undefined);
    const b = spawnPlugin("com.test.win1");
    b.transport.receive({ kind: "call", seq: 30, method: "call", args: ["window", "minimize", []] });
    await tick();
    expect(minimizeWindow).toHaveBeenCalledWith();
    b.transport.receive({ kind: "call", seq: 31, method: "call", args: ["window", "toggleMaximize", []] });
    await tick();
    expect(toggleMaximizeWindow).toHaveBeenCalledWith();
    b.transport.receive({ kind: "call", seq: 32, method: "call", args: ["window", "close", []] });
    await tick();
    expect(closeWindow).toHaveBeenCalledWith();
    expect(b.transport.posted).toContainEqual({ kind: "reply", seq: 30, ok: true, result: true });
    expect(b.transport.posted).toContainEqual({ kind: "reply", seq: 31, ok: true, result: true });
    expect(b.transport.posted).toContainEqual({ kind: "reply", seq: 32, ok: true, result: true });
  });

  it("未知方法：error reply", async () => {
    const b = spawnPlugin("com.test.win2");
    b.transport.receive({ kind: "call", seq: 33, method: "call", args: ["window", "resize", []] });
    await tick();
    expect(b.transport.posted).toContainEqual({ kind: "reply", seq: 33, ok: false, error: "window 无方法 resize" });
  });
});

describe("通用面注册与审计", () => {
  it("注册表含 dialog/clipboard/window；调用记入审计", async () => {
    expect(hostCapabilityNames()).toEqual(expect.arrayContaining(["dialog", "clipboard", "window"]));
    vi.mocked(readClipboardText).mockResolvedValue("x");
    const b = spawnPlugin("com.test.audit1");
    b.transport.receive({ kind: "call", seq: 40, method: "call", args: ["clipboard", "readText", []] });
    await tick();
    expect(runtimeSnapshot().find((e) => e.id === "com.test.audit1")?.used).toContain("clipboard");
  });

  it("流式调用非流式方法：分发器补单个 end(result)", async () => {
    vi.mocked(pickDirectory).mockResolvedValue("C:\\repo");
    const b = spawnPlugin("com.test.audit2");
    b.transport.receive({
      kind: "call",
      seq: 41,
      method: "call",
      args: ["dialog", "pickDirectory", [], { stream: true }],
    });
    await tick();
    const frames = b.transport.posted.filter((m) => (m as { kind?: string }).kind === "stream") as Array<{
      event: string;
      data?: unknown;
    }>;
    expect(frames.map((f) => f.event)).toEqual(["end"]);
    expect(frames[0].data).toBe("C:\\repo");
  });
});
