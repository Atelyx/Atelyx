/**
 * 视图承载（主窗口面板与撕裂窗口共用）：按视图类型分派渲染 + 头部状态指示。
 *
 * hostId = 面板 id 或撕裂窗口 id（画布/表格聚焦门控用）。各视图内容由全局 store
 * 驱动（画布/表格/笔记打开文件状态），本组件只做分派；`ViewStatusIndicator` 供
 * 面板头/撕裂窗口头渲染保存/冲突/错误状态。
 */
import {
  CalendarDays,
  Clock,
  FileText,
  Files,
  History,
  Info,
  LayoutTemplate,
  Palette,
  Puzzle,
  Search,
  Sparkles,
  Table as TableIcon,
  Users,
  X,
} from "lucide-react";
import { memo, type ReactNode } from "react";
import { VIEW_LABELS } from "@/constants/views";
import { useCanvasStore } from "@/stores/canvasStore";
import { useAppStore } from "@/stores/appStore";
import { useTableStore } from "@/stores/tableStore";
import { useVaultStore } from "@/stores/vaultStore";
import { usePluginStore } from "@/stores/pluginStore";
import { ErrorBoundary } from "@/components/common/ErrorBoundary";
import { CanvasView } from "@/components/layout/views/CanvasView";
import { NoteView } from "@/components/layout/views/NoteView";
import { TableView } from "@/components/layout/views/TableView";
import { FilesView } from "@/components/layout/views/FilesView";
import { AiChatView } from "@/components/layout/views/AiChatView";
import { InspectorPanel } from "@/components/canvas/panels/InspectorPanel";
import { CollabRoomPanel } from "@/components/canvas/panels/CollabRoomPanel";
import { RepoHistoryPanel } from "@/components/history/RepoHistoryPanel";
import type { ViewKind } from "@/types";

/** 视图元信息（标签/头部共用；显示名单一来源 = VIEW_LABELS，图标在此维护）。 */
export const VIEW_META: Record<ViewKind, { label: string; icon: ReactNode }> = {
  canvas: { label: VIEW_LABELS.canvas, icon: <Palette size={13} /> },
  note: { label: VIEW_LABELS.note, icon: <FileText size={13} /> },
  table: { label: VIEW_LABELS.table, icon: <TableIcon size={13} /> },
  files: { label: VIEW_LABELS.files, icon: <Files size={13} /> },
  search: { label: VIEW_LABELS.search, icon: <Search size={13} /> },
  inspector: { label: VIEW_LABELS.inspector, icon: <Info size={13} /> },
  aichat: { label: VIEW_LABELS.aichat, icon: <Sparkles size={13} /> },
  collabroom: { label: VIEW_LABELS.collabroom, icon: <Users size={13} /> },
  calendar: { label: VIEW_LABELS.calendar, icon: <CalendarDays size={13} /> },
  repohistory: { label: VIEW_LABELS.repohistory, icon: <History size={13} /> },
  recent: { label: VIEW_LABELS.recent, icon: <Clock size={13} /> },
  empty: { label: VIEW_LABELS.empty, icon: <LayoutTemplate size={13} /> },
};

/**
 * 视图元信息解析：内建 VIEW_META（内置视图元数据权威）→ 统一注册表（插件面板 label）→ 原样兜底。
 * 插件视图可能出现在持久化布局里（插件停用后仍在），各处索引必须走此函数避免崩溃。
 */
export function viewMetaFor(view: string): { label: string; icon: ReactNode } {
  const builtin = (VIEW_META as Record<string, { label: string; icon: ReactNode }>)[view];
  if (builtin) return builtin;
  const contrib = usePluginStore.getState().viewContribution(view);
  if (contrib) return { label: contrib.label, icon: <Puzzle size={13} /> };
  return { label: view, icon: <Puzzle size={13} /> };
}

/** 视图贡献承载：按 kind 渲染注册的组件（内置插件/插件面板同表；缺注册 = 空面板占位）。
 *  订阅 uiRevision——插件异步注册晚于首渲染时自动升级占位；ViewHost 本体不订阅，
 *  避免内置重型视图（笔记编辑器等）在插件注册时被无效重渲染。 */
