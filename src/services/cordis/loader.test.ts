/**
 * Cordis 挂载器测试（services/cordis/loader）。
 *
 * 验证：插件 ctx.plugin 挂载 → 服务/槽/事件生效 → 卸载随 fiber 撤销；
 * apply 抛错 → failed + 可读原因且不留残留；同 id 重复/并发挂载 = 串行替换（句柄不丢、能力不残留）。
 */
import { Context } from "@atelyx/cordis";
import { describe, expect, it, afterEach } from "vitest";
import { createKernel, type Kernel } from "./kernel";
import {
  contextToPluginId,
  mountPlugin,
  mountedPluginIds,
  unmountAll,
  unmountPlugin,
} from "./loader";
import { registerViewSlot } from "./slots";
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
    if (!result.ok) {
      expect(result.phase).toBe("apply");
      expect(result.message).toContain("初始化失败");
    }
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

  it("同 id 并发挂载串行执行：只留一个可卸载的 fiber，先到的实例被撤销", async () => {
    const k = makeKernel();
    let applied = 0;
    let disposed = 0;
    const apply = (ctx: Context) => {
      applied += 1;
      // 槽贡献的撤销函数必须经 effect 返回交给 Cordis 收集（槽注册表不是 ctx 服务，不会自动追踪）
      ctx.effect(() => {
        const off = registerViewSlot("loader-race", "builtin.race", { label: "并发" });
        return () => {
          disposed += 1;
          off();
        };
      });
    };
    // 同一 tick 两次挂载（双击启用/重复 load 场景）
    const [first, second] = await Promise.all([
      mountPlugin(k, { id: "builtin.race", apply }),
      mountPlugin(k, { id: "builtin.race", apply }),
    ]);
    expect(first).toEqual({ ok: true });
    expect(second).toEqual({ ok: true });
    expect(applied).toBe(2);
    // 第二次挂载先撤销第一次的 fiber：句柄不丢，能力不残留
    expect(disposed).toBe(1);
    expect(mountedPluginIds(k)).toEqual(["builtin.race"]);
    await unmountPlugin(k, "builtin.race");
    expect(disposed).toBe(2);
    expect(mountedPluginIds(k)).toEqual([]);
    expect(resolveViewKind("loader-race")).toBeUndefined();
  });

  it("挂载在途时卸载：先等挂载结算再卸载，无残留", async () => {
    const k = makeKernel();
    const mounting = mountPlugin(k, { id: "builtin.pending", apply: () => {} });
    await unmountPlugin(k, "builtin.pending");
    expect(await mounting).toEqual({ ok: true });
    expect(mountedPluginIds(k)).toEqual([]);
  });

  it("清场与在途挂载并发：unmountAll 收敛后无残留", async () => {
    const k = makeKernel();
    const mounting = mountPlugin(k, { id: "builtin.pending", apply: () => {} });
    await unmountAll(k);
    expect(await mounting).toEqual({ ok: true });
    expect(mountedPluginIds(k)).toEqual([]);
  });
});
