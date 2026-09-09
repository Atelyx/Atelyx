/**
 * Cordis 内核契约：类型化 ctx 服务 + typed events（Atelyx 宿主侧服务面）。
 *
 * 服务实现 = 宿主能力门面（复用桥宿主能力实现，见 kernel.ts）；本文件只定义类型契约。
 * 字符串路由（bridge.call(ns, method)）在此收敛为 ctx.<domain>.<method>() 的类型化方法；
 * 事件闭集（vault:switch/canvas:changed/table:changed/collab:changed/vault:changed）
 * 在此声明为 typed event map（@mode 标注分派模式）。
 *
 * note/chat/history/layout/uiState 为服务面预留（M3 落实现）；canvas/table 由对应
 * 第一方插件提供（停用即不可用）；其余平台服务由内核提供。
 */
import type {
  CellValue,
  CollabPeer,
  FileTreeNode,
  GlobVaultResult,
  GrepVaultResult,
  ListDirResult,
  LlmMessage,
  PluginCanvasSnapshot,
  PluginTableSnapshot,
  ReadWindowResult,
  ReasoningEffort,
} from "@/types";
import type { SlotsApi } from "./slotsApi";

/** 服务流式收尾契约：流一定以 end/error 收尾（宿主 handler 未自行收尾时内核补 end）。 */
export interface CordisStreamSink {
  chunk(data: unknown): void;
  end(data?: unknown): void;
  error(message: string): void;
  /** 是否已 end/error（已收尾后其余调用忽略）。 */
  ended: boolean;
}

/** ai.chat 请求（与桥 ai 能力同字段；供应商未指定时跟随默认模型）。 */
export interface ChatRequest {
  providerId?: string;
  model?: string;
  messages: LlmMessage[];
  reasoningEffort?: ReasoningEffort;
  temperature?: number;
  maxTokens?: number;
  maxRetries?: number;
}

/** ai.chat 聚合结果（非流式；流式经 chunk/end 推送）。 */
export interface ChatResult {
  content: string;
  reasoning: string;
  finishReason?: unknown;
}

/** ai.chat 流式回调（chunk{type:text|reasoning} → end{content,reasoning,finishReason}）。 */
export interface ChatStreamHandlers {
  chunk(data: { type: "text" | "reasoning"; text: string }): void;
  end(result: ChatResult): void;
  error(message: string): void;
}

/** shell.exec 选项（command 必填；cwd/env 可选）。 */
export interface ShellExecOptions {
  command: string;
  args?: string[];
  cwd?: string;
  env?: Record<string, string>;
}

/** shell.exec 聚合结果（非流式）。 */
export interface ShellExecResult {
  code: number | null;
  stdout: string;
  stderr: string;
}

/** shell.exec 流式回调（stdout/stderr → chunk；退出 → end{code}）。 */
export interface ShellStreamHandlers {
  chunk(data: { stream: "stdout" | "stderr"; data: string }): void;
  end(data: { code: number | null }): void;
  error(message: string): void;
}

/** 系统对话框过滤器数组（{ name, extensions }）。 */
export interface DialogFilters {
  name: string;
  extensions: string[];
}

/** 插件自持状态服务（按插件 id 读写；单 JSON 对象）。 */
export interface StateService {
  read(pluginId: string): Promise<unknown>;
  write(pluginId: string, data: unknown): Promise<void>;
}

/** 宿主信息服务。 */
export interface AppService {
  version(): Promise<string>;
  platform(): Promise<string>;
  /** 打开插件应用页面（app 类型插件入口）。 */
  openPage(pageId: string): Promise<boolean>;
}

/** 外部程序执行服务（敏感：可执行任意命令）。 */
export interface ShellService {
  /** 非流式：聚合输出后一次性返回；传 handlers 则流式（stdout/stderr → chunk）。 */
  exec(opts: ShellExecOptions, handlers?: ShellStreamHandlers): Promise<ShellExecResult | undefined>;
}

/** 仓库文件读写服务（读写全开；写方法语义与 AI 文件工具一致；失败返回 { ok:false, summary } 不抛断）。 */
export interface VaultService {
  listFiles(): Promise<FileTreeNode[]>;
  readFile(file: string): Promise<string>;
  readFileWindow(file: string, opts?: { offset?: number; limit?: number }): Promise<ReadWindowResult>;
  listDir(dir?: string): Promise<ListDirResult>;
  glob(pattern: string, opts?: { path?: string }): Promise<GlobVaultResult>;
  grep(pattern: string, opts?: { path?: string; include?: string }): Promise<GrepVaultResult>;
  writeFile(file: string, content: string): Promise<{ ok: boolean; summary: string }>;
  editFile(file: string, edits: { oldText: string; newText: string }[]): Promise<{ ok: boolean; summary: string }>;
  appendFile(file: string, content: string): Promise<{ ok: boolean; summary: string }>;
  renameFile(oldPath: string, newName: string): Promise<{ ok: boolean; summary: string; actualPath: string }>;
  moveFile(oldPath: string, targetDir: string): Promise<{ ok: boolean; summary: string; actualPath: string }>;
  deleteFile(path: string): Promise<{ ok: boolean; summary: string }>;
  deleteDir(
    dir: string,
    force?: boolean,
  ): Promise<{ ok: boolean; summary: string; needsConfirm?: boolean; itemCount?: number }>;
  createFolder(dir: string): Promise<{ ok: boolean; summary: string; path: string }>;
}

