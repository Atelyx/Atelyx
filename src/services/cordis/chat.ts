/**
 * AI 对话能力提供器（ctx.chat）：由随应用分发的对话核心插件在挂载时经 ctx.provide 提供。
 *
 * 实现 = 核心注册表里的对话运行时（`stores/chatTurn.ts` 构造、`utils/chatRuntimeHost.ts` 注册）——
 * 停用/卸载对话核心插件时运行时随注册表撤销、服务随 fiber 撤销（ctx.effect 逆序），
 * 消费方据此降级（面板提示占位、对话节点禁用发送）。
 */
import { getChatRuntime } from "@/utils/chatRuntimeHost";
import type { ChatService } from "./types";

/** 构造 AI 对话能力（要求对话核心已注册运行时：同一次 apply 里接线在前、提供在后）。 */
export function createChatService(): ChatService {
  const runtime = getChatRuntime();
  if (!runtime) throw new Error("AI 对话能力未就绪");
  return runtime;
}
