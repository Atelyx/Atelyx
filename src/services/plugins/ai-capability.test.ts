/**
 * ai 能力（模型/Agent 列表 + 流式对话）桥路由测试（services/plugins/bridge 注册的宿主 `ai` 命名空间）。
 *
 * 覆盖——listModels/listAgents、chat 流式（chunk/end 帧）+ 非流式（聚合返回）、供应商/默认模型解析、
 * 参数校验、未接线错误、审计。`@/services/ai/client` 的 streamChat 整体 mock：
 * 测试驱动其回调验证帧映射，不触碰真实网络。
 */
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import type { PluginManifest, PluginType } from "@/types";
import type { PluginTransport } from "./worker";
import {
  attachPlugin,
  hostCapabilityNames,
  runtimeSnapshot,
  setSettingsAccess,
  unloadPlugin,
  type PluginAiAccess,
} from "./bridge";
import { streamChat } from "@/services/ai/client";

vi.mock("@/services/ai/client", () => ({
  streamChat: vi.fn(),
}));

class FakeTransport implements PluginTransport {
  posted: unknown[] = [];
  private handlers: Array<(m: unknown) => void> = [];
  post(message: unknown): void {
    this.posted.push(message);
  }
  onMessage(handler: (m: unknown) => void): () => void {
    this.handlers.push(handler);
    return () => {
      this.handlers = this.handlers.filter((h) => h !== handler);
    };
  }
  receive(message: unknown): void {
    for (const h of this.handlers) h(message);
  }
  dispose(): void {}
}

interface Spawned {
  id: string;
  transport: FakeTransport;
}
const spawned: Spawned[] = [];

const manifest = (id: string): PluginManifest => ({
  schemaVersion: 2,
  id,
  name: id,
  version: "1.0.0",
  type: "tool" as PluginType,
  main: "plugin.js",
});

function spawnPlugin(id: string): Spawned {
  const transport = new FakeTransport();
  attachPlugin(manifest(id), transport);
  spawned.push({ id, transport });
  return { id, transport };
}

const tick = (): Promise<void> => new Promise((r) => setTimeout(r, 0));

/** 假 AI 配置访问：一个供应商（含两个模型）+ 一个 Agent；默认目标解析指向默认模型。 */
function makeSettings(): PluginAiAccess {
  const provider = {
    id: "p1",
    name: "供应商一",
    baseUrl: "https://example.com/v1",
    apiKey: "k",
    models: [
      { id: "model-a", nickname: "甲" },
      { id: "model-b" },
    ],
  };
  return {
    providers: [provider],
    agents: [{ id: "agent-1", name: "助手", tools: [] }],
    resolveChatTarget: (sel) =>
      sel?.providerId
        ? { ok: false, reason: "provider-missing", error: "所选供应商已不存在" }
        : { ok: true, provider, model: sel?.model ?? "model-a" },
  };
}

beforeEach(() => {
  vi.resetAllMocks();
  setSettingsAccess(makeSettings);
});

afterEach(() => {
  setSettingsAccess(null);
  for (const s of spawned) unloadPlugin(s.id);
  spawned.length = 0;
});

