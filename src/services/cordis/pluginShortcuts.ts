/**
 * 插件全局快捷键的登记表（按内核隔离）。
 *
 * `ctx.shortcuts.registerGlobal` 注册的快捷键按**调用方插件**记账，插件停用/卸载时由宿主
 * 统一注销（`stores/pluginStore.ts` 的 `stopPlugin` 与 `load` 收尾）——OS 级热键是应用内
 * 唯一资源，不该活过插件本身。与 `pluginProcesses.ts` 同一套纪律：
 * - 登记先于注册 promise 落地（停用可覆盖在途注册）；注销/释放前先等在途注册落地；
 * - 登记表按内核隔离（多窗口各自持有内核）；OS 层归属仲裁与释放是应用级的，
 *   由 Rust 侧登记表裁定（任一窗口的释放都生效）。
 *
 * 本模块不 import 任何 service：注销动作由调用方以 `release` 回调注入，保持纯表 + 可直测。
 */

import type { WindowOptions } from "@/types";

/** 快捷键登记表：插件 id → 快捷键（原始注册串）→ 触发回调。 */
const registries = new WeakMap<object, Map<string, Map<string, () => void | Promise<void>>>>();

/** 声明注册的元数据（设置页改键的重注册依据：声明 id + 窗口切换选项）。 */
export interface DeclaredShortcutMeta {
  /** manifest 声明 id（覆盖表键 = `${pluginId}:${id}`）。 */
  id: string;
  /** 窗口切换热键选项（重注册原样携带；普通热键无此字段）。 */
  windowToggle?: { view: string; options: WindowOptions };
}

/** 声明注册的完整快照（改键重注册用：旧 accelerator + 触发回调 + 声明元数据）。 */
export interface DeclaredShortcutRegistration {
  accelerator: string;
  /** 普通热键的触发回调（窗口切换热键由 Rust 直控，无回调）。 */
  handler?: () => void | Promise<void>;
  meta: DeclaredShortcutMeta;
}

/** 声明元数据登记表：按内核隔离，与主登记表同键位（accelerator）平行记账。 */
const declarationMetas = new WeakMap<object, Map<string, Map<string, DeclaredShortcutMeta>>>();

/** 登记表 → 插件 id → 在途注册 promise（注册落地前停用，须先等落地才谈得上注销）。 */
const pendingRegisters = new WeakMap<Map<string, Map<string, () => void | Promise<void>>>, Map<string, Set<Promise<unknown>>>>();

/** 登记表落在内核根上下文上：插件的 `this.ctx` 是它的派生 ctx，此处按原型链归位。 */
function registryOf(ctx: object): Map<string, Map<string, () => void | Promise<void>>> {
  let cur: object | null = ctx;
  while (cur) {
    const found = registries.get(cur);
    if (found) return found;
    cur = Object.getPrototypeOf(cur);
  }
  const created = new Map<string, Map<string, () => void | Promise<void>>>();
  registries.set(ctx, created);
  return created;
}

function shortcutsOf(
  registry: Map<string, Map<string, () => void | Promise<void>>>,
  pluginId: string,
): Map<string, () => void | Promise<void>> {
  let map = registry.get(pluginId);
  if (!map) {
    map = new Map<string, () => void | Promise<void>>();
    registry.set(pluginId, map);
  }
  return map;
}

/** 登记一个属于该插件的快捷键及其触发回调（同插件同键重复登记 = 覆盖回调）。 */
export function trackShortcut(
  ctx: object,
  pluginId: string,
  accelerator: string,
  handler: () => void | Promise<void>,
): void {
  shortcutsOf(registryOf(ctx), pluginId).set(accelerator, handler);
}

/** 按原型链取声明元数据登记表（与 registryOf 同构）。 */
function metaRegistryOf(ctx: object): Map<string, Map<string, DeclaredShortcutMeta>> {
  let cur: object | null = ctx;
  while (cur) {
    const found = declarationMetas.get(cur);
    if (found) return found;
    cur = Object.getPrototypeOf(cur);
  }
  const created = new Map<string, Map<string, DeclaredShortcutMeta>>();
  declarationMetas.set(ctx, created);
  return created;
}

