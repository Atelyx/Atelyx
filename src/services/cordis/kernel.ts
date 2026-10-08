/**
 * Cordis 内核宿主：根 Context + 平台类型化服务；平台服务直连 service 层与注入访问（access.ts，无字符串路由中转），canvas/table/note/chat 由对应插件提供。
 * 每窗口一个内核（懒单例，pluginStore.load 首行取用），撕裂窗口 bootstrap 时各自创建；事件发射经 events.ts，审计由 audit.ts 单独安装。
 * 用户插件的 ESM 求值需 React 全局（JSX 经 esbuild 转出 React.createElement 引用，见下方 window.React 声明）。
 */
import React from "react";
import { Context, symbols } from "@atelyx/cordis";
import { getAppVersion } from "@/services/app";
import { detectPlatform } from "@/utils/pluginHost";
import { runProcess, killProcessTree, writeProcessStdin, endProcessStdin } from "@/services/shell";
import { resolveBundledRuntime } from "@/services/bundledRuntime";
import {
  hostRuntimeAttach,
  hostRuntimeDetach,
  hostRuntimeSend,
  subscribeHostRuntime,
} from "@/services/hostRuntime";
import { createRpcChannel, type RpcChannelFeed } from "./rpcChannel";
import { trackPluginSession, trackPendingSessionAttach, untrackPluginSession } from "./pluginRpcSessions";
import { pickDirectory, pickFile, saveFile } from "@/services/dialog";
import { copyImageToClipboard, readClipboardImage, readClipboardText, writeClipboardText } from "@/services/clipboard";
import { closeWindow, listMonitors, minimizeWindow, toggleMaximizeWindow } from "@/services/window";
import { clearPluginTrayMenu, setPluginTrayMenu } from "@/services/trayMenu";
import {
  onGlobalShortcutTriggered,
  registerGlobalShortcut,
  registerWindowToggleShortcut,
  unregisterGlobalShortcut,
} from "@/services/globalShortcut";
import { listVaultTree } from "@/services/vault";
import { pluginKvDelete, pluginKvRead, pluginKvSet, pluginKvWrite, pluginReadState, pluginWriteState } from "@/services/plugins";
import { httpRequest } from "@/services/http";
import {
  externalCreateFolder,
  externalDeleteDir,
  externalDeleteFile,
  externalListDir,
  externalMoveFile,
  externalPrivateDir,
  externalReadFile,
  externalReadFileDataUrl,
  externalRenameFile,
  externalWriteFile,
  externalWriteFileBase64,
} from "@/services/externalFs";
import { registerPluginTools, unregisterPluginTools } from "@/services/ai/tools";
import { registerPluginChannel } from "@/utils/collabHost";
import {
  globVault,
  grepVault,
  listVaultDir,
  readVaultFile,
  readVaultFileWindow,
} from "@/services/vault/aiFiles";
import { streamChat } from "@/services/ai/client";
import { isToolNameTaken, pluginToolDefinition } from "@/services/ai/tools";
import type { PluginToolOptions } from "@/types";
import {
  getAppPageOpener,
  getPluginCollabAccess,
  getPluginNotificationAccess,
  getPluginShortcutAccess,
  getSettingsAccess,
  requireVaultWrite,
  type PluginNotificationAccess,
} from "./access";
import { installAudit, resetAudit } from "./audit";
import { pluginIdOf } from "./loader";
import { trackPendingLaunch, trackPluginProcess, untrackPluginProcess } from "./pluginProcesses";
import {
  dispatchShortcutTrigger,
  declaredShortcutOf,
  trackPendingShortcutRegister,
  trackShortcut,
  trackShortcutDeclaration,
  untrackShortcut,
  untrackShortcutDeclarationIf,
  untrackShortcutIf,
} from "./pluginShortcuts";
import { createSlotsApi } from "./slotsApi";
import { createServicesService } from "./services";
import { nativeInvoke } from "@/services/native";
import { platformCapabilities } from "@/services/platform";
import { createHistoryService } from "./history";
import { createLayoutService } from "./layout";
import { createUiStateService } from "./uiState";
import { createChatService } from "./chat";
import { createUiService } from "./uiFloating";
import { createMarkdownService } from "./markdown";
import { installEventIsolation, setKernelRef } from "./events";
import type {
  AiService,
  AppService,
  ChatResult,
  ClipboardService,
  CollabService,
  DialogService,
  FsService,
  HttpService,
  NativeService,
  NotificationService,
  ProcessExecOptions,
  ProcessExecResult,
  ProcessService,
  RpcAttachOptions,
  RpcChannel,
  RpcService,
  StateService,
  StorageService,
  TrayService,
  TrayMenuEntry,
  VaultService,
  WindowService,
  ShortcutsService,
} from "./types";
import "./types";

declare global {
  interface Window {
    /** 插件 ESM 求值运行时：JSX 转出 React.createElement 引用的全局。 */
    React?: typeof React;
  }
}

/** 进程执行能力（桌面有 / 移动端无）。缺失必须显式可见：调用即以可读错误拒绝，不静默失效。 */
const PROCESS_EXECUTION = platformCapabilities().processExecution;
/** `ctx.process` 在无进程执行能力平台上的拒绝原因（插件按错误处理，宿主不做隐式降级）。 */
const PROCESS_UNAVAILABLE = "当前平台不支持进程执行";

/** ai 服务实例（tracker 注入调用方插件上下文：registerTool 随其 fiber 撤销）。 */
interface AiServiceInstance extends AiService {
  ctx: Context;
}

/** state 服务实例（tracker 注入调用方插件上下文：数据按调用方插件隔离）。 */
interface StateServiceInstance extends StateService {
  ctx: Context;
}

/** storage 服务实例（tracker 注入调用方插件上下文：数据按调用方插件隔离）。 */
interface StorageServiceInstance extends StorageService {
  ctx: Context;
}

/** fs 服务实例（tracker 注入调用方插件上下文：调用归属按调用方插件审计）。 */
interface FsServiceInstance extends FsService {
  ctx: Context;
}

/** process 服务实例（tracker 注入调用方插件上下文：启动的进程按调用方插件记账）。 */
interface ProcessServiceInstance extends ProcessService {
  ctx: Context;
}

