/**
 * 浏览器侧插件挂载机制验证。
 * 链路：TS 插件源码 → esbuild-wasm 转译 → ESM 求值 → Cordis apply。
 * WebView 主路径为 blob: URL import（CSP 已含 blob:）；node 环境无法 import blob:，
 * 用 data: URL 代理（同一 ESM 求值路径）。Function 求值为兜底（CSP unsafe-inline）。
 * 注：现有转译器输出 iife（隔离上下文模型），WebView 挂载需 ESM 默认导出——
 * 转译器需补 esm 模式。
 */
import { Context } from "@atelyx/cordis";
import { initialize, transform } from "esbuild-wasm";
import esbuildWasmUrl from "esbuild-wasm/esbuild.wasm?url";
import { describe, expect, it } from "vitest";

/** 模拟第三方插件入口（TS，默认导出 apply）。 */
const TS_PLUGIN = `
export default function apply(ctx: any) {
  ctx.provide("spikeS1b", { ping: () => "pong" });
}
`;

let init: Promise<void> | null = null;
function ensureInit(): Promise<void> {
  if (!init) {
    // node（测试）用默认 wasm 加载；浏览器（WebView）须显式传 wasmURL。
    const options = typeof window === "undefined" ? {} : { wasmURL: esbuildWasmUrl };
    init = initialize(options).catch((e) => {
      init = null;
      throw e;
    });
  }
  return init;
}

async function transpileEsm(source: string): Promise<string> {
  await ensureInit();
  const result = await transform(source, {
    loader: "tsx",
    format: "esm",
    target: "es2020",
    jsx: "transform",
  });
  return result.code;
}

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
    await ensureInit();
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
