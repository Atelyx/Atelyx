import { useCallback } from "react";
import { useAppStore } from "@/stores/appStore";
import { useUiStateStore } from "@/stores/uiStateStore";
import { useVaultStore } from "@/stores/vaultStore";
import { baseName, noteTitleFromFile, tableTitleFromFile } from "@/utils/filename";
import type { CanvasFileRow } from "@/types";

/** 拖拽会话（pointer 模拟拖拽期间的状态，见 useVaultDrag）。 */
export interface DragSession {
  kind: "note" | "attachment" | "canvas" | "table" | "folder";
  file: string;
  name: string;
  title?: string;
  startX: number;
  startY: number;
  active: boolean;
  x: number;
  y: number;
}

/** 右键菜单目标。 */
export type MenuTarget =
  | { kind: "folder"; dir: string }
  | { kind: "canvas"; row: CanvasFileRow }
  | { kind: "note"; file: string; name: string }
  | { kind: "table"; file: string; name: string }
  | { kind: "attachment"; file: string; name: string };

/** inline 输入行（行内重命名 / 文件夹下新建）。 */
export type Editing =
  | { kind: "canvas"; file: string; value: string }
  | { kind: "note"; file: string; value: string }
  | { kind: "table"; file: string; value: string }
  | { kind: "attachment"; file: string; value: string }
  | { kind: "folder"; dir: string; value: string }
  | { kind: "creating"; dir: string; type: "canvas" | "note" | "table" | "folder"; value: string };

/** 拖拽松手在文件夹行上：移动文件/文件夹到该目录（画布/笔记/附件/文件夹按 kind 分派）。 */
export function useHandleMoveFile(onNotice: (message: string) => void) {
  const canvases = useAppStore((s) => s.canvases);
  const moveFolder = useVaultStore((s) => s.moveFolder);
  const moveCanvas = useAppStore((s) => s.moveCanvas);
  const moveNote = useVaultStore((s) => s.moveNote);
  const moveTable = useVaultStore((s) => s.moveTable);
  const moveAttachment = useVaultStore((s) => s.moveAttachment);
  const expandDirs = useUiStateStore((s) => s.expandDirs);

  const handleMoveFile = useCallback(
    async (d: DragSession, dir: string) => {
      try {
        if (d.kind === "folder") {
          const newDir = await moveFolder(d.file, dir);
          const newName = baseName(newDir);
          if (newName !== d.name) onNotice(`「${d.name}」已存在，已重命名为「${newName}」`);
        } else if (d.kind === "canvas") {
          const row = canvases.find((c) => c.file === d.file);
          if (row) {
            const newFile = await moveCanvas(row, dir);
            const newName = baseName(newFile);
            if (newName !== d.name) onNotice(`「${d.name}」已存在，已重命名为「${newName}」`);
          } else {
            // 外部白板（.canvas）不在画布列表：走通用文件移动（对任意文件生效）
            const newFile = await moveAttachment(d.file, dir);
            const newName = baseName(newFile);
            if (newName !== d.name) onNotice(`「${d.name}」已存在，已重命名为「${newName}」`);
          }
        } else if (d.kind === "note") {
          const newFile = await moveNote(d.file, dir);
          const newName = baseName(newFile);
          if (newName !== d.name) onNotice(`「${d.name}」已存在，已重命名为「${newName}」`);
        } else if (d.kind === "table") {
          const newFile = await moveTable(d.file, dir);
          const newName = baseName(newFile);
          if (newName !== d.name) onNotice(`「${d.name}」已存在，已重命名为「${newName}」`);
        } else {
          const newFile = await moveAttachment(d.file, dir);
          const newName = baseName(newFile);
          if (newName !== d.name) onNotice(`「${d.name}」已存在，已重命名为「${newName}」`);
        }
        // 移动成功：展开目标文件夹及其祖先让文件可见
        const parts = dir.split("/").filter(Boolean);
        let acc = "";
        const dirs: string[] = [];
        for (const p of parts) {
          acc = acc ? `${acc}/${p}` : p;
          dirs.push(acc);
        }
        expandDirs(dirs);
      } catch (err) {
        console.error("移动文件失败", err);
        onNotice("移动文件失败，请重试");
      }
    },
    [canvases, moveFolder, moveCanvas, moveNote, moveTable, moveAttachment, expandDirs, onNotice],
  );

  return handleMoveFile;
}

