/**
 * 内核访问注入：插件面服务的 store/service 数据源（单例注入点）。
 *
 * 内核服务（kernel/canvas/table）不 import store——pluginStore 与领域 store 经本模块把
 * 当前仓库/画布/表格/协作/ai 配置等运行时数据注入插件面服务；未接线（未进仓/未打开）时
 * getter 返回 null，服务侧据此抛「能力未就绪」。
 */
import type {
  AppUiState,
  CellValue,
  CollabPeer,
  EditorChatSession,
  LayoutOp,
  LayoutOpResult,
  PluginCanvasSnapshot,
  PluginTableSnapshot,
  RepoHistoryResult,
  WorkspaceLayout,
} from "@/types";
import type { HistoryKind, HistoryVersion } from "@/services/history";
import type { AgentConfig, ChatTargetResult, ProviderConfig } from "@/types";

/** 表格能力访问（ctx.table 的 store 数据源；pluginStore/tableStore 接线注入）。 */
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

/** 注入/复位表格能力访问（builtin.table 启停时接线；null 复位供测试）。 */
export function setPluginTableRuntimeAccess(access: PluginTableRuntimeAccess | null): void {
  tableRuntimeAccess = access;
}

/** 读取表格能力访问（ctx.table 服务消费同一数据源；未接线 = null）。 */
export function getPluginTableRuntimeAccess(): PluginTableRuntimeAccess | null {
  return tableRuntimeAccess;
}

/** 协作能力访问（ctx.collab 的 store 数据源；pluginStore 接线注入）。 */
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

/** 读取协作能力访问（ctx.collab 服务消费同一数据源；未接线 = null）。 */
export function getPluginCollabAccess(): PluginCollabAccess | null {
  return collabAccess;
}

/**
 * 画布能力访问（ctx.canvas 的 store 数据源；canvasStore 接线注入）。
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

/** 注入/复位画布能力访问（builtin.canvas 启停时接线；null 复位供测试）。 */
export function setPluginCanvasAccess(access: PluginCanvasAccess | null): void {
  canvasAccess = access;
}

/** 读取画布能力访问（ctx.canvas 服务消费同一数据源；未接线 = null）。 */
export function getPluginCanvasAccess(): PluginCanvasAccess | null {
  return canvasAccess;
}

/** AI 配置访问（ctx.ai 的配置数据源；pluginStore 接线注入）。
 *  providers 为运行时配置（含 apiKey——key 读取已由 settingsStore 完成，本层不碰 keychain）。 */
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

/** 读取 AI 配置访问函数（ctx.ai 服务消费；未接线 = null）。 */
export function getSettingsAccess(): (() => PluginAiAccess | null) | null {
  return settingsAccess;
}

/**
 * vault 写能力访问（ctx.vault 写方法的 store/service 数据源；pluginStore 接线注入）。
 * 语义与 AI 文件工具一致（原子写/扩展名分发引用维护/树刷新）；业务失败返回 `{ ok:false, summary }`
 * 不抛断，参数越界等硬错误抛错。本模块不 import store——写方法只碰此注入对象。
 */
export interface PluginVaultWriteAccess {
  /** 写任意文本文件（原子写 + 自动建父目录；失败抛错）。 */
  writeFile(file: string, content: string): Promise<{ ok: boolean; summary: string }>;
  /** 行级修改（oldText 唯一精确匹配、块间不重叠，校验通过统一替换；失败返回 { ok:false, summary }）。 */
  editFile(file: string, edits: { oldText: string; newText: string }[]): Promise<{ ok: boolean; summary: string }>;
  /** 追加到已存在文本文件（不存在/不可读拒绝，新建请用 writeFile）。 */
  appendFile(file: string, content: string): Promise<{ ok: boolean; summary: string }>;
  /** 同目录重命名（扩展名不可变更；按扩展名分发引用维护 + 树刷新）。 */
  renameFile(oldPath: string, newName: string): Promise<{ ok: boolean; summary: string; actualPath: string }>;
  /** 移动到目标目录（保持文件名；按扩展名分发引用维护 + 树刷新）。 */
  moveFile(oldPath: string, targetDir: string): Promise<{ ok: boolean; summary: string; actualPath: string }>;
  /** 删除文件（按扩展名分发；.atb 连带删私有附件目录；树刷新）。 */
  deleteFile(path: string): Promise<{ ok: boolean; summary: string }>;
  /** 删除目录（force=false 且非空时返回 needsConfirm 供调用方确认后重试）。 */
  deleteDir(dir: string, force?: boolean): Promise<{ ok: boolean; summary: string; needsConfirm?: boolean; itemCount?: number }>;
  /** 创建目录（返回实际路径）。 */
  createFolder(dir: string): Promise<{ ok: boolean; summary: string; path: string }>;
}

let vaultWriteAccess: PluginVaultWriteAccess | null = null;

/** 注入/复位 vault 写能力访问（pluginStore.load 时接线；null 复位供测试）。 */
export function setPluginVaultWriteAccess(access: PluginVaultWriteAccess | null): void {
  vaultWriteAccess = access;
}

/** 读取 vault 写能力访问（ctx.vault 服务消费；未接线 = null）。 */
export function getPluginVaultWriteAccess(): PluginVaultWriteAccess | null {
  return vaultWriteAccess;
}

