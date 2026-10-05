/**
 * AI 对话能力提供器（ctx.chat）：由随应用分发的对话核心插件在挂载时经 ctx.provide 提供。
 *
 * 编排方法 = 核心注册表里的对话运行时（`stores/chatTurn.ts` 构造、`utils/chatRuntimeHost.ts` 注册）；
 * 容器方法（importSession/appendMessages）= 注入的面板会话写入访问（`access.ts`，builtin.chatpanel
 * 接线）——停用/卸载对话核心插件时运行时随注册表撤销、整个服务随 fiber 撤销；停用对话面板插件时
 * 容器方法抛「对话面板能力未就绪」（编排方法不受影响）。
 */
import { getChatRuntime } from "@/utils/chatRuntimeHost";
import { getPluginChatPanelAccess, type PluginChatPanelAccess } from "./access";
import type { ChatService } from "./types";

/** 面板会话写入访问：未接线（面板插件未启用）时抛错，调用插件据此降级。 */
function requireChatPanelAccess(): PluginChatPanelAccess {
  const access = getPluginChatPanelAccess();
  if (!access) throw new Error("对话面板能力未就绪（对话面板插件未启用）");
  return access;
}

/** 构造 AI 对话能力（要求对话核心已注册运行时：同一次 apply 里接线在前、提供在后）。 */
export function createChatService(): ChatService {
  const runtime = getChatRuntime();
  if (!runtime) throw new Error("AI 对话能力未就绪");
  return {
    ...runtime,
    importSession: (messages, opts) => requireChatPanelAccess().importSession(messages, opts),
    appendMessages: (sessionId, messages) => requireChatPanelAccess().appendMessages(sessionId, messages),
    listSessions: () => requireChatPanelAccess().listSessions(),
    openSession: (sessionId) => requireChatPanelAccess().readSession(sessionId),
    createSession: (opts) => requireChatPanelAccess().createSession(opts),
    setSessionTitle: (sessionId, title) => requireChatPanelAccess().setSessionTitle(sessionId, title),
    deleteSession: (sessionId) => requireChatPanelAccess().deleteSession(sessionId),
  };
}
