/**
 * 插件运行时（桥宿主）：Worker 生命周期 + 能力注册表 + 流式中转 + 审计 + 事件分发。
 *
 * 桥是插件触达 App 能力的唯一通道（worker 内无 window/invoke）。每个插件一个独立 Worker，
 * 故障隔离：单个插件崩溃只影响自身（标记 failed、终止、清注册），不拖垮 App。
 *
 * 能力注册表模型：宿主与插件在同一注册表里对等提供能力——
 * - 宿主命名空间（不含点，如 `state`/`app`/`shell`）经 `registerHostCapability` 注册，
 *   携带展示文案与敏感标记（UI 据此渲染，见 `hostCapabilityLabel`/`hostCapabilitySensitive`）；
 * - 插件经桥 `registerCapability` 注册反向域名命名空间（含点），其他插件与宿主
 *   经 `bridge.call(namespace, method, args)` 调用；跨插件调用由宿主在运行时之间中转。
 * - 调用支持流式（`callStream`）：宿主能力与插件能力都以 chunk/end/error 帧推送。
 *   **流式收尾契约**：流一定以 end/error 收尾——宿主 handler 若未自行 end（sink 已置 ended）
 *   由分发器补 end；流式调用出错走 stream error 帧；跨插件提供方未调 ctx.stream.end 时
 *   其尾随 reply 转发为 end；提供方卸载/崩溃时向调用方补 error 并清 relay。
 * - `registerContribution` 提供通用扩展点注册（现有 register* 是其特化）；载荷内
 *   函数以 `{ $fn: fnId }` 序列化，宿主据此回指可调用。
 *
 * 信任模型（完全自由）：能力声明仅披露与审计，无运行时拒绝——恶意插件拦不住，
 * 责任在用户知情（安装警告）+ 稳定性隔离（worker/子进程崩溃不影响 App）。
 * 审计记录插件实际调用的能力命名空间（`declares` 声明的是同一词汇表），管理页对照。
 */
import { invoke } from "@tauri-apps/api/core";
import type { PluginFiberPhase, PluginManifest, ToolDefinition, ToolResult } from "@/types";
import { registerPluginTools, unregisterPluginTools } from "@/services/ai/tools";
import { AGENT_TOOLS_META } from "@/constants/tools";
import { getAppVersion } from "@/services/app";
import { runProcess } from "@/services/shell";
import { detectPlatform } from "@/utils/pluginHost";
import {
  createPluginWorker,
  type PluginCapabilitySpec,
  type PluginCommandSpec,
  type PluginContributionSpec,
  type PluginToolSpec,
  type PluginTransport,
  type WorkerCallMessage,
  type WorkerStreamMessage,
} from "./worker";

/** 审计上限（内存，防无限增长）。 */
const MAX_AUDIT = 64;

/** 插件运行时条目（桥持有，store 只读快照）。 */
export interface PluginRuntimeEntry {
  id: string;
  manifest: PluginManifest;
  phase: PluginFiberPhase;
  error?: string;
  /** 桥实际调用过的能力命名空间（内存审计，上限截断；与清单 declares 同词汇表）。 */
  used: string[];
}

interface Runtime {
  entry: PluginRuntimeEntry;
  transport: PluginTransport;
  disposed: boolean;
  toolDefs: ToolDefinition[];
  commandDefs: PluginCommandSpec[];
  subscriptions: Set<string>;
  invokeSeq: number;
  invokePending: Map<number, PendingInvoke>;
  /** 零桥调用插件「loading」兜底定时器（到点置 active，防后台纯逻辑插件永显加载中）。 */
  activeTimer?: ReturnType<typeof setTimeout>;
}

const runtimes = new Map<string, Runtime>();

/** 流式推送句柄：宿主能力与插件能力经它向调用方推 chunk/end/error；ended 标记已收尾。 */
export interface PluginStreamSink {
  chunk(data: unknown): void;
  end(data?: unknown): void;
  error(message: string): void;
  /** 是否已 end/error（宿主 handler 自行收尾后分发器不再补 end）。 */
  ended: boolean;
}

/** 宿主能力处理器：method + args 经 ctx（含调用插件 id 与可选流句柄）执行。 */
export type HostCapabilityHandler = (
  method: string,
  args: unknown[],
  ctx: { pluginId: string; stream?: PluginStreamSink },
) => Promise<unknown>;

