import { create } from "zustand";
import { getApiKey, setApiKey, deleteApiKey } from "@/services/keychain";
import { readAgents, readFolderColors, readNote, readPromptNotes, readVaultConfig, writeAgents, writeFolderColors, writePromptNotes, writeVaultConfig } from "@/services/vault";
import { fetchProviderModels } from "@/services/ai/client";
import { buildAgentTools } from "@/services/ai/tools";
import { getHostname, readGlobalConfig, updateGlobalConfig } from "@/services/global";
import { useAppStore } from "@/stores/appStore";
import { useCollabStore } from "@/stores/collabStore";
import type {
  AgentConfig,
  AiConfig,
  ChatTargetResult,
  FileExplorerSortKey,
  GlobalConfig,
  GlobalProvider,
  GlobalSearchConfig,
  ProviderConfig,
  ToolSchema,
  VaultConfig,
} from "@/types";
import {
  BUILTIN_THEME_PLUGIN_ID,
  DEFAULT_BUILTIN_THEME_SETTINGS,
  normalizeThemeConfig,
} from "@/utils/pluginTheme";
import { DEFAULT_AI_CONFIG } from "@/constants/ai";
import { DEFAULT_AGENT_TOOLS } from "@/constants/tools";
import { BUILTIN_AGENTS, BUILTIN_AGENT_CHAT_ID } from "@/constants/agents";
import { PROVIDER_PRESETS } from "@/constants/providers";
import { remapDirPrefix } from "@/utils/filename";
import { modelDisplayLabel } from "@/utils/text";
import { createPersistController } from "@/utils/persist";

/**
 * 设置 store（供应商/搜索源等仓库化，界面外观应用级）。
 *
 * 仓库级配置（`.atelyx/config.json`）：AI 供应商 + 默认模型 + 搜索源 +
 * 文件面板排序/排除文件夹/附件文件夹等按仓库独立；API key 默认走 keychain 条目
 * `provider-<vaultId>-<providerId>`（**按仓库隔离**）；开启 `syncKeys`
 * （「API key 随仓库保存」，多设备同步）后 key 明文随 config.json 落盘。
 * 应用级配置（`app_data_dir/global.json`，随 `updateGlobalConfig` 落盘）：主题 +
 * 强调色 + 字号/字体 + 自动恢复上次打开文件，跨仓库共享。
 *
 * 解析链（画布对话节点 / AI 对话面板共用 `resolveChatTarget`）：
 * 选定 {providerId, model}（无 = 跟随仓库默认）→ 供应商缺失报错不静默回落；
 * 未选定 = 仓库默认模型（vaultConfig.model + 固定供应商 modelProviderId：固定供应商含该模型则用之、
 * 失效判定未配置；仅旧配置无固定供应商时按 model 名反查），未配置默认模型报错；
 * 选定供应商但未选模型 = 供应商首个模型（models[0]）。
 *
 * 加载时机：应用挂载 load（读 global.json 填充应用级外观）；
 * selectVault → loadVaultConfig（读 config.json；syncKeys 关时从 keychain 填充 key，开时直读 config 内 key）；无仓库时状态为空。
 */

interface SettingsState {
  /** 运行时 AI 配置（providers 含 key，从 keychain 填充）。 */
  config: AiConfig;
  /** 激活的主题插件 id（应用级，写 global.json；缺省 = 内置主题插件，其深浅模式默认跟随系统）。 */
  theme: string;
  /** 各主题插件的设置项值字典（应用级，写 global.json；预置键 colorMode/accentColor + 插件自定义键）。 */
  themeSettings: Record<string, Record<string, unknown>>;
  /** 应用级界面基础字号（px；undefined = 默认 18，存 global.json）。 */
  fontSize?: number;
  /** 应用级界面字体（CSS font-family；undefined = 系统默认，存 global.json）。 */
  fontFamily?: string;
  /** 进入仓库时自动恢复上次打开的文件（应用级，存 global.json；缺省 true = 开启）。 */
  autoRestoreFiles: boolean;
  /** 进入仓库时自动切到「主页」布局（应用级，存 global.json；缺省 false = 保持恢复上次界面）。 */
  defaultHomeLayout: boolean;
  /** 协作中转（collab-relay）开关（应用级，存 global.json；缺省 false = 关闭）。 */
  collabEnabled: boolean;
  /** 协作中转地址（应用级，如 ws://192.168.1.10:17701/ws）。 */
  collabRelayUrl: string;
  /** 协作显示昵称（空 = 设备名兜底）。 */
  collabNickname: string;
  /** 协作身份色（hex；空 = 随机分配）。 */
  collabColor: string;
  /** 本机设备名（get_hostname：昵称兜底 + 在线列表展示）。 */
  deviceName: string;
  /** 当前仓库级覆盖；null = 未打开仓库。 */
  vaultConfig: VaultConfig | null;
  /** 搜索源配置（仓库级，无 key；Tavily key 运行时从 keychain 读）。 */
  searchConfig: GlobalSearchConfig;
  /** Tavily API key（运行时，keychain 条目 `provider-<vaultId>-search-tavily`）。 */
  tavilyKey: string;
  /** 已标记为系统提示词的笔记相对路径列表（独立落盘 .atelyx/prompt-notes.json，config.json 不承载）。 */
  promptNotes: string[];
  /** Agent 配置列表（仓库级，独立落盘 .atelyx/agents.json；对话节点/面板按 id 实时引用）。 */
  agents: AgentConfig[];
  /** 文件面板文件夹图标颜色（相对仓库根路径 → hex 色；独立落盘 .atelyx/folder-colors.json）。 */
  folderColors: Record<string, string>;
  loaded: boolean;

