/**
 * 流式引擎增量交付测试（stores/streaming.ts 的 runStreamExchange）。
 * 锁定交付语义：增量经 50ms 节流定时器合并交付——不依赖 rAF/帧管线（独立 WebView 的
 * 可见性状态下 rAF 不触发，增量曾攒到流结束才一次性吐出），且同一窗口内的多个增量
 * 合并为一次回调，不逐 token 打渲染。
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import type { ProviderConfig } from "@/types";

const h = vi.hoisted(() => ({
  /** streamChat 替身的产出脚本：按序执行的动作（增量/等待毫秒/结束/出错）。 */
  script: [] as Array<
    | { kind: "delta"; text: string }
    | { kind: "reasoning"; text: string }
    | { kind: "wait"; ms: number }
    | { kind: "done" }
    | { kind: "error"; message: string }
  >,
}));

vi.mock("@/services/ai/client", () => ({
  STREAM_IDLE_TIMEOUT_MS: 120_000,
  streamChat: vi.fn(async (_req: unknown, handlers: Record<string, (v?: unknown) => void>) => {
    for (const step of h.script) {
      if (step.kind === "delta") handlers.onDelta(step.text);
      else if (step.kind === "reasoning") handlers.onReasoningDelta(step.text);
      else if (step.kind === "wait") await new Promise((r) => setTimeout(r, step.ms));
      else if (step.kind === "done") handlers.onDone("stop");
      else handlers.onError(new Error(step.message));
    }
  }),
}));
vi.mock("./settingsStore", () => ({
  useSettingsStore: { getState: () => ({ resolveAutoNamingModel: () => null }) },
}));
vi.mock("@/services/ai/tools", () => ({
  summarizeAgentTool: (name: string) => name,
  summarizePartialAgentTool: (name: string) => name,
}));

import { runStreamExchange } from "./streaming";

const provider = {
  id: "p1",
  name: "P",
  baseUrl: "http://x.example",
  apiKey: "k",
  models: [],
} as unknown as ProviderConfig;

describe("runStreamExchange 增量交付", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  function makeRun(batches: Array<{ content: string; reasoning: string }>) {
    return runStreamExchange({
      provider,
      model: "m1",
      apiMessages: [{ role: "user", text: "hi" }],
      signal: new AbortController().signal,
      applyBatch: (b) => batches.push(b),
      onError: () => {},
      onDone: () => {},
      executeTools: async () => ({ messages: [], results: [] }),
    });
  }

  it("增量经节流定时器在流结束前交付，全程不触碰帧管线", async () => {
    const raf = vi.fn();
    vi.stubGlobal("requestAnimationFrame", raf);
    h.script = [
      { kind: "delta", text: "第一段" },
      { kind: "wait", ms: 100 },
      { kind: "delta", text: "第二段" },
      { kind: "wait", ms: 100 },
      { kind: "done" },
    ];
    const batches: Array<{ content: string; reasoning: string }> = [];
    const done = makeRun(batches);

    // 第一段发出后 50ms（节流窗口）即交付，远早于流结束（onDone 在 200ms）
    await vi.advanceTimersByTimeAsync(60);
    expect(batches.map((b) => b.content)).toEqual(["第一段"]);

    await vi.advanceTimersByTimeAsync(1000);
    await done;
    expect(batches.map((b) => b.content)).toEqual(["第一段", "第二段"]);
    // 交付只走定时器，不依赖帧管线（rAF 不触发的 WebView 行为一致）
    expect(raf).not.toHaveBeenCalled();
  });

  it("同一节流窗口内的多个增量合并为一次回调，不逐 token 打渲染", async () => {
    h.script = [
      { kind: "delta", text: "你" },
      { kind: "delta", text: "好" },
      { kind: "reasoning", text: "思考" },
      { kind: "done" },
    ];
    const batches: Array<{ content: string; reasoning: string }> = [];
    const done = makeRun(batches);

    await vi.advanceTimersByTimeAsync(1000);
    await done;
    expect(batches).toEqual([{ content: "你好", reasoning: "思考" }]);
  });
});