/** rpc 服务实例（tracker 注入调用方插件上下文：通道进程按调用方插件记账）。 */
interface RpcServiceInstance extends RpcService {
  ctx: Context;
}

/** tray 服务实例（tracker 注入调用方插件上下文：菜单贡献随调用方插件停用清除）。 */
interface TrayServiceInstance extends TrayService {
  ctx: Context;
}

/** 插件托盘菜单树节点数上限与深度上限（与 Rust 侧对抗性复核同数值）。 */
const TRAY_MENU_MAX_NODES = 64;
const TRAY_MENU_MAX_DEPTH = 3;

/** 托盘菜单树形状预校验：id 命名空间、文案、规模与深度（Rust 侧对入参再做对抗性复核）。
 *  叶子 key = 插件 id + 路径 id（分层命名空间），树内唯一性按 key 判定。 */
function validateTrayMenu(pluginId: string, items: TrayMenuEntry[]): void {
  if (!Array.isArray(items)) throw new Error("ctx.tray.setMenu 需要菜单节点数组");
  const keys = new Set<string>();
  const walk = (nodes: TrayMenuEntry[], depth: number, path: string[]): void => {
    if (depth > TRAY_MENU_MAX_DEPTH) {
      throw new Error(`托盘菜单嵌套超过 ${TRAY_MENU_MAX_DEPTH} 层`);
    }
    if (keys.size + nodes.length > TRAY_MENU_MAX_NODES) {
      throw new Error(`托盘菜单节点数超过 ${TRAY_MENU_MAX_NODES}`);
    }
    for (const node of nodes) {
      if (node.type === "separator") continue;
      if (typeof node.id !== "string" || node.id === "" || node.id.includes(":")) {
        throw new Error(`托盘菜单 id 须为非空且不含 ":" 的字符串`);
      }
      if (typeof node.label !== "string" || node.label.trim() === "") {
        throw new Error("托盘菜单文案不能为空");
      }
      if (node.type === "item") {
        if (typeof node.onActivate !== "function") {
          throw new Error(`托盘菜单项 ${node.id} 需要 onActivate 回调`);
        }
        const key = [pluginId, ...path, node.id].join(":");
        if (keys.has(key)) throw new Error(`托盘菜单项 key 重复：${key}`);
        keys.add(key);
      } else {
        walk(node.items, depth + 1, [...path, node.id]);
      }
    }
  };
  walk(items, 1, []);
}

/** collab 服务实例（tracker 注入调用方插件上下文：频道命名空间与订阅归属由调用方决定）。 */
interface CollabServiceInstance extends CollabService {
  ctx: Context;
}

/** shortcuts 服务实例（tracker 注入调用方插件上下文：注册归属按调用方插件记账）。 */
interface ShortcutsServiceInstance extends ShortcutsService {
  ctx: Context;
}

/** 调用方插件 id：state/storage/fs 的数据落点与授权查表由调用方决定，缺归属的访问（非插件上下文）
 *  直接拒绝——静默落到共享命名空间会制造跨插件数据混写。 */
function requireCallerPluginId(ctx: Context): string {
  const id = pluginIdOf(ctx);
  if (!id) throw new Error("插件服务只能在插件上下文中使用");
  return id;
}

/** 声明热键解析：manifest 缺该声明即报错（可见）；实际键 = 用户覆盖 → 声明默认键。 */
function resolveDeclaredAccelerator(pluginId: string, id: string): string {
  const access = getPluginShortcutAccess();
  if (!access) throw new Error("全局热键声明数据源未就绪");
  const decl = access.declarations(pluginId).find((d) => d.id === id);
  if (!decl) throw new Error(`热键声明 ${id} 不存在（须在 manifest 的 atelyx.shortcuts 中声明）`);
  return access.overrides()[`${pluginId}:${id}`] || decl.key;
}

/** 插件频道线路名 = 插件 id + ":" + 逻辑频道名：命名空间分隔符，跨插件撞名不串台。
 *  channel 须为非空字符串且不含 ":"（保留分隔符），非法输入直接拒绝（不静默改写）。 */
function pluginWireChannel(pluginId: string, channel: string): string {
  if (typeof channel !== "string" || channel === "") throw new Error("协作频道名须为非空字符串");
  if (channel.includes(":")) throw new Error('协作频道名不能包含 ":"（命名空间保留分隔符）');
  return `${pluginId}:${channel}`;
}

/** 流句柄（process/ai 流式：chunk/end/error 帧；已收尾后忽略后续调用）。 */
interface StreamSink {
  chunk(data: unknown): void;
  end(data?: unknown): void;
  error(message: string): void;
  ended: boolean;
}

/** 流句柄工厂：把调用方回调转成流句柄（收尾后忽略后续调用）。 */
function makeStreamSink(handlers: {
  chunk(data: unknown): void;
  end(data?: unknown): void;
  error(message: string): void;
}): StreamSink {
  let ended = false;
  return {
    get ended() {
      return ended;
    },
    chunk: (d) => {
      if (!ended) handlers.chunk(d);
    },
    end: (d) => {
      if (!ended) {
        ended = true;
        handlers.end(d);
      }
    },
    error: (m) => {
      if (!ended) {
        ended = true;
        handlers.error(m);
      }
    },
  };
}

/** 系统对话框过滤器数组形状校验（{ name, extensions } 数组；extensions 为非空字符串数组）。 */
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

/** vault.editFile 编辑项数组形状校验（{ oldText, newText } 数组；空数组合法——服务层按无改动处理）。 */
function isEditEntries(value: unknown): value is { oldText: string; newText: string }[] {
  if (!Array.isArray(value)) return false;
  return value.every(
    (e) =>
      typeof e === "object" &&
      e !== null &&
      typeof (e as { oldText?: unknown }).oldText === "string" &&
      typeof (e as { newText?: unknown }).newText === "string",
  );
}

/** 内核句柄：根 Context + 平台服务撤销（App 生命周期内不销毁；供测试/重载）。 */
export interface Kernel {
  ctx: Context;
  /** 撤销内核提供的全部平台服务（root ctx 本身保留）。 */
  dispose(): void;
}

