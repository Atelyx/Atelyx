/**
 * 插件平台 store：已装插件状态（app + 当前仓库 vault + 内置插件行）+ 运行时生命周期编排。
 *
 * 分层：本 store 是插件相关状态的唯一出口——组件不直连 `services/plugins`；
 * 插件运行时（第一方与第三方 Cordis 插件）本体在 `services/cordis`（内核/挂载器/注册表），
 * 本 store 只做编排与快照。
 * 加载时机：应用挂载/进仓后 `load()` 一次——先列清单，再逐个拉起启用插件（单个失败不影响其余）。
 * 例外说明：本 store 静态 import `components/plugins/cordis/builtins.tsx`（第一方插件注册表，
 * 组件层承载组件引用——services 不 import components 的约束所致）；该边经头注释文档化，
 * 环上跨模块访问均为函数体内延迟求值，无顶层 getState/useXxx（新增顶层触碰会 TDZ 崩溃）。
 */
import { create } from "zustand";
import type { ComponentType } from "react";
import type {
  InstalledPlugin,
  PluginIndexEntry,
  PluginManifest,
  PluginScope,
  PluginSourceKind,
} from "@/types";
import { errText, type PluginFiberPhase } from "@/types";
import {
  pluginInstall,
  pluginInstallLocal,
  pluginList,
  pluginSeedBuiltin,
  pluginSetEnabled,
  pluginUninstall,
  pluginUpdate,
} from "@/services/plugins";
import {
  getPluginAppPages,
  getPluginCommands,
  getPluginEdge,
  getPluginEdges,
  getPluginNode,
  getPluginNodes,
  getPluginSetting,
  getPluginSettings,
  getPluginThemeSettings,
  getPluginTableView,
  getPluginTableViews,
  onPluginUiChange,
} from "@/services/cordis/ui";
import {
  setAppPageOpener,
  setPluginCollabAccess,
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
  PluginEdgeRegistration,
  PluginNodeRegistration,
  PluginSettingRegistration,
  PluginTableViewRegistration,
  ThemeSettingRegistration,
} from "@/services/cordis/ui";
import type { ViewContribution } from "@/services/cordis/slots";
import {
  CORDIS_BUILTIN_BY_ID,
  CORDIS_BUILTIN_DEFS,
  builtinManifest,
} from "@/components/plugins/cordis/builtins";
import { getKernel } from "@/services/cordis/kernel";
import { mountPlugin, unmountPlugin } from "@/services/cordis/loader";
import { mountThirdPartyPlugin } from "@/services/cordis/thirdParty";
import { resolveViewKind, onSlotChange, viewKinds as slotViewKinds } from "@/services/cordis/slots";
import type { ViewSlotContribution } from "@/services/cordis/slots";
import { VIEW_LABELS } from "@/constants/views";
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
import { pluginToolMetas as pluginToolMetasSvc } from "@/services/ai/tools";
import type { AgentToolMeta } from "@/constants/tools";
import {
  fetchMarketIndex,
  isMarketStale,
  readMarketCache,
} from "@/services/plugins/market";
import { getAppVersion } from "@/services/app";
import {
  pluginCompatibleWithHost,
  validatePluginManifest,
} from "@/utils/pluginManifest";
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
  /** 插件工具的 UI 元数据（Agent 设置页名册合并；组件经此读取，不直连 services）。 */
  pluginToolMetas(): AgentToolMeta[];
  /** 插件设置项注册（设置页 tab 合并）。 */
  pluginSettings(): PluginSettingRegistration[];
  pluginSetting(key: string): PluginSettingRegistration | undefined;
  /** 某主题插件的设置项注册（主题页设置区渲染用；经 store 中转，组件不直连 services）。 */
  pluginThemeSettings(pluginId: string): ThemeSettingRegistration[];
  /** 插件画布节点注册（CanvasView nodeTypes 合并）。 */
  pluginNode(type: string): PluginNodeRegistration | undefined;
  /** 插件画布节点组件表（nodeTypes 合并用：type → component）。 */
  pluginNodeTypes(): Record<string, ComponentType>;
  /** 插件画布边注册（CanvasView edgeTypes 合并；与节点同 last-wins 语义）。 */
  pluginEdge(type: string): PluginEdgeRegistration | undefined;
  /** 插件画布边组件表（edgeTypes 合并用：type → component）。 */
  pluginEdgeTypes(): Record<string, ComponentType>;
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
  };
}

/** 停止单个插件的运行时（Cordis fiber 卸载，effects 全部撤销）。 */
async function stopPlugin(id: string): Promise<void> {
  await unmountPlugin(getKernel(), id);
}

/** slots 视图贡献 → ViewContribution 转换缓存（selector 稳定引用；随贡献对象 GC 自动失效）。 */
const slotViewCache = new WeakMap<ViewSlotContribution, ViewContribution>();

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