describe("ai 能力路由", () => {
  it("listModels：扁平化供应商模型", async () => {
    const b = spawnPlugin("com.test.ai1");
    b.transport.receive({ kind: "call", seq: 1, method: "call", args: ["ai", "listModels", []] });
    await tick();
    expect(b.transport.posted).toContainEqual({
      kind: "reply",
      seq: 1,
      ok: true,
      result: [
        { providerId: "p1", providerName: "供应商一", modelId: "model-a", label: "甲" },
        { providerId: "p1", providerName: "供应商一", modelId: "model-b", label: "model-b" },
      ],
    });
  });

  it("listAgents：id/name 列表", async () => {
    const b = spawnPlugin("com.test.ai2");
    b.transport.receive({ kind: "call", seq: 2, method: "call", args: ["ai", "listAgents", []] });
    await tick();
    expect(b.transport.posted).toContainEqual({
      kind: "reply",
      seq: 2,
      ok: true,
      result: [{ id: "agent-1", name: "助手" }],
    });
  });

  it("chat 流式：chunk 文本/思考 + end 聚合（显式 providerId）", async () => {
    vi.mocked(streamChat).mockImplementation(async (_params, cb) => {
      cb.onDelta("你");
      cb.onDelta("好");
      cb.onReasoningDelta?.("想想");
      cb.onDone("stop");
    });
    const b = spawnPlugin("com.test.ai3");
    b.transport.receive({
      kind: "call",
      seq: 3,
      method: "call",
      args: ["ai", "chat", [{ providerId: "p1", messages: [{ role: "user", text: "hi" }] }], { stream: true }],
    });
    await tick();
    const frames = b.transport.posted.filter((m) => (m as { kind?: string }).kind === "stream") as Array<{
      event: string;
      data?: unknown;
    }>;
    expect(frames.map((f) => f.event)).toEqual(["chunk", "chunk", "chunk", "end"]);
    expect(frames[0].data).toEqual({ type: "text", text: "你" });
    expect(frames[2].data).toEqual({ type: "reasoning", text: "想想" });
    expect(frames[3].data).toEqual({ content: "你好", reasoning: "想想", finishReason: "stop" });
    // provider 解析：显式 providerId → 用该供应商 + 首个模型（未给 model）
    expect(streamChat).toHaveBeenCalledWith(
      expect.objectContaining({ baseUrl: "https://example.com/v1", apiKey: "k", model: "model-a" }),
      expect.any(Object),
    );
  });

  it("chat 非流式：聚合返回 content/reasoning/finishReason（默认模型解析）", async () => {
    vi.mocked(streamChat).mockImplementation(async (_params, cb) => {
      cb.onDelta("答");
      cb.onDone("stop");
    });
    const b = spawnPlugin("com.test.ai4");
    b.transport.receive({
      kind: "call",
      seq: 4,
      method: "call",
      args: ["ai", "chat", [{ messages: [{ role: "user", text: "q" }] }]],
    });
    await tick();
    expect(b.transport.posted).toContainEqual({
      kind: "reply",
      seq: 4,
      ok: true,
      result: { content: "答", reasoning: "", finishReason: "stop" },
    });
  });

  it("chat 校验与错误：空 messages / 供应商不存在 / 未接线", async () => {
    const b = spawnPlugin("com.test.ai5");
    b.transport.receive({ kind: "call", seq: 5, method: "call", args: ["ai", "chat", [{ messages: [] }]] });
    await tick();
    expect(b.transport.posted).toContainEqual({ kind: "reply", seq: 5, ok: false, error: "ai.chat 需要非空 messages" });

    b.transport.receive({
      kind: "call",
      seq: 6,
      method: "call",
      args: ["ai", "chat", [{ providerId: "ghost", messages: [{ role: "user", text: "q" }] }]],
    });
    await tick();
    expect(b.transport.posted).toContainEqual({ kind: "reply", seq: 6, ok: false, error: "供应商 ghost 不存在" });

    setSettingsAccess(null);
    b.transport.receive({ kind: "call", seq: 7, method: "call", args: ["ai", "listModels", []] });
    await tick();
    expect(b.transport.posted).toContainEqual({ kind: "reply", seq: 7, ok: false, error: "AI 配置未就绪（未打开仓库）" });
  });

  it("chat 流式错误：onError → stream error 帧收尾", async () => {
    vi.mocked(streamChat).mockImplementation(async (_params, cb) => {
      cb.onError(new Error("网络失败"));
    });
    const b = spawnPlugin("com.test.ai6");
    b.transport.receive({
      kind: "call",
      seq: 8,
      method: "call",
      args: ["ai", "chat", [{ messages: [{ role: "user", text: "q" }] }], { stream: true }],
    });
    await tick();
    const frames = b.transport.posted.filter((m) => (m as { kind?: string }).kind === "stream") as Array<{
      event: string;
      data?: unknown;
    }>;
    expect(frames.map((f) => f.event)).toEqual(["error"]);
    expect(frames[0].data).toBe("网络失败");
  });

  it("审计记录 ai 命名空间；注册表含 ai", async () => {
    vi.mocked(streamChat).mockImplementation(async (_params, cb) => cb.onDone("stop"));
    const b = spawnPlugin("com.test.ai7");
    b.transport.receive({ kind: "call", seq: 9, method: "call", args: ["ai", "listModels", []] });
    await tick();
    expect(runtimeSnapshot().find((e) => e.id === "com.test.ai7")?.used).toContain("ai");
    expect(hostCapabilityNames()).toContain("ai");
  });
});
