/**
 * 视图显示名（面板头标签 / 撕裂窗口标题共用）。键 = 内置视图类型（穷举保护）；
 * 插件视图 label 不在此表，走 pluginViewLabel/viewMetaFor 兜底。
 */
import type { BuiltinViewKind } from "@/types";

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
