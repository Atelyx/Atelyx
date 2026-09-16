/**
 * 对话核心运行时契约测试（stores/chatTurn.ts）。
 *
 * 锁定一轮对话的编排时序（本测试只关心编排，流式引擎与模型调用以替身驱动）：
 * 请求组装（系统提示词 / 工具名册 / 尾部上下文块）→ 流式产出经写入器快照交回 →
 * 收尾判定（空回复移除 / 超时降级 / 截断提示 / 叙述提升）→ 工具能力与钩子 → 轮末命名与命名让路。
 */
import { describe, expect, it, beforeEach, vi } from "vitest";
import type {
  AgentStep,
  ChatNamingTarget,
  ChatTurnRequest,
  ChatTurnSink,
  ChatTurnTarget,
  ToolRun,
} from "@/types";

interface EngineOptions {
  apiMessages: Array<{ role: string; text: string }>;
  tools?: Array<{ name: string }>;
  applyBatch: (b: { content: string; reasoning: string }) => void;
  onToolRuns: (runs: ToolRun[]) => void;
  onNarration: (text: string) => void;
  onError: (e: Error) => void;
  onDone: (r: {
    content: string;
    reasoning: string;
    timedOut: boolean;
    truncated: boolean;
    promoteNarration: boolean;
  }) => void;
  executeTools: (calls: unknown[]) => Promise<unknown>;
}

const h = vi.hoisted(() => ({
  scenario: null as null | ((o: never) => void | Promise<void>),
  engineOptions: null as unknown,
  aborted: [] as string[],
  naming: [] as string[],
  events: [] as Array<[string, Record<string, unknown>]>,
  agentRequest: undefined as unknown,
  agentRejects: false,
  currentNote: null as string | null,
  toolExec: null as unknown,
  toolHooks: null as unknown,
}));

vi.mock("./streaming", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./streaming")>();
  return {
    ...actual,
    runStreamExchange: async (opts: EngineOptions) => {
      h.engineOptions = opts;
      await h.scenario?.(opts as never);
    },
    // 命名管线自身的行为在别处覆盖；此处只锁定「谁来触发、带什么 key」
    runAutoNaming: async (_target: ChatNamingTarget, opts?: { key?: string }) => {
      h.naming.push(opts?.key ?? "");
      return "ok" as const;
    },
  };
});

vi.mock("@/services/ai/autoTitle", () => ({
  abortAutoTitle: (key?: string) => {
    h.aborted.push(key ?? "");
  },
}));

vi.mock("@/services/cordis/events", () => ({
  emitPluginEvent: (name: string, payload: Record<string, unknown>) => {
    h.events.push([name, payload]);
  },
}));

vi.mock("@/services/vault/agentTodos", () => ({
  currentTodosBlock: () => "",
  readAgentTodos: async () => [],
  writeAgentTodos: async () => {},
}));

vi.mock("@/services/ai/tools", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/services/ai/tools")>();
  return {
    ...actual,
    runAgentTools: async (_calls: unknown[], exec: unknown, hooks?: unknown) => {
      h.toolExec = exec;
      h.toolHooks = hooks;
      return { messages: [], results: [] };
    },
  };
});

vi.mock("./settingsStore", () => ({
  useSettingsStore: {
    getState: () => ({
      resolveAgentRequest: async () => {
        if (h.agentRejects) throw new Error("Agent 配置读取失败");
        return h.agentRequest;
      },
      searchConfig: {},
    }),
  },
}));

vi.mock("./appStore", () => ({
  useAppStore: {
    getState: () => ({ currentNoteFile: h.currentNote, currentNoteTitle: "笔记标题" }),
  },
}));

vi.mock("./vaultStore", () => ({
  useVaultStore: { getState: () => ({}) },
}));

type ChatTurn = typeof import("./chatTurn");

let runChatTurn: ChatTurn["runChatTurn"];
let compactChatTurn: ChatTurn["compactChatTurn"];

const TARGET: ChatTurnTarget = {
  provider: { id: "p1", name: "P", baseUrl: "http://x", apiKey: "k", models: [] },
  model: "m1",
};

