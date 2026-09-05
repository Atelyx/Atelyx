/**
 * 应用级 UI 使用状态（`app_data_dir/ui-state.json`，schema `atelyx-ui-state/v1`）。
 *
 * 工作区布局（布局列表 + 激活布局 + 聚焦面板）+ 上次打开的文件 + 文件面板展开，
 * 全部**应用级**：app_data_dir 本机独有、不随仓库同步，跨仓库共享——
 * 布局/展开/上次文件是个人使用偏好，与仓库无关。
 *
 * 与全局配置（global.json）分离：global.json 只保存低频配置（最近仓库列表 +
 * 自动更新开关），本文件保存高频「使用数据」——写入抖动不进配置，损坏只影响恢复。
 */
import type { DetachedWindow, SplitDirection, ViewKind, WorkspaceLayout } from "@/types/workspaceLayout";

/** `ui-state.json` 文件 schema 版本（Rust 侧 `layout.rs` 有同名常量，两端须保持一致）。 */
export const UI_STATE_SCHEMA = "atelyx-ui-state/v1" as const;

/** 最近打开的文件条目（应用级、跨仓库记录；主页面板按当前仓库过滤展示）。 */
export interface RecentFileEntry {
  /** 相对仓库根路径。 */
  file: string;
  kind: "canvas" | "note" | "table";
  /** 归属仓库 id（vaultId；打开时按当前仓库记录）。 */
  vaultId: string;
  /** 打开时间戳（ms）。 */
  openedAt: number;
}

/** 窗口位置尺寸（logical px；与 Rust layout.rs WindowBounds / DetachedWindow.bounds 对齐）。 */
export type LayoutBounds = { x: number; y: number; width: number; height: number };

/**
 * 布局操作（`layout_op` 命令参数；op 标签 + 字段均 camelCase，与 Rust `layout.rs::LayoutOp` 逐字段对齐）。
 * 布局模型的唯一变更入口——前端任何窗口都不直接改布局，只发命令、靠广播收敛。
 */
export type LayoutOp =
  | { op: "addView"; panelId: string; view: ViewKind }
  | { op: "setActive"; panelId: string; tabId: string }
  | { op: "closeTab"; panelId: string; tabId: string }
  | { op: "setLocked"; panelId: string; tabId: string; locked: boolean }
  | { op: "setTabView"; panelId: string; tabId: string; view: ViewKind }
  | { op: "moveTabWithin"; panelId: string; tabId: string; toIndex: number }
  | { op: "moveTabBetween"; fromPanelId: string; toPanelId: string; tabId: string; index?: number }
  | { op: "splitPanel"; panelId: string; direction: SplitDirection; position?: "before" | "after" }
  | { op: "closePanel"; panelId: string }
  | { op: "tearOff"; panelId: string; tabId: string; bounds: LayoutBounds }
  | { op: "tearOffFromDetached"; windowId: string; tabId: string; bounds: LayoutBounds }
  | { op: "dockIntoPanel"; panelId: string; tabId: string; index?: number }
  | { op: "dockIntoDetached"; windowId: string; tabId: string; index?: number }
  | { op: "detachedAddView"; windowId: string; view: ViewKind }
  | { op: "detachedSetActive"; windowId: string; tabId: string }
  | { op: "detachedCloseTab"; windowId: string; tabId: string }
  | { op: "detachedSetLocked"; windowId: string; tabId: string; locked: boolean }
  | { op: "detachedSetTabView"; windowId: string; tabId: string; view: ViewKind }
  | { op: "detachedMoveTab"; windowId: string; tabId: string; toIndex: number }
  | { op: "removeDetachedWindow"; windowId: string }
  | { op: "setLayoutSizes"; splitId: string; sizes: number[] }
  | { op: "addLayout" }
  | { op: "renameLayout"; id: string; name: string }
  | { op: "deleteLayout"; id: string }
  | { op: "activateLayout"; id: string }
  | { op: "moveLayout"; fromIndex: number; toIndex: number };

/** 布局操作返回值（仅 splitPanel/tearOff 需要；其余操作前端靠广播收敛）。 */
export interface LayoutOpResult {
  /** SplitPanel 新建面板 id。 */
  splitPanelId?: string | null;
  /** TearOff 创建的新撕裂窗口条目。 */
  detachedWindow?: DetachedWindow | null;
}

/** 非布局字段补丁（前端 JS 拥有这些字段，patch 到 Rust 合并后由 Rust 统一落盘）。 */
export interface UiStatePatch {
  fileExplorerExpanded?: string[];
  lastCanvasFile?: string | null;
  lastNoteFile?: string | null;
  lastTableFile?: string | null;
  focusedPanelId?: string | null;
  recentFiles?: RecentFileEntry[];
}

/** 拖拽转正载荷（源窗口按下超阈值转正时随 `drag_update` 的 start 字段上报；拖拽会话由 Rust 持有）。 */
export interface DragStart {
  tabId: string;
  view: ViewKind;
  /** 源窗口 label（"main" 或 panel-<id>）。 */
  sourceWindow: string;
  /** 源宿主：主窗口面板 id 或撕裂窗口 id。 */
  sourceHost: string;
}

/** drop 区类型：center = 加标签；left/right/top/bottom = 分割（主窗口面板）；tab = 标签条排序。 */
export type DropZone = "center" | "left" | "right" | "top" | "bottom" | "tab";

/** 某窗口上报的 DOM 命中（zone 语义与前端 hitTest* 一致；落点解析在 Rust）。 */
export interface DragHit {
  zone: DropZone;
  /** 主窗口面板 id（撕裂窗口命中为 null）。 */
  panelId: string | null;
  /** zone = tab 时的插入位置（其他 zone 为 null）。 */
  tabIndex: number | null;
}

/** 拖拽会话广播（Rust → 各窗口；ghost 渲染 + 各窗口命中计算驱动）。 */
export interface DragBroadcast {
  /** false = 会话结束（其余字段忽略）。 */
  active: boolean;
  tabId?: string | null;
  view?: ViewKind | null;
  screenX?: number | null;
  screenY?: number | null;
  sourceWindow?: string | null;
}

/** 应用级 UI 使用状态（`app_data_dir/ui-state.json` 磁盘格式，扁平无分桶）。 */
export interface AppUiState {
  schema: typeof UI_STATE_SCHEMA;
  /** 文件面板展开的文件夹相对路径列表（缺省 = 全部折叠；跨仓库按路径共享）。 */
  fileExplorerExpanded: string[];
  /** 上次打开的画布文件（相对仓库根路径；恢复时按当前仓库列表查找命中才打开）。 */
  lastCanvasFile?: string;
  /** 上次打开的笔记文件（相对仓库根路径）。 */
  lastNoteFile?: string;
  /** 上次打开的表格文件（相对仓库根路径）。 */
  lastTableFile?: string;
  /** 工作区布局列表（缺省 = 无条目时回退默认布局）。 */
  workspaceLayouts?: WorkspaceLayout[];
  /** 激活布局 id（缺省 = 布局列表第一个）。 */
  activeLayoutId?: string;
  /** 聚焦面板 id（画布快捷键门控；缺省 = 布局第一个面板）。 */
  focusedPanelId?: string;
  /** 撕裂出去的独立窗口（应用级、跨布局共享；缺省 = 无）。 */
  detachedWindows?: DetachedWindow[];
  /** 最近打开的文件（跨仓库记录、按 file+vaultId 去重置顶、上限截断；缺省 = 无）。 */
  recentFiles?: RecentFileEntry[];
}
