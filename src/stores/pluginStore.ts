/**
 * 插件平台 store：插件行状态（app + 当前仓库 vault）+ 组合层（默认组合 + 已装行）+ 生命周期编排。
 *
 * 分层：本 store 是插件相关状态的唯一出口——组件不直连 `services/plugins`；
 * 插件运行时（Cordis 内核/挂载器/注册表）在 `services/cordis`，组合层推导在 `utils/cordis/composition`，
 * 本 store 只做编排与快照。
 * 装配：行来自插件列表（磁盘包行 + 随应用分发的行），装配顺序 = 默认组合成员在前、其余按 id 追加；
 * 挂载实现按「入口解析方式」判定——行有落位目录则读该包磁盘入口，否则经随应用分发实现注册表取编译实现。
 * 「谁替换谁」由插件在 apply 里经 ctx.slots 的 priority / inject 声明，组合层不提供行级覆盖。
 * 加载时机：应用挂载/进仓后 `load()` 一次——先按默认组合清单播种并取行，再按装配顺序拉起启用行。
 * 例外说明：本 store 静态 import `components/plugins/cordis/builtins.tsx`（随应用分发插件注册表，
 * 组件层承载组件引用——services 不 import components 的约束所致）；该边经头注释文档化，
 * 环上跨模块访问均为函数体内延迟求值，无顶层 getState/useXxx（新增顶层触碰会 TDZ 崩溃）。
 */
import { create } from "zustand";
import type { ComponentType } from "react";
import type { InstalledPlugin, PluginIndexEntry, PluginManifest, PluginPackageJson, PluginScope } from "@/types";
import { errText, type PluginFiberPhase } from "@/types";
import type { AppUiState } from "@/types";
import {
  pluginInstall,
  pluginInstallLocal,
  pluginList,
  pluginSeedDefault,
  pluginSetEnabled,
  pluginUninstall,
  pluginUpdate,
} from "@/services/plugins";
import {
  getPluginAppPages,
  getPluginCommands,
  getPluginEdges,
  getPluginNodes,
  getPluginSettings,
  getPluginThemeSettings,
  getPluginTableView,
  getPluginTableViews,
  onPluginUiChange,
} from "@/services/cordis/ui";
import {
  setAppPageOpener,
  setPluginCollabAccess,
  setPluginHistoryAccess,
  setPluginLayoutAccess,
  setPluginNotificationAccess,
  setPluginUiStateAccess,
  setPluginVaultWriteAccess,
  setSettingsAccess,
} from "@/services/cordis/access";
import { emitPluginEvent } from "@/services/cordis/events";
import { auditSnapshot, type PluginAuditEntry } from "@/services/cordis/audit";
import { PLUGIN_SERVICE_LABELS, PLUGIN_SERVICE_SENSITIVE } from "@/constants/pluginServices";
import type { PluginRow } from "@/services/plugins";
import type {
  PluginAppPageRegistration,
  PluginCommandContribution,
  PluginSettingRegistration,
  PluginTableViewRegistration,
  ThemeSettingRegistration,
} from "@/services/cordis/ui";
import type { ViewContribution } from "@/services/cordis/slots";
import {
  CORDIS_BUILTIN_BY_ID,
  CORDIS_BUILTIN_DEFS,
  DEFAULT_COMPOSITION,
  builtinManifest,
} from "@/components/plugins/cordis/builtins";
import { getKernel } from "@/services/cordis/kernel";
import { installCommandHotkeys } from "@/services/cordis/commandHotkeys";
import { mountPlugin, unmountAll, unmountPlugin } from "@/services/cordis/loader";
import { mountPluginFromPackage } from "@/services/cordis/packageMount";
import { resolveViewKind, onSlotChange, viewKinds as slotViewKinds } from "@/services/cordis/slots";
import type { ViewSlotContribution } from "@/services/cordis/slots";
import { VIEW_LABELS } from "@/constants/views";
import { composePlugins, compositionPackages, mountOrder } from "@/utils/cordis/composition";
import { useCollabStore, publishPluginPresence } from "@/stores/collabStore";
import { useVaultStore } from "@/stores/vaultStore";
import { useNoteStore } from "@/stores/noteStore";
import { useAppStore } from "@/stores/appStore";
import { useCanvasStore } from "@/stores/canvasStore";
import { useTableStore } from "@/stores/tableStore";
import { useRepoHistoryStore } from "@/stores/repoHistoryStore";
import { useUiStateStore } from "@/stores/uiStateStore";
import { useNotificationStore } from "@/stores/notificationStore";
import { useSettingsStore } from "@/stores/settingsStore";
import { loadHistory } from "@/services/history";
import { layoutOp } from "@/services/layout";
import { pickDirectory as pickDirectorySvc } from "@/services/dialog";
import {
  appendVaultFile,
  editVaultFile,
  writeVaultFile,
} from "@/services/vault/aiFiles";
import { pluginToolMetas as pluginToolMetasSvc } from "@/services/ai/tools";
import type { AgentToolMeta } from "@/constants/tools";
import {
  fetchMarketIndex,
  isMarketStale,
  readMarketCache,
} from "@/services/plugins/market";
import { getAppVersion } from "@/services/app";
import { pluginCompatibleWithHost, validatePluginManifest } from "@/utils/pluginManifest";
import { detectPlatform } from "@/utils/pluginHost";

