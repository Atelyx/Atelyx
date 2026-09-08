/**
 * 插件平台 store：已装插件状态（app + 当前仓库 vault + 内置插件行）+ 运行时生命周期编排。
 *
 * 分层：本 store 是插件相关状态的唯一出口——组件不直连 `services/plugins`；
 * 运行时（Worker/桥）本体在 `services/plugins/bridge`，本 store 只做编排与快照。
 * 加载时机：应用挂载/进仓后 `load()` 一次——先列清单，再逐个拉起启用插件（单个失败不影响其余）。
 * 例外说明：本 store 静态 import `components/plugins/builtinViews.tsx`（内置插件宿主视图载荷，
 * 组件层承载组件引用——services 不 import components 的约束所致）；该边经头注释文档化，
 * 环上跨模块访问均为函数体内延迟求值，无顶层 getState/useXxx（新增顶层触碰会 TDZ 崩溃）。
 */
import { create } from "zustand";
import type { ComponentType } from "react";
import type {
  InstalledPlugin,
  PluginCanvasEdge,
  PluginCanvasNode,
  PluginIndexEntry,
  PluginManifest,
  PluginScope,
  PluginSourceKind,
  ToolDefinition,
} from "@/types";
import { errText, type PluginFiberPhase } from "@/types";
import {
  attachPlugin,
  contributedCommands,
  contributedPluginTools,
  exposePluginFacade,
  getPluginAppPages,
  getPluginCommands,
  getPluginNode,
  getPluginNodes,
  getPluginSetting,
  getPluginSettings,
  getPluginTableView,
  getPluginTableViews,
  getViewContribution,
  hostCapabilityLabel,
  hostCapabilityNames,
  hostCapabilitySensitive,
  loadPlugin,
  loadUiPlugin,
  onPluginUiChange,
  onRuntimeChange,
  pluginInstall,
  pluginInstallLocal,
  pluginList,
  pluginDefaultPlugins,
  pluginReadEntry,
  pluginSeedBuiltin,
  pluginSetEnabled,
  pluginUninstall,
  pluginUpdate,
  pluginViewKinds as allPluginViewKinds,
  pluginViewLabel as pluginViewLabelOf,
  registerBuiltinView,
  runContributedCommand,
  runtimeSnapshot,
  setAppPageOpener,
  setBuiltinPluginIds,
  setPluginCanvasAccess,
  setPluginCollabAccess,
  setPluginTableAccess,
  setPluginTableRuntimeAccess,
  setPluginVaultAccess,
  setPluginVaultWriteAccess,
  setSettingsAccess,
  startPluginProcess,
  transpileTs,
  unloadPlugin,
  unregisterPluginUi,
  emitPluginEvent,
} from "@/services/plugins";
import type {
  PluginAppPageRegistration,
  PluginCommandContribution,
  PluginNodeRegistration,
  PluginRow,
  PluginSettingRegistration,
  PluginTableAccess,
  PluginTableViewRegistration,
  ViewContribution,
} from "@/services/plugins";
import { BUILTIN_VIEWS } from "@/components/plugins/builtinViews";
import { useTableStore } from "@/stores/tableStore";
import { useCanvasStore } from "@/stores/canvasStore";
import { useCollabStore, publishPluginPresence } from "@/stores/collabStore";
import { useVaultStore } from "@/stores/vaultStore";
import { useAppStore } from "@/stores/appStore";
import { useSettingsStore } from "@/stores/settingsStore";
import { pickDirectory as pickDirectorySvc } from "@/services/dialog";
import {
  appendVaultFile,
  editVaultFile,
  writeVaultFile,
} from "@/services/vault/aiFiles";
import { buildPluginTableSnapshot } from "@/utils/table";
import { serializeEdgeForCollab, serializeNodeForCollab } from "@/utils/canvasCollab";
import { resolveTableImageUrl } from "@/services/tableImageCache";
import { pluginToolMetas as pluginToolMetasSvc } from "@/services/ai/tools";
import type { AgentToolMeta } from "@/constants/tools";
import {
  fetchMarketIndex,
  isMarketStale,
  readMarketCache,
} from "@/services/plugins/market";
import { getAppVersion } from "@/services/app";
import {
  isUiPluginType,
  isWorkerPluginType,
  pluginCompatibleWithHost,
  pluginTypeList,
  validatePluginManifest,
} from "@/utils/pluginManifest";
import { resolveEnabledDeps } from "@/utils/pluginDeps";
import { detectPlatform } from "@/utils/pluginHost";

/** 某视图 kind 由内置插件提供时的状态（ViewHost 降级占位/菜单过滤用；installed=false = 已卸载）。 */
interface BuiltinViewState {
  name: string;
  enabled: boolean;
  installed: boolean;
}

