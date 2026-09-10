import { AlertTriangle, Eye, FileText, Pencil, SlidersHorizontal, StickyNote } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { NodeProps } from "@xyflow/react";
import type { EditorView } from "@codemirror/view";
import type { TextData } from "@/types";
import { useCanvasStore } from "@/stores/canvasStore";
import { useCollabStore } from "@/stores/collabStore";
import { useVaultStore } from "@/stores/vaultStore";
import { useAppStore } from "@/stores/appStore";
import { ResizeHandle } from "./ResizeHandle";
import {
  DEFAULT_TEXT_NODE_HEIGHT,
  DEFAULT_TEXT_NODE_WIDTH,
} from "@/constants/canvas";
import { ConnectionFrame } from "./ConnectionFrame";
import { useInlineEdit } from "@/hooks/useInlineEdit";
import { useVaultLinkHandlers } from "@/hooks/useVaultLinkHandlers";
import { useWikiNodeLocate } from "@/hooks/useWikiNodeLocate";
import { useNoteBodySession, useNoteConflicted, useNoteSurface } from "@/hooks/useNoteBodySession";
import { NoteBodyEditor } from "@/components/editor/NoteBodyEditor";
import {
  MarkdownEditor,
  MarkdownView,
  type MarkdownEditorLinks,
} from "@/components/editor/MarkdownEditor";
import { parseFrontmatter } from "@/utils/frontmatter";

