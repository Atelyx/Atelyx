/**
 * 协作换路的 watcher 回波抑制（笔记改名/移动）。
 *
 * 共享盘上改名方落盘后发换路帧，本端据帧把打开的同一笔记切到新路径；同一变更在共享盘上
 * 也表现为「旧路径消失、新路径出现」，本端 watcher 稍后同样报一次。窗口内按帧的结果跳过
 * 外部修改处理——否则刚跟上的编辑面会被判成「外部改盘」或「已删除」打回。
 *
 * 路径级（旧 + 新）且带窗口，仿 `utils/canvasCollab.ts` 的 `markCollabCanvasRename`；
 * 与 `utils/selfSave` 区分：那是本端自写回波，这是对端变更已由帧接管。
 */
const COLLAB_RELOCATE_SUPPRESS_MS = 10_000;
const relocatedAt = new Map<string, number>();

/** 登记对端换路涉及的路径（旧 + 新）；顺带清理过期条目，防长会话累积。 */
export function markCollabNoteRelocate(paths: string[]): void {
  const now = Date.now();
  for (const [p, at] of relocatedAt) {
    if (now - at >= COLLAB_RELOCATE_SUPPRESS_MS) relocatedAt.delete(p);
  }
  for (const p of paths) relocatedAt.set(p, now);
}

/** watcher 笔记分支判断该路径事件是否为协作换路的回波（窗口内 = 跳过外部修改处理）。 */
export function isCollabNoteRelocatePath(path: string): boolean {
  return Date.now() - (relocatedAt.get(path) ?? 0) < COLLAB_RELOCATE_SUPPRESS_MS;
}
