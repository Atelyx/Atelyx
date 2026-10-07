/**
 * 跨窗口事件总线（多窗口面板体系的 Tauri event 封装，纯 I/O，无状态）：只承载前端私有协调事件。
 * 布局与拖拽的权威在 Rust（布局变更经 `layout-broadcast` 全量广播、拖拽经 `drag-session` 广播），
 * 打开文件上下文真源在 Rust（见 services/hostContext）；会话容器快照/增量/op 线协议在
 * services/chatContainerWire（宿主-镜像模型），均不在此层。
 */
import { emit, listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { DropZone, ViewKind } from "@/types";

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

// ---------- emit ----------

export function emitPanelLayoutOp(windowId: string, op: PanelLayoutOp): Promise<void> {
  return emit("panel-layout-op", { windowId, op });
}

/** 广播组合接管用户层已变更（写盘方调用）：接管表决定装配计划，其他窗口据此重载插件运行时，
 *  否则各窗口会各自跑着不同的实现来源。 */
export function emitCompositionChanged(): Promise<void> {
  return emit("composition-changed");
}

// ---------- listen ----------

export function onPanelLayoutOp(
  handler: (windowId: string, op: PanelLayoutOp) => void,
): Promise<UnlistenFn> {
  return listen<{ windowId: string; op: PanelLayoutOp }>("panel-layout-op", (e) =>
    handler(e.payload.windowId, e.payload.op),
  );
}

/** 订阅组合接管用户层变更（每个窗口各订一份，收到后重载插件运行时）。 */
export function onCompositionChanged(handler: () => void): Promise<UnlistenFn> {
  return listen("composition-changed", () => handler());
}
