/**
 * 内置插件载荷（组件层承载组件引用——services 不 import components）。
 *
 * 内置插件 = 随 App 分发的插件（plugin-state 种子条目，sourceKind builtin）；实现随宿主编译，
 * 本模块是 id → 宿主侧载荷的装载映射。pluginStore 按插件行启停状态注册/撤销：
 * - `views`：视图贡献（停用即撤销 → 面板降级占位、菜单不再提供）；
 * - `lifecycle`：领域生命周期钩子（flush/切仓库/释放视图/视图进出，注册进
 *   `utils/kernelLifecycle` 注册表——内核启动路径只做分发；停用即撤销）；
 * - `capability` / `collabWiring` / `vaultEventHandlers`：随插件启停的能力提供者/协作域接线/
 *   仓库事件订阅（未接线时为 undefined，接入点随对应功能落地）。
 *
 * 数组顺序 = 领域生命周期钩子的 flush 注册序（对齐 appStore.selectVault 重构前的硬编码 flush
 * 顺序：canvas → table → aichat → calendar → note）。
 *
 * 注意：id 与 Rust 侧内置插件清单（commands/plugin.rs）一一对应，新增内置插件须两侧同步；
 * 本模块被 pluginStore 静态 import，形成 pluginStore → 视图组件 → 各 store 的模块环——
 * 环内所有跨模块访问均为函数体内延迟求值（无顶层 getState/useXxx），新增顶层触碰会 TDZ 崩溃。
 */
import type { ComponentType, ReactNode } from "react";
import type { DomainLifecycleHooks } from "@/utils/kernelLifecycle";
import type { VaultEventHandler, VaultEvent, VaultEventOf } from "@/utils/vaultEvents";
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
import { useAppStore } from "@/stores/appStore";
import { useCanvasStore, registerCanvasCollabWiring, registerCanvasPluginWiring } from "@/stores/canvasStore";
import { useTableStore, registerTableCollabWiring, registerTablePluginWiring } from "@/stores/tableStore";
import { registerNoteCollabWiring } from "@/stores/noteCollabStore";
import { useChatPanelStore } from "@/stores/chatPanelStore";
import { useCalendarStore } from "@/stores/calendarStore";
import { useNoteUndoStore } from "@/stores/noteUndoStore";
import { useVaultStore } from "@/stores/vaultStore";
import { VIEW_LABELS } from "@/constants/views";

/** 单个视图载荷（无 props 契约的视图组件；重型视图用 render 承载宿主面板 id）。 */
export interface BuiltinViewPayload {
  kind: string;
  label: string;
  /** 无 props 契约的视图组件（第三方面板同款；重型视图用 render，不填 component）。 */
  component?: ComponentType;
  /** 内置重型视图按宿主面板/撕裂窗口 id 渲染（画布/表格需 panelId 聚焦门控）。 */
  render?: (hostId: string) => ReactNode;
}

/** 仓库事件订阅条目（kind 显式声明；handler 按 kind 收窄载荷）。 */
export interface VaultEventHandlerSpec {
  kind: VaultEvent["kind"];
  handler: VaultEventHandler;
}

/** 声明仓库事件订阅（kind 与 handler 载荷按 kind 收窄；存储层统一为宽化 handler）。 */
function vaultHandler<K extends VaultEvent["kind"]>(
  kind: K,
  handler: (event: VaultEventOf<K>) => void,
): VaultEventHandlerSpec {
  return { kind, handler: handler as VaultEventHandler };
}

/** 单个内置插件的宿主侧载荷。 */
export interface BuiltinPayload {
  pluginId: string;
  /** 视图贡献载荷（kind 全局唯一）。 */
  views: BuiltinViewPayload[];
  /** 领域生命周期钩子（随插件启停注册进 kernelLifecycle；停用即撤销）。 */
  lifecycle?: DomainLifecycleHooks;
  /** 能力提供者接线（随插件启停；返回 unregister）。 */
  capability?: () => () => void;
  /** 协作域接线（随插件启停；返回 unregister）。 */
  collabWiring?: () => () => void;
  /** 仓库事件订阅（随插件启停）。 */
  vaultEventHandlers?: VaultEventHandlerSpec[];
}

