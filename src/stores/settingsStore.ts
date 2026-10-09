import { registerVaultSwitchActions } from "@/utils/kernelLifecycle";
import { create } from "zustand";
import { getApiKey, setApiKey, deleteApiKey } from "@/services/keychain";
import {
  readAgents,
  readFolderColors,
  readVaultConfig,
  patchVaultConfig,
  writeAgents,
  writeFolderColors,
  writePromptNotes,
  readPromptNotes,
} from "@/services/metadata";
import { fileExists, readNote } from "@/services/vault";
import { fetchProviderModels } from "@/services/ai/client";
import { buildAgentTools } from "@/services/ai/tools";
import { getHostname, readGlobalConfig, updateGlobalConfig } from "@/services/global";
import { isAutoLaunchEnabled, setAutoLaunchEnabled } from "@/services/autostart";
import { useAppStore } from "@/stores/appStore";
import { useCollabStore } from "@/stores/collabStore";
import { useSpaceDirectoryStore } from "@/stores/spaceDirectoryStore";
import { useNotificationStore } from "@/stores/notificationStore";
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
import { BUILTIN_THEME_PLUGIN_ID, DEFAULT_BUILTIN_THEME_SETTINGS } from "@/utils/pluginTheme";
import { DEFAULT_AI_CONFIG } from "@/constants/ai";
import { DEFAULT_AGENT_TOOLS } from "@/constants/tools";
import { BUILTIN_AGENTS, BUILTIN_AGENT_CHAT_ID } from "@/constants/agents";
import { PROVIDER_PRESETS } from "@/constants/providers";
import {
  NOTE_LINE_WIDTH_DEFAULT,
  NOTE_LINE_WIDTH_MAX,
  NOTE_LINE_WIDTH_MIN,
} from "@/constants/notes";
import { remapDirKey, remapDirPrefix } from "@/utils/filename";
import { modelDisplayLabel } from "@/utils/text";
import { createPersistController } from "@/utils/persist";
import { applyShortcutOverride } from "@/services/cordis/shortcutOverrides";
import { getKernel } from "@/services/cordis/kernel";
import { listGlobalShortcuts, type GlobalShortcutRegistration } from "@/services/globalShortcut";

/**
 * 设置 store（供应商/搜索源等仓库化，界面外观应用级）。
 * 仓库级配置按激活仓库身份双源分发（读写都经 services/metadata）：个人仓库 = `.atelyx/config.json` +
 * 独立文件（prompt-notes/agents/folder-colors），API key 默认走 keychain（按仓库身份哈希隔离），
 * `syncKeys` 开启后 key 明文随 config.json 落盘；协作空间 = AI 配置与 key 整份落服务端团队元数据
 * （按字段分键、全员共用一份，写权限服务端按角色裁决），不使用本机 keychain。
 * 应用级配置（`app_data_dir/global.json`）：主题/强调色/字号/字体/自动恢复，跨仓库共享。
 */

interface SettingsState {
  /** 运行时 AI 配置（providers 含 key，从 keychain 填充）。 */
  config: AiConfig;
  /** 激活的主题插件 id（应用级，写 global.json；缺省 = 默认主题插件，其深浅模式默认跟随系统）。 */
  theme: string;
  /** 各主题插件的设置项值字典（应用级，写 global.json；预置键 colorMode/accentColor + 插件自定义键）。 */
  themeSettings: Record<string, Record<string, unknown>>;
  /** 应用级界面基础字号（px；undefined = 默认 16，存 global.json）。 */
  fontSize?: number;
  /** 应用级界面字体（CSS font-family；undefined = 系统默认，存 global.json）。 */
  fontFamily?: string;
  /** 进入仓库时自动恢复上次打开的文件（应用级，存 global.json；缺省 true = 开启）。 */
  autoRestoreFiles: boolean;
  /** 系统启动项是否已注册本应用（应用级但不落配置：真相源为系统启动项本身，实时读取）。 */
  autoLaunch: boolean;
  /** 宽松换行（应用级显示偏好，存 global.json；缺省 true = 单个换行渲染为换行）。 */
  softLineBreak: boolean;
  /** 页面内标题（应用级显示偏好，存 global.json；缺省 false = 不显示）。 */
  inlineTitle: boolean;
  /** 笔记正文行宽上限（px，应用级显示偏好，存 global.json；0 = 不限制，缺省 780）。 */
  noteLineWidth: number;
  /** 移动端底部导航栏的视图顺序（应用级，存 global.json；空 = 内建常用序，见 utils/mobileNav）。 */
  mobileNavOrder: string[];
  /** 协作空间连接开关（应用级，存 global.json；缺省 false = 关闭，作用于协作空间频道）。 */
  collabEnabled: boolean;
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
  /** Tavily API key（运行时，keychain 条目按仓库身份哈希隔离）。 */
  tavilyKey: string;
  /** 已标记为系统提示词的笔记相对路径列表（独立落盘 .atelyx/prompt-notes.json，config.json 不承载）。 */
  promptNotes: string[];
  /** Agent 配置列表（仓库级，独立落盘 .atelyx/agents.json；对话节点/面板按 id 实时引用）。 */
  agents: AgentConfig[];
  /** 文件面板文件夹图标颜色（相对仓库根路径 → hex 色；独立落盘 .atelyx/folder-colors.json）。 */
  folderColors: Record<string, string>;
  /** 命令快捷键的用户覆盖（命令 globalId → 键串；应用级，存 global.json；缺省空 = 全用声明默认键）。 */
  commandShortcuts: Record<string, string>;
  /** 全局快捷键的用户覆盖（`插件id:声明id` → OS accelerator 串；应用级，存 global.json；缺省空 = 全用声明默认键）。 */
  globalShortcuts: Record<string, string>;
  /** 当前 OS 层登记的全部全局快捷键（现查快照，快捷键设置页展示用；读取失败为空数组）。 */
  globalShortcutRegistrations: GlobalShortcutRegistration[];
  /** 登记清单最近一次读取是否失败（空数组可能是「真没有」，此标记区分失败态）。 */
  globalShortcutRegistrationsError: boolean;
  loaded: boolean;