/** 在飞 invoke 的挂起项：resolve/reject 之外带 cleanup（清超时定时器 + 退订 abort 信号）。 */
interface PendingInvoke {
  resolve: (v: unknown) => void;
  reject: (e: Error) => void;
  cleanup: () => void;
}

/** invoke 附加选项：取消信号（用户停止）与超时（防挂死插件永久阻塞 AI 工具轮）。 */
interface InvokeOptions {
  signal?: AbortSignal;
  timeoutMs?: number;
}

/** 插件 AI 工具单次执行超时（毫秒）：超时发 abort 帧 + reject，不让工具轮永久挂起。 */
const PLUGIN_TOOL_TIMEOUT_MS = 10 * 60 * 1000;

/** 宿主能力展示元数据（UI 标签 + 敏感标记；插件命名空间无元数据、原样显示）。 */
export interface HostCapabilityMeta {
  label: string;
  sensitive?: boolean;
}

/** 宿主命名空间 → 处理器（不含点；插件命名空间必含点，天然不冲突）。 */
const hostCapabilities = new Map<string, HostCapabilityHandler>();
const hostCapabilityMeta = new Map<string, HostCapabilityMeta>();

/** 插件命名空间 → 持有者运行时 + 方法 fnId 表。 */
interface PluginCapabilityEntry {
  owner: string;
  methodIds: Record<string, string>;
}
const pluginCapabilities = new Map<string, PluginCapabilityEntry>();

/** 通用扩展点注册：point → 条目（载荷函数为 `{ $fn: fnId }`）。 */
export interface PluginContributionEntry {
  pluginId: string;
  point: string;
  id?: string;
  payload: unknown;
}
const contributions = new Map<string, PluginContributionEntry>();

/** 跨插件流式中转：键 = `<提供方id>:<invoke seq>`（复合键——提供方 invokeSeq 是本地计数，
 *  多个提供方各自的 seq 会重复，纯数字全局键会互相覆盖错发）。转发目标含调用方运行时、
 *  调用方 seq 与提供方 id。 */
interface StreamRelay {
  callerRuntime: Runtime;
  callerSeq: number;
  owner: string;
}
const pluginStreamRelays = new Map<string, StreamRelay>();

/** 注册宿主命名空间（第一方能力；宿主是注册表里的普通提供者）。 */
export function registerHostCapability(
  namespace: string,
  handler: HostCapabilityHandler,
  meta?: HostCapabilityMeta,
): void {
  if (namespace.includes(".")) throw new Error("宿主命名空间不得含点（插件命名空间用反向域名）");
  hostCapabilities.set(namespace, handler);
  if (meta) hostCapabilityMeta.set(namespace, meta);
}

/** 仅登记宿主能力展示元数据（无 handler 的糖方法面：ai/event/command 等）。 */
export function registerHostCapabilityMeta(namespace: string, meta: HostCapabilityMeta): void {
  hostCapabilityMeta.set(namespace, meta);
}

/** 宿主能力展示文案（插件命名空间返回 undefined，UI 原样显示）。 */
export function hostCapabilityLabel(namespace: string): string | undefined {
  return hostCapabilityMeta.get(namespace)?.label;
}

/** 宿主能力是否敏感（UI「敏感」高亮用）。 */
export function hostCapabilitySensitive(namespace: string): boolean {
  return hostCapabilityMeta.get(namespace)?.sensitive === true;
}

/** 宿主能力命名空间清单（含 meta-only 糖方法面 ai/command/event；UI/registry 展示用）。 */
export function hostCapabilityNames(): string[] {
  return [...new Set([...hostCapabilities.keys(), ...hostCapabilityMeta.keys()])];
}

/** 查询插件能力命名空间持有者（registry 展示/测试用）。 */
export function pluginCapabilityOwner(namespace: string): string | undefined {
  return pluginCapabilities.get(namespace)?.owner;
}

/** 某插件提供的全部能力命名空间（registry 展示/测试用）。 */
export function pluginCapabilitiesByOwner(pluginId: string): string[] {
  return [...pluginCapabilities.entries()].filter(([, e]) => e.owner === pluginId).map(([ns]) => ns);
}

/** 某扩展点的全部注册条目。 */
export function listPluginContributions(point: string): PluginContributionEntry[] {
  return [...contributions.values()].filter((c) => c.point === point);
}

