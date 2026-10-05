/**
 * Cordis 内核契约：类型化 ctx 服务 + typed events（Atelyx 宿主侧服务面）。
 *
 * 服务实现 = 宿主侧直连 service 层与注入访问（见 kernel.ts）；本文件只定义类型契约，
 * 插件侧一律经 ctx.<domain>.<method>() 的类型化方法触达。
 * 事件闭集（vault:switch/canvas:changed/table:changed/collab:changed/collab:reconnected/
 * collab:resync/vault:changed）在此声明为 typed event map（@mode 标注分派模式）。
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
  ChatTurnMessage,
  ChatTurnRequest,
  CollabMyPeer,
  CollabPeer,
  ConversationCompaction,
  FileTreeNode,
  GlobVaultResult,
  GrepVaultResult,
  LayoutOp,
  LayoutOpResult,
  ListDirResult,
  LlmMessage,
  MarkdownDocument,
  PluginCanvasSnapshot,
  PluginTableSnapshot,
  PluginToolOptions,
  ReadWindowResult,
  ReasoningEffort,
  RepoHistoryResult,
  ToolSchema,
  WindowOptions,
  WorkspaceLayout,
  PluginDefaultLayoutSpec,
} from "@/types";
import type { ComponentType } from "react";
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

/** 通知上的可选动作（如「下载」）：带动作的通知不自动消失，需用户明确处理或关闭。 */
export interface NotificationAction {
  label: string;
  onClick: () => void;
}

/** 通知输入（宿主与插件共用同一形状）。 */
export interface NotificationInput {
  message: string;
  title?: string;
  level?: NotificationLevel;
  action?: NotificationAction;
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

/** 仓库外文件读写服务（外部文件服务面）：方法面与 `vault` 镜像，但入参是绝对路径、
 *  作用域为仓库外任意路径（无目录授权门槛；调用经审计按调用方插件记录方法与路径）。
 *  私有目录（`privateDir()`）为插件自有落点，随插件卸载清除、更新保留。插件代码不传
 *  插件 id——宿主按当前 fiber 绑定（与 state/storage 同机制）。
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
  /** 本插件私有文件目录的绝对路径（不存在则创建）。 */
  privateDir(): Promise<string>;
  /** 字节形态写文件（base64 进出；原子写，mime 无关——字节原样落盘）。 */
  writeFileBase64(path: string, base64Data: string): Promise<{ ok: boolean; summary: string }>;
  /** 读文件为 dataURL（mime 按扩展名推断，未知扩展名 = application/octet-stream）。 */
  readFileDataUrl(path: string): Promise<string>;
}

/** 系统对话框服务（用户取消返回 null）。 */
export interface DialogService {
  pickDirectory(): Promise<string | null>;
  pickFile(filters?: DialogFilters[]): Promise<string | null>;
  saveFile(opts?: { defaultPath?: string; filters?: DialogFilters[] }): Promise<string | null>;
}

/** 插件浮层选项（ctx.ui.showFloatingLayer）。 */
export interface FloatingLayerOptions {
  /** 浮层内容组件（无 props 契约；JSX 经宿主转译可用）。 */
  component: ComponentType;
  /** 位置：视口居中（缺省）或浮层左上角视口坐标（宿主钳制到视口内）。 */
  placement?: "center" | { x: number; y: number };
  /** 浮层宽度像素（缺省按内容自适应；最终钳制到视口内）。 */
  width?: number;
  /** 点击浮层外区域时收起（缺省 false：浮层常驻输入场景防误触丢输入；Esc 收起恒由宿主代管）。 */
  closeOnOutsideClick?: boolean;
  /** 浮层收起时回调（Esc/外点/handle.close/插件停用宿主收起均触发；至多一次）。 */
  onClose?: () => void;
}

/** 插件浮层句柄。 */
export interface FloatingLayerHandle {
  /** 收起浮层（已收起 = no-op）。 */
  close(): void;
}

/** 插件浮层承载服务（内核平台能力）：宿主代管浮层的定位、层级与 Esc/外点收起语义，
 *  与宿主弹层同一套层级策略。浮层按调用方插件记账，插件停用/卸载时自动收起。 */
export interface UiService {
  /** 展示一个浮层，返回收起句柄。重复调用叠加多层。 */
  showFloatingLayer(options: FloatingLayerOptions): FloatingLayerHandle;
}

