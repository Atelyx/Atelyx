/**
 * 会话压缩服务契约测试（services/ai/compaction）。
 *
 * 核心回归：压缩指令作为**最后一条 user 消息**追加在既有对话之后（复用前缀缓存），
 * 且失败路径一律不产出半成品——空输出、输出上限截断、请求失败都判失败，让调用方提示重试。
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { runCompaction } from "./compaction";
import { COMPACTION_INSTRUCTION } from "@/constants/compaction";
import type { LlmMessage } from "@/types";

const encoder = new TextEncoder();

/** 构造 SSE 响应（逐帧 enqueue 后关闭）。 */
function sseResponse(frames: string[]): Response {
  return new Response(
    new ReadableStream<Uint8Array>({
      start(controller) {
        for (const frame of frames) controller.enqueue(encoder.encode(frame));
        controller.close();
      },
    }),
    { status: 200 },
  );
}
const data = (json: unknown): string => `data: ${JSON.stringify(json)}\n\n`;

/** 捕获请求体后返回给定 SSE 帧。 */
function stubFetch(frames: string[], capture?: (body: Record<string, unknown>) => void) {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (_url: unknown, init?: RequestInit) => {
      capture?.(JSON.parse(String(init?.body)));
      return sseResponse(frames);
    }),
  );
}

const HISTORY: LlmMessage[] = [{ role: "user", text: "帮我写一段文案" }];

function makeReq(messages: LlmMessage[] = HISTORY) {
  return {
    baseUrl: "https://example.test/v1",
    apiKey: "key",
    model: "test-model",
    messages,
    signal: new AbortController().signal,
  };
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("runCompaction", () => {
  it("指令作为最后一条 user 消息追加在既有对话之后，且带输出上限", async () => {
    let body: Record<string, unknown> | null = null;
    stubFetch(
      [data({ choices: [{ delta: { content: "## 会话主题\n- 写文案" } }] }), "data: [DONE]\n\n"],
      (b) => (body = b),
    );

    const result = await runCompaction(makeReq());

    expect(result).toEqual({ ok: true, summary: "## 会话主题\n- 写文案" });
    const sent = (body as unknown as { messages: Array<{ role: string; content: unknown }> }).messages;
    expect(sent).toHaveLength(2);
    // user 消息线上格式 = content parts 数组（适配器私有翻译）
    expect(sent[0]).toEqual({ role: "user", content: [{ type: "text", text: "帮我写一段文案" }] });
    expect(sent[sent.length - 1].role).toBe("user");
    expect(sent[sent.length - 1].content).toEqual([{ type: "text", text: COMPACTION_INSTRUCTION }]);
    expect((body as unknown as { max_tokens: number }).max_tokens).toBeGreaterThan(0);
  });

  it("空输出判失败（不写入半成品摘要）", async () => {
    stubFetch([data({ choices: [{ delta: {}, finish_reason: "stop" }] }), "data: [DONE]\n\n"]);
    const result = await runCompaction(makeReq());
    expect(result).toEqual({ ok: false, reason: "empty" });
  });

  it("输出上限截断判失败（检查点不完整，采用会掩盖用户意图）", async () => {
    stubFetch([
      data({ choices: [{ delta: { content: "## 会话主题\n- 部分" } }] }),
      data({ choices: [{ delta: {}, finish_reason: "length" }] }),
      "data: [DONE]\n\n",
    ]);
    const result = await runCompaction(makeReq());
    expect(result).toEqual({ ok: false, reason: "truncated" });
  });

  it("请求失败判失败并带回错误", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response("boom", { status: 500 })),
    );
    const result = await runCompaction(makeReq());
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.reason).toBe("failed");
  });

  it("请求前已中止判 aborted（用户主动停止，调用方静默收尾）", async () => {
    stubFetch([data({ choices: [{ delta: { content: "x" } }] }), "data: [DONE]\n\n"]);
    const controller = new AbortController();
    controller.abort();
    const result = await runCompaction({ ...makeReq(), signal: controller.signal });
    expect(result).toEqual({ ok: false, reason: "aborted" });
  });

  it("传入工具名册时一并下发（历史含工具消息时请求结构与最近一次真实请求一致）", async () => {
    let body: Record<string, unknown> | null = null;
    stubFetch(
      [data({ choices: [{ delta: { content: "摘要" } }] }), "data: [DONE]\n\n"],
      (b) => (body = b),
    );
    const tools = [
      { name: "read_file", description: "读文件", parameters: { type: "object", properties: {} } },
    ];
    await runCompaction({ ...makeReq(), tools });
    expect((body as unknown as { tools: unknown[] }).tools).toHaveLength(1);
  });

  it("不传工具名册时请求体不含 tools", async () => {
    let body: Record<string, unknown> | null = null;
    stubFetch(
      [data({ choices: [{ delta: { content: "摘要" } }] }), "data: [DONE]\n\n"],
      (b) => (body = b),
    );
    await runCompaction(makeReq());
    expect(body as unknown as Record<string, unknown>).not.toHaveProperty("tools");
  });
});
