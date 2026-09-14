/**
 * 跨轮工具历史重建契约测试（utils/agentHistory）。
 *
 * 核心回归：上一轮工具调用的结果**必须**随下一轮请求重发——否则模型每轮都「没读过」，
 * 会反复重读同一文件。同时守住线协议不变式：每个 tool 消息都有前置同 id 的 tool_calls
 * （否则部分网关 400）。
 */
import { describe, expect, it } from "vitest";
import {
  expandAgentStepsToLlmMessages,
  planOverflowRetry,
  pruneToolResultText,
} from "./agentHistory";
import { PENDING_RUN_ID_PREFIX } from "@/constants/chat";
import {
  TOOL_RESULT_PRUNE_HEAD_CHARS,
  TOOL_RESULT_PRUNE_MARKER,
  TOOL_RESULT_PRUNE_TAIL_CHARS,
  TOOL_RESULT_PRUNE_THRESHOLD_CHARS,
} from "@/constants/tools";
import type { AgentStep, LlmMessage, ToolRun } from "@/types";

function run(over: Partial<ToolRun> = {}): ToolRun {
  return {
    id: "call-1",
    name: "read_file",
    argsSummary: "读取 笔记/a.md",
    status: "done",
    resultSummary: "已读取（3 行）",
    args: '{"path":"笔记/a.md"}',
    result: "1: 正文\n2: 更多",
    ...over,
  };
}
const tool = (over: Partial<ToolRun> = {}): AgentStep => ({ kind: "tool", run: run(over) });
const R = (text: string): AgentStep => ({ kind: "reasoning", text });
const T = (text: string): AgentStep => ({ kind: "text", text });