/** 某视图 kind 的默认实现提供行状态（ViewHost 降级占位用；installed=false = 已卸载）。 */
interface ViewProviderState {
  name: string;
  enabled: boolean;
  installed: boolean;
}

interface PluginStoreState {
  /** 插件行（按 id；运行时阶段/审计与列表行合并）。 */
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
  /** 加载插件行并按装配顺序拉起运行时。 */
  load(): Promise<void>;
  /** 从 GitHub 仓库安装（repo 为 `owner/repo` 市场引用或完整 git 地址；新装一律停用）。 */
  install(repo: string, scope: PluginScope): Promise<PluginInstallResult>;
  /** 从本地目录安装（junction/符号链接实时引用，源目录改动即时生效；当前仅 app 级）。 */
  installLocal(path: string): Promise<PluginInstallResult>;
  /** 调系统目录选择器选插件源目录并安装；用户取消 = false（未安装）。 */
  installLocalFromPicker(): Promise<boolean>;
  /** 从 git 地址安装（git clone，保留 .git 供更新；当前仅 app 级）。 */
  installGit(url: string): Promise<PluginInstallResult>;
  /** 卸载（删除目录/链接 + 终止运行时 + 清理状态）。 */
  uninstall(id: string): Promise<void>;
  /** 启用/停用（启用 = 拉起运行时；停用 = 终止运行时）。 */
  setEnabled(id: string, enabled: boolean): Promise<void>;
  /** 更新（备份 → 安装 → 失败回滚；成功则重载运行时）。 */
  update(id: string): Promise<void>;
  /** 恢复默认装配：补播种已卸载的默认行 + 重载。 */
  restoreDefaultComposition(): Promise<void>;
  /** 插件工具的 UI 元数据（Agent 设置页名册合并；组件经此读取，不直连 services）。 */
  pluginToolMetas(): AgentToolMeta[];
  /** 插件设置项注册（设置页 tab 合并）。 */
  pluginSettings(): PluginSettingRegistration[];
  /** 某主题插件的设置项注册（主题页设置区渲染用；经 store 中转，组件不直连 services）。 */
  pluginThemeSettings(pluginId: string): ThemeSettingRegistration[];
  /** 插件画布节点组件表（nodeTypes 合并用：type → component，single 槽胜出）。 */
  pluginNodeTypes(): Record<string, ComponentType>;
  /** 插件画布边组件表（edgeTypes 合并用：type → component，single 槽胜出）。 */
  pluginEdgeTypes(): Record<string, ComponentType>;
  /** 插件应用页面注册（app 页面/模式全页接管）。 */
  pluginAppPage(id: string): PluginAppPageRegistration | undefined;
  /** 面板视图候选（内建 + 插件面板）。 */
  pluginViewKinds(): string[];
  /** 视图显示名（含插件面板，未知视图原样兜底）。 */
  pluginViewLabel(view: string): string;
  /** 某视图的贡献（统一视图槽注册表；ViewHost 分派用，缺注册 = 空面板占位）。 */
  viewContribution(kind: string): ViewContribution | undefined;
  /** 插件表格视图注册（kind → 注册；TableEditor 视图切换合并）。 */
  pluginTableView(kind: string): PluginTableViewRegistration | undefined;
  /** 全部插件表格视图注册（工具条视图列表合并用）。 */
  pluginTableViews(): PluginTableViewRegistration[];
  /** 插件命令（管理 UI「运行命令」入口）。 */
  pluginCommands(): PluginCommandContribution[];
  /** 执行插件命令。 */
  runPluginCommand(globalId: string): Promise<unknown>;
  /** 服务展示文案（Atelyx 服务标签，未知服务原样）。 */
  capabilityLabel(namespace: string): string;
  /** 服务是否敏感（展示「敏感」高亮）。 */
  capabilitySensitive(namespace: string): boolean;
  /** 插件审计快照（实际 = ctx 服务读 + 事件订阅，按插件归属；声明对照的实际侧）。 */
  pluginAudit(): PluginAuditEntry[];
  /** 加载市场索引（缓存未过期直接回缓存；网络失败直接提示失败）。 */
  loadMarket(force?: boolean): Promise<void>;
  /** 某视图 kind 的默认实现提供行状态（组件经此查，不直连 services）；非默认组合提供 = undefined。 */
  viewProviderState(kind: string): ViewProviderState | undefined;
}

