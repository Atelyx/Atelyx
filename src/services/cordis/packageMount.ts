/**
 * 插件包入口挂载：入口源码 → ESM 求值 → Cordis apply。
 *
 * 链路：Rust `plugin_read_entry` 读取入口 → `.ts/.tsx` 经 esbuild-wasm 转译 ESM → 动态
 * import 求值（WebView 用 blob: URL，node 测试用 data: URL）→ 取默认导出（apply）→
 * mountPlugin 挂载。入口须自包含（无运行时 import/export 依赖；`import type` 为类型注解，
 * 转译时擦除）；入口扩展名在安装/读取时由清单校验限制为 .js/.ts/.tsx。
 */
import type { Plugin } from "@atelyx/cordis";
import { errText } from "@/types";
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

/** 挂载插件包实现（入口 = 清单 main；读取 → 转译 → 求值 → 挂载，各段失败归入对应阶段）。 */
export async function mountPluginFromPackage(kernel: Kernel, id: string, main: string): Promise<MountResult> {
  let code: string;
  try {
    code = await pluginReadEntry(id, main);
  } catch (e) {
    return { ok: false, phase: "read", message: errText(e) };
  }
  let js: string;
  try {
    js = /\.tsx?$/i.test(main) ? await transpileEsm(code) : code;
  } catch (e) {
    return { ok: false, phase: "transpile", message: errText(e) };
  }
  let apply: unknown;
  try {
    apply = await evaluatePluginModule(js);
  } catch (e) {
    return { ok: false, phase: "eval", message: errText(e) };
  }
  if (!isApply(apply)) {
    return { ok: false, phase: "eval", message: "插件入口未导出 apply 函数" };
  }
  return mountPlugin(kernel, { id, apply });
}