/** 登记声明注册的元数据（按声明 id 注册时调用；与主登记表同 accelerator 键位）。 */
export function trackShortcutDeclaration(
  ctx: object,
  pluginId: string,
  accelerator: string,
  meta: DeclaredShortcutMeta,
): void {
  const registry = metaRegistryOf(ctx);
  let byPlugin = registry.get(pluginId);
  if (!byPlugin) {
    byPlugin = new Map();
    registry.set(pluginId, byPlugin);
  }
  byPlugin.set(accelerator, meta);
}

/** 摘除声明元数据登记（未登记 = no-op）。 */
export function untrackShortcutDeclaration(ctx: object, pluginId: string, accelerator: string): void {
  const registry = metaRegistryOf(ctx);
  const byPlugin = registry.get(pluginId);
  if (!byPlugin) return;
  byPlugin.delete(accelerator);
  if (byPlugin.size === 0) registry.delete(pluginId);
}

/** 条件摘除声明元数据：仅当该键当前登记的声明 id 就是本次传入的 id 时才摘（并发注册时，
 *  先发注册的失败回滚不得摘掉后发注册刚登记的条目）。返回是否实际摘除。 */
export function untrackShortcutDeclarationIf(
  ctx: object,
  pluginId: string,
  accelerator: string,
  declarationId: string,
): boolean {
  const byPlugin = metaRegistryOf(ctx).get(pluginId);
  if (byPlugin?.get(accelerator)?.id !== declarationId) return false;
  untrackShortcutDeclaration(ctx, pluginId, accelerator);
  return true;
}

/** 查某插件某声明的当前注册（改键重注册用；未注册 = undefined）。 */
export function declaredShortcutOf(
  ctx: object,
  pluginId: string,
  declarationId: string,
): DeclaredShortcutRegistration | undefined {
  const registry = registryOf(ctx);
  const metas = metaRegistryOf(ctx).get(pluginId);
  if (!metas) return undefined;
  for (const [accelerator, meta] of metas) {
    if (meta.id !== declarationId) continue;
    return {
      accelerator,
      handler: registry.get(pluginId)?.get(accelerator),
      meta,
    };
  }
  return undefined;
}

/** 把声明注册整体迁到新 accelerator（改键重注册成功后调用：主登记表回调 + 声明元数据同步搬家）。 */
export function moveShortcutRegistration(
  ctx: object,
  pluginId: string,
  oldAccelerator: string,
  newAccelerator: string,
  registration: DeclaredShortcutRegistration,
): void {
  const registry = registryOf(ctx);
  const byPlugin = registry.get(pluginId);
  if (byPlugin) {
    if (byPlugin.get(oldAccelerator) === registration.handler) byPlugin.delete(oldAccelerator);
    if (registration.handler) byPlugin.set(newAccelerator, registration.handler);
    if (byPlugin.size === 0) registry.delete(pluginId);
  }
  untrackShortcutDeclaration(ctx, pluginId, oldAccelerator);
  trackShortcutDeclaration(ctx, pluginId, newAccelerator, registration.meta);
}

/** 摘除登记（未登记 = no-op）。 */
export function untrackShortcut(ctx: object, pluginId: string, accelerator: string): void {
  const registry = registryOf(ctx);
  registry.get(pluginId)?.delete(accelerator);
  if (registry.get(pluginId)?.size === 0) registry.delete(pluginId);
}

/** 条件摘除：仅当该键当前登记的回调就是本次注册传入的回调时才摘（并发同键注册时，
 *  先发注册的失败回滚不得摘掉后发注册刚登记的条目）。返回是否实际摘除。 */
export function untrackShortcutIf(
  ctx: object,
  pluginId: string,
  accelerator: string,
  handler: () => void | Promise<void>,
): boolean {
  const registry = registryOf(ctx);
  const current = registry.get(pluginId)?.get(accelerator);
  if (current !== handler) return false;
  untrackShortcut(ctx, pluginId, accelerator);
  return true;
}