/** 安装结果：`id` = 包内清单声明的实际落位 id（可能不同于市场索引 id），`replaced` = 是否替代了同名既有行。 */
export interface PluginInstallResult {
  id: string;
  replaced: boolean;
}

/** 列表行 → store 条目（清单经前端校验归一化；Rust 侧已滤除损坏清单，回退 cast 仅兜底意外形态）。 */
function toInstalled(row: PluginRow): InstalledPlugin {
  const validated = validatePluginManifest(row.manifest);
  return {
    id: row.id,
    manifest: validated.ok ? validated.manifest : (row.manifest as unknown as PluginManifest),
    scope: row.scope,
    installDir: row.installDir,
    sourceKind: row.sourceKind,
    enabled: row.enabled,
    phase: "pending",
  };
}

/** 停止单个插件的运行时（Cordis fiber 卸载，effects 全部撤销）。 */
async function stopPlugin(id: string): Promise<void> {
  await unmountPlugin(getKernel(), id);
}

/** slots 视图贡献 → ViewContribution 转换缓存（selector 稳定引用；随贡献对象 GC 自动失效）。 */
const slotViewCache = new WeakMap<ViewSlotContribution, ViewContribution>();

/** 安装后统一收尾（模块私有）：宿主兼容强制 + 重载；返回落位行（调用方按实际 id 提示）。 */
async function finishInstall(get: () => PluginStoreState, row: PluginRow): Promise<PluginRow> {
  try {
    // 宿主兼容强制（清单承诺）：契约版本/宿主版本/平台不匹配即回滚并报错。
    // 宿主版本读取失败（瞬时 IPC 异常）传 null：跳过版本范围判断，不误删刚装好的插件
    const hostVersion = await getAppVersion().catch(() => null);
    const compat = pluginCompatibleWithHost(row.manifest, hostVersion, detectPlatform());
    if (!compat.ok) throw new Error(`无法安装：${compat.reason}`);
  } catch (e) {
    // 任一检查失败都回滚已落盘插件，避免「装了一半」留脏。
    await pluginUninstall(row.id, row.scope).catch(() => {});
    throw e;
  }
  await get().load();
  return row;
}