function ViewContributionMount({ kind }: { kind: string }) {
  usePluginStore((s) => s.uiRevision);
  const contrib = usePluginStore.getState().viewContribution(kind);
  if (!contrib) {
    // 缺贡献：kind 由不可用的内置插件提供（停用/卸载）→ 降级占位（提示处置入口）；
    // 否则保持空白占位（未知 kind / 第三方插件已卸载，不猜测原因）。
    const state = usePluginStore.getState().viewKindState(kind);
    if (state && !state.enabled) {
      return (
        <div className="h-full w-full flex items-center justify-center" style={{ background: "var(--bg-primary)" }}>
          <div className="text-xs text-center px-6 leading-relaxed" style={{ color: "var(--text-muted)" }}>
            「{state.name}」为内置插件，{state.installed ? "已停用" : "已卸载"}
            <br />
            {state.installed ? "可在设置 → 插件中重新启用" : "可在设置 → 插件中恢复"}
          </div>
        </div>
      );
    }
    return <div className="h-full w-full" style={{ background: "var(--bg-primary)" }} />;
  }
  const Comp = contrib.component;
  return (
    <ErrorBoundary>
      <Comp />
    </ErrorBoundary>
  );
}

/**
 * 按视图类型分派渲染（memo：view/hostId 为稳定原始值——布局广播全量更新时，
 * 无关面板的视图组件不重渲染，画布/编辑器不被拖拽 resize 等高频广播打扰）。
 */
export const ViewHost = memo(function ViewHost({ view, hostId }: { view: ViewKind; hostId: string }) {
  switch (view) {
    case "canvas":
      return <CanvasView panelId={hostId} />;
    case "note":
      return <NoteView />;
    case "table":
      return <TableView panelId={hostId} />;
    case "files":
      return <FilesView />;
    case "inspector":
      return <InspectorPanel />;
    case "aichat":
      return <AiChatView />;
    case "collabroom":
      return <CollabRoomPanel />;
    case "repohistory":
      return <RepoHistoryPanel />;
    default:
      // 统一视图注册表：内置插件（搜索/最近打开/日历）与插件面板都由 ViewContributionMount 承载——
      // 组件内订阅 uiRevision，异步注册/卸载自动升级或回退占位；"empty" 无贡献同样走空白占位。
      return <ViewContributionMount kind={view} />;
  }
});

/** 画布视图状态指示（无当前画布不显示；冲突 > 错误 > 保存状态）。 */
function CanvasStatusIndicator() {
  const canvasId = useCanvasStore((s) => s.canvasId);
  const canvasFile = useAppStore((s) => s.currentCanvasFile);
  const loading = useCanvasStore((s) => s.loading);
  const saving = useCanvasStore((s) => s.saving);
  const readOnly = useCanvasStore((s) => s.readOnly);
  const conflictPending = useCanvasStore((s) => s.conflictPending);
  const mergeFromDisk = useCanvasStore((s) => s.mergeFromDisk);
  const reloadFromDisk = useCanvasStore((s) => s.reloadFromDisk);
  const error = useCanvasStore((s) => s.error);
  const clearError = useCanvasStore((s) => s.clearError);
  const load = useCanvasStore((s) => s.load);
  if (!canvasId) return null;
  if (conflictPending) {
    return (
      <span
        className="flex items-center gap-1 px-1.5 py-0.5 rounded flex-shrink-0"
        style={{ color: "#f59e0b", background: "rgba(245,158,11,0.1)" }}
      >
        <span className="truncate max-w-[150px]">画布与外部修改冲突</span>
        <button
          onClick={() => void mergeFromDisk()}
          className="px-1 rounded hover:opacity-80"
          style={{ background: "rgba(245,158,11,0.2)", color: "#f59e0b" }}
          title="以磁盘为基底保留本地新增内容（重叠以磁盘为准）"
        >
          合并
        </button>
        <button
          onClick={() => void reloadFromDisk()}
          className="px-1 rounded hover:opacity-80"
          style={{ background: "rgba(245,158,11,0.2)", color: "#f59e0b" }}
          title="丢弃本地改动，加载磁盘最新内容"
        >
          重载
        </button>
      </span>
    );
  }
  if (error) {
    return (
      <span
        className="flex items-center gap-1 px-1.5 py-0.5 rounded flex-shrink-0"
        style={{ color: "#f87171", background: "rgba(248,113,113,0.1)" }}
      >
        <span className="truncate max-w-[160px]">{error}</span>
        {error === "加载画布失败，请重试" && canvasFile && (
          <button
            onClick={() => void load(canvasFile)}
            className="px-1 rounded hover:opacity-80"
            style={{ background: "rgba(248,113,113,0.2)", color: "#f87171" }}
          >
            重试
          </button>
        )}
        <button
          onClick={() => clearError()}
          className="px-1 rounded hover:opacity-80"
          style={{ background: "rgba(248,113,113,0.2)", color: "#f87171" }}
          aria-label="关闭错误提示"
        >
          <X size={12} />
        </button>
      </span>
    );
  }
  return (
    <span className="flex-shrink-0 text-xs" style={{ color: "var(--text-muted)" }}>
      {loading ? "加载中…" : saving ? "保存中…" : readOnly ? "只读（外部白板格式）" : "已自动保存"}
    </span>
  );
}

