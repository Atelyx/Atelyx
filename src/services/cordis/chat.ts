/**
 * AI 会话服务提供器（ctx.chat）：由 builtin.aichat 行挂载时经 ctx.provide 提供。
 *
 * 实现 = 注入的 AI 会话访问对象（pluginStore 接线填充，见 stores/pluginStore ensureChatAccess）——
 * 停用/卸载 builtin.aichat 时服务随之消失（ctx.effect 撤销）。
 */
import { getPluginChatAccess } from "./access";
import type { ChatService } from "./types";

/** 构造 AI 会话服务（要求访问已接线：pluginStore.ensureChatAccess 已填充）。 */
export function createChatService(): ChatService {
  const access = getPluginChatAccess();
  if (!access) throw new Error("AI 会话能力未就绪");
  return {
    sessions: () => access.sessions(),
    activeSession: () => access.activeSession(),
    isStreaming: () => access.isStreaming(),
    openSession: (id) => access.openSession(id),
    startSession: () => access.startSession(),
    sendMessage: (content) => access.sendMessage(content),
    stop: () => access.stop(),
    deleteSession: (id) => access.deleteSession(id),
  };
}
