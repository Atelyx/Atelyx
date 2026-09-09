/**
 * Cordis 挂载器测试（services/cordis/loader）。
 *
 * 验证：第一方插件 ctx.plugin 挂载 → 服务/槽/事件生效 → 卸载随 fiber 撤销；
 * apply 抛错 → failed + 可读原因且不留残留；装配按 profile × 启用集合顺序挂载。
 */
import { Context } from "@atelyx/cordis";
import { describe, expect, it, afterEach } from "vitest";
import { createKernel, type Kernel } from "./kernel";
import {
  contextToPluginId,
  mountPlugin,
  mountProfile,
  mountedPluginIds,
  unmountAll,
  unmountPlugin,
} from "./loader";
import { registerViewSlot } from "./slots";
import type { Profile } from "@/utils/cordis/composition";
import { resolveViewKind } from "./slots";

declare module "@atelyx/cordis" {
  interface Context {
    loaderSvc: { ping: () => string };
  }
  interface Events {
    "loader/evt": (msg: string) => void;
  }
}

let kernel: Kernel | null = null;

function makeKernel(): Kernel {
  kernel = createKernel();
  return kernel;
}

afterEach(async () => {
  if (kernel) {
    await unmountAll(kernel);
    kernel.dispose();
    kernel = null;
  }
});

describe("Cordis 挂载器", () => {
  it("挂载：服务/槽/事件生效；卸载随 fiber 撤销", async () => {
    const k = makeKernel();
    const seen: string[] = [];
    let pluginCtx: Context | null = null;
    const apply = (ctx: Context) => {
      pluginCtx = ctx;
      ctx.provide("loaderSvc", { ping: () => "pong" });
      ctx.on("loader/evt", (msg) => {
        seen.push(msg);
      });
      // 槽注册经 ctx.effect：Cordis 持有其生命周期（apply 中途抛错/卸载均自动撤销）
      ctx.effect(() => registerViewSlot("loader-view", "builtin.loader", { label: "装载视图" }));
    };
    const result = await mountPlugin(k, { id: "builtin.loader", apply });
    expect(result).toEqual({ ok: true });
    // 审计归属登记：插件上下文 → 插件 id。
    expect(pluginCtx).not.toBeNull();
    expect(contextToPluginId.get(pluginCtx!)).toBe("builtin.loader");
    expect(k.ctx.get("loaderSvc")).toBeDefined();
    expect(k.ctx.loaderSvc.ping()).toBe("pong");
    expect(resolveViewKind("loader-view")?.pluginId).toBe("builtin.loader");
    k.ctx.emit("loader/evt", "x");
    expect(seen).toEqual(["x"]);

    await unmountPlugin(k, "builtin.loader");
    expect(k.ctx.get("loaderSvc")).toBeUndefined();
    expect(resolveViewKind("loader-view")).toBeUndefined();
    k.ctx.emit("loader/evt", "y");
    expect(seen).toEqual(["x"]);
  });

  it("apply 抛错 → failed + 可读原因，不留残留，可再挂载", async () => {
    const k = makeKernel();
    const boom = (ctx: Context) => {
      ctx.effect(() => registerViewSlot("loader-boom", "builtin.boom", { label: "炸" }));
      throw new Error("初始化失败");
    };
    const result = await mountPlugin(k, { id: "builtin.boom", apply: boom });
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.reason).toContain("初始化失败");
    expect(mountedPluginIds(k)).toEqual([]);
    expect(resolveViewKind("loader-boom")).toBeUndefined(); // 失败即撤销已注册贡献

    const ok = await mountPlugin(k, { id: "builtin.boom", apply: () => {} });
    expect(ok).toEqual({ ok: true });
    await unmountPlugin(k, "builtin.boom");
  });

  it("同 id 重复挂载 = 替换（无重复注册残留）", async () => {
    const k = makeKernel();
    const apply = (ctx: Context) => {
      ctx.provide("loaderSvc", { ping: () => "pong" });
    };
    await mountPlugin(k, { id: "builtin.loader", apply });
    await mountPlugin(k, { id: "builtin.loader", apply }); // 二次挂载：先撤销旧 fiber
    expect(k.ctx.get("loaderSvc")).toBeDefined();
    await unmountPlugin(k, "builtin.loader");
    expect(k.ctx.get("loaderSvc")).toBeUndefined();
  });

  it("装配：profile × 启用集合按序挂载，失败清单收集", async () => {
    const k = makeKernel();
    const profile: Profile = {
      name: "default",
      plugins: [
        { id: "builtin.a", order: 1, defaultEnabled: true },
        { id: "builtin.b", order: 2, defaultEnabled: true },
        { id: "builtin.c", order: 3, defaultEnabled: true },
      ],
    };
    const plugins = {
      "builtin.a": { id: "builtin.a", apply: (ctx: Context) => { ctx.provide("loaderSvc", { ping: () => "a" }); } },
      "builtin.b": { id: "builtin.b", apply: () => { throw new Error("b 失败"); } },
      "builtin.c": { id: "builtin.c", apply: () => {} },
    };
    const { failed } = await mountProfile(k, profile, plugins, new Set(["builtin.a", "builtin.b", "builtin.c"]));
    expect(failed).toEqual([{ id: "builtin.b", reason: "b 失败" }]);
    expect(mountedPluginIds(k).sort()).toEqual(["builtin.a", "builtin.c"]);
    expect(k.ctx.loaderSvc.ping()).toBe("a");
    // 未启用的不挂载
    await unmountAll(k);
    await mountProfile(k, profile, plugins, new Set(["builtin.c"]));
    expect(mountedPluginIds(k)).toEqual(["builtin.c"]);
  });
});