export function TextNode({ id, data, width, height, selected }: NodeProps) {
  const { bodyMd, title, file, fileMissing } = data as unknown as TextData;
  // 只读白板（外部白板格式）：禁编辑——防把改动写进原 .md 文件
  const readOnly = useCanvasStore((s) => s.readOnly);
  // 样式区分：有 file = 笔记节点（仓库 .md 引用，实线）；无 file = 画布内文本节点（未落盘，虚线 + 圆点标记）
  const isSaved = !!file;
  const [editing, setEditing] = useState(false);
  /** 画布内文本节点（无 file）的编辑草稿：正文随 .atlx 内嵌，退出编辑才写回节点。 */
  const [draft, setDraft] = useState(bodyMd ?? "");
  /** 标题重命名（双击 header 标题 inline 编辑） */
  const renameEdit = useInlineEdit({
    value: title ?? "",
    onCommit: (v) => {
      void commitRename(v);
    },
  });
  /** 笔记正文编辑会话：笔记节点进入编辑才打开（会话持有 Y.Doc 与落盘定时器，预览态不需要）。 */
  const { session, view } = useNoteBodySession(editing && isSaved ? file : null, bodyMd);
  /** 笔记正文编辑能力（笔记插件停用时缺席）；画布内文本节点是画布自有便签，不依赖该能力。 */
  const noteSurface = useNoteSurface();
  const canEditBody = isSaved ? !!noteSurface : true;
  /** 正在这篇笔记上工作的协作者（presence 上报的编辑面集合）。 */
  const peers = useCollabStore((s) => s.peers);
  const noteEditors = useMemo(
    () => (file ? peers.filter((p) => p.presence?.editingNotes?.includes(file)) : []),
    [peers, file],
  );

  const editRootRef = useRef<HTMLDivElement>(null);
  const cmViewRef = useRef<EditorView | null>(null);
  const enterEdit = () => {
    // 编辑中重复进入（内容区双击选词会冒泡到双击处理器）：保持当前草稿，绝不重置
    if (editing) return;
    if (!isSaved) {
      // 每次进入编辑都以节点当前正文为草稿
      setDraft(bodyMd ?? "");
    }
    setEditing(true);
  };
  /** 进入编辑即聚焦，双击后可直接输入 */
  useEffect(() => {
    if (editing) cmViewRef.current?.focus();
  }, [editing]);
  /** 退出编辑：笔记节点把会话最新全文镜像回节点（bodyMd 只是渲染缓存，随画布不落 .md）；
   *  画布内文本节点把草稿写回节点（随画布 debounce 写 .atlx），并入画布撤销栈。 */
  const exitEdit = useCallback(() => {
    setEditing(false);
    if (isSaved) {
      const content = session?.content();
      if (content !== undefined && content !== bodyMd) {
        useCanvasStore.getState().updateNodeData(id, { bodyMd: content });
      }
      return;
    }
    if (draft === (bodyMd ?? "")) return;
    useCanvasStore.getState().pushUndo();
    useCanvasStore.getState().updateNodeData(id, { bodyMd: draft });
  }, [isSaved, session, bodyMd, draft, id]);
  /** 退出编辑的稳定入口：mousedown 监听不随每次输入重建（见下） */
  const exitEditRef = useRef(exitEdit);
  exitEditRef.current = exitEdit;
  /** 编辑态点节点外退出：按节点容器命中判定（容器内交互——属性小标/协作徽标/编辑按钮——不退出；
   *  CodeMirror 失焦会被这些交互误触发，不能用 blur）。 */
  useEffect(() => {
    if (!editing) return;
    const onDocMouseDown = (e: MouseEvent) => {
      const target = e.target as Element | null;
      if (editRootRef.current?.contains(target as Node)) return;
      if (target?.closest?.("[data-popup-layer]")) return;
      exitEditRef.current();
    };
    document.addEventListener("mousedown", onDocMouseDown);
    return () => document.removeEventListener("mousedown", onDocMouseDown);
  }, [editing]);

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

  // 笔记节点：正文与 frontmatter 分离展示（属性在笔记面板编辑，节点内不重复 YAML）
  const parsed = useMemo(
    () => (isSaved ? parseFrontmatter(bodyMd ?? "") : null),
    [isSaved, bodyMd],
  );
  const hasFrontmatter = !!parsed?.fmPrefix;
  const previewBody = parsed ? parsed.body : bodyMd || "*（空）*";
  /** 冲突未决（外部已修改、自动保存已暂停）：节点内提示，解决入口在笔记面板。
   *  取自会话注册表而非会话视图——会话随退出编辑关闭，冲突可能仍未解决。 */
  const conflicted = useNoteConflicted(file ?? null);

  return (
    <div
      ref={editRootRef}
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
        <span className="flex items-center gap-1 nodrag flex-shrink-0">
          {/* 这篇笔记上还有谁（对端 presence 上报的编辑面集合，含在笔记面板里打开的） */}
          {noteEditors.slice(0, 3).map((p) => (
            <span
              key={p.peerId}
              title={`${p.nickname} 打开了这篇笔记`}
              className="w-2 h-2 rounded-full flex-shrink-0"
              style={{ background: p.color }}
            />
          ))}
          {/* 冲突未决：自动保存已暂停，解决入口在笔记面板 */}
          {conflicted && file && (
            <button
              type="button"
              title="外部已修改，自动保存已暂停；在笔记面板处理"
              onClick={(e) => {
                e.stopPropagation();
                useAppStore.getState().openNote(file, title || "未命名");
              }}
              className="rounded p-0.5 hover:bg-[var(--bg-tertiary)] transition-colors"
              style={{ color: "#f59e0b" }}
            >
              <AlertTriangle size={13} />
            </button>
          )}
          {/* 有属性：跳笔记面板编辑（节点内只渲染正文） */}
          {hasFrontmatter && file && (
            <button
              type="button"
              title="在笔记面板编辑属性"
              onClick={(e) => {
                e.stopPropagation();
                useAppStore.getState().openNote(file, title || "未命名");
              }}
              className="rounded p-0.5 hover:bg-[var(--bg-tertiary)] hover:text-[var(--text-primary)] transition-colors"
              style={{ color: "var(--text-muted)" }}
            >
              <SlidersHorizontal size={13} />
            </button>
          )}
          {!fileMissing && !readOnly && (canEditBody || editing) && (
            <span onClick={(e) => e.stopPropagation()}>
              <button
                type="button"
                title={editing ? "预览（Esc 退出）" : "编辑"}
                onClick={() => (editing ? exitEdit() : enterEdit())}
                className="rounded p-0.5 hover:bg-[var(--bg-tertiary)] hover:text-[var(--text-primary)] transition-colors"
                style={{ color: "var(--text-muted)" }}
              >
                {editing ? <Eye size={13} /> : <Pencil size={13} />}
              </button>
            </span>
          )}
        </span>
      </header>

      <div
        className="nodrag nowheel overflow-auto markdown-body max-w-none break-words px-3 py-2 flex-1 min-h-0"
        style={{
          userSelect: "text",
          WebkitUserSelect: "text",
          // 内容区不可拖动（nodrag）：编辑态用文本光标，预览态也不该显示抓手（节点由标题栏拖动）
          cursor: editing ? "text" : "default",
        }}
        onDoubleClick={fileMissing || readOnly || !canEditBody ? undefined : enterEdit}
        onKeyDown={(e) => {
          if (editing && e.key === "Escape") exitEdit();
        }}
      >
        {fileMissing ? (
          <p
            className="text-xs flex items-center gap-1"
            style={{ color: "#f87171" }}
          >
            <AlertTriangle size={14} className="flex-shrink-0" />
            文件缺失（已在文件管理器中删除或重命名）
          </p>
        ) : editing && isSaved && file && session ? (
          <NoteBodyEditor
            file={file}
            content={view?.content ?? bodyMd ?? ""}
            syncSeq={view?.syncSeq ?? 0}
            binding={view?.binding ?? null}
            interactiveCheckbox={false}
            links={textMarkdownLinks}
            editorViewRef={cmViewRef}
            onBodyChange={(md) => session.applyBody(md)}
            onCollabDivergence={(ytextText) => session.handleCollabDivergence(ytextText)}
          />
        ) : editing ? (
          /* 画布内文本节点：正文随 .atlx 内嵌，草稿提交由退出编辑触发（不涉及仓库文件）；
             撤销走 CM 本地历史（该编辑面没有按文件持久栈） */
          <MarkdownEditor
            body={draft}
            syncSeq={0}
            localHistory
            interactiveCheckbox={false}
            links={textMarkdownLinks}
            editorViewRef={cmViewRef}
            onBodyChange={setDraft}
          />
        ) : (
          <>
            {isSaved && !noteSurface && (
              <p className="text-[11px] mb-1" style={{ color: "#f59e0b" }}>
                笔记能力未启用，只能查看
              </p>
            )}
            <MarkdownView
              text={previewBody || "*（空）*"}
              links={textMarkdownLinks}
              className="h-full"
            />
          </>
        )}
      </div>

      {!readOnly && <ResizeHandle />}
    </div>
  );
}
