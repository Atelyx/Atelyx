/**
 * Cordis 挂载器：插件生命周期（ctx.plugin 挂载 / 卸载 / 装配）。
 *
 * 挂载 = 进程内（主线程）apply；失败 = 该插件 failed + 可读原因，不阻塞其余。
 * 插件定义 = Cordis 插件（函数或 { apply, inject, provide, Config } 对象）；`inject` 声明的
 * 依赖服务缺失时插件不激活（apply 不执行），挂载器据此返回失败 + 缺失清单（可见化）。
 * 审计归属：contextToPluginId 记录插件上下文 → 插件 id（audit.ts 据此归属服务读/事件订阅）。
 */
import { Context, FiberState, type Fiber, type Plugin } from "@atelyx/cordis";
import { errText, type PluginMountFailure } from "@/types";
import { forgetPluginAudit } from "./audit";
import type { Kernel } from "./kernel";

/** 插件定义（宿主侧编译实现或插件包求值模块；可携带 Cordis 插件元数据）。 */
export interface PluginDefinition {
  id: string;
  /** Cordis 插件：函数 (ctx, config) 或对象 { apply, inject?, provide?, Config?, name? }。 */
  apply: Plugin;
}

/** 挂载结果：ok 或分段失败诊断（phase + 可读原因 + 可选缺失服务清单）。 */
export type MountResult = { ok: true } | ({ ok: false } & PluginMountFailure);

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

/** 每内核每 id 的挂载串行队列（挂载含 await，跨越它存在 check-then-act 窗口：
 *  同一 id 并发挂载会让后到者覆盖 mountsOf 的句柄，先到的活 fiber 从此无法被卸载
 *  （能力残留 + 槽 id 占死）。串行化后「先撤销旧 fiber 再挂载」的语义对并发调用同样成立。 */
const queuesByKernel = new WeakMap<Kernel, Map<string, Promise<unknown>>>();

function queueOf(kernel: Kernel): Map<string, Promise<unknown>> {
  let m = queuesByKernel.get(kernel);
  if (!m) {
    m = new Map();
    queuesByKernel.set(kernel, m);
  }
  return m;
}

/** 排入该插件 id 的操作队列（队首等待前一个任务结算，无论成败）。 */
function enqueue<T>(kernel: Kernel, id: string, task: () => Promise<T>): Promise<T> {
  const queue = queueOf(kernel);
  const prev = queue.get(id) ?? Promise.resolve();
  const next = prev.then(task, task);
  // 队列尾只记「已结算」状态，避免单次失败让后续任务永不执行
  queue.set(
    id,
    next.then(
      () => undefined,
      () => undefined,
    ),
  );
  return next;
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

/** 失败收尾：摘掉挂载句柄 → 撤销 effects → 丢弃审计记录，返回分段诊断。
 *  失败挂载（apply 读了服务后抛错）同样丢弃审计记录：该 id 不在 mountsOf 里，卸载路径摸不到它，
 *  留着会与之后成功挂载的读数并集（披露 UI 把旧实现的服务面算到新代码头上）。 */
async function failMount(
  kernel: Kernel,
  id: string,
  fiber: Fiber,
  failure: PluginMountFailure,
): Promise<MountResult> {
  mountsOf(kernel).delete(id);
  await disposeFiber(fiber);
  forgetPluginAudit(id);
  return { ok: false, ...failure };
}

/** 挂载单个插件（先撤销旧 fiber 防重复注册）；失败返回可读原因。
 *  同一 id 的并发调用按入队顺序串行执行（队列见 enqueue）。 */
export function mountPlugin(
  kernel: Kernel,
  plugin: PluginDefinition,
  config?: Record<string, unknown>,
): Promise<MountResult> {
  return enqueue(kernel, plugin.id, () => mountNow(kernel, plugin, config));
}

/** 挂载实现（队列内执行）：不做排队，避免自等待。 */
async function mountNow(
  kernel: Kernel,
  plugin: PluginDefinition,
  config?: Record<string, unknown>,
): Promise<MountResult> {
  await unmountNow(kernel, plugin.id);
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
      return failMount(kernel, plugin.id, fiber, {
        phase: "apply",
        message:
          missing.length > 0 ? `依赖服务未提供：${missing.join("、")}` : "插件未激活（依赖服务缺失）",
        ...(missing.length > 0 ? { missing } : {}),
      });
    }
    return { ok: true };
  } catch (e) {
    return failMount(kernel, plugin.id, fiber, { phase: "apply", message: errText(e) });
  }
}

/** 卸载单个插件（fiber.dispose → 全部 effects 撤销：槽/服务/接线/订阅）。
 *  与挂载同队列，保证「挂载 → 卸载」按调用顺序发生（挂载在途时先等其结算再卸载）。 */
export function unmountPlugin(kernel: Kernel, id: string): Promise<void> {
  return enqueue(kernel, id, () => unmountNow(kernel, id));
}

/** 卸载实现（队列内执行）：不做排队，避免自等待。 */
async function unmountNow(kernel: Kernel, id: string): Promise<void> {
  const fiber = mountsOf(kernel).get(id);
  if (!fiber) return;
  mountsOf(kernel).delete(id);
  await disposeFiber(fiber);
  forgetPluginAudit(id);
}

/** 卸载当前全部已挂载插件（重载清场；pluginStore.load 用）。
 *  逐个 id 走同一队列（与在途挂载同序，不会交错删掉刚建好的 fiber）；id 集合取调用时的
 *  「已挂载 ∪ 已入队」——不追调用之后新入队的挂载：那是并发的另一次 load 自己的职责，
 *  多轮追猎会把它刚挂上的 fiber 卸掉（行标 active 而运行时空缺）。 */
export async function unmountAll(kernel: Kernel): Promise<void> {
  const ids = new Set([...mountsOf(kernel).keys(), ...queueOf(kernel).keys()]);
  for (const id of ids) {
    await unmountPlugin(kernel, id);
  }
}

/** 已挂载插件 id 列表（测试断言挂载状态用）。 */
export function mountedPluginIds(kernel: Kernel): string[] {
  return [...mountsOf(kernel).keys()];
}