interface PluginStoreState {
  /** 已装插件（按 id；运行时阶段/审计与磁盘行合并）。 */
  plugins: Record<string, InstalledPlugin>;
  /** 已初始化（应用挂载/进仓后加载一次）。 */
  initialized: boolean;
  /** UI 注册修订号（主线程插件脚本异步注册到达时自增；依赖插件 UI 的组件据此重渲染）。 */
  uiRevision: number;
  /** 市场索引条目（含徽标合并）。 */
  marketItems: PluginIndexEntry[];
  marketLoading: boolean;
  marketError: string;
  /** 市场是否已加载过（UI 据此显示加载/空态）。 */
  marketLoaded: boolean;
  /** 默认装配（官方默认插件集；组合配置视图的默认值层来源，含已卸载成员）。 */
  compositionDefaults: PluginManifest[];
  /** 加载已装插件并按启用状态拉起运行时。 */
  load(): Promise<void>;
  /** 从 GitHub 仓库安装（repo 为 `owner/repo` 市场引用或完整 git 地址；安装后默认未启用，由管理 UI 确认后启用）。 */
  install(repo: string, scope: PluginScope): Promise<void>;
  /** 从本地目录安装（junction/符号链接实时引用，源目录改动即时生效；当前仅 app 级）。 */
  installLocal(path: string): Promise<void>;
  /** 调系统目录选择器选插件源目录并安装；用户取消 = false（未安装）。 */
  installLocalFromPicker(): Promise<boolean>;
  /** 从 git 地址安装（git clone，保留 .git 供更新；当前仅 app 级）。 */
  installGit(url: string): Promise<void>;
  /** 卸载（删除目录 + 终止运行时 + 清理状态）。 */
  uninstall(id: string): Promise<void>;
  /** 启用/停用（启用 = 拉起运行时；停用 = 终止运行时）。 */
  setEnabled(id: string, enabled: boolean): Promise<void>;
  /** 更新（备份 → 安装 → 失败回滚；成功则重载运行时）。 */
  update(id: string): Promise<void>;
  /** 插件贡献的 AI 工具（Agent 名册组装用）。 */
  pluginTools(): ToolDefinition[];
  /** 插件工具的 UI 元数据（Agent 设置页名册合并；组件经此读取，不直连 services）。 */
  pluginToolMetas(): AgentToolMeta[];
  /** 插件设置项注册（设置页 tab 合并）。 */
  pluginSettings(): PluginSettingRegistration[];
  pluginSetting(key: string): PluginSettingRegistration | undefined;
  /** 插件画布节点注册（CanvasView nodeTypes 合并）。 */
  pluginNode(type: string): PluginNodeRegistration | undefined;
  /** 插件画布节点组件表（nodeTypes 合并用：type → component）。 */
  pluginNodeTypes(): Record<string, ComponentType>;
  /** 插件应用页面注册（app 页面/模式全页接管）。 */
  pluginAppPage(id: string): PluginAppPageRegistration | undefined;
  /** 面板视图候选（内建 + 插件面板）。 */
  pluginViewKinds(): string[];
  /** 视图显示名（含插件面板，未知视图原样兜底）。 */
  pluginViewLabel(view: string): string;
  /** 某视图的贡献（内置 + 插件面板统一注册表；ViewHost 分派用，缺注册 = 空面板占位）。 */
  viewContribution(kind: string): ViewContribution | undefined;
  /** 插件表格视图注册（kind → 注册；TableEditor 视图切换合并）。 */
  pluginTableView(kind: string): PluginTableViewRegistration | undefined;
  /** 全部插件表格视图注册（工具条视图列表合并用）。 */
  pluginTableViews(): PluginTableViewRegistration[];
  /** 插件命令（UI 平面 + worker 平面合并；管理 UI「运行命令」入口）。 */
  pluginCommands(): PluginCommandContribution[];
  /** 执行插件命令（UI 平面直接 run；worker 平面经桥 RPC）。 */
  runPluginCommand(globalId: string): Promise<unknown>;
  /** 能力命名空间展示文案（宿主能力返回注册表标签，插件命名空间原样）。 */
  capabilityLabel(namespace: string): string;
  /** 能力命名空间是否敏感（宿主注册表标记，UI「敏感」高亮）。 */
  capabilitySensitive(namespace: string): boolean;
  /** 加载市场索引（缓存未过期直接回缓存；网络失败直接提示失败）。 */
  loadMarket(force?: boolean): Promise<void>;
  /** 某视图 kind 由内置插件提供时的状态（组件经此查，不直连 services）；非内置提供 = undefined。 */
  viewKindState(kind: string): BuiltinViewState | undefined;
  /** 恢复内置插件（管理 UI「恢复内置插件」入口）：补播种缺失的内置条目后重载插件列表。 */
  restoreBuiltin(): Promise<void>;
}

