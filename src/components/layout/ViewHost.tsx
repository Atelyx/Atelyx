/**
 * 视图承载（主窗口面板与撕裂窗口共用）：所有视图经统一视图贡献注册表分派（默认组合与用户插件同表），
 * 内核不硬编码视图，只做渲染宿主。hostId = 面板 id 或撕裂窗口 id（画布/表格聚焦门控用）；
 * `ViewStatusIndicator` 供面板头/撕裂窗口头渲染保存/错误状态。
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
import { useNoteStore } from "@/stores/noteStore";
import { usePluginStore } from "@/stores/pluginStore";
import { ErrorBoundary } from "@/components/common/ErrorBoundary";
import { Button, IconButton } from "@/components/common/Button";
import { StatusPill } from "@/components/common/Badge";
import { ViewBootSkeleton } from "@/components/layout/PanelSkeleton";
import { viewFallbackOf } from "@/utils/viewFallback";
import type { BuiltinViewKind, ViewKind } from "@/types";

/** 视图元信息（标签/头部共用；显示名单一来源 = VIEW_LABELS，图标在此维护）。
 *  键 = 宿主视图类型（穷举保护）；插件视图走 viewMetaFor 兜底。 */
export const VIEW_META: Record<BuiltinViewKind, { label: string; icon: ReactNode }> = {
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

/** 视图贡献承载：按 kind 渲染注册的组件（统一视图槽；缺注册 = 降级占位）。
 *  精确订阅——selector 只选本 kind 的贡献引用，贡献增删/替换时本组件重渲染（占位 ↔ 视图切换），
 *  其他插件注册事件（uiRevision 变化但本 kind 贡献不变）不打扰重型视图（画布/笔记编辑器等）。 */
function ViewContributionMount({ kind, hostId }: { kind: string; hostId: string }) {
  const contrib = usePluginStore((s) => s.viewContribution(kind));
  const assemblyLoading = usePluginStore((s) => s.assemblyLoading);
  const initialized = usePluginStore((s) => s.initialized);
  if (!contrib) {
    // 缺贡献：装配在途 = 骨架；kind 由随应用分发的默认实现提供但其行不可用（停用/卸载）→
    // 降级占位（提示处置入口）；否则保持空白占位（未知 kind / 插件视图已卸载，不猜测原因）。
    const provider = usePluginStore.getState().viewProviderState(kind);
    const fallback = viewFallbackOf({ assemblyLoading, initialized, provider });
    if (fallback === "skeleton") return <ViewBootSkeleton />;
    if (fallback === "provider" && provider) {
      return (
        <div className="h-full w-full flex items-center justify-center" style={{ background: "var(--bg-primary)" }}>
          <div className="text-xs text-center px-6 leading-relaxed" style={{ color: "var(--text-muted)" }}>
            「{provider.name}」插件{provider.installed ? "已停用" : "已卸载"}
            <br />
            {provider.installed ? "可在设置 → 插件中重新启用" : "可在设置 → 插件中恢复"}
          </div>
        </div>
      );
    }
    return <div className="h-full w-full" style={{ background: "var(--bg-primary)" }} />;
  }
  const Comp = contrib.component;
  return (
    <ErrorBoundary>
      {/* 重型视图（画布/表格）经贡献的 render(hostId) 接收宿主 id（聚焦门控）；普通插件面板组件契约无 props */}
      {contrib.render ? contrib.render(hostId) : Comp ? <Comp /> : null}
    </ErrorBoundary>
  );
}

/**
 * 按视图类型分派渲染（memo：view/hostId 为稳定原始值——布局广播全量更新时，
 * 无关面板的视图组件不重渲染，画布/编辑器不被拖拽 resize 等高频广播打扰）。
 * 所有视图经统一视图贡献注册表分派（默认组合与用户插件同表），本组件只做渲染宿主。
 */
export const ViewHost = memo(function ViewHost({ view, hostId }: { view: ViewKind; hostId: string }) {
  return <ViewContributionMount kind={view} hostId={hostId} />;
});

/** 视图状态条的错误提示（danger 配色 + 可选重试按钮 + 关闭按钮；画布/表格共用）。 */
function ErrorPill({ error, onRetry, onDismiss }: { error: string; onRetry?: () => void; onDismiss: () => void }) {
  const actionStyle = { background: "color-mix(in srgb, var(--danger) 20%, transparent)" };
  return (
    <span
      className="flex items-center gap-1 px-1.5 py-0.5 rounded flex-shrink-0"
      style={{ color: "var(--danger)", background: "color-mix(in srgb, var(--danger) 12%, transparent)" }}
    >
      <span className="truncate max-w-[160px]">{error}</span>
      {onRetry && (
        <Button variant="danger" size="sm" onClick={onRetry} style={actionStyle}>
          重试
        </Button>
      )}
      <IconButton variant="danger" size="sm" icon={<X size={12} />} label="关闭错误提示" style={actionStyle} onClick={onDismiss} />
    </span>
  );
}

/** 画布视图状态指示（无当前画布不显示；错误 > 保存状态）。 */
function CanvasStatusIndicator() {
  const canvasId = useCanvasStore((s) => s.canvasId);
  const canvasFile = useAppStore((s) => s.currentCanvasFile);
  const loading = useCanvasStore((s) => s.loading);
  const saving = useCanvasStore((s) => s.saving);
  const readOnly = useCanvasStore((s) => s.readOnly);
  const error = useCanvasStore((s) => s.error);
  const clearError = useCanvasStore((s) => s.clearError);
  const load = useCanvasStore((s) => s.load);
  if (!canvasId) return null;
  if (error) {
    return (
      <ErrorPill
        error={error}
        onRetry={error === "加载画布失败，请重试" && canvasFile ? () => void load(canvasFile) : undefined}
        onDismiss={() => clearError()}
      />
    );
  }
  if (loading) return <StatusPill status="pending" label="加载中…" />;
  if (saving) return <StatusPill status="pending" label="保存中…" />;
  if (readOnly) return <StatusPill status="off" label="只读（外部白板格式）" />;
  return <StatusPill status="ok" label="已自动保存" />;
}

/** 表格视图状态指示（无当前表格不显示；错误 > 保存状态）。 */
function TableStatusIndicator() {
  const currentTableFile = useAppStore((s) => s.currentTableFile);
  const saving = useTableStore((s) => s.saving);
  const error = useTableStore((s) => s.error);
  const clearError = useTableStore((s) => s.clearError);
  if (!currentTableFile) return null;
  if (error) {
    return <ErrorPill error={error} onDismiss={() => clearError()} />;
  }
  if (saving) return <StatusPill status="pending" label="保存中…" />;
  return <StatusPill status="ok" label="已自动保存" />;
}

/** 笔记视图状态指示（无当前笔记不显示；保存状态）。 */
function NoteStatusIndicator() {
  const currentNoteFile = useAppStore((s) => s.currentNoteFile);
  const status = useNoteStore((s) => (currentNoteFile ? s.noteSaveStates[currentNoteFile] : undefined));
  if (!currentNoteFile) return null;
  if (!status) return null;
  if (status.loadError) return <StatusPill status="error" label="读取失败" />;
  if (status.state === "saving") return <StatusPill status="pending" label="保存中…" />;
  if (status.state === "error") return <StatusPill status="error" label="保存失败" />;
  // 「未保存」是有待落盘的待办（警告档），不是失败
  if (status.state === "edited") return <StatusPill status="pending" label="未保存" />;
  if (status.state === "saved") return <StatusPill status="ok" label="已自动保存" />;
  return null;
}

/** 按视图类型分派状态指示（view 变化 = 子组件类型切换，各子组件 hooks 固定）。
 *  画布/表格/笔记视图由随应用分发的插件提供：其行停用/卸载（贡献缺失）时面板已是降级占位，
 *  状态指示一并隐藏，不显示过期的保存状态。订阅 uiRevision 使同一窗口内插件启停
 *  也能收敛。注意：Tauri 各窗口是独立 WebView（各自 pluginStore/视图注册表，windowBus
 *  无插件状态广播），主窗口启停插件不会同步到撕裂窗口——撕裂窗口仅在其自身动作下收敛。 */
export function ViewStatusIndicator({ view }: { view: ViewKind }) {
  usePluginStore((s) => s.uiRevision);
  switch (view) {
    case "canvas":
    case "table":
    case "note":
      if (!usePluginStore.getState().viewContribution(view)) return null;
      if (view === "canvas") return <CanvasStatusIndicator />;
      if (view === "table") return <TableStatusIndicator />;
      return <NoteStatusIndicator />;
    default:
      return null;
  }
}
