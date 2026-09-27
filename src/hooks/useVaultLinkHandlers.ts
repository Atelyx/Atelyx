/**
 * 仓库笔记链接处理器（wiki 链接 / 基于仓库的路径链接 / 空链接快捷新建）。
 *
 * 收敛对话节点 / 文本节点 / 笔记编辑器 / AI 对话面板四处逐字复制的接线簇：
 * - handleOpenWikiNote：`[[wiki 链接]]` 命中仓库笔记 → 打开
 * - resolveWikiNote：`[[wiki 链接]]` 是否命中仓库笔记（渲染层据此做缺失样式 + 点击新建）
 * - isVaultPathNote / handleOpenVaultPathNote：`[label](路径)` 命中仓库笔记 → 打开
 * - handleCreateNote：`[名]()` 空路径链接、未命中 wiki 链接与未命中路径链接 →
 *   按名字/相对路径创建笔记，返回新文件相对路径（失败 = null）。
 *   只创建不打开——打开时机归渲染层（onOpenCreatedNote），必须晚于回填：
 *   openNote 会切换编辑面文档，先打开会让回填写进错误文档。
 * - openCreatedNote：打开快捷新建的笔记（渲染层回填完成后调用）
 *
 * 回调全部 useCallback 稳定化（内部 getState() 实时读 noteList），
 * 是调用方 `useMemo([])` 缓存 MarkdownEditorLinks 的前提（气泡 memo 生效）。
 */
import { useCallback } from "react";
import { useAppStore } from "@/stores/appStore";
import { useVaultStore } from "@/stores/vaultStore";
import { vaultPathNoteOf, wikiNoteFileOf } from "@/utils/markdown";

export function useVaultLinkHandlers() {
  const resolveWikiNote = useCallback(
    (value: string) => wikiNoteFileOf(value, useVaultStore.getState().noteList) != null,
    [],
  );

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

  const handleCreateNote = useCallback((name: string): Promise<string | null> => {
    // 路径形式（含 `/`）= 按仓库相对路径创建（目录段为目录、末段去 .md 为标题）；
    // 纯名字 = 仓库根创建。同名自动加序号，返回实际落盘路径。
    const slash = name.lastIndexOf("/");
    const dir = slash > 0 ? name.slice(0, slash) : "";
    const title = slash > 0 ? name.slice(slash + 1).replace(/\.md$/i, "") : name;
    return useVaultStore
      .getState()
      .createNote(title, dir)
      .catch((e) => {
        console.error("创建笔记失败", e);
        return null;
      });
  }, []);

  const openCreatedNote = useCallback((file: string, name: string) => {
    useAppStore.getState().openNote(file, name);
  }, []);

  return {
    resolveWikiNote,
    handleOpenWikiNote,
    isVaultPathNote,
    handleOpenVaultPathNote,
    handleCreateNote,
    openCreatedNote,
  };
}
