/**
 * 日历（主页面板）类型：手动日程条目（`.atelyx/calendar.json` 仓库级存储）。
 * 自动进日历的带日期笔记（经内容面 `listDatedNotes`，个人仓库 = `list_dated_notes` 命令、
 * 协作空间 = 服务端端点）不在此 schema 内，见 `services/home`。
 */

/** 手动日程条目（date 为本地日期 `YYYY-MM-DD`）。 */
export interface CalendarItem {
  id: string;
  /** 条目归属日期（本地 `YYYY-MM-DD`）。 */
  date: string;
  title: string;
  /** 备注（可选）。 */
  note?: string;
  /** 展示色（hex；缺省 = 默认强调色）。 */
  color?: string;
  /** 创建时间戳（ms）。 */
  createdAt: number;
}
