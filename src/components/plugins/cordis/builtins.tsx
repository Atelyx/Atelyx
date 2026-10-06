/**
 * 随应用分发的插件：实现注册表 + 默认组合定义（分发属性，非类别）。
 *
 * 这些插件与用户安装的插件同注册表/同生命周期/同 slots·services·events/同审计，无特权；
 * 唯一差别是**实现解析方式**——实现随宿主编译（本模块 id → Cordis 插件定义的映射），
 * 而非从磁盘读入口。pluginStore 按组合行启停状态经 loader 挂载/卸载（fiber 生命周期）：
 * - 视图贡献 → view/<kind> 槽（single；重型视图 render(hostId) 承载宿主面板 id）；
 * - 领域生命周期钩子（flush/切仓库/释放视图）经 kernelLifecycle 注册，随 fiber 撤销；
 * - 能力提供者 / 协作域接线 / 仓库事件订阅经 ctx.effect 注册（apply 中途抛错/卸载均自动撤销）；
 * - builtin.canvas/table/note/chatcore 额外提供 ctx.canvas/ctx.table/ctx.note/ctx.chat 类型化服务（停用即消失）；
 *   其中 chatcore 是能力行（无视图）：提供 AI 对话运行时，面板行与画布对话节点经注册表消费它。
 *
 * 数组顺序 = 领域生命周期钩子的 flush 注册序（对齐既有注册序）；也是默认组合的装配顺序。
 * 本模块被 pluginStore 静态 import，环内所有跨模块访问均为函数体内延迟求值（无顶层
 * getState/useXxx），新增顶层触碰会 TDZ 崩溃。
 */
import type { ComponentType, ReactNode } from "react";
import type { Context } from "@atelyx/cordis";
import type { PluginManifest, PluginPackageJson } from "@/types";
import type { CompositionDefault } from "@/utils/cordis/composition";
import type { DomainLifecycleHooks } from "@/utils/kernelLifecycle";
import type { VaultEventHandler, VaultEvent, VaultEventOf } from "@/utils/vaultEvents";
import type { SlotCardinality } from "@/utils/cordis/slots";
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
import {
  useCanvasStore,
  registerCanvasCollabWiring,
  registerCanvasPluginWiring,
  syncCanvasDirRefs,
  syncCanvasNodeRefs,
} from "@/stores/canvasStore";
import { useTableStore, registerTableCollabWiring, registerTablePluginWiring } from "@/stores/tableStore";
import { broadcastNoteRelocate, registerNoteCollabWiring, useNoteCollabStore } from "@/stores/noteCollabStore";
import { useChatPanelStore } from "@/stores/chatPanelStore";
import { useCalendarStore, registerCalendarCollabWiring } from "@/stores/calendarStore";
import { useSettingsStore } from "@/stores/settingsStore";
import { useUiStateStore } from "@/stores/uiStateStore";
import { useNoteUndoStore } from "@/stores/noteUndoStore";
import { useNoteStore } from "@/stores/noteStore";
import { closeAllNoteSessions, followRemoteNoteDirRename, noteSurfaceProvider, openNoteSessionFiles } from "@/stores/noteSessionStore";
import { registerCollabPresenceProvider, registerCollabRenamed } from "@/stores/collabStore";
import { registerDomainLifecycle } from "@/utils/kernelLifecycle";
import { registerNoteSurface } from "@/utils/noteSurfaceHost";
import { subscribeVaultEvent } from "@/utils/vaultEvents";
import { registerViewSlot, registerUiSlot } from "@/services/cordis/slots";
import { pluginIdOf } from "@/services/cordis/loader";
import { registerServiceProvider } from "@/services/cordis/services";
import { createCanvasService } from "@/services/cordis/canvas";
import { createTableService } from "@/services/cordis/table";
import { createNoteService } from "@/services/cordis/note";
import { createChatRuntime } from "@/stores/chatTurn";
import { setPluginNoteAccess, setPluginChatPanelAccess } from "@/services/cordis/access";
import { registerChatRuntime } from "@/utils/chatRuntimeHost";
import { VIEW_LABELS } from "@/constants/views";
import { BUILTIN_COMMAND_SHORTCUTS } from "@/constants/commandShortcuts";
import { getOpenNoteSession } from "@/hooks/useNoteBodySession";
import { AURORA_THEME_ID, AURORA_THEME_NAME, AURORA_THEME_VARIABLES } from "@/constants/themes";
import { CanvasEmptyState, NoteEmptyState, TableEmptyState } from "@/components/plugins/cordis/emptyStates";