  /** 应用挂载时调用：读 global.json 填充应用级外观（主题/强调色/字号/字体/自动恢复），重置仓库级运行时状态。 */
  load: () => Promise<void>;
  /** 打开仓库后读 `.atelyx/config.json`（AI 供应商/搜索源等仓库级配置）+ keychain 填充 key。由 appStore.selectVault 调用。 */
  loadVaultConfig: () => Promise<void>;
  /** 设搜索源（仓库级，写 .atelyx/config.json）。 */
  setSearchConfig: (patch: Partial<GlobalSearchConfig>) => Promise<void>;
  /** 设 Tavily API key（仓库级；key 随配置落盘时写配置（本地 syncKeys 开 / 空间团队元数据），否则写 keychain；空串删除）。 */
  setTavilyKey: (key: string) => Promise<void>;
  /** 开关「API key 随仓库保存」（多设备同步，仅本地仓库）：开启 = 当前 key 全量写入 config.json；
   *  关闭 = 剥离 config key + 回写 keychain。协作空间恒由服务端团队元数据承载 key，无此开关。 */
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
  /** 设应用级界面基础字号（undefined = 跟随默认 16px，写 global.json）。 */
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
  /** 设置启动仓库时自动切换的场景 id（仓库级；undefined = 不切换，保持上次界面）。 */
  setEntryScene: (sceneId: string | undefined) => Promise<void>;
  /** 设置宽松换行（应用级显示偏好，缺省 true，写 global.json）。 */
  setSoftLineBreak: (enabled: boolean) => Promise<void>;
  /** 设置页面内标题（应用级显示偏好，缺省 false，写 global.json）。 */
  setInlineTitle: (enabled: boolean) => Promise<void>;
  /** 设置笔记正文行宽上限（应用级显示偏好；0 = 不限制，值在设置项里钳制到可设区间，写 global.json）。 */
  setNoteLineWidth: (width: number) => Promise<void>;
  /** 设置移动端底部导航栏的视图顺序（应用级，写 global.json；空数组 = 回到内建常用序）。 */
  setMobileNavOrder: (order: string[]) => Promise<void>;
  /** 设置进入仓库时是否自动恢复上次打开的文件（应用级；缺省 true = 开启，写 global.json）。 */
  setAutoRestoreFiles: (enabled: boolean) => Promise<void>;
  /** 刷新开机自启真实状态（设置面板打开时调用：用户可能已在系统侧改过启动项）。 */
  refreshAutoLaunch: () => Promise<void>;
  /** 开关开机自启（写系统启动项；失败提示并回读系统真实状态，开关不虚报）。 */
  setAutoLaunch: (enabled: boolean) => Promise<void>;
  /** 更新协作配置（应用级）：内存 + global.json 落盘 + 重建协作连接。 */
  setCollabConfig: (
    patch: Partial<Pick<SettingsState, "collabEnabled" | "collabNickname" | "collabColor">>,
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
  /**
   * 清理失效提示词（进入 Agent 设置页时触发）：注册列表与 Agent 引用中指向已不存在笔记的路径一并移除。
   * 存在性按内容面元数据查询判定，查询失败视为存在（不确定不删，防网络抖动误清团队标记）。
   */
  pruneMissingPromptNotes: () => Promise<void>;
  /** 设置文件夹图标颜色（dir = 相对仓库根路径，color = hex 色；undefined = 清除还原默认，写 .atelyx/folder-colors.json）。 */
  setFolderColor: (dir: string, color: string | undefined) => Promise<void>;
  /** 设命令快捷键覆盖（globalId → 键串；undefined = 清除恢复声明默认键，写 global.json）。 */
  setCommandShortcut: (globalId: string, shortcut: string | undefined) => Promise<void>;
  /** 设全局快捷键覆盖（`插件id:声明id` → accelerator 串；undefined = 清除恢复声明默认键，写 global.json）。
   *  返回是否生效：覆盖变更触发的重注册失败（新键被占用）时返回 false 并保留原覆盖。 */
  setGlobalShortcutOverride: (declarationKey: string, accelerator: string | undefined) => Promise<boolean>;
  /** 现查 OS 层全局快捷键登记清单（快捷键设置页进入/改键后刷新用）。 */
  refreshGlobalShortcutRegistrations: () => Promise<void>;
  /** 文件夹重命名/移动后同步颜色键（目录键精确或 `oldDir/` 前缀命中才更新，写 .atelyx/folder-colors.json）。 */
  remapFolderColorsByDir: (oldDir: string, newDir: string) => Promise<void>;
  /**
   * 远端重命名/移动跟随（协作 renamed 帧）：目录前缀类设置（文件夹颜色 / 提示词标记 /
   * Agent 提示词文件）的键随新路径迁移。只更新内存、不写真源——真源已由发起方写好
   * （空间 = 团队 meta，个人仓库无远端帧），本地写盘会与发起方竞争且 viewer 无写权；
   * 键未命中（含发起方回放帧、内存未加载）时为 no-op。
   */
  followRemotePathRename: (oldPath: string, newPath: string) => void;
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

/** 运行时搜索配置 → 磁盘 GlobalSearchConfig（`syncKeys` 关时剥离明文 `tavilyApiKey`，与
 *  `toGlobalProvider` 同口径：非同步模式 key 只存本机 keychain）。 */
function toGlobalSearchConfig(
  search: GlobalSearchConfig | undefined,
  syncKeys: boolean,
): GlobalSearchConfig | undefined {
  if (!search || syncKeys) return search;
  const { tavilyApiKey: _omitted, ...rest } = search;
  return rest;
}

/** 当前仓库身份的 keychain 条目前缀：local = root 绝对路径；space = `space:<serverUrl>#<spaceId>`
 * （Rust 侧对整串取哈希隔离，空间与本地仓库、不同空间互不共条目；设置入口只在工作区，必有身份）。 */
function currentVaultRoot(): string {
  const app = useAppStore.getState();
  const id = app.vaultIdentity;
  if (id?.kind === "space") return `space:${id.serverUrl}#${id.spaceId}`;
  return app.vaultRoot ?? "";
}

/** 写盘守卫键：local = root 绝对路径；space = `space:<serverUrl>#<spaceId>`。
 * 未激活身份（启动早期/测试）按 vaultRoot 兜底，保持 root 守卫语义。 */
function activeGuardKey(): string | null {
  const app = useAppStore.getState();
  const id = app.vaultIdentity;
  if (id?.kind === "space") return `space:${id.serverUrl}#${id.spaceId}`;
  return app.vaultRoot;
}

/**
 * 写 keychain：有 key 则存、无 key 则删旧条目（清空 key 时真正移除，防止加载时把旧 key 读回）。
 * 只写与上次不同的条目——持久化由配置变化触发，绝大多数轮次 key 并未改动，逐条重写会白跑一轮 IPC。
 *
 * `force` 用于切换 key 落盘策略（syncKeys 开关）时：那一刻「keychain 里是什么」无从假设
 * （文件里可能是别的设备写入的值），必须无条件重写，否则本机 keychain 会留着旧值。
 * `baseline` = 本次写盘前的「已写入」记录（模块级基线，调用方读传）。
 * 返回新的基线，由调用方保存——只有写成功才推进，失败下次仍会重试。
 */
async function persistKeys(
  vaultRoot: string,
  providers: ProviderConfig[],
  force = false,
  baseline: Map<string, string> = persistedKeys,
): Promise<Map<string, string>> {
  const next = new Map<string, string>();
  const writes: Array<Promise<void>> = [];
  for (const p of providers) {
    const entry = `${vaultRoot}:${p.id}`;
    next.set(entry, p.apiKey);
    if (!force && baseline.get(entry) === p.apiKey) continue;
    writes.push(
      p.apiKey
        ? setApiKey(vaultRoot, p.id, p.apiKey).catch((e) =>
            console.error("keychain 写入失败", p.id, e),
          )
        : deleteApiKey(vaultRoot, p.id).catch((e) =>
            console.error("keychain 删除失败", p.id, e),
          ),
    );
  }
  // 已被移除的 provider：删除其 keychain 条目（removeProvider 也会定向删，此处兜底重复删除幂等）
  for (const entry of baseline.keys()) {
    if (next.has(entry)) continue;
    const id = entry.slice(vaultRoot.length + 1);
    writes.push(
      deleteApiKey(vaultRoot, id).catch((e) => console.error("keychain 删除失败", id, e)),
    );
  }
  await Promise.all(writes);
  return next;
}

/** 仓库级配置补丁：`undefined` = 该字段不在补丁里（磁盘保持当前值），`null` = 删除磁盘上该键。
 * 清空语义必须用 `null` 表达——补丁只发变更字段，`undefined` 与「没改」无法区分。 */
type VaultConfigPatch = {
  [K in keyof VaultConfig]?: NonNullable<VaultConfig[K]> | null;
};

/** 已加载配置的仓库守卫键（activeGuardKey()；null = 未加载/加载失败）。脏门控与写盘归属守卫共用：
 * 加载失败时为空，写盘一律跳过——此时内存是默认值，落盘会把磁盘上真实配置抹掉。 */
let loadedForVault: string | null = null;

/** 上一次成功持久化的配置摘要（providers 含 key）。空 = 无基线（下次必写）。
 * 判「有脏」不能只看 config.json：非同步模式下 key 不落文件，纯改 key 时磁盘内容不变，
 * 摘要必须含 key 才能让 keychain 写入照常发生。 */
let persistedDigest = "";

/** 上一次已写入 keychain 的 provider key（root:providerId → key），只重写变化项。 */
let persistedKeys = new Map<string, string>();

/** 加载时在 config.json 里发现的残留明文 Tavily key（`syncKeys` 关时不该存在：手工编辑、
 *  云盘合并回旧版本、开启期崩溃）。置位后任意一次仓库级写盘都会显式下发 `tavilyApiKey: null`
 *  把它删掉——「关闭开关即剥离」不能只依赖开关那一次删键。 */
let strayTavilyKeyOnDisk = false;

/** 上一次已提示过的仓库配置写盘失败原因（同因不重复弹：防抖重试会让同一错误刷屏）。 */
let lastVaultPatchError = "";

/** 配置摘要（providers 含 key；字段顺序稳定，同一内容恒等）。 */
function configDigest(cfg: AiConfig): string {
  return JSON.stringify(
    cfg.providers.map((p) => [p.id, p.name, p.baseUrl, p.models, p.apiKey]),
  );
}

/** 丢弃写盘基线：切仓库后下次进入必写。 */
function resetPersistBaseline(): void {
  loadedForVault = null;
  persistedDigest = "";
  persistedKeys = new Map();
  strayTavilyKeyOnDisk = false;
}

/** 仓库级配置写盘（本模块唯一出口）：补丁交给 Rust 侧字段级合并，并处理三件必须可见的事：
 *  - 磁盘原文损坏：后端已把原文备份并退回空基线，必须提示用户（他看到的是「设置被重置」）；
 *  - 后端拒绝写盘（损坏原文备份失败）：内存已改、磁盘没改，静默等于用户以为已保存；
 *  - 本次写盘是否顺带删掉了加载时发现的残留明文 key（`cleanVaultPatch` 注入）→ 写完即失效该标记。 */
async function writeVaultPatch(patch: Record<string, unknown>): Promise<void> {
  let backup: string | null;
  try {
    backup = await patchVaultConfig(patch);
  } catch (e) {
    const message = `仓库配置保存失败：${e instanceof Error ? e.message : String(e)}`;
    // 持久性失败（如磁盘只读）会被 400ms 防抖反复重试：同一原因只提示一次，成功后才允许再提示
    if (lastVaultPatchError !== message) {
      lastVaultPatchError = message;
      useNotificationStore.getState().notify({ level: "error", message });
    }
    throw e;
  }
  lastVaultPatchError = "";
  strayTavilyKeyOnDisk = false;
  if (backup) {
    useNotificationStore.getState().notify({
      level: "error",
      message: `仓库配置文件已损坏，原文备份为 .atelyx/${backup}：本次改动之外的设置已重置`,
    });
  }
}

/** 全量持久化：把当前供应商配置补丁进仓库级配置 + 按 key 落点决定是否写 keychain
 * （本地：syncKeys 开 = key 随 config.json 落盘多设备同步，关 = 写 keychain 按仓库身份隔离；
 * 空间：恒随团队元数据落盘，不进 keychain）。
 * 落盘目标按激活仓库身份分发（metadata 层：local = config.json；space = 团队元数据）。
 * 无脏（与上次成功持久化内容一致）直接返回，不写盘也不写 keychain。 */
async function persist(cfg: AiConfig): Promise<void> {
  // 捕获仓库快照：persist 是异步的（debounce 自动触发），期间用户可能已切换仓库，
  // 防 A 仓库的配置覆盖 B 仓库的配置、A 的 key 写进 B 的 keychain 条目。
  // 守卫键与 keychain 身份都在 await 前捕获：写盘在途期间切换仓库，本次写仍落旧身份自己的存储。
  const guardKey = activeGuardKey();
  const vaultAtCapture = currentVaultRoot();
  // 未进仓库或仓库配置未成功加载（内存为默认值）时不写：否则会抹掉磁盘上的真实配置
  if (!guardKey || loadedForVault !== guardKey) return;
  const digest = configDigest(cfg);
  if (digest === persistedDigest) return;
  const base = useSettingsStore.getState().vaultConfig ?? {};
  const syncKeys = !!base.syncKeys;
  // 只发 providers 一个字段：其余字段保留磁盘当前值（同一仓库的其他写者/撕裂窗口可能刚改过）
  const providers = cfg.providers.length ? cfg.providers : null;
  // 写盘前同步校验仓库未变；已切换则丢弃本次持久化（loadVaultConfig 已加载新仓库）
  if (activeGuardKey() !== guardKey) return;
  await writeVaultPatch(cleanActiveVaultPatch({ providers }));
  // keychain 写按捕获身份（vaultAtCapture）归属：数据属于写盘开始时的那个仓库，
  // 写盘在途期间切换不影响归属正确性（跳过反而会丢旧仓库的 key）。
  // 空间不用 keychain（key 随团队元数据走服务端），故只在「key 不随配置落盘」时写
  if (!keysInConfigForActive(syncKeys)) persistedKeys = await persistKeys(vaultAtCapture, cfg.providers);
  // 写盘在途期间切换仓库：内存回填与基线推进必须复检身份键——旧仓库的 providers 不得
  // 回填进新仓库的设置态；基线推进会让新仓库的下一次写盘被脏门控误跳过
  if (activeGuardKey() !== guardKey) return;
  // 内存基线在 await 之后按「当前状态」重取：等待 IPC 期间可能已有别的 commitVault 落进内存，
  // 用 await 之前捕获的旧 base 覆盖会把那次改动回滚掉（磁盘无碍，但设置页显示回退）
  useSettingsStore.setState((s) => ({
    vaultConfig: { ...(s.vaultConfig ?? {}), providers: providers ?? undefined },
  }));
  persistedDigest = digest;
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

/**
 * key 是否随配置本体一起落盘（决定写盘补丁里是否保留 apiKey、以及是否还需要写 keychain）：
 * 本地仓库 = 「API key 随仓库保存」开关（开 = 随 config.json 落盘多设备同步）；
 * 协作空间 = 恒真——空间的 AI 配置（含 key）整体由服务端团队元数据承载，不再进本机 keychain。
 */
function keysInConfigForActive(syncKeys: boolean): boolean {
  return useAppStore.getState().vaultIdentity?.kind === "space" || syncKeys;
}

/**
 * 写盘补丁清理：逐字段改动，不重建整份配置。
 *
 * 两个不可省的语义：
 * 1) `null` 是**删键指令**，必须原样传到 Rust 侧（`vault_config_patch` 只在值为 `null` 时删键）。
 *    合并语义下「省略键」= 保留磁盘现值，所以清空 key 只能靠 `null`——若在这里把 `null` 当成
 *    「无值」丢掉，关闭「API key 随仓库保存」后 `search.tavilyApiKey` 的明文会永远留在 config.json。
 * 2) `providers` 按 `keysInConfig` 映射为磁盘形状（false = 剥离 `apiKey`）；`search` 同口径，
 *    但**只剥离已有值、不引入 `null`**（那是显式删除，由补丁自己携带）。
 *
 * `keysInConfig` / `stray` 由调用方按激活仓库传入（本地看开关、空间恒真；残留 key 标记只对本地有意义）。
 * 补丁里没有的字段一律不出现——磁盘上其余字段由 Rust 侧按字段合并保留。
 * 这里是前端全部仓库级写盘路径（persist / commitVault / 切 key 落盘策略）的唯一汇聚点。
 */
function cleanVaultPatch(
  keysInConfig: boolean,
  stray: boolean,
  patch: Partial<Record<keyof VaultConfig, unknown>>,
): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(patch)) {
    if (value === undefined) continue;
    out[key] = value;
  }
  if (out.providers !== undefined) {
    out.providers =
      out.providers === null
        ? null
        : (out.providers as ProviderConfig[]).map((p) => toGlobalProvider(p, keysInConfig));
  }
  if (out.search && typeof out.search === "object") {
    out.search = stripSearchKeys(
      out.search as Record<string, unknown>,
      keysInConfig,
    );
  }
  // 磁盘上已发现的残留明文 key：借任意一次写盘顺手删掉（`null` = 删键指令）
  if (!keysInConfig && stray) {
    const search =
      out.search && typeof out.search === "object"
        ? (out.search as Record<string, unknown>)
        : {};
    out.search = { ...search, tavilyApiKey: null };
  }
  return out;
}

/** 激活仓库的写盘补丁清理：key 落点按激活仓库判断，残留 key 标记取加载时观察。 */
function cleanActiveVaultPatch(
  patch: Partial<Record<keyof VaultConfig, unknown>>,
): Record<string, unknown> {
  return cleanVaultPatch(
    keysInConfigForActive(!!useSettingsStore.getState().vaultConfig?.syncKeys),
    strayTavilyKeyOnDisk,
    patch,
  );
}

/**
 * 搜索引擎配置落盘前的 key 剥离：`null`（删键指令）原样保留，`undefined` 丢弃，
 * 非同步模式下剥离 `tavilyApiKey` 的**已有值**。
 */
function stripSearchKeys(
  search: Record<string, unknown>,
  syncKeys: boolean,
): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(search)) {
    if (value === undefined) continue;
    if (!syncKeys && key === "tavilyApiKey" && value !== null) continue;
    out[key] = value;
  }
  return out;
}

