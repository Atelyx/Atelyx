/**
 * Cordis 内核宿主：根 Context + 平台类型化服务（typed ctx 服务）。
 *
 * 服务实现 = 宿主能力门面：平台服务（state/app/shell/vault/dialog/clipboard/window/ai/collab）
 * 经桥宿主能力（callHostCapability）复用同一实现——能力逻辑单一来源，此处只做类型化外壳
 * （字符串路由 → 类型化方法）；canvas/table 由对应第一方插件提供（见 canvas.ts/table.ts）。
 *
 * 每窗口一个内核（懒单例，pluginStore.load 首行取用）；撕裂窗口 bootstrap 时各自创建。
 * 事件桥在 createKernel 时装好（桥事件 → ctx.emit 同步转发）；审计由 audit.ts 单独安装。
 */
import { Context } from "@atelyx/cordis";
import { callHostCapability, getPluginCollabAccess } from "@/services/plugins";
import type { PluginStreamSink } from "@/services/plugins";
import { installEventBridge } from "./eventBridge";
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

/** 宿主侧调用方 id（内核服务经桥宿主能力时的 ctx.pluginId；state 按调用方插件 id 透传）。 */
const KERNEL_PLUGIN_ID = "kernel";

/** 调用流句柄工厂：把调用方回调转成桥流句柄（sink 帧直达调用方；收尾后忽略后续调用）。 */
function makeStreamSink(handlers: {
  chunk(data: unknown): void;
  end(data?: unknown): void;
  error(message: string): void;
}): PluginStreamSink {
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

/** 内核句柄：根 Context + 平台服务撤销（App 生命周期内不销毁；供测试/重载）。 */
export interface Kernel {
  ctx: Context;
  /** 撤销内核提供的全部平台服务（root ctx 本身保留）。 */
  dispose(): void;
}

/** 构造平台服务并挂到根 Context（服务对象引用注入 access 经桥 getter 惰性读取）。 */
export function createKernel(): Kernel {
  const ctx = new Context();
  const disposables: Array<() => void> = [];

  /** 在根 Context 上提供平台服务并收集撤销。 */
  function provide(name: string, value: unknown): void {
    disposables.push(ctx.provide(name as never, value as never));
  }

  const state: StateService = {
    read: (pluginId) =>
      callHostCapability("state", "read", [], { pluginId }) as Promise<unknown>,
    write: (pluginId, data) =>
      callHostCapability("state", "write", [data], { pluginId }) as Promise<void>,
  };
  provide("state", state);

  const app: AppService = {
    version: () =>
      callHostCapability("app", "version", [], { pluginId: KERNEL_PLUGIN_ID }) as Promise<string>,
    platform: () =>
      callHostCapability("app", "platform", [], { pluginId: KERNEL_PLUGIN_ID }) as Promise<string>,
    openPage: (pageId) =>
      callHostCapability("app", "openPage", [pageId], { pluginId: KERNEL_PLUGIN_ID }) as Promise<boolean>,
  };
  provide("app", app);

  const shell: ShellService = {
    exec: (opts, handlers) => {
      if (!handlers) {
        return callHostCapability("shell", "exec", [opts], {
          pluginId: KERNEL_PLUGIN_ID,
        }) as Promise<ShellExecResult>;
      }
      const sink = makeStreamSink({
        chunk: (d) => handlers.chunk(d as { stream: "stdout" | "stderr"; data: string }),
        end: (d) => handlers.end(d as { code: number | null }),
        error: (m) => handlers.error(m),
      });
      return callHostCapability("shell", "exec", [opts], {
        pluginId: KERNEL_PLUGIN_ID,
        stream: sink,
      }) as Promise<ShellExecResult | undefined>;
    },
  };
  provide("shell", shell);

  const vault: VaultService = {
    listFiles: () =>
      callHostCapability("vault", "listFiles", [], { pluginId: KERNEL_PLUGIN_ID }) as ReturnType<VaultService["listFiles"]>,
    readFile: (file) =>
      callHostCapability("vault", "readFile", [file], { pluginId: KERNEL_PLUGIN_ID }) as ReturnType<VaultService["readFile"]>,
    readFileWindow: (file, opts) =>
      callHostCapability("vault", "readFileWindow", [file, opts], { pluginId: KERNEL_PLUGIN_ID }) as ReturnType<
        VaultService["readFileWindow"]
      >,
    listDir: (dir) =>
      callHostCapability("vault", "listDir", dir === undefined ? [] : [dir], {
        pluginId: KERNEL_PLUGIN_ID,
      }) as ReturnType<VaultService["listDir"]>,
    glob: (pattern, opts) =>
      callHostCapability("vault", "glob", [pattern, opts], { pluginId: KERNEL_PLUGIN_ID }) as ReturnType<VaultService["glob"]>,
    grep: (pattern, opts) =>
      callHostCapability("vault", "grep", [pattern, opts], { pluginId: KERNEL_PLUGIN_ID }) as ReturnType<VaultService["grep"]>,
    writeFile: (file, content) =>
      callHostCapability("vault", "writeFile", [file, content], { pluginId: KERNEL_PLUGIN_ID }) as ReturnType<
        VaultService["writeFile"]
      >,
    editFile: (file, edits) =>
      callHostCapability("vault", "editFile", [file, edits], { pluginId: KERNEL_PLUGIN_ID }) as ReturnType<
        VaultService["editFile"]
      >,
    appendFile: (file, content) =>
      callHostCapability("vault", "appendFile", [file, content], { pluginId: KERNEL_PLUGIN_ID }) as ReturnType<
        VaultService["appendFile"]
      >,
    renameFile: (oldPath, newName) =>
      callHostCapability("vault", "renameFile", [oldPath, newName], { pluginId: KERNEL_PLUGIN_ID }) as ReturnType<
        VaultService["renameFile"]
      >,
    moveFile: (oldPath, targetDir) =>
      callHostCapability("vault", "moveFile", [oldPath, targetDir], { pluginId: KERNEL_PLUGIN_ID }) as ReturnType<
        VaultService["moveFile"]
      >,
    deleteFile: (path) =>
      callHostCapability("vault", "deleteFile", [path], { pluginId: KERNEL_PLUGIN_ID }) as ReturnType<
        VaultService["deleteFile"]
      >,
    deleteDir: (dir, force) =>
      callHostCapability("vault", "deleteDir", force === undefined ? [dir] : [dir, force], {
        pluginId: KERNEL_PLUGIN_ID,
      }) as ReturnType<VaultService["deleteDir"]>,
    createFolder: (dir) =>
      callHostCapability("vault", "createFolder", [dir], { pluginId: KERNEL_PLUGIN_ID }) as ReturnType<
        VaultService["createFolder"]
      >,
  };
  provide("vault", vault);

  const dialog: DialogService = {
    pickDirectory: () =>
      callHostCapability("dialog", "pickDirectory", [], { pluginId: KERNEL_PLUGIN_ID }) as Promise<string | null>,
    pickFile: (filters) =>
      callHostCapability("dialog", "pickFile", filters === undefined ? [] : [filters], {
        pluginId: KERNEL_PLUGIN_ID,
      }) as Promise<string | null>,
    saveFile: (opts) =>
      callHostCapability("dialog", "saveFile", opts === undefined ? [] : [opts], {
        pluginId: KERNEL_PLUGIN_ID,
      }) as Promise<string | null>,
  };
  provide("dialog", dialog);

  const clipboard: ClipboardService = {
    readText: () =>
      callHostCapability("clipboard", "readText", [], { pluginId: KERNEL_PLUGIN_ID }) as Promise<string>,
    writeText: (text) =>
      callHostCapability("clipboard", "writeText", [text], { pluginId: KERNEL_PLUGIN_ID }) as Promise<void>,
    copyImage: (dataUrl) =>
      callHostCapability("clipboard", "copyImage", [dataUrl], { pluginId: KERNEL_PLUGIN_ID }) as Promise<void>,
  };
  provide("clipboard", clipboard);

  const windowSvc: WindowService = {
    minimize: () =>
      callHostCapability("window", "minimize", [], { pluginId: KERNEL_PLUGIN_ID }) as Promise<void>,
    toggleMaximize: () =>
      callHostCapability("window", "toggleMaximize", [], { pluginId: KERNEL_PLUGIN_ID }) as Promise<void>,
    close: () =>
      callHostCapability("window", "close", [], { pluginId: KERNEL_PLUGIN_ID }) as Promise<void>,
  };
  provide("window", windowSvc);

  const ai: AiService = {
    chat: async (req, handlers) => {
      if (!handlers) {
        return (await callHostCapability("ai", "chat", [req], {
          pluginId: KERNEL_PLUGIN_ID,
        })) as ChatResult | undefined;
      }
      const sink = makeStreamSink({
        chunk: (d) => handlers.chunk(d as { type: "text" | "reasoning"; text: string }),
        end: (d) => handlers.end(d as ChatResult),
        error: (m) => handlers.error(m),
      });
      await callHostCapability("ai", "chat", [req], { pluginId: KERNEL_PLUGIN_ID, stream: sink });
      return undefined;
    },
    listModels: () =>
      callHostCapability("ai", "listModels", [], { pluginId: KERNEL_PLUGIN_ID }) as Promise<
        Array<{ providerId: string; providerName: string; modelId: string; label: string }>
      >,
    listAgents: () =>
      callHostCapability("ai", "listAgents", [], { pluginId: KERNEL_PLUGIN_ID }) as Promise<Array<{ id: string; name: string }>>,
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

  // 桥事件 → Cordis 事件总线同步转发（typed events 见 types.ts CordisEvents）。
  disposables.push(installEventBridge(ctx));

  return {
    ctx,
    dispose: () => {
      for (const d of disposables) d();
      disposables.length = 0;
    },
  };
}

let kernel: Kernel | null = null;

/** 内核懒单例（每窗口一个；pluginStore.load 首行取用）。测试请直接用 createKernel。 */
export function getKernel(): Kernel {
  if (!kernel) kernel = createKernel();
  return kernel;
}

/** 复位懒单例（供测试）。 */
export function resetKernel(): void {
  if (kernel) kernel.dispose();
  kernel = null;
}