  /** 应用挂载时调用：读 global.json 填充应用级外观（主题/强调色/字号/字体/自动恢复），重置仓库级运行时状态。 */
  load: () => Promise<void>;
  /** 打开仓库后读 `.atelyx/config.json`（AI 供应商/搜索源等仓库级配置）+ keychain 填充 key。由 appStore.selectVault 调用。 */
  loadVaultConfig: () => Promise<void>;
  /** 返回仓库选择页时清空仓库级状态（应用级外观保留）。由 appStore.backToVaultSelect 调用。 */
  clearVaultConfig: () => void;
  /** 设搜索源（仓库级，写 .atelyx/config.json）。 */
  setSearchConfig: (patch: Partial<GlobalSearchConfig>) => Promise<void>;
  /** 设 Tavily API key（仓库级；syncKeys 关 = 写 keychain 条目 `provider-<vaultId>-search-tavily`，开 = 随 config.json 落盘；空串删除）。 */
  setTavilyKey: (key: string) => Promise<void>;
  /** 开关「API key 随仓库保存」（多设备同步）：开启 = 当前 key 全量写入 config.json；关闭 = 剥离 config key + 回写 keychain。 */
  setSyncKeys: (enabled: boolean) => Promise<void>;
  /** 解析仓库默认模型及其所属供应商：默认模型可来自任意供应商（模型服务 tab 从全部供应商的 models 中选），
   * 优先按存储的固定供应商（modelProviderId）定位，缺失/失效回退按模型名反查；未配置返回 null。 */
  resolveDefaultModel: () => { provider: ProviderConfig; model: string } | null;
  /**
   * 解析一次对话请求的目标 {provider, model}（画布对话节点 / AI 对话面板共用）：
   * 选定 {providerId, model}（null = 跟随仓库默认）优先，**不回退默认**；选定供应商已删 → 报错不静默回落；
   * 未选定 = 仓库默认模型（vaultConfig.model + 固定供应商 modelProviderId：固定供应商含该模型则用之、
   * 失效判定未配置；仅旧配置无固定供应商时按 model 名反查），未配置默认模型 → 报错；
   * 选定供应商但未选模型 → 供应商首个模型（models[0]）。
   * 失败返回 {ok:false, reason, error}，调用方负责提示。
   */
  resolveChatTarget: (
    selection?: { providerId?: string; model?: string } | null,
  ) => ChatTargetResult;
  /** 搜索源是否已配置（tavily key 或 searxng URL 存在）——工具开关开着但未配置时发送提示并降级。 */
  isSearchConfigured: () => boolean;
  /** 解析话题自动命名模型：设置页指定（autoNamingModel）→ 仓库默认模型（vaultConfig.model）；未配置返回 null。ignoreToggle = 重新命名场景，不受「话题自动命名」开关限制。 */
  resolveAutoNamingModel: (ignoreToggle?: boolean) => { provider: ProviderConfig; model: string } | null;
  /** 新增 provider（基于预设或空白），返回新 id。 */
  addProvider: (preset?: (typeof PROVIDER_PRESETS)[number]) => Promise<string>;
  /** 拉取供应商可用模型 ID 列表（GET {baseUrl}/models；设置页「获取模型列表/测试连通性」共用）。失败抛错，由调用方降级展示。 */
  fetchProviderModelIds: (id: string) => Promise<string[]>;
  /** 更新 provider（debounce 落盘，含 keychain 写）。 */
  updateProvider: (id: string, patch: Partial<ProviderConfig>) => Promise<void>;
  /** 删除 provider（同步删 keychain 条目）。 */
  removeProvider: (id: string) => Promise<void>;
  /** 设仓库级默认模型（null = 未配置——跟随默认的对话请求会报错提示）。 */
  setVaultModel: (model: { providerId: string; model: string } | null) => Promise<void>;
  /** 开关话题自动命名（仓库级；缺省不启用）。 */
  setAutoNamingEnabled: (enabled: boolean) => Promise<void>;
  /** 设话题自动命名模型（null = 跟随默认模型；话题命名一般用小模型）。 */
  setAutoNamingModel: (model: { providerId: string; model: string } | null) => Promise<void>;
  /** 设应用级界面基础字号（undefined = 跟随默认 18px，写 global.json）。 */
  setFontSize: (size: number | undefined) => Promise<void>;
  /** 设应用级界面字体（undefined = 跟随系统默认，写 global.json）。 */
  setFontFamily: (family: string | undefined) => Promise<void>;
  /** 切换激活的主题插件（应用级，写 global.json）。 */
  setThemePlugin: (pluginId: string) => Promise<void>;
  /** 写激活/指定主题插件的设置项值（应用级，写 global.json；value = undefined 删除键恢复默认）。
   * 预置键：colorMode（内置深浅模式）/ accentColor（强调色）；其余为插件自定义键。 */
  setThemeSetting: (pluginId: string, key: string, value: unknown) => Promise<void>;
  /** 文件面板排序方式（仓库级）。 */
  setFileExplorerSort: (sortKey: FileExplorerSortKey) => Promise<void>;
  /** 设置文件面板排除的文件夹名列表（仓库级；空数组 = 无排除）。 */
  setExcludeFolders: (folders: string[]) => Promise<void>;
  /** 设置附件导入默认文件夹（仓库级；undefined = 仓库根目录）。 */
  setAttachmentFolder: (folder: string | undefined) => Promise<void>;
  /** 设置宽松换行（仓库级，缺省 true）。 */
  setSoftLineBreak: (enabled: boolean) => Promise<void>;
  /** 设置进入仓库时是否自动恢复上次打开的文件（应用级；缺省 true = 开启，写 global.json）。 */
  setAutoRestoreFiles: (enabled: boolean) => Promise<void>;
  /** 设置进入仓库时是否自动切到「主页」布局（应用级；缺省 false = 保持恢复上次界面，写 global.json）。 */
  setDefaultHomeLayout: (enabled: boolean) => Promise<void>;
  /** 更新协作配置（应用级）：内存 + global.json 落盘 + 重建 relay 连接。 */
  setCollabConfig: (
    patch: Partial<
      Pick<SettingsState, "collabEnabled" | "collabRelayUrl" | "collabNickname" | "collabColor">
    >,
  ) => Promise<void>;
  /** 注册/注销系统提示词笔记（数组含该路径则移除，否则添加；写 .atelyx/prompt-notes.json）。 */
  togglePromptNote: (file: string) => Promise<void>;
  /** 笔记重命名/移动后同步标记路径（oldFile → newFile，写 .atelyx/prompt-notes.json）。 */
  remapPromptNote: (oldFile: string, newFile: string) => Promise<void>;
  /** 文件夹重命名后同步标记路径（`oldDir/` 前缀 → `newDir/`，写 .atelyx/prompt-notes.json）。 */
  remapPromptNotesByDir: (oldDir: string, newDir: string) => Promise<void>;
  /** 新建 Agent（默认名「新 Agent」+ 全工具勾选），返回新 id；写 .atelyx/agents.json。 */
  addAgent: () => Promise<string>;
  /** 更新 Agent（merge patch；写 .atelyx/agents.json）。 */
  updateAgent: (id: string, patch: Partial<AgentConfig>) => Promise<void>;
  /** 删除 Agent（写 .atelyx/agents.json；引用它的节点/会话发送时降级为普通对话）。 */
  removeAgent: (id: string) => Promise<void>;
  /** 复制 Agent（新 id + 名称加「副本」；写 .atelyx/agents.json）。 */
  duplicateAgent: (id: string) => Promise<void>;
  /**
   * 解析 Agent 发送请求（画布/面板共用）：系统提示词（引用已注册提示词笔记实时读正文）+ 工具组装。
   * Agent 不存在返回 null；笔记缺失降级为不带系统提示词；tools 空 = 纯对话（无任何工具）。
   */
  resolveAgentRequest: (
    agentId: string | undefined,
  ) => Promise<{ systemPrompt?: string; tools: ToolSchema[]; skippedWebSearch: boolean } | null>;
  /** 笔记重命名/移动后同步 Agent 引用的提示词笔记路径（写 .atelyx/agents.json）。 */
  remapAgentPromptNote: (oldFile: string, newFile: string) => Promise<void>;
  /** 文件夹重命名后同步 Agent 引用的提示词笔记路径前缀（写 .atelyx/agents.json）。 */
  remapAgentPromptNotesByDir: (oldDir: string, newDir: string) => Promise<void>;
  /** 设置文件夹图标颜色（dir = 相对仓库根路径，color = hex 色；undefined = 清除还原默认，写 .atelyx/folder-colors.json）。 */
  setFolderColor: (dir: string, color: string | undefined) => Promise<void>;
  /** 文件夹重命名/移动后同步颜色键（`oldDir/` 前缀 → `newDir/`，写 .atelyx/folder-colors.json）。 */
  remapFolderColorsByDir: (oldDir: string, newDir: string) => Promise<void>;
  /** 立即落盘当前配置（关窗/切仓库前 flush，防 debounce 窗口内丢设置）。 */
  flush: () => Promise<void>;
}

