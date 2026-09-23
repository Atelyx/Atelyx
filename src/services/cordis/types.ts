/**
 * Cordis 内核契约：类型化 ctx 服务 + typed events（Atelyx 宿主侧服务面）。
 *
 * 服务实现 = 宿主侧直连 service 层与注入访问（见 kernel.ts）；本文件只定义类型契约，
 * 插件侧一律经 ctx.<domain>.<method>() 的类型化方法触达。
 * 事件闭集（vault:switch/canvas:changed/table:changed/collab:changed/collab:message/vault:changed）
 * 在此声明为 typed event map（@mode 标注分派模式）。
 *
 * 平台服务（state/app/shell/vault/dialog/clipboard/window/ai/collab）与内核领域服务
 * （history/layout/uiState）由内核提供；canvas/table/note/chat 由对应插件提供（停用即不可用）。
 */
import type {
  AppUiState,
  CellValue,
  ChatAutoNameOptions,
  ChatAutoNameResult,
  ChatCompactRequest,
  ChatCompactResult,
  ChatNamingTarget,
  ChatTargetResult,
  ChatTargetSelection,
  ChatTurnRequest,
  CollabMyPeer,
  CollabPeer,
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
  ToolSchema,
  WorkspaceLayout,
} from "@/types";
import type { Context } from "@atelyx/cordis";
import type { HistoryKind, HistoryVersion } from "@/services/history";
import type { HttpRequestInput, HttpResponseResult } from "@/services/http";
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

/** serial 拦截面监听器返回值（`note:before-save` / `ai:before-request`）：veto 阻断管线，改写载荷字段传给下一监听器。 */
interface SerialHookOutput {
  /** true = 阻断管线（后续监听器不再执行；保存/请求不落地）。 */
  veto?: boolean;
}

/** note:before-save 监听器返回值：content 改写落盘内容（缺省 = 上一值原样）。 */
export interface NoteBeforeSaveOutput extends SerialHookOutput {
  content?: string;
}

/** ai:before-request 监听器返回值：messages 改写请求消息（缺省 = 上一值原样）。 */
export interface AiBeforeRequestOutput extends SerialHookOutput {
  messages?: LlmMessage[];
}

/** shell.exec 选项（command 必填；cwd/env 可选）。
 *  `env` 是**追加/覆盖**宿主环境（不传即完全继承宿主的 PATH/TEMP 等——本机服务需要它们）；
 *  没有「清空环境」的写法，stdin 也不开放（不给子进程写输入）。 */
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

/** shell.spawn 的进程句柄。 */
export interface ShellProcessHandle {
  pid: number;
  /** 结束该进程及其全部子孙（含 `sh -c`/`cmd.exe /C` 包装出的实际服务进程）。
   *  进程已退出（或已被结束）时本调用为 no-op；进程已不存在不算失败，其余失败 reject 带原因。 */
  cancel(): Promise<void>;
}

/** 系统对话框过滤器数组（{ name, extensions }）。 */
export interface DialogFilters {
  name: string;
  extensions: string[];
}

/** 插件自持状态服务（按调用方插件隔离；单 JSON 对象）。归属 id 由宿主按调用方上下文绑定，API 不暴露。 */
export interface StateService {
  read(): Promise<unknown>;
  write(data: unknown): Promise<void>;
}

/** 插件键值存储服务（按调用方插件隔离，独立于 `ctx.state`；值须为 JSON 可序列化，整表落 `data/kv.json`）。
 *  归属 id 由宿主按调用方上下文绑定，API 不暴露。 */
export interface StorageService {
  /** 读一个键（不存在 = undefined）。 */
  get(key: string): Promise<unknown>;
  /** 写一个键（整表原子写）。 */
  set(key: string, value: unknown): Promise<void>;
  /** 删一个键（不存在 = no-op）。 */
  delete(key: string): Promise<void>;
  /** 全部键名。 */
  keys(): Promise<string[]>;
  /** 清空该插件的全部键。 */
  clear(): Promise<void>;
}

/** 通用 HTTP 请求/响应类型：形状定义在 `services/http`（命令封装处），此处只做别名与注入面声明。 */
export type { HttpRequestInput as HttpRequest, HttpResponseResult as HttpResponse } from "@/services/http";

