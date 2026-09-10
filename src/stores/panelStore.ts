/**
 * 多窗口面板运行时（每窗口一个实例；角色由窗口 label 决定）。
 *
 * **布局唯一权威在 Rust `layout.rs`**（迷你窗口管理器）：主窗口与撕裂窗口的布局
 * 都经 `layout-broadcast` 广播镜像到 `uiStateStore`。跨窗口拖拽的**会话、命中调和、
 * 落点解析、释放检测（看门狗）也全部在 Rust**——本 store 只承担：
 * - 角色/窗口身份（label 分流）；撕裂窗口启动 bootstrap + 订阅广播渲染自身切片
 * - 拖拽输入桥：源窗口 4px 候选阈值 → `drag_update`（带 start）转正；pointermove →
 *   `drag_update`（start=null）上报坐标（Rust 广播 `drag-session` 驱动各窗口 ghost +
 *   命中计算）；pointerup → `drag_end`
 * - 本窗口 DOM 命中计算与上报（`drag_hit`）+ 本地 drop 指示器（dropTarget 渲染用）
 * - 撕裂窗口 OS 生命周期（Rust 建/关窗，本 store 只负责关闭上报 `panel_window_closed`）
 * - 视图交接（releaseView：flush 落盘 + 清内存；视图离开本窗口时调用）
 * - 协作连接宿主重算（按当前布局镜像判断本窗口是否承载协作视图）
 *
 * 释放检测三层冗余（全部汇入 Rust `drag_end`，先到先得幂等）：
 * 1) 源窗口 pointerup（捕获期间窗口外事件通常可达）
 * 2) Windows 左键物理状态轮询（`is_mouse_left_down`，窗口外 pointerup 丢失的主修复）
 * 3) Rust 看门狗（光标移出所有应用窗口后长时间无输入，跨平台兜底）
 */
import { create } from "zustand";
import type { Viewport } from "@xyflow/react";
import type { LayoutNode, TabItem, ViewKind } from "@/types";
import { useUiStateStore } from "@/stores/uiStateStore";
import { useAppStore } from "@/stores/appStore";
import { useSettingsStore } from "@/stores/settingsStore";
import { usePluginStore } from "@/stores/pluginStore";
import { useCollabStore } from "@/stores/collabStore";
import { useVaultStore } from "@/stores/vaultStore";
import * as kernelLifecycle from "@/utils/kernelLifecycle";
import { collectTabs, findViewHost } from "@/utils/workspaceLayout";
import { pluginViewLabel } from "@/services/cordis/slots";
import {
  cacheCanvasViewport as cacheCanvasViewportSvc,
  emitCanvasViewportHandoff,
  getCachedCanvasViewport as getCachedCanvasViewportSvc,
  onCanvasViewportHandoff as onCanvasViewportHandoffSvc,
} from "@/services/viewHandoff";
import {
  dragEnd,
  dragHit,
  dragUpdate,
  onDragSession,
  panelWindowClosed,
} from "@/services/layout";
import * as bus from "@/services/windowBus";
import type { DropTargetInfo } from "@/services/windowBus";
import type { DragBroadcast, DragHit } from "@/types";
import {
  getCurrentOuterPosition,
  getCurrentWindowLabel,
  isMouseLeftDown,
  onCloseRequested,
  onWindowMoved,
  setWindowTitle,
} from "@/services/window";
import type { DetachedWindow } from "@/types/workspaceLayout";

/** 拖拽转正阈值（px）：按下移动超过才视为拖拽，否则为点击激活标签。 */
const DRAG_THRESHOLD_PX = 4;
/** 左键物理状态轮询间隔（ms）：Windows 拖拽活跃期间检测鼠标松开（窗口外 pointerup 丢失的主修复）。 */
const MOUSE_POLL_INTERVAL_MS = 120;
/** 拖拽上报节流（ms）：pointermove 高频，按 ~25fps 上报（Rust 广播回程驱动命中/ghost）。 */
const DRAG_REPORT_INTERVAL_MS = 40;
/** 面板窗口 label 前缀（与 Rust PANEL_LABEL_PREFIX 对齐）。 */
export const PANEL_LABEL_PREFIX = "panel-";

/** 本窗口 label：主窗口 "main"，撕裂窗口 "panel-<id>"（与 Rust 权威 bounds/命中注册表键一致）。 */
function windowLabelOf(windowId: string): string {
  return windowId === "main" ? "main" : `${PANEL_LABEL_PREFIX}${windowId}`;
}