/** 仓库级配置写盘统一入口（各 setXxx 收敛于此）：只把补丁里出现的字段发出去 → 更新内存 → 落盘。
 * 未出现的字段保留磁盘当前值（Rust 侧字段级合并），撕裂窗口的陈旧副本因此不会覆盖主窗口刚写入的值。
 * 失败仅记日志不打断 UI（配置丢失可重设，非关键路径）。
 * 空间查看者先拒掉：否则内存已改、服务端会拒绝，形成「已保存」的假象。 */
async function commitVault(patch: VaultConfigPatch): Promise<void> {
  if (rejectActiveSpaceViewer("修改")) return;
  const base = useSettingsStore.getState().vaultConfig ?? {};
  useSettingsStore.setState({ vaultConfig: applyVaultPatch(base, patch) });
  const clean = cleanActiveVaultPatch(patch);
  // 全字段被过滤（补丁实际为空）：无事可做，省一次 IPC
  if (Object.keys(clean).length === 0) return;
  try {
    await writeVaultPatch(clean);
  } catch (e) {
    console.error("保存仓库级配置失败", e);
  }
}

/** 补丁应用到内存态：`null` 删键（与磁盘合并语义一致），`undefined` 不动。 */
function applyVaultPatch(base: VaultConfig, patch: VaultConfigPatch): VaultConfig {
  const next: VaultConfig = { ...base };
  for (const [key, value] of Object.entries(patch) as Array<
    [keyof VaultConfig, VaultConfig[keyof VaultConfig] | null | undefined]
  >) {
    if (value === undefined) continue;
    (next as Record<string, unknown>)[key] = value === null ? undefined : value;
  }
  return next;
}