/** 写方法守卫：未接线（未打开仓库）时报错，插件侧可据此降级。 */
export function requireVaultWrite(): PluginVaultWriteAccess {
  if (!vaultWriteAccess) throw new Error("仓库写能力未就绪");
  return vaultWriteAccess;
}

/** app.openPage 的中转回调（pluginStore 接线注入，绕开服务→store 层上依赖；null = 未接线）。 */
let appPageOpener: ((pageId: string) => void) | null = null;

/** 注入/复位 app.openPage 中转（pluginStore.load 时接线；null 复位供测试）。 */
export function setAppPageOpener(opener: ((pageId: string) => void) | null): void {
  appPageOpener = opener;
}

/** 读取 app.openPage 中转（ctx.app 服务消费；未接线 = null）。 */
export function getAppPageOpener(): ((pageId: string) => void) | null {
  return appPageOpener;
}

/** 笔记能力访问（ctx.note 的 store 数据源；pluginStore 接线注入）。
 *  read/write 走当前仓库上下文的编辑器链（读写全开；未打开笔记时 write 抛错）。 */
export interface PluginNoteAccess {
  currentFile(): string | null;
  open(file: string, title: string): void;
  /** 读笔记内容（未指定 file = 当前打开的笔记；失败 throw）。 */
  read(file?: string): Promise<string>;
  /** 写当前打开笔记内容（原子写 + 基线登记；失败 throw）。 */
  write(content: string): Promise<void>;
  /** 落盘当前笔记挂起输入。 */
  save(): Promise<void>;
}

let noteAccess: PluginNoteAccess | null = null;

/** 注入/复位笔记能力访问（pluginStore 接线；null 复位供测试）。 */
export function setPluginNoteAccess(access: PluginNoteAccess | null): void {
  noteAccess = access;
}

/** 读取笔记能力访问（ctx.note 服务消费；未接线 = null）。 */
export function getPluginNoteAccess(): PluginNoteAccess | null {
  return noteAccess;
}

/** AI 会话能力访问（ctx.chat 的 store 数据源；pluginStore 接线注入）。 */
export interface PluginChatAccess {
  sessions(): EditorChatSession[];
  activeSession(): EditorChatSession | null;
  isStreaming(): boolean;
  openSession(id: string): void;
  startSession(): void;
  sendMessage(content: string): Promise<void>;
  stop(): void;
  deleteSession(id: string): void;
}

let chatAccess: PluginChatAccess | null = null;

/** 注入/复位 AI 会话能力访问（pluginStore 接线；null 复位供测试）。 */
export function setPluginChatAccess(access: PluginChatAccess | null): void {
  chatAccess = access;
}

/** 读取 AI 会话能力访问（ctx.chat 服务消费；未接线 = null）。 */
export function getPluginChatAccess(): PluginChatAccess | null {
  return chatAccess;
}

/** 领域历史能力访问（ctx.history 的 store/service 数据源；pluginStore 接线注入）。
 *  list 走通用历史服务；rollback 按 kind 分派到对应文件存储链；repoHistory 读仓库聚合。 */
export interface PluginHistoryAccess {
  list(kind: HistoryKind, file: string): Promise<HistoryVersion[]>;
  rollback(kind: HistoryKind, file: string, seq: number): Promise<void>;
  repoHistory(): RepoHistoryResult | null;
}

let historyAccess: PluginHistoryAccess | null = null;

/** 注入/复位历史能力访问（pluginStore 接线；null 复位供测试）。 */
export function setPluginHistoryAccess(access: PluginHistoryAccess | null): void {
  historyAccess = access;
}

/** 读取历史能力访问（ctx.history 服务消费；未接线 = null）。 */
export function getPluginHistoryAccess(): PluginHistoryAccess | null {
  return historyAccess;
}

/** 布局能力访问（ctx.layout 的 store/service 数据源；pluginStore 接线注入）。
 *  op 限定安全子集（布局权威在 Rust layout.rs）；addView 为常用快捷。 */
export interface PluginLayoutAccess {
  activeLayoutId(): string | null;
  layouts(): WorkspaceLayout[];
  addView(panelId: string, view: string): Promise<LayoutOpResult>;
  op(op: LayoutOp): Promise<LayoutOpResult>;
}

let layoutAccess: PluginLayoutAccess | null = null;

/** 注入/复位布局能力访问（pluginStore 接线；null 复位供测试）。 */
export function setPluginLayoutAccess(access: PluginLayoutAccess | null): void {
  layoutAccess = access;
}

/** 读取布局能力访问（ctx.layout 服务消费；未接线 = null）。 */
export function getPluginLayoutAccess(): PluginLayoutAccess | null {
  return layoutAccess;
}

/** 应用级 UI 使用状态访问（ctx.uiState 的 store 数据源；pluginStore 接线注入）。只读非布局字段 + 布局镜像。 */
export interface PluginUiStateAccess {
  read(): AppUiState;
}

let uiStateAccess: PluginUiStateAccess | null = null;

/** 注入/复位 UI 状态访问（pluginStore 接线；null 复位供测试）。 */
export function setPluginUiStateAccess(access: PluginUiStateAccess | null): void {
  uiStateAccess = access;
}

/** 读取 UI 状态访问（ctx.uiState 服务消费；未接线 = null）。 */
export function getPluginUiStateAccess(): PluginUiStateAccess | null {
  return uiStateAccess;
}