/** 调用贡献载荷里的 `{ $fn: fnId }` 函数引用（宿主/其他插件消费扩展点时用）。 */
export function callPluginContributionFn(pluginId: string, ref: unknown, args: unknown[]): Promise<unknown> {
  const fnId = typeof ref === "object" && ref !== null ? (ref as { $fn?: unknown }).$fn : undefined;
  if (typeof fnId !== "string") return Promise.reject(new Error("贡献载荷函数引用无效"));
  const runtime = runtimes.get(pluginId);
  if (!runtime || runtime.disposed) return Promise.reject(new Error("插件未运行"));
  return invokeFn(runtime, fnId, args);
}

/** 加载插件（worker 平面）：创建 blob Worker 并接入桥；失败由 onCrash 置 failed。 */
export function loadPlugin(manifest: PluginManifest, code: string): PluginRuntimeEntry {
  return attachPlugin(manifest, createPluginWorker(code));
}

/** 以给定传输接入桥（子进程运行时/测试用）：同一套能力注册表与生命周期编排。 */
export function attachPlugin(manifest: PluginManifest, transport: PluginTransport): PluginRuntimeEntry {
  unloadPlugin(manifest.id);
  const runtime: Runtime = {
    entry: { id: manifest.id, manifest, phase: "loading", used: [] },
    transport,
    disposed: false,
    toolDefs: [],
    commandDefs: [],
    subscriptions: new Set(),
    invokeSeq: 0,
    invokePending: new Map(),
  };
  transport.onMessage((data) => handleWorkerMessage(runtime, data));
  transport.onCrash?.((message) => failPlugin(runtime, message));
  // 零桥调用兜底：纯后台/日志型插件永不发消息会卡 loading，30s 后视为已激活。
  runtime.activeTimer = setTimeout(() => {
    if (!runtime.disposed && runtime.entry.phase === "loading") {
      runtime.entry.phase = "active";
      notifyChange();
    }
  }, 30_000);
  runtimes.set(manifest.id, runtime);
  notifyChange();
  return runtime.entry;
}

/** 卸载插件：终止 worker + 撤销全部贡献（工具/能力/扩展点）+ 清流式中转。 */
export function unloadPlugin(id: string): void {
  const runtime = runtimes.get(id);
  if (!runtime) return;
  runtime.disposed = true;
  if (runtime.activeTimer) clearTimeout(runtime.activeTimer);
  // 在飞 invoke 必须 reject：worker 已终止不会再回包，若不 reject，AI 工具轮会永久挂起。
  rejectPending(runtime, "插件已卸载");
  runtime.transport.dispose();
  if (runtime.toolDefs.length > 0) unregisterPluginTools(runtime.toolDefs);
  runtime.toolDefs = [];
  runtime.commandDefs = [];
  clearStreamRelaysFor(id);
  unregisterPluginCapabilities(id);
  unregisterPluginContributions(id);
  runtimes.delete(id);
  notifyChange();
}

/** 运行时快照（pluginStore 用）。 */
export function runtimeSnapshot(): PluginRuntimeEntry[] {
  return [...runtimes.values()].map((r) => r.entry);
}

/** 插件贡献的全部 AI 工具（Agent 名册组装用）。 */
export function contributedPluginTools(): ToolDefinition[] {
  return [...runtimes.values()].flatMap((r) => r.toolDefs);
}

/** 插件贡献的命令（全局 id = `<pluginId>:<命令 id>`；供命令面板/管理 UI 展示与执行）。 */
export interface PluginCommandContribution {
  globalId: string;
  pluginId: string;
  id: string;
  label: string;
}

export function contributedCommands(): PluginCommandContribution[] {
  const out: PluginCommandContribution[] = [];
  for (const [pluginId, r] of runtimes) {
    for (const def of r.commandDefs) {
      out.push({ globalId: `${pluginId}:${def.id}`, pluginId, id: def.id, label: def.label });
    }
  }
  return out.sort((a, b) => (a.globalId < b.globalId ? -1 : 1));
}

/** 执行插件命令（经桥 RPC 回 worker 的 run 函数）。 */
export async function runContributedCommand(globalId: string): Promise<unknown> {
  const sep = globalId.indexOf(":");
  if (sep <= 0) throw new Error("非法命令 id");
  const pluginId = globalId.slice(0, sep);
  const cmdId = globalId.slice(sep + 1);
  const runtime = runtimes.get(pluginId);
  const def = runtime?.commandDefs.find((d) => d.id === cmdId);
  if (!runtime || !def) throw new Error("命令不存在");
  return invokeFn(runtime, def.runId, []);
}

