/**
 * 协作域接线注册表（通用协作宿主机制，域无关）：
 * 画布/笔记/表格域在此注册消息通道 handler、重连/拆卸钩子与 presence 合并 provider，
 * 宿主（collabStore）只查表分发与合并。纯数据容器 + 纯函数，无 store/service 运行时依赖（类型
 * 引入除外），可直测。
 *
 * 注册/撤销随插件启停（cordis/builtins 的 collabWiring）：register* 返回撤销函数。
 * 重连/拆卸/presence 运行序 = priority 升序（同 priority 按注册序）——域间「谁覆盖谁」显式声明，
 * 不依赖注册序（随插件启停后注册序 = 插件 spawn 序，不再是可靠顺序来源）。
 */
import type { CollabHello, CollabPresence } from "@/types";
import type { VaultIdentity } from "@/services/content/contract";

/** 域消息通道 handler（channel = 传输层消息类型；payload 为不透明原始值，解码归 handler 侧）。 */
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

/** 注册域重连钩子（协作重连成功时按 priority 升序逐个调，如重发全量握手/补发 presence）；
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

// ===== 身份 → 传输选择（纯函数，依赖注入保持本模块无 store/service 运行时依赖） =====

/** 协作连接目标（resolveCollabTarget 的解析结果，collabStore 据此 connectTransport）。 */
export interface CollabTarget {
  /** 传输工厂名：space（协作空间频道）。 */
  transport: "space";
  url: string;
  hello: CollabHello;
}

/** 协作连接的运行时配置（应用级开关/身份；collabStore 从 settingsStore 读取并组装）。 */
export interface CollabConnectConfig {
  enabled: boolean;
  nickname: string;
  color: string;
  deviceName: string;
  /** 本端应用版本号（随 hello 上报；可选）。 */
  version?: string;
}

/** resolveCollabTarget 的依赖（注入 getToken/spaceWsUrl，避免 utils 运行时依赖 service）。 */
export interface CollabTargetDeps {
  /** 激活仓库身份（null = 未进仓；协作只存在于协作空间，local 身份不连接）。 */
  identity: VaultIdentity | null;
  collab: CollabConnectConfig;
  /** 空间登录令牌（space 身份每次连接触取——断线重连也重取，防携带已吊销令牌）。 */
  getToken: (serverUrl: string) => Promise<string>;
  /** 空间 WebSocket 地址构造（services/collab/spaceTransport 的 spaceWsUrl）。 */
  spaceWsUrl: (serverUrl: string) => string;
}

/**
 * 按激活仓库身份解析协作连接目标：
 * - space → space 工厂，url = spaceWsUrl(serverUrl)，hello 带 spaceId + 登录令牌；
 * - local / 无身份 / 协作开关关闭 = 不连接（null）——协作只存在于协作空间（服务端真源）。
 * 空间令牌为空也照常发起连接：服务端以 error 帧拒绝（无效令牌），失败经 onServerError 可见，
 * 不在此静默吞掉。
 */
export async function resolveCollabTarget(deps: CollabTargetDeps): Promise<CollabTarget | null> {
  const { identity, collab } = deps;
  if (!identity || identity.kind !== "space" || !collab.enabled) return null;
  const token = await deps.getToken(identity.serverUrl);
  return {
    transport: "space",
    url: deps.spaceWsUrl(identity.serverUrl),
    hello: {
      spaceId: identity.spaceId,
      token,
      nickname: collab.nickname,
      color: collab.color,
      deviceName: collab.deviceName,
      version: collab.version,
    },
  };
}