/** 面板窗口关闭守卫已注册标志（installPanelCloseGuard 幂等，防 React StrictMode 双挂载重复订阅）。 */
let closeGuardInstalled = false;
/** initMain 幂等守卫（防 React StrictMode 双挂载重复订阅 uiState/总线/拖拽广播）。 */
let mainInitialized = false;
/** initPanel 幂等守卫（防 React StrictMode 双挂载重复 bootstrap/订阅广播/总线）。 */
let panelInitialized = false;

/**
 * 画布视口跨窗口交接薄封装（组件 → store → service 分层：CanvasView 不直连 services，
 * 只经本 store 转发；签名与 services/viewHandoff 一致）。
 */
export function cacheCanvasViewport(file: string, vp: Viewport): void {
  cacheCanvasViewportSvc(file, vp);
}

/** 取本窗口缓存的画布视口（画布挂载恢复用；无缓存返回 null）。 */
export function getCachedCanvasViewport(file: string): Viewport | null {
  return getCachedCanvasViewportSvc(file);
}

/** 订阅「本画布视口交接到达」（目标窗口画布挂载后到达时补恢复；返回取消订阅函数）。 */
export function onCanvasViewportHandoff(
  fn: (file: string, vp: Viewport) => void,
): () => void {
  return onCanvasViewportHandoffSvc(fn);
}

/** 按下但未转正的候选（点击激活 vs 拖拽判定）。 */
interface DragCandidate {
  tab: TabItem;
  sourceHost: string;
  x: number;
  y: number;
}

/** 本窗口布局镜像（来自 uiStateStore = Rust 广播）。 */
interface LayoutMirror {
  activeTree: LayoutNode;
  detachedWindows: DetachedWindow[];
}

interface PanelStore {
  role: "main" | "panel";
  /** 本窗口 label（main 为 "main"；撕裂窗口为 panel-<id>）。 */
  windowId: string;
  /** 撕裂窗口镜像标签组（panel 用；来自 uiStateStore 自身切片）。 */
  panelTabs: TabItem[];
  panelActiveTabId: string | null;
  /** panel 是否已完成 bootstrap（渲染 gate）。 */
  panelReady: boolean;
  /** 按下候选（未转正）。 */
  dragCandidate: DragCandidate | null;
  /** 本窗口发起的拖拽是否已转正（Rust 会话活跃；标签 pointer 处理器据此分流）。 */
  dragActive: boolean;
  /** 本窗口当前 drop 命中（渲染指示器；本地计算）。 */
  dropTarget: DropTargetInfo | null;
  /** 活跃拖拽会话（Rust 广播；ghost 渲染用）。 */
  dragSession: DragBroadcast | null;
  /** 本窗口屏幕位置缓存（屏幕坐标 ↔ 本地坐标换算；ghost/命中计算用）。 */
  windowPos: { x: number; y: number };
  /** 布局镜像（来自 uiStateStore = Rust 广播）。 */
  layoutMirror: LayoutMirror | null;

  /** 主窗口初始化：缓存窗口位置 + 订阅 uiState/拖拽广播 + 注册事件监听。 */
  initMain: () => Promise<void>;
  /** 撕裂窗口初始化：bootstrap + 订阅广播 + 事件监听。 */
  initPanel: () => Promise<void>;

  /** 拖拽源：记录按下候选（锁定标签不进入）。 */
  beginDragCandidate: (tab: TabItem, sourceHost: string, x: number, y: number) => void;
  /** 拖拽候选移动：超阈值转正为 Rust 会话，返回是否已转正。 */
  moveDragCandidate: (clientX: number, clientY: number) => boolean;
  /** 拖拽中移动：上报屏幕坐标给 Rust（命中由 drag-session 广播回程驱动）。 */
  updateDrag: (clientX: number, clientY: number) => void;
  /** 拖拽结束公共收尾（finishDrag/cancelDrag 合流）：停轮询 + 清节流 + 上报 Rust。 */
  endDrag: (screenX: number | null, screenY: number | null, cancelled: boolean) => void;
  /** 拖拽结束（pointerup）：上报 Rust 解析落点。 */
  finishDrag: (clientX: number, clientY: number, cancelled: boolean) => void;
  /** 取消拖拽（pointercancel）。 */
  cancelDrag: () => void;

  /** 撕裂窗口本地操作（乐观镜像 + 请求 Rust：经 uiStateStore 命令）。 */
  panelSetActive: (tabId: string) => void;
  panelCloseTab: (tabId: string) => void;
  panelSetLocked: (tabId: string, locked: boolean) => void;
  panelSetTabView: (tabId: string, view: ViewKind) => void;
  panelMoveTab: (tabId: string, toIndex: number) => void;
  panelAddView: (view: ViewKind) => void;

