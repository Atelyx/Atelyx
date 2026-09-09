/**
 * TS 入口转译测试（services/plugins/transpile）。
 *
 * esm 模式为浏览器侧挂载的入口预处理器（默认导出 apply）。
 * 注：esbuild 的 esm 输出为 `export { apply as default }` 形式，只断言 export 存在。
 */
import { describe, expect, it } from "vitest";
import { transpileEsm } from "./transpile";

describe("TS 入口转译", () => {
  it("esm 模式输出模块导出", async () => {
    const code = await transpileEsm(
      "export default function apply(ctx: any) { ctx.provide('x', { ping: () => 'pong' }); }",
    );
    expect(code).toContain("export");
    expect(code).toContain("apply");
  });
});
