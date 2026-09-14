/**
 * 命令快捷键：主线程统一键盘监听，按插件命令注册的 `shortcut` 匹配执行。
 *
 * 快捷键格式 `mod+k`（mod = Ctrl/Cmd 任一，与既有画布/编辑器快捷键一致）；匹配为修饰键
 * 集合精确相等——多按的修饰键不算命中（注册 ctrl+k 时 Ctrl+Shift+K 不触发，防插件命令
 * 与用户刻意组合键冲突）。监听经 installCommandHotkeys 幂等安装一次（pluginStore.load 时）。
 * 输入框聚焦时跳过（防与打字冲突）。
 */
import { getPluginCommands } from "./ui";
import type { PluginCommandRegistration } from "./ui";

/** 修饰键白名单（快捷键解析用）。 */
const MODIFIERS = new Set(["mod", "ctrl", "meta", "shift", "alt"]);

/** 从快捷键 token 中取主键（首个非修饰键 token）。 */
function keyOf(parts: string[]): string | undefined {
  return parts.find((p) => !MODIFIERS.has(p));
}

/** 快捷键声明的修饰键匹配集（物理修饰键的精确组合）：mod 声明 = 「其余键 + ctrl」或「其余键 + meta」两套替代。 */
function declaredModifierSets(parts: string[]): string[][] {
  const rest = parts.filter((p) => MODIFIERS.has(p) && p !== "mod");
  if (parts.includes("mod")) return [[...rest, "ctrl"], [...rest, "meta"]];
  return [rest];
}

/** 快捷键是否命中当前键盘事件（mod = Ctrl/Cmd 任一；主键大小写不敏感；修饰键集合精确匹配）。 */
export function matchesShortcut(shortcut: string, e: KeyboardEvent): boolean {
  const parts = shortcut.split("+").map((p) => p.trim().toLowerCase());
  const key = keyOf(parts);
  if (!key) return false;
  if (e.key.toLowerCase() !== key) return false;
  const pressed: string[] = [];
  if (e.ctrlKey) pressed.push("ctrl");
  if (e.metaKey) pressed.push("meta");
  if (e.shiftKey) pressed.push("shift");
  if (e.altKey) pressed.push("alt");
  return declaredModifierSets(parts).some(
    (alt) => alt.length === pressed.length && alt.every((m) => pressed.includes(m)),
  );
}

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

/** 事件分发：按注册表顺序找首条命中命令，preventDefault 后执行；返回是否命中。 */
export function dispatchHotkey(e: KeyboardEvent, commands: PluginCommandRegistration[]): boolean {
  for (const c of commands) {
    if (c.shortcut && matchesShortcut(c.shortcut, e)) {
      e.preventDefault();
      // 直接执行注册表里的回调（注册时已绑定调用方插件 fiber，随停用撤销）
      runIsolated(c.run);
      return true;
    }
  }
  return false;
}

let installed = false;

/** 安装主线程快捷键监听（幂等；window 未定义跳过——node 测试无 DOM）。 */
export function installCommandHotkeys(): void {
  if (installed || typeof window === "undefined") return;
  installed = true;
  window.addEventListener("keydown", (e) => {
    if (isInputTarget(e.target)) return;
    dispatchHotkey(e, getPluginCommands());
  });
}
