/**
 * Cordis 内核宿主测试（services/cordis/kernel）。
 *
 * 验证：平台服务提供/撤销、事件发射（emitPluginEvent → ctx.emit）、canvas/table/collab 服务工厂
 * （经注入的访问对象）、state/storage 按调用方插件隔离（tracker 绑定）、懒单例。
 * invoke 路径以替身替代，只验证归属 id 的推导与调用面。
 */
import { Context } from "@atelyx/cordis";
import { describe, expect, it, vi, afterEach } from "vitest";
import { setPluginCanvasAccess, setPluginCollabAccess, setPluginTableRuntimeAccess } from "./access";
import { emitPluginEvent, setKernelRef } from "./events";
import { createKernel, getKernel, resetKernel, type Kernel } from "./kernel";
import { dispatchPluginChannel } from "@/utils/collabHost";
import { createCanvasService } from "./canvas";
import { createTableService } from "./table";
import { buildAgentTools, pluginToolMetas } from "@/services/ai/tools";
import { mountPlugin, unmountAll, unmountPlugin } from "./loader";
import { registerGlobalShortcut } from "@/services/globalShortcut";
import {
  pluginReadState,
  pluginWriteState,
  pluginKvRead,
  pluginKvSet,
  pluginKvDelete,
  pluginKvWrite,
} from "@/services/plugins";
import {
  externalPrivateDir,
  externalWriteFileBase64,
  externalReadFileDataUrl,
} from "@/services/externalFs";

vi.mock("@/services/plugins", () => ({
  pluginReadState: vi.fn(async () => ({ saved: true })),
  pluginWriteState: vi.fn(async () => {}),
  pluginKvRead: vi.fn(async () => ({ k: "v" })),
  pluginKvSet: vi.fn(async () => {}),
  pluginKvDelete: vi.fn(async () => {}),
  pluginKvWrite: vi.fn(async () => {}),
}));

vi.mock("@/services/externalFs", () => ({
  externalReadFile: vi.fn(async () => ""),
  externalWriteFile: vi.fn(async () => {}),
  externalListDir: vi.fn(async () => ({ entries: [], total: 0, capped: false })),
  externalCreateFolder: vi.fn(async () => {}),
  externalRenameFile: vi.fn(async () => ""),
  externalMoveFile: vi.fn(async () => ""),
  externalDeleteFile: vi.fn(async () => {}),
  externalDeleteDir: vi.fn(async () => ({ deleted: true, needsConfirm: false, itemCount: 0 })),
  externalPrivateDir: vi.fn(async () => "C:/plugins/data/files"),
  externalWriteFileBase64: vi.fn(async () => {}),
  externalReadFileDataUrl: vi.fn(async () => "data:image/png;base64,AA=="),
}));

const shortcutTest = vi.hoisted(() => ({ trigger: null as ((accelerator: string, pluginId: string) => void) | null }));

vi.mock("@/services/globalShortcut", () => ({
  registerGlobalShortcut: vi.fn(async () => {}),
  unregisterGlobalShortcut: vi.fn(async () => {}),
  releasePluginGlobalShortcuts: vi.fn(async () => {}),
  onGlobalShortcutTriggered: vi.fn(async (handler: (accelerator: string, pluginId: string) => void) => {
    shortcutTest.trigger = handler;
    return () => {};
  }),
}));

afterEach(() => {
  resetKernel();
  setPluginCanvasAccess(null);
  setPluginTableRuntimeAccess(null);
  setPluginCollabAccess(null);
});

