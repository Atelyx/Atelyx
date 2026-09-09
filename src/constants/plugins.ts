/**
 * 插件平台常量：类型/作用域/徽标的展示文案，市场索引地址，官方名单。
 * 插件面服务的展示文案与敏感标记见 constants/pluginServices.ts（此处不维护服务词汇表）。
 */
import type { PluginBadge, PluginScope, PluginSourceKind, PluginType } from "@/types";

/** 插件发现标签：作者给仓库打此 topic 即进入市场聚合。 */
export const PLUGIN_DISCOVERY_TOPIC = "atelyx-plugin";

/** 插件包清单文件名（插件根目录；npm 标准字段 + `atelyx` 块）。 */
export const PLUGIN_MANIFEST_FILE = "package.json";

/** app 级插件目录名（位于 app_data_dir 下）。 */
export const PLUGIN_APP_DIR = "plugins";

/** vault 级插件目录（相对仓库根）。 */
export const PLUGIN_VAULT_DIR = ".atelyx/plugins";

/** 官方账号名单：这些账号发布的插件自动带 official 徽标（市场聚合侧同用）。 */
export const OFFICIAL_PLUGIN_ORGS = ["Xuhang944"] as const;

/** 市场索引地址（官方索引仓库的 CDN 直链）。 */
export const PLUGIN_INDEX_URL =
  "https://cdn.jsdelivr.net/gh/Xuhang944/Atelyx-plugin-index@main/index.json";
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

/** 插件安装来源展示文案（已装列表徽标）。 */
export const PLUGIN_SOURCE_LABELS: Record<PluginSourceKind, string> = {
  market: "市场",
  git: "Git",
  local: "本地目录",
  builtin: "内置",
};