type RuntimeChangeListener = (entries: PluginRuntimeEntry[]) => void;
const changeListeners = new Set<RuntimeChangeListener>();

/** 订阅运行时变更（加载/激活/失败/卸载）；返回退订函数。 */
export function onRuntimeChange(listener: RuntimeChangeListener): () => void {
  changeListeners.add(listener);
  return () => {
    changeListeners.delete(listener);
  };
}

function notifyChange(): void {
  if (changeListeners.size === 0) return;
  const snap = runtimeSnapshot();
  for (const listener of changeListeners) listener(snap);
}

/** 事件投递：发给订阅了该事件的全部插件。 */
export function emitPluginEvent(event: string, payload: unknown): void {
  for (const runtime of runtimes.values()) {
    if (runtime.disposed || !runtime.subscriptions.has(event)) continue;
    try {
      runtime.transport.post({ kind: "event", event, payload });
    } catch {
      // worker 已失效：忽略该插件
    }
  }
}

// ===== 消息处理 =====

function post(runtime: Runtime, message: unknown): void {
  if (!runtime.disposed) runtime.transport.post(message);
}

function handleWorkerMessage(runtime: Runtime, data: unknown): void {
  if (runtime.disposed || typeof data !== "object" || data === null) return;
  const msg = data as { kind?: string };
  if (msg.kind === "call") {
    void handleCall(runtime, data as WorkerCallMessage);
  } else if (msg.kind === "reply") {
    const m = data as { seq?: number; ok?: boolean; result?: unknown; error?: string };
    const pending = typeof m.seq === "number" ? runtime.invokePending.get(m.seq) : undefined;
    if (pending) {
      runtime.invokePending.delete(m.seq as number);
      pending.cleanup();
      if (m.ok) pending.resolve(m.result);
      else pending.reject(new Error(m.error || "插件执行失败"));
    } else if (typeof m.seq === "number") {
      // 流式 invoke 的尾随 reply（提供方未走 end 帧就返回）：转发为 end/error 给调用方，
      // 与宿主侧「流一定以 end/error 收尾」契约对称，防调用方 callStream 永久挂起。
      // 复合键 `<提供方id>:<seq>`——不同提供方各自的 seq 会重复，纯数字键会误删他方 relay。
      const key = `${runtime.entry.id}:${m.seq}`;
      const relay = pluginStreamRelays.get(key);
      if (relay) {
        pluginStreamRelays.delete(key);
        if (!relay.callerRuntime.disposed) {
          relay.callerRuntime.transport.post({
            kind: "stream",
            seq: relay.callerSeq,
            event: m.ok ? "end" : "error",
            data: m.ok ? m.result : m.error,
          });
        }
      }
    }
  } else if (msg.kind === "stream") {
    handlePluginStream(runtime, data as WorkerStreamMessage);
  }
  // 首个任意消息 = 顶层代码已跑完，标记 active。
  if (runtime.entry.phase === "loading") {
    if (runtime.activeTimer) clearTimeout(runtime.activeTimer);
    runtime.entry.phase = "active";
    notifyChange();
  }
}

/** 插件流式帧：按发送方运行时 + invoke seq 的复合键转发给跨插件调用方。 */
function handlePluginStream(runtime: Runtime, m: WorkerStreamMessage): void {
  const key = `${runtime.entry.id}:${m.seq}`;
  const relay = pluginStreamRelays.get(key);
  if (relay && !relay.callerRuntime.disposed) {
    relay.callerRuntime.transport.post({ kind: "stream", seq: relay.callerSeq, event: m.event, data: m.data });
  }
  if (m.event === "end" || m.event === "error") pluginStreamRelays.delete(key);
}

/** 向调用方补 error 并清理该提供方的全部流式中转（提供方卸载/崩溃时调用）。 */
function clearStreamRelaysFor(pluginId: string): void {
  for (const [seq, relay] of pluginStreamRelays) {
    if (relay.owner !== pluginId) continue;
    pluginStreamRelays.delete(seq);
    if (!relay.callerRuntime.disposed) {
      relay.callerRuntime.transport.post({
        kind: "stream",
        seq: relay.callerSeq,
        event: "error",
        data: `能力提供者 ${pluginId} 已停止`,
      });
    }
  }
}