/** 磁盘行 → store 条目（清单经前端校验归一化；Rust 侧 plugin_list 已滤除损坏清单，
 * 此处回退 cast 仅兜底意外形态——正常路径 validated.ok 恒真）。 */
function toInstalled(row: {
  id: string;
  scope: PluginScope;
  installDir: string;
  sourceKind: PluginSourceKind;
  enabled: boolean;
  manifest: unknown;
}): InstalledPlugin {
  const validated = validatePluginManifest(row.manifest);
  return {
    id: row.id,
    manifest: validated.ok ? validated.manifest : (row.manifest as InstalledPlugin["manifest"]),
    scope: row.scope,
    installDir: row.installDir,
    sourceKind: row.sourceKind,
    enabled: row.enabled,
    phase: "pending",
    usedCapabilities: [],
  };
}

/** 安装后统一收尾（模块私有）：宿主兼容强制 + 重载。 */
async function finishInstall(get: () => PluginStoreState, row: PluginRow): Promise<void> {
  try {
    // 宿主兼容强制（清单承诺）：版本/平台不匹配即回滚并报错。
    // 宿主版本读取失败（瞬时 IPC 异常）按放行处理：不误删刚装好的插件，不兼容风险由运行时兜底。
    let hostVersion: string | null = null;
    try {
      hostVersion = await getAppVersion();
    } catch {
      hostVersion = null;
    }
    if (hostVersion !== null) {
      const compat = pluginCompatibleWithHost(row.manifest, hostVersion, detectPlatform());
      if (!compat.ok) throw new Error(`无法安装：${compat.reason}`);
    }
  } catch (e) {
    // 任一检查失败都回滚已落盘插件，避免「装了一半」留脏。
    await pluginUninstall(row.id, row.scope).catch(() => {});
    throw e;
  }
  await get().load();
}

/** 表格数据访问接线守卫：load 每次进仓/启动都会跑，接线幂等只做一次（订阅常驻、跨仓库不重绑）。
 * 注意模块环：pluginStore ↔ { tableStore, collabStore, appStore, vaultStore }（appStore 反向依赖
 * pluginStore）为良性环——全部 store 访问延迟到回调内 `getState()`，模块顶层零触碰。 */
let tableAccessWired = false;
function ensureTableAccess(): void {
  if (tableAccessWired) return;
  tableAccessWired = true;
  const access: PluginTableAccess = {
    subscribeSnapshot: (cb) => {
      const push = () => {
        const ts = useTableStore.getState();
        cb(
          buildPluginTableSnapshot(
            ts.tableFile,
            ts.fields,
            ts.rows,
            ts.selectedRowId,
            useCollabStore.getState().peers,
          ),
        );
      };
      push();
      // 仅相关切片引用变化才推送（保存/脏标记等高频无关变更不打扰插件）；
      // rows/fields 直传 store 不可变引用，选中变化不重建数组、插件卡片 memo 不受击穿。
      const unsubTable = useTableStore.subscribe((s, prev) => {
        if (
          s.tableFile !== prev.tableFile ||
          s.fields !== prev.fields ||
          s.rows !== prev.rows ||
          s.selectedRowId !== prev.selectedRowId
        ) {
          push();
        }
      });
      // 协作订阅按 peers 整数组比较属粗粒度：presence 帧（onPeerPresence 恒 map 新数组）都会触发一次推送，
      // 含与当前表格无关的笔记/画布选中更新；发送端 100ms 节流 + 局域网少量 peer，频率低，推送成本 ≈ 快照本身，可接受。
      const unsubCollab = useCollabStore.subscribe((s, prev) => {
        if (s.peers !== prev.peers) push();
      });
      return () => {
        unsubTable();
        unsubCollab();
      };
    },
    selectRow: (rowId) => useTableStore.getState().selectRow(rowId),
    resolveImage: resolveTableImageUrl,
  };
  setPluginTableAccess(access);
}

/** 插件侧仓库访问接线守卫：load 每次进仓/启动都会跑，接线幂等只做一次。
 *  把仓库文件树与打开回调暴露给主线程插件（facade 的 listFiles/open* 方法）。
 *  全部 store 访问延迟到回调内 getState()（与 ensureTableAccess 同模式，防模块环顶层触碰）。 */
let vaultAccessWired = false;
function ensureVaultAccess(): void {
  if (vaultAccessWired) return;
  vaultAccessWired = true;
  setPluginVaultAccess({
    listFiles: async () => useVaultStore.getState().tree,
    openCanvasFile: (row) => useAppStore.getState().openCanvas(row),
    openNote: (file, title) => useAppStore.getState().openNote(file, title),
    openTable: (file, title) => useAppStore.getState().openTable(file, title),
  });
}