// ===== 仓库级设置的读取 =====
//
// 仓库级设置的作用域恒为当前激活仓库：`vaultConfig` / `config` 这些字段就是它的配置，
// 运行时消费者（文件面板排序、编辑器、默认模型解析、搜索、Agent）读的也是同一份内存
// ——设置页与运行时不允许出现双重真相。

/** 失效提示词清理的在途标记（重入守卫：幂等清理并发跑只会重复写同值）。 */
let pruningPromptNotes = false;

/** 读到的仓库设置数据（不含守卫与基线）。 */
interface VaultSettingsData {
  vaultConfig: VaultConfig;
  config: AiConfig;
  searchConfig: GlobalSearchConfig;
  tavilyKey: string;
  agents: AgentConfig[];
  promptNotes: string[];
  /** 磁盘上有残留明文 Tavily key（本地仓库、syncKeys 关时观察到）。 */
  strayTavilyKey: boolean;
  /** 配置损坏备份文件名（仅本地路径可能非空）。 */
  corruptBackup: string | null;
}

/**
 * 读当前激活仓库的设置数据（metadata 层按仓库身份分流：local = `.atelyx/*`；space = 服务端团队元数据）。
 */
async function readVaultSettings(): Promise<VaultSettingsData> {
  const keychainRoot = currentVaultRoot();
  const isSpace = useAppStore.getState().vaultIdentity?.kind === "space";
  const { config: vc, corruptBackup } = await readVaultConfig();
  // key 落点：本地仓库默认存本机 keychain（「API key 随仓库保存」开时随配置文件落盘，多设备共享）；
  // 协作空间恒随团队元数据走服务端（团队共用一份），不读本机 keychain
  const keysFromKeychain = !isSpace && !vc.syncKeys;
  const providers = await Promise.all(
    (vc.providers ?? []).map(async (p): Promise<ProviderConfig> => {
      let apiKey = p.apiKey ?? "";
      if (keysFromKeychain) {
        try {
          apiKey = await getApiKey(keychainRoot, p.id);
        } catch (e) {
          console.error("keychain 读取失败", p.id, e);
        }
      }
      return { id: p.id, name: p.name, baseUrl: p.baseUrl, models: p.models ?? [], apiKey };
    }),
  );
  const config: AiConfig = { providers };
  // 搜索源：key 随配置文件落盘（本地 syncKeys / 空间团队元数据）= 直读配置内 tavilyApiKey
  const searchConfig: GlobalSearchConfig = toGlobalSearchConfig(vc.search, !keysFromKeychain) ?? {
    provider: "tavily",
    searxngUrl: "",
  };
  let tavilyKey = "";
  let strayTavilyKey = false;
  if (!keysFromKeychain) {
    tavilyKey = searchConfig.tavilyApiKey ?? "";
  } else {
    try {
      tavilyKey = await getApiKey(keychainRoot, "search-tavily");
    } catch (e) {
      console.error("keychain 读取失败 search-tavily", e);
    }
    // 配置里残留明文 key（曾在开启状态下落盘、之后 syncKeys 关闭或手工改回）：采纳为本机 keychain
    // 条目——与「关闭 = 剥离配置文件并回写 keychain」同语义，避免用户已配置的 key 被静默丢弃。
    // 文件里那份残留不参与取用（Rust 侧同样只在 syncKeys 开启时读文件内 key），
    // 置位标记后由下一次任意仓库级写盘经 cleanVaultPatch 显式删键。
    const stray = vc.search?.tavilyApiKey?.trim();
    strayTavilyKey = !!stray;
    if (!tavilyKey && stray) {
      tavilyKey = stray;
      await setApiKey(keychainRoot, "search-tavily", stray).catch((e) =>
        console.error("keychain 回写失败 search-tavily", e),
      );
    }
  }
  // 系统提示词标记（空间内为团队元数据；读到的团队数据不落本机）
  let promptNotes: string[] = [];
  try {
    promptNotes = await readPromptNotes();
  } catch (e) {
    console.error("读取系统提示词标记失败", e);
  }
  // Agent 配置（空间内为团队元数据）；预置 Agent（builtin 不可删）缺失即补入内存保证默认必现。
  // 空间内不落盘：种子补齐不是用户操作，不该因一次查看就改动团队共享的列表
  let agents: AgentConfig[] = [];
  try {
    agents = await readAgents();
  } catch (e) {
    console.error("读取 Agent 配置失败", e);
  }
  const missingBuiltins = BUILTIN_AGENTS.filter((b) => !agents.some((a) => a.id === b.id));
  if (missingBuiltins.length) {
    agents = [...missingBuiltins, ...agents];
    if (!isSpace) {
      // 首次种子/补齐预置：await 落盘（防迟到的写盘用旧列表覆盖用户刚做的增改）
      await writeAgents(agents).catch((e) => console.error("写入预置 Agent 失败", e));
    }
  }
  return { vaultConfig: vc, config, searchConfig, tavilyKey, agents, promptNotes, strayTavilyKey, corruptBackup };
}

/**
 * 写 Tavily key：key 落点与供应商 key 一致——随配置本体落盘（本地 = 「API key 随仓库保存」开；
 * 空间 = 恒随团队元数据）或写本机 keychain（本地默认）。
 */
async function applyTavilyKey(key: string): Promise<void> {
  const vaultRoot = currentVaultRoot();
  const vault = useSettingsStore.getState().vaultConfig;
  const search = useSettingsStore.getState().searchConfig;
  // 「key 随配置落盘」：本地看开关，空间恒真（团队元数据承载 key）
  const inConfig = keysInConfigForActive(!!vault?.syncKeys);
  try {
    if (inConfig) {
      // key 随配置落盘：空串必须发 `null`（删键指令）——
      // 合并语义下省略该键 = 保留磁盘现值，明文 key 会留在配置文件里。
      const searchConfig = { ...search, tavilyApiKey: key || undefined };
      useSettingsStore.setState({
        searchConfig,
        vaultConfig: { ...(useSettingsStore.getState().vaultConfig ?? {}), search: searchConfig },
      });
      // 此分支即「key 随配置落盘」：key 明文随补丁落盘（不剥离），残留标记无意义
      await writeVaultPatch(
        cleanVaultPatch(true, false, { search: { ...searchConfig, tavilyApiKey: key || null } }),
      );
    } else if (key) {
      // 默认：key 走 keychain（按仓库身份哈希隔离，与 provider key 区分）；空串删除条目
      await setApiKey(vaultRoot, "search-tavily", key);
    } else {
      await deleteApiKey(vaultRoot, "search-tavily");
    }
  } catch (e) {
    console.error("保存 Tavily key 失败", e);
  }
}

/**
 * 开关「API key 随仓库保存」（语义见 store 内注释）：配置补丁 +（关闭时）keychain 回写。
 * 两者必须按序执行（先落盘剥离、再回写 keychain），故整体作为一个任务。
 */
