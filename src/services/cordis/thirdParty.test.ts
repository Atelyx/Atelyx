/**
 * 第三方插件挂载测试（services/cordis/thirdParty + loader 扩展）。
 *
 * 覆盖：TS 入口 → ESM 求值 → apply 挂载/卸载；apply 对象（inject）缺失依赖 → failed + 缺失清单；
 * inject 满足 → 激活；.py 入口拒绝；ctx→pluginId 审计归属。
 */
import { describe, expect, it, afterEach } from "vitest";
import type { Context } from "@atelyx/cordis";
import { transpileEsm } from "@/services/plugins/transpile";
import { createKernel, type Kernel } from "./kernel";
import { evaluatePluginModule, mountThirdPartyPlugin } from "./thirdParty";
import { contextToPluginId, mountPlugin, unmountAll } from "./loader";

const TS_APPLY = `
import type { Context } from "@atelyx/cordis";
export default function apply(ctx: Context) {
  ctx.provide("tpSvc", { ping: () => "pong" });
}
`;

let kernel: Kernel | null = null;

afterEach(async () => {
  if (kernel) {
    await unmountAll(kernel);
    kernel.dispose();
    kernel = null;
  }
});

describe("第三方插件挂载", () => {
  it("TS 入口 → ESM 求值 → apply 挂载；卸载随 fiber 撤销", async () => {
    kernel = createKernel();
    const js = await transpileEsm(TS_APPLY);
    const apply = await evaluatePluginModule(js);
    expect(typeof apply).toBe("function");

    const result = await mountPlugin(kernel, { id: "com.test.tp", apply: apply as never });
    expect(result).toEqual({ ok: true });
    expect(kernel.ctx.get("tpSvc")).toBeDefined();
    expect(kernel.ctx.get("tpSvc")!.ping()).toBe("pong");

    await unmountAll(kernel);
    expect(kernel.ctx.get("tpSvc")).toBeUndefined();
  });

  it("apply 对象（inject）：依赖缺失 → failed + 缺失清单", async () => {
    kernel = createKernel();
    const apply = {
      name: "tp-dep",
      inject: ["table"],
      apply: () => {},
    };
    const result = await mountPlugin(kernel, { id: "com.test.dep", apply });
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.reason).toContain("依赖服务未提供：table");
  });

  it("apply 对象（inject）：依赖满足 → 激活", async () => {
    kernel = createKernel();
    kernel.ctx.provide("table", { ping: () => "pong" } as never);
    const apply = {
      name: "tp-dep",
      inject: ["table"],
      apply: (ctx: Context) => {
        ctx.provide("depSvc", { ok: true });
      },
    };
    const result = await mountPlugin(kernel, { id: "com.test.dep", apply });
    expect(result).toEqual({ ok: true });
    expect(kernel.ctx.get("depSvc")).toBeDefined();
  });

  it("审计归属：求值模块 apply 挂载后 ctx → 插件 id 登记", async () => {
    kernel = createKernel();
    const js = await transpileEsm(TS_APPLY);
    const apply = (await evaluatePluginModule(js)) as (ctx: Context) => void;
    let pluginCtx: object | null = null;
    const wrapper = (ctx: Context) => {
      pluginCtx = ctx as object;
      apply(ctx);
    };
    await mountPlugin(kernel, { id: "com.test.tp", apply: wrapper as never });
    expect(pluginCtx).not.toBeNull();
    expect(contextToPluginId.get(pluginCtx!)).toBe("com.test.tp");
  });

  it("mountThirdPartyPlugin 拒绝 .py 入口", async () => {
    kernel = createKernel();
    const result = await mountThirdPartyPlugin(kernel, "com.test.py", "main.py");
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.reason).toContain("Python 插件运行时已不受支持");
  });
});