/** vault 写能力接线守卫：把仓库写方法暴露给 worker 平面 `vault` 命名空间（幂等一次）。
 *  复用 AI 文件工具同一批 service/store 语义（原子写/扩展名分发引用维护/树刷新）；
 *  直接 service 的写方法不登记 .md 磁盘基线（按外部写入处理，与 AI 写入一致）；
 *  rename/move/delete/deleteDir/createFolder 走 vaultStore（扩展名分发 + loadFiles 刷新）。 */
let vaultWriteWired = false;
function ensureVaultWriteAccess(): void {
  if (vaultWriteWired) return;
  vaultWriteWired = true;
  setPluginVaultWriteAccess({
    writeFile: async (file, content) => {
      await writeVaultFile(file, content);
      return { ok: true, summary: `已写入「${file}」` };
    },
    editFile: (file, edits) => editVaultFile(file, edits),
    appendFile: (file, content) => appendVaultFile(file, content),
    renameFile: (oldPath, newName) => useVaultStore.getState().renameFile(oldPath, newName),
    moveFile: (oldPath, targetDir) => useVaultStore.getState().moveFile(oldPath, targetDir),
    deleteFile: (path) => useVaultStore.getState().deleteFile(path),
    deleteDir: async (dir, force) => {
      const r = await useVaultStore.getState().deleteFolder(dir, force);
      return {
        ok: r.deleted,
        summary: r.deleted
          ? `已删除目录「${dir}」`
          : r.needsConfirm
            ? `目录非空（${r.itemCount} 项），需确认后删除`
            : "删除目录失败",
        needsConfirm: r.needsConfirm,
        itemCount: r.itemCount,
      };
    },
    createFolder: async (dir) => {
      const path = await useVaultStore.getState().createFolder(dir);
      return { ok: true, summary: `已创建「${path}」`, path };
    },
  });
}

/** 表格能力接线守卫：把当前表格数据与写操作暴露给 worker 平面 `table` 命名空间（幂等一次）。
 *  快照复用表数据订阅的同一构造（buildPluginTableSnapshot），写操作直连 tableStore 动作。 */
let tableRuntimeWired = false;
function ensureTableRuntimeAccess(): void {
  if (tableRuntimeWired) return;
  tableRuntimeWired = true;
  setPluginTableRuntimeAccess({
    snapshot: () =>
      buildPluginTableSnapshot(
        useTableStore.getState().tableFile,
        useTableStore.getState().fields,
        useTableStore.getState().rows,
        useTableStore.getState().selectedRowId,
        useCollabStore.getState().peers,
      ),
    updateCell: (rowId, fieldId, value) => useTableStore.getState().updateCell(rowId, fieldId, value),
    addRow: () => useTableStore.getState().addRow(),
    removeRow: (rowId) => useTableStore.getState().removeRow(rowId),
    selectRow: (rowId) => useTableStore.getState().selectRow(rowId),
  });
}

/** 协作能力接线守卫：把在线用户与 presence 上报暴露给 worker 平面 `collab` 命名空间（幂等一次）。 */
let collabRuntimeWired = false;
function ensureCollabRuntimeAccess(): void {
  if (collabRuntimeWired) return;
  collabRuntimeWired = true;
  setPluginCollabAccess({
    peers: () => useCollabStore.getState().peers,
    setPresence: (view, file) => publishPluginPresence(view, file),
  });
}

/** 画布能力接线守卫：把当前画布快照与写操作暴露给 worker 平面 `canvas` 命名空间（幂等一次）。
 *  投影复用协作/磁盘序列化纯函数（serializeNodeForCollab 内嵌对话消息、剥离 React Flow 视图态，JSON 安全）；
 *  写方法守卫「已打开可写画布」；全部 store 访问延迟到回调内 getState()（防模块环）。 */
