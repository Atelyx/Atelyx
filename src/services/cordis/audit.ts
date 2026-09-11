/**
 * 插件审计：「声明 vs 实际」的实际侧 = ctx 服务读 + 事件订阅（按插件归属）。
 *
 * 机制（不动框架源码）：
 * - 服务读：包装 `ReflectService.handler.get`——插件经 ctx 代理访问服务时记录服务名；
 *   归属 = 挂载器登记的「插件上下文 → 插件 id」（contextToPluginId），宿主侧读（root ctx）
 *   不在登记表内、不记录。
 * - 事件订阅：扫 `ctx.events._hooks`，按 hook.ctx 归属插件。
 * 由 getKernel() 安装（应用路径）；测试用 installAudit 直装。
 */
import { ReflectService } from "@atelyx/cordis";
import type { Context } from "@atelyx/cordis";
import { PLUGIN_SERVICE_NAMES } from "@/constants/pluginServices";
import { pluginIdOf } from "./loader";

/** 单个插件的审计结果。 */
export interface PluginAuditEntry {
  pluginId: string;
  /** 实际读过的 Atelyx ctx 服务名（访问序去重）。 */
  services: string[];
  /** 实际订阅过的事件名。 */
  events: string[];
}

/** 纳入审计的 Atelyx 服务面（与展示标签同一清单，避免两处枚举漂移；新增服务面改 constants 一处）。 */
const ATELYX_SERVICES = new Set(PLUGIN_SERVICE_NAMES);

/** 服务读记录：插件 id → 服务名集合。 */
const serviceReads = new Map<string, Set<string>>();

function recordServiceRead(pluginId: string, name: string): void {
  let set = serviceReads.get(pluginId);
  if (!set) {
    set = new Set();
    serviceReads.set(pluginId, set);
  }
  set.add(name);
}

let installed = false;

/** 安装审计（应用路径经 getKernel 调用；幂等——已装则 no-op）；返回撤销函数（供测试/复位）。 */
export function installAudit(): () => void {
  if (installed) return () => {};
  installed = true;
  const originalGet = ReflectService.handler.get!;
  ReflectService.handler.get = function (target, prop, receiver) {
    const value = originalGet.call(this, target, prop, receiver);
    if (typeof prop === "string" && ATELYX_SERVICES.has(prop) && value !== undefined) {
      const pluginId = pluginIdOf(receiver as object);
      if (pluginId) recordServiceRead(pluginId, prop);
    }
    return value;
  };
  return () => {
    ReflectService.handler.get = originalGet;
    installed = false;
  };
}

/** 审计快照：服务读 + 事件订阅按插件聚合（声明对照的实际侧）。 */
export function auditSnapshot(ctx: Context): PluginAuditEntry[] {
  const ids = new Set<string>(serviceReads.keys());
  const eventsByPlugin = new Map<string, Set<string>>();
  for (const name of Object.keys(ctx.events._hooks)) {
    for (const hook of ctx.events._hooks[name]) {
      const pluginId = pluginIdOf(hook.ctx as object);
      if (!pluginId) continue;
      let set = eventsByPlugin.get(pluginId);
      if (!set) {
        set = new Set();
        eventsByPlugin.set(pluginId, set);
      }
      set.add(name);
      ids.add(pluginId);
    }
  }
  const out: PluginAuditEntry[] = [];
  for (const id of ids) {
    out.push({
      pluginId: id,
      services: [...(serviceReads.get(id) ?? [])],
      events: [...(eventsByPlugin.get(id) ?? [])],
    });
  }
  return out.sort((a, b) => (a.pluginId < b.pluginId ? -1 : 1));
}

/** 清空服务读记录（供测试复位）。 */
export function resetAudit(): void {
  serviceReads.clear();
}

/** 丢弃单个插件的服务读记录（卸载时调用）：记录只增不删会让审计跨卸载/换包累积，
 *  把已卸载插件的旧访问当作当前行的「实际侧」。事件侧随 fiber 撤销自然消失，无需清理。 */
export function forgetPluginAudit(pluginId: string): void {
  serviceReads.delete(pluginId);
}