describe("expandAgentStepsToLlmMessages", () => {
  it("回归：上一轮 read_file 的调用与结果都进入后续请求历史", () => {
    const msgs = expandAgentStepsToLlmMessages("已读取文件", [tool()]);
    expect(msgs).toEqual([
      {
        role: "assistant",
        text: null,
        toolCalls: [{ id: "call-1", name: "read_file", arguments: '{"path":"笔记/a.md"}' }],
      },
      { role: "tool", text: "1: 正文\n2: 更多", toolCallId: "call-1" },
      { role: "assistant", text: "已读取文件" },
    ]);
  });

  it("每个 tool 消息都有**前置**同 id 的 tool_calls（线协议不变式）", () => {
    const msgs = expandAgentStepsToLlmMessages("答", [
      T("先看看"),
      tool({ id: "c1" }),
      R("再读一个"),
      tool({ id: "c2", name: "grep", args: "{}" }),
    ]);
    const openedIds = new Set<string>();
    for (const m of msgs) {
      if (m.role === "assistant") for (const c of m.toolCalls ?? []) openedIds.add(c.id);
      // 顺序关系：tool 消息出现时其调用必须已经被前面的 assistant 发出
      if (m.role === "tool") expect(openedIds.has(m.toolCallId as string)).toBe(true);
    }
    expect(msgs.some((m) => m.role === "tool")).toBe(true);
  });

  it("叙述随其所属轮次作为 assistant 正文发出，不并进最终回答", () => {
    const msgs = expandAgentStepsToLlmMessages("最终答复", [T("我先读取文件"), tool()]);
    expect(msgs[0]).toMatchObject({ role: "assistant", text: "我先读取文件" });
    expect(msgs[msgs.length - 1]).toEqual({ role: "assistant", text: "最终答复" });
  });

  it("思考步不进上下文", () => {
    const msgs = expandAgentStepsToLlmMessages("答复", [R("内部推理"), tool()]);
    expect(JSON.stringify(msgs)).not.toContain("内部推理");
  });

  it("无 steps 退回单条 assistant 正文", () => {
    expect(expandAgentStepsToLlmMessages("普通回复")).toEqual([
      { role: "assistant", text: "普通回复" },
    ]);
  });

  it("参数缺失回退空对象 JSON（缺字段的 tool_calls 会被部分网关 400）", () => {
    const msgs = expandAgentStepsToLlmMessages("", [tool({ args: undefined })]);
    expect(msgs[0].role === "assistant" && msgs[0].toolCalls?.[0].arguments).toBe("{}");
  });

  it("error 行携带结果文本（失败也要让模型知道）", () => {
    const msgs = expandAgentStepsToLlmMessages("", [
      tool({ status: "error", result: undefined, resultSummary: "读取失败：文件不存在" }),
    ]);
    expect(msgs[1]).toEqual({
      role: "tool",
      text: "读取失败：文件不存在",
      toolCallId: "call-1",
    });
  });

  it("跳过参数生成中的合成行（无真实 id，无法与 tool_calls 配对）", () => {
    const msgs = expandAgentStepsToLlmMessages("答复", [
      T("叙述"),
      tool({ id: `${PENDING_RUN_ID_PREFIX}1:0`, name: "read_file", result: undefined }),
    ]);
    // 该轮无有效调用：叙述降级为普通 assistant 文本，不产出孤立 tool 消息
    expect(msgs).toEqual([
      { role: "assistant", text: "叙述" },
      { role: "assistant", text: "答复" },
    ]);
  });

  it("无名工具行同样跳过", () => {
    const msgs = expandAgentStepsToLlmMessages("答复", [tool({ name: "" })]);
    expect(msgs.some((m) => m.role === "tool")).toBe(false);
  });

  it("叙述-only 消息（content 由叙述步回填）不重复发出叙述", () => {
    const msgs = expandAgentStepsToLlmMessages("我先读取文件", [T("我先读取文件"), tool()]);
    const texts = msgs.filter((m) => m.role === "assistant").map((m) => m.text);
    expect(texts.filter((t) => t === "我先读取文件")).toHaveLength(1);
  });

  it("正文为空但带工具步（工具轮中止/无最终回答）：只发工具消息，不发空正文", () => {
    const msgs = expandAgentStepsToLlmMessages("", [tool()]);
    expect(msgs.some((m) => m.role === "assistant" && m.text === "")).toBe(false);
    expect(msgs.map((m) => m.role)).toEqual(["assistant", "tool"]);
  });

  it("仅思考步、正文为空：不产出任何消息（空 content 会被部分端点 400）", () => {
    expect(expandAgentStepsToLlmMessages("", [R("只有推理")])).toEqual([]);
  });
});
describe("pruneToolResultText", () => {
  it("未超阈值原样返回", () => {
    const text = "x".repeat(TOOL_RESULT_PRUNE_THRESHOLD_CHARS);
    expect(pruneToolResultText(text)).toBe(text);
  });

  it("超阈值保留头尾并插入省略标记（总长显著变小且不超阈值）", () => {
    const text = "x".repeat(TOOL_RESULT_PRUNE_THRESHOLD_CHARS + 5000);
    const out = pruneToolResultText(text);
    expect(out).toContain(TOOL_RESULT_PRUNE_MARKER);
    expect(out.length).toBeLessThan(text.length);
    expect(out.length).toBe(
      TOOL_RESULT_PRUNE_HEAD_CHARS + TOOL_RESULT_PRUNE_MARKER.length + TOOL_RESULT_PRUNE_TAIL_CHARS,
    );
    // 折叠结果必须小于阈值（否则折叠完仍超预算，白折）
    expect(out.length).toBeLessThan(TOOL_RESULT_PRUNE_THRESHOLD_CHARS);
  });

  it("按码点切分不切断代理对", () => {
    const text = "😀".repeat(TOOL_RESULT_PRUNE_THRESHOLD_CHARS);
    const out = pruneToolResultText(text);
    expect(out.startsWith("😀")).toBe(true);
    expect(out.endsWith("😀")).toBe(true);
    // 若按 UTF-16 码元切分会产出落单代理项（\uD83D 无后续 \uDE00）
    expect(/[\uD800-\uDBFF](?![\uDC00-\uDFFF])/.test(out)).toBe(false);
  });
});

describe("planOverflowRetry", () => {
  it("只折叠超长 tool 消息，报告出现变化", () => {
    const messages: LlmMessage[] = [
      { role: "assistant", text: "问" },
      { role: "tool", text: "x".repeat(TOOL_RESULT_PRUNE_THRESHOLD_CHARS + 1), toolCallId: "c1" },
    ];
    const plan = planOverflowRetry(messages);
    expect(plan.changed).toBe(true);
    expect((plan.messages[1] as { text: string }).text).toContain(TOOL_RESULT_PRUNE_MARKER);
    expect(plan.messages[0]).toBe(messages[0]);
  });

  it("无超长结果时原样返回（调用方不应重试）", () => {
    const messages: LlmMessage[] = [{ role: "tool", text: "短", toolCallId: "c1" }];
    const plan = planOverflowRetry(messages);
    expect(plan.changed).toBe(false);
    expect(plan.messages).toBe(messages);
  });
});
