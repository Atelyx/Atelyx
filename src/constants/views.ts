/**
 * 视图显示名（面板头标签 / 撕裂窗口标题共用）。键 = 内置视图类型（穷举保护）；
 * 插件视图 label 不在此表，走 pluginViewLabel/viewMetaFor 兜底。
 */
import type { BuiltinViewKind, ViewKind } from "@/types";

/** 内置视图类型清单（视图选择器选项顺序；插件可注册开放 kind，不在此表）。 */
export const VIEW_KINDS: ViewKind[] = [
  "canvas",
  "note",
  "table",
  "files",
  "search",
  "inspector",
  "aichat",
  "collabroom",
  "calendar",
  "repohistory",
  "recent",
];

/** 主页布局的稳定 id（固定置顶、不可删除/排序/重命名；uiState 加载时缺失即补入，幂等只补一次）。 */
export const HOME_LAYOUT_ID = "home";

/** 默认场景的稳定 id（固定置顶、不可删除/排序/重命名；含主页布局，与 Rust `DEFAULT_SCENE_ID` 对齐）。 */
export const DEFAULT_SCENE_ID = "default";

export const VIEW_LABELS: Record<BuiltinViewKind, string> = {
  canvas: "画布",
  note: "笔记",
  table: "表格",
  files: "文件",
  search: "搜索",
  inspector: "属性",
  aichat: "AI 对话",
  collabroom: "协作房间",
  calendar: "日历",
  repohistory: "仓库历史",
  recent: "最近打开",
  empty: "空面板",
};
