import type { PointerEvent as ReactPointerEvent } from "react";
import {
  ChevronDown,
  ChevronRight,
  FileText,
  Folder,
  LayoutDashboard,
  Paperclip,
  StickyNote,
  Table,
} from "lucide-react";
import { InlineInput } from "./InlineInput";
import { sortChildren, upperExt } from "./sort";
import { noteTitleFromFile, stripExt, tableTitleFromFile } from "@/utils/filename";
import type { CanvasFileRow, FileExplorerSortKey, FileTreeNode } from "@/types";
import type { Editing, MenuTarget } from "./actions";

export interface FileTreeProps {
  nodes: FileTreeNode[];
  depth: number;
  /** 该层所属目录（根目录 = ""，供「在目标文件夹新建草稿」落点判定）。 */
  parentDir: string;
  sortKey: FileExplorerSortKey;
  expanded: Set<string>;
  /** 切换单个文件夹展开/收起。 */
  toggleExpanded: (path: string) => void;
  editing: Editing | null;
  onEditingChange: (e: Editing | null) => void;
  onCommitEditing: (e: Editing) => void;
  dropDir: string | null;
  folderColors: Record<string, string> | undefined;
  currentCanvasFile: string | null;
  openedNoteFile: string | null;
  openedTableFile: string | null;
  canvasRowOf: (path: string) => CanvasFileRow | undefined;
  startPotentialDrag: (e: ReactPointerEvent, node: FileTreeNode) => void;
  onOpenCanvasFile: (row: CanvasFileRow) => void;
  onOpenNoteForEdit: (file: string, title: string) => void;
  onOpenTableFile: (file: string, title: string) => void;
  onOpenMenu: (x: number, y: number, target: MenuTarget) => void;
}

