/**
 * 仓库笔记链接处理器（wiki 链接 / 基于仓库的路径链接 / 空链接快捷新建）。
 *
 * 收敛对话节点 / 文本节点 / 笔记编辑器 / AI 对话面板四处逐字复制的接线簇：
 * - handleOpenWikiNote：`[[wiki 链接]]` 命中仓库笔记 → 打开
 * - isVaultPathNote / handleOpenVaultPathNote：`[label](路径)` 命中仓库笔记 → 打开
 * - handleCreateNote：`[名]()` 空链接（目标不存在）→ 快捷新建并打开
 *
 * 回调全部 useCallback 稳定化（内部 getState() 实时读 noteList），
 * 是调用方 `useMemo([])` 缓存 MarkdownEditorLinks 的前提（气泡 memo 生效）。
 */
import { useCallback } from "react";
import { useAppStore } from "@/stores/appStore";
import { useVaultStore } from "@/stores/vaultStore";
import { vaultPathNoteOf, wikiNoteFileOf } from "@/utils/markdown";

export function useVaultLinkHandlers() {
  const handleOpenWikiNote = useCallback((value: string) => {
    const hit = wikiNoteFileOf(value, useVaultStore.getState().noteList);
    if (hit) useAppStore.getState().openNote(hit.file, hit.title);
  }, []);

  const isVaultPathNote = useCallback(
    (href: string) => vaultPathNoteOf(href, useVaultStore.getState().noteList) != null,
    []
  );

  const handleOpenVaultPathNote = useCallback((href: string) => {
    const hit = vaultPathNoteOf(href, useVaultStore.getState().noteList);
    if (hit) useAppStore.getState().openNote(hit.file, hit.title);
  }, []);

  const handleCreateNote = useCallback((name: string) => {
    void useVaultStore
      .getState()
      .createNote(name)
      .then((file) => useAppStore.getState().openNote(file, name))
      .catch((e) => console.error("创建笔记失败", e));
  }, []);

  return { handleOpenWikiNote, isVaultPathNote, handleOpenVaultPathNote, handleCreateNote };
}
