/**
 * 工作区文件生命周期联动（桌面工作区与移动端单栏壳共用）：
 * - 打开中的笔记/表格从文件列表消失 → 软件内重命名/文件夹重命名跟随切到新文件，
 *   真删除/外部删除关闭视图（表格静默关闭不 flush，防写回重建已删文件）；
 * - 进仓库自动恢复上次打开的文件（设置开启时；文件缺失/已删除静默跳过）；
 * - 历史作者登记（应用级全局：画布/笔记/表格共用身份，随协作昵称/设备名刷新）。
 *
 * 视图渲染形态由页面自行决定；本 hook 只承担跨 store 的文件状态一致性。
 */
import { useEffect } from "react";
import { useAppStore } from "@/stores/appStore";
import { useSettingsStore } from "@/stores/settingsStore";
import { useUiStateStore } from "@/stores/uiStateStore";
import { useVaultStore, lastFolderRenameTarget, lastNoteRenameTarget, lastTableRenameTarget } from "@/stores/vaultStore";
import { noteTitleFromFile, tableTitleFromFile } from "@/utils/filename";
import type { FileTreeNode } from "@/types";

export function useWorkspaceFileEffects(): void {
  // 当前打开文件状态（联动触发源）
  const currentCanvasFile = useAppStore((s) => s.currentCanvasFile);
  const currentNoteFile = useAppStore((s) => s.currentNoteFile);
  const currentTableFile = useAppStore((s) => s.currentTableFile);

  const vaultNoteList = useVaultStore((s) => s.noteList);
  const vaultTableList = useVaultStore((s) => s.tableList);

  // 应用级 UI 使用状态：上次打开的画布/笔记/表格，进仓库自动恢复
  const uiLoaded = useUiStateStore((s) => s.loaded);
  const lastCanvasFile = useUiStateStore((s) => s.lastCanvasFile);
  const lastNoteFile = useUiStateStore((s) => s.lastNoteFile);
  const lastTableFile = useUiStateStore((s) => s.lastTableFile);
  const autoRestoreFiles = useSettingsStore((s) => s.autoRestoreFiles);

  // 当前打开的笔记从列表消失 → 区分处理：软件内重命名/文件夹重命名（切到新文件）；真删除/外部删除（关闭笔记）
  useEffect(() => {
    if (!currentNoteFile) return;
    const stillExists = vaultNoteList.some((n) => n.file === currentNoteFile);
    if (!stillExists) {
      const newFile =
        lastNoteRenameTarget(currentNoteFile) ?? lastFolderRenameTarget(currentNoteFile);
      if (newFile) {
        useAppStore.getState().openNote(
          newFile,
          noteTitleFromFile(newFile),
        );
      } else {
        useAppStore.getState().closeNote();
      }
    }
  }, [vaultNoteList, currentNoteFile]);

  // 当前打开的表格从列表消失 → 同笔记：软件内重命名/文件夹重命名切到新文件（重载内容）；
  // 真删除/外部删除静默关闭（不 flush——防写回重建已删文件，只清内存态）
  useEffect(() => {
    if (!currentTableFile) return;
    const stillExists = vaultTableList.some((t) => t.file === currentTableFile);
    if (!stillExists) {
      const newFile =
        lastTableRenameTarget(currentTableFile) ?? lastFolderRenameTarget(currentTableFile);
      if (newFile) {
        const newTitle = tableTitleFromFile(newFile);
        useAppStore.getState().openTable(newFile, newTitle);
      } else {
        useAppStore.getState().closeTableSilent();
      }
    }
  }, [vaultTableList, currentTableFile]);

  // 历史记录作者登记（应用级全局，三 kind——画布/笔记/表格——共用同一身份）：
  // 身份随协作昵称/设备名变化刷新；未打开笔记时画布/表格历史也能正确署名
  const collabNickname = useSettingsStore((s) => s.collabNickname);
  const collabDevice = useSettingsStore((s) => s.deviceName);
  useEffect(() => {
    useVaultStore.getState().historySetAuthor(
      collabNickname || collabDevice || "用户",
      collabDevice || "",
    );
  }, [collabNickname, collabDevice]);

  /** 进仓库后恢复上次打开的文件（设置「自动恢复上次打开的文件」开启时）。
   * 依赖 uiLoaded（uiState 已从磁盘加载）+ 文件树/列表就绪后才执行，文件已被外部删除/
   * 移动则静默跳过（降级占位，不报错）。openCanvas/openNote/openTable 内部记录「上次打开」。 */
  useEffect(() => {
    if (!uiLoaded) return;
    if (!autoRestoreFiles) return;
    const store = useAppStore.getState();
    // 画布：lastCanvasFile 能在当前画布列表命中才打开（文件缺失/已删除则跳过）；
    // 外部白板（.canvas）不在画布列表，从文件树命中后合成行打开（只读查看）
    if (lastCanvasFile && !store.currentCanvasFile) {
      const row = store.canvases.find((c) => c.file === lastCanvasFile);
      if (row) {
        store.openCanvas(row);
      } else if (lastCanvasFile.toLowerCase().endsWith(".canvas")) {
        const hit = findFileInTree(useVaultStore.getState().tree, lastCanvasFile);
        if (hit) {
          store.openCanvas({
            id: hit.path,
            title: hit.name.replace(/\.canvas$/i, ""),
            file: hit.path,
            updatedAt: hit.updatedAt,
          });
        }
      }
    }
    // 笔记：lastNoteFile 能在笔记列表命中才打开（文件缺失/已删除则跳过）
    if (lastNoteFile && !store.currentNoteFile) {
      const note = vaultNoteList.find((n) => n.file === lastNoteFile);
      if (note) store.openNote(note.file, note.name.replace(/\.md$/i, ""));
    }
    // 表格：lastTableFile 能在表格列表命中才打开（文件缺失/已删除则跳过）
    if (lastTableFile && !store.currentTableFile) {
      const table = vaultTableList.find((t) => t.file === lastTableFile);
      if (table) store.openTable(table.file, table.name.replace(/\.atb$/i, ""));
    }
  }, [
    uiLoaded,
    autoRestoreFiles,
    lastCanvasFile,
    lastNoteFile,
    lastTableFile,
    vaultNoteList,
    vaultTableList,
    currentCanvasFile,
    currentNoteFile,
    currentTableFile,
  ]);
}

/** 在文件树中按相对路径查找文件（恢复上次打开的外部白板用，.canvas 不在画布列表）。 */
function findFileInTree(nodes: FileTreeNode[], path: string): FileTreeNode | null {
  for (const n of nodes) {
    if (n.path === path) return n;
    if (n.isDir) {
      const hit = findFileInTree(n.children, path);
      if (hit) return hit;
    }
  }
  return null;
}