/** 单个视图载荷（无 props 契约的视图组件；重型视图用 render 承载宿主面板 id）。 */
export interface BuiltinViewPayload {
  kind: string;
  label: string;
  /** 无 props 契约的视图组件（用户插件面板同款；重型视图用 render，不填 component）。 */
  component?: ComponentType;
  /** 重型视图按宿主面板/撕裂窗口 id 渲染（画布/表格需 panelId 聚焦门控）。 */
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

/** 主题类行的声明式载荷（清单里的 `themes` + `themeOptions`）。 */
export interface BuiltinThemePayload {
  themes: PluginManifest["themes"];
  themeOptions: PluginManifest["themeOptions"];
}

/** 单个随应用分发的插件定义（数组顺序 = 装配顺序 = 领域生命周期 flush 注册序）。 */
export interface CordisBuiltinDef {
  id: string;
  name: string;
  type: string;
  tagline: string;
  /** 视图载荷（kind + 标签；apply 据此注册槽，管理页按 kind 反查提供行）。 */
  views: BuiltinViewPayload[];
  /** 主题类行的声明（随清单交给主题系统消费；仅纯声明式主题行填）。 */
  theme?: BuiltinThemePayload;
  /** 挂载实现（pluginStore 经 loader 调 ctx.plugin(apply)）。 */
  apply(ctx: Context): void;
}

// ===== 挂载装配辅助（全部经 ctx.effect，生命周期随 fiber） =====

function mountViews(ctx: Context, views: BuiltinViewPayload[]): void {
  // 槽归属按挂载上下文推导（= 组合行 id）：行实现来源被替换时，贡献归属仍跟着跑代码的那一行。
  const pluginId = pluginIdOf(ctx) ?? "plugin";
  for (const v of views) {
    ctx.effect(() =>
      registerViewSlot(v.kind, pluginId, { label: v.label, component: v.component, render: v.render }),
    );
  }
}

/** 单条 UI 槽贡献规格（registerUi 的载荷；single 用于替换类槽位如 empty/<kind>）。 */
export interface BuiltinUiPayload {
  slot: string;
  component: ComponentType;
  /** 槽优先级（list 排序 / single 决胜；缺省 0）。 */
  priority?: number;
  /** 基数（缺省 list；替换类槽位如 empty/<viewKind> 传 single）。 */
  cardinality?: SlotCardinality;
}

/** 挂载 UI 槽贡献（与外部插件同注册表/同优先级语义；随 fiber 撤销）。 */
function mountUi(ctx: Context, items: BuiltinUiPayload[]): void {
  const pluginId = pluginIdOf(ctx) ?? "plugin";
  for (const item of items) {
    ctx.effect(() =>
      registerUiSlot(item.slot, pluginId, { component: item.component }, {
        priority: item.priority ?? 0,
        cardinality: item.cardinality,
      }),
    );
  }
}

function mountLifecycle(ctx: Context, hooks: DomainLifecycleHooks): void {
  ctx.effect(() => registerDomainLifecycle(hooks));
}

/** 挂载内置命令（ctx.slots.registerCommand 语义）：label/默认键/作用域取 constants/commandShortcuts
 *  清单（唯一事实源），run 由各插件行按 id 提供——双向不对齐（清单缺 run / runs 多余 id）
 *  均属装配错误，抛错可见。 */
function mountCommands(ctx: Context, runs: Record<string, () => unknown>): void {
  const pluginId = pluginIdOf(ctx) ?? "plugin";
  for (const c of BUILTIN_COMMAND_SHORTCUTS.filter((x) => x.pluginId === pluginId)) {
    const run = runs[c.id];
    if (!run) throw new Error(`内置命令 ${pluginId}:${c.id} 缺少 run 实现`);
    ctx.effect(() =>
      ctx.slots.registerCommand({ id: c.id, label: c.label, run, shortcut: c.shortcut, scope: c.scope }),
    );
  }
  for (const id of Object.keys(runs)) {
    if (!BUILTIN_COMMAND_SHORTCUTS.some((x) => x.pluginId === pluginId && x.id === id)) {
      throw new Error(`内置命令 ${pluginId}:${id} 不在快捷键清单中（清单与 runs 不对齐）`);
    }
  }
}

function mountWiring(ctx: Context, wire: () => () => void): void {
  ctx.effect(() => wire());
}

/** 提供 root 作用域类型化服务（其它插件可消费）：`ctx.root.provide` 注册到 root fiber
 *  （全局可见），提供者归属经登记表补充（root fiber 无插件归属，services.list 据此标 provider）。
 *  生命周期随调用插件 fiber（ctx.effect），卸载时撤销服务与登记。 */
function provideRootService(ctx: Context, name: string, factory: () => unknown): void {
  ctx.effect(() => {
    // 先构造服务再登记：工厂抛错（如能力未接线）时不留登记残留，effect 失败随 fiber 清空。
    const service = factory();
    const unregister = registerServiceProvider(name, pluginIdOf(ctx) ?? "plugin");
    const dispose = ctx.root.provide(name, service);
    return () => {
      unregister();
      return dispose();
    };
  });
}

function mountVaultEvents(ctx: Context, specs: VaultEventHandlerSpec[]): void {
  for (const spec of specs) {
    ctx.effect(() => subscribeVaultEvent(spec));
  }
}

/** 笔记能力接线（builtin.note 的 capability）：把当前打开的笔记 + 编辑器读写注入 ctx.note 数据源；
 *  返回 unregister（停用/卸载复位访问）。 */
function wireNoteAccess(): () => void {
  setPluginNoteAccess({
    currentFile: () => useAppStore.getState().currentNoteFile,
    open: (file, title) => useAppStore.getState().openNote(file, title),
    read: async (file) => {
      const target = file ?? useAppStore.getState().currentNoteFile;
      if (!target) throw new Error("未打开笔记");
      return useNoteStore.getState().readNoteContent(target);
    },
    write: async (content) => {
      const target = useAppStore.getState().currentNoteFile;
      if (!target) throw new Error("未打开笔记");
      const result = await useNoteStore.getState().saveNoteContent(target, content);
      // 保存被其它插件 veto：内容未落盘，如实抛错让调用插件感知（静默当成功会让调用方以为内容已写入）
      if (!result.written) throw new Error("笔记保存被插件拒绝");
    },
    save: async () => {
      await useNoteStore.getState().flushPendingNotes();
    },
  });
  return () => setPluginNoteAccess(null);
}

/** 笔记正文编辑能力接线（builtin.note 的 capability）：注册会话提供者，笔记面板与画布文本节点经
 *  `utils/noteSurfaceHost` 取同一篇的会话；presence 上报本端已打开编辑面的笔记（画布节点上编辑笔记时
 *  聚焦文件是画布，对端据此仍能看到「谁在这篇笔记上」）。
 *  返回 unregister（停用/卸载先落盘挂起输入再注销）。 */
function wireNoteSurface(): () => void {
  const off = registerNoteSurface(noteSurfaceProvider);
  const offPresence = registerCollabPresenceProvider((base) => {
    const editingNotes = openNoteSessionFiles();
    return editingNotes.length ? { ...base, editingNotes } : base;
  });
  return () => {
    offPresence();
    closeAllNoteSessions();
    off();
  };
}

/**
 * AI 对话核心接线（builtin.chatcore 的 capability）：把对话运行时（一轮对话编排 + 压缩 + 命名）
 * 注册进注册表——面板会话与画布对话节点经 `utils/chatRuntimeHost` 取用，插件经 ctx.chat 取用。
 * 返回 unregister（停用/卸载复位注册表，消费方随之降级为「能力未启用」）。
 */
function wireChatRuntime(): () => void {
  return registerChatRuntime(createChatRuntime());
}

/** 对话面板容器接线（builtin.chatpanel 的 capability）：把面板会话同源读写注入 ctx.chat
 *  容器方法数据源（校验、转换与落盘调度在面板 store 内）。返回 unregister
 *  （停用/卸载复位访问，容器方法抛「未就绪」）。 */
function wireChatPanelAccess(): () => void {
  setPluginChatPanelAccess({
    importSession: (messages, opts) => useChatPanelStore.getState().importSession(messages, opts),
    appendMessages: (sessionId, messages) =>
      useChatPanelStore.getState().appendMessages(sessionId, messages),
    listSessions: () => useChatPanelStore.getState().listSessions(),
    readSession: (sessionId) => useChatPanelStore.getState().readSession(sessionId),
    createSession: (opts) => useChatPanelStore.getState().createSession(opts),
    setSessionTitle: (sessionId, title) => useChatPanelStore.getState().setSessionTitle(sessionId, title),
    deleteSession: (sessionId) => useChatPanelStore.getState().deleteSessionExternal(sessionId),
  });
  return () => setPluginChatPanelAccess(null);
}

interface BuiltinDefOptions {
  id: string;
  name: string;
  type: string;
  tagline: string;
  views: BuiltinViewPayload[];
  /** 主题类行的声明（随清单交主题系统消费）。 */
  theme?: BuiltinThemePayload;
  /** UI 槽贡献（工具栏/空态/标题栏等；registerUi 语义）。 */
  ui?: BuiltinUiPayload[];
  /** 命令快捷键 run 实现（id → run；label/默认键/作用域取 constants/commandShortcuts 清单）。 */
  commands?: Record<string, () => unknown>;
  lifecycle?: DomainLifecycleHooks;
  /** 能力提供者接线（如 canvas/table 命名空间数据源 + 变更事件；返回 unregister）。 */
  capability?: () => () => void;
  /** 协作域接线（返回 unregister）。 */
  collabWiring?: () => () => void;
  /** 声明需要协作通道（apply 时经 ctx.collab.acquire 声明，随 fiber 撤销释放；
   *  宿主按活跃声明决定是否为本窗口维持协作连接——与用户插件的声明机制一视同仁）。 */
  needsCollab?: boolean;
  vaultEventHandlers?: VaultEventHandlerSpec[];
  /** 提供类型化 ctx 服务（须在 capability 接线之后执行——服务构造读取已接线的访问）。 */
  provideService?: (ctx: Context) => void;
}

/** 由选项生成插件定义（apply = 装配所有载荷；卸载 = fiber dispose 全撤销）。 */
function def(opts: BuiltinDefOptions): CordisBuiltinDef {
  const apply = (ctx: Context): void => {
    mountViews(ctx, opts.views);
    if (opts.ui) mountUi(ctx, opts.ui);
    if (opts.commands) mountCommands(ctx, opts.commands);
    if (opts.lifecycle) mountLifecycle(ctx, opts.lifecycle);
    if (opts.capability) mountWiring(ctx, opts.capability);
    if (opts.collabWiring) mountWiring(ctx, opts.collabWiring);
    if (opts.needsCollab) mountWiring(ctx, () => ctx.collab.acquire());
    if (opts.vaultEventHandlers) mountVaultEvents(ctx, opts.vaultEventHandlers);
    opts.provideService?.(ctx);
  };
  return {
    id: opts.id,
    name: opts.name,
    type: opts.type,
    tagline: opts.tagline,
    views: opts.views,
    ...(opts.theme ? { theme: opts.theme } : {}),
    apply,
  };
}

/** 默认主题插件声明的全部主题条目（由该插件行承载，主题下拉按条目逐个列出）：
 *  深浅两基底（空变量 = 基础方案，未覆盖变量落回内置 CSS 双 palette）+ 「极光」皮肤（携带整套变量覆盖）。 */
const BUILTIN_THEME_MANIFEST: BuiltinThemePayload = {
  themes: [
    { id: "light", name: "浅色", colorScheme: "light", variables: {} },
    { id: "dark", name: "深色", colorScheme: "dark", variables: {} },
    { id: AURORA_THEME_ID, name: AURORA_THEME_NAME, colorScheme: "dark", variables: AURORA_THEME_VARIABLES },
  ],
  themeOptions: { accent: true },
};

/** 随应用分发插件定义总表（id 全局唯一；views 内 kind 全局唯一）。 */
export const CORDIS_BUILTIN_DEFS: CordisBuiltinDef[] = [
  def({
    id: "builtin.search",
    name: "搜索",
    type: "panel",
    tagline: "全文搜索仓库文件",
    views: [{ kind: "search", label: VIEW_LABELS.search, component: SearchView }],
  }),
  def({
    id: "builtin.recent",
    name: "最近打开",
    type: "panel",
    tagline: "最近打开的文件列表",
    views: [{ kind: "recent", label: VIEW_LABELS.recent, component: RecentPanel }],
  }),
  def({
    id: "builtin.calendar",
    name: "日历",
    type: "panel",
    tagline: "活动密度与手动日程",
    views: [{ kind: "calendar", label: VIEW_LABELS.calendar, component: CalendarPanel }],
    lifecycle: {
      id: "builtin.calendar",
      flush: async () => {
        await useCalendarStore.getState().flush();
      },
    },
    // 共享日历依赖 meta-changed 帧互见：日历须自行声明协作需求，
    // 否则其余协作插件全停用/未挂载时连接被拆除，订阅静默失同步
    needsCollab: true,
    collabWiring: registerCalendarCollabWiring,
  }),
  def({
    id: "builtin.chatcore",
    name: "AI 对话核心",
    type: "background",
    tagline: "为面板、对话节点与插件提供 AI 对话能力",
    views: [], // 能力行 = 纯声明式（无视图载荷）：注册对话运行时；停用即无运行时（ctx.chat 随之「未就绪」）
    capability: wireChatRuntime,
  }),
  def({
    id: "builtin.chatpanel",
    name: "AI 对话面板",
    type: "panel",
    tagline: "AI 对话会话面板",
    views: [{ kind: "aichat", label: VIEW_LABELS.aichat, component: AiChatView }],
    capability: wireChatPanelAccess,
    lifecycle: {
      id: "builtin.chatpanel",
      flush: async () => {
        // 归属校验在 store 内按身份键做（当前激活身份 ≠ 内存会话所属身份 → 不写）
        await useChatPanelStore.getState().flush();
      },
      onVaultEntered: async () => {
        // 进仓后读盘加载 AI 会话（force：真实切换强制重读，防幂等守卫跳过旧会话）；
        // 目标身份取调用时的激活仓库身份键（空间模式下 root 恒 null，不按 root 判别）
        await useChatPanelStore.getState().load(true);
      },
      onViewGained: (view) => {
        if (view !== "aichat") return;
        // 撕裂出去的 AI 会话视图回归主窗口：重读盘（面板窗口可能已改会话）
        void useChatPanelStore.getState().load();
      },
      releaseView: async (view) => {
        if (view !== "aichat") return;
        await useChatPanelStore.getState().flush();
      },
    },
  }),
  def({
    id: "builtin.canvas",
    name: "画布",
    type: "panel",
    tagline: "有向图对话画布",
    needsCollab: true,
    views: [
      {
        kind: "canvas",
        label: VIEW_LABELS.canvas,
        render: (hostId) => <CanvasView panelId={hostId} />,
      },
    ],
    ui: [{ slot: "empty/canvas", component: CanvasEmptyState, cardinality: "single" }],
    commands: {
      delete: () => useCanvasStore.getState().deleteSelected(),
      copy: () => useCanvasStore.getState().copySelectedNodes(),
      // 粘贴落点依赖画布视口（快捷键路径由 CanvasView 以 canvasCenter 提供）；命令入口无视口上下文
      paste: () => false,
      undo: () => useCanvasStore.getState().undo(),
      redo: () => useCanvasStore.getState().redo(),
      selectAll: () => {
        const st = useCanvasStore.getState();
        if (st.nodes.length === 0) return;
        st.onNodesChange(st.nodes.map((n) => ({ type: "select", id: n.id, selected: true })));
      },
    },
    lifecycle: {
      id: "builtin.canvas",
      flush: async () => {
        await useCanvasStore.getState().flush();
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
      vaultHandler("note:renamed", async (e) => {
        await syncCanvasNodeRefs(e.oldPath, e.newPath, e.newTitle ?? null, "text");
      }),
      vaultHandler("note:moved", async (e) => {
        await syncCanvasNodeRefs(e.oldPath, e.newPath, e.newTitle ?? null, "text");
      }),
      vaultHandler("table:renamed", async (e) => {
        await syncCanvasNodeRefs(e.oldPath, e.newPath, e.newTitle ?? null, "table");
      }),
      vaultHandler("table:moved", async (e) => {
        await syncCanvasNodeRefs(e.oldPath, e.newPath, e.newTitle ?? null, "table");
      }),
      vaultHandler("attachment:renamed", async (e) => {
        await syncCanvasNodeRefs(e.oldPath, e.newPath, e.newTitle ?? null, "media");
      }),
      vaultHandler("attachment:moved", async (e) => {
        await syncCanvasNodeRefs(e.oldPath, e.newPath, e.newTitle ?? null, "media");
      }),
      vaultHandler("folder:renamed", async (e) => {
        await syncCanvasDirRefs(e.oldDir, e.newDir);
      }),
      vaultHandler("folder:moved", async (e) => {
        await syncCanvasDirRefs(e.oldDir, e.newDir);
      }),
      vaultHandler("canvas:renamed", async (e) => {
        try {
          // 磁盘 .atlx 已被重命名：同步当前画布打开路径（防下次保存回写旧路径）
          if (useAppStore.getState().currentCanvasFile === e.oldPath) {
            useCanvasStore.setState({ canvasFile: e.newPath });
          }
        } catch (err) {
          console.error("画布重命名引用同步失败", err);
        }
      }),
      vaultHandler("canvas:moved", async (e) => {
        try {
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
    provideService: (ctx) => provideRootService(ctx, "canvas", () => createCanvasService()),
  }),
  def({
    id: "builtin.note",
    name: "笔记",
    type: "panel",
    tagline: "Markdown 笔记编辑器",
    needsCollab: true,
    views: [{ kind: "note", label: VIEW_LABELS.note, component: NoteView }],
    ui: [{ slot: "empty/note", component: NoteEmptyState, cardinality: "single" }],
    commands: {
      // 撤销/重做作用于当前打开笔记的编辑会话（快捷键路径按焦点编辑面路由，见 useNoteUndoRouting；
      // 命令入口无焦点上下文，取当前打开笔记的会话，未打开时无操作）
      undo: () => {
        const file = useAppStore.getState().currentNoteFile;
        (file ? getOpenNoteSession(file) : null)?.undo();
      },
      redo: () => {
        const file = useAppStore.getState().currentNoteFile;
        (file ? getOpenNoteSession(file) : null)?.redo();
      },
    },
    lifecycle: {
      id: "builtin.note",
      flush: async () => {
        await useNoteStore.getState().flushPendingNotes();
      },
    },
    capability: () => {
      const offAccess = wireNoteAccess();
      const offSurface = wireNoteSurface();
      return () => {
        offSurface();
        offAccess();
      };
    },
    collabWiring: () => {
      // 笔记域协作接线 + 远端改名/移动跟随注册：renamed 帧旧路径为目录前缀时打开中笔记换路
      // （单文件换路由 note-sync 换路帧覆盖，见下方 note:renamed|moved 事件的 broadcastNoteRelocate）
      const offWiring = registerNoteCollabWiring();
      const offRenamed = registerCollabRenamed(followRemoteNoteDirRename);
      return () => {
        offRenamed();
        offWiring();
      };
    },
    vaultEventHandlers: [
      // 软件内 `.md` 写落点信号（AI 文件工具/插件写盘、重建链接改写，见 services/vault/aiFiles.ts）：
      // 作废内容缓存 + bump 外部变更序号，编辑会话据此收敛（无未落盘输入采纳磁盘、有则保留本地输入）
      vaultHandler("note:changed", (e) => {
        useNoteStore.getState().markNoteExternallyEdited(e.path);
        useNoteStore.getState().invalidateNoteCache(e.path);
      }),
      // 路径迁移/消失：撤销栈随路径迁移（撤销历史不因改名丢失、旧键不滞留内存），正文缓存按旧路径作废。
      // 缓存按路径键存，旧路径可被同名新文件复用，不作废会把已改走/已删的正文串给新笔记
      vaultHandler("note:renamed", (e) => {
        // 对端可能正打开同一笔记：广播换路，对端据此把路径身份跟上（未连接时静默丢弃）
        broadcastNoteRelocate(e.oldPath, e.newPath);
        useNoteStore.getState().invalidateNoteCache(e.oldPath);
        // Rust 代写正文的其它笔记（链接改写）无独立变更信号，缓存在此作废
        for (const file of e.rewritten ?? []) useNoteStore.getState().invalidateNoteCache(file);
        useNoteUndoStore.getState().renameFile(e.oldPath, e.newPath);
        // 协作文档以路径为身份：旧路径文档随迁作废（同名新文件不得复用其 CRDT 状态）
        useNoteCollabStore.getState().disposeDoc(e.oldPath);
      }),
      vaultHandler("note:moved", (e) => {
        broadcastNoteRelocate(e.oldPath, e.newPath);
        useNoteStore.getState().invalidateNoteCache(e.oldPath);
        for (const file of e.rewritten ?? []) useNoteStore.getState().invalidateNoteCache(file);
        useNoteUndoStore.getState().renameFile(e.oldPath, e.newPath);
        useNoteCollabStore.getState().disposeDoc(e.oldPath);
      }),
      vaultHandler("note:deleted", (e) => {
        // 文件已删：清撤销栈、挂起输入与正文缓存（挂起输入不清会在下次 flush 时经 writeNote 重建已删文件）
        useNoteUndoStore.getState().clearFile(e.path);
        useNoteStore.getState().setPendingNoteContent(e.path, null);
        useNoteStore.getState().invalidateNoteCache(e.path);
        useNoteCollabStore.getState().disposeDoc(e.path);
      }),
      // 文件夹改名/移动：目录下笔记的正文缓存按新旧前缀作废——旧前缀不再指代这批文件，
      // 新前缀可能复用本会话内已改走/已删目录的路径
      vaultHandler("folder:renamed", (e) => {
        useNoteStore.getState().invalidateNoteCacheUnder(e.oldDir);
        useNoteStore.getState().invalidateNoteCacheUnder(e.newDir);
        // 目录前缀之外也可能有笔记被代写链接（指向该目录下笔记的引用）：按清单逐条作废
        for (const file of e.rewritten ?? []) useNoteStore.getState().invalidateNoteCache(file);
        // 协作文档以路径为身份：旧目录前缀下的文档随迁作废
        useNoteCollabStore.getState().disposeDocsUnder(e.oldDir);
      }),
      vaultHandler("folder:moved", (e) => {
        useNoteStore.getState().invalidateNoteCacheUnder(e.oldDir);
        useNoteStore.getState().invalidateNoteCacheUnder(e.newDir);
        for (const file of e.rewritten ?? []) useNoteStore.getState().invalidateNoteCache(file);
        useNoteCollabStore.getState().disposeDocsUnder(e.oldDir);
      }),
    ],
    provideService: (ctx) => provideRootService(ctx, "note", () => createNoteService()),
  }),
  def({
    id: "builtin.table",
    name: "表格",
    type: "panel",
    tagline: "多维表格编辑器",
    needsCollab: true,
    views: [
      {
        kind: "table",
        label: VIEW_LABELS.table,
        render: (hostId) => <TableView panelId={hostId} />,
      },
    ],
    ui: [{ slot: "empty/table", component: TableEmptyState, cardinality: "single" }],
    commands: {
      undo: () => useTableStore.getState().undo(),
      redo: () => useTableStore.getState().redo(),
      copy: () => useTableStore.getState().copySelection(),
      cut: () => void useTableStore.getState().cutSelection(),
      paste: () => void useTableStore.getState().pasteFromClipboard(),
      // 清空只处理框选区域（快捷键路径的单元格清空依赖选中格字段类型，见 TableEditor）
      clear: () => {
        const st = useTableStore.getState();
        if (st.selection?.kind === "range") st.clearSelectionCells();
      },
    },
    lifecycle: {
      id: "builtin.table",
      flush: async () => {
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
    provideService: (ctx) => provideRootService(ctx, "table", () => createTableService()),
  }),
  def({
    id: "builtin.files",
    name: "文件",
    type: "panel",
    tagline: "仓库文件树面板",
    views: [{ kind: "files", label: VIEW_LABELS.files, component: FilesView }],
    collabWiring: () =>
      // 远端改名/移动跟随（renamed 帧无 peerId、含发起者回放——本端已迁移，重复迁移为 no-op）：
      // 目录前缀类设置（文件夹颜色/提示词标记/Agent 提示词文件）只迁内存（真源已由发起方写好），
      // 展开集合/上次打开走 uiState 同步（含跨窗口 patch）
      registerCollabRenamed((oldPath, newPath) => {
        useSettingsStore.getState().followRemotePathRename(oldPath, newPath);
        useUiStateStore.getState().renameByDir(oldPath, newPath);
      }),
  }),
  def({
    id: "builtin.inspector",
    name: "属性",
    type: "panel",
    tagline: "节点/笔记属性面板",
    views: [{ kind: "inspector", label: VIEW_LABELS.inspector, component: InspectorPanel }],
  }),
  def({
    id: "builtin.collabroom",
    name: "协作房间",
    type: "panel",
    tagline: "协作在线用户面板",
    needsCollab: true,
    views: [{ kind: "collabroom", label: VIEW_LABELS.collabroom, component: CollabRoomPanel }],
  }),
  def({
    id: "builtin.repohistory",
    name: "仓库历史",
    type: "panel",
    tagline: "仓库版本历史面板",
    views: [{ kind: "repohistory", label: VIEW_LABELS.repohistory, component: RepoHistoryPanel }],
  }),
  def({
    id: "builtin.theme",
    name: "默认主题",
    type: "theme",
    tagline: "内置浅色/深色主题与极光皮肤，含强调色设置",
    views: [], // 主题类行 = 纯声明式（无视图载荷），只置 active 供主题系统派生消费
    theme: BUILTIN_THEME_MANIFEST,
  }),
];

/** 随应用分发的插件按 id 索引（实现解析用：命中即用编译内置实现，否则读磁盘入口）。 */
export const CORDIS_BUILTIN_BY_ID: Record<string, CordisBuiltinDef> = Object.fromEntries(
  CORDIS_BUILTIN_DEFS.map((d) => [d.id, d]),
);

/** 默认组合层：随应用分发的插件即默认组合的普通行（数组顺序 = 装配顺序，也是领域生命周期 flush 注册序）。
 *  enabled 由插件状态持久化决定；用户可停用或卸载，替换关系由各插件在 apply 里声明（priority/inject）。 */
export const DEFAULT_COMPOSITION: CompositionDefault[] = CORDIS_BUILTIN_DEFS.map((d) => ({
  id: d.id,
  name: d.name,
  tagline: d.tagline,
}));

/** 随应用分发插件的清单（默认组合的唯一权威，随 `plugin_list` 交给 Rust 播种并保存）。
 *  形状 = 插件包原始清单（`package.json`：`name` = 插件 id + `atelyx` 块），与磁盘插件包同一
 *  字段契约——宿主按此形状读写行的显示名/类型/主题声明；行对象经 `validatePluginManifest`
 *  归一化为 `PluginManifest`。
 *  `main` 为占位——这些插件的实现随宿主编译，入口经 id 查实现注册表，不从磁盘读。 */
export function builtinManifest(def: CordisBuiltinDef, version: string): PluginPackageJson {
  const atelyx: Record<string, unknown> = {
    name: def.name,
    type: def.type,
    tagline: def.tagline,
    author: "Atelyx",
    license: "MIT",
  };
  if (def.theme) {
    atelyx.themes = def.theme.themes;
    atelyx.themeOptions = def.theme.themeOptions;
  }
  return { name: def.id, version: version || "0.0.0", main: "builtin", atelyx };
}