let canvasRuntimeWired = false;
function ensureCanvasRuntimeAccess(): void {
  if (canvasRuntimeWired) return;
  canvasRuntimeWired = true;
  const requireWritable = (): void => {
    const s = useCanvasStore.getState();
    if (!s.canvasFile) throw new Error("未打开画布");
    if (s.readOnly) throw new Error("画布为只读");
  };
  setPluginCanvasAccess({
    snapshot: () => {
      const s = useCanvasStore.getState();
      const nodes: PluginCanvasNode[] = s.nodes.map((n) => {
        const ser = serializeNodeForCollab(n, s.messagesByConv);
        return {
          id: ser.id,
          type: ser.type,
          x: ser.x,
          y: ser.y,
          width: ser.width,
          height: ser.height,
          data: ser.data as unknown as Record<string, unknown>,
        };
      });
      const edges: PluginCanvasEdge[] = s.edges.map((e) => {
        const ser = serializeEdgeForCollab(e);
        return {
          id: ser.id,
          source: ser.source,
          target: ser.target,
          sourceHandle: ser.sourceHandle,
          targetHandle: ser.targetHandle,
          directed: ser.directed,
          linkMode: ser.linkMode,
        };
      });
      return {
        canvasFile: s.canvasFile,
        canvasTitle: s.canvasTitle,
        nodes,
        edges,
        selectedNodeId: s.selectedNodeId,
      };
    },
    addNode: (node) => {
      requireWritable();
      const id = crypto.randomUUID();
      useCanvasStore.getState().addNode({ id, type: node.type, position: node.position, data: node.data ?? {} });
      return id;
    },
    updateNode: (nodeId, patch) => {
      requireWritable();
      useCanvasStore.getState().updateNodeData(nodeId, patch);
    },
    moveNode: (nodeId, position) => {
      requireWritable();
      const st = useCanvasStore.getState();
      // position 变更经 onNodesChange 应用（自身不入 undo 栈），显式 pushUndo 使其可撤销
      st.pushUndo();
      st.onNodesChange([{ type: "position", id: nodeId, position, dragging: false }]);
    },
    deleteNode: (nodeId) => {
      requireWritable();
      useCanvasStore.getState().deleteNodes([nodeId]);
    },
    addEdge: (edge) => {
      requireWritable();
      const st = useCanvasStore.getState();
      const id = crypto.randomUUID();
      st.addEdge({ id, ...edge });
      return id;
    },
    deleteEdge: (edgeId) => {
      requireWritable();
      const st = useCanvasStore.getState();
      st.pushUndo();
      st.onEdgesChange([{ type: "remove", id: edgeId }]);
    },
    selectNode: (nodeId) => {
      useCanvasStore.getState().selectNode(nodeId);
    },
  });
}

/** AI 配置接线守卫：把供应商/模型/Agent 与默认目标解析暴露给 worker 平面 `ai` 命名空间（幂等一次）。
 *  providers 为运行时配置（apiKey 已由 settingsStore 填充——key 读取不进本层）。 */
let settingsAccessWired = false;
function ensureSettingsAccess(): void {
  if (settingsAccessWired) return;
  settingsAccessWired = true;
  setSettingsAccess(() => {
    const s = useSettingsStore.getState();
    return {
      providers: s.config.providers,
      agents: s.agents,
      resolveChatTarget: (sel) => s.resolveChatTarget(sel),
    };
  });
}

/** 能力变更事件接线守卫：store 变更 → emitPluginEvent 通知 worker 平面订阅插件（幂等一次）。
 *  载荷为轻量信号（canvas/table 只带 file，插件按需再调 snapshot() 取数据）——
 *  画布拖拽/流式是每帧高频变更，全量序列化快照会造成事件风暴与陈旧大载荷。 */
let runtimeEventsWired = false;
function ensureRuntimeChangeEvents(): void {
  if (runtimeEventsWired) return;
  runtimeEventsWired = true;
  useTableStore.subscribe((s, prev) => {
    if (
      s.tableFile !== prev.tableFile ||
      s.fields !== prev.fields ||
      s.rows !== prev.rows ||
      s.selectedRowId !== prev.selectedRowId
    ) {
      emitPluginEvent("table:changed", { file: s.tableFile });
    }
  });
  useCollabStore.subscribe((s, prev) => {
    if (s.peers !== prev.peers) emitPluginEvent("collab:changed", { peers: s.peers });
  });
  useCanvasStore.subscribe((s, prev) => {
    if (
      s.canvasFile !== prev.canvasFile ||
      s.canvasTitle !== prev.canvasTitle ||
      s.nodes !== prev.nodes ||
      s.edges !== prev.edges ||
      s.messagesByConv !== prev.messagesByConv ||
      s.selectedNodeId !== prev.selectedNodeId
    ) {
      emitPluginEvent("canvas:changed", { file: s.canvasFile });
    }
  });
  useVaultStore.subscribe((s, prev) => {
    if (s.tree !== prev.tree) emitPluginEvent("vault:changed", {});
  });
}

