/**
 * 随应用分发的插件：实现注册表 + 默认组合定义（分发属性，非类别）。
 *
 * 这些插件与用户安装的插件同注册表/同生命周期/同 slots·services·events/同审计，无特权；
 * 唯一差别是**实现解析方式**——实现随宿主编译（本模块 id → Cordis 插件定义的映射），
 * 而非从磁盘读入口。pluginStore 按组合行启停状态经 loader 挂载/卸载（fiber 生命周期）：
 * - 视图贡献 → view/<kind> 槽（single；重型视图 render(hostId) 承载宿主面板 id）；
 * - 领域生命周期钩子（flush/切仓库/释放视图）经 kernelLifecycle 注册，随 fiber 撤销；
 * - 能力提供者 / 协作域接线 / 仓库事件订阅经 ctx.effect 注册（apply 中途抛错/卸载均自动撤销）；
 * - builtin.canvas/table/note/chat 额外提供 ctx.canvas/ctx.table/ctx.note/ctx.chat 类型化服务（停用即消失）。
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
  hasCollabPeerOnCanvas,
  registerCanvasCollabWiring,
  registerCanvasPluginWiring,
  syncCanvasDirRefs,
  syncCanvasNodeRefs,
} from "@/stores/canvasStore";
import { useTableStore, registerTableCollabWiring, registerTablePluginWiring } from "@/stores/tableStore";
import { registerNoteCollabWiring, useNoteCollabStore } from "@/stores/noteCollabStore";
import { useChatPanelStore } from "@/stores/chatPanelStore";
import { useCalendarStore } from "@/stores/calendarStore";
import { useNoteUndoStore } from "@/stores/noteUndoStore";
import { useNoteStore } from "@/stores/noteStore";
import { closeAllNoteSessions, noteSurfaceProvider, openNoteSessionFiles } from "@/stores/noteSessionStore";
import { registerCollabPresenceProvider } from "@/stores/collabStore";
import { isPendingFolderRenameOldPath, isPendingRenameOldPath, useVaultStore } from "@/stores/vaultStore";
import { isSelfSaveEcho } from "@/utils/selfSave";
import { isCollabCanvasRenamePath } from "@/utils/canvasCollab";
import { tableToSnapshotText } from "@/utils/table";
import { registerDomainLifecycle } from "@/utils/kernelLifecycle";
import { registerNoteSurface } from "@/utils/noteSurfaceHost";
import { subscribeVaultEvent } from "@/utils/vaultEvents";
import { registerViewSlot } from "@/services/cordis/slots";
import { pluginIdOf } from "@/services/cordis/loader";
import { createCanvasService } from "@/services/cordis/canvas";
import { createTableService } from "@/services/cordis/table";
import { createNoteService } from "@/services/cordis/note";
import { createChatService } from "@/services/cordis/chat";
import { setPluginNoteAccess, setPluginChatAccess } from "@/services/cordis/access";
import { VIEW_LABELS } from "@/constants/views";

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

/** 单个随应用分发的插件定义（数组顺序 = 装配顺序 = 领域生命周期 flush 注册序）。 */
export interface CordisBuiltinDef {
  id: string;
  name: string;
  type: string;
  tagline: string;
  /** 视图载荷（kind + 标签；apply 据此注册槽，管理页按 kind 反查提供行）。 */
  views: BuiltinViewPayload[];
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
      await useNoteStore.getState().saveNoteContent(target, content);
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

/** AI 会话能力接线（builtin.aichat 的 capability）：把会话历史 + 发起/停止注入 ctx.chat 数据源；
 *  返回 unregister（停用/卸载复位访问）。 */
function wireChatAccess(): () => void {
  setPluginChatAccess({
    sessions: () => useChatPanelStore.getState().sessions,
    activeSession: () => {
      const s = useChatPanelStore.getState();
      return s.sessions.find((x) => x.id === s.activeSessionId) ?? null;
    },
    isStreaming: () => useChatPanelStore.getState().streaming,
    openSession: (id) => useChatPanelStore.getState().openSession(id),
    startSession: () => useChatPanelStore.getState().newSession(),
    sendMessage: (content) => useChatPanelStore.getState().send(content),
    stop: () => useChatPanelStore.getState().stop(),
    deleteSession: (id) => useChatPanelStore.getState().deleteSession(id),
  });
  return () => setPluginChatAccess(null);
}

interface BuiltinDefOptions {
  id: string;
  name: string;
  type: string;
  tagline: string;
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

/** 由选项生成插件定义（apply = 装配所有载荷；卸载 = fiber dispose 全撤销）。 */
function def(opts: BuiltinDefOptions): CordisBuiltinDef {
  const apply = (ctx: Context): void => {
    mountViews(ctx, opts.views);
    if (opts.lifecycle) mountLifecycle(ctx, opts.lifecycle);
    if (opts.capability) mountWiring(ctx, opts.capability);
    if (opts.collabWiring) mountWiring(ctx, opts.collabWiring);
    if (opts.vaultEventHandlers) mountVaultEvents(ctx, opts.vaultEventHandlers);
    opts.provideService?.(ctx);
  };
  return { id: opts.id, name: opts.name, type: opts.type, tagline: opts.tagline, views: opts.views, apply };
}

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
      onVaultExit: async (ctx) => {
        // 必须用 ctx.vaultId：分发晚于 store 置空，回读 store 会拿到 null 而被仓库守卫丢弃
        await useChatPanelStore.getState().flush(ctx.vaultId);
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
    capability: wireChatAccess,
    vaultEventHandlers: [
      vaultHandler("chat:changed", (e) => {
        // AI 对话历史（.atelyx/对话历史/*.jsonl|*.meta.json）：外部变更内容比对合并，
        // 新会话/新消息/改名/删除经此实时互见（自写回波由 chatPanelStore 内容比对判别）
        useChatPanelStore.getState().applyExternalChatChange(e.path);
      }),
    ],
    provideService: (ctx) =>
      // 服务 root 作用域提供（其它插件可消费），生命周期随 builtin 插件 fiber（ctx.effect）
      ctx.effect(() => ctx.root.provide("chat", createChatService())),
  }),
  def({
    id: "builtin.canvas",
    name: "画布",
    type: "panel",
    tagline: "有向图对话画布",
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
      vaultHandler("canvas:changed", (e) => {
        const store = useCanvasStore.getState();
        // 按文件路径匹配当前画布（画布任意文件夹存放，路径即磁盘身份）；
        // 文件夹重命名期间旧路径删除事件：canvasFile 尚未 remap（Rust 移动目录可能慢于 300ms debounce），
        // 跳过重读防误触 reloadFromDisk 读已不存在的旧路径
        if (e.path !== store.canvasFile || isPendingFolderRenameOldPath(e.path)) {
          // 非当前画布：外部新建/删除/重命名画布（含协作重命名的旧路径删除事件）→ 刷新列表 + 文件树。
          // 自写回放跳过：画布 CRUD 已在 appStore 内主动刷新两数据源
          if (!isSelfSaveEcho(e.path)) {
            void useAppStore.getState().loadList();
            void useVaultStore.getState().loadFiles();
          }
          return;
        }
        // 自写回波 / 协作重命名回波 / 协作对端在场 → 内容已由本端写盘或广播应用进内存，跳过重载：
        // 对端在场时磁盘合法落后于广播（500ms 防抖落盘 + 300ms watcher 延迟），重载会用陈旧盘回退
        // 已应用内容，且 reloadFromDisk→load 杀进行中 AI 流/清锁/清撤销（画布版闪烁/运行态破坏根因）；
        // 磁盘收敛由下次保存的乐观锁自动三方合并负责（canvasStore.handleSaveConflict）。
        // 真实外部修改（无对端在场）才重载。
        if (isSelfSaveEcho(e.path) || isCollabCanvasRenamePath(e.path) || hasCollabPeerOnCanvas(e.path)) {
          return;
        }
        if (store.dirty) {
          // 本地有未保存改动：自动重载会丢改动，改为冲突提示让用户决策
          useCanvasStore.setState({ conflictPending: true });
        } else {
          // 无未保存改动：安全自动重载磁盘最新内容
          void store.reloadFromDisk();
        }
        // 当前画布内容被外部改写：仅列表行 updatedAt 排序可能变化，刷新列表即可（纯内容写不改文件树）
        void useAppStore.getState().loadList();
      }),
      vaultHandler("note:changed", (e) => {
        if (isPendingRenameOldPath(e.path) || isPendingFolderRenameOldPath(e.path)) return;
        // 画布上引用该笔记的节点：silent 刷新正文（与 NoteEditor 外部感知相互独立）
        void useCanvasStore.getState().refreshTextContent(e.path);
      }),
      vaultHandler("table:changed", (e) => {
        if (isPendingRenameOldPath(e.path) || isPendingFolderRenameOldPath(e.path)) return;
        // 画布上引用该表格的节点：silent 刷新快照。打开表格的自写回波直接用内存内容构建快照
        //（磁盘 == 内存），免再整表读盘。
        const store = useTableStore.getState();
        const snapshot =
          isSelfSaveEcho(e.path) && e.path === store.tableFile
            ? tableToSnapshotText({ fields: store.fields, rows: store.rows })
            : undefined;
        void useCanvasStore.getState().refreshTableContent(e.path, snapshot ? { snapshot } : {});
      }),
      vaultHandler("attachment:changed", (e) => {
        if (isPendingRenameOldPath(e.path) || isPendingFolderRenameOldPath(e.path)) return;
        void useCanvasStore.getState().refreshMediaContent(e.path);
      }),
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
    provideService: (ctx) =>
      // 服务在 root 作用域提供（其它插件可消费），生命周期随 builtin 插件 fiber（ctx.effect）
      ctx.effect(() => ctx.root.provide("canvas", createCanvasService())),
  }),
  def({
    id: "builtin.note",
    name: "笔记",
    type: "panel",
    tagline: "Markdown 笔记编辑器",
    views: [{ kind: "note", label: VIEW_LABELS.note, component: NoteView }],
    lifecycle: {
      id: "builtin.note",
      flush: async () => {
        await useNoteStore.getState().flushPendingNotes();
      },
      onVaultLeaving: () => {
        // 切仓库清空笔记撤销栈与编辑会话（防同路径串文件）；笔记运行时态的清态由 noteStore 自注册承担
        useNoteUndoStore.getState().clearAll();
        closeAllNoteSessions();
        // 协作文档以「仓库内相对路径」为身份：跨仓库同名路径不得复用旧文档（CRDT 状态与基线序号一并清空）
        useNoteCollabStore.getState().clear();
      },
      onVaultExit: async () => {
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
    collabWiring: registerNoteCollabWiring,
    vaultEventHandlers: [
      vaultHandler("note:changed", (e) => {
        if (isPendingRenameOldPath(e.path) || isPendingFolderRenameOldPath(e.path)) return;
        // NoteEditor 感知外部修改：无本地改动实时刷新、有改动提示冲突
        //（markNoteExternallyEdited 始终保留：跨编辑面同步 + 冲突检测必经，不受自写回波影响）
        useNoteStore.getState().markNoteExternallyEdited(e.path);
        // 真实外部修改（非本端自写回波，含画布/AI 写 .md）：作废笔记内容缓存，下次读取走盘
        //（自写回波缓存已由 saveNoteContent 同步，不另行作废防缓存失效后重读盘）
        if (!isSelfSaveEcho(e.path)) useNoteStore.getState().invalidateNoteCache(e.path);
      }),
      // 路径迁移/消失：撤销栈随路径迁移（撤销历史不因改名丢失、旧键不滞留内存），正文缓存按旧路径作废。
      // 缓存按路径键存，旧路径可被同名新文件复用，不作废会把已改走/已删的正文串给新笔记；
      // 删除路径的 watcher 事件可能落在自写抑制窗口内被跳过，故与改名同款显式作废
      vaultHandler("note:renamed", (e) => {
        useNoteStore.getState().invalidateNoteCache(e.oldPath);
        // Rust 代写正文的其它笔记（链接改写）：自写回波被抑制窗口吞掉，缓存须显式作废
        for (const file of e.rewritten ?? []) useNoteStore.getState().invalidateNoteCache(file);
        useNoteUndoStore.getState().renameFile(e.oldPath, e.newPath);
        // 协作文档以路径为身份：旧路径文档随迁作废（同名新文件不得复用其 CRDT 状态）
        useNoteCollabStore.getState().disposeDoc(e.oldPath);
      }),
      vaultHandler("note:moved", (e) => {
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
    provideService: (ctx) =>
      // 服务 root 作用域提供（其它插件可消费），生命周期随 builtin 插件 fiber（ctx.effect）
      ctx.effect(() => ctx.root.provide("note", createNoteService())),
  }),
  def({
    id: "builtin.table",
    name: "表格",
    type: "panel",
    tagline: "多维表格编辑器",
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
      vaultHandler("table:changed", (e) => {
        if (isPendingRenameOldPath(e.path) || isPendingFolderRenameOldPath(e.path)) return;
        // 当前打开的表格：干净 → 读盘内容比对判别（自写回放/已应用的对端写入跳过，真实外部修改静默重载）；
        // 有脏 → 不弹冲突条——防抖保存 ≤500ms 内触发，乐观锁 + 自动三方合并收敛（冲突条仅作兜底）
        void useTableStore.getState().syncFromDiskIfChanged(e.path);
      }),
      vaultHandler("table:deleted", (e) => {
        // 打开的表格文件已被删除：只清内存态（flush 会写回重建已删文件）
        if (useAppStore.getState().currentTableFile === e.path) {
          useTableStore.getState().clear();
        }
      }),
    ],
    provideService: (ctx) =>
      // 服务在 root 作用域提供（其它插件可消费），生命周期随 builtin 插件 fiber（ctx.effect）
      ctx.effect(() => ctx.root.provide("table", createTableService())),
  }),
  def({
    id: "builtin.files",
    name: "文件",
    type: "panel",
    tagline: "仓库文件树面板",
    views: [{ kind: "files", label: VIEW_LABELS.files, component: FilesView }],
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
    tagline: "默认浅色/深色主题与强调色设置",
    views: [], // 主题类行 = 纯声明式（无视图载荷），只置 active 供主题系统派生消费
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

/** 主题插件声明的基础条目：浅色/深色基底（空变量 = 基础方案，未覆盖变量落回内置 CSS 双 palette）。 */
const BUILTIN_THEME_MANIFEST: Pick<PluginManifest, "themes" | "themeOptions"> = {
  themes: [
    { id: "light", name: "浅色", colorScheme: "light", variables: {} },
    { id: "dark", name: "深色", colorScheme: "dark", variables: {} },
  ],
  themeOptions: { accent: true },
};

/** 随应用分发插件的清单（默认组合的唯一权威，随 `plugin_list` 交给 Rust 播种并保存）。
 *  形状 = 插件包原始清单（`package.json`：`name` = 插件 id + `atelyx` 块），与磁盘插件包同一
 *  字段契约——宿主按此形状读写行的显示名/类型/主题声明；行对象经 `validatePluginManifest`
 *  归一化为 `PluginManifest`。
 *  `main` 为占位——这些插件的实现随宿主编译，入口经 id 查实现注册表，不从磁盘读。 */
export function builtinManifest(def: CordisBuiltinDef, version: string): PluginPackageJson {
  const atelyx: Record<string, unknown> = {
    name: def.name,
    type: def.type,
    scope: "app",
    tagline: def.tagline,
    author: "Atelyx",
    license: "MIT",
  };
  if (def.id === "builtin.theme") {
    atelyx.themes = BUILTIN_THEME_MANIFEST.themes;
    atelyx.themeOptions = BUILTIN_THEME_MANIFEST.themeOptions;
  }
  return { name: def.id, version: version || "0.0.0", main: "builtin", atelyx };
}
