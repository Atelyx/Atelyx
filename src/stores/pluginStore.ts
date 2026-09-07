/**
 * 插件平台 store：已装插件状态（app + 当前仓库 vault）+ 运行时生命周期编排。
 *
 * 分层：本 store 是插件相关状态的唯一出口——组件不直连 `services/plugins`；
 * 运行时（Worker/桥）本体在 `services/plugins/bridge`，本 store 只做编排与快照。
 * 加载时机：应用挂载/进仓后 `load()` 一次——先列清单，再逐个拉起启用插件（单个失败不影响其余）。
 */
import { create } from "zustand";
import type { ComponentType } from "react";
import type {
  InstalledPlugin,
  PluginIndexEntry,
  PluginScope,
  PluginSourceKind,
  ToolDefinition,
} from "@/types";
import { errText, type PluginFiberPhase } from "@/types";
import {
  attachPlugin,
  contributedPluginTools,
  exposePluginFacade,
  getPluginAppPages,
  getPluginNode,
  getPluginNodes,
  getPluginPanel,
  getPluginPanels,
  getPluginSetting,
  getPluginSettings,
  getPluginTableView,
  getPluginTableViews,
  hostCapabilityLabel,
  hostCapabilitySensitive,
  loadPlugin,
  loadUiPlugin,
  onPluginUiChange,
  onRuntimeChange,
  pluginInstall,
  pluginInstallLocal,
  pluginList,
  pluginReadEntry,
  pluginSetEnabled,
  pluginUninstall,
  pluginUpdate,
  pluginViewKinds as allPluginViewKinds,
  pluginViewLabel as pluginViewLabelOf,
  runtimeSnapshot,
  setPluginTableAccess,
  startPluginProcess,
  transpileTs,
  unloadPlugin,
  unregisterPluginUi,
} from "@/services/plugins";
import type {
  PluginAppPageRegistration,
  PluginNodeRegistration,
  PluginPanelRegistration,
  PluginRow,
  PluginSettingRegistration,
  PluginTableAccess,
  PluginTableViewRegistration,
} from "@/services/plugins";
import { useTableStore } from "@/stores/tableStore";
import { useCollabStore } from "@/stores/collabStore";
import { pickDirectory as pickDirectorySvc } from "@/services/dialog";
import { buildPluginTableSnapshot } from "@/utils/table";
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
import { detectPlatform } from "@/utils/pluginHost";

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
  /** 插件面板注册（kind → 注册；ViewHost 渲染 + 视图菜单合并）。 */
  pluginPanels(): PluginPanelRegistration[];
  pluginPanel(kind: string): PluginPanelRegistration | undefined;
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
  /** 插件表格视图注册（kind → 注册；TableEditor 视图切换合并）。 */
  pluginTableView(kind: string): PluginTableViewRegistration | undefined;
  /** 全部插件表格视图注册（工具条视图列表合并用）。 */
  pluginTableViews(): PluginTableViewRegistration[];
  /** 能力命名空间展示文案（宿主能力返回注册表标签，插件命名空间原样）。 */
  capabilityLabel(namespace: string): string;
  /** 能力命名空间是否敏感（宿主注册表标记，UI「敏感」高亮）。 */
  capabilitySensitive(namespace: string): boolean;
  /** 加载市场索引（缓存未过期直接回缓存；网络失败直接提示失败）。 */
  loadMarket(force?: boolean): Promise<void>;
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
 * 注意模块环：pluginStore → tableStore → collabStore → appStore → pluginStore 为良性环——
 * 全部 store 访问延迟到回调内 `getState()`，模块顶层零触碰（与既有 tableStore↔collabStore 同模式）。 */
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
      if (uiEntry) {
        const code = await readEntry(p, uiEntry);
        loadUiPlugin(id, code);
      }
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
      } else {
        // 无逻辑平面：主线程脚本注入后即视为已加载（注册经 onPluginUiChange 刷新 UI）。
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

    /**
     * 全量重载：先取磁盘清单（失败则旧状态原样保留），再卸载旧运行时与 UI 贡献，按当前上下文
     * （app 插件 + 当前仓库 vault 插件）重建。语义 =「重置到磁盘状态」，可在 boot / 切仓库 /
     * 安装/更新后安全重复调用。
     */
    load: async () => {
      exposePluginFacade();
      ensureTableAccess();
      const seq = ++loadSeq;
      const rows = await pluginList();
      if (seq !== loadSeq) return; // 已有更新的 load 开始，本次作废（防孤儿 runtime）
      for (const id of Object.keys(get().plugins)) {
        unloadPlugin(id);
        unregisterPluginUi(id);
      }
      const plugins: Record<string, InstalledPlugin> = {};
      for (const row of rows) {
        plugins[row.id] = toInstalled(row);
      }
      set({ plugins, initialized: true });
      for (const row of rows) {
        if (!row.enabled) continue;
        if (seq !== loadSeq) return;
        await spawn(row.id);
      }
      if (seq !== loadSeq) return;
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
      unloadPlugin(id);
      unregisterPluginUi(id);
      await pluginUpdate(id);
      // load 按 enabled 状态自动重拉（enabled 由状态文件保持），无需再显式 spawn。
      await get().load();
    },

    pluginTools: () => contributedPluginTools(),

    pluginToolMetas: () => pluginToolMetasSvc(),

    pluginPanels: () => getPluginPanels(),
    pluginPanel: (kind) => getPluginPanel(kind),
    pluginSettings: () => getPluginSettings(),
    pluginSetting: (key) => getPluginSetting(key),
    pluginNode: (type) => getPluginNode(type),
    pluginNodeTypes: () => {
      const out: Record<string, ComponentType> = {};
      for (const reg of getPluginNodes()) out[reg.type] = reg.component;
      return out;
    },
    pluginAppPage: (id) => getPluginAppPages().find((p) => p.id === id),
    pluginViewKinds: () => allPluginViewKinds(),
    pluginViewLabel: (view) => pluginViewLabelOf(view),
    pluginTableView: (kind) => getPluginTableView(kind),
    pluginTableViews: () => getPluginTableViews(),
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
  };
});