/** 构造带 ended 标记的调用流句柄（宿主 handler 自行收尾后分发器不再补 end）。 */
function makeCallSink(runtime: Runtime, seq: number): PluginStreamSink {
  const sink: PluginStreamSink = {
    ended: false,
    chunk: (d) => post(runtime, { kind: "stream", seq, event: "chunk", data: d }),
    end: (d) => {
      if (sink.ended) return;
      sink.ended = true;
      post(runtime, { kind: "stream", seq, event: "end", data: d });
    },
    error: (e) => {
      if (sink.ended) return;
      sink.ended = true;
      post(runtime, { kind: "stream", seq, event: "error", data: e });
    },
  };
  return sink;
}

async function handleCall(runtime: Runtime, msg: WorkerCallMessage): Promise<void> {
  const reply = (result?: unknown): void => {
    post(runtime, { kind: "reply", seq: msg.seq, ok: true, result });
  };
  const replyError = (error: string): void => {
    post(runtime, { kind: "reply", seq: msg.seq, ok: false, error });
  };
  try {
    if (msg.method === "call") {
      const [ns, method, args, opts] = msg.args as [
        string,
        string,
        unknown[] | undefined,
        { stream?: boolean } | undefined,
      ];
      if (opts?.stream) {
        // 流式调用：不回 reply，只发 stream 帧；出错也走 stream error（reply 会被 callStream 丢弃）。
        const sink = makeCallSink(runtime, msg.seq);
        try {
          await dispatchCapability(runtime, ns, method, args ?? [], sink, msg.seq);
        } catch (e) {
          sink.error(e instanceof Error ? e.message : String(e));
        }
        return;
      }
      reply(await dispatchCapability(runtime, ns, method, args ?? [], undefined, msg.seq));
      return;
    }
    reply(await dispatchMethod(runtime, msg.method, msg.args ?? []));
  } catch (e) {
    replyError(e instanceof Error ? e.message : String(e));
  }
}

/** 能力调用分发：审计命名空间；宿主命名空间直接执行；插件命名空间经 invoke 中转。 */
async function dispatchCapability(
  runtime: Runtime,
  ns: string,
  method: string,
  args: unknown[],
  stream: PluginStreamSink | undefined,
  callerSeq: number,
): Promise<unknown> {
  auditCapability(runtime, ns);
  const host = hostCapabilities.get(ns);
  if (host) {
    const result = await host(method, args, { pluginId: runtime.entry.id, stream });
    if (stream && !stream.ended) {
      // 宿主 handler 未自行收尾（非流式 handler 被流式调用）时补 end。
      stream.end(result);
    }
    return result;
  }
  const entry = pluginCapabilities.get(ns);
  if (!entry) throw new Error(`能力 ${ns} 不存在`);
  const owner = runtimes.get(entry.owner);
  if (!owner || owner.disposed) throw new Error(`能力 ${ns} 的提供者未运行`);
  const fnId = entry.methodIds[method];
  if (!fnId) throw new Error(`能力 ${ns} 无方法 ${method}`);
  if (stream) {
    const invSeq = ++owner.invokeSeq;
    const relayKey = `${entry.owner}:${invSeq}`;
    pluginStreamRelays.set(relayKey, { callerRuntime: runtime, callerSeq, owner: entry.owner });
    try {
      owner.transport.post({ kind: "invoke", seq: invSeq, fnId, args, stream: true });
    } catch (e) {
      pluginStreamRelays.delete(relayKey);
      throw e;
    }
    return undefined;
  }
  return invokeFn(owner, fnId, args);
}