/** 运行时 ProviderConfig → 磁盘 GlobalProvider（syncKeys 开时带 apiKey 落盘，关时剥离；旧 model 字段一并剥离）。 */
function toGlobalProvider(p: ProviderConfig, syncKeys: boolean): GlobalProvider {
  return {
    id: p.id,
    name: p.name,
    baseUrl: p.baseUrl,
    models: p.models,
    ...(syncKeys && p.apiKey ? { apiKey: p.apiKey } : {}),
  };
}

/** 当前仓库稳定 ID（keychain 条目按仓库隔离用；设置入口只在工作区，必有当前仓库）。 */
function currentVaultId(): string {
  return useAppStore.getState().vaultId ?? "";
}

/** 写 keychain：有 key 则存、无 key 则删旧条目（清空 key 时真正移除，防止 loadVaultConfig 把旧 key 读回）。 */
async function persistKeys(vaultId: string, providers: ProviderConfig[]): Promise<void> {
  await Promise.all(
    providers.map((p) =>
      p.apiKey
        ? setApiKey(vaultId, p.id, p.apiKey).catch((e) => console.error("keychain 写入失败", p.id, e))
        : deleteApiKey(vaultId, p.id).catch((e) => console.error("keychain 删除失败", p.id, e)),
    ),
  );
}

/** 全量持久化：合并进仓库级配置写 `.atelyx/config.json` + 按 syncKeys 决定 key 落盘
 * （开 = key 随 config.json 落盘多设备同步；关 = key 写 keychain 按仓库隔离）。 */
async function persist(cfg: AiConfig): Promise<void> {
  // 捕获仓库快照：persist 是异步的（debounce 自动触发），期间用户可能已切换仓库，
  // 防 A 仓库的配置覆盖 B 仓库 config.json、A 的 key 写进 B 的 keychain 条目
  const vaultId = currentVaultId();
  if (!vaultId) return;
  const base = useSettingsStore.getState().vaultConfig ?? {};
  const syncKeys = !!base.syncKeys;
  const vc: VaultConfig = {
    ...base,
    providers: cfg.providers.length ? cfg.providers.map((p) => toGlobalProvider(p, syncKeys)) : undefined,
  };
  // 写盘前同步校验仓库未变；已切换则丢弃本次持久化（loadVaultConfig 已加载新仓库）
  if (currentVaultId() !== vaultId) return;
  useSettingsStore.setState({ vaultConfig: vc });
  await writeVaultConfig(cleanVaultConfig(vc));
  // syncKeys 开启时 key 已随 config.json 落盘，无需再写 keychain
  if (!syncKeys) await persistKeys(vaultId, cfg.providers);
}

