/**
 * Cordis 内核契约：类型化 ctx 服务 + typed events（Atelyx 宿主侧服务面）。
 *
 * 服务实现 = 宿主侧直连 service 层与注入访问（见 kernel.ts）；本文件只定义类型契约，
 * 插件侧一律经 ctx.<domain>.<method>() 的类型化方法触达。
 * 事件闭集（vault:switch/canvas:changed/table:changed/collab:changed/vault:changed）
 * 在此声明为 typed event map（@mode 标注分派模式）。
 *
 * 平台服务（state/app/shell/vault/dialog/clipboard/window/ai/collab）与内核领域服务
 * （history/layout/uiState）由内核提供；canvas/table/note/chat 由对应插件提供（停用即不可用）。
 */
import type {
  AppUiState,
  CellValue,
  CollabPeer,
  EditorChatSession,
  FileTreeNode,
  GlobVaultResult,
  GrepVaultResult,
  LayoutOp,
  LayoutOpResult,
  ListDirResult,
  LlmMessage,
  PluginCanvasSnapshot,
  PluginTableSnapshot,
  PluginToolOptions,
  ReadWindowResult,
  ReasoningEffort,
  RepoHistoryResult,
  WorkspaceLayout,
} from "@/types";
import type { HistoryKind, HistoryVersion } from "@/services/history";
import type { SlotsApi } from "./slotsApi";

/** ai.chat 请求（供应商未指定时跟随默认模型；signal 可中止流式）。 */
export interface ChatRequest {
  providerId?: string;
  model?: string;
  messages: LlmMessage[];
  reasoningEffort?: ReasoningEffort;
  temperature?: number;
  maxTokens?: number;
  maxRetries?: number;
  /** 中止信号（abort 后流按 onDone 收敛，不重试）。 */
  signal?: AbortSignal;
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

/** AI 会话服务：模型/Agent 列表 + 流式对话 + 插件工具贡献。 */
export interface AiService {
  /** 流式对话；传 handlers 则经 chunk/end 推送（resolve 时流已收尾），否则返回聚合结果。 */
  chat(req: ChatRequest, handlers?: ChatStreamHandlers): Promise<ChatResult | undefined>;
  listModels(): Promise<Array<{ providerId: string; providerName: string; modelId: string; label: string }>>;
  listAgents(): Promise<Array<{ id: string; name: string }>>;
  /** 注册模型可调用的工具（进 Agent 名册「插件」分类，用户勾选后生效）；随插件 fiber 撤销。 */
  registerTool(opts: PluginToolOptions): () => void;
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

/** 笔记内容服务（当前打开的笔记；读写走当前仓库上下文的编辑器链）。 */
export interface NoteService {
  /** 当前打开的笔记路径（相对仓库根；null = 未打开）。 */
  currentFile(): string | null;
  /** 打开笔记（设置全局文件状态；title 为显示标题）。 */
  open(file: string, title: string): void;
  /** 读笔记内容（未指定 file = 当前打开的笔记；失败 throw）。 */
  read(file?: string): Promise<string>;
  /** 写当前打开笔记内容（原子写 + 基线登记；失败 throw）。 */
  write(content: string): Promise<void>;
  /** 落盘当前笔记挂起输入（防抖缓存全部写入）。 */
  save(): Promise<void>;
}

/** AI 会话服务（会话历史 + 发起/停止会话）。 */
export interface ChatService {
  /** 会话列表（按最近打开倒序）。 */
  sessions(): EditorChatSession[];
  /** 当前激活会话（无 = null）。 */
  activeSession(): EditorChatSession | null;
  /** 是否正在流式生成。 */
  isStreaming(): boolean;
  /** 激活指定会话。 */
  openSession(id: string): void;
  /** 切到新对话态（真正会话在首条消息发送时创建）。 */
  startSession(): void;
  /** 发送用户消息（会话流式生成；失败 throw）。 */
  sendMessage(content: string): Promise<void>;
  /** 中止当前流式生成。 */
  stop(): void;
  /** 删除会话（含侧文件；异步落盘，失败仅日志）。 */
  deleteSession(id: string): void;
}

/** 领域历史服务（笔记/画布/表格的版本历史读 + 回滚）。 */
export interface HistoryService {
  /** 列某文件的版本历史（按 seq 升序）。 */
  list(kind: HistoryKind, file: string): Promise<HistoryVersion[]>;
  /** 回滚到某版本（note 直写；canvas/table 仅当前打开文件可回滚）。 */
  rollback(kind: HistoryKind, file: string, seq: number): Promise<void>;
  /** 仓库历史聚合（按日计数 + 版本流；未加载 = null）。 */
  repoHistory(): RepoHistoryResult | null;
}

/** 布局服务（读取布局镜像 + 安全操作子集；布局权威在 Rust layout.rs）。 */
export interface LayoutService {
  /** 当前激活布局 id。 */
  activeLayoutId(): string | null;
  /** 布局列表。 */
  layouts(): WorkspaceLayout[];
  /** 向指定面板添加一个视图（经 layout_op）。 */
  addView(panelId: string, view: string): Promise<LayoutOpResult>;
  /** 发布布局操作（限定安全子集；见 KernelLayoutService 实现）。 */
  op(op: LayoutOp): Promise<LayoutOpResult>;
}

/** 应用级 UI 使用状态读服务（非布局字段；只读）。 */
export interface UiStateService {
  /** 当前 AppUiState（非布局 JS 权威字段 + 布局镜像；只读）。 */
  read(): AppUiState;
}

/** 声明合并：@atelyx/cordis 的 Context 挂上 Atelyx 服务面与事件表。
 *  canvas/table/note/chat 由对应插件提供（停用即不可用），其余平台服务由内核提供（见 kernel.ts）；
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
    /** 进仓/切仓完成广播（载荷 { root, id }）。@emit */
    "vault:switch": (payload: { root: string; id: string }) => void;
    /** 离开仓库/回启动页：清空仓库上下文（插件据此丢弃 vault 级驻留态）。@emit */
    "vault:clear": () => void;
    /** 当前画布变更（轻量信号：只带 file，按需再调 ctx.canvas.snapshot()）。@emit */
    "canvas:changed": (payload: { file: string | null }) => void;
    /** 当前表格变更（轻量信号）。@emit */
    "table:changed": (payload: { file: string | null }) => void;
    /** 协作在线用户变更。@emit */
    "collab:changed": (payload: { peers: CollabPeer[] }) => void;
    /** 仓库文件树变更。@emit */
    "vault:changed": () => void;

    // ===== 领域事件开放（全部 @emit；按需开放 serial/waterfall veto 面） =====
    /** 笔记打开/切换（file = null = 关闭当前笔记）。@emit */
    "note:opened": (payload: { file: string | null }) => void;
    /** 当前笔记内容变更（保存落盘后发出；按需再调 note 服务读内容）。@emit */
    "note:changed": (payload: { file: string | null }) => void;
    /** AI 会话开始（发起请求）。@emit */
    "chat:started": (payload: { sessionId: string }) => void;
    /** AI 会话消息（角色 + 内容；assistant 消息在流式完成后发出，非逐 token）。@emit */
    "chat:message": (payload: { sessionId: string; role: "user" | "assistant"; content: string }) => void;
    /** AI 会话结束（正常 / 中止 / 出错统一收敛）。@emit */
    "chat:finished": (payload: { sessionId: string }) => void;
  }
}

export {};