describe("Cordis 内核宿主", () => {
  it("createKernel 提供平台服务（state/storage/http/notification/app/shell/vault/dialog/clipboard/window/ai/collab/markdown）", () => {
    const { ctx, dispose } = createKernel();
    expect(ctx).toBeInstanceOf(Context);
    for (const name of [
      "state",
      "storage",
      "http",
      "notification",
      "app",
      "shell",
      "vault",
      "dialog",
      "clipboard",
      "window",
      "ai",
      "collab",
      "markdown",
    ]) {
      expect(ctx.get(name as never), name).toBeDefined();
    }
    dispose();
    expect(ctx.get("state" as never)).toBeUndefined();
    expect(ctx.get("vault" as never)).toBeUndefined();
    expect(ctx.get("markdown" as never)).toBeUndefined();
  });

  it("事件发射：emitPluginEvent → ctx.emit（typed event）", () => {
    const k = createKernel();
    setKernelRef(k);
    const seen: string[] = [];
    k.ctx.on("canvas:changed", (p) => {
      seen.push(`canvas:${p.file}`);
    });
    k.ctx.on("vault:changed", () => {
      seen.push("vault");
    });
    emitPluginEvent("canvas:changed", { file: "c.atlx" });
    emitPluginEvent("vault:changed", {});
    expect(seen).toEqual(["canvas:c.atlx", "vault"]);
    setKernelRef(null);
    k.dispose();
  });

  it("内核未登记（kernelRef 空）时发射 no-op", () => {
    const k = createKernel();
    const seen: string[] = [];
    k.ctx.on("canvas:changed", () => {
      seen.push("x");
    });
    setKernelRef(null);
    emitPluginEvent("canvas:changed", { file: "c.atlx" });
    expect(seen).toEqual([]);
    k.dispose();
  });

  it("collab 服务未接线时报错", () => {
    const { ctx, dispose } = createKernel();
    expect(() => ctx.collab.sendMessage("c", {})).toThrow("协作能力未就绪");
    expect(() => ctx.collab.myPeer()).toThrow("协作能力未就绪");
    dispose();
  });

  it("画布/表格服务工厂读取注入的访问对象", () => {
    const fakeCanvas = {
      snapshot: () => ({ canvasFile: "c.atlx", canvasTitle: "t", nodes: [], edges: [], selectedNodeId: null }),
      addNode: () => "n1",
    };
    const fakeTable = {
      snapshot: () => ({ tableFile: "t.atb", fields: [], rows: [] }),
      addRow: () => {},
    };
    setPluginCanvasAccess(fakeCanvas as never);
    setPluginTableRuntimeAccess(fakeTable as never);
    const canvas = createCanvasService();
    const table = createTableService();
    expect(canvas.snapshot().canvasFile).toBe("c.atlx");
    expect(canvas.addNode({ type: "text", position: { x: 0, y: 0 } })).toBe("n1");
    expect(table.snapshot().tableFile).toBe("t.atb");
    expect(() => table.addRow()).not.toThrow();
  });

  it("画布/表格服务工厂在访问未接线时报错", () => {
    expect(() => createCanvasService()).toThrow("画布能力未就绪");
    expect(() => createTableService()).toThrow("表格能力未就绪");
  });

  it("懒单例：getKernel 复用同一内核，resetKernel 重建", () => {
    const a = getKernel();
    const b = getKernel();
    expect(a).toBe(b);
    resetKernel();
    const c = getKernel();
    expect(c).not.toBe(a);
  });

  it("ctx.ai.registerTool：插件注册的 AI 工具进名册与分发器，随 fiber 卸载撤销", async () => {
    const k = createKernel();
    const fiber = k.ctx.plugin((ctx: Context) => {
      ctx.ai.registerTool({
        name: "plugin_echo",
        description: "回显参数",
        run: (args) => `echo:${String(args.text)}`,
      });
    });
    await fiber.await();
    expect(pluginToolMetas().map((m) => m.id)).toContain("plugin_echo");
    expect(pluginToolMetas().find((m) => m.id === "plugin_echo")?.category).toBe("plugin");
    expect(buildAgentTools(["plugin_echo"], true).tools.map((t) => t.name)).toContain("plugin_echo");
    await fiber.dispose();
    expect(pluginToolMetas().map((m) => m.id)).not.toContain("plugin_echo");
    expect(buildAgentTools(["plugin_echo"], true).tools).toEqual([]);
    k.dispose();
  });

  it("ctx.ai.registerTool：工具名非法即报错（不静默注册）", async () => {
    const k = createKernel();
    const fiber = k.ctx.plugin((ctx: Context) => {
      ctx.ai.registerTool({ name: "Bad Name", description: "x", run: () => "x" });
    });
    await expect(fiber.await()).rejects.toThrow("工具名须为非空标识");
    k.dispose();
  });

  it("ctx.ai.registerTool：与内置工具同名即报错（防静默覆盖宿主工具）", async () => {
    const k = createKernel();
    const fiber = k.ctx.plugin((ctx: Context) => {
      ctx.ai.registerTool({ name: "write_file", description: "冒名", run: () => "x" });
    });
    await expect(fiber.await()).rejects.toThrow("工具名已被占用");
    k.dispose();
  });
});