// 输入框键击高频，debounce 后落盘避免每键一次 IPC + keychain 写入
/** 防抖持久化控制器：timer 管理统一在此（400ms）。 */
const persistCtl = createPersistController({
  persist: async () => {
    await persist(useSettingsStore.getState().config).catch((e) =>
      console.error("保存 AI 配置失败", e),
    );
  },
  delay: 400,
});

function persistDebounced(): void {
  persistCtl.schedule();
}

/** 写仓库级配置前剔除 undefined，保持 .atelyx/config.json 干净（providers 空数组不落盘；
 * 主题/强调色/字号/字体/自动恢复为应用级，不在此承载）。 */
function cleanVaultConfig(vc: VaultConfig): VaultConfig {
  const out: VaultConfig = {};
  if (vc.model !== undefined) out.model = vc.model;
  if (vc.modelProviderId !== undefined) out.modelProviderId = vc.modelProviderId;
  if (vc.fileExplorerSort !== undefined) out.fileExplorerSort = vc.fileExplorerSort;
  if (vc.excludeFolders !== undefined) out.excludeFolders = vc.excludeFolders;
  if (vc.attachmentFolder !== undefined) out.attachmentFolder = vc.attachmentFolder;
  if (vc.softLineBreak !== undefined) out.softLineBreak = vc.softLineBreak;
  if (vc.autoNamingEnabled !== undefined) out.autoNamingEnabled = vc.autoNamingEnabled;
  if (vc.autoNamingModel !== undefined) out.autoNamingModel = vc.autoNamingModel;
  // 仓库级 AI 配置（无 key）与仓库稳定 ID 必须保留：否则写盘后供应商/搜索源丢失，
  // vaultId 消失会让下次 open_vault 重新生成 ID、keychain key 失配
  if (vc.providers !== undefined) out.providers = vc.providers;
  if (vc.search !== undefined) out.search = vc.search;
  if (vc.syncKeys !== undefined) out.syncKeys = vc.syncKeys;
  if (vc.vaultId !== undefined) out.vaultId = vc.vaultId;
  // temperature / defaultProviderId 字段不写回（配置中不再承载）
  return out;
}

/** 仓库级配置写盘统一入口（各 setXxx 收敛于此）：merge patch → 更新内存 → 清理敏感字段 → 落盘。
 * 失败仅记日志不打断 UI（配置丢失可重设，非关键路径）。 */
async function commitVault(patch: Partial<VaultConfig>): Promise<void> {
  const vc: VaultConfig = { ...(useSettingsStore.getState().vaultConfig ?? {}), ...patch };
  useSettingsStore.setState({ vaultConfig: vc });
  try {
    await writeVaultConfig(cleanVaultConfig(vc));
  } catch (e) {
    console.error("保存仓库级配置失败", e);
  }
}

/** 应用级配置写盘统一入口（各外观 setXxx 收敛于此）：先写内存再 patch 落 global.json，
 * 失败仅记专属文案日志不打断 UI（外观丢失可重设，非关键路径）。 */
async function commitGlobal(patch: Partial<GlobalConfig>, errMsg: string): Promise<void> {
  useSettingsStore.setState(patch);
  try {
    await updateGlobalConfig(patch);
  } catch (e) {
    console.error(errMsg, e);
  }
}

/**
 * 反查 model 所属供应商：已固定供应商（preferredProviderId）仍含该模型则用之；
 * 固定供应商缺失/不含该模型 = 判定失效返回 undefined（**不跨供应商静默替换同名模型**——
 * 否则会把固定给 A 的默认模型无声切到 B 的同名模型，重蹈混用；设置页此时以「已失效」提示重选）。
 * 仅旧配置（无固定供应商）回退按模型名反查首个命中。
 */
function findProviderByModel(
  providers: ProviderConfig[],
  modelId: string,
  preferredProviderId?: string,
): ProviderConfig | undefined {
  if (preferredProviderId) {
    const pinned = providers.find((p) => p.id === preferredProviderId);
    return pinned && pinned.models.some((m) => m.id === modelId) ? pinned : undefined;
  }
  return providers.find((p) => p.models.some((m) => m.id === modelId));
}