/** vault 写能力接线守卫：把仓库写方法暴露给 `vault` 服务（幂等一次）。
 *  复用 AI 文件工具同一批 service/store 语义（原子写/扩展名分发引用维护/树刷新）；
 *  `.md` 写入对打开的笔记会话就是一次磁盘内容变化（会话按内容事实收敛，不按调用方放行）；
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

/** 协作能力接线守卫：把在线用户与 presence 上报暴露给内核 `collab` 服务（幂等一次）。 */
let collabRuntimeWired = false;
function ensureCollabRuntimeAccess(): void {
  if (collabRuntimeWired) return;
  collabRuntimeWired = true;
  setPluginCollabAccess({
    peers: () => useCollabStore.getState().peers,
    setPresence: (view, file) => publishPluginPresence(view, file),
  });
}

/** 通知能力接线：把应用内通知运行时暴露给内核 `notification` 服务（幂等一次）。 */
let notificationWired = false;
function ensureNotificationAccess(): void {
  if (notificationWired) return;
  notificationWired = true;
  setPluginNotificationAccess({
    notify: (input) => useNotificationStore.getState().notify(input),
    dismiss: (id) => useNotificationStore.getState().dismiss(id),
  });
}

/** AI 配置接线守卫：把供应商/模型/Agent 与默认目标解析暴露给内核 `ai` 服务（幂等一次）。
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

/** 领域历史访问接线守卫：把历史列表/回滚/仓库聚合暴露给内核 `history` 服务（幂等一次）。 */
let historyAccessWired = false;
function ensureHistoryAccess(): void {
  if (historyAccessWired) return;
  historyAccessWired = true;
  setPluginHistoryAccess({
    list: (kind, file) => loadHistory(kind, file),
    rollback: async (kind, file, seq) => {
      if (kind === "note") await useNoteStore.getState().noteHistoryRollback(file, seq);
      else if (kind === "canvas") await useCanvasStore.getState().canvasHistoryRollback(file, seq);
      else await useTableStore.getState().tableHistoryRollback(file, seq);
    },
    repoHistory: () => {
      const s = useRepoHistoryStore.getState();
      return { entries: s.entries, dailyCounts: s.dailyCounts };
    },
  });
}

/** 布局访问接线守卫：把布局镜像 + 安全操作子集暴露给内核 `layout` 服务（幂等一次）。
 *  addView/op 均经 layout-op（Rust 是唯一变更入口）。 */
let layoutAccessWired = false;
function ensureLayoutAccess(): void {
  if (layoutAccessWired) return;
  layoutAccessWired = true;
  setPluginLayoutAccess({
    activeLayoutId: () => useUiStateStore.getState().activeLayoutId,
    layouts: () => useUiStateStore.getState().workspaceLayouts,
    addView: (panelId, view) => layoutOp({ op: "addView", panelId, view }),
    op: (op) => layoutOp(op),
  });
}

/** 应用级 UI 使用状态访问接线守卫：把非布局字段 + 布局镜像暴露给内核 `uiState` 服务（幂等一次）。 */
let uiStateAccessWired = false;
function ensureUiStateAccess(): void {
  if (uiStateAccessWired) return;
  uiStateAccessWired = true;
  setPluginUiStateAccess({
    read: () => {
      const s = useUiStateStore.getState();
      return {
        fileExplorerExpanded: [...s.fileExplorerExpanded],
        lastCanvasFile: s.lastCanvasFile ?? undefined,
        lastNoteFile: s.lastNoteFile ?? undefined,
        lastTableFile: s.lastTableFile ?? undefined,
        workspaceLayouts: s.workspaceLayouts,
        activeLayoutId: s.activeLayoutId ?? undefined,
        focusedPanelId: s.focusedPanelId ?? undefined,
        detachedWindows: s.detachedWindows,
        recentFiles: s.recentFiles,
      } as unknown as AppUiState;
    },
  });
}

