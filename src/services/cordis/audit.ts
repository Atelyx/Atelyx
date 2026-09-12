/**
 * 插件审计：「声明 vs 实际」的实际侧 = ctx 服务读 + 事件订阅 + 高危调用摘要（按插件归属）。
 *
 * 机制（不动框架源码）：
 * - 服务读：包装 `ReflectService.handler.get`——插件经 ctx 代理访问服务时记录服务名；
 *   归属 = 挂载器登记的「插件上下文 → 插件 id」（contextToPluginId），宿主侧读（root ctx）
 *   不在登记表内、不记录。
 * - 高危调用摘要：命中敏感面（服务整体或单方法）的服务换成一层包装视图，调用时只记形状与
 *   规模（程序名 + 参数个数 / 方法 + 主机路径 / 字节数 / 相对路径），参数原文（凭据、正文、
 *   header、body）一律不进审计。
 * - 事件订阅：扫 `ctx.events._hooks`，按 hook.ctx 归属插件。
 * 由 getKernel() 安装（应用路径）；测试用 installAudit 直装。
 */
import { ReflectService } from "@atelyx/cordis";
import type { Context } from "@atelyx/cordis";
import {
  PLUGIN_SENSITIVE_METHODS,
  PLUGIN_SERVICE_NAMES,
  PLUGIN_SERVICE_SENSITIVE,
} from "@/constants/pluginServices";
import type { HttpRequestInput } from "@/services/http";
import { pluginIdOf } from "./loader";
import type { ShellExecOptions } from "./types";

/** 一次高危服务调用的脱敏摘要（只记形状与规模，不含参数原文）。 */
export interface PluginAuditCall {
  service: string;
  method: string;
  summary: string;
}

/** 单个插件的审计结果。 */
export interface PluginAuditEntry {
  pluginId: string;
  /** 实际读过的 Atelyx ctx 服务名（访问序去重）。 */
  services: string[];
  /** 实际订阅过的事件名。 */
  events: string[];
  /** 高危服务调用的脱敏摘要（去重，上限 MAX_AUDIT_CALLS）。 */
  calls: PluginAuditCall[];
}

/** 纳入审计的 Atelyx 服务面（与展示标签同一清单，避免两处枚举漂移；新增服务面改 constants 一处）。 */
const ATELYX_SERVICES = new Set(PLUGIN_SERVICE_NAMES);

/** 高危调用摘要上限：只做「见过哪些调用形态」的窗口，避免长跑插件把记录撑大。 */
const MAX_AUDIT_CALLS = 50;

/** 服务读记录：插件 id → 服务名集合。 */
const serviceReads = new Map<string, Set<string>>();

/** 高危调用记录：插件 id → (service.method:summary → 摘要)，天然去重。 */
const callsByPlugin = new Map<string, Map<string, PluginAuditCall>>();

function recordServiceRead(pluginId: string, name: string): void {
  let set = serviceReads.get(pluginId);
  if (!set) {
    set = new Set();
    serviceReads.set(pluginId, set);
  }
  set.add(name);
}

function recordServiceCall(pluginId: string, call: PluginAuditCall): void {
  let calls = callsByPlugin.get(pluginId);
  if (!calls) {
    calls = new Map();
    callsByPlugin.set(pluginId, calls);
  }
  const key = `${call.service}.${call.method}:${call.summary}`;
  if (!calls.has(key) && calls.size < MAX_AUDIT_CALLS) calls.set(key, call);
}

/** 敏感面：服务整体敏感 ∪ 有方法级敏感声明（名单来自 constants，本文件不维护枚举）。 */
function isSensitiveSurface(service: string): boolean {
  return PLUGIN_SERVICE_SENSITIVE.has(service) || service in PLUGIN_SENSITIVE_METHODS;
}

function isSensitiveMethod(service: string, method: string): boolean {
  return PLUGIN_SENSITIVE_METHODS[service]?.has(method) === true;
}

/** 地址摘要：只留协议 + 主机 + 路径，剥 query/fragment（常带凭据）。 */
function sanitizeUrl(raw: unknown): string {
  if (typeof raw !== "string") return "（无地址）";
  try {
    const url = new URL(raw);
    return `${url.protocol}//${url.host}${url.pathname}`;
  } catch {
    return "（地址无法解析）";
  }
}

/** 调用摘要：只记形状与规模；返回 undefined = 该调用不进审计。 */
function summarizeCall(service: string, method: string, args: unknown[]): string | undefined {
  if (service === "shell") {
    const opts = args[0] as ShellExecOptions | undefined;
    const program = typeof opts?.command === "string" ? opts.command : "未知程序";
    const count = Array.isArray(opts?.args) ? opts.args.length : 0;
    return `${program}（${count} 个参数）`;
  }
  if (service === "http") {
    const req = args[0] as HttpRequestInput | undefined;
    return `${typeof req?.method === "string" ? req.method : "GET"} ${sanitizeUrl(req?.url)}`;
  }
  if (service === "clipboard") {
    const text = args[0];
    return typeof text === "string" ? `${method}（${new TextEncoder().encode(text).length} 字节）` : method;
  }
  if (isSensitiveMethod(service, method)) {
    const path = args[0];
    return typeof path === "string" ? `${method} ${path}` : method;
  }
  return undefined;
}

/** 该插件视角下的服务视图：敏感面多包一层，调用时先记摘要再转发（真实调用抛错也留摘要）。 */
function wrapSensitiveService(pluginId: string, service: string, target: object): object {
  return new Proxy(target, {
    get(obj, prop, receiver) {
      const member = Reflect.get(obj, prop, receiver);
      if (typeof prop !== "string" || typeof member !== "function") return member;
      return (...args: unknown[]) => {
        const summary = summarizeCall(service, prop, args);
        if (summary !== undefined) recordServiceCall(pluginId, { service, method: prop, summary });
        return Reflect.apply(member, obj, args);
      };
    },
  });
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
      if (pluginId) {
        recordServiceRead(pluginId, prop);
        if (isSensitiveSurface(prop) && typeof value === "object" && value !== null) {
          return wrapSensitiveService(pluginId, prop, value);
        }
      }
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
      calls: [...(callsByPlugin.get(id)?.values() ?? [])],
    });
  }
  return out.sort((a, b) => (a.pluginId < b.pluginId ? -1 : 1));
}

/** 清空审计记录（供测试复位）。 */
export function resetAudit(): void {
  serviceReads.clear();
  callsByPlugin.clear();
}

/** 丢弃单个插件的审计记录（卸载时调用）：记录只增不删会让审计跨卸载/换包累积，
 *  把已卸载插件的旧访问当作当前行的「实际侧」。事件侧随 fiber 撤销自然消失，无需清理。 */
export function forgetPluginAudit(pluginId: string): void {
  serviceReads.delete(pluginId);
  callsByPlugin.delete(pluginId);
}
