/**
 * 会话压缩注解（用户手动触发）：把较早的对话总结成一条检查点摘要，边界之前不再进入模型请求。
 *
 * 非破坏性——消息正文原样保留在 `.atlx` / 会话 `.jsonl`，仅重建请求历史时按注解裁剪；
 * 锚点消息缺失（被回滚/分支丢弃）即判定注解失效，退回完整历史（见 `splitByCompaction`）。
 */
export interface ConversationCompaction {
  /** 模型生成的结构化检查点摘要原文（不含框架包裹，包裹在重建历史时施加）。 */
  summary: string;
  /** 压缩覆盖到的最后一条消息 id（该条及其之前不进模型历史）。 */
  upToMessageId: string;
  /** 被覆盖的消息条数（标记行展示；注解失效判定不依赖它）。 */
  messageCount: number;
  /** 生成时间（标记行展示）。 */
  createdAt: number;
  /** 生成摘要所用的供应商 id（展示溯源；缺省 = 未记录）。 */
  providerId?: string;
  /** 生成摘要所用的模型 id（展示溯源；缺省 = 未记录）。 */
  model?: string;
}