async function dispatchMethod(runtime: Runtime, method: string, args: unknown[]): Promise<unknown> {
  switch (method) {
    case "registerTool": {
      auditCapability(runtime, "ai");
      const spec = args[0] as PluginToolSpec;
      if (typeof spec?.name !== "string" || typeof spec.executeId !== "string") {
        throw new Error("registerTool 参数不完整（需要 name/description/parameters/execute）");
      }
      const name = spec.name.trim();
      // 工具名直发模型（函数名）：OpenAI 只接受字母/数字/下划线/中划线；垃圾名会让模型调用直接失败。
      if (!/^[a-zA-Z0-9_-]+$/.test(name) || name.length > 64) {
        throw new Error("工具名仅限字母/数字/下划线/中划线且不超过 64 字符");
      }
      // 内置工具名保留：插件不得覆盖内置工具（注册表按名分发，同名会静默覆盖内置实现）。
      if (AGENT_TOOLS_META.some((t) => t.id === name)) {
        throw new Error(`工具名 ${name} 为内置工具保留`);
      }
      if (spec.description !== undefined && typeof spec.description !== "string") {
        throw new Error("registerTool 的 description 必须是字符串");
      }
      if (
        spec.parameters !== undefined &&
        (typeof spec.parameters !== "object" || spec.parameters === null || Array.isArray(spec.parameters))
      ) {
        throw new Error("registerTool 的 parameters 必须是 JSON Schema 对象");
      }
      // 工具名全局唯一：与 registerCapability 的命名空间冲突拒绝同语义（两插件同名工具会静默覆盖）。
      if ([...runtimes.values()].some((r) => r.toolDefs.some((t) => t.name === name))) {
        throw new Error(`工具名 ${name} 已被其他插件占用`);
      }
      const def = wrapPluginTool(runtime, { ...spec, name });
      runtime.toolDefs.push(def);
      registerPluginTools([def]);
      notifyChange();
      return true;
    }
    case "registerCommand": {
      auditCapability(runtime, "command");
      const spec = args[0] as PluginCommandSpec;
      if (
        typeof spec?.id !== "string" ||
        spec.id.length === 0 ||
        spec.id.includes(":") ||
        typeof spec.label !== "string" ||
        typeof spec.runId !== "string"
      ) {
        throw new Error("registerCommand 参数不完整（需要非空 id/label/run）");
      }
      runtime.commandDefs.push(spec);
      notifyChange();
      return true;
    }
    case "registerCapability": {
      const spec = args[0] as PluginCapabilitySpec;
      if (typeof spec?.namespace !== "string" || !spec.methodIds || Object.keys(spec.methodIds).length === 0) {
        throw new Error("registerCapability 至少需要一个方法");
      }
      if (hostCapabilities.has(spec.namespace)) {
        throw new Error(`命名空间 ${spec.namespace} 为宿主保留`);
      }
      if (!spec.namespace.includes(".")) {
        throw new Error("registerCapability 需要反向域名 namespace（含点）与 methods");
      }
      if (pluginCapabilities.has(spec.namespace)) {
        throw new Error(`命名空间 ${spec.namespace} 已被其他插件占用`);
      }
      pluginCapabilities.set(spec.namespace, { owner: runtime.entry.id, methodIds: spec.methodIds });
      notifyChange();
      return true;
    }
    case "registerContribution": {
      const spec = args[0] as PluginContributionSpec;
      if (typeof spec?.point !== "string" || spec.point.length === 0) {
        throw new Error("registerContribution 需要非空 point");
      }
      const key = `${spec.point}:${runtime.entry.id}${spec.id ? `:${spec.id}` : ""}`;
      if (contributions.has(key)) throw new Error(`扩展点 ${key} 已注册，不能重复注册`);
      contributions.set(key, { pluginId: runtime.entry.id, point: spec.point, id: spec.id, payload: spec.payload });
      notifyChange();
      return true;
    }
    case "stateRead": {
      auditCapability(runtime, "state");
      return invoke<unknown>("plugin_read_state", { id: runtime.entry.id });
    }
    case "stateWrite": {
      auditCapability(runtime, "state");
      return invoke("plugin_write_state", { id: runtime.entry.id, data: args[0] ?? {} });
    }
    case "ready":
      return true;
    case "subscribe": {
      auditCapability(runtime, "event");
      if (typeof args[0] === "string") runtime.subscriptions.add(args[0]);
      return true;
    }
    case "emit": {
      auditCapability(runtime, "event");
      const [topic, payload] = args;
      if (typeof topic !== "string" || topic.length === 0) throw new Error("emit 需要 topic");
      emitPluginEvent(topic, payload);
      return true;
    }
    default:
      // 未知方法：旧宿主忽略新插件的新方法（前向兼容）。
      return null;
  }
}

/** 插件工具包装：execute 经桥 RPC 回 worker 执行（透传模型侧取消信号 + 超时收口）；
 * validate 透传（参数已由注册表 JSON.parse，插件在 execute 内自校验）。 */
