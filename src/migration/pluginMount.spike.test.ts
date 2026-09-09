/**
 * 浏览器侧插件挂载机制验证（第三方入口机制的回归锚点）。
 * 链路：TS 插件源码 → transpileEsm（真实模块）→ ESM 求值 → Cordis apply。
 * WebView 主路径为 blob: URL import（CSP 已含 blob:）；node 环境无法 import blob:，
 * 用 data: URL 代理（同一 ESM 求值路径）。Function 求值为兜底（CSP unsafe-inline）。
 */
import { Context } from "@atelyx/cordis";
import { transform } from "esbuild-wasm";
import { describe, expect, it } from "vitest";
import { ensureEsbuildInit, transpileEsm } from "@/services/plugins/transpile";

/** 模拟第三方插件入口（TS，默认导出 apply）。 */
const TS_PLUGIN = `
export default function apply(ctx: any) {
  ctx.provide("spikeS1b", { ping: () => "pong" });
}
`;

describe("浏览器侧插件挂载机制", () => {
  it("TS → ESM → import 求值 → Cordis apply → 卸载撤销", async () => {
    const js = await transpileEsm(TS_PLUGIN);
    const mod = (await import(
      "data:text/javascript;base64," + Buffer.from(js, "utf8").toString("base64"),
    )) as { default: (ctx: Context) => void };

    const ctx = new Context();
    const fiber = ctx.plugin(mod.default);
    await fiber.await();
    expect(ctx.get("spikeS1b")).toBeDefined();
    await fiber.dispose();
    expect(ctx.get("spikeS1b")).toBeUndefined();
  });

  it("Function 求值兜底（globalName 包装取回默认导出）", async () => {
    await ensureEsbuildInit();
    const code = (
      await transform(TS_PLUGIN, {
        loader: "tsx",
        format: "iife",
        globalName: "plugin",
        target: "es2020",
        jsx: "transform",
      })
    ).code;
    const getMod = new Function(`${code}; return plugin;`) as () => {
      default: (ctx: Context) => void;
    };

    const ctx = new Context();
    const fiber = ctx.plugin(getMod().default);
    await fiber.await();
    expect(ctx.get("spikeS1b")).toBeDefined();
    await fiber.dispose();
  });
});