/** 记录型写入器：核心的每次交回都留痕，供断言收尾判定与中间态快照。 */
function recorder() {
  const updates: Array<{ content: string; steps: AgentStep[] }> = [];
  const finishes: Array<Record<string, unknown>> = [];
  const failures: Error[] = [];
  const notices: string[] = [];
  const sink: ChatTurnSink = {
    update: (state) => updates.push(state),
    finish: (result) => finishes.push(result as unknown as Record<string, unknown>),
    fail: (error) => failures.push(error),
    notice: (message) => notices.push(message),
  };
  return { sink, updates, finishes, failures, notices };
}

const NAMING: ChatNamingTarget = {
  getMessages: () => [],
  isNamed: () => false,
  applyTitle: () => {},
};

function request(over: Partial<ChatTurnRequest>): ChatTurnRequest {
  return {
    targetId: "s1",
    target: TARGET,
    history: [
      { id: "m1", role: "user", content: "问题", displayContent: "问题" },
    ],
    signal: new AbortController().signal,
    sink: recorder().sink,
    naming: NAMING,
    ...over,
  };
}

const AGENT_CHAT = { systemPrompt: "你是助手", tools: [], skippedWebSearch: false };

beforeEach(async () => {
  vi.resetModules();
  h.scenario = null;
  h.engineOptions = null;
  h.aborted = [];
  h.naming = [];
  h.events = [];
  h.agentRequest = AGENT_CHAT;
  h.agentRejects = false;
  h.currentNote = null;
  h.toolExec = null;
  h.toolHooks = null;
  const mod = await import("./chatTurn");
  runChatTurn = mod.runChatTurn;
  compactChatTurn = mod.compactChatTurn;
});