export const useSettingsStore = create<SettingsState>((set, get) => ({
  config: DEFAULT_AI_CONFIG,
  theme: BUILTIN_THEME_PLUGIN_ID,
  themeSettings: { [BUILTIN_THEME_PLUGIN_ID]: { ...DEFAULT_BUILTIN_THEME_SETTINGS } },
  fontSize: undefined,
  fontFamily: undefined,
  autoRestoreFiles: true,
  defaultHomeLayout: false,
  collabEnabled: false,
  collabRelayUrl: "",
  collabNickname: "",
  collabColor: "",
  deviceName: "",
  vaultConfig: null,
  searchConfig: { provider: "tavily", searxngUrl: "" },
  tavilyKey: "",
  promptNotes: [],
  agents: [],
  folderColors: {},
  loaded: false,

  load: async () => {
    // 应用级外观（主题/强调色/字号/字体/自动恢复）从 global.json 读一次，跨仓库共享；
    // 仓库级配置（供应商/搜索源等）无仓库上下文时为空，进入仓库后由 loadVaultConfig 填充
    let theme: string = BUILTIN_THEME_PLUGIN_ID;
    let themeSettings: Record<string, Record<string, unknown>> = {
      [BUILTIN_THEME_PLUGIN_ID]: { ...DEFAULT_BUILTIN_THEME_SETTINGS },
    };
    let fontSize: number | undefined;
    let fontFamily: string | undefined;
    let autoRestoreFiles = true;
    let defaultHomeLayout = false;
    let collabEnabled = false;
    let collabRelayUrl = "";
    let collabNickname = "";
    let collabColor = "";
    let deviceName = "";
    try {
      const cfg = await readGlobalConfig();
      // 主题配置归一化（旧三态/旧全局强调色迁移 + 磁盘优先）为纯函数，见 utils/pluginTheme
      const normalized = normalizeThemeConfig(cfg);
      theme = normalized.theme;
      themeSettings = normalized.themeSettings;
      fontSize = cfg.fontSize;
      fontFamily = cfg.fontFamily;
      autoRestoreFiles = cfg.autoRestoreFiles ?? true;
      defaultHomeLayout = cfg.defaultHomeLayout ?? false;
      collabEnabled = cfg.collabEnabled ?? false;
      collabRelayUrl = cfg.collabRelayUrl ?? "";
      collabNickname = cfg.collabNickname ?? "";
      collabColor = cfg.collabColor ?? "";
    } catch (e) {
      console.error("读取外观配置失败", e);
    }
    try {
      // 设备名独立读（配置读取失败不连带丢失协作身份兜底）
      deviceName = await getHostname();
    } catch (e) {
      console.error("读取设备名失败", e);
    }
    set({
      config: DEFAULT_AI_CONFIG,
      theme,
      themeSettings,
      fontSize,
      fontFamily,
      autoRestoreFiles,
      defaultHomeLayout,
      collabEnabled,
      collabRelayUrl,
      collabNickname,
      collabColor,
      deviceName,
      searchConfig: { provider: "tavily", searxngUrl: "" },
      tavilyKey: "",
      promptNotes: [],
      agents: [],
      loaded: true,
    });
  },

  loadVaultConfig: async () => {
    try {
      const vc = await readVaultConfig();
      const vaultId = currentVaultId();
      // 仓库级 AI 供应商：syncKeys 开 = key 随仓库落盘直读 config（多设备共享）；关 = 从 keychain 按仓库隔离填充
      const providers = await Promise.all(
        (vc.providers ?? []).map(async (p): Promise<ProviderConfig> => {
          let apiKey = "";
          if (vc.syncKeys) {
            apiKey = p.apiKey ?? "";
          } else {
            try {
              apiKey = await getApiKey(vaultId, p.id);
            } catch (e) {
              console.error("keychain 读取失败", p.id, e);
            }
          }
          return { id: p.id, name: p.name, baseUrl: p.baseUrl, models: p.models ?? [], apiKey };
        }),
      );
      const config: AiConfig = { providers };
      // 搜索源：仓库级配置 + Tavily key（syncKeys 开 = 直读 config 内 tavilyApiKey；关 = keychain 条目 provider-<vaultId>-search-tavily）
      let searchConfig: GlobalSearchConfig = { provider: "tavily", searxngUrl: "" };
      let tavilyKey = "";
      if (vc.search) searchConfig = vc.search;
      if (vc.syncKeys) {
        tavilyKey = searchConfig.tavilyApiKey ?? "";
      } else {
        try {
          tavilyKey = await getApiKey(vaultId, "search-tavily");
        } catch (e) {
          console.error("keychain 读取失败 search-tavily", e);
        }
      }
      // 系统提示词标记独立落盘 .atelyx/prompt-notes.json（config.json 只存仓库配置）
      let promptNotes: string[] = [];
      try {
        promptNotes = await readPromptNotes();
      } catch (e) {
        console.error("读取系统提示词标记失败", e);
      }
      // Agent 配置独立落盘 .atelyx/agents.json（config.json 只存仓库配置）；
      // 预置 Agent（builtin 不可删）缺失即补入并落盘（首次种子/手删补齐），保证默认必现
      let agents: AgentConfig[] = [];
      try {
        agents = await readAgents();
      } catch (e) {
        console.error("读取 Agent 配置失败", e);
      }
      const missingBuiltins = BUILTIN_AGENTS.filter(
        (b) => !agents.some((a) => a.id === b.id),
      );
      if (missingBuiltins.length) {
        agents = [...missingBuiltins, ...agents];
        // 首次种子/补齐预置：await 落盘（防迟到的写盘用旧列表覆盖用户刚做的增改）
        await writeAgents(agents).catch((e) => console.error("写入预置 Agent 失败", e));
      }
      // 文件夹图标颜色独立落盘 .atelyx/folder-colors.json（config.json 只存仓库配置）
      let folderColors: Record<string, string> = {};
      try {
        folderColors = await readFolderColors();
      } catch (e) {
        console.error("读取文件夹图标颜色失败", e);
      }
      set({
        vaultConfig: vc,
        config,
        searchConfig,
        tavilyKey,
        promptNotes,
        agents,
        folderColors,
      });
    } catch (e) {
      console.error("读取仓库级配置失败", e);
    }
  },

  clearVaultConfig: () =>
    set({
      vaultConfig: null,
      config: DEFAULT_AI_CONFIG,
      searchConfig: { provider: "tavily", searxngUrl: "" },
      tavilyKey: "",
      promptNotes: [],
      agents: [],
      folderColors: {},
    }),

  // 话题自动命名模型解析：设置页指定（autoNamingModel）→ 仓库默认模型（vaultConfig.model）；
  // 未配置视为不启用（缺省关闭），显式开启（autoNamingEnabled === true）才命名（画布/面板自动命名共用，一次定义）；
  // ignoreToggle = 重新命名（用户显式请求，独立于自动命名开关）。
  resolveAutoNamingModel: (ignoreToggle) => {
    const s = get();
    const vault = s.vaultConfig;
    if (vault?.autoNamingEnabled !== true && !ignoreToggle) return null;
    const named = vault?.autoNamingModel;
    const modelId = named?.model || vault?.model;
    if (!modelId) return null;
    // 固定供应商：命名指定模型优先 named.providerId；跟随默认模型时用默认模型的固定供应商；
    // 固定供应商失效（供应商被删/模型被移除）= 判定未配置，不跨供应商替换同名模型
    const preferred = named?.providerId || (!named ? vault?.modelProviderId : undefined);
    const provider = findProviderByModel(s.config.providers, modelId, preferred);
    if (!provider) return null;
    return { provider, model: modelId };
  },

  isSearchConfigured: () => {
    const s = get();
    return s.searchConfig.provider === "tavily" ? !!s.tavilyKey : !!s.searchConfig.searxngUrl;
  },

  resolveDefaultModel: () => {
    const { config, vaultConfig } = get();
    const vaultModel = vaultConfig?.model;
    if (!vaultModel) return null;
    // 默认模型可来自任意供应商：优先固定供应商（modelProviderId，重选后落盘）且其仍含该模型则用之，
    // 否则判定未配置；仅旧配置（无固定供应商）按模型名反查
    const owner = findProviderByModel(
      config.providers,
      vaultModel,
      vaultConfig?.modelProviderId,
    );
    if (!owner) return null;
    return { provider: owner, model: vaultModel };
  },

  // 对话请求目标统一解析（画布节点 selection = 节点级指定；面板 selection = modelOverride）。
  // 语义：选定 {providerId, model} 优先（供应商已删报错不静默回落）；未选定 = 跟随仓库默认模型（反查所属供应商），未配置默认模型报错。
  resolveChatTarget: (selection) => {
    const s = get();
    const selected = selection?.providerId
      ? s.config.providers.find((p) => p.id === selection.providerId)
      : undefined;
    if (selection?.providerId && !selected) {
      return {
        ok: false,
        reason: "provider-missing",
        error: "所选供应商已不存在，请重新选择",
      };
    }
    if (!selected) {
      const def = s.resolveDefaultModel();
      if (!def) {
        return {
          ok: false,
          reason: "no-model",
          error: "未配置默认模型：请在设置 → 模型服务中配置默认模型，或在本节点选择模型",
        };
      }
      return {
        ok: true,
        provider: def.provider,
        model: def.model,
      };
    }
    const model = selection?.model ?? selected.models[0]?.id ?? "";
    if (!model) {
      return {
        ok: false,
        reason: "no-model",
        error: "未指定模型：请选择模型，或在设置中设置默认模型",
      };
    }
    return {
      ok: true,
      provider: selected,
      model,
    };
  },

  addProvider: async (preset) => {
    const id = crypto.randomUUID();
    const provider: ProviderConfig = {
      id,
      name: preset?.name ?? "自定义",
      baseUrl: preset?.baseUrl ?? "",
      apiKey: "",
      models: preset?.models ?? [],
    };
    const cfg = {
      providers: [...get().config.providers, provider],
    };
    set({ config: cfg });
    // 与其余写路径统一走防抖（400ms；关窗/切仓库前 flush 兜底 await），
    // 避免即时写与防抖写两条路径并发交错写 config.json
    persistDebounced();
    return id;
  },

  fetchProviderModelIds: async (id) => {
    const p = get().config.providers.find((x) => x.id === id);
    if (!p) throw new Error("供应商不存在");
    return fetchProviderModels(p.baseUrl, p.apiKey);
  },

  updateProvider: async (id, patch) => {
    const providers = get().config.providers.map((p) =>
      p.id === id ? { ...p, ...patch } : p,
    );
    const cfg = { ...get().config, providers };
    set({ config: cfg });
    persistDebounced();
  },

  flush: async () => {
    await persistCtl.flush();
  },

  removeProvider: async (id) => {
    const providers = get().config.providers.filter((p) => p.id !== id);
    const cfg = { providers };
    set({ config: cfg });
    await persist(cfg);
    // 删 keychain 条目（best-effort；key 按仓库隔离，无条件删）
    deleteApiKey(currentVaultId(), id).catch((e) => console.error("删除 keychain 条目失败", id, e));
  },

  setVaultModel: async (model) => {
    await commitVault(
      model
        ? { model: model.model, modelProviderId: model.providerId || undefined }
        : { model: undefined, modelProviderId: undefined },
    );
  },

  setAutoNamingEnabled: async (enabled) => {
    await commitVault({ autoNamingEnabled: enabled });
  },

  setAutoNamingModel: async (model) => {
    await commitVault({ autoNamingModel: model ?? undefined });
  },

  setFontSize: (size) => commitGlobal({ fontSize: size }, "保存字号配置失败"),

  setFontFamily: (family) => commitGlobal({ fontFamily: family }, "保存字体配置失败"),

  /** 切换激活的主题插件（应用级，写 global.json）。 */
  setThemePlugin: (pluginId) => commitGlobal({ theme: pluginId }, "保存主题配置失败"),

  /** 写主题插件设置项值（应用级，写 global.json；value = undefined 删除键恢复默认）。 */
  setThemeSetting: async (pluginId, key, value) => {
    const current = get().themeSettings;
    const entry = { ...(current[pluginId] ?? {}) };
    if (value === undefined) delete entry[key];
    else entry[key] = value;
    await commitGlobal({ themeSettings: { ...current, [pluginId]: entry } }, "保存主题设置失败");
  },

  setFileExplorerSort: async (sortKey) => {
    await commitVault({ fileExplorerSort: sortKey });
  },

  setExcludeFolders: async (folders) => {
    // 空数组不落盘（缺省 = 无排除，保持 config.json 干净）
    await commitVault({ excludeFolders: folders.length ? folders : undefined });
  },

  setSoftLineBreak: async (enabled) => {
    await commitVault({ softLineBreak: enabled });
  },

  setAutoRestoreFiles: (enabled) =>
    commitGlobal({ autoRestoreFiles: enabled }, "保存自动恢复配置失败"),

  setDefaultHomeLayout: (enabled) =>
    commitGlobal({ defaultHomeLayout: enabled }, "保存主页默认布局配置失败"),

  setCollabConfig: async (patch) => {
    set(patch);
    try {
      await updateGlobalConfig({
        collabEnabled: get().collabEnabled || undefined,
        collabRelayUrl: get().collabRelayUrl || undefined,
        collabNickname: get().collabNickname || undefined,
        collabColor: get().collabColor || undefined,
      });
      // 配置变更即时生效：重建 relay 连接（开关/地址/身份变化）
      useCollabStore.getState().applyConfig({
        enabled: get().collabEnabled,
        url: get().collabRelayUrl,
        nickname: get().collabNickname,
        color: get().collabColor,
      });
    } catch (e) {
      console.error("保存协作配置失败", e);
    }
  },

  setAttachmentFolder: async (folder) => {
    await commitVault({ attachmentFolder: folder || undefined });
  },

  /** 注册/注销系统提示词笔记：数组含该路径则移除，否则添加（空数组也落盘保持文件干净，独立于 config.json）。 */
  togglePromptNote: async (file) => {
    const marked = get().promptNotes.includes(file);
    const next = marked
      ? get().promptNotes.filter((f) => f !== file)
      : [...get().promptNotes, file];
    set({ promptNotes: next });
    try {
      await writePromptNotes(next);
    } catch (e) {
      console.error("保存系统提示词标记失败", e);
    }
  },

  /** 笔记重命名/移动后同步标记路径（旧路径未标记时 no-op）。 */
  remapPromptNote: async (oldFile, newFile) => {
    const marked = get().promptNotes;
    if (!marked.includes(oldFile)) return;
    const next = marked.map((f) => (f === oldFile ? newFile : f));
    set({ promptNotes: next });
    try {
      await writePromptNotes(next);
    } catch (e) {
      console.error("保存系统提示词标记失败", e);
    }
  },

  /** 文件夹重命名后同步标记路径（`oldDir/` 前缀命中才更新）。 */
  remapPromptNotesByDir: async (oldDir, newDir) => {
    const marked = get().promptNotes;
    const next = marked.map((f) => remapDirPrefix(f, oldDir, newDir));
    if (next.every((f, i) => f === marked[i])) return;
    set({ promptNotes: next });
    try {
      await writePromptNotes(next);
    } catch (e) {
      console.error("保存系统提示词标记失败", e);
    }
  },

  /** Agent 配置落盘统一入口（各 CRUD 收敛于此；失败仅记日志不打断 UI）。 */
  addAgent: async () => {
    const id = crypto.randomUUID();
    const agent: AgentConfig = {
      id,
      name: "新 Agent",
      tools: [...DEFAULT_AGENT_TOOLS],
    };
    set({ agents: [...get().agents, agent] });
    try {
      await writeAgents(get().agents);
    } catch (e) {
      console.error("保存 Agent 配置失败", e);
    }
    return id;
  },

  updateAgent: async (id, patch) => {
    const next = get().agents.map((a) => (a.id === id ? { ...a, ...patch } : a));
    set({ agents: next });
    try {
      await writeAgents(next);
    } catch (e) {
      console.error("保存 Agent 配置失败", e);
    }
  },

  removeAgent: async (id) => {
    // 预置 Agent 不可删除（builtin 标记；UI 已隐藏删除按钮，此处兜底防误删）
    const target = get().agents.find((a) => a.id === id);
    if (target?.builtin) return;
    const next = get().agents.filter((a) => a.id !== id);
    set({ agents: next });
    try {
      await writeAgents(next);
    } catch (e) {
      console.error("保存 Agent 配置失败", e);
    }
  },

  duplicateAgent: async (id) => {
    const src = get().agents.find((a) => a.id === id);
    if (!src) return;
    const copy: AgentConfig = {
      ...src,
      id: crypto.randomUUID(),
      name: `${src.name}（副本）`,
      // 副本是普通用户 Agent（可删除），不继承预置标记
      builtin: undefined,
    };
    set({ agents: [...get().agents, copy] });
    try {
      await writeAgents(get().agents);
    } catch (e) {
      console.error("保存 Agent 配置失败", e);
    }
  },

  resolveAgentRequest: async (agentId) => {
    // 按 id 查找 Agent：空 id = 缺省解析为预置「对话」（对话节点/面板不显式选择时的默认行为）；
    // 未找到返回 null（发送时降级为普通对话）
    const agent = agentId
      ? (get().agents.find((a) => a.id === agentId) ?? null)
      : (get().agents.find((a) => a.id === BUILTIN_AGENT_CHAT_ID) ?? BUILTIN_AGENTS[0] ?? null);
    if (!agent) return null;
    // 系统提示词：引用已注册提示词笔记实时读正文（外部编辑即时生效，读失败降级）
    let systemPrompt: string | undefined;
    if (agent.systemPromptFile) {
      try {
        const sysContent = await readNote(agent.systemPromptFile);
        if (sysContent.trim()) systemPrompt = sysContent;
      } catch {
        // 笔记缺失：跳过注入
      }
    }
    // 工具组装：全部工具按 agent.tools 勾选生效——勾选即赋予、取消即移除，
    // web_search 勾选但未配置搜索源时剔除并提示（预置「对话」除外：其 web_search 缺省自带，
    // 未配置源时静默剔除不弹横幅，设置页工具区仍显示「未配置搜索源」角标；用户显式勾选搜索的 Agent 保持提示）
    const s = get();
    const searchReady = s.isSearchConfigured();
    const assembly = buildAgentTools(agent.tools, searchReady);
    const tools = assembly.tools;
    const skippedWebSearch = assembly.skippedWebSearch && agent.id !== BUILTIN_AGENT_CHAT_ID;
    return { systemPrompt, tools, skippedWebSearch };
  },

  /** 笔记重命名/移动后同步 Agent 引用的提示词笔记路径（旧路径未被引用时 no-op）。 */
  remapAgentPromptNote: async (oldFile, newFile) => {
    const agents = get().agents;
    if (!agents.some((a) => a.systemPromptFile === oldFile)) return;
    const next = agents.map((a) =>
      a.systemPromptFile === oldFile ? { ...a, systemPromptFile: newFile } : a,
    );
    set({ agents: next });
    try {
      await writeAgents(next);
    } catch (e) {
      console.error("保存 Agent 配置失败", e);
    }
  },

  /** 文件夹重命名后同步 Agent 引用的提示词笔记路径前缀（`oldDir/` 前缀命中才更新）。 */
  remapAgentPromptNotesByDir: async (oldDir, newDir) => {
    const agents = get().agents;
    let changed = false;
    const next = agents.map((a) => {
      if (a.systemPromptFile && a.systemPromptFile.startsWith(`${oldDir}/`)) {
        changed = true;
        return {
          ...a,
          systemPromptFile: remapDirPrefix(a.systemPromptFile, oldDir, newDir),
        };
      }
      return a;
    });
    if (!changed) return;
    set({ agents: next });
    try {
      await writeAgents(next);
    } catch (e) {
      console.error("保存 Agent 配置失败", e);
    }
  },

  /** 设文件夹图标颜色（dir = 相对仓库根路径；color = hex 色，undefined = 清除还原默认；空映射也落盘保持文件干净）。 */
  setFolderColor: async (dir, color) => {
    const cur = get().folderColors;
    const next = { ...cur };
    if (color) next[dir] = color;
    else delete next[dir];
    set({ folderColors: next });
    try {
      await writeFolderColors(next);
    } catch (e) {
      console.error("保存文件夹图标颜色失败", e);
    }
  },

  /** 文件夹重命名/移动后同步颜色键（`oldDir/` 前缀命中才更新）。 */
  remapFolderColorsByDir: async (oldDir, newDir) => {
    const cur = get().folderColors;
    const keys = Object.keys(cur);
    const next: Record<string, string> = {};
    let changed = false;
    for (const k of keys) {
      if (k === oldDir || k.startsWith(`${oldDir}/`)) {
        next[remapDirPrefix(k, oldDir, newDir)] = cur[k];
        changed = true;
      } else {
        next[k] = cur[k];
      }
    }
    if (!changed) return;
    set({ folderColors: next });
    try {
      await writeFolderColors(next);
    } catch (e) {
      console.error("保存文件夹图标颜色失败", e);
    }
  },

  setSearchConfig: async (patch) => {
    const next = { ...get().searchConfig, ...patch };
    set({ searchConfig: next });
    await commitVault({ search: next });
  },

  setTavilyKey: async (key) => {
    set({ tavilyKey: key });
    const vaultId = currentVaultId();
    try {
      if (get().vaultConfig?.syncKeys) {
        // syncKeys 开启：Tavily key 随仓库落盘（search.tavilyApiKey，空串剥离）
        const searchConfig = { ...get().searchConfig, tavilyApiKey: key || undefined };
        const vc: VaultConfig = { ...(get().vaultConfig ?? {}), search: searchConfig };
        set({ searchConfig, vaultConfig: vc });
        await writeVaultConfig(cleanVaultConfig(vc));
      } else if (key) {
        // 默认：key 走 keychain（条目 `provider-<vaultId>-search-tavily`，与 provider key 区分）；空串删除条目
        await setApiKey(vaultId, "search-tavily", key);
      } else {
        await deleteApiKey(vaultId, "search-tavily");
      }
    } catch (e) {
      console.error("保存 Tavily key 失败", e);
    }
  },

  /** 开关「API key 随仓库保存」（多设备同步）：开启 = 当前 key 全量写入 config.json；
   * 关闭 = 剥离 config 内 key + 回写 keychain（覆盖旧条目，防开启期间 key 变更丢同步）。 */
  setSyncKeys: async (enabled) => {
    const base = get().vaultConfig ?? {};
    const { config, searchConfig, tavilyKey } = get();
    // searchConfig 必须与磁盘同步更新：开 = 带 tavilyApiKey（后续 setSearchConfig 写回不丢 key），
    // 关 = 剥离（防残留 key 被 setSearchConfig 重新写回 config.json，破坏「关闭 = 剥离」语义）
    const hasSearch = base.search !== undefined || searchConfig.tavilyApiKey !== undefined;
    const nextSearch: GlobalSearchConfig | undefined = enabled
      ? { ...searchConfig, tavilyApiKey: tavilyKey || undefined }
      : hasSearch
        ? { provider: searchConfig.provider, searxngUrl: searchConfig.searxngUrl }
        : undefined;
    const vc: VaultConfig = {
      ...base,
      syncKeys: enabled,
      // 开启：key 随供应商/搜索配置落盘；关闭：剥离 key 字段（key 由下方回写 keychain）
      providers: config.providers.length
        ? config.providers.map((p) => toGlobalProvider(p, enabled))
        : base.providers,
      search: nextSearch,
    };
    set({ vaultConfig: vc, searchConfig: nextSearch ?? { provider: "tavily", searxngUrl: "" } });
    try {
      await writeVaultConfig(cleanVaultConfig(vc));
    } catch (e) {
      console.error("保存仓库级配置失败", e);
    }
    if (!enabled) {
      // 关闭：key 从 config 剥离后回写 keychain（best-effort）
      const vaultId = currentVaultId();
      await persistKeys(vaultId, config.providers);
      try {
        if (tavilyKey) await setApiKey(vaultId, "search-tavily", tavilyKey);
        else await deleteApiKey(vaultId, "search-tavily");
      } catch (e) {
        console.error("保存 Tavily key 失败", e);
      }
    }
  },
}));

/** 默认生效模型显示名 selector（对话节点与 AI 对话面板共用）：跟随仓库默认时显示真实生效模型名
 * （resolveDefaultModel 固定供应商解析 + 跨供应商同名前缀消歧）；未配置默认模型 = null。
 * 返回原始串，Zustand Object.is 比较保证仅在值变时重渲染。 */
export function selectDefaultModelDisplay(s: SettingsState): string | null {
  const def = s.resolveDefaultModel();
  return def ? modelDisplayLabel(s.config.providers, def.provider, def.model) : null;
}
