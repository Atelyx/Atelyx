/**
 * ctx.slots 注册 API 测试（services/cordis/slotsApi）。
 *
 * 覆盖：插件 apply 内经 ctx.slots.registerView/registerTableView 注册生效、pluginId 归属正确、
 * 卸载随 fiber 撤销（tracker 绑定调用方插件上下文）。
 */
import { describe, expect, it, afterEach } from "vitest";
import type { Context } from "@atelyx/cordis";
import { createKernel, type Kernel } from "./kernel";
import { mountPlugin, unmountAll } from "./loader";
import { resolveViewKind, viewKinds, listSlot, registeredSlots } from "./slots";
import { getPluginTableView } from "./ui";

let kernel: Kernel | null = null;

afterEach(async () => {
  if (kernel) {
    await unmountAll(kernel);
    kernel.dispose();
    kernel = null;
  }
});

describe("ctx.slots", () => {
  it("registerView：注册视图槽 + pluginId 归属；卸载撤销", async () => {
    kernel = createKernel();
    const apply = (ctx: Context) => {
      ctx.slots.registerView({ kind: "com.test.panel", label: "面板", component: () => null });
    };
    await mountPlugin(kernel, { id: "com.test.ui", apply });
    expect(viewKinds()).toContain("com.test.panel");
    expect(resolveViewKind("com.test.panel")?.pluginId).toBe("com.test.ui");
    expect(resolveViewKind("com.test.panel")?.payload.label).toBe("面板");

    await unmountAll(kernel);
    expect(viewKinds()).toEqual([]);
  });

  it("registerTableView：表格视图注册；卸载撤销", async () => {
    kernel = createKernel();
    const apply = (ctx: Context) => {
      ctx.slots.registerTableView({ kind: "com.test.tl", label: "时间线", component: () => null });
    };
    await mountPlugin(kernel, { id: "com.test.ui", apply });
    expect(getPluginTableView("com.test.tl")?.pluginId).toBe("com.test.ui");
    expect(getPluginTableView("com.test.tl")?.label).toBe("时间线");

    await unmountAll(kernel);
    expect(getPluginTableView("com.test.tl")).toBeUndefined();
  });

  it("registerView 缺 component/render 拒绝", async () => {
    kernel = createKernel();
    const apply = (ctx: Context) => {
      // 缺 component 与 render：注册时报错 → apply 失败可见。
      ctx.slots.registerView({ kind: "com.test.bad", label: "坏" });
    };
    const result = await mountPlugin(kernel, { id: "com.test.ui", apply });
    expect(result.ok).toBe(false);
    expect(viewKinds()).toEqual([]);
  });

  it("registerUi：可向任意具名槽位贡献组件；卸载撤销", async () => {
    kernel = createKernel();
    const apply = (ctx: Context) => {
      ctx.slots.registerUi({ slot: "toolbar/note/right", component: () => null });
    };
    await mountPlugin(kernel, { id: "com.test.ui", apply });
    expect(registeredSlots()).toContain("toolbar/note/right");
    expect(listSlot("toolbar/note/right")).toHaveLength(1);

    await unmountAll(kernel);
    expect(listSlot("toolbar/note/right")).toHaveLength(0);
  });
});
