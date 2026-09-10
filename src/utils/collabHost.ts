/**
 * 协作域接线注册表（通用协作宿主机制，域无关）：
 * 画布/笔记/表格域在此注册消息通道 handler、重连/拆卸钩子与 presence 合并 provider，
 * 宿主（collabStore）只查表分发与合并。纯数据容器 + 纯函数，无 store/service 依赖，可直测。
 *
 * 注册/撤销随插件启停（cordis/builtins 的 collabWiring）：register* 返回撤销函数。
 * 重连/拆卸/presence 运行序 = priority 升序（同 priority 按注册序）——域间「谁覆盖谁」显式声明，
 * 不依赖注册序（随插件启停后注册序 = 插件 spawn 序，不再是可靠顺序来源）。
 */
import type { CollabPresence } from "@/types";

/** 域消息通道 handler（channel = relay 消息类型；payload 为不透明原始值，解码归 handler 侧）。 */
export type CollabChannelHandler = (peerId: number, file: string, payload: unknown) => void;

const collabChannels = new Map<string, CollabChannelHandler>();

/** 注册域消息通道 handler（同 channel 后注册者生效）；返回撤销函数（按引用守卫，幂等）。 */
export function registerCollabChannel(channel: string, handler: CollabChannelHandler): () => void {
  collabChannels.set(channel, handler);
  return () => {
    if (collabChannels.get(channel) === handler) collabChannels.delete(channel);
  };
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

/** 带序钩子（priority 升序运行；同 priority 按注册序）。 */
interface OrderedHook<F> {
  fn: F;
  priority: number;
  seq: number;
}

const collabReconnects: OrderedHook<() => void>[] = [];
const collabTeardowns: OrderedHook<() => void>[] = [];
const collabPresenceProviders: OrderedHook<(base: CollabPresence) => CollabPresence>[] = [];
let hookSeq = 0;

/** 按 priority 升序（同值按注册序）排序。 */
function sortOrdered<F>(hooks: OrderedHook<F>[]): OrderedHook<F>[] {
  return [...hooks].sort((a, b) => a.priority - b.priority || a.seq - b.seq);
}

/** 按 priority 升序运行一批重连/拆卸钩子。 */
function runOrdered(hooks: OrderedHook<() => void>[]): void {
  for (const h of sortOrdered(hooks)) h.fn();
}

/** 注册域重连钩子（relay 重连成功时按 priority 升序逐个调，如重发全量握手/补发 presence）；
 *  priority 越大越后执行（画布域 > 表格域：画布 presence 最后补发，覆盖表格槽——画布为主工作区）。
 *  返回撤销函数。 */
export function registerCollabReconnect(fn: () => void, priority = 0): () => void {
  const hook: OrderedHook<() => void> = { fn, priority, seq: hookSeq++ };
  collabReconnects.push(hook);
  return () => {
    const i = collabReconnects.indexOf(hook);
    if (i >= 0) collabReconnects.splice(i, 1);
  };
}

/** 按 priority 升序运行全部重连钩子。 */
export function runCollabReconnects(): void {
  runOrdered(collabReconnects);
}

/** 注册域拆卸钩子（协作关闭时按 priority 升序逐个调，如清理协作文档/广播钩子）；返回撤销函数。 */
export function registerCollabTeardown(fn: () => void, priority = 0): () => void {
  const hook: OrderedHook<() => void> = { fn, priority, seq: hookSeq++ };
  collabTeardowns.push(hook);
  return () => {
    const i = collabTeardowns.indexOf(hook);
    if (i >= 0) collabTeardowns.splice(i, 1);
  };
}

/** 按 priority 升序运行全部拆卸钩子。 */
export function runCollabTeardowns(): void {
  runOrdered(collabTeardowns);
}

/** 注册域 presence 合并 provider（presence 广播前按 priority 升序依次合并，如画布锁/流式节点跨视图保活）；
 *  返回撤销函数。 */
export function registerCollabPresenceProvider(
  fn: (base: CollabPresence) => CollabPresence,
  priority = 0,
): () => void {
  const hook: OrderedHook<(base: CollabPresence) => CollabPresence> = { fn, priority, seq: hookSeq++ };
  collabPresenceProviders.push(hook);
  return () => {
    const i = collabPresenceProviders.indexOf(hook);
    if (i >= 0) collabPresenceProviders.splice(i, 1);
  };
}

/** 依次应用全部 presence provider（无 provider = 原样返回；provider 只做增量合并）。 */
export function mergeCollabPresence(base: CollabPresence): CollabPresence {
  let merged = base;
  for (const h of sortOrdered(collabPresenceProviders)) merged = h.fn(merged);
  return merged;
}
