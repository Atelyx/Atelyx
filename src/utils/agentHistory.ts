/**
 * AI 对话历史的跨轮重建：把内部消息的 Agent 步进（`steps`）还原为线上工具消息序列。
 *
 * 工具消息不在消息模型里持有，重建请求历史时按 `steps` 还原（`ToolRun.result` 即完整结果文本）——
 * 否则模型看不到上一轮读过什么，会反复重读同一文件。
 * 纯数据操作，画布与面板共用；思考步不进上下文（与展示口径一致）。
 */
import type { AgentStep, LlmMessage, LlmToolCall, ToolRun } from "@/types";
import { assistantReplyText, groupAgentSteps } from "./agentSteps";
import { PENDING_RUN_ID_PREFIX } from "@/constants/chat";
import {
  TOOL_RESULT_PRUNE_HEAD_CHARS,
  TOOL_RESULT_PRUNE_MARKER,
  TOOL_RESULT_PRUNE_TAIL_CHARS,
  TOOL_RESULT_PRUNE_THRESHOLD_CHARS,
} from "@/constants/tools";

/** 该工具步是否可进上下文：参数生成中的合成行没有真实 id，无法与 tool_calls 配对。 */
function isSendableRun(run: ToolRun): boolean {
  return !!run.name && !run.id.startsWith(PENDING_RUN_ID_PREFIX);
}

/**
 * 一条 assistant 内部消息 → 线上消息序列：每轮发 `assistant(tool_calls)` + 对应的 `tool` 结果，
 * 末尾补最终回答正文。
 *
 * 不变式：**每个 tool 消息都紧跟其所属的 assistant tool_calls**（同一轮内成对发出）——
 * 线协议要求 tool 消息必须能对应到前置调用；某轮无有效调用时把叙述降级为普通 assistant 文本，
 * 不产出孤立的 tool 消息。
 */
export function expandAgentStepsToLlmMessages(
  content: string,
  steps?: AgentStep[],
): LlmMessage[] {
  if (!steps?.length) return [{ role: "assistant", text: content }];

  const out: LlmMessage[] = [];
  for (const group of groupAgentSteps(steps)) {
    // 思考（reasoning）不进上下文；叙述（text）是本轮模型说的话，随该轮一起回填
    const narration = group.thinkings
      .filter((t): t is { kind: "text"; text: string } => t.kind === "text")
      .map((t) => t.text)
      .join("\n");
    const runs = group.tools.filter(isSendableRun);
    if (runs.length === 0) {
      // 纯思考组 / 仅参数未生成完整的合成行：叙述降级为普通 assistant 文本
      if (narration) out.push({ role: "assistant", text: narration });
      continue;
    }
    const toolCalls: LlmToolCall[] = runs.map((r) => ({
      id: r.id,
      name: r.name,
      // 参数缺失（旧数据/异常落盘）发空对象：缺字段的 tool_calls 会被部分网关 400
      arguments: r.args ?? "{}",
    }));
    // assistant 带 tool_calls 时 content 为 null（OpenAI 规范，空串会被部分网关 500）
    out.push({ role: "assistant", text: narration || null, toolCalls });
    for (const r of runs) {
      out.push({
        role: "tool",
        // 结果缺失（异常落盘/未回填的旧行）给中性占位：空 tool content 会被部分网关 400
        text: r.result ?? r.resultSummary ?? "（无结果）",
        toolCallId: r.id,
      });
    }
  }

  // 最终回答单独收尾：与全量叙述拼接相同 = 叙述-only 消息按 content 回填的结果，
  // 各轮叙述已分别发出，再发一次会重复。
  if (content.trim() && content !== assistantReplyText({ content: "", steps })) {
    out.push({ role: "assistant", text: content });
  }
  return out;
}

/**
 * 折叠单条工具结果的中段（超阈值才折叠）：保留头尾给出线索，中间替换为省略标记。
 * 按码点切分，不切断代理对。
 */
export function pruneToolResultText(text: string): string {
  // 码元数 ≤ 码点数，先用码元数廉价短路——长历史下避免为每条结果展开整个码点数组
  if (text.length <= TOOL_RESULT_PRUNE_THRESHOLD_CHARS) return text;
  const points = Array.from(text);
  if (points.length <= TOOL_RESULT_PRUNE_THRESHOLD_CHARS) return text;
  const head = points.slice(0, TOOL_RESULT_PRUNE_HEAD_CHARS).join("");
  const tail = points.slice(points.length - TOOL_RESULT_PRUNE_TAIL_CHARS).join("");
  return `${head}${TOOL_RESULT_PRUNE_MARKER}${tail}`;
}

/**
 * 上下文溢出时的轻量先手：折叠超长的历史工具结果后重试（不删消息，工具配对天然保持）。
 * 返回 `changed` = 确有折叠（无变化时调用方不应重试，避免原样再撞一次）。
 */
export function planOverflowRetry(messages: LlmMessage[]): {
  messages: LlmMessage[];
  changed: boolean;
} {
  let changed = false;
  const out = messages.map((m) => {
    if (m.role !== "tool") return m;
    const text = m.text ?? "";
    const pruned = pruneToolResultText(text);
    if (pruned === text) return m;
    changed = true;
    return { ...m, text: pruned };
  });
  return { messages: changed ? out : messages, changed };
}