export const usePluginStore = create<PluginStoreState>()((set, get) => {
  /** 把运行时快照合并回 store（加载/激活/失败/卸载事件驱动）。 */
  const reconcile = (): void => {
    const entries = runtimeSnapshot();
    set((s) => {
      let changed = false;
      const plugins = { ...s.plugins };
      for (const e of entries) {
        const p = plugins[e.id];
        if (!p) continue;
        plugins[e.id] = { ...p, phase: e.phase, usedCapabilities: e.used, error: e.error };
        changed = true;
      }
      return changed ? { plugins } : s;
    });
  };

  /** 读取插件入口源码；runtime 为 ts 且入口为 .ts/.tsx 时先转译成 JS（发布源码即可用，
   *  无需构建产物；dist/ 预编译 .js 跳过）。 */
  const readEntry = async (p: InstalledPlugin, path: string): Promise<string> => {
    const code = await pluginReadEntry(p.id, path);
    if (p.manifest.runtime === "ts" && /\.tsx?$/i.test(path)) {
      return transpileTs(code);
    }
    return code;
  };

  /** 拉起单个启用插件的运行时（按平面：主线程 UI 入口 + 逻辑平面；失败标 failed 不阻塞）。 */
  const spawn = async (id: string): Promise<void> => {
    const p = get().plugins[id];
    if (!p) return;
    // 先撤销旧贡献（重载防重复注册）。
    unregisterPluginUi(id);
    unloadPlugin(id);
    try {
      // 内置插件：实现随宿主编译（无入口文件/无桥运行时），启用 = 注册宿主视图贡献。
      if (p.sourceKind === "builtin") {
        let registered = 0;
        for (const v of BUILTIN_VIEWS) {
          if (v.pluginId === id) {
            registerBuiltinView(id, { kind: v.kind, label: v.label, component: v.component });
            registered++;
          }
        }
        if (registered === 0) {
          // 内置清单（Rust）与前端载荷（builtinViews.tsx）不同步会静默无贡献：显式告警便于排查。
          console.warn(`内置插件 ${id} 无对应视图载荷，未注册任何视图`);
        }
        set((s) => {
          const cur = s.plugins[id];
          if (!cur) return s;
          return { plugins: { ...s.plugins, [id]: { ...cur, phase: "active" } } };
        });
        return;
      }
      const types = pluginTypeList(p.manifest);
      const hasWorker = types.some(isWorkerPluginType);
      // 主线程平面：mainUi 优先；无 mainUi 时仅当「js/ts 运行时 + 无 worker 平面」才把 main 当
      // UI 入口——python 的 main 是子进程入口，注入主线程会静默失败（UI 平面永远跑 JS）。
      const uiEntry =
        p.manifest.mainUi ??
        ((p.manifest.runtime === "js" || p.manifest.runtime === "ts") &&
        !hasWorker &&
        types.some(isUiPluginType)
          ? p.manifest.main
          : undefined);
      const uiLoad: Promise<void> = uiEntry
        ? readEntry(p, uiEntry).then((code) => loadUiPlugin(id, code))
        : Promise.resolve();
      // UI 平面错误统一收口（镜像为永不 reject，防 worker 分支抛错时未捕获拒绝）。
      const uiError = uiLoad.then(
        () => undefined,
        (e: unknown) => errText(e),
      );
      // 逻辑平面：工具/后台/命令逻辑。
      const syncPhase = (phase: PluginFiberPhase, error?: string): void => {
        set((s) => {
          const cur = s.plugins[id];
          if (!cur) return s;
          return { plugins: { ...s.plugins, [id]: { ...cur, phase, error } } };
        });
      };
      if (hasWorker && p.manifest.main) {
        // 子进程运行时（Python）：spawn 解释器经 stdio 桥接入同一套能力注册表。
        if (p.manifest.runtime === "python") {
          const transport = await startPluginProcess(id, p.manifest.runtime);
          const entry = attachPlugin(p.manifest, transport);
          syncPhase(entry.phase, entry.error);
        } else {
          const code = await readEntry(p, p.manifest.main);
          const entry = loadPlugin(p.manifest, code);
          syncPhase(entry.phase, entry.error);
        }
        // 双平面插件 UI 脚本失败 = 整插件故障：卸载运行时 + 撤销贡献 + 标 failed
        // （与「worker 工具仍可用但标 failed」的状态矛盾相比，卸载是自洽的一致态）。
        void uiError.then((err) => {
          if (!err) return;
          unloadPlugin(id);
          unregisterPluginUi(id);
          syncPhase("failed", err);
        });
      } else {
        // 纯 UI 平面：等脚本加载+执行完成再置 active；失败 → failed（脚本错误不再静默）。
        const err = await uiError;
        if (err) throw new Error(err);
        syncPhase("active");
      }
    } catch (e) {
      set((s) => {
        const cur = s.plugins[id];
        if (!cur) return s;
        return { plugins: { ...s.plugins, [id]: { ...cur, phase: "failed", error: errText(e) } } };
      });
    }
  };

  onRuntimeChange(reconcile);
  onPluginUiChange(() => set((s) => ({ uiRevision: s.uiRevision + 1 })));

  /** load 序号守卫：并发 load（回启动页 fire-and-forget 与紧接着进仓 load 竞态）时
   * 只允许最后一次生效，防止旧 load 覆盖插件表后残留孤儿 runtime。 */
  let loadSeq = 0;

  return {
    plugins: {},
    initialized: false,
    uiRevision: 0,
    marketItems: [],
    marketLoading: false,
    marketError: "",
    marketLoaded: false,
    compositionDefaults: [],

    /**
     * 全量重载：先取磁盘清单（失败则旧状态原样保留），再卸载旧运行时与 UI 贡献，按当前上下文
     * （app 插件 + 当前仓库 vault 插件 + 内置插件行）重建。语义 =「重置到磁盘状态」，可在
     * boot / 切仓库 / 安装/更新后安全重复调用。
     */
    load: async () => {
      exposePluginFacade();
      setAppPageOpener((pageId) => useAppStore.getState().openPluginPage(pageId));
      ensureTableAccess();
      ensureVaultAccess();
      ensureVaultWriteAccess();
      ensureTableRuntimeAccess();
      ensureCollabRuntimeAccess();
      ensureCanvasRuntimeAccess();
      ensureSettingsAccess();
      ensureRuntimeChangeEvents();
      const seq = ++loadSeq;
      const [rows, defaults] = await Promise.all([pluginList(), pluginDefaultPlugins()]);
      if (seq !== loadSeq) return; // 已有更新的 load 开始，本次作废（防孤儿 runtime）
      // 内置插件 id 集合注入注册表（封闭 ViewKind 的防劫持放行依据）；须先于任何视图注册。
      setBuiltinPluginIds(new Set(rows.filter((r) => r.sourceKind === "builtin").map((r) => r.id)));
      for (const id of Object.keys(get().plugins)) {
        unloadPlugin(id);
        unregisterPluginUi(id);
      }
      const plugins: Record<string, InstalledPlugin> = {};
      for (const row of rows) {
        plugins[row.id] = toInstalled(row);
      }
      set({ plugins, compositionDefaults: defaults, initialized: true });
      // 面板/菜单只订阅 uiRevision：插件行落定后补发一次，让已渲染的降级占位/菜单按新状态收敛
      // （全停用内置的冷启动无任何注册 notify）。
      set((s) => ({ uiRevision: s.uiRevision + 1 }));
      // 真依赖校验（requires 启动前）：缺失/成环的启用插件拒绝拉起，附可读原因；
      // 可启动插件按依赖解析顺序拉起（提供者先于依赖者）。
      const depResult = resolveEnabledDeps(
        rows.filter((r) => r.enabled).map((r) => ({ id: r.id, provides: r.manifest.provides, requires: r.manifest.requires })),
        hostCapabilityNames(),
      );
      if (depResult.failed.length > 0) {
        set((s) => {
          const next = { ...s.plugins };
          for (const f of depResult.failed) {
            const cur = next[f.id];
            if (!cur || !cur.enabled) continue;
            next[f.id] = { ...cur, phase: "failed", error: f.reason };
          }
          return { plugins: next };
        });
      }
      for (const id of depResult.spawnable) {
        if (seq !== loadSeq) return;
        await spawn(id);
      }
    },

    install: async (repo, scope) => {
      const row = await pluginInstall(repo, scope);
      await finishInstall(get, row);
    },

    installLocal: async (path) => {
      const row = await pluginInstallLocal(path, "app");
      await finishInstall(get, row);
    },

    installLocalFromPicker: async () => {
      const path = await pickDirectorySvc();
      if (!path) return false;
      await get().installLocal(path);
      return true;
    },

    installGit: async (url) => {
      const trimmed = url.trim();
      if (!trimmed) throw new Error("请输入 git 仓库地址");
      // 纯 owner/repo 输入归一化为完整 GitHub 地址（先剥 .git 尾缀防双后缀）：
      // Git 入口语义统一为 Git 来源（徽标/更新一致）。
      const base = trimmed.replace(/\.git$/i, "");
      const gitRef =
        !base.includes("://") && !base.includes("@") && base.split("/").length === 2
          ? `https://github.com/${base}.git`
          : trimmed;
      const row = await pluginInstall(gitRef, "app");
      await finishInstall(get, row);
    },

    uninstall: async (id) => {
      const p = get().plugins[id];
      if (!p) return;
      unloadPlugin(id);
      unregisterPluginUi(id);
      await pluginUninstall(id, p.scope);
      set((s) => {
        const plugins = { ...s.plugins };
        delete plugins[id];
        return { plugins };
      });
    },

    setEnabled: async (id, enabled) => {
      const p = get().plugins[id];
      if (!p || p.enabled === enabled) return;
      await pluginSetEnabled(id, enabled);
      if (enabled) {
        set((s) => ({
          plugins: { ...s.plugins, [id]: { ...s.plugins[id], enabled } },
        }));
        await spawn(id);
      } else {
        unloadPlugin(id);
        unregisterPluginUi(id);
        // 复位运行阶段（桥已移除运行时，store 残留的 active 是过期状态）。
        set((s) => {
          const cur = s.plugins[id];
          if (!cur) return s;
          return { plugins: { ...s.plugins, [id]: { ...cur, enabled: false, phase: "pending", error: undefined } } };
        });
      }
    },

    update: async (id) => {
      const p = get().plugins[id];
      if (!p) return;
      unloadPlugin(id);
      unregisterPluginUi(id);
      await pluginUpdate(id);
      // load 按 enabled 状态自动重拉（enabled 由状态文件保持），无需再显式 spawn。
      await get().load();
    },

    pluginTools: () => contributedPluginTools(),

    pluginToolMetas: () => pluginToolMetasSvc(),

    pluginSettings: () => getPluginSettings(),
    pluginSetting: (key) => getPluginSetting(key),
    pluginNode: (type) => getPluginNode(type),
    pluginNodeTypes: () => {
      const out: Record<string, ComponentType> = {};
      for (const reg of getPluginNodes()) out[reg.type] = reg.component;
      return out;
    },
    pluginAppPage: (id) => getPluginAppPages().find((p) => p.id === id),
    pluginViewKinds: () => {
      // 过滤掉「提供它的内置插件已停用或已卸载」的视图 kind：已打开面板仍显示降级占位，
      // 只是「添加视图」菜单不再提供。
      const base = allPluginViewKinds();
      const disabledKinds = new Set<string>();
      for (const v of BUILTIN_VIEWS) {
        const p = get().plugins[v.pluginId];
        if (!p || !p.enabled) disabledKinds.add(v.kind);
      }
      if (disabledKinds.size === 0) return base;
      return base.filter((k) => !disabledKinds.has(k));
    },
    pluginViewLabel: (view) => pluginViewLabelOf(view),
    viewContribution: (kind) => getViewContribution(kind),
    pluginTableView: (kind) => getPluginTableView(kind),
    pluginTableViews: () => getPluginTableViews(),
    pluginCommands: () => {
      // UI 平面优先（直接持有 run），同 globalId 去重——双平面插件可注册同名命令。
      const byGlobalId = new Map<string, PluginCommandContribution>();
      for (const c of getPluginCommands()) {
        const item: PluginCommandContribution = {
          globalId: `${c.pluginId}:${c.id}`,
          pluginId: c.pluginId,
          id: c.id,
          label: c.label,
        };
        byGlobalId.set(item.globalId, item);
      }
      for (const c of contributedCommands()) {
        if (!byGlobalId.has(c.globalId)) byGlobalId.set(c.globalId, c);
      }
      return [...byGlobalId.values()].sort((a, b) => (a.globalId < b.globalId ? -1 : 1));
    },
    runPluginCommand: (globalId) => {
      const uiCmd = getPluginCommands().find((c) => `${c.pluginId}:${c.id}` === globalId);
      if (uiCmd) {
        try {
          return Promise.resolve(uiCmd.run());
        } catch (e) {
          return Promise.reject(e);
        }
      }
      return runContributedCommand(globalId);
    },
    capabilityLabel: (namespace) => hostCapabilityLabel(namespace) ?? namespace,
    capabilitySensitive: (namespace) => hostCapabilitySensitive(namespace),

    loadMarket: async (force = false) => {
      const cached = readMarketCache();
      if (!force && cached && !isMarketStale(cached.fetchedAt)) {
        set({
          marketItems: cached.items,
          marketLoaded: true,
          marketLoading: false,
          marketError: "",
        });
        return;
      }
      set({ marketLoading: true, marketError: "" });
      try {
        const snap = await fetchMarketIndex();
        set({
          marketItems: snap.items,
          marketLoaded: true,
          marketLoading: false,
        });
      } catch (e) {
        set({ marketLoading: false, marketError: `市场加载失败：${errText(e)}` });
      }
    },

    viewKindState: (kind) => {
      // kind → 内置插件 id（宿主组件载荷映射）→ 插件行状态；非内置提供 = undefined。
      // 行缺失 = 已卸载（占位区分「停用」与「卸载」提示）。
      const v = BUILTIN_VIEWS.find((x) => x.kind === kind);
      if (!v) return undefined;
      const p = get().plugins[v.pluginId];
      if (!p) return { name: v.label, enabled: false, installed: false };
      return { name: p.manifest.name, enabled: p.enabled, installed: true };
    },
    restoreBuiltin: async () => {
      await pluginSeedBuiltin();
      await get().load();
    },
  };
});
