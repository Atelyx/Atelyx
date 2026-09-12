/**
 * 内核独立启动验证：无任何插件挂载时内核可启动。
 *
 * 断言：平台服务（含内核领域服务与 ctx.slots）就绪，而插件提供的领域服务（canvas/table/note/chat）
 * 缺席——内核不靠领域代码也能跑；再挂一个最小插件（只注册 UI 槽 + ctx.effect）验证贡献随卸载零残留。
 * 与 kernelBoundary.test.ts 互补：那条守静态导入边界，这条守运行时行为边界。
 */
import { afterEach, describe, expect, it } from "vitest";
import { createKernel } from "./kernel";
import { mountedPluginIds, mountPlugin, unmountPlugin } from "./loader";
import { listSlot, registerSlotContrib, registeredSlots, unregisterSlot } from "./slots";

afterEach(() => {
  // 槽注册表为模块级：测试内注册的贡献随 fiber 卸载撤销，此处仅兜底按 id 清空。
  // （不能靠「重注册取撤销函数」兜底：新建贡献的撤销只删新建那条，原贡献仍在；single 槽还会因重复 id 抛错。）
  for (const slot of registeredSlots()) {
    for (const contrib of listSlot(slot)) unregisterSlot(contrib.id);
  }
});

describe("内核独立启动（零插件）", () => {
  it("平台服务与内核领域服务就绪，插件提供的领域服务缺席", () => {
    const { ctx, dispose } = createKernel();
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
      "history",
      "layout",
      "uiState",
      "slots",
    ]) {
      expect(ctx.get(name as never), name).toBeDefined();
    }
    for (const name of ["canvas", "table", "note", "chat"]) {
      expect(ctx.get(name as never), name).toBeUndefined();
    }
    dispose();
  });

  it("零插件时无任何槽贡献与视图 kind", () => {
    const { dispose } = createKernel();
    expect(registeredSlots()).toEqual([]);
    dispose();
  });

  it("挂载最小插件后贡献可见，卸载后零残留", async () => {
    const kernel = createKernel();
    const result = await mountPlugin(kernel, {
      id: "com.test.stub",
      apply: (ctx) => {
        // 插件侧一律经 ctx.effect 注册（随 fiber 撤销的必要条件）
        ctx.effect(() =>
          registerSlotContrib("panelhead/status", "com.test.stub", { component: () => null }, { cardinality: "list" }),
        );
      },
    });
    expect(result).toEqual({ ok: true });
    expect(listSlot("panelhead/status")).toHaveLength(1);

    await unmountPlugin(kernel, "com.test.stub");
    expect(listSlot("panelhead/status")).toEqual([]);
    expect(mountedPluginIds(kernel)).toEqual([]);
    kernel.dispose();
  });

  it("inject 依赖缺失时插件不激活并报缺失服务", async () => {
    const kernel = createKernel();
    const result = await mountPlugin(kernel, {
      id: "com.test.needs-table",
      apply: { inject: ["table"], apply: () => {} },
    });
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.reason).toContain("table");
    kernel.dispose();
  });
});