async function applySyncKeys(enabled: boolean): Promise<void> {
  const active = useSettingsStore.getState();
  const base = active.vaultConfig ?? {};
  const ai = active.config;
  const searchConfig = active.searchConfig;
  const tavilyKey = active.tavilyKey;
  // searchConfig 必须与磁盘同步更新：开 = 带 tavilyApiKey（后续 setSearchConfig 写回不丢 key），
  // 关 = 剥离（防残留 key 被 setSearchConfig 重新写回 config.json，破坏「关闭 = 剥离」语义）
  const hasSearch = base.search !== undefined || searchConfig.tavilyApiKey !== undefined;
  const nextSearch: GlobalSearchConfig | undefined = enabled
    ? { ...searchConfig, tavilyApiKey: tavilyKey || undefined }
    : hasSearch
      ? { provider: searchConfig.provider, searxngUrl: searchConfig.searxngUrl }
      : undefined;
  const providers = ai.providers.length ? ai.providers : base.providers;
  const vc: VaultConfig = { ...base, syncKeys: enabled, providers, search: nextSearch };
  const nextSearchRuntime = nextSearch ?? { provider: "tavily", searxngUrl: "" };
  useSettingsStore.setState({ vaultConfig: vc, searchConfig: nextSearchRuntime });
  try {
    // providers 关闭时按磁盘形状映射（不含 apiKey）后整数组替换——磁盘上原有的 apiKey 随之消失；
    // search 关闭时 tavilyApiKey 发 `null`（嵌套对象是字段级合并，省略键 = 保留磁盘现值）
    const providerPatch =
      (ai.providers.length ? ai.providers : undefined)?.map((p) => toGlobalProvider(p, enabled)) ??
      null;
    const patch: Record<string, unknown> = {
      syncKeys: enabled,
      providers: providerPatch,
      search:
        nextSearch === undefined
          ? null
          : enabled
            ? nextSearch
            : { provider: nextSearch.provider, searxngUrl: nextSearch.searxngUrl, tavilyApiKey: null },
    };
    await writeVaultPatch(
      cleanVaultPatch(keysInConfigForActive(enabled), strayTavilyKeyOnDisk, patch),
    );
  } catch (e) {
    console.error("保存仓库级配置失败", e);
  }
  // 关闭且 key 不随配置落盘（本地仓库）：剥离后回写 keychain（best-effort；force 覆盖旧条目）。
  // 空间无此开关（key 恒随团队元数据），不会走到这里
  if (!enabled && useAppStore.getState().vaultIdentity?.kind !== "space") {
    const vaultRoot = currentVaultRoot();
    persistedKeys = await persistKeys(vaultRoot, ai.providers, true, persistedKeys);
    try {
      if (tavilyKey) await setApiKey(vaultRoot, "search-tavily", tavilyKey);
      else await deleteApiKey(vaultRoot, "search-tavily");
    } catch (e) {
      console.error("保存 Tavily key 失败", e);
    }
  }
}

// ===== 仓库级写入的前置检查与落盘 =====

/** Agent 配置落盘统一入口（落盘 + 内存）：写成功才更新内存——空间只读/写失败不产生虚假可编辑态。
 *  返回是否写入成功（调用方据此决定是否继续，如新增后不返回新 id）。 */
async function writeAgentsToVault(next: AgentConfig[]): Promise<boolean> {
  try {
    await writeAgents(next);
  } catch (e) {
    console.error("保存 Agent 配置失败", e);
    return false;
  }
  useSettingsStore.setState({ agents: next });
  return true;
}

/** 仓库级写入的前置检查（UI 动作入口用）：不可写时给出可见原因并返回 false，调用方直接返回不产生虚假可编辑态。 */
function vaultWritable(action: string): boolean {
  return !rejectActiveSpaceViewer(action);
}

/**
 * 激活空间 + 本账号为查看者：团队层写入会被服务端拒绝，这里先拒掉，避免「内存已改、磁盘没改」
 * 的虚假已保存态。角色未知（空间列表未加载/离线）时放行，交给服务端裁决并给出可读原因。
 * 返回 true = 已拒绝（已弹提示）。
 */
function rejectActiveSpaceViewer(action: string): boolean {
  const identity = useAppStore.getState().vaultIdentity;
  if (identity?.kind !== "space") return false;
  const role = useSpaceDirectoryStore
    .getState()
    .spacesByServer[identity.serverUrl]?.find((x) => x.spaceId === identity.spaceId)?.role;
  if (role !== "viewer") return false;
  useNotificationStore.getState().notify({
    level: "warning",
    message: `你在该空间内是查看者，${action}不会保存`,
  });
  return true;
}

/** 全局配置损坏（原文已备份）的用户可见提示：读到空配置会连带重置最近仓库与外观，
 *  只写日志等于用户看到「东西全没了」却不知原因。备份文件在应用数据目录（与 global.json 同目录）。 */
function notifyGlobalConfigCorrupt(backup: string | null): void {
  if (!backup) return;
  useNotificationStore.getState().notify({
    level: "error",
    message: `全局配置已损坏，原文备份为 ${backup}（应用数据目录）：最近仓库与外观设置已重置`,
  });
}

/** 正文行宽归一化：0（或非数）= 不限制；其余钳制到可设区间（磁盘脏值/手输越界都经此收口）。 */
function normalizeNoteLineWidth(width: number | undefined): number {
  if (width === undefined || !Number.isFinite(width)) return NOTE_LINE_WIDTH_DEFAULT;
  if (width <= 0) return 0;
  return Math.min(NOTE_LINE_WIDTH_MAX, Math.max(NOTE_LINE_WIDTH_MIN, Math.round(width)));
}

/** 字符串记录的磁盘脏值收敛：只保留字符串项（手改/坏值不进运行时，防非串键串流入匹配逻辑）。 */
function sanitizeStringRecord(record: Record<string, string> | undefined): Record<string, string> {
  if (!record || typeof record !== "object") return {};
  const out: Record<string, string> = {};
  for (const [key, value] of Object.entries(record)) {
    if (typeof value === "string" && value.trim()) out[key] = value;
  }
  return out;
}

/** 应用级配置写盘统一入口（各外观 setXxx 收敛于此）：先写内存再 patch 落 global.json，
 * 失败仅记专属文案日志不打断 UI（外观丢失可重设，非关键路径）。
 * 值为 undefined 的字段 = 恢复默认：内存置 undefined（渲染回落默认），补丁通道改发 null 删键——
 * undefined 键经 JSON 序列化会缺席，被后端「缺键 = 保留旧值」吞掉，清空字号/字体后重启回弹。 */
