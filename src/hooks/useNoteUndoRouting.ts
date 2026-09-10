/**
 * 笔记撤销/重做快捷键路由：Mod-Z / Mod-Y / Mod-Shift-Z 按焦点所在编辑面（`data-note-file`）
 * 归属到该文件的会话。编辑面可以是笔记面板，也可以是画布上的笔记节点，各文件撤销栈互不混淆。
 *
 * 未命中笔记编辑面时不拦截：画布自身的撤销由 useCanvasHotkeys 处理（CodeMirror 聚焦时其
 * isInputTarget 已让路）。属性区/弹层输入框留给浏览器原生撤销。
 *
 * 监听器按窗口单例：编辑面可能同时存在多个（面板 + 多个节点），重复挂载会重复执行撤销。
 */

import { useEffect } from "react";
import { getOpenNoteSession } from "@/hooks/useNoteBodySession";

function onKeyDown(e: KeyboardEvent): void {
  if (!e.ctrlKey && !e.metaKey) return;
  const key = e.key.toLowerCase();
  if (key !== "z" && key !== "y") return;
  const target = e.target instanceof Element ? e.target : null;
  if (target instanceof HTMLInputElement || target instanceof HTMLSelectElement) return;
  if (target instanceof HTMLTextAreaElement && !target.hasAttribute("data-note-content")) {
    return;
  }
  const file = target?.closest("[data-note-file]")?.getAttribute("data-note-file");
  const session = file ? getOpenNoteSession(file) : null;
  if (!session) return;
  e.preventDefault();
  if (key === "y" || e.shiftKey) session.redo();
  else session.undo();
}

let mountedCount = 0;

export function useNoteUndoRouting(): void {
  useEffect(() => {
    if (mountedCount++ === 0) window.addEventListener("keydown", onKeyDown);
    return () => {
      if (--mountedCount === 0) window.removeEventListener("keydown", onKeyDown);
    };
  }, []);
}