/** 通用 HTTP 请求服务（Rust 代理：CORS 绕行 + URL 协议白名单；20s 超时 + 1MB 响应上限）。
 *  地址按调用方身份分策略——本服务是插件面（可信主体），走本机/局域网策略：回环/私网/ULA 放行，
 *  云元数据/链路本地仍拒；模型工具抓取（`fetch_web`）走公网策略。IP 字面量与 DNS 解析结果逐 IP
 *  同口径校验 + 重定向每跳复检，连接只建立到已校验地址。 */
export interface HttpService {
  request(req: HttpRequestInput): Promise<HttpResponseResult>;
}

/** 通知级别（ctx.notification、access 注入与宿主通知组件共用）。 */
export type NotificationLevel = "info" | "success" | "warning" | "error";

/** 通知输入（宿主与插件共用同一形状）。 */
export interface NotificationInput {
  message: string;
  title?: string;
  level?: NotificationLevel;
}

/** 应用内通知服务（右下角通知堆叠；`level` = info/success/warning/error，自动消失）。 */
export interface NotificationService {
  /** 弹出一条通知，返回通知 id（可据此提前关闭）。level 缺省 info。 */
  notify(input: NotificationInput): string;
  /** 关闭一条通知（不存在 = no-op）。 */
  dismiss(id: string): void;
}

/** 宿主信息服务（版本 / 平台 / 打开插件应用页）。 */
export interface AppService {
  version(): Promise<string>;
  platform(): Promise<string>;
  /** 打开插件应用页面（app 类型插件入口）。 */
  openPage(pageId: string): Promise<boolean>;
}

/** 外部程序执行服务（敏感：程序只放行 `sh`（Unix，配 `-c`）/ `cmd.exe`（Windows，配 `/C`），`args` 全开，等价任意命令执行）。
 *  插件启动的进程按调用方记账：插件停用/卸载时由宿主统一结束，应用退出时也一并结束——长驻服务
 *  不该活过插件本身，更不该活过应用（被强杀时靠 Windows 作业对象兜底，Unix 该路径不保证）。 */
