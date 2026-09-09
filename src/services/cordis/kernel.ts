/**
 * Cordis 内核宿主：根 Context + 平台类型化服务（typed ctx 服务）。
 *
 * 服务实现 = 宿主侧直连：平台服务（state/app/shell/vault/dialog/clipboard/window/ai/collab）
 * 直接调用 service 层与注入访问（access.ts，store 数据经 pluginStore 接线）——类型化方法面，
 * 无字符串路由中转；canvas/table 由对应第一方插件提供（见 canvas.ts/table.ts）。
 *
 * 每窗口一个内核（懒单例，pluginStore.load 首行取用）；撕裂窗口 bootstrap 时各自创建。
 * 事件发射经 events.ts（ctx.emit 直发）；审计由 audit.ts 单独安装。
 * 第三方插件 ESM 求值需 React 全局（JSX 经 esbuild 转出 React.createElement 引用）。
 */
import { invoke } from "@tauri-apps/api/core";
import React from "react";
import { Context } from "@atelyx/cordis";
import { getAppVersion } from "@/services/app";
import { detectPlatform } from "@/utils/pluginHost";
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
import { streamChat } from "@/services/ai/client";
import {
  getAppPageOpener,
  getPluginCollabAccess,
  getSettingsAccess,
  requireVaultWrite,
} from "./access";
import { installAudit, resetAudit } from "./audit";
import { createSlotsApi } from "./slotsApi";
import { setKernelRef } from "./events";
import type {
  AiService,
  AppService,
  ChatResult,
  ClipboardService,
  CollabService,
  DialogService,
  ShellExecResult,
  ShellService,
  StateService,
  VaultService,
  WindowService,
} from "./types";
import "./types";

declare global {
  interface Window {
    /** 第三方插件 ESM 求值运行时：JSX 转出 React.createElement 引用的全局。 */
    React?: typeof React;
  }
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
  const ctx = new Context();
  const disposables: Array<() => void> = [];

  /** 在根 Context 上提供平台服务并收集撤销。 */
  function provide(name: string, value: unknown): void {
    disposables.push(ctx.provide(name as never, value as never));
  }

  const state: StateService = {
    read: (pluginId) =>
      invoke<unknown>("plugin_read_state", { id: pluginId }),
    write: (pluginId, data) =>
      invoke<void>("plugin_write_state", { id: pluginId, data }),
  };
  provide("state", state);

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
  };
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

  // 插件 UI 注册 API（视图槽/表格视图；经 tracker 绑定调用方插件 fiber）。
  provide("slots", createSlotsApi());

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

/** 第三方插件 ESM 求值运行时：JSX 经 esbuild 转出 React.createElement 引用（window.React 全局）。
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
    // 审计（服务读 + 事件订阅归属）随应用内核安装；createKernel 保持纯净（测试不装全局包装）。
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