/** 登记一次在途注册（落地后自行出册；成功失败都出册，不冒未处理拒绝）。 */
export function trackPendingShortcutRegister(
  ctx: object,
  pluginId: string,
  register: Promise<unknown>,
): void {
  const registry = registryOf(ctx);
  let byPlugin = pendingRegisters.get(registry);
  if (!byPlugin) {
    byPlugin = new Map<string, Set<Promise<unknown>>>();
    pendingRegisters.set(registry, byPlugin);
  }
  let set = byPlugin.get(pluginId);
  if (!set) {
    set = new Set<Promise<unknown>>();
    byPlugin.set(pluginId, set);
  }
  set.add(register);
  void register.then(
    () => {
      set?.delete(register);
      if (set && set.size === 0 && byPlugin?.get(pluginId) === set) byPlugin.delete(pluginId);
    },
    () => {
      set?.delete(register);
      if (set && set.size === 0 && byPlugin?.get(pluginId) === set) byPlugin.delete(pluginId);
    },
  );
}

/** 分发一次触发：按快捷键原始注册串找到归属插件的回调并执行（逐个异常隔离）。
 *  返回是否命中本内核的回调（未命中 = 该键不是本窗口已挂载插件注册的，调用方记可见日志）。 */
export function dispatchShortcutTrigger(ctx: object, accelerator: string): boolean {
  const registry = registryOf(ctx);
  for (const [pluginId, shortcuts] of registry) {
    const handler = shortcuts.get(accelerator);
    if (!handler) continue;
    void Promise.resolve()
      .then(handler)
      .catch((error) => {
        console.error(`插件 ${pluginId} 的全局快捷键 ${accelerator} 回调失败`, error);
      });
    return true;
  }
  return false;
}

/** 注销某插件的全部在册快捷键；登记随之清空。
 *  注销动作由调用方注入（`release`，按插件整体调用一次——Rust 侧按归属表整批清理、幂等）。
 *  本地登记为空也照常调用 release：OS 层归属是应用级的，本窗口没登记不代表归属窗口
 *  没有注册（跨窗口停用时由本调用统一清干净）。 */
export async function releasePluginShortcuts(
  ctx: object,
  pluginId: string,
  release: () => Promise<void>,
): Promise<{ released: number; message: string | null }> {
  const registry = registryOf(ctx);
  // 先等在途注册落地：否则「注册后立即停用」的快捷键尚未入册就会被漏掉
  const pending = [...(pendingRegisters.get(registry)?.get(pluginId) ?? [])];
  if (pending.length > 0) await Promise.allSettled(pending);
  const released = registry.get(pluginId)?.size ?? 0;
  registry.delete(pluginId);
  // 声明元数据登记与主登记表同生命周期：随插件整体释放一并清空
  metaRegistryOf(ctx).delete(pluginId);
  try {
    await release();
    return { released, message: null };
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    console.error(`注销插件 ${pluginId} 的全局快捷键失败`, error);
    return { released, message };
  }
}

/** 注销所有**未挂载**插件的快捷键，返回按插件 id 归集的失败原因。
 *  用于跨窗口停用/卸载（被停用插件的登记在别的窗口内核，本窗口的 `unmountAll` 碰不到）。
 *  已挂载插件的快捷键不在此列——它们仍应响应触发。 */
export async function releaseUnmountedPluginShortcuts(
  ctx: object,
  mountedIds: readonly string[],
  release: (pluginId: string) => Promise<void>,
): Promise<Map<string, string>> {
  const mounted = new Set(mountedIds);
  const registry = registryOf(ctx);
  const stale = [...registry.keys()].filter((id) => !mounted.has(id));
  const failures = new Map<string, string>();
  for (const id of stale) {
    const outcome = await releasePluginShortcuts(ctx, id, () => release(id));
    if (outcome.message !== null) failures.set(id, outcome.message);
  }
  return failures;
}