describe("state/storage 按调用方插件隔离", () => {
  let kernel: Kernel | null = null;

  afterEach(async () => {
    if (kernel) {
      await unmountAll(kernel);
      kernel.dispose();
      kernel = null;
    }
    vi.mocked(pluginReadState).mockClear();
    vi.mocked(pluginWriteState).mockClear();
    vi.mocked(pluginKvRead).mockClear();
    vi.mocked(pluginKvSet).mockClear();
    vi.mocked(pluginKvDelete).mockClear();
    vi.mocked(pluginKvWrite).mockClear();
  });

  it("插件内 ctx.state 读写落到调用方命名空间（id 由宿主推导，API 无 id 参数）", async () => {
    kernel = createKernel();
    const reads: Promise<unknown>[] = [];
    await mountPlugin(kernel, {
      id: "com.test.a",
      apply: (ctx) => {
        reads.push(ctx.state.read());
        void ctx.state.write({ hello: 1 });
      },
    });
    await Promise.all(reads);
    expect(pluginReadState).toHaveBeenCalledWith("com.test.a");
    expect(pluginWriteState).toHaveBeenCalledWith("com.test.a", { hello: 1 });
  });

  it("ctx.storage 各方法同归属；两个插件各自命中自己的命名空间", async () => {
    kernel = createKernel();
    const pending: Promise<unknown>[] = [];
    await mountPlugin(kernel, {
      id: "com.test.a",
      apply: (ctx) => {
        pending.push(ctx.storage.get("k"));
        void ctx.storage.set("k", 1);
        void ctx.storage.delete("k");
        pending.push(ctx.storage.keys());
        void ctx.storage.clear();
      },
    });
    await mountPlugin(kernel, {
      id: "com.test.b",
      apply: (ctx) => {
        pending.push(ctx.storage.get("k"));
        void ctx.state.write({ from: "b" });
      },
    });
    await Promise.all(pending);
    const kvReadIds = vi.mocked(pluginKvRead).mock.calls.map((c) => c[0]);
    expect(kvReadIds).toEqual(["com.test.a", "com.test.a", "com.test.b"]);
    expect(pluginKvSet).toHaveBeenCalledWith("com.test.a", "k", 1);
    expect(pluginKvSet).toHaveBeenCalledTimes(1);
    expect(pluginKvDelete).toHaveBeenCalledTimes(1);
    expect(pluginKvWrite).toHaveBeenCalledTimes(1);
    expect(pluginKvWrite).toHaveBeenCalledWith("com.test.a", {});
    expect(pluginWriteState).toHaveBeenCalledTimes(1);
    expect(pluginWriteState).toHaveBeenCalledWith("com.test.b", { from: "b" });
  });

  it("非插件上下文访问直接拒绝（无共享命名空间可落）", async () => {
    kernel = createKernel();
    expect(() => kernel!.ctx.state.read()).toThrow("只能在插件上下文中使用");
    expect(() => kernel!.ctx.storage.clear()).toThrow("只能在插件上下文中使用");
    expect(pluginReadState).not.toHaveBeenCalled();
    expect(pluginKvWrite).not.toHaveBeenCalled();
  });
});