/** 表格视图状态指示（无当前表格不显示；冲突 > 错误 > 保存状态）。 */
function TableStatusIndicator() {
  const currentTableFile = useAppStore((s) => s.currentTableFile);
  const saving = useTableStore((s) => s.saving);
  const conflictPending = useTableStore((s) => s.conflictPending);
  const resolveConflict = useTableStore((s) => s.resolveConflict);
  const error = useTableStore((s) => s.error);
  const clearError = useTableStore((s) => s.clearError);
  if (!currentTableFile) return null;
  if (conflictPending) {
    return (
      <span
        className="flex items-center gap-1 px-1.5 py-0.5 rounded flex-shrink-0"
        style={{ color: "#f59e0b", background: "rgba(245,158,11,0.1)" }}
      >
        <span className="truncate max-w-[150px]">表格与外部修改冲突</span>
        <button
          onClick={() => void resolveConflict(false)}
          className="px-1 rounded hover:opacity-80"
          style={{ background: "rgba(245,158,11,0.2)", color: "#f59e0b" }}
          title="丢弃本地改动，加载磁盘最新内容"
        >
          重新加载
        </button>
        <button
          onClick={() => void resolveConflict(true)}
          className="px-1 rounded hover:opacity-80"
          style={{ background: "rgba(245,158,11,0.2)", color: "#f59e0b" }}
          title="用本地内容覆盖磁盘（外部改动丢失）"
        >
          保留本地
        </button>
      </span>
    );
  }
  if (error) {
    return (
      <span
        className="flex items-center gap-1 px-1.5 py-0.5 rounded flex-shrink-0"
        style={{ color: "#f87171", background: "rgba(248,113,113,0.1)" }}
      >
        <span className="truncate max-w-[160px]">{error}</span>
        <button
          onClick={() => clearError()}
          className="px-1 rounded hover:opacity-80"
          style={{ background: "rgba(248,113,113,0.2)", color: "#f87171" }}
          aria-label="关闭错误提示"
        >
          <X size={12} />
        </button>
      </span>
    );
  }
  return (
    <span className="flex-shrink-0 text-xs" style={{ color: "var(--text-muted)" }}>
      {saving ? "保存中…" : "已自动保存"}
    </span>
  );
}

/** 笔记视图状态指示（无当前笔记不显示；冲突 > 保存状态）。 */
function NoteStatusIndicator() {
  const currentNoteFile = useAppStore((s) => s.currentNoteFile);
  const conflict = useVaultStore((s) => (currentNoteFile ? s.noteConflicts[currentNoteFile] : false));
  const status = useVaultStore((s) => (currentNoteFile ? s.noteSaveStates[currentNoteFile] : undefined));
  const resolveNoteConflict = useVaultStore((s) => s.resolveNoteConflict);
  if (!currentNoteFile) return null;
  if (conflict) {
    return (
      <span
        className="flex items-center gap-1 px-1.5 py-0.5 rounded flex-shrink-0"
        style={{ color: "#f59e0b", background: "rgba(245,158,11,0.1)" }}
      >
        <span className="truncate max-w-[150px]">外部已修改此文件</span>
        <button
          onClick={() => resolveNoteConflict(currentNoteFile, false)}
          className="px-1 rounded hover:opacity-80"
          style={{ background: "rgba(245,158,11,0.2)", color: "#f59e0b" }}
          title="丢弃本地改动，加载外部最新内容"
        >
          重新加载
        </button>
        <button
          onClick={() => resolveNoteConflict(currentNoteFile, true)}
          className="px-1 rounded hover:opacity-80"
          style={{ background: "rgba(245,158,11,0.2)", color: "#f59e0b" }}
          title="用本地内容覆盖外部修改并立即保存"
        >
          保留本地
        </button>
      </span>
    );
  }
  if (!status) return null;
  const text = status.loadError
    ? "读取失败"
    : status.state === "saving"
      ? "保存中…"
      : status.state === "error"
        ? "保存失败"
        : status.state === "edited"
          ? "未保存"
          : status.state === "saved"
            ? "已自动保存"
            : null;
  if (!text) return null;
  return (
    <span
      className="flex-shrink-0 text-xs"
      style={{ color: status.loadError || status.state === "error" ? "#f87171" : "var(--text-muted)" }}
    >
      {text}
    </span>
  );
}

/** 按视图类型分派状态指示（view 变化 = 子组件类型切换，各子组件 hooks 固定）。 */
export function ViewStatusIndicator({ view }: { view: ViewKind }) {
  switch (view) {
    case "canvas":
      return <CanvasStatusIndicator />;
    case "table":
      return <TableStatusIndicator />;
    case "note":
      return <NoteStatusIndicator />;
    default:
      return null;
  }
}
