/**
 * 内置插件视图载荷（组件层承载组件引用——services 不 import components）。
 *
 * 内置插件 = 随 App 分发的插件（plugin-state 种子条目，sourceKind builtin）；实现随宿主编译，
 * 本模块是 id → 宿主视图组件的装载映射。pluginStore 按插件行启停状态经注册表注册/撤销
 * （停用即撤销 → 面板降级占位、菜单不再提供）。
 *
 * 注意：id 与 Rust 侧内置插件清单（commands/plugin.rs）一一对应，新增内置插件须两侧同步；
 * 本模块被 pluginStore 静态 import，形成 pluginStore → 视图组件 → 各 store 的模块环——
 * 环内所有跨模块访问均为函数体内延迟求值（无顶层 getState/useXxx），新增顶层触碰会 TDZ 崩溃。
 */
import type { ComponentType, ReactNode } from "react";
import { CalendarPanel } from "@/components/calendar/CalendarPanel";
import { RecentPanel } from "@/components/layout/panels/RecentPanel";
import { SearchView } from "@/components/layout/views/SearchView";
import { AiChatView } from "@/components/layout/views/AiChatView";
import { CanvasView } from "@/components/layout/views/CanvasView";
import { NoteView } from "@/components/layout/views/NoteView";
import { TableView } from "@/components/layout/views/TableView";
import { FilesView } from "@/components/layout/views/FilesView";
import { InspectorPanel } from "@/components/canvas/panels/InspectorPanel";
import { CollabRoomPanel } from "@/components/canvas/panels/CollabRoomPanel";
import { RepoHistoryPanel } from "@/components/history/RepoHistoryPanel";
import { VIEW_LABELS } from "@/constants/views";

interface BuiltinViewPayload {
  pluginId: string;
  kind: string;
  label: string;
  /** 无 props 契约的视图组件（第三方面板同款；重型视图用 render，不填 component）。 */
  component?: ComponentType;
  /** 内置重型视图按宿主面板/撕裂窗口 id 渲染（画布/表格需 panelId 聚焦门控）。 */
  render?: (hostId: string) => ReactNode;
}

/** 内置插件视图载荷（kind 全局唯一）。 */
export const BUILTIN_VIEWS: BuiltinViewPayload[] = [
  { pluginId: "builtin.search", kind: "search", label: VIEW_LABELS.search, component: SearchView },
  { pluginId: "builtin.recent", kind: "recent", label: VIEW_LABELS.recent, component: RecentPanel },
  { pluginId: "builtin.calendar", kind: "calendar", label: VIEW_LABELS.calendar, component: CalendarPanel },
  { pluginId: "builtin.aichat", kind: "aichat", label: VIEW_LABELS.aichat, component: AiChatView },
  { pluginId: "builtin.canvas", kind: "canvas", label: VIEW_LABELS.canvas, render: (hostId) => <CanvasView panelId={hostId} /> },
  { pluginId: "builtin.note", kind: "note", label: VIEW_LABELS.note, component: NoteView },
  { pluginId: "builtin.table", kind: "table", label: VIEW_LABELS.table, render: (hostId) => <TableView panelId={hostId} /> },
  { pluginId: "builtin.files", kind: "files", label: VIEW_LABELS.files, component: FilesView },
  { pluginId: "builtin.inspector", kind: "inspector", label: VIEW_LABELS.inspector, component: InspectorPanel },
  { pluginId: "builtin.collabroom", kind: "collabroom", label: VIEW_LABELS.collabroom, component: CollabRoomPanel },
  { pluginId: "builtin.repohistory", kind: "repohistory", label: VIEW_LABELS.repohistory, component: RepoHistoryPanel },
];