export interface ShellService {
  /** 非流式：聚合输出后一次性返回；传 handlers 则流式（stdout/stderr → chunk）。 */
  exec(opts: ShellExecOptions, handlers?: ShellStreamHandlers): Promise<ShellExecResult | undefined>;
  /** 启动进程并立即返回句柄（不等进程结束）——托管长驻服务的可靠停止方式。
   *  启动失败 reject（同时经 handlers.error 上报同因错误）；此后错误只走 handlers.error。
   *  进程创建那一刻即纳入退出清理范围：应用退出（含被强杀，Windows）时随宿主一起结束。 */
  spawn(opts: ShellExecOptions, handlers?: ShellStreamHandlers): Promise<ShellProcessHandle>;
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

/** 仓库外授权目录文件读写服务（外部文件服务面）：方法面与 `vault` 镜像，但入参是绝对路径，
 *  须落在该插件经用户批准的授权目录内（Rust 侧实时校验，撤销立即失效）。
 *  插件代码不传插件 id——宿主按当前 fiber 绑定（与 state/storage 同机制）。
 *  模型工具（AI 文件工具）走 `vault`，结构性够不到本面。 */
export interface FsService {
  readFile(path: string): Promise<string>;
  writeFile(path: string, content: string): Promise<{ ok: boolean; summary: string }>;
  listDir(path: string): Promise<ListDirResult>;
  createFolder(path: string): Promise<{ ok: boolean; summary: string; path: string }>;
  renameFile(path: string, newName: string): Promise<{ ok: boolean; summary: string; actualPath: string }>;
  moveFile(path: string, targetDir: string): Promise<{ ok: boolean; summary: string; actualPath: string }>;
  deleteFile(path: string): Promise<{ ok: boolean; summary: string }>;
  deleteDir(
    path: string,
    force?: boolean,
  ): Promise<{ ok: boolean; summary: string; needsConfirm?: boolean; itemCount?: number }>;
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

/** AI 会话服务：模型/Agent 列表 + 流式对话 + 插件工具贡献；`req.signal` 可中止流式（中止后按 `end` 收敛）。 */
export interface AiService {
  /** 流式对话；传 handlers 则经 chunk/end 推送（resolve 时流已收尾），否则返回聚合结果。 */
  chat(req: ChatRequest, handlers?: ChatStreamHandlers): Promise<ChatResult | undefined>;
  listModels(): Promise<Array<{ providerId: string; providerName: string; modelId: string; label: string }>>;
  listAgents(): Promise<Array<{ id: string; name: string }>>;
  /** 注册模型可调用的工具（进 Agent 名册「插件」分类，用户勾选后生效）；随插件 fiber 撤销。 */
  registerTool(opts: PluginToolOptions): () => void;
}

/** 协作服务（读 peers + 上报 presence + 插件通用消息收发）。 */
export interface CollabService {
  peers(): CollabPeer[];
  setPresence(view: string | null, file: string | null): void;
  /** 发送插件消息到同房间其他成员：payload 任意 JSON；opts.to 指定 = 定向单播只发该 peer，
   *  缺省 = 广播。返回是否已投递到传输层（未连接/断开 = false，调用方据此感知消息未发出）。 */
  sendMessage(channel: string, payload: unknown, opts?: { to?: number }): boolean;
  /** 本端身份（peerId 未连接 = null；与 peers() 对称）。 */
  myPeer(): CollabMyPeer;
}

/** 画布数据服务（由随应用分发的画布插件提供，停用即不可用；写方法要求已打开可写画布）。 */
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

/** 表格数据服务（由随应用分发的表格插件提供，停用即不可用；写操作要求已接线）。 */
export interface TableService {
  snapshot(): PluginTableSnapshot;
  updateCell(rowId: string, fieldId: string, value: CellValue | undefined): void;
  addRow(): void;
  removeRow(rowId: string): void;
  selectRow(rowId: string | null): void;
  /** 表格图片条目 → dataURL（`data:` 内嵌条目原样透传；读取失败 reject）。 */
  resolveImage(entry: string): Promise<string>;
}

/** 笔记内容服务（由随应用分发的笔记插件提供，停用即不可用；读写走当前仓库上下文的编辑器链）。
 *  写入 `.md` 为整文件写（后写者胜）：该笔记若正被编辑且有待落盘输入，其后续自动保存会把本地
 *  输入写盘，覆盖本次写入的内容。 */
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

/** AI 对话能力（由随应用分发的对话核心插件提供，停用即不可用）：用宿主配置的模型/Agent/工具跑一轮对话。
 *  核心只跑一轮——消息容器与落盘留在调用方（插件自带容器），流式与收尾经 `ChatTurnSink` 交回。
 *  类型面与宿主内部消费方同一份契约（见 types/chatRuntime.ts 的 `ChatRuntime`）。 */
export interface ChatService {
  /** 解析对话目标（未指定 = 跟随仓库默认；失败给可展示文案）。 */
  resolveTarget(selection?: ChatTargetSelection | null): ChatTargetResult;
  /** 跑一轮对话（流式 + 工具循环 + 收尾 + 命名），产出经 `req.sink` 交回。 */
  runTurn(req: ChatTurnRequest): Promise<void>;
  /** 生成压缩摘要（只出文本，写回容器由调用方负责）。 */
  compact(req: ChatCompactRequest): Promise<ChatCompactResult>;
  /** 话题命名（轮末自动命名与手动重新命名共用）。 */
  autoName(
    naming: ChatNamingTarget,
    targetId: string,
    opts?: ChatAutoNameOptions,
  ): Promise<ChatAutoNameResult>;
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

/** 布局服务（读取布局镜像 + 发布布局操作；`op` 与 Rust `LayoutOp` 逐字段对齐，改布局一律经 `layout_op`，布局权威在 Rust）。 */
export interface LayoutService {
  /** 当前激活布局 id。 */
  activeLayoutId(): string | null;
  /** 布局列表。 */
  layouts(): WorkspaceLayout[];
  /** 向指定面板添加一个视图（经 layout_op）。 */
  addView(panelId: string, view: string): Promise<LayoutOpResult>;
  /** 发布布局操作（`LayoutOp` 与 Rust `LayoutOp` 逐字段对齐，命令层全量受理；布局权威在 Rust）。 */
  op(op: LayoutOp): Promise<LayoutOpResult>;
}

/** 应用级 UI 使用状态读服务（只读非布局字段 + 布局镜像）。 */
export interface UiStateService {
  /** 当前 AppUiState（非布局 JS 权威字段 + 布局镜像；只读）。 */
  read(): AppUiState;
}

/** 服务注册表条目（ctx.services.list() 返回；提供者 = 注册该服务的插件，缺省 = 宿主内核提供）。 */
export interface ServiceInfo {
  /** 服务名（ctx.<name> 的键）。 */
  name: string;
  /** 提供者插件 id（宿主内核提供的平台服务无此字段）。 */
  provider?: string;
}

/** 服务注册表查询服务（ctx.services）：插件据此发现当前真实可用的服务面与提供者。
 *  宿主内核提供平台服务（无 provider）；插件经 ctx.root.provide 提供的服务带提供者插件 id。 */
export interface ServicesService {
  /** 当前已注册的全部服务面（含提供者插件 id）。 */
  list(): ServiceInfo[];
  /** 读取某服务（不存在/未激活 = undefined；可选依赖判空用）。 */
  get<K extends keyof Context>(name: K): Context[K] | undefined;
}

/** 原始 Rust 命令逃生舱（ctx.native.invoke）：未封装的服务能力经此触达，调用进审计。 */
export interface NativeService {
  /** 调用任意已注册 Rust 命令（敏感：等价原始命令执行；审计记录命令名与参数个数，参数原文不进审计）。 */
  invoke(command: string, args?: Record<string, unknown>): Promise<unknown>;
}

/** 声明合并：@atelyx/cordis 的 Context 挂上 Atelyx 服务面与事件表。
 *  canvas/table/note/chat 由对应插件提供（停用即不可用），其余平台服务由内核提供（见 kernel.ts）；
 *  slots 为插件 UI 注册 API（由内核提供，见 slotsApi.ts）。 */
declare module "@atelyx/cordis" {
  interface Context {
    state: StateService;
    storage: StorageService;
    http: HttpService;
    notification: NotificationService;
    app: AppService;
    shell: ShellService;
    vault: VaultService;
    fs: FsService;
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
    services: ServicesService;
    native: NativeService;
  }
  interface Events {
    /** 进仓/切仓完成广播（载荷 { root }；root null = 协作空间仓库，无本地 root）。@emit */
    "vault:switch": (payload: { root: string | null }) => void;
    /** 当前画布变更（轻量信号：只带 file，按需再调 ctx.canvas.snapshot()）。@emit */
    "canvas:changed": (payload: { file: string | null }) => void;
    /** 当前表格变更（轻量信号）。@emit */
    "table:changed": (payload: { file: string | null }) => void;
    /** 协作在线用户变更。@emit */
    "collab:changed": (payload: { peers: CollabPeer[] }) => void;
    /** 收到同房间其他成员经协作通道发来的插件消息（不含自己；payload 为发送方原样透传的 JSON）。@emit */
    "collab:message": (payload: { peerId: number; channel: string; payload: unknown }) => void;
    /** 仓库文件树变更。@emit */
    "vault:changed": () => void;

