/**
 * 自写回放抑制（watcher 事件消歧）：写盘/CRUD 完成时按「文件路径」记录时刻；
 * watcher 收到同路径事件且在抑制窗口内 → 视为 app 自写回放，不弹「已被外部修改」误提示。
 * 重命名/移动类操作经 Rust 扫盘改写多个 .atlx（前端不知全集），用全局标记兜底。
 * .md 内容自写经路径级标记（`markSelfSave(file)`，见 noteStore.saveNoteContent）——
 * 仅用于 watcher 笔记分支跳过无关的全树重扫；其跨编辑面同步与冲突检测
 * （markNoteExternallyEdited）不走本抑制，不受影响。附件/.md 其它来源事件不抑制。
 *
 * 各 store（画布/表格/面板/配置）保存统一走此工具，与 utils/persist.ts 的防抖样板同层，
 * 避免职责挂在某个 store 上造成跨 store 反向依赖。
 */
const SELF_SAVE_SUPPRESS_MS = 2000;
/** 路径级自写时刻（保存/CRUD 记录具体文件，防跨文件误抑制——存表格不再吞掉画布外部修改）。 */
const selfSavedAt = new Map<string, number>();
let globalSelfSaveAt = 0;

/** watcher 事件处理器判断路径为 path 的事件是否为 app 自写的回放（未命中路径时按全局兜底）。 */
export function isSelfSaveEcho(path?: string): boolean {
  const now = Date.now();
  if (now - globalSelfSaveAt < SELF_SAVE_SUPPRESS_MS) return true;
  if (path && now - (selfSavedAt.get(path) ?? 0) < SELF_SAVE_SUPPRESS_MS)
    return true;
  return false;
}

/** 在「软件内写文件」后标记自写，抑制 watcher 误报。
 * path = 本次写过的具体文件（保存/CRUD）；省略 = 全局（重命名扫盘改写的文件集合未知）。 */
export function markSelfSave(path?: string | string[]): void {
  const now = Date.now();
  // 顺带清理过期条目：抑制窗口外的记录不再需要，防长会话累积（重命名每次新增旧+新两键）
  for (const [p, at] of selfSavedAt) {
    if (now - at >= SELF_SAVE_SUPPRESS_MS) selfSavedAt.delete(p);
  }
  if (path === undefined) {
    globalSelfSaveAt = now;
    return;
  }
  const paths = typeof path === "string" ? [path] : path;
  for (const p of paths) selfSavedAt.set(p, now);
}