/** 构造平台服务并挂到根 Context（服务对象引用注入 access 经 getter 惰性读取）。 */
export function createKernel(): Kernel {
  // 事件投递的异常隔离属 emit 语义本身（不是审计式归因包装）：任何内核都必须有，
  // 否则一个插件监听器抛错会静默吃掉同事件其余监听器——故与内核一同就位，进程级幂等。
  installEventIsolation();
  const ctx = new Context();
  const disposables: Array<() => void> = [];

  /** 取通知能力访问；未接线（未加载插件/未打开仓库）时抛错，两个方法口径一致。 */
  function requireNotificationAccess(): PluginNotificationAccess {
    const access = getPluginNotificationAccess();
    if (!access) throw new Error("通知能力未就绪");
    return access;
  }

  /** 在根 Context 上提供平台服务并收集撤销。 */
  function provide(name: string, value: unknown): void {
    disposables.push(ctx.provide(name as never, value as never));
  }

  /** 启动进程并按调用方插件登记 pid（退出即摘除）；pid 到位后 resolve（启动失败则 reject）。
   *
   *  登记是为了让插件停用/卸载能结束它启动的进程（长驻服务不该活过插件本身）；退出即摘除是 pid 复用的唯一防线——留着已退出进程的 pid，之后系统把它分给别的进程时就会被误杀。
   *  pid 解析与退出回调存在竞态（进程可能极快退出并先触发 close）：退出先到时标记 ended，pid 到位后不再登记。`ended` 同时供 spawn 的 cancel 判断 no-op。
   *  只在 `close`（进程真的结束）摘除登记，**不在 `error` 摘除**：`error` 是「运行期出错」，进程可能仍在跑（如管道读取失败），提前摘除会让停用路径漏杀、`cancel()` 变永久 no-op。启动失败时 pid 从未落地、本就无登记。
   *
   *  stdin 关闭策略（`stdinPolicy`）：`"close"` = pid 落地即关（exec 与历史 null-stdin 语义一致——
   *  读 stdin 的一次性程序用 `opts.input` 喂数据）；`"keep"` = 保持开放（spawn 句柄的 write/endInput）。
   *  `opts.input` 传入时两种策略都写一次再关。写失败**不进插件错误面**：进程自身的退出码与输出即
   *  结果，写入失败只说明 input 没送达（进程未读即退出），且与退出事件无到达序保证——进错误面会
   *  造成时序依赖的偶发 reject 并丢掉已聚合的输出，故只记宿主诊断日志。 */
  function launchTrackedProcess(
    pluginId: string,
    opts: ProcessExecOptions,
    stdinPolicy: "close" | "keep",
    handlers: {
      stdout(line: string): void;
      stderr(line: string): void;
      close(code: number | null): void;
      error(message: string): void;
    },
  ): { pid: Promise<number>; ended: () => boolean } {
    let ended = false;
    let pid: number | null = null;
    const pidPromise = runProcess(
      opts.command,
      opts.args ?? [],
      { cwd: opts.cwd, env: opts.env },
      {
        stdout: handlers.stdout,
        stderr: handlers.stderr,
        close: (code) => {
          ended = true;
          if (pid !== null) untrackPluginProcess(ctx, pluginId, pid);
          handlers.close(code);
        },
        error: handlers.error,
      },
    );
    const tracked = pidPromise.then((value) => {
      pid = value;
      if (!ended) trackPluginProcess(ctx, pluginId, value);
      if (opts.input !== undefined) {
        void writeProcessStdin(value, opts.input)
          .then(() => endProcessStdin(value))
          .catch((e) => console.warn("进程 stdin 写入失败（进程可能未读 stdin 即退出）", e));
      } else if (stdinPolicy === "close") {
        void endProcessStdin(value).catch((e) => console.warn("关闭进程 stdin 失败", e));
      }
      return value;
    });
    // 在途登记：pid 还没解析完就停用时，结束流程会等它落地再收 pid（否则漏杀）
    trackPendingLaunch(ctx, pluginId, tracked);
    // exec 两条路径只关心流/聚合结果，不取 pid：这里附一个空 catch，避免启动失败（未登记程序等）
    // 在无人 await 时冒成 unhandled rejection（错误本身仍走 handlers.error 与 spawn 的 reject）。
    void tracked.catch(() => {});
    return { pid: tracked, ended: () => ended };
  }

  // state/storage 按调用方插件隔离：tracker 让插件经 ctx.state/ctx.storage 读取时 `this.ctx`
  // 解析为调用方上下文，归属 id 由宿主推导——API 不暴露 id 参数，伪造他人命名空间无入口
  // （同 ctx.ai/ctx.slots 的绑定机制）。
  const state: StateService = {
    read(this: StateServiceInstance): Promise<unknown> {
      return pluginReadState(requireCallerPluginId(this.ctx));
    },
    write(this: StateServiceInstance, data: unknown): Promise<void> {
      return pluginWriteState(requireCallerPluginId(this.ctx), data);
    },
  };
  Object.defineProperty(state, symbols.tracker, { value: { property: "ctx" } });
  provide("state", state);

  // 键值面（与 ctx.state 同口径隔离）：单键读改写由 Rust 侧串行完成（并发写不丢键），clear 走整表覆盖。
  const storage: StorageService = {
    async get(this: StorageServiceInstance, key: string): Promise<unknown> {
      return (await pluginKvRead(requireCallerPluginId(this.ctx)))[key];
    },
    set(this: StorageServiceInstance, key: string, value: unknown): Promise<void> {
      return pluginKvSet(requireCallerPluginId(this.ctx), key, value);
    },
    delete(this: StorageServiceInstance, key: string): Promise<void> {
      return pluginKvDelete(requireCallerPluginId(this.ctx), key);
    },
    async keys(this: StorageServiceInstance): Promise<string[]> {
      return Object.keys(await pluginKvRead(requireCallerPluginId(this.ctx)));
    },
    clear(this: StorageServiceInstance): Promise<void> {
      return pluginKvWrite(requireCallerPluginId(this.ctx), {});
    },
  };
  Object.defineProperty(storage, symbols.tracker, { value: { property: "ctx" } });
  provide("storage", storage);

  const http: HttpService = {
    request: (req) => httpRequest(req),
  };
  provide("http", http);

  const app: AppService = {
    version: () => getAppVersion(),
    platform: () => Promise.resolve(detectPlatform()),
    openPage: (pageId) => {
      const opener = getAppPageOpener();
      if (!opener) throw new Error("插件页面入口未就绪");
      opener(pageId);
      return Promise.resolve(true);
    },
  };
  provide("app", app);

  const process: ProcessService = {
    exec(this: ProcessServiceInstance, opts, handlers) {
      if (!PROCESS_EXECUTION) return Promise.reject(new Error(PROCESS_UNAVAILABLE));
      const pluginId = requireCallerPluginId(this.ctx);
      if (!handlers) {
        // 非流式：聚合输出后一次性返回。
        // 行事件自带换行终止符（services/shell.ts runProcess 契约，Rust read_line 不剥 \r?\n），
        // 逐段拼接即原始输出；再补 \n 会把每行撑成两行，隔断表格等需要连续行的块结构。
        return new Promise<ProcessExecResult | undefined>((resolve, reject) => {
          let stdout = "";
          let stderr = "";
          launchTrackedProcess(pluginId, opts, "close", {
            stdout: (line) => {
              stdout += line;
            },
            stderr: (line) => {
              stderr += line;
            },
            close: (code) => resolve({ code, stdout, stderr }),
            error: (msg) => reject(new Error(msg)),
          });
        });
      }
      const sink = makeStreamSink({
        chunk: (d) => handlers.chunk(d as { stream: "stdout" | "stderr"; data: string }),
        end: (d) => handlers.end(d as { code: number | null }),
        error: (m) => handlers.error(m),
      });
      // 流式：stdout/stderr → chunk{stream,data}；退出 → end{code}；错误 → error。
      // 等待进程结束再 resolve：流已收尾，不会提前补 end。
      return new Promise<undefined>((resolve) => {
        launchTrackedProcess(pluginId, opts, "close", {
          stdout: (line) => sink.chunk({ stream: "stdout", data: line }),
          stderr: (line) => sink.chunk({ stream: "stderr", data: line }),
          close: (code) => {
            sink.end({ code });
            resolve(undefined);
          },
          error: (msg) => {
            sink.error(msg);
            resolve(undefined);
          },
        });
      });
    },
    spawn(this: ProcessServiceInstance, opts, handlers) {
      if (!PROCESS_EXECUTION) return Promise.reject(new Error(PROCESS_UNAVAILABLE));
      const pluginId = requireCallerPluginId(this.ctx);
      const sink = makeStreamSink(
        handlers
          ? {
              chunk: (d) => handlers.chunk(d as { stream: "stdout" | "stderr"; data: string }),
              end: (d) => handlers.end(d as { code: number | null }),
              error: (m) => handlers.error(m),
            }
          : {
              // 未传 handlers 时仍不静默：启动与运行期错误记控制台（调用方只要 pid 的场景）。
              // 代价是这条路径观察不到进程退出——要监督退出就传 handlers。
              chunk: () => {},
              end: () => {},
              error: (message) => console.error("插件进程错误", message),
            },
      );
      const launched = launchTrackedProcess(pluginId, opts, "keep", {
        stdout: (line) => sink.chunk({ stream: "stdout", data: line }),
        stderr: (line) => sink.chunk({ stream: "stderr", data: line }),
        close: (code) => sink.end({ code }),
        error: (message) => sink.error(message),
      });
      return launched.pid.then((pid) => ({
        pid,
        write: (data: string) => writeProcessStdin(pid, data),
        endInput: () => endProcessStdin(pid),
        cancel: async () => {
          // 已结束即 no-op：退出时登记已摘除，此时该 pid 可能已被系统复用给别的进程
          if (launched.ended()) return;
          await killProcessTree(pid);
          untrackPluginProcess(ctx, pluginId, pid);
        },
      }));
    },
    bundledRuntime(this: ProcessServiceInstance) {
      // 指路面：未分发运行时的平台返回 null（可探测降级），与 exec/spawn 的「调用即拒绝」不同——
      // 探测本身要能成功，插件才谈得上按自己的口径降级。归属校验与进程记账同源（非插件上下文拒绝）。
      if (!PROCESS_EXECUTION) return Promise.resolve(null);
      requireCallerPluginId(this.ctx);
      return resolveBundledRuntime();
    },
  };
  Object.defineProperty(process, symbols.tracker, { value: { property: "ctx" } });
  provide("process", process);

  // 插件双半通道：宿主半进程经进程托管面启动（记账/退出清理与 ctx.process 同源），stdio 上的
  // JSON-RPC 会话由 rpcChannel 机制承载。stderr 不参与协议（诊断材料）；握手失败不留活进程。
  const rpc: RpcService = {
    async connect(this: RpcServiceInstance, opts) {
      if (!PROCESS_EXECUTION) return Promise.reject(new Error(PROCESS_UNAVAILABLE));
      const pluginId = requireCallerPluginId(this.ctx);
      if (opts.input !== undefined) throw new Error("rpc.connect 不支持 input（stdin 由通道协议占用）");
      const { initializeTimeoutMs, ...spawnOpts } = opts;
      // feed 就位前的行/事件先暂存：进程可能在 pid 解析前就开始输出或退出；持有点用 ref
      //（feed 在 pid 落地后才构造，回调必须能引用到它）
      const feedRef: { current?: RpcChannelFeed } = {};
      const backlog: string[] = [];
      let earlyClose: number | null | undefined;
      let earlyError: string | undefined;
      const launched = launchTrackedProcess(pluginId, spawnOpts, "keep", {
        stdout: (line) =>
          feedRef.current ? feedRef.current.receiveLine(line) : backlog.push(line),
        stderr: () => {},
        close: (code) =>
          feedRef.current ? feedRef.current.transportClosed(code) : (earlyClose = code),
        error: (message) =>
          feedRef.current ? feedRef.current.transportFailed(message) : (earlyError = message),
      });
      const pid = await launched.pid;
      const feed = createRpcChannel(
        pid,
        {
          write: (data) => writeProcessStdin(pid, data),
          kill: async () => {
            if (launched.ended()) return;
            await killProcessTree(pid);
            untrackPluginProcess(ctx, pluginId, pid);
          },
        },
        { initializeTimeoutMs },
      );
      feedRef.current = feed;
      for (const line of backlog.splice(0)) feed.receiveLine(line);
      if (earlyClose !== undefined) feed.transportClosed(earlyClose);
      else if (earlyError !== undefined) feed.transportFailed(earlyError);
      try {
        await feed.initialize();
      } catch (error) {
        // 收尾失败不顶掉原始握手错误（进程登记仍在，随插件停用/退出兜底结束）
        await feed.channel.close().catch(() => {});
        throw error;
      }
      return feed.channel;
    },

    attach(this: RpcServiceInstance, opts: RpcAttachOptions): Promise<RpcChannel> {
      // 非 async 形态：返回值就是已登记的 attachPromise 本身——插件 `void ctx.rpc.attach(...)`
      // 后被停用时，在途登记的 settle 已在其上挂了拒绝处理器，不会冒成未处理拒绝
      try {
        const pluginId = requireCallerPluginId(this.ctx);
        if (!PROCESS_EXECUTION) return Promise.reject(new Error(PROCESS_UNAVAILABLE));
        if (typeof opts?.module !== "string" || opts.module === "") {
          return Promise.reject(new Error("ctx.rpc.attach 需要宿主半模块路径（module）"));
        }
        if (opts.args !== undefined && !Array.isArray(opts.args)) {
          return Promise.reject(new Error("ctx.rpc.attach 的 args 须为数组"));
        }
        // 先订下行事件再发起 attach：attach-ok 之前模块线程就可能产出帧（activate 期反向调用），
        // 事件载荷自带 sessionId，先收进 buffer，拿到 sessionId 后按会话过滤重放
        const attachPromise = (async (): Promise<RpcChannel> => {
          const known: { current: number | null } = { current: null };
          const backlog: Array<{ sessionId: number; kind: "frame" | "end"; payload: string }> = [];
          const feedRef: { current?: RpcChannelFeed } = {};
          const unlisten = await subscribeHostRuntime({
            onFrame: (sessionId, frame) => {
              if (known.current === null) backlog.push({ sessionId, kind: "frame", payload: frame });
              else if (sessionId === known.current) feedRef.current?.receiveLine(frame);
            },
            onSessionEnded: (sessionId, reason) => {
              if (known.current === null) backlog.push({ sessionId, kind: "end", payload: reason });
              else if (sessionId === known.current) feedRef.current?.transportClosed(null);
            },
          });
          try {
            const started = await hostRuntimeAttach(pluginId, opts.module, opts.args ?? null);
            known.current = started.sessionId;
            const feed = createRpcChannel(
              started.pid,
              {
                write: (data) => hostRuntimeSend(started.sessionId, data),
                kill: async () => {
                  await hostRuntimeDetach(started.sessionId);
                },
              },
              { initializeTimeoutMs: opts.initializeTimeoutMs },
            );
            feedRef.current = feed;
            for (const item of backlog) {
              if (item.sessionId !== started.sessionId) continue;
              if (item.kind === "frame") feed.receiveLine(item.payload);
              else feed.transportClosed(null);
            }
            try {
              await feed.initialize();
            } catch (error) {
              // 收尾失败不顶掉原始握手错误（会话登记仍在，随插件停用/退出兜底卸载）
              await feed.channel.close().catch(() => {});
              throw error;
            }
            trackPluginSession(ctx, pluginId, {
              sessionId: started.sessionId,
              close: () => feed.channel.close(),
            });
            // 通道关闭即出册并退订（主动 close / 熔断会话结束 / 运行时进程死亡）
            void feed.channel.done.then(() => {
              untrackPluginSession(ctx, pluginId, started.sessionId);
              unlisten();
            });
            return feed.channel;
          } catch (error) {
            unlisten();
            throw error;
          }
        })();
        // 完整 attach 流程先登记后落地的部分（订阅就绪之后的段落）：插件不等返回即被停用时，
        // 结束流程等它落地再卸载（与进程的在途启动登记同一纪律）
        trackPendingSessionAttach(ctx, pluginId, attachPromise);
        return attachPromise;
      } catch (error) {
        return Promise.reject(error instanceof Error ? error : new Error(String(error)));
      }
    },
  };
  Object.defineProperty(rpc, symbols.tracker, { value: { property: "ctx" } });
  provide("rpc", rpc);

  const vault: VaultService = {
    listFiles: () => listVaultTree(),
    readFile: (file) => readVaultFile(file),
    readFileWindow: (file, opts) => readVaultFileWindow(file, opts),
    listDir: (dir) => listVaultDir(dir),
    glob: (pattern, opts) => globVault(pattern, opts),
    grep: (pattern, opts) => grepVault(pattern, opts),
    writeFile: (file, content) => requireVaultWrite().writeFile(file, content),
    editFile: (file, edits) => {
      if (!isEditEntries(edits)) throw new Error("vault.editFile 需要编辑项数组 [{ oldText, newText }]");
      return requireVaultWrite().editFile(file, edits);
    },
    appendFile: (file, content) => requireVaultWrite().appendFile(file, content),
    renameFile: (oldPath, newName) => requireVaultWrite().renameFile(oldPath, newName),
    moveFile: (oldPath, targetDir) => requireVaultWrite().moveFile(oldPath, targetDir),
    deleteFile: (path) => requireVaultWrite().deleteFile(path),
    deleteDir: (dir, force) => requireVaultWrite().deleteDir(dir, force),
    createFolder: (dir) => requireVaultWrite().createFolder(dir),
  };
  provide("vault", vault);

  // 仓库外文件读写（外部文件服务面）：作用域为任意绝对路径、无目录授权门槛（插件与宿主同 realm，事前授权不构成可信边界；调用经 audit 层按调用方插件记录方法与路径）。
  // 每方法先取调用方插件 id：非插件上下文同步拒绝——fs 面只对插件开放；id 不入命令参数，私有目录（privateDir）按它定位。模型工具走 vault，结构性够不到本面。
  const fs: FsService = {
    readFile(this: FsServiceInstance, path) {
      requireCallerPluginId(this.ctx);
      return externalReadFile(path);
    },
    async writeFile(this: FsServiceInstance, path, content) {
      requireCallerPluginId(this.ctx);
      await externalWriteFile(path, content);
      return { ok: true, summary: `已写入「${path}」` };
    },
    listDir(this: FsServiceInstance, path) {
      requireCallerPluginId(this.ctx);
      return externalListDir(path);
    },
    async createFolder(this: FsServiceInstance, path) {
      requireCallerPluginId(this.ctx);
      await externalCreateFolder(path);
      return { ok: true, summary: `已创建「${path}」`, path };
    },
    async renameFile(this: FsServiceInstance, path, newName) {
      requireCallerPluginId(this.ctx);
      const actualPath = await externalRenameFile(path, newName);
      return { ok: true, summary: `已重命名「${path}」`, actualPath };
    },
    async moveFile(this: FsServiceInstance, path, targetDir) {
      requireCallerPluginId(this.ctx);
      const actualPath = await externalMoveFile(path, targetDir);
      return { ok: true, summary: `已移动「${path}」`, actualPath };
    },
    async deleteFile(this: FsServiceInstance, path) {
      requireCallerPluginId(this.ctx);
      await externalDeleteFile(path);
      return { ok: true, summary: `已删除「${path}」` };
    },
    async deleteDir(this: FsServiceInstance, path, force) {
      requireCallerPluginId(this.ctx);
      const r = await externalDeleteDir(path, force === true);
      return {
        ok: r.deleted,
        summary: r.deleted
          ? `已删除目录「${path}」`
          : `目录非空（${r.itemCount} 项），需确认后删除`,
        needsConfirm: r.needsConfirm,
        itemCount: r.itemCount,
      };
    },
    privateDir(this: FsServiceInstance) {
      return externalPrivateDir(requireCallerPluginId(this.ctx));
    },
    writeFileBase64(this: FsServiceInstance, path, base64Data) {
      requireCallerPluginId(this.ctx);
      // 非 async：归属校验同步抛出（非插件上下文立刻拒绝，不落成静默 rejection）
      return externalWriteFileBase64(path, base64Data).then(
        () => ({ ok: true, summary: `已写入「${path}」` }),
      );
    },
    readFileDataUrl(this: FsServiceInstance, path) {
      requireCallerPluginId(this.ctx);
      return externalReadFileDataUrl(path);
    },
  };
  Object.defineProperty(fs, symbols.tracker, { value: { property: "ctx" } });
  provide("fs", fs);

  const dialog: DialogService = {
    pickDirectory: () => pickDirectory(),
    pickFile: (filters) => {
      if (filters !== undefined && !isDialogFilters(filters)) {
        throw new Error("dialog.pickFile 需要过滤器数组 [{ name, extensions }]");
      }
      return pickFile(filters);
    },
    saveFile: (opts) => {
      if (opts?.filters !== undefined && !isDialogFilters(opts.filters)) {
        throw new Error("dialog.saveFile 的 filters 需要过滤器数组 [{ name, extensions }]");
      }
      return saveFile({ defaultPath: opts?.defaultPath, filters: opts?.filters });
    },
  };
  provide("dialog", dialog);

  const clipboard: ClipboardService = {
    readText: () => readClipboardText(),
    writeText: (text) => writeClipboardText(text),
    copyImage: (dataUrl) => copyImageToClipboard(dataUrl),
    readImage: () => readClipboardImage(),
  };
  provide("clipboard", clipboard);

  const windowSvc: WindowService = {
    minimize: () => minimizeWindow(),
    toggleMaximize: () => toggleMaximizeWindow(),
    close: () => closeWindow(),
    listMonitors: () => listMonitors(),
  };
  provide("window", windowSvc);

  // 插件托盘菜单贡献：整树写入 Rust 注册表（与内置项平铺同层，插件间分隔），
  // 点击由 Rust 定向回传注册窗口的桥分发。归属校验与形状预校验在此，
  // 生命周期经 ctx.effect 随插件停用清除（Rust 侧同步移除，残留点击被白名单拦下）。
  const tray: TrayService = {
    setMenu(this: TrayServiceInstance, items) {
      const pluginId = requireCallerPluginId(this.ctx);
      validateTrayMenu(pluginId, items);
      const ctx = this.ctx;
      return new Promise<void>((resolve, reject) => {
        ctx.effect(() => {
          const applied = setPluginTrayMenu(pluginId, items);
          void applied.then(resolve, reject);
          return () => {
            // 等写入落地再清（invoke 到达序无保证，clear 先到会被后到的 set 覆盖出残留）
            void applied
              .catch(() => {})
              .then(() => clearPluginTrayMenu(pluginId))
              .catch((e) => console.error("清除插件托盘菜单失败", e));
          };
        });
      });
    },
  };
  Object.defineProperty(tray, symbols.tracker, { value: { property: "ctx" } });
  provide("tray", tray);

  // 全局快捷键按调用方插件记账：登记先于注册 promise 落地（停用可覆盖在途注册），
  // 注册失败即摘册；OS 层归属仲裁（同键唯一、仅归属者可注销）在 Rust 登记表。
  // 触发事件由 Rust 固定转发主窗口，本内核收到后按原始注册串分发给登记的回调。
  const shortcuts: ShortcutsService = {
    async registerGlobal(this: ShortcutsServiceInstance, accelerator, handler) {
      const pluginId = requireCallerPluginId(this.ctx);
      if (typeof accelerator !== "string" || accelerator.trim() === "") {
        throw new Error("快捷键须为非空字符串");
      }
      if (typeof handler !== "function") throw new Error("快捷键回调须为函数");
      const register = registerGlobalShortcut(accelerator, pluginId);
      trackShortcut(ctx, pluginId, accelerator, handler);
      trackPendingShortcutRegister(ctx, pluginId, register);
    try {
      await register;
    } catch (e) {
      // 条件摘册：并发同键注册时，先发注册的失败回滚不得摘掉后发注册刚登记的条目
      untrackShortcutIf(ctx, pluginId, accelerator, handler);
      throw e;
    }
    },
    async registerDeclared(this: ShortcutsServiceInstance, id, handler) {
      const pluginId = requireCallerPluginId(this.ctx);
      if (typeof id !== "string" || id.trim() === "") throw new Error("热键声明 id 须为非空字符串");
      if (typeof handler !== "function") throw new Error("快捷键回调须为函数");
      const accelerator = resolveDeclaredAccelerator(pluginId, id);
      const register = registerGlobalShortcut(accelerator, pluginId);
      trackShortcut(ctx, pluginId, accelerator, handler);
      trackShortcutDeclaration(ctx, pluginId, accelerator, { id });
      trackPendingShortcutRegister(ctx, pluginId, register);
      try {
        await register;
      } catch (e) {
        // 条件摘册（回调 + 声明元数据）：先发注册的失败回滚不得摘掉后发注册刚登记的条目
        untrackShortcutIf(ctx, pluginId, accelerator, handler);
        untrackShortcutDeclarationIf(ctx, pluginId, accelerator, id);
        throw e;
      }
    },
    async registerDeclaredWindowToggle(this: ShortcutsServiceInstance, id, view, options) {
      const pluginId = requireCallerPluginId(this.ctx);
      if (typeof id !== "string" || id.trim() === "") throw new Error("热键声明 id 须为非空字符串");
      if (typeof view !== "string" || view.trim() === "") {
        throw new Error("窗口切换热键需要非空视图 kind");
      }
      if (typeof options !== "object" || options === null) {
        throw new Error("窗口切换热键需要窗口选项（WindowOptions）");
      }
      const accelerator = resolveDeclaredAccelerator(pluginId, id);
      const register = registerWindowToggleShortcut(accelerator, pluginId, { view, options });
      // 无 JS 回调可摘；声明元数据登记供设置页改键时按声明定位重注册
      trackShortcutDeclaration(ctx, pluginId, accelerator, { id, windowToggle: { view, options } });
      trackPendingShortcutRegister(ctx, pluginId, register);
      try {
        await register;
      } catch (e) {
        // 条件摘册：注册失败不得留下幽灵声明元数据（否则改键路径会为一条从未注册成功的声明工作）
        untrackShortcutDeclarationIf(ctx, pluginId, accelerator, id);
        throw e;
      }
    },
    async registerWindowToggle(this: ShortcutsServiceInstance, accelerator, view, options) {
      const pluginId = requireCallerPluginId(this.ctx);
      if (typeof accelerator !== "string" || accelerator.trim() === "") {
        throw new Error("快捷键须为非空字符串");
      }
      if (typeof view !== "string" || view.trim() === "") {
        throw new Error("窗口切换热键需要非空视图 kind");
      }
      if (typeof options !== "object" || options === null) {
        throw new Error("窗口切换热键需要窗口选项（WindowOptions）");
      }
      // 无 JS 回调可摘：幂等与声明变更由 Rust 登记表原地处理；随插件停用由 release 整体注销。
      // 在途注册照常进 pending 表——「注册后立即停用」时 release 先等注册落地再整体注销，
      // 漏登记会留下归属插件已停用、OS 层仍注册占用的幽灵热键
      const register = registerWindowToggleShortcut(accelerator, pluginId, { view, options });
      trackPendingShortcutRegister(ctx, pluginId, register);
      await register;
    },
    async unregisterGlobal(this: ShortcutsServiceInstance, accelerator) {
      const pluginId = requireCallerPluginId(this.ctx);
      if (typeof accelerator !== "string" || accelerator.trim() === "") {
        throw new Error("快捷键须为非空字符串");
      }
      untrackShortcut(ctx, pluginId, accelerator);
      await unregisterGlobalShortcut(accelerator, pluginId);
    },
    async unregisterDeclared(this: ShortcutsServiceInstance, id) {
      const pluginId = requireCallerPluginId(this.ctx);
      if (typeof id !== "string" || id.trim() === "") throw new Error("热键声明 id 须为非空字符串");
      const reg = declaredShortcutOf(ctx, pluginId, id);
      if (!reg) return;
      untrackShortcutDeclarationIf(ctx, pluginId, reg.accelerator, id);
      if (reg.handler) untrackShortcut(ctx, pluginId, reg.accelerator);
      await unregisterGlobalShortcut(reg.accelerator, pluginId);
    },
  };
  Object.defineProperty(shortcuts, symbols.tracker, { value: { property: "ctx" } });
  provide("shortcuts", shortcuts);

  void onGlobalShortcutTriggered((accelerator) => {
    if (!dispatchShortcutTrigger(ctx, accelerator)) {
      console.warn(`全局快捷键 ${accelerator} 触发，但本窗口没有已挂载插件登记的回调`);
    }
  }).then(
    (unlisten) => {
      disposables.push(unlisten);
    },
    (e) => console.error("全局快捷键触发事件订阅失败", e),
  );

  const ai: AiService = {
    chat: async (req, handlers) => {
      const access = getSettingsAccess()?.();
      if (!access) throw new Error("AI 配置未就绪（未打开仓库）");
      if (req.messages.length === 0) throw new Error("ai.chat 需要非空 messages");
      // 供应商/模型解析：显式 providerId → 查找；否则跟随默认模型（resolveChatTarget 与画布/面板同源）
      let provider;
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
      const options = {
        baseUrl: provider.baseUrl,
        apiKey: provider.apiKey,
        model,
        messages: req.messages as Parameters<typeof streamChat>[0]["messages"],
        ...(req.reasoningEffort ? { reasoningEffort: req.reasoningEffort } : {}),
        ...(req.temperature !== undefined ? { temperature: req.temperature } : {}),
        ...(req.maxTokens !== undefined ? { maxTokens: req.maxTokens } : {}),
        retry: { maxRetries: typeof req.maxRetries === "number" ? req.maxRetries : 2 },
        ...(req.signal ? { signal: req.signal } : {}),
      };
      let content = "";
      let reasoning = "";
      let streamError: Error | null = null;
      let nonStreamResult: ChatResult | undefined;
      await streamChat(options, {
        onDelta: (t) => {
          content += t;
          handlers?.chunk({ type: "text", text: t });
        },
        onReasoningDelta: (t) => {
          reasoning += t;
          handlers?.chunk({ type: "reasoning", text: t });
        },
        onDone: (reason) => {
          const result = { content, reasoning, finishReason: reason };
          if (handlers) handlers.end(result);
          else nonStreamResult = result;
        },
        onError: (e) => {
          if (handlers) handlers.error(e.message);
          else streamError = e;
        },
      });
      if (handlers) return undefined;
      if (streamError) throw streamError;
      return nonStreamResult;
    },
    listModels: () => {
      const access = getSettingsAccess()?.();
      if (!access) throw new Error("AI 配置未就绪（未打开仓库）");
      const out: Array<{ providerId: string; providerName: string; modelId: string; label: string }> = [];
      for (const p of access.providers) {
        for (const m of p.models) {
          out.push({ providerId: p.id, providerName: p.name, modelId: m.id, label: m.nickname ?? m.id });
        }
      }
      return Promise.resolve(out);
    },
    listAgents: () => {
      const access = getSettingsAccess()?.();
      if (!access) throw new Error("AI 配置未就绪（未打开仓库）");
      return Promise.resolve(access.agents.map((a) => ({ id: a.id, name: a.name })));
    },
    registerTool(this: AiServiceInstance, opts: PluginToolOptions): () => void {
      if (typeof opts.name !== "string" || !/^[a-z0-9_]+$/.test(opts.name)) {
        throw new Error("工具名须为非空标识（小写字母/数字/下划线）");
      }
      // 名字是注册表与模型名册的联结键：重名会同时污染名册与分发（后者静默覆盖宿主工具），直接拒绝。
      if (isToolNameTaken(opts.name)) {
        throw new Error(`工具名已被占用：${opts.name}（请换名）`);
      }
      const ctx = this.ctx;
      const def = pluginToolDefinition(opts);
      return ctx.effect(() => {
        registerPluginTools([def]);
        return () => unregisterPluginTools([def]);
      });
    },
  };
  // tracker：插件经 ctx.ai 读取时 `this.ctx` 解析为调用方上下文（工具注册随其 fiber 撤销）。
  Object.defineProperty(ai, symbols.tracker, { value: { property: "ctx" } });
  provide("ai", ai);

  const collab: CollabService = {
    peers: () => {
      const access = getPluginCollabAccess();
      if (!access) throw new Error("协作能力未就绪");
      return access.peers();
    },
    setPresence: (view, file) => {
      const access = getPluginCollabAccess();
      if (!access) throw new Error("协作能力未就绪");
      access.setPresence(view, file);
    },
    sendMessage(this: CollabServiceInstance, channel, payload, opts) {
      const access = getPluginCollabAccess();
      if (!access) throw new Error("协作能力未就绪");
      const wire = pluginWireChannel(requireCallerPluginId(this.ctx), channel);
      return access.sendMessage(wire, payload, opts?.to);
    },
    subscribe(this: CollabServiceInstance, channel, handler) {
      if (typeof handler !== "function") throw new Error("协作频道订阅需要处理函数");
      const wire = pluginWireChannel(requireCallerPluginId(this.ctx), channel);
      const ctx = this.ctx;
      return ctx.effect(() => registerPluginChannel(wire, handler));
    },
    myPeer: () => {
      const access = getPluginCollabAccess();
      if (!access) throw new Error("协作能力未就绪");
      return access.myPeer();
    },
    acquire: () => {
      const access = getPluginCollabAccess();
      if (!access) throw new Error("协作能力未就绪");
      return access.acquire();
    },
  };
  // tracker：插件经 ctx.collab 读取时 `this.ctx` 解析为调用方上下文（sendMessage/subscribe 按调用方绑定）。
  Object.defineProperty(collab, symbols.tracker, { value: { property: "ctx" } });
  provide("collab", collab);

  const notification: NotificationService = {
    notify: (input) => requireNotificationAccess().notify(input),
    dismiss: (id) => requireNotificationAccess().dismiss(id),
  };
  provide("notification", notification);

  // 插件 UI 注册 API（视图槽/表格视图；经 tracker 绑定调用方插件 fiber）。
  provide("slots", createSlotsApi());

  // 服务注册表查询（ctx.services）：插件发现当前可用服务面与提供者；get 判空读取（可选依赖）。
  provide("services", createServicesService());

  // 原始 Rust 命令逃生舱（ctx.native.invoke）：未封装命令经此触达，调用形状经审计脱敏记录。
  const native: NativeService = {
    invoke: (command, args) => nativeInvoke(command, args),
  };
  provide("native", native);

  // 领域/布局/UI 状态服务（内核提供，root 作用域；实现 = 注入访问对象，见 access.ts）。
  provide("history", createHistoryService());
  provide("layout", createLayoutService());
  provide("uiState", createUiStateService());

  // AI 对话能力（内核提供）：编排半挂「对话核心」行注册的运行时、容器半挂对话面板接线。
  // 由内核提供而非某一插件行提供——运行时提供者是可被替换的（组合接管），
  // 服务本身随提供者消失会让消费方拿到「服务不存在」而不是可降级的「能力未就绪」。
  provide("chat", createChatService());

  // 插件浮层承载（ctx.ui，内核平台能力）：定位/层级/收起语义由宿主代管，登记随调用方 fiber 撤销。
  provide("ui", createUiService());

  // Markdown 渲染服务（内核提供，root 作用域）：能力来自框架无关内核，不依赖任何插件行，
  // 停用笔记/表格等插件后仍可用（插件可据此渲染自己的 Markdown 内容）。
  provide("markdown", createMarkdownService());

  return {
    ctx,
    dispose: () => {
      for (const d of disposables) d();
      disposables.length = 0;
    },
  };
}

