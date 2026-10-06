/**
 * TypeScript 插件入口转译（esbuild-wasm）：TS/TSX → 浏览器可直接执行的 ESM（默认导出 apply），
 * 供插件入口 blob import 求值挂载（见 services/cordis/packageMount）——插件发布源码即可用，无需构建产物。
 */
import { initialize, transform } from "esbuild-wasm";
import esbuildWasmUrl from "esbuild-wasm/esbuild.wasm?url";

let initPromise: Promise<void> | null = null;

/** 初始化 esbuild 运行时（幂等；失败可重试）。node（测试）用默认 wasm 加载；浏览器（WebView）须显式传 wasmURL。
 *  esbuild-wasm 的 initialize 进程内只能调用一次——本模块与依赖方共用此初始化。
 *  wasm（约 14MB）经 Vite `?url` 作静态资源打包，CSP `script-src` 需含 `'wasm-unsafe-eval'`（见 index.html）。 */
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

/** TS/TSX 源码 → ESM（默认导出 apply 的插件入口；WebView 经 blob import 求值挂载）。
 * 按需执行、无缓存：插件加载低频（启用/重载时一次），且本地目录插件改源码须即时生效不被缓存污染。
 * 入口已是 .js 的预编译仓库由调用方跳过本步。 */
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
