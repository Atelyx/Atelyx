/**
 * 第一方 Cordis 插件注册表（components 层承载组件引用——services 不 import components）。
 *
 * 内置插件 = 随 App 分发的插件（sourceKind builtin）；实现随宿主编译，本模块是 id → Cordis
 * 插件定义的装载映射。pluginStore 按插件行启停状态经 loader 挂载/卸载（fiber 生命周期）：
 * - 视图贡献 → view/<kind> 槽（single；重型视图 render(hostId) 承载宿主面板 id）；
 * - 领域生命周期钩子（flush/切仓库/释放视图）经 kernelLifecycle 注册，随 fiber 撤销；
 * - 能力提供者 / 协作域接线 / 仓库事件订阅经 ctx.effect 注册（apply 中途抛错/卸载均自动撤销）；
 * - builtin.canvas/table 额外提供 ctx.canvas/ctx.table 类型化服务（停用即消失，语义同现状）。
 *
 * 数组顺序 = 领域生命周期钩子的 flush 注册序（对齐既有注册序）；也是 profile 装配顺序。
 * 注意：id 与 Rust 侧内置插件清单（commands/plugin.rs）一一对应，新增内置插件须两侧同步；
 * 本模块被 pluginStore 静态 import，环内所有跨模块访问均为函数体内延迟求值（无顶层
 * getState/useXxx），新增顶层触碰会 TDZ 崩溃。
 */
import type { ComponentType, ReactNode } from "react";
import type { Context } from "@atelyx/cordis";
import type { PluginManifest, PluginType, ThemeDefinition } from "@/types";
import type { Profile } from "@/utils/cordis/composition";
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
import { registerDomainLifecycle } from "@/utils/kernelLifecycle";
import { subscribeVaultEvent } from "@/utils/vaultEvents";
import { registerViewSlot } from "@/services/cordis/slots";
import { createCanvasService } from "@/services/cordis/canvas";
import { createTableService } from "@/services/cordis/table";
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

/** 单个第一方插件定义。 */
export interface CordisBuiltinDef {
  id: string;
  name: string;
  type: string;
  tagline: string;
  /** 装配顺序（= 领域生命周期 flush 注册序）。 */
  order: number;
  /** 视图载荷（kind → 插件 id 映射与测试用；apply 据此注册槽）。 */
  views: BuiltinViewPayload[];
  /** 挂载实现（pluginStore 经 loader 调 ctx.plugin(apply)）。 */
  apply(ctx: Context): void;
}

// ===== 挂载装配辅助（全部经 ctx.effect，生命周期随 fiber） =====

function mountViews(ctx: Context, pluginId: string, views: BuiltinViewPayload[]): void {
  for (const v of views) {
    ctx.effect(() =>
      registerViewSlot(v.kind, pluginId, { label: v.label, component: v.component, render: v.render }),
    );
  }
}

function mountLifecycle(ctx: Context, hooks: DomainLifecycleHooks): void {
  ctx.effect(() => registerDomainLifecycle(hooks));
}

function mountWiring(ctx: Context, wire: () => () => void): void {
  ctx.effect(() => wire());
}

function mountVaultEvents(ctx: Context, specs: VaultEventHandlerSpec[]): void {
  for (const spec of specs) {
    ctx.effect(() => subscribeVaultEvent(spec));
  }
}

interface BuiltinDefOptions {
  id: string;
  name: string;
  type: string;
  tagline: string;
  order: number;
  views: BuiltinViewPayload[];
  lifecycle?: DomainLifecycleHooks;
  /** 能力提供者接线（如 canvas/table 命名空间数据源 + 变更事件；返回 unregister）。 */
  capability?: () => () => void;
  /** 协作域接线（返回 unregister）。 */
  collabWiring?: () => () => void;
  vaultEventHandlers?: VaultEventHandlerSpec[];
  /** 提供类型化 ctx 服务（须在 capability 接线之后执行——服务构造读取已接线的访问）。 */
  provideService?: (ctx: Context) => void;
}

/** 由选项生成第一方插件定义（apply = 装配所有载荷；卸载 = fiber dispose 全撤销）。 */
function def(opts: BuiltinDefOptions): CordisBuiltinDef {
  const apply = (ctx: Context): void => {
    mountViews(ctx, opts.id, opts.views);
    if (opts.lifecycle) mountLifecycle(ctx, opts.lifecycle);
    if (opts.capability) mountWiring(ctx, opts.capability);
    if (opts.collabWiring) mountWiring(ctx, opts.collabWiring);
    if (opts.vaultEventHandlers) mountVaultEvents(ctx, opts.vaultEventHandlers);
    opts.provideService?.(ctx);
  };
  return { id: opts.id, name: opts.name, type: opts.type, tagline: opts.tagline, order: opts.order, views: opts.views, apply };
}