function wrapPluginTool(runtime: Runtime, spec: PluginToolSpec): ToolDefinition {
  return {
    name: spec.name,
    description: spec.description ?? "",
    parameters: spec.parameters ?? {},
    validate: (args) => args as Record<string, unknown>,
    summarize: (args) => `${spec.name} ${safeSummary(args)}`,
    execute: async (args, exec): Promise<ToolResult> => {
      // 只传 args：代理在 invoke 时统一追加 ctx，避免双重注入破坏 (args, ctx) 契约。
      // exec.signal（用户停止/超时）→ 宿主发 abort 帧置插件 ctx.aborted；超时/中止时 reject 收口，
      // 不让挂死插件永久阻塞 AI 工具轮。
      const raw = await invokeFn(runtime, spec.executeId, [args], {
        signal: exec?.signal,
        timeoutMs: PLUGIN_TOOL_TIMEOUT_MS,
      });
      return raw as ToolResult;
    },
    parallelSafe: spec.parallelSafe,
  };
}

function safeSummary(args: unknown): string {
  try {
    const text = JSON.stringify(args);
    return text.length > 80 ? `${text.slice(0, 80)}…` : text;
  } catch {
    return "参数不可序列化";
  }
}

function invokeFn(runtime: Runtime, fnId: string, args: unknown[], opts?: InvokeOptions): Promise<unknown> {
  if (runtime.disposed) return Promise.reject(new Error("插件已卸载"));
  const seq = ++runtime.invokeSeq;
  return new Promise((resolve, reject) => {
    let timer: ReturnType<typeof setTimeout> | undefined;
    let cleaned = false;
    // 中止（用户停止/超时）：清理资源 → 删挂起项 → 发 abort 帧置插件 ctx.aborted → reject 收口。
    const abortListener = (): void => {
      if (cleaned) return;
      cleaned = true;
      if (timer) clearTimeout(timer);
      runtime.invokePending.delete(seq);
      post(runtime, { kind: "abort", seq });
      reject(new Error("插件调用已中止"));
    };
    const cleanup = (): void => {
      if (cleaned) return;
      cleaned = true;
      if (timer) clearTimeout(timer);
      opts?.signal?.removeEventListener("abort", abortListener);
    };
    if (opts?.timeoutMs !== undefined) {
      timer = setTimeout(() => {
        if (cleaned) return;
        cleaned = true;
        opts?.signal?.removeEventListener("abort", abortListener);
        runtime.invokePending.delete(seq);
        post(runtime, { kind: "abort", seq });
        reject(new Error("插件执行超时"));
      }, opts.timeoutMs);
    }
    runtime.invokePending.set(seq, {
      resolve: (v) => {
        cleanup();
        resolve(v);
      },
      reject: (e) => {
        cleanup();
        reject(e);
      },
      cleanup,
    });
    if (opts?.signal?.aborted) {
      // 调用前已中止：不发起跨进程调用，直接收口（防浪费一次 post）。
      abortListener();
      return;
    }
    try {
      runtime.transport.post({ kind: "invoke", seq, fnId, args });
    } catch (e) {
      cleanup();
      runtime.invokePending.delete(seq);
      reject(e instanceof Error ? e : new Error(String(e)));
      return;
    }
    if (opts?.signal) {
      opts.signal.addEventListener("abort", abortListener, { once: true });
    }
  });
}

/** 在飞 invoke 统一 reject + 清空（卸载/失败时 worker 已终止、永不会回包）。 */
function rejectPending(runtime: Runtime, reason: string): void {
  for (const pending of runtime.invokePending.values()) {
    pending.cleanup();
    pending.reject(new Error(reason));
  }
  runtime.invokePending.clear();
}

/** 能力审计（完全自由：无门槛，仅记录实际调用的命名空间，管理页对照声明 vs 实际）。 */
function auditCapability(runtime: Runtime, namespace: string): void {
  if (runtime.entry.used.length < MAX_AUDIT && !runtime.entry.used.includes(namespace)) {
    runtime.entry.used.push(namespace);
  }
}

function unregisterPluginCapabilities(pluginId: string): void {
  let changed = false;
  for (const [ns, e] of pluginCapabilities) {
    if (e.owner === pluginId) {
      pluginCapabilities.delete(ns);
      changed = true;
    }
  }
  if (changed) notifyChange();
}

function unregisterPluginContributions(pluginId: string): void {
  let changed = false;
  for (const [k, e] of contributions) {
    if (e.pluginId === pluginId) {
      contributions.delete(k);
      changed = true;
    }
  }
  if (changed) notifyChange();
}

