/**
 * 笔记正文编辑能力注册表（跨域能力的消费者侧查表点）。
 *
 * 笔记域（提供者）注册实现，笔记面板与画布文本节点（消费者）取同一会话；
 * 未注册（笔记插件停用）= 正文不可编辑，消费者据此降级只读。
 * 纯数据容器 + 纯函数，无 store/service 依赖，可直测（模式同 `utils/collabHost.ts`）。
 */

import type { NoteSurfaceProvider } from "@/types/noteSurface";

let provider: NoteSurfaceProvider | null = null;
const listeners = new Set<() => void>();

/** 注册提供者（同能力后注册者生效）；返回撤销函数（按引用守卫，幂等）。 */
export function registerNoteSurface(next: NoteSurfaceProvider): () => void {
  provider = next;
  notify();
  return () => {
    if (provider !== next) return;
    provider = null;
    notify();
  };
}

/** 当前提供者；null = 笔记正文编辑能力不可用。 */
export function getNoteSurface(): NoteSurfaceProvider | null {
  return provider;
}

/** 订阅提供者状态变化（可用性、笔记冲突集合）；返回退订函数。 */
export function onNoteSurfaceChange(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** 提供者状态变化后通知消费者重读（冲突集合变化、批量关会话等）。 */
export function notifyNoteSurfaceChange(): void {
  notify();
}

function notify(): void {
  for (const listener of listeners) listener();
}
