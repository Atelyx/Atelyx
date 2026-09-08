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
import type { CellValue, CollabPeer, PluginCanvasSnapshot, PluginTableSnapshot } from "@/types";
import type { AgentConfig, ChatTargetResult, ProviderConfig, ReasoningEffort } from "@/types";
import { streamChat } from "@/services/ai/client";
import { registerPluginTools, unregisterPluginTools } from "@/services/ai/tools";
import { AGENT_TOOLS_META } from "@/constants/tools";
import { getAppVersion } from "@/services/app";
import { runProcess } from "@/services/shell";
import { pickDirectory, pickFile, saveFile } from "@/services/dialog";
import { copyImageToClipboard, readClipboardText, writeClipboardText } from "@/services/clipboard";
import { closeWindow, minimizeWindow, toggleMaximizeWindow } from "@/services/window";
import { listVaultTree } from "@/services/vault";
import {
  globVault,
  grepVault,
  listVaultDir,
  readVaultFile,
  readVaultFileWindow,
} from "@/services/vault/aiFiles";
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
  /** 能力包裹链续链信息：worker 侧据此构造 ctx.next（next() 经带 chainId 的 call 回调宿主续链）。 */
  chain?: { ns: string; method: string; chainId: string; stream: boolean };
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

/** 能力包裹（middleware）：宿主能力命名空间 → 包裹方列表（按注册序串链，waterfall）。 */
interface PluginWrapEntry {
  pluginId: string;
  fnId: string;
}
const pluginWrappers = new Map<string, PluginWrapEntry[]>();

/** 包裹链上下文（续链定向）：宿主签发随机 chainId，包裹方 next() 经带 chainId 的 call 回调续链。 */
interface WrapChainContext {
  /** 原调用方插件 id（真实 handler 的 ctx.pluginId 透传它——透明中间件语义）。 */
  callerPluginId: string;
  /** 原调用方的流句柄（流式：sink 帧直达原调用方，包裹只看 args/结果）。 */
  sink?: PluginStreamSink;
  /** 下一层要运行的包裹下标。 */
  nextIndex: number;
  /** 当前在跑的包裹方（续链校验：续链调用必须来自它，防其他插件冒用 chainId）。 */
  pendingWrapperId?: string;
}
const wrapChains = new Map<string, WrapChainContext>();

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
  unregisterPluginWrappers(id);
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
        { stream?: boolean; chainId?: string } | undefined,
      ];
      const chainId = typeof opts?.chainId === "string" ? opts.chainId : undefined;
      if (opts?.stream && !chainId) {
        // 流式调用（非续链）：不回 reply，只发 stream 帧；出错也走 stream error（reply 会被 callStream 丢弃）。
        const sink = makeCallSink(runtime, msg.seq);
        try {
          await dispatchCapability(runtime, ns, method, args ?? [], sink, msg.seq, undefined);
        } catch (e) {
          sink.error(e instanceof Error ? e.message : String(e));
        }
        return;
      }
      // 续链调用（包裹方 next() 回调）走 reply：链路结果经 reply 冒回包裹方 await next()。
      reply(await dispatchCapability(runtime, ns, method, args ?? [], undefined, msg.seq, chainId));
      return;
    }
    reply(await dispatchMethod(runtime, msg.method, msg.args ?? []));
  } catch (e) {
    replyError(e instanceof Error ? e.message : String(e));
  }
}