function failPlugin(runtime: Runtime, error: string): void {
  if (runtime.disposed) return;
  runtime.disposed = true;
  if (runtime.activeTimer) clearTimeout(runtime.activeTimer);
  // 在飞 invoke 必须 reject（同 unloadPlugin：worker 已终止不再回包）。
  rejectPending(runtime, `插件已停止：${error}`);
  runtime.transport.dispose();
  if (runtime.toolDefs.length > 0) unregisterPluginTools(runtime.toolDefs);
  runtime.toolDefs = [];
  runtime.commandDefs = [];
  clearStreamRelaysFor(runtime.entry.id);
  unregisterPluginCapabilities(runtime.entry.id);
  unregisterPluginContributions(runtime.entry.id);
  runtime.entry.phase = "failed";
  runtime.entry.error = error;
  notifyChange();
  // 保留条目（failed 状态供管理 UI 展示）；disposed 已拦截后续事件/消息投递。
}

// ===== 宿主第一方能力（注册表里的普通提供者） =====

/** app.openPage 能力的中转回调（pluginStore 接线注入，绕开 bridge→store 层上依赖；null = 未接线）。 */
let appPageOpener: ((pageId: string) => void) | null = null;

/** 注入/复位 app.openPage 中转（pluginStore.load 时接线；null 复位供测试）。 */
export function setAppPageOpener(opener: ((pageId: string) => void) | null): void {
  appPageOpener = opener;
}

registerHostCapability(
  "state",
  async (method, args, ctx) => {
    if (method === "read") return invoke<unknown>("plugin_read_state", { id: ctx.pluginId });
    if (method === "write") {
      await invoke("plugin_write_state", { id: ctx.pluginId, data: args[0] ?? {} });
      return true;
    }
    throw new Error(`state 无方法 ${method}`);
  },
  { label: "插件自持状态" },
);

registerHostCapability(
  "app",
  async (method, args) => {
    if (method === "version") return getAppVersion();
    if (method === "platform") return detectPlatform();
    if (method === "openPage") {
      // 打开插件应用页面（app 类型插件入口；命令/其他插件经 bridge.call("app","openPage",[id]) 调用）。
      const id = args[0];
      if (typeof id !== "string" || id.length === 0) throw new Error("app.openPage 需要页面 id");
      if (!appPageOpener) throw new Error("插件页面入口未就绪");
      appPageOpener(id);
      return true;
    }
    throw new Error(`app 无方法 ${method}`);
  },
  { label: "宿主信息" },
);

registerHostCapability(
  "shell",
  async (method, args, ctx) => {
    if (method === "exec") {
      const opts = (args[0] ?? {}) as {
        command?: string;
        args?: string[];
        cwd?: string;
        env?: Record<string, string>;
      };
      if (typeof opts.command !== "string" || opts.command.length === 0) {
        throw new Error("shell.exec 需要 command");
      }
      const command = opts.command;
      const processArgs = opts.args ?? [];
      const sink = ctx.stream;
      if (sink) {
        // 流式：stdout/stderr → chunk{stream,data}；退出 → end{code}；错误 → error。
        // 等待进程结束再 resolve：分发器据此知道流已收尾，不会提前补 end。
        await new Promise<void>((resolve) => {
          runProcess(command, processArgs, { cwd: opts.cwd, env: opts.env }, {
            stdout: (line) => sink.chunk({ stream: "stdout", data: line }),
            stderr: (line) => sink.chunk({ stream: "stderr", data: line }),
            close: (code) => {
              sink.end({ code });
              resolve();
            },
            error: (msg) => {
              sink.error(msg);
              resolve();
            },
          });
        });
        return undefined;
      }
      // 非流式：聚合输出后一次性返回。
      let stdout = "";
      let stderr = "";
      const code = await new Promise<number | null>((resolve, reject) => {
        runProcess(command, processArgs, { cwd: opts.cwd, env: opts.env }, {
          stdout: (line) => {
            stdout += `${line}\n`;
          },
          stderr: (line) => {
            stderr += `${line}\n`;
          },
          close: (c) => resolve(c),
          error: (msg) => reject(new Error(msg)),
        });
      });
      return { code, stdout, stderr };
    }
    throw new Error(`shell 无方法 ${method}`);
  },
  { label: "执行外部程序", sensitive: true },
);

// 糖方法面（registerTool/registerCommand/on/emit）的展示元数据；handler 由 dispatchMethod 承载。
registerHostCapabilityMeta("ai", { label: "注册 AI 工具" });
registerHostCapabilityMeta("command", { label: "注册命令" });
registerHostCapabilityMeta("event", { label: "事件订阅与发布" });