/** 内置插件载荷总表（pluginId 全局唯一；views 内 kind 全局唯一）。 */
export const BUILTIN_PAYLOADS: BuiltinPayload[] = [
  {
    pluginId: "builtin.search",
    views: [{ kind: "search", label: VIEW_LABELS.search, component: SearchView }],
  },
  {
    pluginId: "builtin.recent",
    views: [{ kind: "recent", label: VIEW_LABELS.recent, component: RecentPanel }],
  },
  {
    pluginId: "builtin.calendar",
    views: [{ kind: "calendar", label: VIEW_LABELS.calendar, component: CalendarPanel }],
    lifecycle: {
      id: "builtin.calendar",
      flush: async () => {
        await useCalendarStore.getState().flush();
      },
      onVaultExit: async () => {
        await useCalendarStore.getState().flush();
      },
    },
  },
  {
    pluginId: "builtin.aichat",
    views: [{ kind: "aichat", label: VIEW_LABELS.aichat, component: AiChatView }],
    lifecycle: {
      id: "builtin.aichat",
      flush: async (ctx) => {
        await useChatPanelStore.getState().flush(ctx.vaultId);
      },
      onVaultEntered: async (ctx) => {
        // 进仓后读盘加载 AI 会话（force：真实切换强制重读，防幂等守卫跳过旧会话）
        await useChatPanelStore.getState().load(ctx.vaultId, true);
      },
      onVaultExit: async () => {
        await useChatPanelStore.getState().flush(useAppStore.getState().vaultId);
      },
      onViewGained: (view) => {
        if (view !== "aichat") return;
        // 撕裂出去的 AI 会话视图回归主窗口：重读盘（面板窗口可能已改会话）
        void useChatPanelStore.getState().load(useAppStore.getState().vaultId);
      },
      releaseView: async (view) => {
        if (view !== "aichat") return;
        await useChatPanelStore.getState().flush(useAppStore.getState().vaultId);
      },
    },
  },
  {
    pluginId: "builtin.canvas",
    views: [
      {
        kind: "canvas",
        label: VIEW_LABELS.canvas,
        render: (hostId) => <CanvasView panelId={hostId} />,
      },
    ],
    lifecycle: {
      id: "builtin.canvas",
      flush: async () => {
        await useCanvasStore.getState().flush();
      },
      onVaultLeaving: () => {
        useCanvasStore.getState().resetCanvasState();
      },
      releaseView: async (view) => {
        if (view !== "canvas") return;
        await useCanvasStore.getState().flush();
        useCanvasStore.getState().resetCanvasState();
      },
      onViewRemoved: (view) => {
        if (view !== "canvas") return;
        useCanvasStore.getState().selectNode(null);
      },
    },
    capability: registerCanvasPluginWiring,
    collabWiring: registerCanvasCollabWiring,
    vaultEventHandlers: [
      vaultHandler("canvas:renamed", async (e) => {
        try {
          // 磁盘 .atlx 已被重命名：同步当前画布乐观锁基准 + 打开路径（防回写旧路径/乐观锁误冲突）
          await useCanvasStore.getState().syncBaseUpdatedAt();
          if (useAppStore.getState().currentCanvasFile === e.oldPath) {
            useCanvasStore.setState({ canvasFile: e.newPath });
          }
        } catch (err) {
          console.error("画布重命名引用同步失败", err);
        }
      }),
      vaultHandler("canvas:moved", async (e) => {
        try {
          await useCanvasStore.getState().syncBaseUpdatedAt();
          if (useAppStore.getState().currentCanvasFile === e.oldPath) {
            useCanvasStore.setState({ canvasFile: e.newPath });
          }
        } catch (err) {
          console.error("画布移动引用同步失败", err);
        }
      }),
      vaultHandler("canvas:deleted", (e) => {
        // 当前画布被删除：复位运行时（防残留 saveTimer 重写已删文件）
        if (useAppStore.getState().currentCanvasFile === e.path) {
          useCanvasStore.getState().resetCanvasState();
        }
      }),
      vaultHandler("canvas:error", (e) => {
        useCanvasStore.setState({ error: e.message });
      }),
    ],
  },
  {
    pluginId: "builtin.note",
    views: [{ kind: "note", label: VIEW_LABELS.note, component: NoteView }],
    lifecycle: {
      id: "builtin.note",
      flush: async () => {
        await useVaultStore.getState().flushPendingNotes();
      },
      onVaultLeaving: () => {
        // 切仓库清空笔记撤销栈（防同路径串文件）
        useNoteUndoStore.getState().clearAll();
      },
      onVaultExit: async () => {
        await useVaultStore.getState().flushPendingNotes();
      },
    },
    collabWiring: registerNoteCollabWiring,
  },
  {
    pluginId: "builtin.table",
    views: [
      {
        kind: "table",
        label: VIEW_LABELS.table,
        render: (hostId) => <TableView panelId={hostId} />,
      },
    ],
    lifecycle: {
      id: "builtin.table",
      flush: async () => {
        await useTableStore.getState().flush();
      },
      onVaultExit: async () => {
        await useTableStore.getState().flush();
      },
      releaseView: async (view) => {
        if (view !== "table") return;
        await useTableStore.getState().flush();
        useTableStore.getState().clear();
      },
    },
    capability: registerTablePluginWiring,
    collabWiring: registerTableCollabWiring,
    vaultEventHandlers: [
      vaultHandler("table:deleted", (e) => {
        // 打开的表格文件已被删除：只清内存态（flush 会写回重建已删文件）
        if (useAppStore.getState().currentTableFile === e.path) {
          useTableStore.getState().clear();
        }
      }),
    ],
  },
  {
    pluginId: "builtin.files",
    views: [{ kind: "files", label: VIEW_LABELS.files, component: FilesView }],
  },
  {
    pluginId: "builtin.inspector",
    views: [{ kind: "inspector", label: VIEW_LABELS.inspector, component: InspectorPanel }],
  },
  {
    pluginId: "builtin.collabroom",
    views: [{ kind: "collabroom", label: VIEW_LABELS.collabroom, component: CollabRoomPanel }],
  },
  {
    pluginId: "builtin.repohistory",
    views: [{ kind: "repohistory", label: VIEW_LABELS.repohistory, component: RepoHistoryPanel }],
  },
  {
    pluginId: "builtin.theme",
    views: [], // 主题类内置插件 = 纯声明式（无视图载荷），只置 active 供主题系统派生消费
  },
];
