/**
 * 插件 attach 会话的登记表（按内核隔离）：`ctx.rpc.attach` 建立的常驻运行时会话按调用方插件记账，
 * 插件停用/卸载时由宿主统一卸载（`stores/pluginStore.ts` 的 `stopPlugin` 与 `load` 收尾）。
 * 常驻运行时进程本身是内核级设施（不随单个插件退出），随插件装卸的只有它的会话与模块；
 * 卸载动作由调用方以通道句柄注入（不 import 任何 service，保持纯表 + 可直测）。
 */

/** 单个会话的卸载结果（失败逐个隔离，不因一个会话失败放弃其余）。 */
export interface SessionDetachOutcome {
  detached: number;
  failed: Array<{ sessionId: number; message: string }>;
}

const UNKNOWN: SessionDetachOutcome = { detached: 0, failed: [] };

/** 会话句柄：卸载 = 关闭通道（close 幂等，含运行时侧模块结束）。 */
export interface TrackedSession {
  sessionId: number;
  close(): Promise<void>;
}

/** 内核根上下文 → 插件 id → 会话表。 */
const registries = new WeakMap<object, Map<string, Map<number, TrackedSession>>>();

/** 内核登记表 → 插件 id → 在途 attach（会话尚未登记）的 promise。
 *  插件 `void ctx.rpc.attach(...)`（不等返回）后紧接着被停用时，会话还没登记就已被清表——
 *  卸载前先等这些 promise 落地，才谈得上「停用即卸载它建立的会话」。键用登记表本身（同一内核
 *  共享一张），随内核回收。 */
const pendingAttaches = new WeakMap<Map<string, Map<number, TrackedSession>>, Map<string, Set<Promise<unknown>>>>();

/** 登记表落在内核根上下文上：插件的 `this.ctx` 是它的派生 ctx，此处按原型链归位。
 *  从 `ctx` 出发一路取原型：命中的第一个已登记上下文即内核根（派生只在内核根之外叠加）。 */
function registryOf(ctx: object): Map<string, Map<number, TrackedSession>> {
  let cur: object | null = ctx;
  while (cur) {
    const found = registries.get(cur);
    if (found) return found;
    cur = Object.getPrototypeOf(cur);
  }
  const created = new Map<string, Map<number, TrackedSession>>();
  registries.set(ctx, created);
  return created;
}

function sessionsOf(registry: Map<string, Map<number, TrackedSession>>, pluginId: string): Map<number, TrackedSession> {
  let set = registry.get(pluginId);
  if (!set) {
    set = new Map<number, TrackedSession>();
    registry.set(pluginId, set);
  }
  return set;
}

/** 登记一个属于该插件的会话（幂等；同 id 重复登记覆盖旧句柄——通道已关闭的旧句柄无副作用）。 */
export function trackPluginSession(ctx: object, pluginId: string, session: TrackedSession): void {
  sessionsOf(registryOf(ctx), pluginId).set(session.sessionId, session);
}

/** 摘除登记（通道关闭后调用；不存在 = no-op）。 */
export function untrackPluginSession(ctx: object, pluginId: string, sessionId: number): void {
  const registry = registryOf(ctx);
  const set = registry.get(pluginId);
  if (!set) return;
  set.delete(sessionId);
  if (set.size === 0) registry.delete(pluginId);
}

/** 登记一次在途 attach（会话登记后自行出册；成功失败都会出册，不泄漏也不冒未处理拒绝）。 */
export function trackPendingSessionAttach(ctx: object, pluginId: string, attach: Promise<unknown>): void {
  const registry = registryOf(ctx);
  let byPlugin = pendingAttaches.get(registry);
  if (!byPlugin) {
    byPlugin = new Map<string, Set<Promise<unknown>>>();
    pendingAttaches.set(registry, byPlugin);
  }
  let set = byPlugin.get(pluginId);
  if (!set) {
    set = new Set<Promise<unknown>>();
    byPlugin.set(pluginId, set);
  }
  set.add(attach);
  const settle = (): void => {
    set?.delete(attach);
    // 只在自己这张 Set 空掉时摘插件条目，且确认表里现在挂的还是这张（防重复登记时误删新表）
    if (set && set.size === 0 && byPlugin?.get(pluginId) === set) byPlugin.delete(pluginId);
  };
  void attach.then(settle, settle);
}

/** 卸载某插件的全部在册会话（在途 attach 未覆盖的也在其列）；登记随之清空。
 *  先等在途 attach 落地：否则「attach 后立即停用」的会话尚未入册就会被漏掉。
 *  单个失败不中断其余，失败原因原样带回供上层提示。 */
export async function detachPluginSessions(ctx: object, pluginId: string): Promise<SessionDetachOutcome> {
  const registry = registryOf(ctx);
  const pending = [...(pendingAttaches.get(registry)?.get(pluginId) ?? [])];
  if (pending.length > 0) await Promise.allSettled(pending);
  const sessions = [...(registry.get(pluginId)?.values() ?? [])];
  if (sessions.length === 0) return UNKNOWN;
  registry.delete(pluginId);
  const outcome: SessionDetachOutcome = { detached: 0, failed: [] };
  for (const session of sessions) {
    try {
      await session.close();
      outcome.detached += 1;
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      outcome.failed.push({ sessionId: session.sessionId, message });
      console.error(`卸载插件会话 ${session.sessionId} 失败`, error);
    }
  }
  return outcome;
}

/** 卸载所有**未挂载**插件的会话，返回按插件 id 归集的失败原因。
 *  与进程清扫同口径：跨窗口停用/卸载（被停用插件的会话由别的窗口建立，本窗口的 `unmountAll`
 *  碰不到）与「apply 抛错导致未挂载」的残留。已挂载插件的会话不动。 */
export async function detachUnmountedPluginSessions(
  ctx: object,
  mountedIds: readonly string[],
): Promise<Map<string, SessionDetachOutcome>> {
  const mounted = new Set(mountedIds);
  const registry = registryOf(ctx);
  const pending = pendingAttaches.get(registry);
  const candidates = new Set([...registry.keys(), ...(pending?.keys() ?? [])]);
  const stale = [...candidates].filter((id) => !mounted.has(id));
  const failures = new Map<string, SessionDetachOutcome>();
  for (const id of stale) {
    const outcome = await detachPluginSessions(ctx, id);
    if (outcome.failed.length > 0) failures.set(id, outcome);
  }
  return failures;
}
