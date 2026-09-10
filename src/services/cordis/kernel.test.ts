/**
 * Cordis 内核宿主测试（services/cordis/kernel）。
 *
 * 验证：平台服务提供/撤销、事件发射（emitPluginEvent → ctx.emit）、canvas/table/collab 服务工厂
 * （经注入的访问对象）、懒单例。不触碰 Tauri invoke 路径的服务实现（其与注入访问同源，
 * 由领域侧接线覆盖）。
 */
import { Context } from "@atelyx/cordis";
import { describe, expect, it, afterEach } from "vitest";
import { setPluginCanvasAccess, setPluginCollabAccess, setPluginTableRuntimeAccess } from "./access";
import { emitPluginEvent, setKernelRef } from "./events";
import { createKernel, getKernel, resetKernel } from "./kernel";
import { createCanvasService } from "./canvas";
import { createTableService } from "./table";
import { buildAgentTools, pluginToolMetas } from "@/services/ai/tools";

afterEach(() => {
  resetKernel();
  setPluginCanvasAccess(null);
  setPluginTableRuntimeAccess(null);
  setPluginCollabAccess(null);
});

describe("Cordis 内核宿主", () => {
  it("createKernel 提供平台服务（state/storage/http/notification/app/shell/vault/dialog/clipboard/window/ai/collab）", () => {
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
    ]) {
      expect(ctx.get(name as never), name).toBeDefined();
    }
    dispose();
    expect(ctx.get("state" as never)).toBeUndefined();
    expect(ctx.get("vault" as never)).toBeUndefined();
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

  it("collab 服务经注入的访问对象可用", () => {
    const { ctx, dispose } = createKernel();
    const fake = {
      peers: () => [{ id: "p1" }],
      setPresence: (_view: string | null, _file: string | null) => {},
    };
    setPluginCollabAccess(fake as never);
    expect(ctx.collab.peers()).toEqual([{ id: "p1" }]);
    expect(() => ctx.collab.setPresence("canvas", "c.atlx")).not.toThrow();
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