/** 能力调用分发：审计命名空间；宿主命名空间直接执行（有包裹则走链）；插件命名空间经 invoke 中转。 */
async function dispatchCapability(
  runtime: Runtime,
  ns: string,
  method: string,
  args: unknown[],
  stream: PluginStreamSink | undefined,
  callerSeq: number,
  chainId?: string,
): Promise<unknown> {
  auditCapability(runtime, ns);
  const host = hostCapabilities.get(ns);
  if (host) {
    const wrappers = pluginWrappers.get(ns);
    if (wrappers && wrappers.length > 0) {
      const result = await runWrappedChain(runtime, host, ns, method, args, stream, chainId);
      // 流式收尾契约同非包裹路径：包裹短路/非流式 handler 被流式调用时补 end。
      if (stream && !stream.ended) stream.end(result);
      return result;
    }
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

/**
 * 能力包裹链（waterfall）：宿主能力按注册序被包裹——包裹方收到 (args, ctx, next)，
 * next() 经带 chainId 的 call 消息回调宿主续链（跨线无法传函数，续链以消息往返表达）。
 * 包裹可：改写参数（next(新 args)）、短路（不调 next 直接返回）、后处理（await next() 后改结果）。
 * 流式：sink 帧直达原调用方（包裹只看 args/结果，不拦截流）；链上包裹方已停止时跳过继续。
 */
async function runWrappedChain(
  runtime: Runtime,
  host: HostCapabilityHandler,
  ns: string,
  method: string,
  args: unknown[],
  stream: PluginStreamSink | undefined,
  chainId?: string,
): Promise<unknown> {
  const wrappers = pluginWrappers.get(ns) ?? [];
  let ctx: WrapChainContext;
  if (chainId) {
    const existing = wrapChains.get(chainId);
    if (!existing) throw new Error("包裹链已失效");
    // 续链调用必须来自当前在跑的包裹方（防其他插件冒用 chainId 续链）
    if (existing.pendingWrapperId !== runtime.entry.id) throw new Error("包裹链续链调用方不匹配");
    ctx = existing;
  } else {
    ctx = { callerPluginId: runtime.entry.id, sink: stream, nextIndex: 0 };
    const id = crypto.randomUUID();
    wrapChains.set(id, ctx);
    chainId = id;
  }
  try {
    const step = async (index: number, stepArgs: unknown[]): Promise<unknown> => {
      const w = wrappers[index];
      if (!w) {
        // 链走完：真实 handler——sink 用链上下文（原调用方）的，pluginId 透传原调用方（透明中间件）
        return host(method, stepArgs, { pluginId: ctx.callerPluginId, stream: ctx.sink });
      }
      const owner = runtimes.get(w.pluginId);
      if (!owner || owner.disposed) return step(index + 1, stepArgs); // 包裹方已停止：跳过继续
      ctx.nextIndex = index + 1;
      ctx.pendingWrapperId = w.pluginId;
      const result = await invokeFn(owner, w.fnId, [stepArgs], {
        chain: { ns, method, chainId: chainId as string, stream: !!ctx.sink },
      });
      delete ctx.pendingWrapperId;
      return result;
    };
    return await step(ctx.nextIndex, args);
  } finally {
    if (chainId) wrapChains.delete(chainId);
  }
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
      const existing = pluginCapabilities.get(spec.namespace);
      if (existing) {
        // 冲突默认 first-wins（拒绝 + 告警含持有者）；显式声明 replace 且 requires 同时声明
        // 该命名空间 → last-wins 替换（替换无独立留痕；管理页覆盖展示归能力发现面）
        const manifest = runtime.entry.manifest;
        const canReplace =
          Array.isArray(manifest.replace) &&
          manifest.replace.includes(spec.namespace) &&
          Array.isArray(manifest.requires) &&
          manifest.requires.includes(spec.namespace);
        if (!canReplace) {
          throw new Error(`命名空间 ${spec.namespace} 已被插件 ${existing.owner} 占用`);
        }
        // last-wins 替换：注册表指向新持有者（旧持有者卸载按 owner 清理不误删新项；
        // 新持有者卸载后能力消失、无自动回退——符合无特权原则）
        pluginCapabilities.set(spec.namespace, { owner: runtime.entry.id, methodIds: spec.methodIds });
        notifyChange();
        return true;
      }
      pluginCapabilities.set(spec.namespace, { owner: runtime.entry.id, methodIds: spec.methodIds });
      notifyChange();
      return true;
    }
    case "wrapCapability": {
      const spec = args[0] as { namespace?: unknown; handlerId?: unknown };
      if (typeof spec?.namespace !== "string" || typeof spec?.handlerId !== "string") {
        throw new Error("wrapCapability 需要 { namespace, handlerId }");
      }
      if (spec.namespace.includes(".")) {
        throw new Error("wrapCapability 仅支持宿主能力命名空间（不含点）");
      }
      if (!hostCapabilities.has(spec.namespace)) {
        throw new Error(`能力 ${spec.namespace} 不存在，无法包裹`);
      }
      const list = pluginWrappers.get(spec.namespace) ?? [];
      // 同插件重复包裹 = 原位替换（一个插件对一个能力只包一层）；跨插件同命名空间叠加串链
      const mine = list.findIndex((w) => w.pluginId === runtime.entry.id);
      if (mine >= 0) list[mine] = { pluginId: runtime.entry.id, fnId: spec.handlerId };
      else list.push({ pluginId: runtime.entry.id, fnId: spec.handlerId });
      pluginWrappers.set(spec.namespace, list);
      // 包裹参与审计：包裹即披露对目标能力的接触（管理页「声明 vs 实际」对照可见）
      auditCapability(runtime, spec.namespace);
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
      runtime.transport.post({
        kind: "invoke",
        seq,
        fnId,
        args,
        ...(opts?.chain ? { chain: opts.chain } : {}),
      });
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

/** 撤销某插件的全部能力包裹（卸载/失败时；跨插件同命名空间包裹不受影响）。 */
function unregisterPluginWrappers(pluginId: string): void {
  let changed = false;
  for (const [ns, list] of pluginWrappers) {
    const filtered = list.filter((w) => w.pluginId !== pluginId);
    if (filtered.length !== list.length) {
      if (filtered.length === 0) pluginWrappers.delete(ns);
      else pluginWrappers.set(ns, filtered);
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
  unregisterPluginWrappers(runtime.entry.id);
  runtime.entry.phase = "failed";
  runtime.entry.error = error;
  notifyChange();
  // 保留条目（failed 状态供管理 UI 展示）；disposed 已拦截后续事件/消息投递。
}

// ===== 宿主第一方能力（注册表里的普通提供者） =====

/** dialog 过滤器数组形状校验（{ name, extensions } 数组；extensions 为非空字符串数组）。 */
function isDialogFilters(value: unknown): value is { name: string; extensions: string[] }[] {
  if (!Array.isArray(value) || value.length === 0) return false;
  return value.every(
    (f) =>
      typeof f === "object" &&
      f !== null &&
      typeof (f as { name?: unknown }).name === "string" &&
      Array.isArray((f as { extensions?: unknown }).extensions) &&
      (f as { extensions: unknown[] }).extensions.every((e) => typeof e === "string" && e.length > 0),
  );
}

/** app.openPage 能力的中转回调（pluginStore 接线注入，绕开 bridge→store 层上依赖；null = 未接线）。 */
let appPageOpener: ((pageId: string) => void) | null = null;

/** 注入/复位 app.openPage 中转（pluginStore.load 时接线；null 复位供测试）。 */
export function setAppPageOpener(opener: ((pageId: string) => void) | null): void {
  appPageOpener = opener;
}

/**
 * 表格能力访问（worker 平面 `table` 命名空间的 store 数据源；pluginStore 接线注入，
 * bridge 不 import store——分层：store 经此把当前表格数据与写操作暴露给插件；null = 未接线）。
 */
export interface PluginTableRuntimeAccess {
  /** 当前打开的表格快照（未打开表格时 tableFile 为 null、fields/rows 为空数组）。 */
  snapshot(): PluginTableSnapshot;
  /** 改单元格（value 为 JSON 可序列化值；undefined = 清空单元格）。 */
  updateCell(rowId: string, fieldId: string, value: CellValue | undefined): void;
  /** 追加一行。 */
  addRow(): void;
  /** 删除指定行。 */
  removeRow(rowId: string): void;
  /** 选中行（表格视图联动；null = 取消选中）。 */
  selectRow(rowId: string | null): void;
}

let tableRuntimeAccess: PluginTableRuntimeAccess | null = null;

/** 注入/复位表格能力访问（pluginStore.load 时接线；null 复位供测试）。 */
export function setPluginTableRuntimeAccess(access: PluginTableRuntimeAccess | null): void {
  tableRuntimeAccess = access;
}

/** 协作能力访问（worker 平面 `collab` 命名空间的 store 数据源；pluginStore 接线注入）。 */
export interface PluginCollabAccess {
  /** 同仓库在线用户列表（本端已过滤；可序列化）。 */
  peers(): CollabPeer[];
  /** 上报本端 presence（view 为 null = 离开；表格类插件视图 kind 原样透传）。 */
  setPresence(view: string | null, file: string | null): void;
}

let collabAccess: PluginCollabAccess | null = null;

/** 注入/复位协作能力访问（pluginStore.load 时接线；null 复位供测试）。 */
export function setPluginCollabAccess(access: PluginCollabAccess | null): void {
  collabAccess = access;
}

/**
 * 画布能力访问（worker 平面 `canvas` 命名空间的 store 数据源；pluginStore 接线注入）。
 * 写方法要求已打开可写画布（canvasFile 非空且非只读），否则抛错（守卫在接线实现内）。
 */
export interface PluginCanvasAccess {
  /** 当前画布快照（canvasFile=null = 未打开画布；投影与磁盘/协作格式同构）。 */
  snapshot(): PluginCanvasSnapshot;
  /** 新增节点（type/position/data；宿主生成 id 并守卫可写；返回创建的节点 id）。 */
  addNode(node: { type: string; position: { x: number; y: number }; data?: Record<string, unknown> }): string;
  /** 更新节点 data（浅合并，不碰 position/尺寸）。 */
  updateNode(nodeId: string, patch: Record<string, unknown>): void;
  /** 移动节点（position 变更入 undo）。 */
  moveNode(nodeId: string, position: { x: number; y: number }): void;
  /** 删除节点（连带其边/流/消息；入 undo）。 */
  deleteNode(nodeId: string): void;
  /** 新增边（source/target 自动锚点；宿主生成 id；返回创建的边 id）。 */
  addEdge(edge: {
    source: string;
    target: string;
    sourceHandle?: string;
    targetHandle?: string;
    directed?: boolean;
    linkMode?: string;
  }): string;
  /** 删除边（入 undo）。 */
  deleteEdge(edgeId: string): void;
  /** 选中节点（属性面板联动；null = 取消选中）。 */
  selectNode(nodeId: string | null): void;
}

let canvasAccess: PluginCanvasAccess | null = null;

/** 注入/复位画布能力访问（pluginStore.load 时接线；null 复位供测试）。 */
export function setPluginCanvasAccess(access: PluginCanvasAccess | null): void {
  canvasAccess = access;
}

/** AI 配置访问（worker 平面 `ai` 命名空间的配置数据源；pluginStore 接线注入。
 *  providers 为运行时配置（含 apiKey——key 读取已由 settingsStore 完成，桥侧不碰 keychain）。 */
export interface PluginAiAccess {
  providers: ProviderConfig[];
  agents: AgentConfig[];
  /** 解析对话目标（provider/model；未指定时跟随默认模型），与画布/面板同源。 */
  resolveChatTarget(selection?: { providerId?: string; model?: string }): ChatTargetResult;
}

let settingsAccess: (() => PluginAiAccess | null) | null = null;

/** 注入/复位 AI 配置访问（pluginStore.load 时接线；null 复位供测试）。 */
export function setSettingsAccess(fn: (() => PluginAiAccess | null) | null): void {
  settingsAccess = fn;
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

registerHostCapability(
  "vault",
  async (method, args) => {
    // 仓库文件读取（worker/子进程平面经 bridge.call("vault", …)）；写方法后续按需补齐。
    // 参数全为可序列化值（路径/选项对象），经 JSON 传输无引用跨越；路径安全边界在 Rust safe_join。
    if (method === "listFiles") return listVaultTree();
    if (method === "readFile") {
      const file = args[0];
      if (typeof file !== "string") throw new Error("vault.readFile 需要相对仓库根的文件路径");
      return readVaultFile(file);
    }
    if (method === "readFileWindow") {
      const file = args[0];
      if (typeof file !== "string") throw new Error("vault.readFileWindow 需要相对仓库根的文件路径");
      return readVaultFileWindow(file, args[1] as { offset?: number; limit?: number } | undefined);
    }
    if (method === "listDir") {
      const dir = args[0];
      if (dir !== undefined && typeof dir !== "string") throw new Error("vault.listDir 需要目录路径或省略");
      return listVaultDir(dir as string | undefined);
    }
    if (method === "glob") {
      const pattern = args[0];
      if (typeof pattern !== "string") throw new Error("vault.glob 需要检索模式");
      return globVault(pattern, args[1] as { path?: string } | undefined);
    }
    if (method === "grep") {
      const pattern = args[0];
      if (typeof pattern !== "string") throw new Error("vault.grep 需要检索正则");
      return grepVault(pattern, args[1] as { path?: string; include?: string } | undefined);
    }
    throw new Error(`vault 无方法 ${method}`);
  },
  { label: "仓库文件读取" },
);

registerHostCapability(
  "dialog",
  async (method, args) => {
    // 系统对话框（目录/文件选择 + 保存）；用户取消返回 null。
    if (method === "pickDirectory") return pickDirectory();
    if (method === "pickFile") {
      const filters = args[0];
      if (filters !== undefined && !isDialogFilters(filters)) {
        throw new Error("dialog.pickFile 需要过滤器数组 [{ name, extensions }]");
      }
      return pickFile(filters as { name: string; extensions: string[] }[] | undefined);
    }
    if (method === "saveFile") {
      const opts = args[0];
      if (opts !== undefined && (typeof opts !== "object" || opts === null || Array.isArray(opts))) {
        throw new Error("dialog.saveFile 需要选项对象 { defaultPath?, filters? }");
      }
      const o = opts as { defaultPath?: string; filters?: { name: string; extensions: string[] }[] } | undefined;
      if (o?.filters !== undefined && !isDialogFilters(o.filters)) {
        throw new Error("dialog.saveFile 的 filters 需要过滤器数组 [{ name, extensions }]");
      }
      return saveFile({ defaultPath: o?.defaultPath, filters: o?.filters });
    }
    throw new Error(`dialog 无方法 ${method}`);
  },
  { label: "系统对话框" },
);

registerHostCapability(
  "clipboard",
  async (method, args) => {
    // 系统剪贴板读写（文本 + 图片 dataURL）。安全敏感：可读取/写入用户剪贴板内容。
    if (method === "readText") return readClipboardText();
    if (method === "writeText") {
      const text = args[0];
      if (typeof text !== "string") throw new Error("clipboard.writeText 需要文本");
      await writeClipboardText(text);
      return true;
    }
    if (method === "copyImage") {
      const dataUrl = args[0];
      if (typeof dataUrl !== "string") throw new Error("clipboard.copyImage 需要图片 dataURL");
      await copyImageToClipboard(dataUrl);
      return true;
    }
    throw new Error(`clipboard 无方法 ${method}`);
  },
  { label: "剪贴板读写", sensitive: true },
);

registerHostCapability(
  "window",
  async (method) => {
    // 当前窗口控制（自定义标题栏窗口的最小化/最大化/关闭）。
    if (method === "minimize") {
      await minimizeWindow();
      return true;
    }
    if (method === "toggleMaximize") {
      await toggleMaximizeWindow();
      return true;
    }
    if (method === "close") {
      await closeWindow();
      return true;
    }
    throw new Error(`window 无方法 ${method}`);
  },
  { label: "窗口控制" },
);

registerHostCapability(
  "table",
  async (method, args) => {
    // 当前打开的表格：读快照 + 写操作。变更经事件 `table:changed` 通知（pluginStore 发）。
    if (!tableRuntimeAccess) throw new Error("表格能力未就绪");
    if (method === "snapshot") return tableRuntimeAccess.snapshot();
    if (method === "updateCell") {
      const [rowId, fieldId, value] = args;
      if (typeof rowId !== "string" || typeof fieldId !== "string") {
        throw new Error("table.updateCell 需要 rowId 与 fieldId");
      }
      tableRuntimeAccess.updateCell(rowId, fieldId, value as CellValue | undefined);
      return true;
    }
    if (method === "addRow") {
      tableRuntimeAccess.addRow();
      return true;
    }
    if (method === "removeRow") {
      const rowId = args[0];
      if (typeof rowId !== "string") throw new Error("table.removeRow 需要 rowId");
      tableRuntimeAccess.removeRow(rowId);
      return true;
    }
    if (method === "selectRow") {
      const rowId = args[0];
      if (rowId !== null && typeof rowId !== "string") throw new Error("table.selectRow 需要 rowId 或 null");
      tableRuntimeAccess.selectRow(rowId as string | null);
      return true;
    }
    throw new Error(`table 无方法 ${method}`);
  },
  { label: "表格数据" },
);

registerHostCapability(
  "collab",
  async (method, args) => {
    // 协作在线状态：读 peers + 上报本端 presence。变更经事件 `collab:changed` 通知。
    if (!collabAccess) throw new Error("协作能力未就绪");
    if (method === "peers") return collabAccess.peers();
    if (method === "setPresence") {
      const [view, file] = args;
      if (view !== null && typeof view !== "string") throw new Error("collab.setPresence 需要 view 或 null");
      if (file !== null && typeof file !== "string") throw new Error("collab.setPresence 需要 file 或 null");
      collabAccess.setPresence(view as string | null, file as string | null);
      return true;
    }
    throw new Error(`collab 无方法 ${method}`);
  },
  { label: "协作在线状态" },
);

registerHostCapability(
  "canvas",
  async (method, args) => {
    // 当前打开的画布：读快照 + 写操作（节点/边）。变更经事件 `canvas:changed` 通知（pluginStore 发）。
    if (!canvasAccess) throw new Error("画布能力未就绪");
    if (method === "snapshot") return canvasAccess.snapshot();
    if (method === "addNode") {
      const node = args[0];
      if (
        typeof node !== "object" ||
        node === null ||
        typeof (node as { type?: unknown }).type !== "string" ||
        typeof (node as { position?: unknown }).position !== "object" ||
        (node as { position?: unknown }).position === null
      ) {
        throw new Error("canvas.addNode 需要 { type, position, data? }");
      }
      return canvasAccess.addNode(
        node as { type: string; position: { x: number; y: number }; data?: Record<string, unknown> },
      );
    }
    if (method === "updateNode") {
      const [nodeId, patch] = args;
      if (typeof nodeId !== "string") throw new Error("canvas.updateNode 需要 nodeId");
      if (typeof patch !== "object" || patch === null || Array.isArray(patch)) {
        throw new Error("canvas.updateNode 需要 data 补丁对象");
      }
      canvasAccess.updateNode(nodeId, patch as Record<string, unknown>);
      return true;
    }
    if (method === "moveNode") {
      const [nodeId, position] = args;
      if (typeof nodeId !== "string") throw new Error("canvas.moveNode 需要 nodeId");
      if (typeof position !== "object" || position === null || Array.isArray(position)) {
        throw new Error("canvas.moveNode 需要 position 对象");
      }
      canvasAccess.moveNode(nodeId, position as { x: number; y: number });
      return true;
    }
    if (method === "deleteNode") {
      const nodeId = args[0];
      if (typeof nodeId !== "string") throw new Error("canvas.deleteNode 需要 nodeId");
      canvasAccess.deleteNode(nodeId);
      return true;
    }
    if (method === "addEdge") {
      const edge = args[0];
      if (
        typeof edge !== "object" ||
        edge === null ||
        typeof (edge as { source?: unknown }).source !== "string" ||
        typeof (edge as { target?: unknown }).target !== "string"
      ) {
        throw new Error("canvas.addEdge 需要 { source, target }");
      }
      const e = edge as {
        source: string;
        target: string;
        sourceHandle?: string;
        targetHandle?: string;
        directed?: boolean;
        linkMode?: string;
      };
      return canvasAccess.addEdge(e);
    }
    if (method === "deleteEdge") {
      const edgeId = args[0];
      if (typeof edgeId !== "string") throw new Error("canvas.deleteEdge 需要 edgeId");
      canvasAccess.deleteEdge(edgeId);
      return true;
    }
    if (method === "selectNode") {
      const nodeId = args[0];
      if (nodeId !== null && typeof nodeId !== "string") throw new Error("canvas.selectNode 需要 nodeId 或 null");
      canvasAccess.selectNode(nodeId as string | null);
      return true;
    }
    throw new Error(`canvas 无方法 ${method}`);
  },
  { label: "画布数据" },
);

registerHostCapability("ai", async (method, args, ctx) => {
  // AI 会话能力：模型/Agent 列表 + 流式对话（chunk{type:text|reasoning,text} → end{content,reasoning,finishReason}）。
  // 不传 meta 第三参：保留糖方法面「AI 会话与工具」的展示文案（handler 与 meta 分离，互不覆盖）。
  const access = settingsAccess?.();
  if (method === "chat") {
    if (!access) throw new Error("AI 配置未就绪（未打开仓库）");
    const req = (args[0] ?? {}) as {
      providerId?: string;
      model?: string;
      messages?: unknown;
      reasoningEffort?: string;
      temperature?: unknown;
      maxTokens?: unknown;
      maxRetries?: unknown;
    };
    if (!Array.isArray(req.messages) || req.messages.length === 0) throw new Error("ai.chat 需要非空 messages");
    // 供应商/模型解析：显式 providerId → 查找；否则跟随默认模型（resolveChatTarget 与画布/面板同源）
    let provider: ProviderConfig;
    let model: string;
    if (req.providerId !== undefined) {
      const p = access.providers.find((x) => x.id === req.providerId);
      if (!p) throw new Error(`供应商 ${req.providerId} 不存在`);
      const m = typeof req.model === "string" ? req.model : p.models[0]?.id;
      if (!m) throw new Error("该供应商无可用模型");
      provider = p;
      model = m;
    } else {
      const target = access.resolveChatTarget(typeof req.model === "string" ? { model: req.model } : undefined);
      if (!target.ok) throw new Error(target.error);
      provider = target.provider;
      model = target.model;
    }
    const sink = ctx.stream;
    let content = "";
    let reasoning = "";
    let streamError: Error | null = null;
    let nonStreamResult: { content: string; reasoning: string; finishReason?: unknown } | undefined;
    await streamChat(
      {
        baseUrl: provider.baseUrl,
        apiKey: provider.apiKey,
        model,
        messages: req.messages as Parameters<typeof streamChat>[0]["messages"],
        ...(typeof req.reasoningEffort === "string" ? { reasoningEffort: req.reasoningEffort as ReasoningEffort } : {}),
        ...(typeof req.temperature === "number" ? { temperature: req.temperature } : {}),
        ...(typeof req.maxTokens === "number" ? { maxTokens: req.maxTokens } : {}),
        retry: { maxRetries: typeof req.maxRetries === "number" ? req.maxRetries : 2 },
      },
      {
        onDelta: (t) => {
          content += t;
          sink?.chunk({ type: "text", text: t });
        },
        onReasoningDelta: (t) => {
          reasoning += t;
          sink?.chunk({ type: "reasoning", text: t });
        },
        onDone: (reason) => {
          const result = { content, reasoning, finishReason: reason };
          if (sink) sink.end(result);
          else nonStreamResult = result;
        },
        onError: (e) => {
          if (sink) sink.error(e.message);
          else streamError = e;
        },
      },
    );
    if (sink) return undefined; // sink 已自行 end，分发器不再补 end
    if (streamError) throw streamError;
    return nonStreamResult;
  }
  if (method === "listModels") {
    if (!access) throw new Error("AI 配置未就绪（未打开仓库）");
    const out: Array<{ providerId: string; providerName: string; modelId: string; label: string }> = [];
    for (const p of access.providers) {
      for (const m of p.models) {
        out.push({ providerId: p.id, providerName: p.name, modelId: m.id, label: m.nickname ?? m.id });
      }
    }
    return out;
  }
  if (method === "listAgents") {
    if (!access) throw new Error("AI 配置未就绪（未打开仓库）");
    return access.agents.map((a) => ({ id: a.id, name: a.name }));
  }
  throw new Error(`ai 无方法 ${method}`);
});

// 糖方法面（registerTool/registerCommand/on/emit）的展示元数据；handler 由 dispatchMethod 承载。
registerHostCapabilityMeta("ai", { label: "AI 会话与工具" });
registerHostCapabilityMeta("command", { label: "注册命令" });
registerHostCapabilityMeta("event", { label: "事件订阅与发布" });
