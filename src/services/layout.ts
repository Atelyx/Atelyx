/**
 * 布局迷你窗口管理器前端封装（纯 I/O，无状态）。
 *
 * 布局模型（布局列表 + 激活布局 + 撕裂窗口）的唯一权威在 Rust `layout.rs`：
 * - `layoutBootstrap`：拉取全量快照（主窗口/撕裂窗口初始化渲染用）
 * - `layoutOp`：发布局操作命令（前端不直接改布局，靠广播收敛）
 * - `uiStatePatch`：非布局字段补丁（JS 拥有这些字段，合并进模型后由 Rust 统一落盘）
 * - `layoutFlush`：立即落盘（应用退出/切页面前 flush 用）
 * - `onLayoutBroadcast`：订阅布局广播（各窗口据此渲染自身切片）
 *
 * 跨窗口拖拽（会话/命中调和/落点解析在 Rust）：源窗口只上报输入（`dragUpdate`：
 * 转正带 start、移动 start 为 null），各窗口经 `onDragSession` 广播渲染 ghost 并
 * 计算自身 DOM 命中上报 `dragHit`。
 */
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  AppUiState,
  DragBroadcast,
  DragHit,
  DragStart,
  LayoutOp,
  LayoutOpResult,
  UiStatePatch,
} from "@/types";

/** 拉取布局状态全量快照（返回归一化后的完整 AppUiState，含非布局字段）。 */
export async function layoutBootstrap(): Promise<AppUiState> {
  return invoke<AppUiState>("layout_bootstrap");
}

/** 应用一个布局操作（唯一变更入口；返回 splitPanel/tearOff 的结果，其余操作靠广播收敛）。 */
export async function layoutOp(op: LayoutOp): Promise<LayoutOpResult> {
  return invoke<LayoutOpResult>("layout_op", { op });
}

/** 非布局字段补丁（只发变更字段；字段缺失 = 不改，null = 显式清除）。 */
export async function uiStatePatch(patch: UiStatePatch): Promise<void> {
  await invoke("ui_state_patch", { patch });
}

/** 立即落盘（应用退出/切页面前 flush 用，防防抖窗口内丢状态）。 */
export async function layoutFlush(): Promise<void> {
  await invoke("layout_flush");
}

/** 订阅布局广播（布局模型变更后由 Rust 全量广播；返回取消订阅函数）。 */
export async function onLayoutBroadcast(
  handler: (state: AppUiState) => void,
): Promise<UnlistenFn> {
  return listen<AppUiState>("layout-broadcast", (e) => handler(e.payload));
}

// ---- 跨窗口拖拽 ----

/** 拖拽输入上报（源窗口统一入口）：转正时带 start，移动时 start = null。 */
export async function dragUpdate(
  screenX: number,
  screenY: number,
  start: DragStart | null,
): Promise<void> {
  await invoke("drag_update", { screenX, screenY, start });
}

/** 窗口上报自身 DOM 命中（光标经过时计算；null = 未命中（主窗口 chrome 等））。 */
export async function dragHit(window: string, hit: DragHit | null): Promise<void> {
  await invoke("drag_hit", { window, hit });
}

/** 结束拖拽（pointerup/轮询/看门狗；screenX/screenY 为 null = 用会话最后已知坐标）。 */
export async function dragEnd(
  screenX: number | null,
  screenY: number | null,
  cancelled: boolean,
): Promise<void> {
  await invoke("drag_end", { screenX, screenY, cancelled });
}

/** 订阅拖拽会话广播（ghost 渲染 + 各窗口命中计算驱动；active=false = 会话结束）。 */
export async function onDragSession(
  handler: (state: DragBroadcast) => void,
): Promise<UnlistenFn> {
  return listen<DragBroadcast>("drag-session", (e) => handler(e.payload));
}

/** 撕裂窗口关闭上报（用户关闭窗口/≡删除面板后调用；Rust 移除模型条目，不重复关 OS 窗口）。 */
export async function panelWindowClosed(windowId: string): Promise<void> {
  await invoke("panel_window_closed", { windowId });
}

/** 布局调和（主窗口启动进仓库后调用）：补建持久化撕裂窗口的 OS 窗口。 */
export async function layoutReconcile(): Promise<void> {
  await invoke("layout_reconcile");
}
