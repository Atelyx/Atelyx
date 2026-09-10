/**
 * 随应用分发插件集成测试：默认组合行经 loader 全部挂载到真实内核。
 *
 * 验证 pluginStore.load 的挂载路径等价语义（行 → 实现解析 → apply 装配视图槽 + 领域服务 + 接线）：
 * - 挂载后 ctx.canvas/ctx.table/ctx.note/ctx.chat 可用、视图槽可解析、装配顺序 = 默认组合顺序
 * - 卸载（fiber dispose）全部撤销：槽消失、服务消失、能力接线复位
 * - 用户插件经 priority 替换默认实现（作者侧声明，用户只需安装 + 启用）
 */
import { describe, expect, it, afterEach } from "vitest";
import type { Context } from "@atelyx/cordis";
import { setPluginCanvasAccess, setPluginTableRuntimeAccess } from "./access";
import { CORDIS_BUILTIN_DEFS, DEFAULT_COMPOSITION } from "@/components/plugins/cordis/builtins";
import { createKernel, type Kernel } from "./kernel";
import { mountPlugin, mountedPluginIds, unmountAll } from "./loader";
import { resolveViewKind, viewKinds } from "./slots";
import { composePlugins, mountOrder, type CompositionPackage } from "@/utils/cordis/composition";

const packages: CompositionPackage[] = CORDIS_BUILTIN_DEFS.map((d) => ({
  id: d.id,
  name: d.name,
  version: "0.0.0",
  sourceKind: "builtin",
  enabled: true,
}));

/** 按装配顺序挂载全部启用的行（= pluginStore.load 的挂载路径）。 */
async function mountAll(kernel: Kernel, all: CompositionPackage[] = packages) {
  const rows = composePlugins(DEFAULT_COMPOSITION, all);
  for (const id of mountOrder(rows)) {
    const def = CORDIS_BUILTIN_DEFS.find((d) => d.id === id);
    if (!def) throw new Error(`测试缺少实现定义：${id}`);
    const result = await mountPlugin(kernel, { id, apply: def.apply });
    expect(result).toEqual({ ok: true });
  }
  return rows;
}

let kernel: Kernel | null = null;

afterEach(async () => {
  if (kernel) {
    await unmountAll(kernel);
    kernel.dispose();
    kernel = null;
  }
  setPluginCanvasAccess(null);
  setPluginTableRuntimeAccess(null);
});

describe("随应用分发插件挂载集成", () => {
  it("默认组合全部可挂载：视图槽/领域服务就绪；卸载后全部撤销", async () => {
    kernel = createKernel();
    await mountAll(kernel);

    // 视图槽：默认组合的视图 kind 全部注册（主题行无视图）。
    const kinds = viewKinds();
    expect(kinds).toContain("canvas");
    expect(kinds).toContain("table");
    expect(kinds).toContain("note");
    expect(resolveViewKind("canvas")?.pluginId).toBe("builtin.canvas");

    // 领域服务：ctx.canvas/ctx.table 由对应行提供，与平台服务并存。
    expect(kernel.ctx.get("canvas")).toBeDefined();
    expect(kernel.ctx.get("table")).toBeDefined();
    expect(kernel.ctx.canvas.snapshot()).toBeDefined();
    expect(kernel.ctx.table.snapshot()).toBeDefined();
    expect(kernel.ctx.get("vault")).toBeDefined();
    // 能力全开：ctx.note/ctx.chat 由对应行提供（停用即不可用）。
    expect(kernel.ctx.get("note")).toBeDefined();
    expect(kernel.ctx.get("chat")).toBeDefined();
    // 内核服务：ctx.history/ctx.layout/ctx.uiState 由内核提供（root 常驻）。
    expect(kernel.ctx.get("history")).toBeDefined();
    expect(kernel.ctx.get("layout")).toBeDefined();
    expect(kernel.ctx.get("uiState")).toBeDefined();

    // 卸载全部：槽与服务随 fiber 撤销。
    await unmountAll(kernel);
    expect(viewKinds()).toEqual([]);
    expect(kernel.ctx.get("canvas")).toBeUndefined();
    expect(kernel.ctx.get("table")).toBeUndefined();
    expect(kernel.ctx.get("note")).toBeUndefined();
    expect(kernel.ctx.get("chat")).toBeUndefined();
  });

  it("停用单行 = 只撤销该行的槽与服务，其余不受影响", async () => {
    kernel = createKernel();
    // 停用画布行（启用集合排除它）→ 不挂载，其余照常。
    await mountAll(kernel, packages.map((p) => (p.id === "builtin.canvas" ? { ...p, enabled: false } : p)));
    expect(mountedPluginIds(kernel)).not.toContain("builtin.canvas");
    expect(kernel.ctx.get("canvas")).toBeUndefined();
    expect(resolveViewKind("canvas")).toBeUndefined();
    expect(kernel.ctx.get("table")).toBeDefined();
    expect(resolveViewKind("table")).toBeDefined();
    expect(resolveViewKind("note")).toBeDefined();
  });

  it("用户插件（裸 apply，无 inject）可直接消费 ctx.table/ctx.canvas", async () => {
    kernel = createKernel();
    await mountAll(kernel);
    // 模拟用户插件：入口 = 默认导出 apply 函数（非 { inject, apply } 对象），直接访问 ctx.table/canvas。
    const consumer: { id: string; apply: (ctx: Context) => void } = {
      id: "com.test.consumer",
      apply: (ctx) => {
        expect(ctx.table.snapshot).toBeDefined();
        expect(ctx.canvas.snapshot).toBeDefined();
      },
    };
    const result = await mountPlugin(kernel, consumer);
    expect(result.ok).toBe(true);
  });

  it("用户插件可替换默认组合的 note 视图（作者侧 priority 声明，无特权）", async () => {
    kernel = createKernel();
    await mountAll(kernel);
    // 默认实现的 note 视图已注册（builtin.note，priority 0）。
    expect(resolveViewKind("note")?.pluginId).toBe("builtin.note");

    // 用户插件注册 view/note 且 priority 更高 → single 槽胜出，替换默认实现。
    const replacer: { id: string; apply: (ctx: Context) => void } = {
      id: "com.test.note",
      apply: (ctx) => {
        ctx.slots.registerView({ kind: "note", label: "我的笔记", component: () => null, priority: 10 });
      },
    };
    const result = await mountPlugin(kernel, replacer);
    expect(result.ok).toBe(true);
    const winner = resolveViewKind("note");
    expect(winner?.pluginId).toBe("com.test.note");
    expect(winner?.payload.label).toBe("我的笔记");
  });
});