describe("collab 按调用方插件绑定频道", () => {
  let kernel: Kernel | null = null;

  const fakeAccess = () => ({
    peers: () => [],
    setPresence: () => {},
    sendMessage: () => false,
    myPeer: () => ({ peerId: null, nickname: "", color: "", deviceName: "" }),
    acquire: () => () => {},
  });

  afterEach(async () => {
    if (kernel) {
      await unmountAll(kernel);
      kernel.dispose();
      kernel = null;
    }
    setPluginCollabAccess(null);
  });

  it("sendMessage 频道按调用方插件命名空间（线路名 = 插件id:频道）", async () => {
    kernel = createKernel();
    const sent: Array<[string, unknown, number | undefined]> = [];
    setPluginCollabAccess({
      ...fakeAccess(),
      sendMessage: (channel: string, payload: unknown, to?: number) => {
        sent.push([channel, payload, to]);
        return to !== undefined;
      },
      myPeer: () => ({ peerId: 7, nickname: "甲", color: "#000", deviceName: "机器甲" }),
    } as never);
    const result = await mountPlugin(kernel, {
      id: "com.test.a",
      apply: (ctx) => {
        expect(ctx.collab.myPeer()).toEqual({
          peerId: 7,
          nickname: "甲",
          color: "#000",
          deviceName: "机器甲",
        });
        ctx.collab.sendMessage("ch", { cmd: "start" });
        ctx.collab.sendMessage("ch", { cmd: "stop" }, { to: 9 });
      },
    });
    expect(result.ok).toBe(true);
    expect(sent).toEqual([
      ["com.test.a:ch", { cmd: "start" }, undefined],
      ["com.test.a:ch", { cmd: "stop" }, 9],
    ]);
  });

  it("subscribe 只收已订阅线路频道（撞名不串台），随插件卸载撤销", async () => {
    kernel = createKernel();
    setPluginCollabAccess(fakeAccess() as never);
    const received: Array<[number, unknown]> = [];
    const result = await mountPlugin(kernel, {
      id: "com.test.a",
      apply: (ctx) => {
        ctx.collab.subscribe("ch", (peerId, payload) => received.push([peerId, payload]));
      },
    });
    expect(result.ok).toBe(true);
    // 命中订阅：线路频道 = 插件id:频道（入站统一经 dispatchPluginChannel 投递）
    dispatchPluginChannel(5, "com.test.a:ch", { n: 1 });
    expect(received).toEqual([[5, { n: 1 }]]);
    // 其他插件同名频道不串台；未订阅频道不投递
    dispatchPluginChannel(6, "com.test.b:ch", { n: 2 });
    dispatchPluginChannel(7, "com.test.a:other", { n: 3 });
    expect(received).toEqual([[5, { n: 1 }]]);
    // 卸载撤销订阅（fiber effects 清理）
    await unmountPlugin(kernel, "com.test.a");
    dispatchPluginChannel(8, "com.test.a:ch", { n: 4 });
    expect(received).toEqual([[5, { n: 1 }]]);
  });

  it("subscribe 频道名非法（含命名空间分隔符）即挂载失败", async () => {
    kernel = createKernel();
    setPluginCollabAccess(fakeAccess() as never);
    const result = await mountPlugin(kernel, {
      id: "com.test.a",
      apply: (ctx) => {
        ctx.collab.subscribe("a:b", () => {});
      },
    });
    expect(result.ok).toBe(false);
  });
});