/** 系统对话框服务（用户取消返回 null）。 */
export interface DialogService {
  pickDirectory(): Promise<string | null>;
  pickFile(filters?: DialogFilters[]): Promise<string | null>;
  saveFile(opts?: { defaultPath?: string; filters?: DialogFilters[] }): Promise<string | null>;
}

/** 剪贴板服务（文本 + 图片 dataURL；敏感：可读写用户剪贴板）。 */
export interface ClipboardService {
  readText(): Promise<string>;
  writeText(text: string): Promise<void>;
  copyImage(dataUrl: string): Promise<void>;
}

/** 窗口控制服务（自定义标题栏窗口）。 */
export interface WindowService {
  minimize(): Promise<void>;
  toggleMaximize(): Promise<void>;
  close(): Promise<void>;
}

/** AI 会话服务：模型/Agent 列表 + 流式对话。 */
export interface AiService {
  /** 流式对话；传 handlers 则经 chunk/end 推送（resolve 时流已收尾），否则返回聚合结果。 */
  chat(req: ChatRequest, handlers?: ChatStreamHandlers): Promise<ChatResult | undefined>;
  listModels(): Promise<Array<{ providerId: string; providerName: string; modelId: string; label: string }>>;
  listAgents(): Promise<Array<{ id: string; name: string }>>;
}

/** 协作在线状态服务（读 peers + 上报本端 presence）。 */
export interface CollabService {
  peers(): CollabPeer[];
  setPresence(view: string | null, file: string | null): void;
}

/** 画布数据服务（当前打开的画布；写方法要求已打开可写画布）。 */
export interface CanvasService {
  snapshot(): PluginCanvasSnapshot;
  addNode(node: { type: string; position: { x: number; y: number }; data?: Record<string, unknown> }): string;
  updateNode(nodeId: string, patch: Record<string, unknown>): void;
  moveNode(nodeId: string, position: { x: number; y: number }): void;
  deleteNode(nodeId: string): void;
  addEdge(edge: {
    source: string;
    target: string;
    sourceHandle?: string;
    targetHandle?: string;
    directed?: boolean;
    linkMode?: string;
  }): string;
  deleteEdge(edgeId: string): void;
  selectNode(nodeId: string | null): void;
}

/** 表格数据服务（当前打开的表格；写操作要求已接线）。 */
export interface TableService {
  snapshot(): PluginTableSnapshot;
  updateCell(rowId: string, fieldId: string, value: CellValue | undefined): void;
  addRow(): void;
  removeRow(rowId: string): void;
  selectRow(rowId: string | null): void;
  /** 表格图片条目 → dataURL（`data:` 内嵌条目原样透传；读取失败 reject）。 */
  resolveImage(entry: string): Promise<string>;
}

/** 服务面预留（M3 落实现；类型契约先行）。 */
export interface NoteService {
  /** 预留：打开笔记内容 / 编辑器读写。 */
  readonly _reserved: true;
}
export interface ChatService {
  readonly _reserved: true;
}
export interface HistoryService {
  readonly _reserved: true;
}
export interface LayoutService {
  readonly _reserved: true;
}
export interface UiStateService {
  readonly _reserved: true;
}

/** 声明合并：@atelyx/cordis 的 Context 挂上 Atelyx 服务面与事件表。
 *  canvas/table 由对应第一方插件提供（停用即不可用），其余平台服务由内核提供（见 kernel.ts）；
 *  slots 为插件 UI 注册 API（由内核提供，见 slotsApi.ts）。 */
declare module "@atelyx/cordis" {
  interface Context {
    state: StateService;
    app: AppService;
    shell: ShellService;
    vault: VaultService;
    dialog: DialogService;
    clipboard: ClipboardService;
    window: WindowService;
    ai: AiService;
    collab: CollabService;
    canvas: CanvasService;
    table: TableService;
    note: NoteService;
    chat: ChatService;
    history: HistoryService;
    layout: LayoutService;
    uiState: UiStateService;
    slots: SlotsApi;
  }
  interface Events {
    /** 进仓/切仓完成广播（载荷 { root, id }）。 */
    "vault:switch": (payload: { root: string; id: string }) => void;
    /** 离开仓库/回启动页：清空仓库上下文（插件据此丢弃 vault 级驻留态）。 */
    "vault:clear": () => void;
    /** 当前画布变更（轻量信号：只带 file，按需再调 ctx.canvas.snapshot()）。 */
    "canvas:changed": (payload: { file: string | null }) => void;
    /** 当前表格变更（轻量信号）。 */
    "table:changed": (payload: { file: string | null }) => void;
    /** 协作在线用户变更。 */
    "collab:changed": (payload: { peers: CollabPeer[] }) => void;
    /** 仓库文件树变更。 */
    "vault:changed": () => void;
  }
}

export {};
