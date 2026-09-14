/**
 * 会话压缩注解的纯数据操作：注解 → 模型可见历史 / 渲染标记位（画布与面板共用）。
 *
 * 注解非破坏性——消息本体不删改，仅在重建请求历史时按注解把覆盖区间换成摘要。
 * 锚点消息缺失（被回滚/分支丢弃）一律视为注解失效，退回完整历史，宁可多发也不静默丢内容。
 */
import type { ConversationCompaction } from "@/types";

/**
 * 按压缩注解切分历史：注解锚点（覆盖到的最后一条消息）及其之前不进模型历史，
 * 之后的原样保留。锚点缺失 = 注解失效，退回完整历史；
 * 返回的 `checkpoint` 供调用方在最前注入摘要。
 */
export function splitByCompaction<T extends { id: string }>(
  messages: T[],
  compaction?: ConversationCompaction | null,
): { kept: T[]; checkpoint: ConversationCompaction | null } {
  if (!compaction) return { kept: messages, checkpoint: null };
  const anchor = messages.findIndex((m) => m.id === compaction.upToMessageId);
  if (anchor < 0) return { kept: messages, checkpoint: null };
  return { kept: messages.slice(anchor + 1), checkpoint: compaction };
}

/**
 * 手动压缩的边界：把**当前全部消息（含最新一轮）**折进检查点，模型续聊时以该摘要为背景。
 * 待压缩范围 = 列表全部，故标记行落在折叠块末尾（刚压缩完即列表最下方），之后的新消息接在其后。
 * 返回 null = 不足一轮问答，或已覆盖到最后一条（无新增可压缩内容）。
 */
export function nextCompactionBoundary(
  messages: Array<{ id: string }>,
  current?: ConversationCompaction | null,
): { upToMessageId: string; messageCount: number } | null {
  // 至少一轮问答（user + assistant）才有摘要意义
  if (messages.length < 2) return null;
  const last = messages[messages.length - 1];
  // 边界只前进：已覆盖到最后一条即无新增；锚点缺失（被回滚丢弃）视为失效，允许重算
  if (current && current.upToMessageId === last.id) return null;
  return { upToMessageId: last.id, messageCount: messages.length };
}

/**
 * 压缩标记行的插入位置（原始消息列表下标）：锚点之后的第一条。返回 -1 = 无注解或注解失效
 * （锚点已被回滚/分支丢弃）——与 `splitByCompaction` 同一判定，标记行与模型历史口径一致。
 * 等于 `messages.length` = 压缩覆盖到最后一条，标记行渲染在列表末尾。
 */
export function compactionMarkerIndex<T extends { id: string }>(
  messages: T[],
  compaction?: ConversationCompaction | null,
): number {
  if (!compaction) return -1;
  const anchor = messages.findIndex((m) => m.id === compaction.upToMessageId);
  return anchor < 0 ? -1 : anchor + 1;
}