describe("runChatTurn 一轮对话编排", () => {
  it("流式产出经写入器快照交回，收尾判定写回最终正文与步骤", async () => {
    h.scenario = (o) => {
      const opts = o as unknown as EngineOptions;
      opts.applyBatch({ content: "你好", reasoning: "想一下" });
      opts.onNarration("先说一句");
      opts.onToolRuns([
        { id: "t1", name: "read_file", argsSummary: "读文件", status: "done" },
      ]);
      opts.applyBatch({ content: "，世界", reasoning: "" });
      opts.onDone({
        content: "你好，世界",
        reasoning: "想一下",
        timedOut: false,
        truncated: false,
        promoteNarration: false,
      });
    };
    const r = recorder();
    await runChatTurn(request({ sink: r.sink }));

    // 中间态快照：正文与步骤（思考/叙述/工具交错）逐帧累积
    expect(r.updates.at(-1)?.content).toBe("你好，世界");
    expect(r.updates.at(-1)?.steps.map((s) => s.kind)).toEqual([
      "reasoning",
      "text",
      "tool",
    ]);
    // 收尾：保留占位，正文与步骤为 finalize 结果
    expect(r.finishes).toEqual([
      {
        content: "你好，世界",
        steps: [
          { kind: "reasoning", text: "想一下" },
          { kind: "text", text: "先说一句" },
          {
            kind: "tool",
            run: { id: "t1", name: "read_file", argsSummary: "读文件", status: "done" },
          },
        ],
        removed: false,
        timedOut: false,
        aborted: false,
      },
    ]);
    expect(r.failures).toEqual([]);
  });

  it("请求组装：系统提示词置首、历史按中性消息转换、易变上下文只走尾部块", async () => {
    h.currentNote = "笔记/甲.md";
    h.agentRequest = {
      systemPrompt: "你是助手",
      tools: [{ name: "read_file", description: "读", parameters: {} }],
      skippedWebSearch: false,
    };
    h.scenario = (o) => {
      (o as unknown as EngineOptions).onDone({
        content: "回答",
        reasoning: "",
        timedOut: false,
        truncated: false,
        promoteNarration: false,
      });
    };
    await runChatTurn(request({ includeCurrentNote: true }));

    const opts = h.engineOptions as EngineOptions;
    expect(opts.apiMessages[0].role).toBe("system");
    expect(opts.apiMessages[0].text).toContain("你是助手");
    // 尾部上下文块折叠进末条 user 消息（不进系统提示词，保前缀缓存）
    expect(opts.apiMessages.at(-1)?.role).toBe("user");
    expect(opts.apiMessages.at(-1)?.text).toContain("笔记/甲.md");
    expect(opts.tools?.map((t) => t.name)).toEqual(["read_file"]);
  });

  it("未声明 includeCurrentNote 时不注入当前笔记块", async () => {
    h.currentNote = "笔记/甲.md";
    h.agentRequest = {
      systemPrompt: "你是助手",
      tools: [{ name: "read_file", description: "读", parameters: {} }],
      skippedWebSearch: false,
    };
    h.scenario = (o) => {
      (o as unknown as EngineOptions).onDone({
        content: "回答",
        reasoning: "",
        timedOut: false,
        truncated: false,
        promoteNarration: false,
      });
    };
    await runChatTurn(request({}));
    expect((h.engineOptions as EngineOptions).apiMessages.at(-1)?.text).not.toContain("笔记/甲.md");
  });

  it("搜索源未配置：经写入器提示，不中断本轮", async () => {
    h.agentRequest = { systemPrompt: "S", tools: [], skippedWebSearch: true };
    h.scenario = (o) => {
      (o as unknown as EngineOptions).onDone({
        content: "回答",
        reasoning: "",
        timedOut: false,
        truncated: false,
        promoteNarration: false,
      });
    };
    const r = recorder();
    await runChatTurn(request({ sink: r.sink }));
    expect(r.notices).toHaveLength(1);
    expect(r.notices[0]).toContain("联网搜索");
    expect(r.finishes).toHaveLength(1);
  });

  it("空回复：判定为移除占位（消费方据此删气泡），且不发 assistant 消息事件", async () => {
    h.scenario = (o) => {
      (o as unknown as EngineOptions).onDone({
        content: "",
        reasoning: "",
        timedOut: false,
        truncated: false,
        promoteNarration: false,
      });
    };
    const r = recorder();
    await runChatTurn(request({ sink: r.sink }));
    expect(r.finishes[0]?.removed).toBe(true);
    expect(h.events.filter(([n]) => n === "chat:message").map(([, p]) => p.role)).toEqual([
      "user",
    ]);
  });

  it("超时且无产出：写超时降级文案；截断则附截断提示", async () => {
    h.scenario = (o) => {
      const opts = o as unknown as EngineOptions;
      opts.onDone({
        content: "",
        reasoning: "想了但没答",
        timedOut: true,
        truncated: false,
        promoteNarration: false,
      });
    };
    const timeout = recorder();
    await runChatTurn(request({ sink: timeout.sink }));
    expect(String(timeout.finishes[0]?.content)).toContain("[错误]");
    expect(timeout.finishes[0]?.removed).toBe(false);

    h.scenario = (o) => {
      (o as unknown as EngineOptions).onDone({
        content: "半截回答",
        reasoning: "",
        timedOut: false,
        truncated: true,
        promoteNarration: false,
      });
    };
    const truncated = recorder();
    await runChatTurn(request({ sink: truncated.sink }));
    expect(String(truncated.finishes[0]?.content)).toContain("截断");
  });

  it("引擎报错：经写入器交回失败，轮次结束事件与轮末命名照常", async () => {
    h.scenario = (o) => {
      (o as unknown as EngineOptions).onError(new Error("网络断了"));
    };
    const r = recorder();
    await runChatTurn(request({ sink: r.sink }));
    expect(r.failures.map((e) => e.message)).toEqual(["网络断了"]);
    expect(r.finishes).toEqual([]);
    expect(h.events.map(([n]) => n)).toEqual([
      "chat:started",
      "chat:message",
      "chat:finished",
    ]);
    expect(h.naming).toEqual(["s1"]);
  });

  it("编排侧异常经写入器交回失败、不外抛（消费方的流式态与错误占位必有归宿）", async () => {
    h.agentRejects = true;
    h.scenario = () => {
      throw new Error("不应进入引擎");
    };
    const r = recorder();
    await expect(runChatTurn(request({ sink: r.sink }))).resolves.toBeUndefined();
    expect(r.failures.map((e) => e.message)).toEqual(["Agent 配置读取失败"]);
    expect(r.finishes).toEqual([]);
    // 轮次未真正开始（请求未发出）也收敛：订阅方不会等不到 chat:finished
    expect(h.events.map(([n]) => n)).toEqual(["chat:finished"]);
  });

  it("轮次开始中止同目标的在途命名请求（让路），轮末按同 key 再触发命名", async () => {
    h.scenario = (o) => {
      const opts = o as unknown as EngineOptions;
      // 收尾判定读的是「经写入器已落进容器」的内容，故先走增量再收尾（与真实链路同序）
      opts.applyBatch({ content: "答", reasoning: "" });
      opts.onDone({
        content: "答",
        reasoning: "",
        timedOut: false,
        truncated: false,
        promoteNarration: false,
      });
    };
    await runChatTurn(request({}));
    expect(h.aborted).toEqual(["s1"]);
    expect(h.naming).toEqual(["s1"]);
    expect(h.events.map(([n, p]) => [n, p.targetId])).toEqual([
      ["chat:started", "s1"],
      ["chat:message", "s1"],
      ["chat:message", "s1"],
      ["chat:finished", "s1"],
    ]);
  });

  it("工具能力：标准集交给执行器，消费方覆盖同名方法与产物钩子", async () => {
    h.agentRequest = {
      systemPrompt: "S",
      tools: [{ name: "read_file", description: "读", parameters: {} }],
      skippedWebSearch: false,
    };
    const override = () => {};
    h.scenario = async (o) => {
      const opts = o as unknown as EngineOptions;
      await opts.executeTools([]);
      opts.onDone({
        content: "答",
        reasoning: "",
        timedOut: false,
        truncated: false,
        promoteNarration: false,
      });
    };
    const onToolResult = () => {};
    await runChatTurn(
      request({
        hooks: {
          capabilities: (standard) => {
            expect(standard.readFile).toBeDefined();
            expect(standard.writeTodos).toBeDefined();
            return { readFile: override as never };
          },
          onToolResult,
        },
      }),
    );
    const exec = h.toolExec as { capabilities: { readFile: unknown } };
    expect(exec.capabilities.readFile).toBe(override);
    expect((h.toolHooks as { onToolResult: unknown }).onToolResult).toBe(onToolResult);
  });

  it("压缩切分：锚点及其之前的消息不进请求，摘要以 user 消息置系统提示词之后", async () => {
    h.scenario = (o) => {
      (o as unknown as EngineOptions).onDone({
        content: "答",
        reasoning: "",
        timedOut: false,
        truncated: false,
        promoteNarration: false,
      });
    };
    await runChatTurn(
      request({
        history: [
          { id: "m1", role: "user", content: "旧问题" },
          { id: "m2", role: "assistant", content: "旧回答" },
          { id: "m3", role: "user", content: "新问题" },
        ],
        compaction: {
          summary: "更早的摘要",
          upToMessageId: "m2",
          messageCount: 2,
          createdAt: 1,
          providerId: "p1",
          model: "m1",
        },
      }),
    );
    const msgs = (h.engineOptions as EngineOptions).apiMessages;
    expect(msgs.map((m) => m.role)).toEqual(["system", "user", "user"]);
    expect(msgs[1].text).toContain("更早的摘要");
    expect(msgs[2].text).toBe("新问题");
  });
});

describe("compactChatTurn 压缩摘要", () => {
  it("锚点已被丢弃（回滚/分支）→ 如实报错，不发请求", async () => {
    const result = await compactChatTurn({
      target: TARGET,
      messages: [{ id: "m1", role: "user", content: "问题" }],
      upToMessageId: "gone",
      signal: new AbortController().signal,
    });
    expect(result.ok).toBe(false);
    expect(result.ok === false && result.message).toContain("压缩失败");
  });
});