/** 递归渲染文件树一层（文件夹行 + 文件行 + 该层「新建草稿」输入行）。 */
export function FileTree(props: FileTreeProps) {
  const {
    nodes,
    depth,
    parentDir,
    sortKey,
    expanded,
    toggleExpanded,
    editing,
    onEditingChange,
    onCommitEditing,
    dropDir,
    folderColors,
    currentCanvasFile,
    openedNoteFile,
    openedTableFile,
    canvasRowOf,
    startPotentialDrag,
    onOpenCanvasFile,
    onOpenNoteForEdit,
    onOpenTableFile,
    onOpenMenu,
  } = props;

  // 新建草稿输入行：渲染在目标文件夹 children 顶部（根目录 = 树顶部）
  const sorted = sortChildren(nodes, sortKey);
  const creatingHere =
    editing?.kind === "creating" && editing.dir === parentDir ? (
      <li key="__creating__" className="px-2 pl-6 py-1 flex items-center gap-1">
        <InlineInput
          value={editing.value}
          onChange={(v) => onEditingChange({ ...editing, value: v })}
          onCommit={() => void onCommitEditing(editing)}
          onCancel={() => onEditingChange(null)}
          placeholder={
            editing.type === "canvas"
              ? "画布名称"
              : editing.type === "note"
                ? "笔记名称"
                : editing.type === "table"
                  ? "表格名称"
                  : "文件夹名称"
          }
        />
      </li>
    ) : null;

  const childProps = {
    sortKey,
    expanded,
    toggleExpanded,
    editing,
    onEditingChange,
    onCommitEditing,
    dropDir,
    folderColors,
    currentCanvasFile,
    openedNoteFile,
    openedTableFile,
    canvasRowOf,
    startPotentialDrag,
    onOpenCanvasFile,
    onOpenNoteForEdit,
    onOpenTableFile,
    onOpenMenu,
  };

  return (
    <>
      {creatingHere}
      {sorted.map((node) => {
        const indent = { paddingLeft: 6 + depth * 12 };
        if (node.isDir) {
          const isExpanded = expanded.has(node.path);
          const editingThis: Editing | null =
            editing?.kind === "folder" && editing.dir === node.path ? editing : null;
          return (
            <li key={node.path}>
              {editingThis ? (
                <div className="flex items-center px-2 py-1 min-h-8" style={indent}>
                  <InlineInput
                    value={editingThis.value}
                    onChange={(v) => onEditingChange({ ...editingThis, value: v })}
                    onCommit={() => void onCommitEditing(editingThis)}
                    onCancel={() => onEditingChange(null)}
                    placeholder="文件夹名称"
                  />
                </div>
              ) : (
                <div
                  className="flex items-center gap-1 px-2 py-1 min-h-8 select-none cursor-default rounded-md hover:bg-[var(--hover)]"
                  style={{
                    ...indent,
                    // 拖拽悬停目标高亮（金色底），提示可放入移动
                    background: dropDir === node.path ? "color-mix(in srgb, var(--accent) 25%, transparent)" : undefined,
                  }}
                  data-dir={node.path}
                  onPointerDown={(e) => startPotentialDrag(e, node)}
                  onClick={() => toggleExpanded(node.path)}
                  onContextMenu={(e) => {
                    e.preventDefault();
                    e.stopPropagation();
                    onOpenMenu(e.clientX, e.clientY, { kind: "folder", dir: node.path });
                  }}
                >
                  <span className="flex items-center">{isExpanded ? <ChevronDown size={14} /> : <ChevronRight size={14} />}</span>
                  <Folder size={14} style={{ color: folderColors?.[node.path] ?? "var(--text-muted)" }} />
                  <span className="flex-1 truncate text-xs" style={{ color: "var(--text-primary)" }}>{node.name}</span>
                </div>
              )}
              {isExpanded && (
                <ul className="relative">
                  {/* 展开指示线：从父行图标中心垂下，贯穿全部子项（pointer-events-none 防拦截拖拽落点判定） */}
                  <div
                    className="absolute top-0 bottom-0 w-px pointer-events-none z-10"
                    style={{
                      // 行 padding-left（inline 覆盖 px-2）= 6 + depth*12，加 chevron 半宽 7
                      left: 13 + depth * 12,
                      background: "var(--text-muted)",
                      opacity: 0.6,
                    }}
                  />
                  <FileTree nodes={node.children} depth={depth + 1} parentDir={node.path} {...childProps} />
                </ul>
              )}
            </li>
          );
        }
        // 文件行
        const isCanvas = node.name.toLowerCase().endsWith(".atlx");
        const isWhiteboard = node.name.toLowerCase().endsWith(".canvas");
        const isNote = node.name.toLowerCase().endsWith(".md");
        const isTable = node.name.toLowerCase().endsWith(".atb");
        const row = isCanvas ? canvasRowOf(node.path) : undefined;
        const active =
          (isCanvas && row && currentCanvasFile === row.file) ||
          (isWhiteboard && currentCanvasFile === node.path) ||
          (isNote && openedNoteFile === node.path) ||
          (isTable && openedTableFile === node.path);
        const editingThis: Editing | null =
          editing?.kind === "canvas" && editing.file === node.path
            ? editing
            : editing?.kind === "note" && editing.file === node.path
              ? editing
              : editing?.kind === "table" && editing.file === node.path
                ? editing
                : editing?.kind === "attachment" && editing.file === node.path
                  ? editing
                  : null;
        return (
          <li key={node.path}>
            {editingThis ? (
              <div className="flex items-center px-2 py-1 min-h-8" style={indent}>
                <InlineInput
                  value={editingThis.value}
                  onChange={(v) => onEditingChange({ ...editingThis, value: v })}
                  onCommit={() => void onCommitEditing(editingThis)}
                  onCancel={() => onEditingChange(null)}
                />
              </div>
            ) : (
              <div
                className="flex items-center gap-1 px-2 py-1 min-h-8 cursor-default rounded-md hover:bg-[var(--hover)]"
                style={{
                  ...indent,
                  background: active ? "color-mix(in srgb, var(--accent) 20%, transparent)" : undefined,
                }}
                data-file={node.path}
                onPointerDown={(e) => startPotentialDrag(e, node)}
                onClick={() => {
                  if (isCanvas && row) onOpenCanvasFile(row);
                  else if (isWhiteboard) {
                    // 外部白板：合成行打开（id = 路径 = 运行时身份，只读查看）
                    onOpenCanvasFile({
                      id: node.path,
                      title: stripExt(node.name),
                      file: node.path,
                      updatedAt: node.updatedAt,
                    });
                  } else if (isNote) onOpenNoteForEdit(node.path, noteTitleFromFile(node.path));
                  else if (isTable) onOpenTableFile(node.path, tableTitleFromFile(node.path));
                }}
                onContextMenu={(e) => {
                  e.preventDefault();
                  e.stopPropagation();
                  if (isCanvas && row) {
                    onOpenMenu(e.clientX, e.clientY, { kind: "canvas", row });
                  } else if (isNote) {
                    onOpenMenu(e.clientX, e.clientY, { kind: "note", file: node.path, name: node.name });
                  } else if (isTable) {
                    onOpenMenu(e.clientX, e.clientY, { kind: "table", file: node.path, name: node.name });
                  } else {
                    onOpenMenu(e.clientX, e.clientY, { kind: "attachment", file: node.path, name: node.name });
                  }
                }}
              >
                {isCanvas ? (
                  <FileText size={14} style={{ color: "var(--text-muted)" }} />
                ) : isWhiteboard ? (
                  <LayoutDashboard size={14} style={{ color: "var(--text-muted)" }} />
                ) : isNote ? (
                  <StickyNote size={14} style={{ color: "var(--text-muted)" }} />
                ) : isTable ? (
                  <Table size={14} style={{ color: "var(--text-muted)" }} />
                ) : (
                  <Paperclip size={14} style={{ color: "var(--text-muted)" }} />
                )}
                <span className="flex-1 truncate text-xs" style={{ color: "var(--text-primary)" }}>{node.name}</span>
                <span
                  className="ml-auto pl-2 text-[10px] font-bold flex-shrink-0"
                  style={{ color: "var(--text-muted)", opacity: 0.6 }}
                >
                  {isCanvas ? "ATLX" : isWhiteboard ? "CANVAS" : isNote ? "MD" : isTable ? "ATB" : upperExt(node.name)}
                </span>
              </div>
            )}
          </li>
        );
      })}
    </>
  );
}
