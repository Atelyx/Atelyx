/**
 * AI 对话能力消费 hook：视图（AI 对话面板、画布对话节点）经此取对话运行时并订阅可用性。
 * 只依赖契约与注册表（`utils/chatRuntimeHost`），实现缺席（对话核心插件停用）时返回 null，
 * 视图据此降级（面板提示占位、对话节点禁用发送）。
 */

import { useSyncExternalStore } from "react";
import type { ChatRuntime } from "@/types";
import { getChatRuntime, onChatRuntimeChange } from "@/utils/chatRuntimeHost";

/** 当前对话运行时；null = 对话能力不可用。 */
export function useChatRuntime(): ChatRuntime | null {
  return useSyncExternalStore(onChatRuntimeChange, getChatRuntime, getChatRuntime);
}