  /** 释放本窗口托管的视图（flush 落盘 + 清内存；视图离开本窗口时调用）。 */
  releaseView: (view: ViewKind) => Promise<void>;
  /** 协作连接宿主重算：本窗口托管画布/表格/笔记任一视图且协作开启 → 连接，否则断开。 */
  syncCollabHost: () => void;
  /** 撕裂窗口关闭上报（用户关闭窗口/≡删除面板；Rust 移除模型条目）。 */
  notifyPanelClosed: () => Promise<void>;
  /** 撕裂窗口关闭守卫注册：关窗前 flush 全部托管视图 + 上报关闭（幂等，防重复订阅）。 */
  installPanelCloseGuard: () => Promise<void>;
}

/** 从 uiStateStore 提取布局镜像（激活布局树 + 撕裂窗口列表）。 */
function mirrorFromUiState(): LayoutMirror {
  const ui = useUiStateStore.getState();
  const active = ui.workspaceLayouts.find((l) => l.id === ui.activeLayoutId) ?? ui.workspaceLayouts[0];
  return { activeTree: active?.tree ?? { kind: "panel", id: "empty", tabs: [], activeTabId: null }, detachedWindows: ui.detachedWindows };
}

/**
 * 布局镜像订阅：仅布局字段（workspaceLayouts/activeLayoutId/detachedWindows）变化时触发。
 * 非布局字段（recentFiles/展开/上次文件）高频变更不触发整树遍历与视图交接重算。
 */
function subscribeLayoutMirror(fn: () => void): () => void {
  return useUiStateStore.subscribe((s, prev) => {
    if (
      s.workspaceLayouts !== prev.workspaceLayouts ||
      s.activeLayoutId !== prev.activeLayoutId ||
      s.detachedWindows !== prev.detachedWindows
    ) {
      fn();
    }
  });
}

/** 两次命中是否相同（zone/panelId/tabIndex 全等；命中未变化时不重复 set/上报）。 */
function sameDropTarget(a: DropTargetInfo | null, b: DropTargetInfo | null): boolean {
  if (a === b) return true;
  if (!a || !b) return false;
  return a.zone === b.zone && a.panelId === b.panelId && a.tabIndex === b.tabIndex;
}

/** 主窗口 drop 命中：按面板 DOM rect 计算 zone（头部 = tab 排序；四边缘 = 分割；中部 = 加标签）。 */
function hitTestMainWindow(cx: number, cy: number, windowId: string): DropTargetInfo | null {
  const panels = Array.from(document.querySelectorAll<HTMLElement>("[data-drop-panel]"));
  for (const el of panels) {
    const r = el.getBoundingClientRect();
    if (cx < r.left || cx > r.right || cy < r.top || cy > r.bottom) continue;
    // 头部条（tab 排序区）：面板 header 顶部 28px
    if (cy <= r.top + 28) {
      const tabEls = Array.from(el.querySelectorAll<HTMLElement>("[data-tab-id]"));
      return { window: windowId, panelId: el.dataset.dropPanel, zone: "tab", tabIndex: tabIndexAt(tabEls, cx) };
    }
    const w = r.width;
    const h = r.height;
    const zone: DropTargetInfo["zone"] =
      cx - r.left < w * 0.12
        ? "left"
        : r.right - cx < w * 0.12
          ? "right"
          : cy - r.top < h * 0.12
            ? "top"
            : r.bottom - cy < h * 0.12
              ? "bottom"
              : "center";
    return { window: windowId, panelId: el.dataset.dropPanel, zone };
  }
  return null;
}

/** 撕裂窗口 drop 命中：整体 = 加标签；头部条 = tab 排序。 */
function hitTestPanelWindow(cx: number, cy: number, windowId: string): DropTargetInfo | null {
  const root = document.querySelector<HTMLElement>("[data-panel-drop-root]");
  if (!root) return null;
  const r = root.getBoundingClientRect();
  if (cx < r.left || cx > r.right || cy < r.top || cy > r.bottom) return null;
  const tabbar = document.querySelector<HTMLElement>("[data-panel-tabbar]");
  if (tabbar) {
    const tr = tabbar.getBoundingClientRect();
    if (cy >= tr.top && cy <= tr.bottom) {
      const tabEls = Array.from(tabbar.querySelectorAll<HTMLElement>("[data-tab-id]"));
      return { window: windowId, zone: "tab", tabIndex: tabIndexAt(tabEls, cx) };
    }
  }
  return { window: windowId, zone: "center" };
}

/** 标签条插入位：光标在标签前半 → 该标签下标，否则末尾（主/撕裂窗口命中测试共用）。 */
function tabIndexAt(tabEls: HTMLElement[], cx: number): number {
  for (let i = 0; i < tabEls.length; i++) {
    const t = tabEls[i].getBoundingClientRect();
    if (cx < t.left + t.width / 2) return i;
  }
  return tabEls.length;
}