/** vault 写能力接线守卫：把仓库写方法暴露给 `vault` 服务（幂等一次）。
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

/** 能力变更事件接线守卫：内核侧 store 变更 → emitPluginEvent 通知订阅插件（幂等一次）。
 *  canvas/table 变更事件随各自内置插件启停注册（见 canvasStore/tableStore 的 register*PluginWiring）；
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

  /** 拉起单个启用插件的运行时：内置经 loader 挂载第一方 apply；第三方 = 包格式入口
   *  （.py 拒绝；TS 转译后 blob import 求值挂载）；失败标 failed + 可读原因，不阻塞其余。 */
  const spawn = async (id: string): Promise<void> => {
    const p = get().plugins[id];
    if (!p) return;
    // 先撤销旧运行时（重载防重复注册）。
    await stopPlugin(id);
    try {
      if (p.sourceKind === "builtin") {
        const def = CORDIS_BUILTIN_BY_ID[id];
        if (!def) {
          // 内置清单（Rust）与前端注册表（cordis/builtins.tsx）不同步：标 failed + 可读原因，
          // 防「active 但无贡献」误导（加载失败可见语义）。
          syncPhase(id, "failed", "内置插件运行定义缺失");
          return;
        }
        const result = await mountPlugin(getKernel(), { id, apply: def.apply });
        if (!result.ok) throw new Error(result.reason);
        syncPhase(id, "active");
        return;
      }
      // 声明式插件（如纯 theme）无入口：置 active 即可（主题提供者经清单消费）。
      if (!p.manifest.main) {
        syncPhase(id, "active");
        return;
      }
      const result = await mountThirdPartyPlugin(getKernel(), id, p.manifest.main);
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
      setAppPageOpener((pageId) => useAppStore.getState().openPluginPage(pageId));
      // 内核就绪（创建根 Context + 平台服务 + slots 注册 API；幂等单例）。
      getKernel();
      // 内核侧数据访问接线（vault 写/协作/ai 配置 + collab/vault 变更事件）：
      // canvas/table 能力提供者与变更事件随内置插件启停注册（cordis/builtins 的 capability）
      ensureVaultWriteAccess();
      ensureCollabRuntimeAccess();
      ensureSettingsAccess();
      ensureRuntimeChangeEvents();
      const seq = ++loadSeq;
      // 组合默认值层权威 = 前端第一方 profile（存在/顺序/默认启用；版本取宿主版本，读失败用占位）。
      const [rows, hostVersion] = await Promise.all([pluginList(), getAppVersion().catch(() => null)]);
      if (seq !== loadSeq) return; // 已有更新的 load 开始，本次作废（防孤儿 runtime）
      for (const id of Object.keys(get().plugins)) {
        await stopPlugin(id);
      }
      const plugins: Record<string, InstalledPlugin> = {};
      for (const row of rows) {
        plugins[row.id] = toInstalled(row);
      }
      set({
        plugins,
        compositionDefaults: CORDIS_BUILTIN_DEFS.map((d) => builtinManifest(d, hostVersion ?? "0.0.0")),
        initialized: true,
      });
      // 面板/菜单只订阅 uiRevision：插件行落定后补发一次，让已渲染的降级占位/菜单按新状态收敛
      // （全停用内置的冷启动无任何注册 notify）。
      set((s) => ({ uiRevision: s.uiRevision + 1 }));
      // 挂载顺序：内置先于第三方（内置提供 ctx.canvas/table 等第三方 inject 依赖的服务；
      // Cordis 注入对延迟出现的服务会重载，但挂载器一次性检查激活态，须先挂提供者）。
      const enabled = rows.filter((r) => r.enabled);
      const spawnable = [
        ...enabled.filter((r) => r.sourceKind === "builtin"),
        ...enabled.filter((r) => r.sourceKind !== "builtin"),
      ].map((r) => r.id);
      const total = spawnable.length;
      for (let i = 0; i < spawnable.length; i++) {
        if (seq !== loadSeq) return;
        const id = spawnable[i];
        if (useAppStore.getState().entryLoading) {
          useAppStore.getState().reportLoad(
            `加载插件：${get().plugins[id]?.manifest.name ?? id}（${i + 1}/${total}）`,
          );
        }
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
      // Rust 先行（含守恒校验等拒绝路径）：失败抛错时本地运行时保持完好、状态一致；
      // 成功后再清理本地运行时与贡献，删除 store 行。
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
      // load 按 enabled 状态自动重拉（enabled 由状态文件保持），无需再显式 spawn。
      await get().load();
    },

    pluginToolMetas: () => pluginToolMetasSvc(),

    pluginSettings: () => getPluginSettings(),
    pluginSetting: (key) => getPluginSetting(key),
    pluginThemeSettings: (pluginId) => getPluginThemeSettings(pluginId),
    pluginNode: (type) => getPluginNode(type),
    pluginNodeTypes: () => {
      const out: Record<string, ComponentType> = {};
      for (const reg of getPluginNodes()) out[reg.type] = reg.component;
      return out;
    },
    pluginEdge: (type) => getPluginEdge(type),
    pluginEdgeTypes: () => {
      const out: Record<string, ComponentType> = {};
      for (const reg of getPluginEdges()) out[reg.type] = reg.component;
      return out;
    },
    pluginAppPage: (id) => getPluginAppPages().find((p) => p.id === id),
    pluginViewKinds: () => {
      // 已挂载的视图槽（启用中的内置；停用/卸载随 fiber 撤销自动消失）。
      return slotViewKinds();
    },
    pluginViewLabel: (view) =>
      resolveViewKind(view)?.payload.label ?? (VIEW_LABELS as Record<string, string>)[view] ?? view,
    viewContribution: (kind) => {
      // 分派 = slots（内置/第三方视图槽统一注册表）。
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

    viewKindState: (kind) => {
      // kind → 第一方插件 id（注册表映射）→ 插件行状态；非内置提供 = undefined。
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
    restoreBuiltin: async () => {
      await pluginSeedBuiltin();
      await get().load();
    },
  };
});
