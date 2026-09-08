/**
 * 协作域接线注册表（通用协作宿主机制，域无关）：
 * 画布/笔记/表格域在此注册消息通道 handler、重连/拆卸钩子与 presence 合并 provider，
 * 宿主（collabStore）只查表分发与合并。纯数据容器 + 纯函数，无 store/service 依赖，可直测。
 */
import type { CollabPresence } from "@/types";

/** 域消息通道 handler（channel = relay 消息类型；payload 为不透明原始值，解码归 handler 侧）。 */
export type CollabChannelHandler = (peerId: number, file: string, payload: unknown) => void;

const collabChannels = new Map<string, CollabChannelHandler>();

/** 注册域消息通道 handler（幂等覆盖：同 channel 后注册者生效）。 */
export function registerCollabChannel(channel: string, handler: CollabChannelHandler): void {
  collabChannels.set(channel, handler);
}

/** 通道查表分发（未注册通道静默丢弃）。 */
export function dispatchCollabChannel(
  channel: string,
  peerId: number,
  file: string,
  payload: unknown,
): void {
  collabChannels.get(channel)?.(peerId, file, payload);
}

const collabReconnects: Array<() => void> = [];

/** 注册域重连钩子（relay 重连成功时按注册序逐个调，如重发全量握手/补发 presence）。 */
export function registerCollabReconnect(fn: () => void): void {
  collabReconnects.push(fn);
}

/** 按注册序运行全部重连钩子。 */
export function runCollabReconnects(): void {
  for (const fn of collabReconnects) fn();
}

const collabTeardowns: Array<() => void> = [];

/** 注册域拆卸钩子（协作关闭时按注册序逐个调，如清理协作文档/广播钩子）。 */
export function registerCollabTeardown(fn: () => void): void {
  collabTeardowns.push(fn);
}

/** 按注册序运行全部拆卸钩子。 */
export function runCollabTeardowns(): void {
  for (const fn of collabTeardowns) fn();
}

const collabPresenceProviders: Array<(base: CollabPresence) => CollabPresence> = [];

/** 注册域 presence 合并 provider（presence 广播前按注册序依次合并，如画布锁/流式节点跨视图保活）。 */
export function registerCollabPresenceProvider(fn: (base: CollabPresence) => CollabPresence): void {
  collabPresenceProviders.push(fn);
}

/** 依次应用全部 presence provider（无 provider = 原样返回；provider 只做增量合并）。 */
export function mergeCollabPresence(base: CollabPresence): CollabPresence {
  let merged = base;
  for (const fn of collabPresenceProviders) merged = fn(merged);
  return merged;
}
