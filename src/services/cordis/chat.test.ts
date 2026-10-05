/**
 * AI 对话服务组合测试（services/cordis/chat）：编排方法来自核心注册表运行时，
 * 同源容器方法来自注入的面板会话访问；未接线时如实抛错。
 */
import { describe, expect, it, afterEach } from "vitest";
import { createChatService } from "./chat";
import { setPluginChatPanelAccess, type PluginChatPanelAccess } from "./access";
import { registerChatRuntime } from "@/utils/chatRuntimeHost";

afterEach(() => {
  setPluginChatPanelAccess(null);
  // 复位注册表：注册时留存撤销函数，逐一调用
  registeredOffs.forEach((off) => off());
  registeredOffs.length = 0;
});

const registeredOffs: Array<() => void> = [];

const runtimeStub = () => ({
  resolveTarget: () => ({
    ok: true as const,
    provider: { id: "p1", name: "P", baseUrl: "http://x", apiKey: "k", models: [] },
    model: "m1",
  }),
  runTurn: async () => {},
  compact: async () => ({ ok: false as const, aborted: false, message: "不应被调用" }),
  autoName: async () => "skipped" as const,
});

const accessStub = (calls: string[]): PluginChatPanelAccess => ({
  importSession: async (messages, opts) => {
    calls.push(`import:${messages.length}:${opts?.title ?? ""}`);
    return { id: "s9" };
  },
  appendMessages: async (sessionId, messages) => {
    calls.push(`append:${sessionId}:${messages.length}`);
  },
  listSessions: async () => {
    calls.push("list");
    return [{ id: "s9", updatedAt: 1 }];
  },
  readSession: async (sessionId) => {
    calls.push(`read:${sessionId}`);
    return { id: sessionId, messages: [] };
  },
  createSession: async (opts) => {
    calls.push(`create:${opts?.agentId ?? ""}`);
    return { id: "s10" };
  },
  setSessionTitle: async (sessionId, title) => {
    calls.push(`title:${sessionId}:${title}`);
  },
  deleteSession: async (sessionId) => {
    calls.push(`delete:${sessionId}`);
  },
});

describe("createChatService", () => {
  it("对话核心未注册运行时：构造即抛「未就绪」", () => {
    expect(() => createChatService()).toThrow("AI 对话能力未就绪");
  });

  it("编排方法透传运行时；容器方法未接线抛「对话面板能力未就绪」", () => {
    registeredOffs.push(registerChatRuntime(runtimeStub()));
    const service = createChatService();
    expect(service.resolveTarget().ok).toBe(true);
    expect(() => service.importSession([], {})).toThrow("对话面板能力未就绪");
    expect(() => service.appendMessages("s1", [])).toThrow("对话面板能力未就绪");
    expect(() => service.listSessions()).toThrow("对话面板能力未就绪");
    expect(() => service.openSession("s1")).toThrow("对话面板能力未就绪");
    expect(() => service.createSession({})).toThrow("对话面板能力未就绪");
    expect(() => service.setSessionTitle("s1", "t")).toThrow("对话面板能力未就绪");
  });

  it("容器方法委托注入访问", async () => {
    registeredOffs.push(registerChatRuntime(runtimeStub()));
    const calls: string[] = [];
    setPluginChatPanelAccess(accessStub(calls));
    const service = createChatService();
    const { id } = await service.importSession([{ id: "m1", role: "user", content: "hi" }], {
      title: "T",
    });
    expect(id).toBe("s9");
    await service.appendMessages("s9", [{ id: "m2", role: "assistant", content: "答" }]);
    expect(await service.listSessions()).toHaveLength(1);
    await service.openSession("s9");
    await service.createSession({ agentId: "a1" });
    await service.setSessionTitle("s9", "标题");
    expect(calls).toEqual([
      "import:1:T",
      "append:s9:1",
      "list",
      "read:s9",
      "create:a1",
      "title:s9:标题",
    ]);
  });
});
