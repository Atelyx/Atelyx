import { useEffect } from "react";
import { useCanvasStore } from "@/stores/canvasStore";
import { useSettingsStore } from "@/stores/settingsStore";
import { matchesShortcut } from "@/utils/shortcutKeys";
import {
  BUILTIN_CANVAS_PLUGIN_ID,
  BUILTIN_COMMAND_SHORTCUT_DEFAULTS,
} from "@/constants/commandShortcuts";

/**
 * 画布快捷键：按命令注册表的生效键匹配（用户覆盖 → 内置默认，见 constants/commandShortcuts）。
 * 在 ProjectWorkspacePage 中调用一次即可。
 * 输入框/文本区聚焦时自动跳过快捷键；生效条件（面板聚焦且非只读）由 enabled 门控。
 */
function isInputTarget(e: KeyboardEvent) {
  return (
    e.target instanceof HTMLInputElement ||
    e.target instanceof HTMLTextAreaElement ||
    e.target instanceof HTMLSelectElement ||
    (e.target as HTMLElement)?.isContentEditable
  );
}

export function useCanvasHotkeys(
  onEscape?: () => void,
  enabled = true,
  onPaste?: () => boolean,
) {
  const getState = useCanvasStore.getState;
  const commandShortcuts = useSettingsStore((s) => s.commandShortcuts);
  const eff = (id: string) =>
    commandShortcuts[`${BUILTIN_CANVAS_PLUGIN_ID}:${id}`] ??
    BUILTIN_COMMAND_SHORTCUT_DEFAULTS[`${BUILTIN_CANVAS_PLUGIN_ID}:${id}`];
  const keys = {
    delete: eff("delete"),
    copy: eff("copy"),
    paste: eff("paste"),
    undo: eff("undo"),
    redo: eff("redo"),
    selectAll: eff("selectAll"),
  };

  useEffect(() => {
    if (!enabled) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (isInputTarget(e)) return;

      const store = getState();
      const {
        nodes,
        edges,
        onNodesChange,
        onEdgesChange,
        deleteSelected,
        undo,
        redo,
        copySelectedNodes,
      } = store;

      // Escape：取消选择 + 关联菜单收尾（浮层关闭语义与该键耦合，不开放自定义）
      if (e.key === "Escape") {
        if (nodes.some((n) => n.selected) || edges.some((e) => e.selected)) {
          onNodesChange(
            nodes
              .filter((n) => n.selected)
              .map((n) => ({ type: "select", id: n.id, selected: false })),
          );
          onEdgesChange(
            edges
              .filter((e) => e.selected)
              .map((e) => ({ type: "select", id: e.id, selected: false })),
          );
        }
        onEscape?.();
        return;
      }

      // 删除选中：默认 Delete；Backspace 保留为别名（编辑惯例，不单独开放；任意修饰键组合照常删除）
      if ((keys.delete && matchesShortcut(keys.delete, e)) || e.key === "Backspace") {
        e.preventDefault();
        deleteSelected();
        return;
      }

      if (keys.copy && matchesShortcut(keys.copy, e)) {
        // 有选中节点才接管为「复制节点」；无选中时放行浏览器默认文本复制
        // （文本节点预览/对话气泡是普通 div，isInputTarget 不拦截，无条件
        // preventDefault 会静默吞掉节点内的文本复制）
        if (copySelectedNodes()) e.preventDefault();
        return;
      }

      if (keys.paste && matchesShortcut(keys.paste, e)) {
        // 有画布节点可粘贴才接管；剪贴板无节点（如复制的是页面文本）时放行默认粘贴，
        // 否则无条件 preventDefault 会静默吞掉正文里的粘贴
        if (onPaste?.()) e.preventDefault();
        return;
      }

      if (keys.undo && matchesShortcut(keys.undo, e)) {
        e.preventDefault();
        undo();
        return;
      }

      if (keys.redo && matchesShortcut(keys.redo, e)) {
        e.preventDefault();
        redo();
        return;
      }
      // 重做别名：mod+shift+z（与笔记/表格撤销重做同一惯例；重做默认键未解绑时生效）
      if (
        keys.redo &&
        (e.ctrlKey || e.metaKey) &&
        e.shiftKey &&
        !e.altKey &&
        e.key.toLowerCase() === "z"
      ) {
        e.preventDefault();
        redo();
        return;
      }

      if (keys.selectAll && matchesShortcut(keys.selectAll, e) && nodes.length > 0) {
        e.preventDefault();
        onNodesChange(
          nodes.map((n) => ({ type: "select", id: n.id, selected: true })),
        );
      }
    };

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
    // 生效键随覆盖变更重绑；逐项展开保证引用变化即可触发
  }, [
    onEscape,
    getState,
    enabled,
    onPaste,
    keys.delete,
    keys.copy,
    keys.paste,
    keys.undo,
    keys.redo,
    keys.selectAll,
  ]);
}
