/**
 * AI 对话核心能力注册表（跨域能力的消费者侧查表点）。
 *
 * 对话核心插件（提供者）注册运行时，面板会话、画布对话节点与其它消费方取同一份；
 * 未注册（核心插件停用/卸载）= 对话能力不可用，消费者据此降级（面板提示占位、对话节点禁用发送）。
 * 纯数据容器 + 纯函数，无 store/service 依赖，可直测（模式同 `utils/noteSurfaceHost.ts`）。
 */

import type { ChatRuntime } from "@/types";

let runtime: ChatRuntime | null = null;
const listeners = new Set<() => void>();

/** 注册运行时（同能力后注册者生效）；返回撤销函数（按引用守卫，幂等）。 */
export function registerChatRuntime(next: ChatRuntime): () => void {
  runtime = next;
  notify();
  return () => {
    if (runtime !== next) return;
    runtime = null;
    notify();
  };
}

/** 当前运行时；null = AI 对话能力不可用（核心插件未启用）。 */
export function getChatRuntime(): ChatRuntime | null {
  return runtime;
}

/** 订阅可用性变化（停用/启用后消费者重读）；返回退订函数。 */
export function onChatRuntimeChange(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

function notify(): void {
  for (const listener of listeners) listener();
}