/** 能力变更事件接线守卫：内核侧 store 变更 → emitPluginEvent 通知订阅插件（幂等一次）。
 *  canvas/table 变更事件随各自插件启停注册（见 canvasStore/tableStore 的 register*PluginWiring）；
 *  collab/vault 属内核数据访问，常驻。载荷为轻量信号（插件按需再调 snapshot()/取数据）。 */
let runtimeEventsWired = false;
function ensureRuntimeChangeEvents(): void {
  if (runtimeEventsWired) return;
  runtimeEventsWired = true;
  useCollabStore.subscribe((s, prev) => {
    if (s.peers !== prev.peers) emitPluginEvent("collab:changed", { peers: s.peers });
  });
  useVaultStore.subscribe((s, prev) => {
    if (s.tree !== prev.tree) emitPluginEvent("vault:changed", {});
  });
}

export const usePluginStore = create<PluginStoreState>()((set, get) => {
  /** 置插件运行阶段（加载/激活/失败）。 */
  const syncPhase = (id: string, phase: PluginFiberPhase, error?: string): void => {
    set((s) => {
      const cur = s.plugins[id];
      if (!cur) return s;
      return { plugins: { ...s.plugins, [id]: { ...cur, phase, error } } };
    });
  };

  /** 默认组合清单（默认组合层定义 → 原始插件包清单；版本取宿主版本）。 */
  const defaultManifests = (hostVersion: string | null): PluginPackageJson[] =>
    CORDIS_BUILTIN_DEFS.map((d) => builtinManifest(d, hostVersion ?? "0.0.0"));

  /** 拉起单个插件行的运行时：行有落位目录（磁盘包）则读该包入口，否则经实现注册表取编译实现
   *  （同为「入口解析方式」，与列表的磁盘行优先一致）；失败标 failed + 可读原因。 */
  const spawn = async (id: string): Promise<void> => {
    // 先撤销旧运行时（重载防重复注册）。
    await stopPlugin(id);
    try {
      const plugin = get().plugins[id];
      if (!plugin) throw new Error("插件不存在");
      if (plugin.installDir === "") {
        const def = CORDIS_BUILTIN_BY_ID[id];
        if (!def) throw new Error("实现随应用编译但缺少对应实现定义");
        const result = await mountPlugin(getKernel(), { id, apply: def.apply });
        if (!result.ok) throw new Error(result.reason);
        syncPhase(id, "active");
        return;
      }
      // 磁盘包无入口 = 声明式插件（如纯 theme）：置 active 即可（主题提供者经清单消费）。
      if (!plugin.manifest.main) {
        syncPhase(id, "active");
        return;
      }
      const result = await mountPluginFromPackage(getKernel(), id, plugin.manifest.main);
      if (!result.ok) throw new Error(result.reason);
      syncPhase(id, "active");
    } catch (e) {
      syncPhase(id, "failed", errText(e));
    }
  };

  onPluginUiChange(() => set((s) => ({ uiRevision: s.uiRevision + 1 })));
  // 槽注册变化（视图槽随 fiber 挂载/撤销）→ uiRevision 驱动视图菜单/面板重渲染。
  onSlotChange(() => set((s) => ({ uiRevision: s.uiRevision + 1 })));

  /** load 序号守卫：并发 load（回启动页 fire-and-forget 与紧接着进仓 load 竞态）时
   * 只允许最后一次生效，防止旧 load 覆盖插件表后残留孤儿 runtime。 */
  let loadSeq = 0;

  /** 装配顺序：默认组合成员在前，其余按 id 追加（行有落位目录 → 磁盘入口，否则编译实现）。 */
  const mountIds = (): string[] =>
    mountOrder(composePlugins(DEFAULT_COMPOSITION, compositionPackages(get().plugins)));

  return {
    plugins: {},
    initialized: false,
    uiRevision: 0,
    marketItems: [],
    marketLoading: false,
    marketError: "",
    marketLoaded: false,

    /**
     * 全量重载：先按默认组合清单播种并取行（失败则旧状态原样保留），再卸载旧运行时与 UI 贡献，
     * 按装配顺序重建。语义 =「重置到当前插件行状态」，可在 boot / 切仓库 / 安装更新后安全重复调用。
     */
    load: async () => {
      setAppPageOpener((pageId) => useAppStore.getState().openPluginPage(pageId));
      // 内核就绪（创建根 Context + 平台服务 + slots 注册 API；幂等单例）。
      getKernel();
      // 内核侧数据访问接线（vault 写/协作/ai 配置 + collab/vault 变更事件）：
      // canvas/table 能力提供者与变更事件随对应插件启停注册（cordis/builtins 的 capability）
      ensureVaultWriteAccess();
      ensureCollabRuntimeAccess();
      ensureNotificationAccess();
      ensureSettingsAccess();
      ensureHistoryAccess();
      ensureLayoutAccess();
      ensureUiStateAccess();
      ensureRuntimeChangeEvents();
      installCommandHotkeys();
      const seq = ++loadSeq;
      // 默认组合清单权威在本层（存在/顺序/清单随 App 版本）；Rust 据此播种随应用分发的行。
      const hostVersion = await getAppVersion().catch(() => null);
      const rows = await pluginList(defaultManifests(hostVersion));
      if (seq !== loadSeq) return; // 已有更新的 load 开始，本次作废（防孤儿 runtime）
      // 清场：卸载内核里全部已挂载 fiber（比按 store 行逐个 stop 更彻底，含已不在行里的残留）。
      await unmountAll(getKernel());
      const plugins: Record<string, InstalledPlugin> = {};
      for (const row of rows) {
        plugins[row.id] = toInstalled(row);
      }
      set({ plugins, initialized: true });
      // 面板/菜单只订阅 uiRevision：行落定后补发一次，让已渲染的降级占位/菜单按新状态收敛
      // （全停用行的冷启动无任何注册 notify）。
      set((s) => ({ uiRevision: s.uiRevision + 1 }));
      // 装配顺序 = 默认组合成员在前（其提供者先就绪，满足后续行的 inject 依赖）。
      const mounts = mountIds();
      const total = mounts.length;
      const platform = detectPlatform();
      for (let i = 0; i < mounts.length; i++) {
        if (seq !== loadSeq) return;
        const id = mounts[i];
        // 加载前兼容校验（契约版本/宿主版本范围/平台）：不兼容的行响亮失败并附原因，不挂载、不阻塞其余行
        const manifest = get().plugins[id].manifest;
        const compat = pluginCompatibleWithHost(manifest, hostVersion, platform);
        if (!compat.ok) {
          syncPhase(id, "failed", compat.reason);
          continue;
        }
        if (useAppStore.getState().entryLoading) {
          useAppStore.getState().reportLoad(
            `加载插件：${get().plugins[id]?.manifest.name ?? id}（${i + 1}/${total}）`,
          );
        }
        await spawn(id);
      }
    },

    install: async (repo, scope) => {
      const before = new Set(Object.keys(get().plugins));
      const row = await pluginInstall(repo, scope);
      await finishInstall(get, row);
      // 替换判定按「包内实际 id」对安装前快照比对：市场索引 id 与包内 name 不一致时，
      // 只认后者才能如实提示被替代的行
      return { id: row.id, replaced: before.has(row.id) };
    },

    installLocal: async (path) => {
      const before = new Set(Object.keys(get().plugins));
      const row = await pluginInstallLocal(path, "app");
      await finishInstall(get, row);
      return { id: row.id, replaced: before.has(row.id) };
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
      const before = new Set(Object.keys(get().plugins));
      const row = await pluginInstall(gitRef, "app");
      await finishInstall(get, row);
      return { id: row.id, replaced: before.has(row.id) };
    },

    uninstall: async (id) => {
      const p = get().plugins[id];
      if (!p) return;
      // Rust 先行（含守恒校验等拒绝路径）：失败抛错时本地运行时保持完好、状态一致；
      // 成功后再终止运行时与贡献、删除 store 行（默认组合成员卸载后仍在列表中成灰行）。
      await pluginUninstall(id, p.scope);
      await stopPlugin(id);
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
        await stopPlugin(id);
        // 复位运行阶段（运行时已移除，store 残留的 active 是过期状态）。
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
      await stopPlugin(id);
      await pluginUpdate(id);
      // load 按行状态自动重拉（enabled 由状态文件保持），无需再显式 spawn。
      await get().load();
    },

    pluginToolMetas: () => pluginToolMetasSvc(),

    pluginSettings: () => getPluginSettings(),
    pluginThemeSettings: (pluginId) => getPluginThemeSettings(pluginId),
    pluginNodeTypes: () => {
      const out: Record<string, ComponentType> = {};
      for (const reg of getPluginNodes()) out[reg.type] = reg.component;
      return out;
    },
    pluginEdgeTypes: () => {
      const out: Record<string, ComponentType> = {};
      for (const reg of getPluginEdges()) out[reg.type] = reg.component;
      return out;
    },
    pluginAppPage: (id) => getPluginAppPages().find((p) => p.id === id),
    pluginViewKinds: () => {
      // 已挂载的视图槽（启用中的行；停用/卸载随 fiber 撤销自动消失）。
      return slotViewKinds();
    },
    pluginViewLabel: (view) =>
      resolveViewKind(view)?.payload.label ?? (VIEW_LABELS as Record<string, string>)[view] ?? view,
    viewContribution: (kind) => {
      // 分派 = slots（统一视图槽注册表）。
      // 转换结果按槽贡献对象缓存：selector 订阅需稳定引用（新对象会触发无限重渲染），
      // 贡献卸载/重挂载时是新对象 → 自然换缓存；旧对象随 WeakMap 自动回收。
      const slot = resolveViewKind(kind);
      if (!slot) return undefined;
      let cached = slotViewCache.get(slot);
      if (!cached) {
        cached = {
          kind,
          label: slot.payload.label,
          component: slot.payload.component,
          render: slot.payload.render,
          pluginId: slot.pluginId,
        };
        slotViewCache.set(slot, cached);
      }
      return cached;
    },
    pluginTableView: (kind) => getPluginTableView(kind),
    pluginTableViews: () => getPluginTableViews(),
    pluginCommands: () => {
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
      return Promise.reject(new Error("命令不存在"));
    },
    capabilityLabel: (namespace) => PLUGIN_SERVICE_LABELS[namespace] ?? namespace,
    capabilitySensitive: (namespace) => PLUGIN_SERVICE_SENSITIVE.has(namespace),
    pluginAudit: () => auditSnapshot(getKernel().ctx),

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

    viewProviderState: (kind) => {
      // kind → 随应用分发的默认实现行（注册表映射）→ 行状态；非默认组合提供 = undefined。
      // 行缺失 = 已卸载（占位区分「停用」与「卸载」提示）。
      for (const def of CORDIS_BUILTIN_DEFS) {
        const v = def.views.find((x) => x.kind === kind);
        if (!v) continue;
        const plugin = get().plugins[def.id];
        if (!plugin) return { name: v.label, enabled: false, installed: false };
        return { name: plugin.manifest.name, enabled: plugin.enabled, installed: true };
      }
      return undefined;
    },

    restoreDefaultComposition: async () => {
      const hostVersion = await getAppVersion().catch(() => null);
      await pluginSeedDefault(defaultManifests(hostVersion));
      await get().load();
    },
  };
});
