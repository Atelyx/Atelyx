/**
 * 服务注册表查询服务（ctx.services）：插件据此发现当前真实可用的服务面与提供者。
 *
 * 数据真相 = Cordis `ctx.reflect.store`（root 作用域，含内核平台服务与插件经 provide
 * 注册的全部服务）；`list()` 遍历它返回服务名 + 提供者插件 id。提供者归属两路：
 * 插件经 `ctx.provide`（自身 ctx）注册的服务，`impl.fiber` 即插件 fiber，`pluginIdOf`
 * 可直接推断；builtins 领域服务经 `ctx.root.provide` 注册（root fiber 无归属），由
 * 登记表补充（builtins 挂载时经 `registerServiceProvider` 登记，随 fiber 撤销）。
 * 宿主内核平台服务无提供者（list 返回项不带 provider 字段）。
 *
 * `get(name)` 读某服务（不存在/未激活 = undefined，不抛错）——可选依赖判空入口：
 * inject 声明 `{ foo: { optional: true } }` 的可选依赖经 loader 剥出后，插件在 apply
 * 内经 `ctx.services.get("foo")` 判空降级（直接 `ctx.foo` 访问会因不在 inject 而抛错）。
 * 返回值重新绑定调用方 ctx（`ctx.get` 返回 root 绑定的 traceable，直接透传会丢审计归属），
 * 敏感面套审计包装（与 `ctx.<service>` 直连同口径记录高危调用摘要）。
 */
import { getTraceable, symbols } from "@atelyx/cordis";
import type { Context } from "@atelyx/cordis";
import { pluginIdOf } from "./loader";
import { wrapSensitiveService } from "./audit";
import { PLUGIN_SENSITIVE_METHODS, PLUGIN_SERVICE_SENSITIVE } from "@/constants/pluginServices";
import type { ServiceInfo, ServicesService } from "./types";

/** 服务名 → 提供者插件 id（builtins 领域服务经 ctx.root.provide 注册的归属补充；随 fiber 撤销）。 */
const serviceProviders = new Map<string, string>();

/** 登记某服务的提供者插件 id（builtins 领域服务挂载时调用；返回撤销函数随 fiber 清理）。 */
export function registerServiceProvider(name: string, pluginId: string): () => void {
  serviceProviders.set(name, pluginId);
  return () => {
    if (serviceProviders.get(name) === pluginId) serviceProviders.delete(name);
  };
}

/** 服务是否属敏感面（整体敏感 ∪ 方法级敏感；与审计同源判定）。 */
function isSensitiveSurface(name: string): boolean {
  return PLUGIN_SERVICE_SENSITIVE.has(name) || name in PLUGIN_SENSITIVE_METHODS;
}

interface ServicesServiceInstance extends ServicesService {
  /** 调用方插件上下文（tracker 注入；get 返回的服务据此绑定调用方 ctx）。 */
  ctx: Context;
}

/** 构造 ctx.services 服务（内核 provide；插件经 ctx.services 触达）。 */
export function createServicesService(): ServicesService {
  const api = {
    list(this: ServicesServiceInstance): ServiceInfo[] {
      const out: ServiceInfo[] = [];
      const seen = new Set<string>();
      // store 的键是 isolate symbol（不枚举进 Object.values），须按 symbol 键取 impl。
      // root store 含全部已注册服务（含插件 isolate 注册的服务——isolate key 都挂在 root 下）。
      const store = this.ctx.reflect.store as Record<symbol, { name: string; fiber: { ctx: object } }>;
      for (const key of Object.getOwnPropertySymbols(store)) {
        const impl = store[key];
        if (typeof impl?.name !== "string" || seen.has(impl.name)) continue;
        seen.add(impl.name);
        const provider = pluginIdOf(impl.fiber.ctx) ?? serviceProviders.get(impl.name);
        out.push(provider ? { name: impl.name, provider } : { name: impl.name });
      }
      return out.sort((a, b) => (a.name < b.name ? -1 : 1));
    },
    get<K extends keyof Context>(this: ServicesServiceInstance, name: K): Context[K] | undefined {
      const raw = this.ctx.get(name as never);
      if (raw === undefined) return undefined;
      // ctx.get 返回 root 绑定的 traceable：重绑调用方 ctx，敏感服务再套审计包装（与 ctx.<name> 直连同口径）。
      const pluginId = pluginIdOf(this.ctx);
      let value: unknown = getTraceable(this.ctx, raw);
      if (pluginId && isSensitiveSurface(name as string)) {
        value = wrapSensitiveService(pluginId, name as string, value as object);
      }
      return value as Context[K];
    },
  };
  Object.defineProperty(api, symbols.tracker, { value: { property: "ctx" } });
  return api;
}
