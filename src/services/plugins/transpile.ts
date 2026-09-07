/**
 * TypeScript 插件入口转译（esbuild-wasm）。
 *
 * `runtime: "ts"` 的插件入口经此把 TS/TSX 转译成浏览器可直接执行的 JS，再走既有
 * worker/blob 路径——插件发布源码即可用，无需构建产物；仓库自带 dist/ 预编译 JS
 * （入口为 .js）时跳过转译（调用方判定）。转译按需执行、无缓存：插件加载低频
 * （启用/重载时一次），esbuild 毫秒级，且本地目录插件改源码即时生效不被缓存污染。
 *
 * 注：esbuild.wasm（约 14MB）经 Vite `?url` 作为静态资源打包，CSP `script-src`
 * 需含 `'wasm-unsafe-eval'`（见 index.html）。
 */
import { initialize, transform } from "esbuild-wasm";
import esbuildWasmUrl from "esbuild-wasm/esbuild.wasm?url";

let initPromise: Promise<void> | null = null;

/** 初始化 esbuild 运行时（幂等；失败可重试）。 */
function ensureInit(): Promise<void> {
  if (!initPromise) {
    initPromise = initialize({ wasmURL: esbuildWasmUrl }).catch((e) => {
      initPromise = null;
      throw e;
    });
  }
  return initPromise;
}

/** TS/TSX 源码 → 浏览器可执行 JS（iife；JSX 转 `React.createElement`——主线程 UI 平面
 *  由 loadUiPlugin 注入 React 变量；逻辑平面不渲染、用 JSX 会在运行时因 React 未定义报错）。 */
export async function transpileTs(source: string): Promise<string> {
  await ensureInit();
  const result = await transform(source, {
    loader: "tsx",
    format: "iife",
    target: "es2020",
    jsx: "transform",
    jsxFactory: "React.createElement",
    jsxFragment: "React.Fragment",
  });
  return result.code;
}
