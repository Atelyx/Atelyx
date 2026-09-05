/**
 * 跨窗口事件总线（多窗口面板体系的 Tauri event 封装，纯 I/O，无状态）。
 *
 * 布局权威在 Rust（`layout.rs` 迷你窗口管理器），布局/撕裂窗口变更由 Rust 全量广播
 * `layout-broadcast`；跨窗口拖拽由 Rust 会话 + `drag-session` 广播驱动。本文件只保留
 * 前端私有协调事件：
 * - `panel-layout-op`：撕裂窗口本地操作请求（panel → main；经 uiStateStore 命令进 Rust）
 * - `open-file-changed`：当前打开文件/仓库广播（main → all；撕裂窗口镜像上下文）
 * - `request-open-file-state`：撕裂窗口启动时一次性请求当前上下文（panel → main，
 *   主窗口以 `open-file-changed` 应答——窗口 boot 可能晚于主窗口的上下文广播）
 */
import { emit, listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { DropZone, ViewKind } from "@/types";

/** 窗口位置尺寸（logical px）。 */
export interface WindowRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** 一次命中的 drop 目标（本窗口本地计算；指示器渲染用——落点解析在 Rust）。 */
export interface DropTargetInfo {
  /** 目标窗口 label（"main" 或 panel label）。 */
  window: string;
  /** 主窗口面板 id（撕裂窗口命中时为 undefined）。 */
  panelId?: string;
  zone: DropZone;
  /** zone = tab 时的插入位置（目标标签组内下标）。 */
  tabIndex?: number;
}

/** 撕裂窗口本地操作请求（布局权威在 Rust：经主窗口 uiStateStore 命令转交）。 */
export type PanelLayoutOp =
  | { op: "setActive"; tabId: string }
  | { op: "closeTab"; tabId: string }
  | { op: "setLocked"; tabId: string; locked: boolean }
  | { op: "setTabView"; tabId: string; view: ViewKind }
  | { op: "moveTab"; tabId: string; toIndex: number }
  | { op: "addView"; view: ViewKind };

export interface OpenFileChangedPayload {
  vaultId: string | null;
  vaultRoot: string | null;
  vaultName: string;
  currentCanvasFile: string | null;
  currentNoteFile: string | null;
  currentTableFile: string | null;
  currentNoteTitle: string;
  currentTableTitle: string;
}

// ---------- emit ----------

export function emitPanelLayoutOp(windowId: string, op: PanelLayoutOp): Promise<void> {
  return emit("panel-layout-op", { windowId, op });
}

export function emitOpenFileChanged(payload: OpenFileChangedPayload): Promise<void> {
  return emit("open-file-changed", payload);
}

/** 撕裂窗口启动时请求当前仓库/打开文件上下文（主窗口以 open-file-changed 应答）。 */
export function emitRequestOpenFileState(): Promise<void> {
  return emit("request-open-file-state");
}

// ---------- listen ----------

export function onPanelLayoutOp(
  handler: (windowId: string, op: PanelLayoutOp) => void,
): Promise<UnlistenFn> {
  return listen<{ windowId: string; op: PanelLayoutOp }>("panel-layout-op", (e) =>
    handler(e.payload.windowId, e.payload.op),
  );
}

export function onOpenFileChanged(handler: (payload: OpenFileChangedPayload) => void): Promise<UnlistenFn> {
  return listen<OpenFileChangedPayload>("open-file-changed", (e) => handler(e.payload));
}

export function onRequestOpenFileState(handler: () => void): Promise<UnlistenFn> {
  return listen("request-open-file-state", () => handler());
}
