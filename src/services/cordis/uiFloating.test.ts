/**
 * 插件浮层承载服务测试（services/cordis/uiFloating）：登记归属、随插件停用自动收起、
 * 句柄收起、参数校验与未接线拒绝。
 */
import { describe, expect, it, afterEach } from "vitest";
import { createKernel, resetKernel, type Kernel } from "./kernel";
import { setPluginFloatingLayerAccess } from "./access";
import type { FloatingLayerEntry } from "./types";

let kernel: Kernel | null = null;
let rec: ReturnType<typeof recordingAccess> | null = null;

afterEach(() => {
  if (kernel) {
    resetKernel();
    kernel = null;
  }
  setPluginFloatingLayerAccess(null);
  rec = null;
});

/** 记录型浮层访问：open/close 调用留痕，收起语义对齐 store（close 触发该条目 onClose 一次）。 */
function recordingAccess() {
  const opens: FloatingLayerEntry[] = [];
  const closed: string[] = [];
  return {
    opens,
    closed,
    open: (entry: Omit<FloatingLayerEntry, "id">) => {
      const id = `layer-${opens.length + 1}`;
      opens.push({ ...entry, id });
      return id;
    },
    close: (id: string) => {
      const entry = opens.find((l) => l.id === id);
      if (!entry || closed.includes(id)) return;
      closed.push(id);
      entry.onClose?.();
    },
  };
}

describe("ctx.ui.showFloatingLayer", () => {
  it("登记归属调用方插件；句柄 close 收起并触发 onClose", async () => {
    kernel = createKernel();
    rec = recordingAccess();
    setPluginFloatingLayerAccess(rec);
    let onClose = 0;
    let handle: { close(): void } | null = null;
    const { mountPlugin } = await import("./loader");
    const result = await mountPlugin(kernel, {
      id: "com.test.a",
      apply: (ctx) => {
        handle = ctx.ui.showFloatingLayer({
          component: () => null,
          placement: { x: 10, y: 20 },
          width: 300,
          closeOnOutsideClick: true,
          onClose: () => onClose++,
        });
      },
    });
    expect(result.ok).toBe(true);
    expect(rec.opens).toHaveLength(1);
    expect(rec.opens[0].pluginId).toBe("com.test.a");
    expect(rec.opens[0].placement).toEqual({ x: 10, y: 20 });
    expect(rec.opens[0].width).toBe(300);
    expect(rec.opens[0].closeOnOutsideClick).toBe(true);
    handle!.close();
    expect(rec.closed).toEqual(["layer-1"]);
    expect(onClose).toBe(1);
    // 重复 close = no-op（onClose 不再触发）
    handle!.close();
    expect(onClose).toBe(1);
  });

  it("插件停用/卸载：登记的浮层自动收起", async () => {
    kernel = createKernel();
    rec = recordingAccess();
    setPluginFloatingLayerAccess(rec);
    const { mountPlugin, unmountPlugin } = await import("./loader");
    const result = await mountPlugin(kernel, {
      id: "com.test.a",
      apply: (ctx) => {
        ctx.ui.showFloatingLayer({ component: () => null });
      },
    });
    expect(result.ok).toBe(true);
    expect(rec.opens).toHaveLength(1);
    await unmountPlugin(kernel, "com.test.a");
    expect(rec.closed).toEqual([rec.opens[0].id]);
  });

  it("参数校验与未接线拒绝（挂载失败带可读原因，不留登记）", async () => {
    kernel = createKernel();
    const { mountPlugin } = await import("./loader");
    // 未接线（浮层能力未就绪）：合法调用挂载失败且原因可读
    const unwired = await mountPlugin(kernel, {
      id: "com.test.a",
      apply: (ctx) => {
        ctx.ui.showFloatingLayer({ component: () => null });
      },
    });
    expect(unwired.ok).toBe(false);
    expect(unwired).toMatchObject({ message: expect.stringContaining("浮层能力未就绪") });

    rec = recordingAccess();
    setPluginFloatingLayerAccess(rec);
    const cases: Array<{ options: unknown; reason: string }> = [
      { options: {}, reason: "component" },
      { options: { component: () => null, placement: { x: "a" } }, reason: "placement" },
      { options: { component: () => null, width: -1 }, reason: "width" },
    ];
    for (const [i, c] of cases.entries()) {
      const result = await mountPlugin(kernel, {
        id: `com.test.case${i}`,
        apply: (ctx) => {
          ctx.ui.showFloatingLayer(c.options as never);
        },
      });
      expect(result.ok).toBe(false);
      expect(result).toMatchObject({ message: expect.stringContaining(c.reason) });
    }
    expect(rec.opens).toHaveLength(0);
  });
});
