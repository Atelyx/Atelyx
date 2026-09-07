/**
 * 插件平台常量：类型/作用域/能力/徽标的展示文案，市场索引地址，官方名单。
 * 类型与能力枚举定义在 types/plugin.ts（此处只做展示映射，避免双份数据源）。
 */
import type { PluginBadge, PluginCapability, PluginScope, PluginSourceKind, PluginType } from "@/types";

/** 插件发现标签：作者给仓库打此 topic 即进入市场聚合。 */
export const PLUGIN_DISCOVERY_TOPIC = "atelyx-plugin";

/** 插件清单文件名（插件根目录）。 */
export const PLUGIN_MANIFEST_FILE = "atelyx.json";

/** app 级插件目录名（位于 app_data_dir 下）。 */
export const PLUGIN_APP_DIR = "plugins";

/** vault 级插件目录（相对仓库根）。 */
export const PLUGIN_VAULT_DIR = ".atelyx/plugins";

/** 官方账号名单：这些账号发布的插件自动带 official 徽标（市场聚合侧同用）。 */
export const OFFICIAL_PLUGIN_ORGS = ["Xuhang944"] as const;

/** 市场索引地址（官方索引仓库的 CDN 直链）。 */
export const PLUGIN_INDEX_URL =
  "https://cdn.jsdelivr.net/gh/Xuhang944/Atelyx-plugin-index@main/index.json";
export const PLUGIN_BLOCKLIST_URL =
  "https://cdn.jsdelivr.net/gh/Xuhang944/Atelyx-plugin-index@main/blocklist.json";
export const PLUGIN_ENDORSED_URL =
  "https://cdn.jsdelivr.net/gh/Xuhang944/Atelyx-plugin-index@main/endorsed.json";

/** 市场索引本地缓存时长（毫秒）。 */
export const PLUGIN_INDEX_CACHE_MS = 6 * 60 * 60 * 1000;

/** 插件类型展示文案。 */
export const PLUGIN_TYPE_LABELS: Record<PluginType, string> = {
  tool: "AI 工具/命令",
  setting: "设置项",
  panel: "面板视图",
  app: "应用页面/模式",
  node: "画布节点",
  theme: "UI 皮肤",
  command: "命令/快捷键",
  background: "后台服务",
  tableview: "表格视图",
};

/** 插件作用域展示文案。 */
export const PLUGIN_SCOPE_LABELS: Record<PluginScope, string> = {
  app: "本机",
  vault: "随仓库共享",
};

/** 插件徽标展示文案。 */
export const PLUGIN_BADGE_LABELS: Record<PluginBadge, string> = {
  official: "官方",
  endorsed: "精选",
};

/** 插件能力/命令展示文案（市场「命令使用清单」与权限说明共用）。 */
export const PLUGIN_CAPABILITY_LABELS: Record<PluginCapability, string> = {
  "keychain:read": "读取 API Key（敏感）",
  shell: "执行外部程序（敏感）",
  "vault:delete": "删除仓库文件（敏感）",
  "vault:read": "读取仓库文件",
  "vault:write": "写入仓库文件",
  "vault:rename": "重命名/移动仓库文件",
  "ai:chat": "发起 AI 对话",
  "ai:tool": "注册 AI 工具",
  "search:web": "联网搜索",
  "web:fetch": "抓取网页",
  clipboard: "读写剪贴板",
  "window:manage": "窗口控制",
  "settings:read": "读取设置",
  "settings:write": "修改设置",
  "state:persist": "持久化自身数据",
  "events:subscribe": "订阅应用事件",
  "table:read": "读取当前表格数据（行/字段/图片/选中行）",
};

/** 插件安装来源展示文案（已装列表徽标）。 */
export const PLUGIN_SOURCE_LABELS: Record<PluginSourceKind, string> = {
  market: "市场",
  git: "Git",
  local: "本地目录",
};
