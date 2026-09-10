/**
 * 插件平台常量：类型/作用域/来源/徽标的展示文案，市场索引地址，官方名单。
 * 插件面服务的展示文案与敏感标记见 constants/pluginServices.ts（此处不维护服务词汇表）；
 * 插件目录布局与清单文件名归 Rust 侧（`commands/plugin.rs` 的常量），前端不重复持有。
 */
import type { PluginBadge, PluginScope, PluginSourceKind, PluginType } from "@/types";

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

/** 插件来源展示文案（组合树行徽标；来源仅作中性信息，不构成类别）。 */
export const PLUGIN_SOURCE_LABELS: Record<PluginSourceKind, string> = {
  market: "市场",
  git: "Git",
  local: "本地目录",
  builtin: "随应用",
};
