/**
 * 会话压缩（用户手动触发）：把较早的对话交给模型总结成一份结构化检查点摘要。
 *
 * 压缩本身是**建议性**的：这里只负责生成摘要文本，是否采用、覆盖到哪条消息由调用方（store）决定；
 * 摘要为空、被输出上限截断、请求失败或被中止一律判失败——不静默写入半成品。
 * 指令作为最后一条 user 消息追加在既有对话之后（是最近一次已路由请求的前缀，可命中供应商 KV 缓存）。
 */
import { streamChat } from "@/services/ai/client";
import { COMPACTION_INSTRUCTION, COMPACTION_MAX_TOKENS } from "@/constants/compaction";
import type { LlmFinishReason, LlmMessage, ToolSchema } from "@/types";

/** 压缩失败原因：中止（不算失败，静默）/ 空输出 / 触顶截断 / 请求失败。 */
export type CompactionFailureReason = "aborted" | "empty" | "truncated" | "failed";

export type CompactionResult =
  | { ok: true; summary: string }
  | { ok: false; reason: CompactionFailureReason; error?: Error };

export interface CompactionRequest {
  baseUrl: string;
  apiKey: string;
  model: string;
  /** 待压缩的既有对话（已按中性词汇组装，可含工具消息）。 */
  messages: LlmMessage[];
  /**
   * 与当前会话一致的工具名册：历史含 `tool_calls`/`tool` 消息时须一并带上，
   * 请求结构才与最近一次真实请求一致（前缀缓存命中 + 严格的兼容网关不会因「引用了未声明的工具」拒绝）。
   */
  tools?: ToolSchema[];
  signal: AbortSignal;
}

/** 生成检查点摘要。 */
export async function runCompaction(params: CompactionRequest): Promise<CompactionResult> {
  const requestMessages: LlmMessage[] = [
    ...params.messages,
    { role: "user", text: COMPACTION_INSTRUCTION },
  ];
  let text = "";
  let finish: LlmFinishReason | undefined;
  let failure: Error | null = null;
  await streamChat(
    {
      baseUrl: params.baseUrl,
      apiKey: params.apiKey,
      model: params.model,
      messages: requestMessages,
      // 不传 temperature：摘要不需要采样自由度
      maxTokens: COMPACTION_MAX_TOKENS,
      ...(params.tools?.length ? { tools: params.tools } : {}),
      signal: params.signal,
    },
    {
      onDelta: (delta) => {
        text += delta;
      },
      onDone: (reason) => {
        finish = reason;
      },
      onError: (err) => {
        failure = err;
      },
    },
  );

  if (params.signal.aborted) return { ok: false, reason: "aborted" };
  if (failure) return { ok: false, reason: "failed", error: failure };
  // 触顶截断的检查点不完整（后面小节缺失），采用会掩盖用户意图，判失败让用户重试
  if (finish === "max-tokens") return { ok: false, reason: "truncated" };
  const summary = text.trim();
  if (!summary) return { ok: false, reason: "empty" };
  return { ok: true, summary };
}
