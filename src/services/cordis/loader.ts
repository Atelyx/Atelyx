/**
 * Cordis 挂载器：插件生命周期（ctx.plugin 挂载 / 卸载 / 装配）。
 *
 * 挂载 = 进程内（主线程）apply；失败 = 该插件 failed + 可读原因，不阻塞其余。
 * 插件定义 = Cordis 插件（函数或 { apply, inject, provide, Config } 对象）；`inject` 声明的
 * 依赖服务缺失时插件不激活（apply 不执行），挂载器据此返回失败 + 缺失清单（可见化）。
 * 审计归属：contextToPluginId 记录插件上下文 → 插件 id（audit.ts 据此归属服务读/事件订阅）。
 */
import { Context, FiberState, type Fiber, type Plugin } from "@atelyx/cordis";
import { errText } from "@/types";
import type { Kernel } from "./kernel";

/** 插件定义（宿主侧编译实现或插件包求值模块；可携带 Cordis 插件元数据）。 */
export interface PluginDefinition {
  id: string;
  /** Cordis 插件：函数 (ctx, config) 或对象 { apply, inject?, provide?, Config?, name? }。 */
  apply: Plugin;
}

/** 挂载结果：ok 或失败原因。 */
export type MountResult = { ok: true } | { ok: false; reason: string };

/** 插件上下文 → 插件 id（审计归属；mount 时经包装 apply 记录，WeakMap 随上下文回收）。 */
export const contextToPluginId = new WeakMap<object, string>();

/** 从调用方上下文推导插件 id（追原型链：ctx.extend 派生的上下文命中挂载时登记的原始 ctx）。 */
export function pluginIdOf(ctx: object): string | undefined {
  let cur: object | null = ctx;
  while (cur) {
    const id = contextToPluginId.get(cur);
    if (id) return id;
    cur = Object.getPrototypeOf(cur);
  }
  return undefined;
}

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

/** 插件定义 → Cordis 可调用函数（函数即 apply；对象取 apply 方法）。 */
function resolveApply(plugin: Plugin): (ctx: Context, config: unknown) => unknown {
  return (typeof plugin === "function" ? plugin : plugin.apply) as (ctx: Context, config: unknown) => unknown;
}

/** 挂载单个插件（先撤销旧 fiber 防重复注册）；失败返回可读原因。 */
export async function mountPlugin(
  kernel: Kernel,
  plugin: PluginDefinition,
  config?: Record<string, unknown>,
): Promise<MountResult> {
  await unmountPlugin(kernel, plugin.id);
  const callback = resolveApply(plugin.apply);
  const metadata = typeof plugin.apply === "object" ? plugin.apply : undefined;
  // 包装 apply：登记插件上下文归属（审计用），透传 Cordis 插件元数据（inject 激活语义），再执行真实 apply。
  const wrapped: Plugin = {
    ...(metadata?.name ? { name: metadata.name } : {}),
    ...(metadata?.inject ? { inject: metadata.inject } : {}),
    ...(metadata?.provide ? { provide: metadata.provide } : {}),
    ...(metadata?.Config ? { Config: metadata.Config } : {}),
    apply: (ctx: Context, pluginConfig?: unknown) => {
      contextToPluginId.set(ctx as object, plugin.id);
      return callback(ctx, pluginConfig);
    },
  };
  const fiber = kernel.ctx.plugin(wrapped, config);
  mountsOf(kernel).set(plugin.id, fiber);
  try {
    await fiber.await();
    if (fiber.state !== FiberState.ACTIVE) {
      // inject 依赖未满足：插件未激活（apply 未执行），附缺失服务清单。
      const missing = Object.keys(fiber.inject ?? {}).filter((key) => !fiber.store?.[key]);
      throw new Error(
        missing.length > 0 ? `依赖服务未提供：${missing.join("、")}` : "插件未激活（依赖服务缺失）",
      );
    }
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

/** 卸载当前全部已挂载插件（重载清场；pluginStore.load 用）。 */
export async function unmountAll(kernel: Kernel): Promise<void> {
  for (const id of [...mountsOf(kernel).keys()]) {
    await unmountPlugin(kernel, id);
  }
}

/** 已挂载插件 id 列表（测试断言挂载状态用）。 */
export function mountedPluginIds(kernel: Kernel): string[] {
  return [...mountsOf(kernel).keys()];
}
