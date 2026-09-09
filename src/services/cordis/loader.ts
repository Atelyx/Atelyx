/**
 * Cordis 挂载器：第一方插件生命周期（ctx.plugin 挂载 / 卸载 / 装配）。
 *
 * 挂载 = 进程内（主线程）直接 apply 引用；失败 = 该插件 failed + 可读原因，不阻塞其余。
 * 第三方插件入口（TS → ESM → blob import 求值）为后续接入点，本模块只处理宿主侧 apply。
 * 审计归属：contextToPluginId 记录插件上下文 → 插件 id（audit.ts 据此归属服务读/事件订阅）。
 */
import type { Context, Fiber, Plugin } from "@atelyx/cordis";
import { errText } from "@/types";
import type { Kernel } from "./kernel";
import { resolveProfileMounts, type Profile } from "@/utils/cordis/composition";

/** 第一方插件定义（apply 为宿主侧闭包；返回可选撤销函数，随 fiber 卸载执行）。 */
export interface FirstPartyPlugin {
  id: string;
  apply: (ctx: Context) => void | (() => void);
}

/** 挂载结果：ok 或失败原因。 */
export type MountResult = { ok: true } | { ok: false; reason: string };

/** 插件上下文 → 插件 id（审计归属；mount 时经包装 apply 记录，WeakMap 随上下文回收）。 */
export const contextToPluginId = new WeakMap<object, string>();

/** 每内核的已挂载 fiber 表（按插件 id）。 */
const mountsByKernel = new WeakMap<Kernel, Map<string, Fiber>>();

function mountsOf(kernel: Kernel): Map<string, Fiber> {
  let m = mountsByKernel.get(kernel);
  if (!m) {
    m = new Map();
    mountsByKernel.set(kernel, m);
  }
  return m;
}

async function disposeFiber(fiber: Fiber): Promise<void> {
  try {
    await fiber.dispose();
  } catch {
    // 卸载失败忽略：effects 已尽力按序撤销，剩余残留交由下次挂载前兜底
  }
}

/** 挂载单个第一方插件（先撤销旧 fiber 防重复注册）；失败返回可读原因。 */
export async function mountPlugin(
  kernel: Kernel,
  plugin: FirstPartyPlugin,
  config?: Record<string, unknown>,
): Promise<MountResult> {
  await unmountPlugin(kernel, plugin.id);
  // 包装 apply：登记插件上下文归属（审计用），再执行真实 apply。
  const wrappedApply = (ctx: Context): void | (() => void) => {
    contextToPluginId.set(ctx as object, plugin.id);
    return plugin.apply(ctx);
  };
  const fiber = kernel.ctx.plugin(wrappedApply as Plugin, config);
  mountsOf(kernel).set(plugin.id, fiber);
  try {
    await fiber.await();
    return { ok: true };
  } catch (e) {
    mountsOf(kernel).delete(plugin.id);
    await disposeFiber(fiber);
    return { ok: false, reason: errText(e) };
  }
}

/** 卸载单个插件（fiber.dispose → 全部 effects 撤销：槽/服务/接线/订阅）。 */
export async function unmountPlugin(kernel: Kernel, id: string): Promise<void> {
  const fiber = mountsOf(kernel).get(id);
  if (!fiber) return;
  mountsOf(kernel).delete(id);
  await disposeFiber(fiber);
}

/** 卸载当前全部已挂载插件（重载/清场用）。 */
export async function unmountAll(kernel: Kernel): Promise<void> {
  for (const id of [...mountsOf(kernel).keys()]) {
    await unmountPlugin(kernel, id);
  }
}

/** 已挂载插件 id 列表（快照；管理/审计用）。 */
export function mountedPluginIds(kernel: Kernel): string[] {
  return [...mountsOf(kernel).keys()];
}

/** 装配挂载：先清场（重置语义 = 重置到磁盘状态），再按 profile × 启用集合挂载；
 *  单插件失败不阻塞其余，返回失败清单（pluginStore 据此置 failed + 可读原因）。 */
export async function mountProfile(
  kernel: Kernel,
  profile: Profile,
  plugins: Record<string, FirstPartyPlugin>,
  enabledIds: ReadonlySet<string>,
): Promise<{ failed: Array<{ id: string; reason: string }> }> {
  await unmountAll(kernel);
  const failed: Array<{ id: string; reason: string }> = [];
  for (const m of resolveProfileMounts(profile, enabledIds)) {
    const plugin = plugins[m.id];
    if (!plugin) {
      failed.push({ id: m.id, reason: "无第一方插件定义" });
      continue;
    }
    const result = await mountPlugin(kernel, plugin, m.config);
    if (!result.ok) failed.push({ id: m.id, reason: result.reason });
  }
  return { failed };
}