/** 复制文件/文件夹为同目录副本（同名自动加序号），按 kind 分派到 store。 */
export function useDuplicateAction(onNotice: (message: string) => void) {
  const duplicateFolder = useVaultStore((s) => s.duplicateFolder);
  const duplicateCanvas = useAppStore((s) => s.duplicateCanvas);
  const duplicateNote = useVaultStore((s) => s.duplicateNote);
  const duplicateTable = useVaultStore((s) => s.duplicateTable);
  const duplicateAttachment = useVaultStore((s) => s.duplicateAttachment);

  const duplicateAction = useCallback(
    async (t: MenuTarget) => {
      try {
        if (t.kind === "folder") {
          const newDir = await duplicateFolder(t.dir);
          onNotice(`已创建副本「${baseName(newDir)}」`);
        } else if (t.kind === "canvas") {
          const title = await duplicateCanvas(t.row);
          onNotice(`已创建副本「${title}」`);
        } else if (t.kind === "note") {
          const newFile = await duplicateNote(t.file);
          onNotice(`已创建副本「${baseName(newFile)}」`);
        } else if (t.kind === "table") {
          const newFile = await duplicateTable(t.file);
          onNotice(`已创建副本「${baseName(newFile)}」`);
        } else {
          const newFile = await duplicateAttachment(t.file);
          onNotice(`已创建副本「${baseName(newFile)}」`);
        }
      } catch (err) {
        console.error("复制失败", err);
        onNotice("复制失败，请重试");
      }
    },
    [duplicateFolder, duplicateCanvas, duplicateNote, duplicateTable, duplicateAttachment, onNotice],
  );

  return duplicateAction;
}

interface CommitEditingOptions {
  /** 提交结果提示（失败/重名自动改名）。 */
  onNotice: (message: string) => void;
  /** 结束 inline 编辑态（提交/取消后清空）。 */
  onEditingChange: (e: Editing | null) => void;
  /** 新建画布成功后打开画布（页面层包装 openCanvas + setActiveWindow）。 */
  onOpenCanvasFile: (row: CanvasFileRow) => void;
  /** 新建表格成功后打开表格。 */
  onOpenTableFile: (file: string, title: string) => void;
}

/** 提交 inline 输入（新建/重命名），返回是否继续保留编辑态。 */
export function useCommitEditing({
  onNotice,
  onEditingChange,
  onOpenCanvasFile,
  onOpenTableFile,
}: CommitEditingOptions) {
  const canvases = useAppStore((s) => s.canvases);
  const createCanvas = useAppStore((s) => s.createCanvas);
  const renameCanvas = useAppStore((s) => s.renameCanvas);
  const createNote = useVaultStore((s) => s.createNote);
  const renameNote = useVaultStore((s) => s.renameNote);
  const createTable = useVaultStore((s) => s.createTable);
  const renameTable = useVaultStore((s) => s.renameTable);
  const renameAttachment = useVaultStore((s) => s.renameAttachment);
  const createFolder = useVaultStore((s) => s.createFolder);
  const renameFolder = useVaultStore((s) => s.renameFolder);

  const commitEditing = useCallback(
    async (e: Editing) => {
      const v = e.value.trim();
      onEditingChange(null);
      if (!v) return;
      try {
        if (e.kind === "canvas") {
          const actual = await renameCanvas(
            canvases.find((c) => c.file === e.file)!,
            v,
          );
          if (actual !== v) onNotice(`「${v}」已存在，已重命名为「${actual}」`);
        } else if (e.kind === "note") {
          const newFile = await renameNote(e.file, v);
          const actualTitle = noteTitleFromFile(newFile);
          if (actualTitle !== v) onNotice(`「${v}」已存在，已重命名为「${actualTitle}」`);
        } else if (e.kind === "table") {
          const newFile = await renameTable(e.file, v);
          const actualTitle = tableTitleFromFile(newFile);
          if (actualTitle !== v) onNotice(`「${v}」已存在，已重命名为「${actualTitle}」`);
        } else if (e.kind === "attachment") {
          await renameAttachment(e.file, v);
        } else if (e.kind === "folder") {
          const actualDir = await renameFolder(e.dir, v);
          const actualName = baseName(actualDir);
          if (actualName !== v) onNotice(`「${v}」已存在，已重命名为「${actualName}」`);
        } else if (e.kind === "creating") {
          if (e.type === "canvas") {
            const { id, file, title } = await createCanvas(v, e.dir);
            onOpenCanvasFile({ id, file, title, updatedAt: 0 });
            if (title !== v) onNotice(`「${v}」已存在，已创建为「${title}」`);
          } else if (e.type === "note") {
            const file = await createNote(v, e.dir);
            const actualTitle = noteTitleFromFile(file);
            if (actualTitle !== v) onNotice(`「${v}」已存在，已创建为「${actualTitle}」`);
          } else if (e.type === "table") {
            const { file, title } = await createTable(v, e.dir);
            if (title !== v) onNotice(`「${v}」已存在，已创建为「${title}」`);
            if (file) onOpenTableFile(file, title);
          } else {
            const dirPath = e.dir ? `${e.dir}/${v}` : v;
            await createFolder(dirPath);
          }
        }
      } catch (err) {
        console.error("操作失败", err);
        onNotice("操作失败，请重试");
      }
    },
    [canvases, createCanvas, renameCanvas, createNote, renameNote, createTable, renameTable, renameAttachment, createFolder, renameFolder, onNotice, onEditingChange, onOpenCanvasFile, onOpenTableFile],
  );

  return commitEditing;
}
