/**
 * 笔记撤销/重做快捷键路由：按焦点所在编辑面（`data-note-file`）归属到该文件的会话。
 * 生效键 = 用户覆盖 → 内置默认（constants/commandShortcuts）；mod+shift+z 保留为重做别名
 * （与表格同一惯例，不单独开放；撤销绑定命中时优先于该别名）。编辑面可以是笔记面板，
 * 也可以是画布上的笔记节点，各文件撤销栈互不混淆。
 *
 * 未命中笔记编辑面时不拦截：画布自身的撤销由 useCanvasHotkeys 处理（编辑输入面为 textarea，
 * 其 isInputTarget 已让路）。属性区/弹层输入框留给浏览器原生撤销。
 *
 * 监听器按窗口单例：编辑面可能同时存在多个（面板 + 多个节点），重复挂载会重复执行撤销；
 * 生效键存模块级镜像、渲染期同步，覆盖变更不重挂监听。
 */

import { useEffect } from "react";
import { getOpenNoteSession } from "@/hooks/useNoteBodySession";
import { useSettingsStore } from "@/stores/settingsStore";
import { matchesShortcut } from "@/utils/shortcutKeys";
import {
  BUILTIN_COMMAND_SHORTCUT_DEFAULTS,
  BUILTIN_NOTE_PLUGIN_ID,
} from "@/constants/commandShortcuts";

/** 当前生效键镜像（渲染期由 useNoteUndoRouting 同步；监听器回调现查）。 */
const effectiveKeys: { undo?: string; redo?: string } = {};

function onKeyDown(e: KeyboardEvent): void {
  // 已被更内层处理的事件不重复消费；IME 组合中的 z/y 是正文输入而非撤销命令
  if (e.defaultPrevented || e.isComposing) return;
  if (!e.ctrlKey && !e.metaKey) return;
  // 撤销绑定命中优先消费；重做 = 绑定命中或 mod+shift+z 别名（惯例键，恒生效）
  if (effectiveKeys.undo !== undefined && matchesShortcut(effectiveKeys.undo, e)) {
    route(e, "undo");
    return;
  }
  const hitRedo =
    (effectiveKeys.redo !== undefined && matchesShortcut(effectiveKeys.redo, e)) ||
    (e.shiftKey && !e.altKey && e.key.toLowerCase() === "z");
  if (hitRedo) route(e, "redo");
}

/** 按焦点所在编辑面归属会话并执行（未命中编辑面不拦截）。 */
function route(e: KeyboardEvent, action: "undo" | "redo"): void {
  const target = e.target instanceof Element ? e.target : null;
  if (target instanceof HTMLInputElement || target instanceof HTMLSelectElement) return;
  if (target instanceof HTMLTextAreaElement && !target.hasAttribute("data-note-content")) {
    return;
  }
  const file = target?.closest("[data-note-file]")?.getAttribute("data-note-file");
  const session = file ? getOpenNoteSession(file) : null;
  if (!session) return;
  e.preventDefault();
  if (action === "redo") session.redo();
  else session.undo();
}

let mountedCount = 0;

export function useNoteUndoRouting(): void {
  const commandShortcuts = useSettingsStore((s) => s.commandShortcuts);
  effectiveKeys.undo =
    commandShortcuts[`${BUILTIN_NOTE_PLUGIN_ID}:undo`] ??
    BUILTIN_COMMAND_SHORTCUT_DEFAULTS[`${BUILTIN_NOTE_PLUGIN_ID}:undo`];
  effectiveKeys.redo =
    commandShortcuts[`${BUILTIN_NOTE_PLUGIN_ID}:redo`] ??
    BUILTIN_COMMAND_SHORTCUT_DEFAULTS[`${BUILTIN_NOTE_PLUGIN_ID}:redo`];

  useEffect(() => {
    if (mountedCount++ === 0) window.addEventListener("keydown", onKeyDown);
    return () => {
      if (--mountedCount === 0) window.removeEventListener("keydown", onKeyDown);
    };
  }, []);
}
