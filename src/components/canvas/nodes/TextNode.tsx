import { AlertTriangle, Eye, FileText, Pencil, StickyNote } from "lucide-react";
import { useRef, useState } from "react";
import type { NodeProps } from "@xyflow/react";
import type { TextData } from "@/types";
import { useCanvasStore } from "@/stores/canvasStore";
import { useVaultStore } from "@/stores/vaultStore";
import { ResizeHandle } from "./ResizeHandle";
import {
  DEFAULT_TEXT_NODE_HEIGHT,
  DEFAULT_TEXT_NODE_WIDTH,
} from "@/constants/canvas";
import { ConnectionFrame } from "./ConnectionFrame";
import { useInlineEdit } from "@/hooks/useInlineEdit";
import { useVaultLinkHandlers } from "@/hooks/useVaultLinkHandlers";
import { useWikiNodeLocate } from "@/hooks/useWikiNodeLocate";
import { MarkdownView, type MarkdownEditorLinks } from "@/components/editor/MarkdownEditor";

export function TextNode({ id, data, width, height, selected }: NodeProps) {
  const { bodyMd, title, file, fileMissing } = data as unknown as TextData;
  // 只读白板（外部白板格式）：禁编辑——防经笔记写回链路把改动写进原 .md 文件
  const readOnly = useCanvasStore((s) => s.readOnly);
  // 样式区分：有 file = 笔记节点（仓库 .md 引用，实线）；无 file = 画布内文本节点（未落盘，虚线 + 圆点标记）
  const isSaved = !!file;
  const [editing, setEditing] = useState(false);
  /** 编辑草稿：输入期间只写本地 state，不触碰 store（防输入法组合中间态被 debounce 落盘） */
  const [draft, setDraft] = useState("");
  /** Esc 丢弃标记：退出编辑不提交草稿（onBlur 同样跳过，防 textarea 移除时误提交） */
  const discardEditRef = useRef(false);
  /** 标题重命名（双击 header 标题 inline 编辑） */
  const renameEdit = useInlineEdit({
    value: title ?? "",
    onCommit: (v) => {
      void commitRename(v);
    },
  });

  const enterEdit = () => {
    // 重置 Esc 丢弃标记：上次 Esc 退出若未触发 blur（浏览器不保证），避免本次编辑首次失焦误跳过提交
    discardEditRef.current = false;
    setDraft(bodyMd ?? "");
    setEditing(true);
  };
  /** 退出编辑时提交草稿：diff 非空才写 store → 随画布 debounce 500ms 写回 .md（编辑完成才落盘） */
  const exitEdit = () => {
    if (draft !== (bodyMd ?? "")) {
      useCanvasStore.getState().updateNodeData(id, { bodyMd: draft });
    }
    setEditing(false);
  };

  /** 确认重命名：笔记节点 renameNote 改名 + 扫全部 .atlx 更新引用；画布内文本节点只改标题（无仓库文件） */
  const commitRename = async (draftTitle: string) => {
    const t = draftTitle.trim();
    if (!t || t === title) return;
    try {
      if (file) {
        const newFile = await useVaultStore.getState().renameNote(file, t);
        useCanvasStore
          .getState()
          .updateNodeData(id, { title: t, file: newFile });
      } else {
        useCanvasStore.getState().updateNodeData(id, { title: t });
      }
    } catch (e) {
      console.error("重命名笔记失败", e);
      useCanvasStore.setState({ error: "重命名笔记失败，请重试" });
    }
  };

  // [[wiki 链接]] 定位 + 笔记链接打开/新建（公共接线簇，见 hooks/useWikiNodeLocate、useVaultLinkHandlers）
  const { isWikiLocatable, handleLocateWiki } = useWikiNodeLocate();
  const {
    handleOpenWikiNote,
    isVaultPathNote,
    handleOpenVaultPathNote,
    handleCreateNote,
  } = useVaultLinkHandlers();
  // 统一渲染引擎的链接/定位回调（回调全部稳定，防随内容重建装饰）
  const textMarkdownLinks: MarkdownEditorLinks = {
    onOpenNote: handleOpenWikiNote,
    isVaultPathNote,
    onOpenVaultPathNote: handleOpenVaultPathNote,
    onCreateNote: handleCreateNote,
    isLocatable: isWikiLocatable,
    onLocate: handleLocateWiki,
  };

  return (
    <div
      className="rounded-lg shadow-lg border flex flex-col text-sm"
      style={{
        width: width ?? DEFAULT_TEXT_NODE_WIDTH,
        height: height ?? DEFAULT_TEXT_NODE_HEIGHT,
        minWidth: 200,
        minHeight: 100,
        background: "var(--bg-card)",
        borderColor: selected ? "var(--accent)" : "var(--border)",
        // 未保存的画布内文本节点用虚线边框与笔记节点（实线）区分
        borderStyle: isSaved ? "solid" : "dashed",
        position: "relative",
      }}
    >
      <ConnectionFrame topType="source" selected={selected} />

      <header
        className="px-3 py-1.5 border-b rounded-t-lg text-xs font-medium flex-shrink-0 flex items-center justify-between gap-1"
        style={{
          cursor: "grab",
          borderColor: "var(--border)",
          color: "var(--text-secondary)",
        }}
      >
        <span className="inline-flex items-center gap-1 min-w-0 flex-1 overflow-hidden">
          {isSaved ? (
            <StickyNote size={14} className="flex-shrink-0" />
          ) : (
            /* 画布内文本节点：文件图标 + 琥珀圆点标记「未保存为笔记」 */
            <span
              className="inline-flex items-center flex-shrink-0"
              title="画布内文本，未保存为笔记"
            >
              <FileText size={14} />
              <span
                className="ml-1 w-1.5 h-1.5 rounded-full"
                style={{ background: "#f59e0b" }}
              />
            </span>
          )}
          {renameEdit.editing ? (
            <input
              {...renameEdit.inputProps}
              autoFocus
              onClick={(e) => e.stopPropagation()}
              className="nodrag w-full min-w-0 rounded px-1 text-xs outline-none focus:ring-1 focus:ring-[var(--accent)]"
              style={{
                background: "var(--input-bg)",
                color: "var(--text-primary)",
              }}
            />
          ) : (
            <span
              className="truncate"
              title={fileMissing || readOnly ? undefined : "双击重命名"}
              onDoubleClick={
                fileMissing || readOnly ? undefined : renameEdit.start
              }
            >
              {title || "文本"}
            </span>
          )}
        </span>
        {!fileMissing && !readOnly && (
          <div
            className="flex items-center nodrag"
            onClick={(e) => e.stopPropagation()}
          >
            <button
              type="button"
              title={editing ? "预览（Esc 退出）" : "编辑"}
              onClick={() => (editing ? exitEdit() : enterEdit())}
              className="nodrag rounded p-0.5 hover:bg-[var(--bg-tertiary)] hover:text-[var(--text-primary)] transition-colors"
              style={{ color: "var(--text-muted)" }}
            >
              {editing ? <Eye size={13} /> : <Pencil size={13} />}
            </button>
          </div>
        )}
      </header>

      <div
        className={`nodrag nowheel overflow-auto markdown-body max-w-none break-words px-3 py-2 flex-1 min-h-0`}
        style={{ userSelect: "text", WebkitUserSelect: "text" }}
        onDoubleClick={fileMissing || readOnly ? undefined : enterEdit}
      >
        {fileMissing ? (
          <p
            className="text-xs flex items-center gap-1"
            style={{ color: "#f87171" }}
          >
            <AlertTriangle size={14} className="flex-shrink-0" />
            文件缺失（已在文件管理器中删除或重命名）
          </p>
        ) : editing ? (
          <textarea
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onBlur={() => {
              if (!discardEditRef.current) exitEdit();
              discardEditRef.current = false;
            }}
            onKeyDown={(e) => {
              if (e.key === "Escape") {
                // Esc = 丢弃草稿退出（不写 store；标记防止 textarea 移除触发 onBlur 提交）
                discardEditRef.current = true;
                setEditing(false);
              } else if (e.ctrlKey && e.key === "Enter") {
                exitEdit();
              }
            }}
            autoFocus
            spellCheck={false}
            className="nodrag w-full min-h-[120px] h-full resize-none bg-transparent text-sm rounded outline-none focus:ring-1 focus:ring-[var(--accent)]"
            style={{ color: "var(--text-primary)", lineHeight: 1.6 }}
          />
        ) : (
          <MarkdownView
            text={bodyMd || "*（空）*"}
            links={textMarkdownLinks}
            className="h-full"
          />
        )}
      </div>

      {!readOnly && <ResizeHandle />}
    </div>
  );
}