/** 源面板尺寸（logical px；撕裂新窗默认取此值）。主窗口 = 面板 DOM rect，
 * 撕裂窗口 = 整窗区域（含标题栏）rect；查不到（转正瞬间 DOM 缺失）返回 0 走 Rust 回退。 */
function sourcePanelSize(sourceHost: string, role: "main" | "panel"): { width: number; height: number } {
  const el =
    role === "main"
      ? document.querySelector(`[data-drop-panel="${sourceHost}"]`)
      : document.querySelector("[data-panel-drop-root]");
  if (!el) return { width: 0, height: 0 };
  const r = el.getBoundingClientRect();
  return { width: Math.round(r.width), height: Math.round(r.height) };
}

/** 面板窗口标签标题（窗口标题 = 激活标签视图名；插件视图走 pluginViewLabel 兜底；
 * 撕裂窗口标题复用（PanelWindowRoot 等）。 */
export function titleOfTabs(tabs: TabItem[], activeTabId: string | null): string {
  const active = tabs.find((t) => t.id === activeTabId) ?? tabs[0];
  return active ? pluginViewLabel(active.view) : "面板";
}

/** 当前仓库/打开文件上下文广播载荷（主窗口发出，撕裂窗口镜像）。 */
function currentOpenFilePayload(): bus.OpenFileChangedPayload {
  const s = useAppStore.getState();
  return {
    vaultId: s.vaultId,
    vaultRoot: s.vaultRoot,
    vaultName: s.vaultName,
    currentCanvasFile: s.currentCanvasFile,
    currentNoteFile: s.currentNoteFile,
    currentTableFile: s.currentTableFile,
    currentNoteTitle: s.currentNoteTitle,
    currentTableTitle: s.currentTableTitle,
  };
}

