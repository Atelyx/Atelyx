/**
 * 插件托管进程的登记表（按内核隔离）。
 *
 * `ctx.shell.exec`/`ctx.shell.spawn` 启动的进程按**调用方插件**记账，插件停用/卸载时由
 * 宿主统一结束（`stores/pluginStore.ts` 的 `stopPlugin` 与 `load` 收尾）——插件的长驻服务
 * （本机模型服务、sidecar 等）不该活过插件本身。
 *
 * 两条与「按 pid 结束」绑定的纪律：
 * - 进程结束（`close`）即摘除登记，否则同 pid 被系统复用给别的进程时会被误杀；
 *   运行期 `error` **不摘除**（进程可能仍在跑，摘了会让停用漏杀）；
 * - 登记表按内核隔离（多窗口各自持有内核），只有启动进程的那个窗口能结束它。
 *
 * 本模块不 import 任何 service：结束动作由调用方以 `kill` 回调注入，保持纯表 + 可直测。
 */

/** 单个 pid 的结束结果（失败逐个隔离，不因一个 pid 失败放弃其余）。 */
export interface ProcessKillOutcome {
  killed: number;
  failed: Array<{ pid: number; message: string }>;
}

const UNKNOWN: ProcessKillOutcome = { killed: 0, failed: [] };

/** 内核根上下文 → 插件 id → 进程 pid 集合。 */
const registries = new WeakMap<object, Map<string, Set<number>>>();

/** 内核登记表 → 插件 id → 在途启动（pid 尚未解析）的 promise。
 *
 *  插件 `void ctx.shell.spawn(...)`（不等返回）后紧接着被停用时，pid 还没登记就已被清表——
 *  结束前先等这些 promise 落地，才谈得上「停用即结束它启动的进程」。键用登记表本身（同一内核
 *  共享一张），随内核回收。 */
const pendingLaunches = new WeakMap<Map<string, Set<number>>, Map<string, Set<Promise<unknown>>>>();

/** 登记表落在内核根上下文上：插件的 `this.ctx` 是它的派生 ctx，此处按原型链归位。
 *  从 `ctx` 出发一路取原型：命中的第一个已登记上下文即内核根（派生只在内核根之外叠加）。 */
function registryOf(ctx: object): Map<string, Set<number>> {
  let cur: object | null = ctx;
  while (cur) {
    const found = registries.get(cur);
    if (found) return found;
    cur = Object.getPrototypeOf(cur);
  }
  const created = new Map<string, Set<number>>();
  registries.set(ctx, created);
  return created;
}

function processesOf(registry: Map<string, Set<number>>, pluginId: string): Set<number> {
  let set = registry.get(pluginId);
  if (!set) {
    set = new Set<number>();
    registry.set(pluginId, set);
  }
  return set;
}

/** 登记一个属于该插件的进程（幂等）。 */
export function trackPluginProcess(ctx: object, pluginId: string, pid: number): void {
  processesOf(registryOf(ctx), pluginId).add(pid);
}

/** 摘除登记（进程退出后调用；不存在 = no-op）。 */
export function untrackPluginProcess(ctx: object, pluginId: string, pid: number): void {
  const registry = registryOf(ctx);
  const set = registry.get(pluginId);
  if (!set) return;
  set.delete(pid);
  if (set.size === 0) registry.delete(pluginId);
}

/** 登记一次在途启动（pid 解析后自行出册；成功失败都会出册，不泄漏也不冒未处理拒绝）。 */
export function trackPendingLaunch(ctx: object, pluginId: string, launch: Promise<unknown>): void {
  const registry = registryOf(ctx);
  let byPlugin = pendingLaunches.get(registry);
  if (!byPlugin) {
    byPlugin = new Map<string, Set<Promise<unknown>>>();
    pendingLaunches.set(registry, byPlugin);
  }
  let set = byPlugin.get(pluginId);
  if (!set) {
    set = new Set<Promise<unknown>>();
    byPlugin.set(pluginId, set);
  }
  set.add(launch);
  const settle = (): void => {
    set?.delete(launch);
    // 只在自己这张 Set 空掉时摘插件条目，且确认表里现在挂的还是这张（防重复登记时误删新表）
    if (set && set.size === 0 && byPlugin?.get(pluginId) === set) byPlugin.delete(pluginId);
  };
  void launch.then(settle, settle);
}

/** 结束某插件的全部在册进程（撤销句柄未覆盖的也在其列）；登记随之清空。
 *  结束动作由调用方注入（`kill`），单个失败不中断其余，失败原因原样带回供上层提示。 */
export async function killPluginProcesses(
  ctx: object,
  pluginId: string,
  kill: (pid: number) => Promise<void>,
): Promise<ProcessKillOutcome> {
  const registry = registryOf(ctx);
  // 先等在途启动落地：否则「启动后立即停用」的进程 pid 尚未入册就会被漏掉
  const pending = [...(pendingLaunches.get(registry)?.get(pluginId) ?? [])];
  if (pending.length > 0) await Promise.allSettled(pending);
  const pids = [...(registry.get(pluginId) ?? [])];
  if (pids.length === 0) return UNKNOWN;
  registry.delete(pluginId);
  const outcome: ProcessKillOutcome = { killed: 0, failed: [] };
  for (const pid of pids) {
    try {
      await kill(pid);
      outcome.killed += 1;
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      outcome.failed.push({ pid, message });
      console.error(`结束插件进程 ${pid} 失败`, error);
    }
  }
  return outcome;
}

/** 结束所有**未挂载**插件的进程，返回按插件 id 归集的失败原因。
 *
 *  用于两类漏网：跨窗口停用/卸载（被停用插件的进程由别的窗口启动，本窗口的 `unmountAll`
 *  碰不到）与「apply 抛错导致未挂载」的残留。已挂载插件的进程不在此列——它们仍在运行。
 *  候选同时取在途启动的插件：pid 还没落地时表里没有它的条目，只看进程表会漏掉这一类
 *  （`killPluginProcesses` 内部会先等在途启动落地）。 */
export async function killUnmountedPluginProcesses(
  ctx: object,
  mountedIds: readonly string[],
  kill: (pid: number) => Promise<void>,
): Promise<Map<string, ProcessKillOutcome>> {
  const mounted = new Set(mountedIds);
  const registry = registryOf(ctx);
  const pending = pendingLaunches.get(registry);
  const candidates = new Set([...registry.keys(), ...(pending?.keys() ?? [])]);
  const stale = [...candidates].filter((id) => !mounted.has(id));
  const failures = new Map<string, ProcessKillOutcome>();
  for (const id of stale) {
    const outcome = await killPluginProcesses(ctx, id, kill);
    if (outcome.failed.length > 0) failures.set(id, outcome);
  }
  return failures;
}