/** 插件浮层条目（宿主渲染层消费；FloatingLayerHost 按 open 顺序叠放）。 */
export interface FloatingLayerEntry {
  id: string;
  /** 登记方插件 id（审计/诊断展示）。 */
  pluginId: string;
  component: ComponentType;
  placement: "center" | { x: number; y: number };
  width?: number;
  closeOnOutsideClick: boolean;
  onClose?: () => void;
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

/** 系统级全局快捷键服务（OS 层注册，应用不在前台也触发）。无 OS 支持的平台：注册以
 *  错误拒绝，注销/释放幂等成功——无 OS 层即不存在任何登记可清理。
 *  注册属应用级资源：归属与触发转发由宿主 Rust 侧统一登记，不随注册窗口销毁失效。 */
export interface ShortcutsService {
  /** 注册全局快捷键（同一快捷键在应用内唯一，已被其他插件占用即失败；随调用方插件
   *  停用/卸载自动注销）。accelerator 为 OS 层格式（如 "Shift+Alt+E"），非法格式报错。 */
  registerGlobal(accelerator: string, handler: () => void | Promise<void>): Promise<void>;
  /** 注册窗口切换热键：触发由 Rust 按声明的窗口选项（置顶、不进任务栏、失焦自动收起、
   *  关闭即藏——任意子集组合）直接切换承载 `view` 的撕裂窗口，不经本插件回调——主窗口
   *  驻留托盘时照常生效。占用/幂等/注销语义同 registerGlobal（unregisterGlobal 通用）。 */
  registerWindowToggle(
    accelerator: string,
    view: string,
    options: WindowOptions,
  ): Promise<void>;
  /** 注销单个全局快捷键（仅归属插件可注销；未注册 = no-op）。 */
  unregisterGlobal(accelerator: string): Promise<void>;
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
  /** 发送插件消息到同房间其他成员：payload 为任意 JSON 或二进制（Uint8Array，传输层按二进制帧直传）。
   *  channel 是本插件的逻辑频道名，宿主自动加插件命名空间（线路名 = `插件id:频道`，跨插件撞名不串台）。
   *  opts.to 指定 = 定向单播只发该 peer，缺省 = 广播。返回是否已投递到传输层（未连接/断开 = false，
   *  调用方据此感知消息未发出）。通道为尽力而为语义；断线/裁剪丢帧经 collab:reconnected /
   *  collab:resync 事件感知后自行补发。 */
  sendMessage(channel: string, payload: unknown, opts?: { to?: number }): boolean;
  /** 订阅本插件的协作频道（线路名 = `本插件id:channel`），返回退订函数（随插件 fiber 撤销）。
   *  handler 只收到已订阅频道的入站消息（其他插件频道与未订阅频道不投递，跨插件撞名不串台）；
   *  payload 为发送方原样透传的 JSON 值或二进制 Uint8Array；不含本端自己发出的消息。 */
  subscribe(channel: string, handler: (peerId: number, payload: unknown) => void): () => void;
  /** 本端身份（peerId 未连接 = null；与 peers() 对称）。 */
  myPeer(): CollabMyPeer;
  /** 声明本插件需要协作通道，返回释放函数（撤销声明；随插件 fiber 撤销调用）。
   *  存在活跃声明时宿主为本窗口维持协作连接，其余连接条件不变——插件的协作需求宿主
   *  看不到，不经此声明，承载插件面板的窗口不会建立连接，ctx.collab 收发恒不可用。 */
  acquire(): () => void;
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

/** 插件侧 Markdown 渲染选项（框架无关；链接形态判定等宿主语义不暴露给插件）。 */
export interface PluginMarkdownOptions {
  /** 是否启用 KaTeX（缺省启用；关闭时数学回显源码）。 */
  katex?: boolean;
  /** mention 胶囊候选（源文本以 `@` 或 `#` 触发）。 */
  mentions?: { key: string; label: string }[];
}

/** Markdown 渲染服务（内核平台能力，恒可用）：与编辑器同一内核，
 *  纯文本进、规格/DOM/已清洗 HTML 出，渲染结果与应用内展示一致。 */
export interface MarkdownService {
  /** 渲染为已清洗 HTML（raw HTML 经白名单清洗，可直接挂入插件 UI）。 */
  renderHtml(markdown: string, options?: PluginMarkdownOptions): string;
  /** 解析为文档规格（块 + 行内片段；偏移基于源文本，便于插件做定位）。 */
  parse(markdown: string, options?: PluginMarkdownOptions): MarkdownDocument;
  /** 渲染为文档片段（无 DOM 环境返回 null）。 */
  renderToFragment(markdown: string, options?: PluginMarkdownOptions): DocumentFragment | null;
}

/** AI 对话能力（由随应用分发的对话核心插件提供，停用即不可用）：用宿主配置的模型/Agent/工具跑一轮对话。
 *  核心只跑一轮——消息容器与落盘留在调用方（插件自带容器），流式与收尾经 `ChatTurnSink` 交回。
 *  同源容器方法（importSession/appendMessages/listSessions/openSession/createSession/setSessionTitle）
 *  读写宿主对话面板的会话（同一批会话文件，磁盘为真源；面板 store 每窗口一份内存实例，
 *  跨窗口并发以写盘广播对账，见 chatPanelStore），要求对话面板插件已启用。
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
  /** 把插件侧消息登记为宿主对话面板会话（新建并返回会话 id；面板历史可见、可在面板中继续对话）。
   *  消息经宿主校验转换（role 限 user/assistant、content 须为字符串、附件仅接受 file 引用），
   *  落盘由宿主会话链承担；opts.title 缺省按首条 user 消息派生，opts.agentId 指定会话 Agent。
   *  不改变面板当前激活会话。 */
  importSession(
    messages: ChatTurnMessage[],
    opts?: { title?: string; agentId?: string },
  ): Promise<{ id: string }>;
  /** 向既有面板会话追加插件侧消息（会话不存在即抛错；校验规则同 importSession）。 */
  appendMessages(sessionId: string, messages: ChatTurnMessage[]): Promise<void>;
  /** 面板会话清单（同源只读：id + 标题 + 最近活动时间，按最近活动降序）。 */
  listSessions(): Promise<Array<{ id: string; title?: string; updatedAt: number }>>;
  /** 打开面板会话（全部消息 + 元数据；会话不存在即抛错）。附件按 file 引用出契约
   *  （payload 是面板窗口的运行时缓存），消息中的面板特有标注（refs/错误占位标记）不出契约。 */
  openSession(sessionId: string): Promise<{
    id: string;
    title?: string;
    agentId?: string;
    compaction?: ConversationCompaction;
    messages: ChatTurnMessage[];
  }>;
  /** 新建空面板会话（返回 id；不改面板激活会话；首条消息落盘时会话文件才实际出现）。 */
  createSession(opts?: { title?: string; agentId?: string }): Promise<{ id: string }>;
  /** 写面板会话标题（会话不存在即抛错）。 */
  setSessionTitle(sessionId: string, title: string): Promise<void>;
  /** 删除面板会话（连带消息 .jsonl / 元数据侧车 / 任务清单侧车）。 */
  deleteSession(sessionId: string): Promise<void>;
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
  /** 声明插件默认布局（每插件一次性生效）：宿主把规格实例化为布局列表新条目追加（不激活、
   *  不改既有布局）；用户任一布局已含规格中的视图时不追加。声明随插件停用撤销（已追加的
   *  布局保留为普通用户布局）。布局名非法 / 规格树缺失随声明同步抛错（插件行标 failed）；
   *  规格形状非法由 Rust 侧校验拒绝，经应用通知可见、不阻断插件其余注册。 */
  declareDefaultLayout(spec: PluginDefaultLayoutSpec): () => void;
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
    shortcuts: ShortcutsService;
    ai: AiService;
    collab: CollabService;
    canvas: CanvasService;
    table: TableService;
    note: NoteService;
    markdown: MarkdownService;
    chat: ChatService;
    history: HistoryService;
    layout: LayoutService;
    uiState: UiStateService;
    ui: UiService;
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
    /** 协作连接建立（含首连进房与断线重连）：插件据此补发同步状态与 presence。@emit */
    "collab:reconnected": (payload: Record<string, never>) => void;
    /** 协作接收队列被裁剪（本端消费过慢，帧已丢）：插件据此重新对账/补发状态。@emit */
    "collab:resync": (payload: Record<string, never>) => void;
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
