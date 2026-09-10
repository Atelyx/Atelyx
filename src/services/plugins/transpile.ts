/**
 * TypeScript 插件入口转译（esbuild-wasm）。
 *
 * TS 入口经此把 TS/TSX 转译成浏览器可直接执行的 ESM（默认导出 apply），供插件入口
 * blob import 求值挂载（见 services/cordis/packageMount）——插件发布源码即可用，无需构建产物；
 * 仓库自带 dist/ 预编译 JS（入口为 .js）时跳过转译（调用方判定）。转译按需执行、无缓存：
 * 插件加载低频（启用/重载时一次），esbuild 毫秒级，且本地目录插件改源码即时生效不被缓存污染。
 *
 * 注：esbuild.wasm（约 14MB）经 Vite `?url` 作为静态资源打包，CSP `script-src`
 * 需含 `'wasm-unsafe-eval'`（见 index.html）。
 */
import { initialize, transform } from "esbuild-wasm";
import esbuildWasmUrl from "esbuild-wasm/esbuild.wasm?url";

let initPromise: Promise<void> | null = null;

/** 初始化 esbuild 运行时（幂等；失败可重试）。node（测试）用默认 wasm 加载；浏览器（WebView）须显式传 wasmURL。
 *  esbuild-wasm 的 initialize 进程内只能调用一次——本模块与依赖方共用此初始化。 */
function ensureInit(): Promise<void> {
  if (!initPromise) {
    const options = typeof window === "undefined" ? {} : { wasmURL: esbuildWasmUrl };
    initPromise = initialize(options).catch((e) => {
      initPromise = null;
      throw e;
    });
  }
  return initPromise;
}

/** TS/TSX 源码 → ESM（默认导出 apply 的插件入口；WebView 经 blob import 求值挂载）。 */
export async function transpileEsm(source: string): Promise<string> {
  await ensureInit();
  const result = await transform(source, {
    loader: "tsx",
    format: "esm",
    target: "es2020",
    jsx: "transform",
  });
  return result.code;
}