let kernel: Kernel | null = null;
let auditDispose: (() => void) | null = null;
let reactGlobalSet = false;

/** 插件 ESM 求值运行时：JSX 经 esbuild 转出 React.createElement 引用（window.React 全局）。
 *  幂等一次；node（测试）无 window 跳过。 */
function ensurePluginRuntimeGlobals(): void {
  if (reactGlobalSet || typeof window === "undefined") return;
  reactGlobalSet = true;
  window.React = React;
}

/** 内核懒单例（每窗口一个；pluginStore.load 首行取用）。测试请直接用 createKernel。 */
export function getKernel(): Kernel {
  if (!kernel) {
    ensurePluginRuntimeGlobals();
    kernel = createKernel();
    // 审计（服务读 + 事件订阅归属）是「归因」包装，只在应用路径安装（测试不装，避免把测试自身的
    // 服务访问记进归属表）；emit 异常隔离属 emit 语义本身，已随 createKernel 就位。
    auditDispose = installAudit();
    setKernelRef(kernel);
  }
  return kernel;
}

/** 复位懒单例（供测试）。 */
export function resetKernel(): void {
  if (kernel) {
    kernel.dispose();
    auditDispose?.();
    auditDispose = null;
    resetAudit();
    setKernelRef(null);
    kernel = null;
  }
}
