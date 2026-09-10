/**
 * 命令快捷键：主线程统一键盘监听，按插件命令注册的 `shortcut` 匹配执行。
 *
 * 快捷键格式 `mod+k`（mod = Ctrl/Cmd，与既有画布/编辑器快捷键一致）；监听经 installCommandHotkeys
 * 幂等安装一次（pluginStore.load 时）。输入框聚焦时跳过（防与打字冲突）。
 */
import { getPluginCommands } from "./ui";
import { usePluginStore } from "@/stores/pluginStore";

/** 修饰键白名单（快捷键解析用）。 */
const MODIFIERS = new Set(["mod", "ctrl", "meta", "shift", "alt"]);

/** 从快捷键 token 中取主键（首个非修饰键 token）。 */
function keyOf(parts: string[]): string | undefined {
  return parts.find((p) => !MODIFIERS.has(p));
}

/** 快捷键是否命中当前键盘事件（mod = Ctrl/Cmd 任一；主键大小写不敏感）。 */
export function matchesShortcut(shortcut: string, e: KeyboardEvent): boolean {
  const parts = shortcut.split("+").map((p) => p.trim().toLowerCase());
  const key = keyOf(parts);
  if (!key) return false;
  if (e.key.toLowerCase() !== key) return false;
  if (parts.includes("mod") && !(e.ctrlKey || e.metaKey)) return false;
  if (parts.includes("ctrl") && !e.ctrlKey) return false;
  if (parts.includes("meta") && !e.metaKey) return false;
  if (parts.includes("shift") && !e.shiftKey) return false;
  if (parts.includes("alt") && !e.altKey) return false;
  return true;
}

/** 输入/编辑态目标（快捷键跳过，防与打字/选区冲突）。 */
function isInputTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  return target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.isContentEditable;
}

let installed = false;

/** 安装主线程快捷键监听（幂等；window 未定义跳过——node 测试无 DOM）。 */
export function installCommandHotkeys(): void {
  if (installed || typeof window === "undefined") return;
  installed = true;
  window.addEventListener("keydown", (e) => {
    if (isInputTarget(e.target)) return;
    for (const c of getPluginCommands()) {
      if (c.shortcut && matchesShortcut(c.shortcut, e)) {
        e.preventDefault();
        const globalId = `${c.pluginId}:${c.id}`;
        void usePluginStore.getState().runPluginCommand(globalId).catch((err) => console.error(err));
        return;
      }
    }
  });
}
