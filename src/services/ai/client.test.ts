/**
 * OpenAI 兼容适配器契约测试（services/ai/client）。
 *
 * 核心回归：工具调用参数分片以 `tool-call-delta` 逐片发出（参数进度实时可见、空闲超时可喂狗），
 * 完整调用仍在流末一次性发出（工具执行只认完整调用）；`streamChat` 把增量原样转发给
 * `onToolCallDelta`，`onToolCalls` 仍只在流末触发一次（引擎据此把「生成中」行固化为正式行）。
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { resolveMessageAttachments, streamChat, streamRequest, toLlmMessages } from "./client";
import type { LlmStreamEvent, Message } from "@/types";

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

const REQ = {
  url: "https://example.test/v1/chat/completions",
  apiKey: "key",
  model: "test-model",
  messages: [{ role: "user" as const, text: "hi" }],
};

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("streamRequest 工具调用参数分片", () => {
  it("tool_calls 分片逐片发 tool-call-delta，流末一次性发完整 tool-call", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        sseResponse([
          data({
            choices: [
              { delta: { tool_calls: [{ index: 0, id: "call-1", function: { name: "write_file", arguments: '{"path":' } }] } },
            ],
          }),
          data({
            choices: [{ delta: { tool_calls: [{ index: 0, function: { arguments: '"a.md"}' } }] } }],
          }),
          data({ choices: [{ delta: {}, finish_reason: "tool_calls" }] }),
          "data: [DONE]\n\n",
        ]),
      ),
    );

    const events: LlmStreamEvent[] = [];
    for await (const e of streamRequest(REQ)) events.push(e);

    expect(events).toEqual([
      { type: "tool-call-delta", index: 0, id: "call-1", name: "write_file", argumentsDelta: '{"path":' },
      { type: "tool-call-delta", index: 0, argumentsDelta: '"a.md"}' },
      { type: "tool-call", call: { id: "call-1", name: "write_file", arguments: '{"path":"a.md"}' } },
      { type: "finish", reason: "tool-calls" },
    ]);
  });

  it("纯文本流不受影响（text-delta + finish stop，无工具增量）", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        sseResponse([
          data({ choices: [{ delta: { content: "你" } }] }),
          data({ choices: [{ delta: { content: "好" } }] }),
          data({ choices: [{ delta: {}, finish_reason: "stop" }] }),
          "data: [DONE]\n\n",
        ]),
      ),
    );

    const events: LlmStreamEvent[] = [];
    for await (const e of streamRequest(REQ)) events.push(e);

    expect(events).toEqual([
      { type: "text-delta", text: "你" },
      { type: "text-delta", text: "好" },
      { type: "finish", reason: "stop" },
    ]);
  });
});

describe("streamChat 回调转发", () => {
  it("参数增量转发 onToolCallDelta，完整调用仍只在流末 onToolCalls 一次", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        sseResponse([
          data({
            choices: [
              { delta: { tool_calls: [{ index: 0, id: "call-1", function: { name: "write_file", arguments: '{"path":' } }] } },
            ],
          }),
          data({
            choices: [{ delta: { tool_calls: [{ index: 0, function: { arguments: '"a.md"}' } }] } }],
          }),
          data({ choices: [{ delta: {}, finish_reason: "tool_calls" }] }),
          "data: [DONE]\n\n",
        ]),
      ),
    );

    const deltas: unknown[] = [];
    let toolCalls: Array<{ id: string; name: string; arguments: string }> | undefined;
    let doneReason: string | undefined;
    await streamChat(
      { baseUrl: REQ.url, apiKey: REQ.apiKey, model: REQ.model, messages: [...REQ.messages] },
      {
        onDelta: () => {},
        onError: (e) => {
          throw e;
        },
        onDone: (reason) => {
          doneReason = reason;
        },
        onToolCallDelta: (d) => deltas.push(d),
        onToolCalls: (c) => {
          toolCalls = c;
        },
      },
    );

    expect(deltas).toHaveLength(2);
    expect(toolCalls).toEqual([{ id: "call-1", name: "write_file", arguments: '{"path":"a.md"}' }]);
    expect(doneReason).toBe("tool-calls");
  });
});

describe("附件内容缺失的降级（一条读失败不拖垮整轮）", () => {
  /** 构造带附件的 user 消息。 */
  function userMsg(id: string, attachments: Message["attachments"]): Message {
    return { id, role: "user", content: id, createdAt: 0, attachments };
  }

  it("resolveMessageAttachments：单个附件读失败只丢该附件（补齐回调返回空串即可）", async () => {
    const messages = [
      userMsg("m1", [
        { kind: "image", mime: "image/png", file: "附件/ok.png" },
        { kind: "file", mime: "application/pdf", file: "附件/broken.pdf" },
      ]),
    ];
    const resolved = await resolveMessageAttachments(messages, async (att) =>
      att.file === "附件/ok.png" ? "data:image/png;base64,AAAA" : "",
    );

    expect(resolved[0].attachments?.[0].payload).toBe("data:image/png;base64,AAAA");
    expect(resolved[0].attachments?.[1].payload).toBe("");
    // 空载荷不进请求：只带上成功的那张图
    const llm = toLlmMessages(resolved);
    expect(llm).toEqual([{ role: "user", text: "m1", images: [{ url: "data:image/png;base64,AAAA" }] }]);
  });

  it("toLlmMessages：空 dataURL 不产出 image_url（防整条请求 400）", () => {
    const llm = toLlmMessages([
      userMsg("m1", [
        { kind: "image", mime: "image/png", file: "附件/gone.png", payload: "" },
        { kind: "file", mime: "text/plain", file: "附件/a.txt", payload: "正文" },
      ]),
    ]);

    expect(llm).toEqual([{ role: "user", text: "m1", fileTexts: ["正文"] }]);
  });

  it("resolveMessageAttachments：无缺失时返回原数组引用（调用方据此判是否回写缓存）", async () => {
    const messages = [
      userMsg("m1", [{ kind: "image", mime: "image/png", file: "附件/a.png", payload: "data:image/png;base64,AAAA" }]),
    ];

    const resolved = await resolveMessageAttachments(messages, async () => "不该被调用");

    expect(resolved).toBe(messages);
  });
});
