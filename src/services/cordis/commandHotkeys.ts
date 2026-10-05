/**
 * 命令快捷键：主线程统一键盘监听，按插件命令注册的生效键匹配执行。
 *
 * 生效键 = 用户覆盖（global.json，调用方注入 getter）→ 命令声明的默认 shortcut；仅分发
 * `scope: "global"` 的命令——作用域类命令（画布/笔记/表格）的匹配逻辑留在对应视图的既有
 * 监听点（面板聚焦、编辑面归属等上下文判断与视图生命周期绑定），注册表只承载键位数据。
 * 监听经 installCommandHotkeys 幂等安装一次（pluginStore.load 时）。输入框聚焦时跳过。
 */
import { matchesShortcut } from "@/utils/shortcutKeys";
import { getPluginCommands } from "./ui";
import type { PluginCommandRegistration } from "./ui";

/** 输入/编辑态目标（快捷键跳过，防与打字/选区冲突）。 */
function isInputTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  return target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.isContentEditable;
}

/** 命中命令的执行：同步异常与异步 rejection 均收敛到 console.error（不冒出键盘监听器）。 */
function runIsolated(run: () => unknown): void {
  try {
    const result = run() as unknown;
    if (result instanceof Promise) {
      result.catch((err) => console.error(err));
    }
  } catch (err) {
    console.error(err);
  }
}

/** 事件分发：按注册表顺序找首条命中命令，preventDefault 后执行；返回是否命中。
 *  overrides = 命令 globalId → 自定义键串（无覆盖条目的命令用声明默认键）。 */
export function dispatchHotkey(
  e: KeyboardEvent,
  commands: PluginCommandRegistration[],
  overrides: Record<string, string> = {},
): boolean {
  for (const c of commands) {
    if (c.scope !== "global") continue;
    const effective = overrides[`${c.pluginId}:${c.id}`] ?? c.shortcut;
    if (effective && matchesShortcut(effective, e)) {
      e.preventDefault();
      // 直接执行注册表里的回调（注册时已绑定调用方插件 fiber，随停用撤销）
      runIsolated(c.run);
      return true;
    }
  }
  return false;
}

let installed = false;

/** 安装主线程快捷键监听（幂等；window 未定义跳过——node 测试无 DOM）。
 *  getOverrides 注入用户覆盖表（settingsStore 的 commandShortcuts），命中时现查。 */
export function installCommandHotkeys(getOverrides?: () => Record<string, string>): void {
  if (installed || typeof window === "undefined") return;
  installed = true;
  window.addEventListener("keydown", (e) => {
    if (isInputTarget(e.target)) return;
    dispatchHotkey(e, getPluginCommands(), getOverrides?.() ?? {});
  });
}