export const usePanelStore = create<PanelStore>((set, get) => {
  /** 左键状态轮询 timer（源窗口；Windows 释放事件丢失的主修复）。 */
  let mousePollTimer: number | null = null;
  /** 平台是否支持左键状态轮询（null = 未探测；Windows 支持，其他平台不可用）。 */
  let mousePollSupported: boolean | null = null;
  /** 拖拽上报节流 timer。 */
  let reportTimer: number | null = null;
  /** 待上报的屏幕坐标（节流合并）。 */
  let pendingMove: { x: number; y: number } | null = null;
  /** ESC 取消监听器（拖拽活跃期间注册，endDrag/会话结束移除）。 */
  let onEscapeKey: ((e: KeyboardEvent) => void) | null = null;

  /** 停止左键状态轮询。 */
  const stopMousePoll = (): void => {
    if (mousePollTimer !== null) {
      window.clearInterval(mousePollTimer);
      mousePollTimer = null;
    }
  };

  /** 启动左键状态轮询：拖拽活跃期间检测鼠标物理松开（窗口外 pointerup 丢失的根因修复）。 */
  const startMousePoll = (): void => {
    stopMousePoll();
    void (async () => {
      if (mousePollSupported === null) {
        mousePollSupported = (await isMouseLeftDown()) !== null;
      }
      if (!mousePollSupported) return;
      mousePollTimer = window.setInterval(() => {
        void isMouseLeftDown().then((down) => {
          if (down !== false) return;
          // 汇入公共收尾：停轮询 + 清节流/候选 + ESC 监听 + 上报 Rust
          get().endDrag(null, null, false);
        });
      }, MOUSE_POLL_INTERVAL_MS);
    })();
  };

  /** 跟随窗口移动刷新位置缓存（失败静默：仅影响屏幕↔本地坐标换算精度）。 */
  const followWindowMoves = (): void => {
    void onWindowMoved(() => {
      void getCurrentOuterPosition()
        .then((pos) => set({ windowPos: pos }))
        .catch(() => {});
    });
  };

  /** 停止 ESC 取消监听（与 stopMousePoll 同生命周期）。 */
  const stopEscapeCancel = (): void => {
    if (onEscapeKey) {
      window.removeEventListener("keydown", onEscapeKey);
      onEscapeKey = null;
    }
  };

  /** 注册 ESC 取消：拖拽活跃期间按 Escape 显式取消（释放检测之外的取消路径）。 */
  const startEscapeCancel = (): void => {
    stopEscapeCancel();
    onEscapeKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      // 仅源窗口取消（本窗口 dragActive 才持有会话）
      if (!get().dragActive) return;
      get().cancelDrag();
    };
    window.addEventListener("keydown", onEscapeKey);
  };

  /** 光标是否在本窗口内（窗口位置缓存 + 自身尺寸判断；决定是否计算/上报命中）。 */
  const cursorInWindow = (screenX: number, screenY: number): boolean => {
    const { windowPos } = get();
    const cx = screenX - windowPos.x;
    const cy = screenY - windowPos.y;
    return cx >= 0 && cx <= window.innerWidth && cy >= 0 && cy <= window.innerHeight;
  };

  /** 本窗口命中计算 + 上报 Rust + 本地指示器（由 drag-session 广播驱动，每窗口一份）。 */
  const computeOwnHit = (screenX: number, screenY: number): void => {
    const { role, windowId, windowPos } = get();
    if (!cursorInWindow(screenX, screenY)) {
      // 光标不在本窗口：清本地指示器（不重复上报 null——Rust 以权威 bounds 判窗口）
      if (get().dropTarget) set({ dropTarget: null });
      return;
    }
    const cx = screenX - windowPos.x;
    const cy = screenY - windowPos.y;
    const hit = role === "main" ? hitTestMainWindow(cx, cy, windowId) : hitTestPanelWindow(cx, cy, windowId);
    // 与当前 dropTarget 比较：命中未变化不重复 set/上报（Rust 只存每窗口最近命中，
    // 不依赖周期性上报；确认结束时若命中与上次相同，Rust 已有该值无需重复）
    if (sameDropTarget(get().dropTarget, hit)) return;
    set({ dropTarget: hit });
    // 上报键 = 窗口 label（与 Rust window_under_cursor/drag_hits 键一致）
    const reportKey = windowLabelOf(windowId);
    if (hit) {
      const payload: DragHit = {
        zone: hit.zone,
        panelId: hit.panelId ?? null,
        tabIndex: hit.tabIndex ?? null,
      };
      void dragHit(reportKey, payload).catch((e) => console.error("上报拖拽命中失败", e));
    } else {
      void dragHit(reportKey, null).catch((e) => console.error("上报拖拽命中失败", e));
    }
  };

  /** 拖拽会话广播处理器（ghost 数据 + 本窗口命中计算；active=false 清全套拖拽视觉态）。 */
  const onDragSessionState = (s: DragBroadcast): void => {
    if (!s.active) {
      set({ dragSession: null, dropTarget: null, dragActive: false });
      stopMousePoll();
      stopEscapeCancel();
      return;
    }
    set({ dragSession: s });
    if (typeof s.screenX === "number" && typeof s.screenY === "number") {
      computeOwnHit(s.screenX, s.screenY);
    }
  };

  /** 订阅拖拽会话广播（每窗口一次）。 */
  const subscribeDragSession = (): void => {
    void onDragSession(onDragSessionState).catch((e) => console.error("订阅拖拽会话广播失败", e));
  };

  return {
    role: "main",
    windowId: "main",
    panelTabs: [],
    panelActiveTabId: null,
    panelReady: false,
    dragCandidate: null,
    dragActive: false,
    dropTarget: null,
    dragSession: null,
    windowPos: { x: 0, y: 0 },
    layoutMirror: null,

    initMain: async () => {
      if (mainInitialized) return;
      mainInitialized = true;
      set({ role: "main", windowId: "main" });
      // 窗口位置缓存（ghost/命中坐标换算；bounds 权威在 Rust，此处仅本地换算用）
      try {
        const pos = await getCurrentOuterPosition();
        set({ windowPos: pos });
      } catch {
        /* 位置读取失败仅影响 ghost/命中换算，降级为 0,0 */
      }
      followWindowMoves();

      // 布局镜像跟随 uiStateStore（Rust 广播收敛）：视图离开本窗口 → releaseView
      // （撕裂出去后 flush 落盘 + 清内存 + 画布视口交接）；aichat 回归主窗口重读盘；协作宿主重算
      const syncFromUi = (): void => {
        const mirror = mirrorFromUiState();
        const prev = get().layoutMirror;
        set({ layoutMirror: mirror });
        if (prev) {
          const before = new Set(collectTabs(prev.activeTree).map((t) => t.view));
          const after = new Set(collectTabs(mirror.activeTree).map((t) => t.view));
          // 视图进入本窗口（撕裂回归，如 aichat 重读盘）→ 生命周期分发
          for (const v of after) {
            if (!before.has(v)) kernelLifecycle.notifyViewGained(v);
          }
          // 视图离开本窗口（撕裂出去）→ 释放（flush 落盘 + 清内存 + 画布视口交接）
          for (const v of before) {
            if (!after.has(v)) void get().releaseView(v);
          }
        }
        get().syncCollabHost();
      };
      subscribeLayoutMirror(syncFromUi);
      syncFromUi();
      subscribeDragSession();

      // 当前打开文件 + 仓库信息广播（撕裂窗口镜像文件状态/切仓库换上下文用）
      useAppStore.subscribe((s, prev) => {
        if (
          s.vaultId !== prev.vaultId ||
          s.currentCanvasFile !== prev.currentCanvasFile ||
          s.currentNoteFile !== prev.currentNoteFile ||
          s.currentTableFile !== prev.currentTableFile ||
          s.currentNoteTitle !== prev.currentNoteTitle ||
          s.currentTableTitle !== prev.currentTableTitle
        ) {
          void bus.emitOpenFileChanged(currentOpenFilePayload());
        }
      });
      // 撕裂窗口启动时请求上下文（窗口 boot 可能晚于上述广播）→ 以当前状态应答
      void bus.onRequestOpenFileState(() => {
        void bus.emitOpenFileChanged(currentOpenFilePayload());
      });

      // 撕裂窗口本地操作请求（布局权威在 Rust：经 uiStateStore 命令）
      void bus.onPanelLayoutOp((windowId, op) => {
        const ui = useUiStateStore.getState();
        switch (op.op) {
          case "setActive":
            ui.detachedSetActive(windowId, op.tabId);
            break;
          case "closeTab":
            ui.detachedCloseTab(windowId, op.tabId);
            break;
          case "setLocked":
            ui.detachedSetLocked(windowId, op.tabId, op.locked);
            break;
          case "setTabView":
            ui.detachedSetTabView(windowId, op.tabId, op.view);
            break;
          case "moveTab":
            ui.detachedMoveTab(windowId, op.tabId, op.toIndex);
            break;
          case "addView":
            ui.detachedAddView(windowId, op.view);
            break;
        }
      });

      // 设置变化 → 协作宿主重算
      useSettingsStore.subscribe((s, prev) => {
        if (
          s.collabEnabled !== prev.collabEnabled ||
          s.collabRelayUrl !== prev.collabRelayUrl ||
          s.collabNickname !== prev.collabNickname ||
          s.collabColor !== prev.collabColor ||
          s.deviceName !== prev.deviceName
        ) {
          get().syncCollabHost();
        }
      });
      // 仓库切换（vaultId 变化）→ 协作重算
      useAppStore.subscribe((s, prev) => {
        if (s.vaultId !== prev.vaultId) get().syncCollabHost();
      });
    },

    initPanel: async () => {
      if (panelInitialized) return;
      panelInitialized = true;
      const label = getCurrentWindowLabel();
      const windowId = label.startsWith(PANEL_LABEL_PREFIX) ? label.slice(PANEL_LABEL_PREFIX.length) : label;
      set({ role: "panel", windowId });
      try {
        const pos = await getCurrentOuterPosition();
        set({ windowPos: pos });
      } catch {
        /* 忽略 */
      }
      followWindowMoves();

      // 布局镜像 bootstrap + 订阅广播（uiStateStore 持有 Rust 广播来的权威布局）
      await useUiStateStore.getState().load();
      const syncFromUi = (): void => {
        const ui = useUiStateStore.getState();
        const entry = ui.detachedWindows.find((w) => w.id === windowId) ?? null;
        set({ layoutMirror: mirrorFromUiState() });
        const prevTabs = get().panelTabs;
        const nextTabs = entry ? entry.tabs : [];
        // 视图离开本窗口 → releaseView（落盘 + 清内存）；进入 → 由视图组件挂载加载
        const before = new Set(prevTabs.map((t) => t.view));
        const after = new Set(nextTabs.map((t) => t.view));
        for (const v of before) {
          if (!after.has(v)) void get().releaseView(v);
        }
        set({ panelTabs: nextTabs, panelActiveTabId: entry ? entry.activeTabId : null, panelReady: true });
        get().syncCollabHost();
      };
      subscribeLayoutMirror(syncFromUi);
      syncFromUi();
      subscribeDragSession();

      // 窗口标题 = 激活标签（Rust 建窗用占位标题，boot 后按视图名刷新）
      const entry0 = useUiStateStore.getState().detachedWindows.find((w) => w.id === windowId);
      if (entry0) void setWindowTitle(titleOfTabs(entry0.tabs, entry0.activeTabId));
      // 请求当前仓库/打开文件上下文（boot 可能晚于主窗口的上下文广播）
      void bus.emitRequestOpenFileState();

      void bus.onOpenFileChanged((payload) => {
        const app = useAppStore.getState();
        useAppStore.setState({
          vaultId: payload.vaultId,
          vaultRoot: payload.vaultRoot,
          vaultName: payload.vaultName,
          currentCanvasFile: payload.currentCanvasFile,
          currentNoteFile: payload.currentNoteFile,
          currentTableFile: payload.currentTableFile,
          currentNoteTitle: payload.currentNoteTitle,
          currentTableTitle: payload.currentTableTitle,
        });
        // 仓库上下文到达（切仓库或启动请求应答）：按需加载仓库级配置/文件树/领域仓库上下文
        if (payload.vaultId !== app.vaultId) {
          if (payload.vaultId) {
            void useSettingsStore.getState().loadVaultConfig();
            void useVaultStore.getState().loadFiles();
          }
          // AI 会话换仓库读盘（含 vaultId 置空 = 回启动页场景）经生命周期注册表分发
          void kernelLifecycle
            .notifyVaultEntered({ vaultId: payload.vaultId })
            .catch((e) => console.error("撕裂窗口加载领域仓库上下文失败", e));
          // 撕裂窗口插件运行时随仓库上下文重载（与主窗口 selectVault/backToVaultSelect 时机一致）：
          // vaultId 置空（回启动页）也 load——此时只扫 app 插件，自然卸载 vault 插件；
          // 插件事件（vault:switch/clear）按窗口隔离不跨窗口转发，撕裂窗口插件经重载兜底
          void usePluginStore.getState().load().catch((e) => console.error("撕裂窗口加载插件失败", e));
          // 协作宿主重算（仓库房间变化）
          get().syncCollabHost();
        }
      });
    },

    beginDragCandidate: (tab, sourceHost, x, y) => {
      if (tab.locked) return;
      if (get().dragCandidate) return;
      set({ dragCandidate: { tab, sourceHost, x, y } });
    },

    moveDragCandidate: (clientX, clientY) => {
      const c = get().dragCandidate;
      if (!c) return false;
      if (Math.hypot(clientX - c.x, clientY - c.y) < DRAG_THRESHOLD_PX) return false;
      // 转正：随首帧上报 Rust 开始拖拽会话（OS 鼠标按下隐式捕获保证窗口外仍收事件）
      const { windowPos, windowId, role } = get();
      const screenX = clientX + windowPos.x;
      const screenY = clientY + windowPos.y;
      set({ dragCandidate: null, dragActive: true });
      const size = sourcePanelSize(c.sourceHost, role);
      void dragUpdate(screenX, screenY, {
        tabId: c.tab.id,
        view: c.tab.view,
        // sourceWindow = 窗口 label（Rust 依此区分主窗口/撕裂窗口来源 + 看门狗/落点解析）
        sourceWindow: windowLabelOf(windowId),
        sourceHost: c.sourceHost,
        // 源面板尺寸：撕裂新窗默认取此值（0 = DOM 缺失，Rust 回退固定默认）
        sourceWidth: size.width,
        sourceHeight: size.height,
      }).catch((e) => {
        console.error("开始拖拽会话失败", e);
        // 转正失败回滚本地拖拽态（会话未建立，避免停在 dragActive 假态继续空上报）
        stopMousePoll();
        stopEscapeCancel();
        set({ dragActive: false, dragCandidate: null });
      });
      // Windows 左键轮询：窗口外 pointerup 丢失的主修复；ESC 显式取消路径
      startMousePoll();
      startEscapeCancel();
      return true;
    },

    updateDrag: (clientX, clientY) => {
      if (!get().dragActive) return;
      const { windowPos } = get();
      pendingMove = { x: clientX + windowPos.x, y: clientY + windowPos.y };
      if (reportTimer !== null) return;
      reportTimer = window.setTimeout(() => {
        reportTimer = null;
        const m = pendingMove;
        pendingMove = null;
        if (m) void dragUpdate(m.x, m.y, null).catch((e) => console.error("上报拖拽移动失败", e));
      }, DRAG_REPORT_INTERVAL_MS);
    },

    /** 拖拽结束公共收尾：停轮询 + 清节流 + 清候选 + 上报 Rust（screen 为 null = 用会话最后坐标/取消）。 */
    endDrag: (screenX, screenY, cancelled) => {
      stopMousePoll();
      stopEscapeCancel();
      set({ dragActive: false, dragCandidate: null });
      if (reportTimer !== null) {
        window.clearTimeout(reportTimer);
        reportTimer = null;
        pendingMove = null;
      }
      void dragEnd(screenX, screenY, cancelled).catch((e) => console.error("结束拖拽会话失败", e));
    },

    finishDrag: (clientX, clientY, cancelled) => {
      const { windowPos } = get();
      get().endDrag(cancelled ? null : clientX + windowPos.x, cancelled ? null : clientY + windowPos.y, cancelled);
    },

    cancelDrag: () => {
      get().endDrag(null, null, true);
    },

    panelSetActive: (tabId) => {
      const windowId = get().windowId;
      set({ panelActiveTabId: tabId });
      void setWindowTitle(titleOfTabs(get().panelTabs, tabId));
      void bus.emitPanelLayoutOp(windowId, { op: "setActive", tabId });
    },

    panelCloseTab: (tabId) => {
      const windowId = get().windowId;
      const win = get().layoutMirror?.detachedWindows.find((w) => w.id === windowId);
      const tab = win?.tabs.find((t) => t.id === tabId);
      if (!tab || tab.locked) return;
      void bus.emitPanelLayoutOp(windowId, { op: "closeTab", tabId });
    },

    panelSetLocked: (tabId, locked) => {
      const windowId = get().windowId;
      set((s) => ({ panelTabs: s.panelTabs.map((t) => (t.id === tabId ? { ...t, locked } : t)) }));
      void bus.emitPanelLayoutOp(windowId, { op: "setLocked", tabId, locked });
    },

    panelSetTabView: (tabId, view) => {
      const windowId = get().windowId;
      const tab = get().panelTabs.find((t) => t.id === tabId);
      if (!tab || tab.locked) return;
      // 释放交接统一走 initPanel.syncFromUi 的广播 diff（before/after 比较 releaseView）：
      // 本地提前 release 会与广播重复释放，且 Rust 拒 op 竞态下会清空内存态但视图不变
      void bus.emitPanelLayoutOp(windowId, { op: "setTabView", tabId, view });
    },

    panelMoveTab: (tabId, toIndex) => {
      const windowId = get().windowId;
      void bus.emitPanelLayoutOp(windowId, { op: "moveTab", tabId, toIndex });
    },

    panelAddView: (view) => {
      const windowId = get().windowId;
      void bus.emitPanelLayoutOp(windowId, { op: "addView", view });
    },

    releaseView: async (view) => {
      try {
        // 画布视口跨窗口交接（目标窗口挂载后恢复）；内容落盘为共享真相
        if (view === "canvas") {
          emitCanvasViewportHandoff(useAppStore.getState().currentCanvasFile);
        }
        // 领域视图释放（flush 落盘 + 清内存）经生命周期注册表分发
        await kernelLifecycle.releaseView(view);
      } catch (e) {
        console.error(`释放视图 ${view} 落盘失败`, e);
      }
    },

    syncCollabHost: () => {
      const st = useSettingsStore.getState();
      const collab = useCollabStore.getState();
      if (!st.collabEnabled) {
        if (collab.connected) collab.dispose();
        return;
      }
      const mirror = get().layoutMirror;
      if (!mirror) return;
      // 协作宿主 = 本窗口渲染了协作相关视图才连接（每个窗口独立持有连接）：
      // 画布/笔记/表格显示远端 presence，协作房间面板本身也需要在线成员列表。
      // 只统计「视图贡献存在」的相关视图（领域插件停用/卸载后其视图为降级占位，不算协作相关）
      const candidates: ViewKind[] = ["canvas", "table", "note", "collabroom"];
      const relevant = candidates.filter(
        (v) => usePluginStore.getState().viewContribution(v) !== undefined,
      );
      const isHost = relevant.some(
        (v) => findViewHost(mirror.activeTree, mirror.detachedWindows, v) === get().windowId,
      );
      if (isHost && !collab.connected) {
        // 域协作接线随插件启停注册（cordis/builtins 的 collabWiring，pluginStore.spawn 时注册），
        // 此处只需按当前布局是否承载协作视图连接宿主
        collab.init({
          enabled: true,
          url: st.collabRelayUrl,
          nickname: st.collabNickname,
          color: st.collabColor,
          deviceName: st.deviceName,
        });
      } else if (!isHost && collab.connected) {
        collab.dispose();
      }
    },

    notifyPanelClosed: async () => {
      await panelWindowClosed(get().windowId).catch((e) => console.error("上报撕裂窗口关闭失败", e));
    },

    installPanelCloseGuard: async () => {
      if (closeGuardInstalled) return;
      closeGuardInstalled = true;
      // 关窗前先 flush 全部托管视图 + 上报关闭（Rust 移除模型条目，不重复关 OS 窗口）；
      // 销毁由 onCloseRequested 内部完成。panelStore 每窗口一个实例（各自 JS 运行时），
      // 模块级标志恰好按窗口防重
      await onCloseRequested(async () => {
        const ps = usePanelStore.getState();
        for (const v of ps.panelTabs.map((t) => t.view)) {
          await ps.releaseView(v);
        }
        await ps.notifyPanelClosed();
      });
    },
  };
});