/** 第一方插件定义总表（id 全局唯一；views 内 kind 全局唯一）。 */
export const CORDIS_BUILTIN_DEFS: CordisBuiltinDef[] = [
  def({
    id: "builtin.search",
    name: "搜索",
    type: "panel",
    tagline: "全文搜索仓库文件",
    order: 1,
    views: [{ kind: "search", label: VIEW_LABELS.search, component: SearchView }],
  }),
  def({
    id: "builtin.recent",
    name: "最近打开",
    type: "panel",
    tagline: "最近打开的文件列表",
    order: 2,
    views: [{ kind: "recent", label: VIEW_LABELS.recent, component: RecentPanel }],
  }),
  def({
    id: "builtin.calendar",
    name: "日历",
    type: "panel",
    tagline: "活动密度与手动日程",
    order: 3,
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
  }),
  def({
    id: "builtin.aichat",
    name: "AI 对话",
    type: "panel",
    tagline: "AI 对话会话面板",
    order: 4,
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
  }),
  def({
    id: "builtin.canvas",
    name: "画布",
    type: "panel",
    tagline: "有向图对话画布",
    order: 5,
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
    provideService: (ctx) => ctx.provide("canvas", createCanvasService()),
  }),
  def({
    id: "builtin.note",
    name: "笔记",
    type: "panel",
    tagline: "Markdown 笔记编辑器",
    order: 6,
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
  }),
  def({
    id: "builtin.table",
    name: "表格",
    type: "panel",
    tagline: "多维表格编辑器",
    order: 7,
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
    provideService: (ctx) => ctx.provide("table", createTableService()),
  }),
  def({
    id: "builtin.files",
    name: "文件",
    type: "panel",
    tagline: "仓库文件树面板",
    order: 8,
    views: [{ kind: "files", label: VIEW_LABELS.files, component: FilesView }],
  }),
  def({
    id: "builtin.inspector",
    name: "属性",
    type: "panel",
    tagline: "节点/笔记属性面板",
    order: 9,
    views: [{ kind: "inspector", label: VIEW_LABELS.inspector, component: InspectorPanel }],
  }),
  def({
    id: "builtin.collabroom",
    name: "协作房间",
    type: "panel",
    tagline: "协作在线用户面板",
    order: 10,
    views: [{ kind: "collabroom", label: VIEW_LABELS.collabroom, component: CollabRoomPanel }],
  }),
  def({
    id: "builtin.repohistory",
    name: "仓库历史",
    type: "panel",
    tagline: "仓库版本历史面板",
    order: 11,
    views: [{ kind: "repohistory", label: VIEW_LABELS.repohistory, component: RepoHistoryPanel }],
  }),
  def({
    id: "builtin.theme",
    name: "默认主题",
    type: "theme",
    tagline: "内置浅色/深色主题与强调色设置",
    order: 12,
    views: [], // 主题类内置插件 = 纯声明式（无视图载荷），只置 active 供主题系统派生消费
  }),
];

/** 第一方插件按 id 索引（装配/挂载用）。 */
export const CORDIS_BUILTIN_BY_ID: Record<string, CordisBuiltinDef> = Object.fromEntries(
  CORDIS_BUILTIN_DEFS.map((d) => [d.id, d]),
);

/** 第一方装配 profile（默认装配权威：存在/顺序/默认启用；enabled 运行时真相在插件状态持久化）。 */
export const CORDIS_BUILTIN_PROFILE: Profile = {
  name: "default",
  plugins: CORDIS_BUILTIN_DEFS.map((d) => ({ id: d.id, order: d.order, defaultEnabled: true })),
};

/** 内置主题插件合成清单：浅色/深色基底（空变量 = 基础方案；与 Rust builtin_manifest 同构）。 */
const BUILTIN_THEME_MANIFEST: Pick<PluginManifest, "themes" | "themeOptions"> = {
  themes: [
    { id: "light", name: "浅色", colorScheme: "light", variables: {} },
    { id: "dark", name: "深色", colorScheme: "dark", variables: {} },
  ],
  themeOptions: { accent: true },
};

/** 内置插件合成清单（组合 UI 默认值层消费；与 Rust builtin_manifest 同字段契约）。 */
export function builtinManifest(def: CordisBuiltinDef, version: string): PluginManifest {
  const base: PluginManifest = {
    schemaVersion: 2,
    id: def.id,
    name: def.name,
    version: version || "0.0.0",
    type: def.type as PluginType,
    scope: "app",
    runtime: "js",
    main: "builtin",
    tagline: def.tagline,
    author: "Atelyx",
    license: "MIT",
  };
  if (def.id === "builtin.theme") {
    return { ...base, themes: BUILTIN_THEME_MANIFEST.themes as ThemeDefinition[], themeOptions: BUILTIN_THEME_MANIFEST.themeOptions };
  }
  return base;
}