describe("fs 服务按调用方插件绑定", () => {
  let kernel: Kernel | null = null;

  afterEach(async () => {
    if (kernel) {
      await unmountAll(kernel);
      kernel.dispose();
      kernel = null;
    }
    vi.mocked(externalPrivateDir).mockClear();
    vi.mocked(externalWriteFileBase64).mockClear();
    vi.mocked(externalReadFileDataUrl).mockClear();
  });

  it("插件内 ctx.fs 私有目录与二进制方法带调用方插件 id 调用底层命令（writeFileBase64 包 { ok, summary }）", async () => {
    kernel = createKernel();
    const pending: Promise<unknown>[] = [];
    let privateDir: unknown;
    let writeResult: unknown;
    let dataUrl: unknown;
    await mountPlugin(kernel, {
      id: "com.test.fs",
      apply: (ctx) => {
        pending.push(ctx.fs.privateDir().then((p) => (privateDir = p)));
        pending.push(
          ctx.fs.writeFileBase64("E:/p/img.png", "QUJD").then((r) => {
            writeResult = r;
          }),
        );
        pending.push(ctx.fs.readFileDataUrl("E:/p/img.png").then((u) => (dataUrl = u)));
      },
    });
    await Promise.all(pending);
    expect(externalPrivateDir).toHaveBeenCalledWith("com.test.fs");
    expect(externalWriteFileBase64).toHaveBeenCalledWith("E:/p/img.png", "QUJD");
    expect(externalReadFileDataUrl).toHaveBeenCalledWith("E:/p/img.png");
    expect(privateDir).toBe("C:/plugins/data/files");
    expect(writeResult).toEqual({ ok: true, summary: "已写入「E:/p/img.png」" });
    expect(dataUrl).toBe("data:image/png;base64,AA==");
  });

  it("非插件上下文访问 fs 直接拒绝（归属 id 推导不到）", async () => {
    kernel = createKernel();
    expect(() => kernel!.ctx.fs.privateDir()).toThrow("只能在插件上下文中使用");
    expect(() => kernel!.ctx.fs.writeFileBase64("E:/x", "QQ==")).toThrow("只能在插件上下文中使用");
    expect(() => kernel!.ctx.fs.readFileDataUrl("E:/x")).toThrow("只能在插件上下文中使用");
    expect(externalPrivateDir).not.toHaveBeenCalled();
  });
});

describe("shortcuts 全局快捷键按调用方插件记账", () => {
  let kernel: Kernel | null = null;

  afterEach(async () => {
    if (kernel) {
      await unmountAll(kernel);
      kernel.dispose();
      kernel = null;
    }
    shortcutTest.trigger = null;
    vi.mocked(registerGlobalShortcut).mockClear();
  });

  it("registerGlobal 归属调用方插件；触发事件按原始注册串分发到登记回调", async () => {
    kernel = createKernel();
    const handler = vi.fn();
    await mountPlugin(kernel, {
      id: "com.test.a",
      apply: (ctx) => {
        void ctx.shortcuts.registerGlobal("Shift+Alt+E", handler);
      },
    });
    await vi.waitFor(() => expect(registerGlobalShortcut).toHaveBeenCalledWith("Shift+Alt+E", "com.test.a"));

    // 模拟 Rust 触发事件转发（载荷 = 原始注册串）
    shortcutTest.trigger?.("Shift+Alt+E", "com.test.a");
    await vi.waitFor(() => expect(handler).toHaveBeenCalledTimes(1));
    // 未登记的键不分发
    shortcutTest.trigger?.("Shift+Alt+X", "com.test.a");
    expect(handler).toHaveBeenCalledTimes(1);
  });

  it("注册失败即摘册：触发不再命中该键", async () => {
    vi.mocked(registerGlobalShortcut).mockRejectedValueOnce(new Error("已被其他插件注册"));
    kernel = createKernel();
    const handler = vi.fn();
    await mountPlugin(kernel, {
      id: "com.test.a",
      apply: async (ctx) => {
        await expect(ctx.shortcuts.registerGlobal("Shift+Alt+E", handler)).rejects.toThrow("已被其他插件注册");
      },
    });
    shortcutTest.trigger?.("Shift+Alt+E", "com.test.a");
    expect(handler).not.toHaveBeenCalled();
  });

  it("非插件上下文访问直接拒绝（无归属插件可记账）", async () => {
    kernel = createKernel();
    await expect(kernel!.ctx.shortcuts.registerGlobal("Shift+Alt+E", () => {})).rejects.toThrow(
      "只能在插件上下文中使用",
    );
    await expect(kernel!.ctx.shortcuts.unregisterGlobal("Shift+Alt+E")).rejects.toThrow(
      "只能在插件上下文中使用",
    );
    expect(registerGlobalShortcut).not.toHaveBeenCalled();
  });
});