async function commitGlobal(patch: Partial<GlobalConfig>, errMsg: string): Promise<void> {
  useSettingsStore.setState(patch);
  const wire = Object.fromEntries(
    Object.entries(patch).map(([key, value]) => [key, value ?? null]),
  ) as { [K in keyof GlobalConfig]?: GlobalConfig[K] | null };
  try {
    notifyGlobalConfigCorrupt((await updateGlobalConfig(wire)).corruptBackup);
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
  autoLaunch: false,
  softLineBreak: true,
  inlineTitle: false,
  noteLineWidth: NOTE_LINE_WIDTH_DEFAULT,
  mobileNavOrder: [],
  collabEnabled: false,
  collabNickname: "",
  collabColor: "",
  deviceName: "",
  vaultConfig: null,
  searchConfig: { provider: "tavily", searxngUrl: "" },
  tavilyKey: "",
  promptNotes: [],
  agents: [],
  folderColors: {},
  commandShortcuts: {},
  globalShortcuts: {},
  globalShortcutRegistrations: [],
  globalShortcutRegistrationsError: false,
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
    let autoLaunch = false;
    let softLineBreak = true;
    let inlineTitle = false;
    let noteLineWidth = NOTE_LINE_WIDTH_DEFAULT;
    let mobileNavOrder: string[] = [];
    let collabEnabled = false;
    let collabNickname = "";
    let collabColor = "";
    let deviceName = "";
    let commandShortcuts: Record<string, string> = {};
    let globalShortcuts: Record<string, string> = {};
    try {
      const { config: cfg, corruptBackup } = await readGlobalConfig();
      notifyGlobalConfigCorrupt(corruptBackup);
      // 主题：激活插件 id 直接取磁盘值（缺省 = 默认主题插件），设置字典缺省补该插件的预置项。
      theme = cfg.theme ?? BUILTIN_THEME_PLUGIN_ID;
      themeSettings = { ...(cfg.themeSettings ?? {}) };
      if (themeSettings[BUILTIN_THEME_PLUGIN_ID] === undefined) {
        themeSettings[BUILTIN_THEME_PLUGIN_ID] = { ...DEFAULT_BUILTIN_THEME_SETTINGS };
      }
      fontSize = cfg.fontSize;
      fontFamily = cfg.fontFamily;
      autoRestoreFiles = cfg.autoRestoreFiles ?? true;
      softLineBreak = cfg.softLineBreak ?? true;
      inlineTitle = cfg.inlineTitle ?? false;
      noteLineWidth = normalizeNoteLineWidth(cfg.noteLineWidth);
      // 磁盘脏值（手改/旧格式）只保留字符串项，顺序与去重交给 orderMobileViews
      mobileNavOrder = Array.isArray(cfg.mobileNavOrder)
        ? cfg.mobileNavOrder.filter((k): k is string => typeof k === "string")
        : [];
      collabEnabled = cfg.collabEnabled ?? false;
      collabNickname = cfg.collabNickname ?? "";
      collabColor = cfg.collabColor ?? "";
      // 覆盖表磁盘脏值只保留字符串项（手改/坏值不进运行时）
      commandShortcuts = sanitizeStringRecord(cfg.commandShortcuts);
      globalShortcuts = sanitizeStringRecord(cfg.globalShortcuts);
    } catch (e) {
      console.error("读取外观配置失败", e);
      // 读失败（含「全局配置损坏且原文备份失败」被后端拒绝）会让外观与协作配置本次不可用，必须可见
      useNotificationStore.getState().notify({
        level: "error",
        message: `全局配置读取失败，本次使用默认外观：${e instanceof Error ? e.message : String(e)}`,
      });
    }
    try {
      // 设备名独立读（配置读取失败不连带丢失协作身份兜底）
      deviceName = await getHostname();
    } catch (e) {
      console.error("读取设备名失败", e);
    }
    try {
      // 开机自启状态独立读（不落配置，直接取系统启动项；移动端 service 内 no-op）
      autoLaunch = await isAutoLaunchEnabled();
    } catch (e) {
      console.error("读取开机自启状态失败", e);
    }
    set({
      config: DEFAULT_AI_CONFIG,
      theme,
      themeSettings,
      fontSize,
      fontFamily,
      autoRestoreFiles,
      autoLaunch,
      softLineBreak,
      inlineTitle,
      noteLineWidth,
      mobileNavOrder,
      collabEnabled,
      collabNickname,
      collabColor,
      deviceName,
      commandShortcuts,
      globalShortcuts,
      searchConfig: { provider: "tavily", searxngUrl: "" },
      tavilyKey: "",
      promptNotes: [],
      agents: [],
      loaded: true,
    });
  },

  loadVaultConfig: async () => {
    // 基线随仓库重置：新仓库的配置与上一仓库无关，下次写盘必须发生
    resetPersistBaseline();
    const guardKey = activeGuardKey();
    const vaultAtCapture = currentVaultRoot();
    try {
      // 按激活仓库身份分流（local = `.atelyx/config.json`；space = 服务端团队元数据：
      // AI 配置本体（含 key）+ 排序/排除夹/附件夹）
      const data = await readVaultSettings();
      // 文件夹图标颜色（空间内为 team meta `folder-colors`）
      let folderColors: Record<string, string> = {};
      try {
        folderColors = await readFolderColors();
      } catch (e) {
        console.error("读取文件夹图标颜色失败", e);
      }
      set({
        vaultConfig: data.vaultConfig,
        config: data.config,
        searchConfig: data.searchConfig,
        tavilyKey: data.tavilyKey,
        promptNotes: data.promptNotes,
        agents: data.agents,
        folderColors,
      });
      // 配置损坏时读到的是空配置（磁盘原文已备份）：供应商清单随原文一起丢失，不提示就等于
      // 用户看到「设置与 API key 全没了」而不知原因，必须明确要求重新配置。
      if (data.corruptBackup) {
        useNotificationStore.getState().notify({
          level: "error",
          message: `仓库配置文件已损坏，原文备份为 .atelyx/${data.corruptBackup}：供应商、默认模型与 API key 已重置，需重新配置`,
        });
      }
      // 只有走到这里才算「加载成功」：写盘守卫据此判定，加载失败时内存是默认值，落盘会抹掉磁盘配置
      loadedForVault = guardKey;
      persistedDigest = configDigest(data.config);
      // 只有走 keychain 的仓库才建基线：key 随配置落盘（本地 syncKeys 开 / 空间团队元数据）时
      // 内存 key 来自配置文件，把它写进「keychain 已写入」基线会让「关闭开关」时的回写被误跳过
      // （本机 keychain 残留旧值）；空间整段不用 keychain。
      persistedKeys = new Map(
        keysInConfigForActive(!!data.vaultConfig.syncKeys)
          ? []
          : data.config.providers.map((p) => [`${vaultAtCapture}:${p.id}`, p.apiKey]),
      );
      strayTavilyKeyOnDisk = data.strayTavilyKey;
    } catch (e) {
      console.error("读取仓库级配置失败", e);
      // 清空为默认态：内存不得残留上一仓库的配置（切换收尾后依赖方按空态降级，
      // 而不是把上一个仓库的供应商/Agent 读成当前仓库的）。
      set({
        vaultConfig: null,
        config: DEFAULT_AI_CONFIG,
        searchConfig: { provider: "tavily", searxngUrl: "" },
        tavilyKey: "",
        promptNotes: [],
        agents: [],
        folderColors: {},
      });
      // 加载失败后 persist 路径的 provider 写盘会被守卫（loadedForVault 未登记）整体跳过
      //（内存是默认值，落盘会抹掉磁盘配置）；commitVault/applySyncKeys/applyTavilyKey 不经该守卫。
      // 「该写却不写」必须让用户知道，否则改设置看起来生效、重启后全丢。
      useNotificationStore.getState().notify({
        level: "error",
        message: "仓库配置读取失败：供应商与 API key 的修改不会保存，请重新打开仓库后再试",
      });
    }
  },

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
    if (!vaultWritable("修改")) return "";
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
    if (!vaultWritable("修改")) return;
    const providers = get().config.providers.map((p) =>
      p.id === id ? { ...p, ...patch } : p,
    );
    const cfg = { providers };
    set({ config: cfg });
    persistDebounced();
  },

  flush: async () => {
    await persistCtl.flush();
  },

  removeProvider: async (id) => {
    if (!vaultWritable("修改")) return;
    const providers = get().config.providers.filter((p) => p.id !== id);
    const cfg = { providers };
    set({ config: cfg });
    await persist(cfg);
    // 删 keychain 条目（best-effort；key 按仓库隔离，无条件删；空间不用 keychain）
    if (!keysInConfigForActive(!!get().vaultConfig?.syncKeys)) {
      deleteApiKey(currentVaultRoot(), id).catch((e) =>
        console.error("删除 keychain 条目失败", id, e),
      );
    }
  },

  setVaultModel: async (model) => {
    await commitVault(
      model
        ? { model: model.model, modelProviderId: model.providerId || null }
        : { model: null, modelProviderId: null },
    );
  },

  setAutoNamingEnabled: async (enabled) => {
    await commitVault({ autoNamingEnabled: enabled });
  },

  setAutoNamingModel: async (model) => {
    await commitVault({ autoNamingModel: model ?? null });
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
    // 空数组 = 删除该键（缺省 = 无排除，保持 config.json 干净）
    await commitVault({ excludeFolders: folders.length ? folders : null });
  },

  setEntryScene: async (sceneId) => {
    // undefined = 删除该键（缺省 = 不切换，保持上次界面）
    await commitVault({ entrySceneId: sceneId ?? null });
  },

  setSoftLineBreak: (enabled) =>
    commitGlobal({ softLineBreak: enabled }, "保存宽松换行配置失败"),

  setInlineTitle: (enabled) =>
    commitGlobal({ inlineTitle: enabled }, "保存页面内标题配置失败"),
  setNoteLineWidth: (width) =>
    commitGlobal({ noteLineWidth: normalizeNoteLineWidth(width) }, "保存正文行宽配置失败"),

  setMobileNavOrder: (order) =>
    commitGlobal({ mobileNavOrder: [...order] }, "保存移动端导航顺序失败"),

  setAutoRestoreFiles: (enabled) =>
    commitGlobal({ autoRestoreFiles: enabled }, "保存自动恢复配置失败"),

  refreshAutoLaunch: async () => {
    try {
      set({ autoLaunch: await isAutoLaunchEnabled() });
    } catch (e) {
      console.error("读取开机自启状态失败", e);
    }
  },

  setAutoLaunch: async (enabled) => {
    try {
      await setAutoLaunchEnabled(enabled);
    } catch (e) {
      console.error("设置开机自启失败", e);
      useNotificationStore.getState().notify({
        level: "error",
        message: `设置开机自启失败：${e instanceof Error ? e.message : String(e)}`,
      });
    }
    // 无论成败都回读系统启动项真实状态：写失败时开关不虚报（系统可能是被外部改过的状态）
    await get().refreshAutoLaunch();
  },

  setCollabConfig: async (patch) => {
    set(patch);
    try {
      // 空值显式发 null：补丁通道里 null = 删键；发 undefined 会在序列化时缺席，
      // 被服务端「缺键 = 保留旧值」语义吞掉，关掉的协作配置重启后回弹
      notifyGlobalConfigCorrupt(
        (
          await updateGlobalConfig({
            collabEnabled: get().collabEnabled || null,
            collabNickname: get().collabNickname || null,
            collabColor: get().collabColor || null,
          })
        ).corruptBackup,
      );
      // 配置变更即时生效：重建协作连接（开关/身份变化）
      useCollabStore.getState().applyConfig({
        enabled: get().collabEnabled,
        nickname: get().collabNickname,
        color: get().collabColor,
      });
    } catch (e) {
      console.error("保存协作配置失败", e);
    }
  },

  setAttachmentFolder: async (folder) => {
    await commitVault({ attachmentFolder: folder || null });
  },

  /** 注册/注销系统提示词笔记：数组含该路径则移除，否则添加（空数组也落盘保持文件干净，独立于 config.json）。
   *  写成功才更新内存：空间写被服务端拒绝（如 viewer）、本地写盘失败时，内存不产生虚假可编辑态。 */
  togglePromptNote: async (file) => {
    const marked = get().promptNotes.includes(file);
    const next = marked
      ? get().promptNotes.filter((f) => f !== file)
      : [...get().promptNotes, file];
    try {
      await writePromptNotes(next);
    } catch (e) {
      console.error("保存系统提示词标记失败", e);
      return;
    }
    set({ promptNotes: next });
  },

  /** 笔记重命名/移动后同步标记路径（旧路径未标记时 no-op；写成功才更新内存，同 togglePromptNote）。 */
  remapPromptNote: async (oldFile, newFile) => {
    const marked = get().promptNotes;
    if (!marked.includes(oldFile)) return;
    const next = marked.map((f) => (f === oldFile ? newFile : f));
    try {
      await writePromptNotes(next);
    } catch (e) {
      console.error("保存系统提示词标记失败", e);
      return;
    }
    set({ promptNotes: next });
  },

  /** 文件夹重命名后同步标记路径（`oldDir/` 前缀命中才更新；写成功才更新内存，同 togglePromptNote）。 */
  remapPromptNotesByDir: async (oldDir, newDir) => {
    const marked = get().promptNotes;
    const next = marked.map((f) => remapDirPrefix(f, oldDir, newDir));
    if (next.every((f, i) => f === marked[i])) return;
    try {
      await writePromptNotes(next);
    } catch (e) {
      console.error("保存系统提示词标记失败", e);
      return;
    }
    set({ promptNotes: next });
  },

  /** Agent 配置落盘统一入口（各 CRUD 收敛于此；写成功才更新内存——空间只读/本地写失败时不产生虚假可编辑态）。 */
  addAgent: async () => {
    const id = crypto.randomUUID();
    const agent: AgentConfig = {
      id,
      name: "新 Agent",
      tools: [...DEFAULT_AGENT_TOOLS],
    };
    await writeAgentsToVault([...get().agents, agent]);
    return id;
  },

  updateAgent: async (id, patch) => {
    const next = get().agents.map((a) => (a.id === id ? { ...a, ...patch } : a));
    await writeAgentsToVault(next);
  },

  removeAgent: async (id) => {
    // 预置 Agent 不可删除（builtin 标记；UI 已隐藏删除按钮，此处兜底防误删）
    const target = get().agents.find((a) => a.id === id);
    if (target?.builtin) return;
    await writeAgentsToVault(get().agents.filter((a) => a.id !== id));
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
    await writeAgentsToVault([...get().agents, copy]);
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

  /** 笔记重命名/移动后同步 Agent 引用的提示词笔记路径（旧路径未被引用时 no-op；写成功才更新内存）。 */
  remapAgentPromptNote: async (oldFile, newFile) => {
    const agents = get().agents;
    if (!agents.some((a) => a.systemPromptFile === oldFile)) return;
    const next = agents.map((a) =>
      a.systemPromptFile === oldFile ? { ...a, systemPromptFile: newFile } : a,
    );
    try {
      await writeAgents(next);
    } catch (e) {
      console.error("保存 Agent 配置失败", e);
      return;
    }
    set({ agents: next });
  },

  /** 文件夹重命名后同步 Agent 引用的提示词笔记路径前缀（`oldDir/` 前缀命中才更新；写成功才更新内存）。 */
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
    try {
      await writeAgents(next);
    } catch (e) {
      console.error("保存 Agent 配置失败", e);
      return;
    }
    set({ agents: next });
  },

  /**
   * 清理失效提示词：注册列表与 Agent 引用中指向已不存在笔记的路径一并移除（进入 Agent 设置页时触发）。
   * 口径：
   * - 只认磁盘事实：存在性走内容面元数据查询，查询失败的路径视为存在（不确定不删，防网络抖动误清团队标记）；
   * - 写盘前重取最新内存态应用缺失集合（校验在途期间的其他增删不被本次清理覆盖），写成功才更新内存；
   * - Agent 引用清理与注册列表清理对称（同重命名同步的组合），写失败不回滚注册列表——下次进入再收敛。
   */
  pruneMissingPromptNotes: async () => {
    if (pruningPromptNotes) return;
    pruningPromptNotes = true;
    try {
      const s = get();
      // Agent 引用路径可能不在注册列表（注册被注销后引用仍在），一并纳入校验
      const referenced = s.agents
        .map((a) => a.systemPromptFile)
        .filter((f): f is string => !!f && !s.promptNotes.includes(f));
      const paths = [...new Set([...s.promptNotes, ...referenced])];
      if (paths.length === 0) return;
      const checks = await Promise.all(
        paths.map(async (file) => {
          try {
            return await fileExists(file);
          } catch (e) {
            console.warn("提示词笔记存在性校验失败，本次跳过清理", file, e);
            return true;
          }
        }),
      );
      const missing = new Set(paths.filter((_, i) => !checks[i]));
      if (missing.size === 0) return;
      // 写盘前重取最新态应用缺失集合：校验在途期间的其他增删不被本次清理覆盖
      const latestNotes = get().promptNotes;
      const nextNotes = latestNotes.filter((f) => !missing.has(f));
      const agents = get().agents;
      const nextAgents = agents.map((a) =>
        a.systemPromptFile && missing.has(a.systemPromptFile)
          ? { ...a, systemPromptFile: undefined }
          : a,
      );
      const agentsCleared = nextAgents.filter((a, i) => a !== agents[i]).length;
      const notesCleared = latestNotes.length - nextNotes.length;
      if (notesCleared === 0 && agentsCleared === 0) return;
      if (notesCleared > 0) {
        try {
          await writePromptNotes(nextNotes);
        } catch (e) {
          console.error("清理失效提示词标记失败", e);
          return;
        }
        set({ promptNotes: nextNotes });
      }
      // Agent 引用清理与注册列表清理对称（同重命名同步的组合）；
      // 引用写失败不回滚注册列表——已清的标记保持清理，残留引用下次进入再收敛
      if (agentsCleared > 0 && !(await writeAgentsToVault(nextAgents))) return;
      const parts: string[] = [];
      if (notesCleared > 0) parts.push(`已清理 ${notesCleared} 条失效提示词标记`);
      if (agentsCleared > 0) parts.push(`移除 ${agentsCleared} 个 Agent 的提示词引用`);
      useNotificationStore.getState().notify({ level: "info", message: `${parts.join("，")}` });
    } finally {
      pruningPromptNotes = false;
    }
  },

  /** 设文件夹图标颜色（dir = 相对仓库根路径，color = hex 色；undefined = 清除还原默认，写 .atelyx/folder-colors.json）。
   *  写成功才更新内存：空间内写被服务端拒绝（无编辑权限）时不产生虚假可编辑态。 */
  setFolderColor: async (dir, color) => {
    const cur = get().folderColors;
    const next = { ...cur };
    if (color) next[dir] = color;
    else delete next[dir];
    try {
      await writeFolderColors(next);
    } catch (e) {
      console.error("保存文件夹图标颜色失败", e);
      return;
    }
    set({ folderColors: next });
  },

  /** 设命令快捷键覆盖（globalId → 键串；undefined = 清除恢复声明默认键，写 global.json）。
   *  生效不需重注册：应用内命令分发按注册表 + 覆盖表现查。 */
  setCommandShortcut: async (globalId, shortcut) => {
    const cur = get().commandShortcuts;
    const next: Record<string, string> = { ...cur };
    if (shortcut) next[globalId] = shortcut;
    else delete next[globalId];
    set({ commandShortcuts: next });
    // 空表整字段删除，global.json 不留空壳（与 excludeFolders 空数组删键同口径）
    try {
      notifyGlobalConfigCorrupt(
        (await updateGlobalConfig({ commandShortcuts: Object.keys(next).length ? next : null }))
          .corruptBackup,
      );
    } catch (e) {
      console.error("保存命令快捷键失败", e);
    }
  },

  /** 设全局快捷键覆盖（`插件id:声明id` → accelerator；undefined = 清除恢复声明默认键）。
   *  先执行 OS 层重注册（失败不落覆盖、保留原键并提示），成功才更新内存与 global.json。 */
  setGlobalShortcutOverride: async (declarationKey, accelerator) => {
    const sep = declarationKey.indexOf(":");
    const pluginId = sep > 0 ? declarationKey.slice(0, sep) : "";
    const declarationId = sep > 0 ? declarationKey.slice(sep + 1) : "";
    if (!pluginId || !declarationId) return false;
    const outcome = await applyShortcutOverride(getKernel().ctx, pluginId, declarationId, accelerator);
    if (!outcome.ok) {
      useNotificationStore.getState().notify({
        level: "error",
        message: `快捷键修改失败：${outcome.error ?? "未知原因"}（已保留原快捷键）`,
      });
      return false;
    }
    const cur = get().globalShortcuts;
    const next: Record<string, string> = { ...cur };
    if (accelerator) next[declarationKey] = accelerator;
    else delete next[declarationKey];
    set({ globalShortcuts: next });
    try {
      notifyGlobalConfigCorrupt(
        (await updateGlobalConfig({ globalShortcuts: Object.keys(next).length ? next : null }))
          .corruptBackup,
      );
    } catch (e) {
      console.error("保存全局快捷键失败", e);
    }
    await get().refreshGlobalShortcutRegistrations();
    return true;
  },

  /** 现查 OS 层全局快捷键登记清单（读失败置空数组 + 失败标记——展示层不虚报状态）。 */
  refreshGlobalShortcutRegistrations: async () => {
    try {
      set({ globalShortcutRegistrations: await listGlobalShortcuts(), globalShortcutRegistrationsError: false });
    } catch (e) {
      console.error("读取全局快捷键登记失败", e);
      set({ globalShortcutRegistrations: [], globalShortcutRegistrationsError: true });
    }
  },

  /** 文件夹重命名/移动后同步颜色键（目录键精确或 `oldDir/` 前缀命中才更新；写成功才更新内存，同 setFolderColor）。 */
  remapFolderColorsByDir: async (oldDir, newDir) => {
    const cur = get().folderColors;
    const keys = Object.keys(cur);
    const next: Record<string, string> = {};
    let changed = false;
    for (const k of keys) {
      const nk = remapDirKey(k, oldDir, newDir);
      if (nk !== k) changed = true;
      next[nk] = cur[k];
    }
    if (!changed) return;
    try {
      await writeFolderColors(next);
    } catch (e) {
      console.error("保存文件夹图标颜色失败", e);
      return;
    }
    set({ folderColors: next });
  },

  followRemotePathRename: (oldPath, newPath) => {
    // 文件夹颜色：目录键精确或前缀命中迁移（单文件改名帧对目录键为不命中 no-op）
    const colors = get().folderColors;
    const nextColors: Record<string, string> = {};
    let colorsChanged = false;
    for (const [k, v] of Object.entries(colors)) {
      const nk = remapDirKey(k, oldPath, newPath);
      if (nk !== k) colorsChanged = true;
      nextColors[nk] = v;
    }
    // 提示词标记：文件键精确（该笔记本身被改名）或前缀（所在文件夹被改名）命中迁移
    const marked = get().promptNotes;
    const nextMarked = marked.map((f) => remapDirKey(f, oldPath, newPath));
    const markedChanged = nextMarked.some((f, i) => f !== marked[i]);
    // Agent 引用的提示词笔记路径：命中规则同提示词标记
    const agents = get().agents;
    let agentsChanged = false;
    const nextAgents = agents.map((a) => {
      if (!a.systemPromptFile) return a;
      const nf = remapDirKey(a.systemPromptFile, oldPath, newPath);
      if (nf === a.systemPromptFile) return a;
      agentsChanged = true;
      return { ...a, systemPromptFile: nf };
    });
    if (!colorsChanged && !markedChanged && !agentsChanged) return;
    set({
      ...(colorsChanged ? { folderColors: nextColors } : null),
      ...(markedChanged ? { promptNotes: nextMarked } : null),
      ...(agentsChanged ? { agents: nextAgents } : null),
    });
  },

  setSearchConfig: async (patch) => {
    if (!vaultWritable("修改")) return;
    const next = { ...get().searchConfig, ...patch };
    set({ searchConfig: next });
    await commitVault({ search: next });
  },

  setTavilyKey: async (key) => {
    if (!vaultWritable("修改")) return;
    set({ tavilyKey: key });
    await applyTavilyKey(key);
  },

  /**
   * 开关「API key 随仓库保存」（多设备同步）：开启 = 当前 key 写入 config.json；
   * 关闭 = 剥离 config 内 key + 回写 keychain。
   *
   * 关闭时必须显式发 `null` 删键：合并语义下「省略键」= 保留磁盘现值，省略等于把开启期间落盘的
   * 明文 key 永久留在 config.json（文件可能随 Git/云盘同步 → 泄露）。同理 keychain 回写要 `force`：
   * 那一刻「本机 keychain 里是什么」无从假设（文件里可能是别的设备写入的值）。
   */
  setSyncKeys: async (enabled) => {
    // 空间无此设定：空间的 AI 配置（含 key）整体由服务端团队元数据承载，没有「key 落哪」的选项
    if (useAppStore.getState().vaultIdentity?.kind === "space") return;
    if (!vaultWritable("修改")) return;
    await applySyncKeys(enabled);
  },
}));

/** 默认生效模型显示名 selector（对话节点与 AI 对话面板共用）：跟随仓库默认时显示真实生效模型名
 * （resolveDefaultModel 固定供应商解析 + 跨供应商同名前缀消歧）；未配置默认模型 = null。
 * 返回原始串，Zustand Object.is 比较保证仅在值变时重渲染。 */
export function selectDefaultModelDisplay(s: SettingsState): string | null {
  const def = s.resolveDefaultModel();
  return def ? modelDisplayLabel(s.config.providers, def.provider, def.model) : null;
}

// ===== 设置 UI 的读取选择器 =====
//
// 设置组件读仓库级配置直接取状态字段（`s.vaultConfig` / `s.config` / `s.searchConfig` / `s.agents` …）：
// 作用域恒为当前激活仓库，没有第二份可供分发的数据。

// 切仓编排动作登记：appStore 经 kernelLifecycle 分发（appStore 不 import 本模块）。
registerVaultSwitchActions({
  loadConfig: () => useSettingsStore.getState().loadVaultConfig(),
  flushConfig: () => useSettingsStore.getState().flush(),
});

