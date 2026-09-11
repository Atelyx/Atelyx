/**
 * Cordis 内核宿主：根 Context + 平台类型化服务（typed ctx 服务）。
 *
 * 服务实现 = 宿主侧直连：平台服务（state/app/shell/vault/dialog/clipboard/window/ai/collab）
 * 直接调用 service 层与注入访问（access.ts，store 数据经 pluginStore 接线）——类型化方法面，
 * 无字符串路由中转；canvas/table/note/chat 由对应插件提供（见 canvas.ts/table.ts/note.ts/chat.ts）。
 *
 * 每窗口一个内核（懒单例，pluginStore.load 首行取用）；撕裂窗口 bootstrap 时各自创建。
 * 事件发射经 events.ts（ctx.emit 直发）；审计由 audit.ts 单独安装。
 * 用户插件的 ESM 求值需 React 全局（JSX 经 esbuild 转出 React.createElement 引用）。
 */
import React from "react";
import { Context, symbols } from "@atelyx/cordis";
import { getAppVersion } from "@/services/app";
import { detectPlatform } from "@/utils/pluginHost";
import { runProcess } from "@/services/shell";
import { pickDirectory, pickFile, saveFile } from "@/services/dialog";
import { copyImageToClipboard, readClipboardText, writeClipboardText } from "@/services/clipboard";
import { closeWindow, minimizeWindow, toggleMaximizeWindow } from "@/services/window";
import { listVaultTree } from "@/services/vault";
import { pluginKvDelete, pluginKvRead, pluginKvSet, pluginKvWrite, pluginReadState, pluginWriteState } from "@/services/plugins";
import { httpRequest } from "@/services/http";
import { registerPluginTools, unregisterPluginTools } from "@/services/ai/tools";
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
  getSettingsAccess,
  requireVaultWrite,
  type PluginNotificationAccess,
} from "./access";
import { installAudit, resetAudit } from "./audit";
import { createSlotsApi } from "./slotsApi";
import { createHistoryService } from "./history";
import { createLayoutService } from "./layout";
import { createUiStateService } from "./uiState";
import { installEventIsolation, setKernelRef } from "./events";
import type {
  AiService,
  AppService,
  ChatResult,
  ClipboardService,
  CollabService,
  DialogService,
  HttpService,
  NotificationService,
  ShellExecResult,
  ShellService,
  StateService,
  StorageService,
  VaultService,
  WindowService,
} from "./types";
import "./types";

declare global {
  interface Window {
    /** 插件 ESM 求值运行时：JSX 转出 React.createElement 引用的全局。 */
    React?: typeof React;
  }
}

/** ai 服务实例（tracker 注入调用方插件上下文：registerTool 随其 fiber 撤销）。 */
interface AiServiceInstance extends AiService {
  ctx: Context;
}

/** 流句柄（shell/ai 流式：chunk/end/error 帧；已收尾后忽略后续调用）。 */
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

  const state: StateService = {
    read: (pluginId) => pluginReadState(pluginId),
    write: (pluginId, data) => pluginWriteState(pluginId, data),
  };
  provide("state", state);

  /** 键值面按插件 id 显式寻址（与 ctx.state 同约定）：服务侧不猜调用方身份；
   *  单键读改写由 Rust 侧串行完成（并发写不丢键），clear 走整表覆盖。 */
  const storage: StorageService = {
    get: async (pluginId, key) => (await pluginKvRead(pluginId))[key],
    set: (pluginId, key, value) => pluginKvSet(pluginId, key, value),
    delete: (pluginId, key) => pluginKvDelete(pluginId, key),
    keys: async (pluginId) => Object.keys(await pluginKvRead(pluginId)),
    clear: (pluginId) => pluginKvWrite(pluginId, {}),
  };
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

  const shell: ShellService = {
    exec: (opts, handlers) => {
      if (!handlers) {
        // 非流式：聚合输出后一次性返回。
        return new Promise<ShellExecResult>((resolve, reject) => {
          let stdout = "";
          let stderr = "";
          runProcess(
            opts.command,
            opts.args ?? [],
            { cwd: opts.cwd, env: opts.env },
            {
              stdout: (line) => {
                stdout += `${line}\n`;
              },
              stderr: (line) => {
                stderr += `${line}\n`;
              },
              close: (code) => resolve({ code, stdout, stderr }),
              error: (msg) => reject(new Error(msg)),
            },
          );
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
        runProcess(
          opts.command,
          opts.args ?? [],
          { cwd: opts.cwd, env: opts.env },
          {
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
          },
        );
      });
    },
  };
  provide("shell", shell);

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
  };
  provide("clipboard", clipboard);

  const windowSvc: WindowService = {
    minimize: () => minimizeWindow(),
    toggleMaximize: () => toggleMaximizeWindow(),
    close: () => closeWindow(),
  };
  provide("window", windowSvc);

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
  };
  provide("collab", collab);

  const notification: NotificationService = {
    notify: (input) => requireNotificationAccess().notify(input),
    dismiss: (id) => requireNotificationAccess().dismiss(id),
  };
  provide("notification", notification);

  // 插件 UI 注册 API（视图槽/表格视图；经 tracker 绑定调用方插件 fiber）。
  provide("slots", createSlotsApi());

  // 领域/布局/UI 状态服务（内核提供，root 作用域；实现 = 注入访问对象，见 access.ts）。
  provide("history", createHistoryService());
  provide("layout", createLayoutService());
  provide("uiState", createUiStateService());

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
