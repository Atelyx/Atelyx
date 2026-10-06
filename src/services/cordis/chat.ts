/**
 * AI 对话能力（ctx.chat）：由宿主内核启动时提供（与 history/layout/uiState 同层）。
 * 编排方法 = 注册的对话运行时（`utils/chatRuntimeHost.ts`），容器方法 = 注入的面板会话写入访问（`access.ts`，builtin.chatpanel 接线）。
 */
import { getChatRuntime, registerChatRuntime } from "@/utils/chatRuntimeHost";
import { getPluginChatPanelAccess, type PluginChatPanelAccess } from "./access";
import type { ChatRuntime } from "@/types";
import type { ChatService } from "./types";

/** 面板会话写入访问：未接线（面板插件未启用）时抛错，调用插件据此降级。 */
function requireChatPanelAccess(): PluginChatPanelAccess {
  const access = getPluginChatPanelAccess();
  if (!access) throw new Error("对话面板能力未就绪（对话面板插件未启用）");
  return access;
}

/** 当前对话运行时：未注册（对话核心行未启用）时抛错，调用方据此降级。 */
function requireChatRuntime(): ChatRuntime {
  const runtime = getChatRuntime();
  if (!runtime) throw new Error("AI 对话能力未就绪（对话核心插件未启用）");
  return runtime;
}

/** 构造 AI 对话能力（不要求运行时已注册）。运行时按调用现取：提供者可替换，构造时快照会指向旧实现。 */
export function createChatService(): ChatService {
  return {
    resolveTarget: (selection) => requireChatRuntime().resolveTarget(selection),
    runTurn: (req) => requireChatRuntime().runTurn(req),
    compact: (req) => requireChatRuntime().compact(req),
    autoName: (naming, targetId, opts) => requireChatRuntime().autoName(naming, targetId, opts),
    registerRuntime: (runtime) => registerChatRuntime(runtime),
    importSession: (messages, opts) => requireChatPanelAccess().importSession(messages, opts),
    appendMessages: (sessionId, messages) => requireChatPanelAccess().appendMessages(sessionId, messages),
    listSessions: () => requireChatPanelAccess().listSessions(),
    openSession: (sessionId) => requireChatPanelAccess().readSession(sessionId),
    createSession: (opts) => requireChatPanelAccess().createSession(opts),
    setSessionTitle: (sessionId, title) => requireChatPanelAccess().setSessionTitle(sessionId, title),
    deleteSession: (sessionId) => requireChatPanelAccess().deleteSession(sessionId),
  };
}
