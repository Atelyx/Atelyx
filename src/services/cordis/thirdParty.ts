/**
 * 第三方插件浏览器侧挂载：入口源码 → ESM 求值 → Cordis apply。
 *
 * 链路：Rust `plugin_read_entry` 读取入口 → `.ts/.tsx` 经 esbuild-wasm 转译 ESM → 动态
 * import 求值（WebView 用 blob: URL，node 测试用 data: URL）→ 取默认导出（apply）→
 * mountPlugin 挂载。入口须自包含（无运行时 import/export 依赖；`import type` 为类型注解，
 * 转译时擦除）；Python 入口直接拒绝。
 */
import type { Plugin } from "@atelyx/cordis";
import { pluginReadEntry } from "@/services/plugins";
import { transpileEsm } from "@/services/plugins/transpile";
import type { Kernel } from "./kernel";
import { mountPlugin, type MountResult } from "./loader";

/** 入口默认导出是否为合法 apply（函数或含 apply 方法的对象）。 */
function isApply(value: unknown): value is Plugin {
  return (
    typeof value === "function" ||
    (typeof value === "object" && value !== null && typeof (value as { apply?: unknown }).apply === "function")
  );
}

/** 求值 ESM 模块并返回默认导出（apply；函数或 { apply, inject, ... } 对象）。 */
export async function evaluatePluginModule(code: string): Promise<unknown> {
  if (typeof window === "undefined") {
    const mod = await import(/* @vite-ignore */ "data:text/javascript;base64," + Buffer.from(code, "utf8").toString("base64"));
    return mod.default;
  }
  const url = URL.createObjectURL(new Blob([code], { type: "text/javascript" }));
  try {
    const mod = (await import(/* @vite-ignore */ url)) as { default?: unknown };
    return mod.default;
  } finally {
    URL.revokeObjectURL(url);
  }
}

/** 挂载第三方插件（入口 = 清单 main；.py 拒绝；转译后求值挂载）。 */
export async function mountThirdPartyPlugin(
  kernel: Kernel,
  id: string,
  main: string,
): Promise<MountResult> {
  if (/\.py$/i.test(main)) {
    return { ok: false, reason: "Python 插件运行时已不受支持（请使用 JS/TS）" };
  }
  const code = await pluginReadEntry(id, main);
  const js = /\.tsx?$/i.test(main) ? await transpileEsm(code) : code;
  const apply = await evaluatePluginModule(js);
  if (!isApply(apply)) {
    return { ok: false, reason: "插件入口未导出 apply 函数" };
  }
  return mountPlugin(kernel, { id, apply });
}
