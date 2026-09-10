/**
 * 第一方插件集成测试：CORDIS_BUILTIN_DEFS 全部经 loader 挂载到真实内核。
 *
 * 验证 pluginStore.spawn 的内置路径等价语义（apply 装配视图槽 + 领域服务 + 接线）：
 * - 挂载后 ctx.canvas/ctx.table 可用（builtin.canvas/table 提供）、视图槽可解析、mountProfile 顺序
 * - 卸载（fiber dispose）全部撤销：槽消失、服务消失、能力接线复位
 */
import { describe, expect, it, afterEach } from "vitest";
import type { Context } from "@atelyx/cordis";
import { setPluginCanvasAccess, setPluginTableRuntimeAccess } from "./access";
import { CORDIS_BUILTIN_DEFS, type CordisBuiltinDef } from "@/components/plugins/cordis/builtins";
import { createKernel, type Kernel } from "./kernel";
import { mountPlugin, mountProfile, unmountAll, unmountPlugin } from "./loader";
import { resolveViewKind, viewKinds } from "./slots";
import type { Profile } from "@/utils/cordis/composition";

const profile: Profile = {
  name: "default",
  plugins: CORDIS_BUILTIN_DEFS.map((d) => ({ id: d.id, order: d.order, defaultEnabled: true })),
};
const byId: Record<string, Pick<CordisBuiltinDef, "id" | "apply">> = Object.fromEntries(
  CORDIS_BUILTIN_DEFS.map((d) => [d.id, { id: d.id, apply: d.apply }]),
);

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

describe("第一方插件挂载集成", () => {
  it("全部内置插件可挂载：视图槽/领域服务就绪；卸载后全部撤销", async () => {
    kernel = createKernel();
    const { failed } = await mountProfile(
      kernel,
      profile,
      byId as never,
      new Set(CORDIS_BUILTIN_DEFS.map((d) => d.id)),
    );
    expect(failed).toEqual([]);

    // 视图槽：内置视图 kind 全部注册（theme 无视图）。
    const kinds = viewKinds();
    expect(kinds).toContain("canvas");
    expect(kinds).toContain("table");
    expect(kinds).toContain("note");
    expect(resolveViewKind("canvas")?.pluginId).toBe("builtin.canvas");

    // 领域服务：ctx.canvas/ctx.table 由对应插件提供，与平台服务并存。
    expect(kernel.ctx.get("canvas")).toBeDefined();
    expect(kernel.ctx.get("table")).toBeDefined();
    expect(kernel.ctx.canvas.snapshot()).toBeDefined();
    expect(kernel.ctx.table.snapshot()).toBeDefined();
    expect(kernel.ctx.get("vault")).toBeDefined();
    // 能力全开：ctx.note/ctx.chat 由内置插件提供（停用即不可用）。
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

  it("停用单插件 = 只撤销该插件的槽与服务，其余不受影响", async () => {
    kernel = createKernel();
    await mountProfile(
      kernel,
      profile,
      byId as never,
      new Set(CORDIS_BUILTIN_DEFS.map((d) => d.id)),
    );
    expect(kernel.ctx.get("canvas")).toBeDefined();
    expect(kernel.ctx.get("table")).toBeDefined();

    await unmountPlugin(kernel, "builtin.canvas");
    expect(kernel.ctx.get("canvas")).toBeUndefined();
    expect(resolveViewKind("canvas")).toBeUndefined();
    expect(kernel.ctx.get("table")).toBeDefined();
    expect(resolveViewKind("table")).toBeDefined();
    expect(resolveViewKind("note")).toBeDefined();
  });

  it("第三方插件（裸 apply，无 inject）可直接消费 ctx.table/ctx.canvas", async () => {
    kernel = createKernel();
    await mountProfile(
      kernel,
      profile,
      byId as never,
      new Set(CORDIS_BUILTIN_DEFS.map((d) => d.id)),
    );
    // 模拟第三方插件：入口 = 默认导出 apply 函数（非 { inject, apply } 对象），直接访问 ctx.table/canvas。
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

  it("第三方可替换内置 note 视图（single 槽高 priority 胜出，无特权报错）", async () => {
    kernel = createKernel();
    await mountProfile(
      kernel,
      profile,
      byId as never,
      new Set(CORDIS_BUILTIN_DEFS.map((d) => d.id)),
    );
    // 内置 note 视图已注册（builtin.note，priority 0）。
    expect(resolveViewKind("note")?.pluginId).toBe("builtin.note");

    // 第三方注册 view/note 且 priority 更高 → single 槽胜出，替换内置实现。
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
