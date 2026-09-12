import { useEffect } from "react";
import { useCanvasStore } from "@/stores/canvasStore";

/**
 * 画布快捷键。
 * 在 ProjectWorkspacePage 中调用一次即可。
 * 输入框/文本区聚焦时自动跳过快捷键。
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

      switch (e.key) {
        case "Delete":
        case "Backspace": {
          e.preventDefault();
          deleteSelected();
          break;
        }

        case "Escape": {
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
          break;
        }

        case "c":
        case "C": {
          if (e.ctrlKey || e.metaKey) {
            // 有选中节点才接管为「复制节点」；无选中时放行浏览器默认文本复制
            // （文本节点预览/对话气泡是普通 div，isInputTarget 不拦截，无条件
            // preventDefault 会静默吞掉节点内的文本复制）
            if (copySelectedNodes()) e.preventDefault();
          }
          break;
        }

        case "v":
        case "V": {
          if (e.ctrlKey || e.metaKey) {
            // 有画布节点可粘贴才接管；剪贴板无节点（如复制的是页面文本）时放行默认粘贴，
            // 否则无条件 preventDefault 会静默吞掉正文里的粘贴
            if (onPaste?.()) e.preventDefault();
          }
          break;
        }

        case "z":
        case "Z": {
          if (e.ctrlKey || e.metaKey) {
            e.preventDefault();
            undo();
          }
          break;
        }

        case "y":
        case "Y": {
          if (e.ctrlKey || e.metaKey) {
            e.preventDefault();
            redo();
          }
          break;
        }

        case "a":
        case "A": {
          if ((e.ctrlKey || e.metaKey) && nodes.length > 0) {
            e.preventDefault();
            onNodesChange(
              nodes.map((n) => ({ type: "select", id: n.id, selected: true })),
            );
          }
          break;
        }
      }
    };

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [onEscape, getState, enabled, onPaste]);
}