    // ===== 领域事件开放（emit 广播不可中断；serial 可顺序否决/改写，见 events.ts runSerialHook） =====
    /** 笔记保存前钩子（serial：顺序执行；返回 { veto } 阻断本次保存，返回 { content } 改写落盘内容）。@serial */
    "note:before-save": (payload: { file: string; content: string }) => NoteBeforeSaveOutput | void;
    /** AI 请求发出前钩子（serial：顺序执行；返回 { veto } 阻断本次请求，返回 { messages } 改写请求消息）。@serial */
    "ai:before-request": (payload: {
      model: string;
      messages: LlmMessage[];
      tools?: ToolSchema[];
    }) => AiBeforeRequestOutput | void;
    /** 笔记打开/切换（file = null = 关闭当前笔记）。@emit */
    "note:opened": (payload: { file: string | null }) => void;
    /** 当前笔记内容变更（保存落盘后发出；按需再调 note 服务读内容）。@emit */
    "note:changed": (payload: { file: string | null }) => void;
    /** AI 对话轮次开始（发起请求）。@emit */
    "chat:started": (payload: { targetId: string }) => void;
    /** AI 对话消息（角色 + 内容；assistant 消息在流式完成后发出，非逐 token）。@emit */
    "chat:message": (payload: { targetId: string; role: "user" | "assistant"; content: string }) => void;
    /** AI 对话轮次结束（正常 / 中止 / 出错统一收敛）。@emit */
    "chat:finished": (payload: { targetId: string }) => void;
  }
}

export {};
