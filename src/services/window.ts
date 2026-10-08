/**
 * 当前窗口控制 service（decorations: false 自定义标题栏/全屏用）：桌面专有能力，移动端一律 no-op。
 * 调用点先查能力表（services/platform），不把不支持的原生窗口 API 打进安卓 WebView。
 */
import { getCurrentWindow, type Window } from "@tauri-apps/api/window";
import { LogicalSize } from "@tauri-apps/api/dpi";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { platformCapabilities } from "@/services/platform";

/** 窗口控制是否可用（模块级恒定：能力表运行期内不变）。 */
const WINDOW_CONTROLS = platformCapabilities().windowControls;

/** 工作区窗口尺寸（默认与最小一致：不可缩小到默认以下）。 */
const WORKSPACE_WINDOW = { width: 1440, height: 900 };

/** 调整尺寸并按当前显示器居中：center 基于显示器几何计算，不读窗口旧位置——多次调用幂等、无累积漂移
 * （Windows 上 setPosition 异步应用、outerPosition 读回旧值，按中心调整会持续向右下漂移）。 */
async function setSizeCentered(win: Window, width: number, height: number): Promise<void> {
  await win.setSize(new LogicalSize(width, height));
  await win.center();
}

/** 最小化当前窗口（移动端无窗口控制，no-op）。 */
export function minimizeWindow(): Promise<void> {
  if (!WINDOW_CONTROLS) return Promise.resolve();
  return getCurrentWindow().minimize();
}

/** 最大化 / 还原当前窗口（移动端 no-op）。 */
export function toggleMaximizeWindow(): Promise<void> {
  if (!WINDOW_CONTROLS) return Promise.resolve();
  return getCurrentWindow().toggleMaximize();
}

/** 关闭当前窗口（移动端无窗口语义，no-op）。 */
export function closeWindow(): Promise<void> {
  if (!WINDOW_CONTROLS) return Promise.resolve();
  return getCurrentWindow().close();
}

/** 关窗收尾动作：destroy = 销毁本窗口（撕裂窗口）；hideAll = 隐藏全部窗口驻留托盘（主窗口，
 * Rust 侧同步置驻留标志，见 src-tauri/src/tray.rs）。 */
export type CloseAfter = "destroy" | "hideAll";

/** 关窗回调返回 "keep" = 本次关闭请求已由回调消化（如声明 closeHides 的撕裂窗口：
 * 隐藏不销毁），跳过 after 收尾——不返回或返回其他值照常收尾。 */
export type CloseDisposition = "keep" | void;

/** 注册窗口关闭请求监听：先阻止默认关闭，await 回调（落盘等）后按 after 收尾。
 * 返回取消订阅函数；收尾在回调完成后执行，防 debounce 窗口内丢改动。
 * 移动端无「关窗」语义，不注册。 */
export async function onCloseRequested(
  handler: () => Promise<CloseDisposition>,
  after: CloseAfter = "destroy",
): Promise<() => void> {
  if (!WINDOW_CONTROLS) return () => {};
  const win = getCurrentWindow();
  return win.onCloseRequested(async (event) => {
    event.preventDefault();
    let keep = false;
    try {
      keep = (await handler()) === "keep";
    } finally {
      // 回调失败照常收尾（维持既有语义：flush 失败不卡死窗口）；
      // 仅回调明确返回 keep 才跳过收尾
      if (!keep) {
        if (after === "destroy") {
          await win.destroy();
        } else {
          await invoke("hide_to_tray").catch((e) => console.error("驻留系统托盘失败", e));
        }
      }
    }
  });
}

/** 托盘退出请求事件（Rust tray 模块广播给全部 WebView）。 */
const TRAY_EXIT_EVENT = "atelyx:tray-exit-requested";

/** 注册托盘完全退出请求监听（主窗口/撕裂窗口各自注册收尾回调）。
 * 回调完成后调用点须回报 ackExitFlushDone，Rust 收齐全部窗口回报才真正退出。
 * 移动端无托盘语义，不注册。 */
export function onTrayExitRequested(handler: () => Promise<void>): () => void {
  if (!WINDOW_CONTROLS) return () => {};
  let disposed = false;
  let unlisten: (() => void) | null = null;
  void listen(TRAY_EXIT_EVENT, () => void handler()).then((off) => {
    if (disposed) off();
    else unlisten = off;
  });
  return () => {
    disposed = true;
    unlisten?.();
  };
}

/** 回报本窗口退出收尾完成（Rust 收齐全部窗口回报后真正退出进程）。移动端 no-op。 */
export function ackExitFlushDone(): Promise<void> {
  if (!WINDOW_CONTROLS) return Promise.resolve();
  return invoke("exit_flush_done");
}

/** 切换全屏（视图控制图标用；移动端 no-op）。 */
export async function toggleFullscreen(): Promise<void> {
  if (!WINDOW_CONTROLS) return;
  const win = getCurrentWindow();
  const fs = await win.isFullscreen();
  await win.setFullscreen(!fs);
}

/** 应用工作区形态：恢复可调整；窗口小于默认时放大到默认；最小尺寸 = 默认（不可缩小）。
 * 移动端窗口尺寸由系统管理，no-op。 */
export async function applyWorkspaceWindow(): Promise<void> {
  if (!WINDOW_CONTROLS) return;
  const win = getCurrentWindow();
  await win.setResizable(true);
  const minSize = new LogicalSize(WORKSPACE_WINDOW.width, WORKSPACE_WINDOW.height);
  if (await win.isMaximized()) {
    // 最大化时跳过 resize（防强制放大跳变），但最小尺寸约束仍要设：
    // 否则取消最大化后窗口停在小尺寸且无约束（加载屏期间手动最大化 + 自动进工作区的唯一触发路径）
    await win.setMinSize(minSize);
    return;
  }
  const logical = (await win.outerSize()).toLogical(await win.scaleFactor());
  if (logical.width < WORKSPACE_WINDOW.width || logical.height < WORKSPACE_WINDOW.height) {
    await setSizeCentered(win, WORKSPACE_WINDOW.width, WORKSPACE_WINDOW.height);
  }
  // 先 resize 再设最小尺寸：窗口已到默认尺寸，setMinSize 不触发强制拉大
  // （Windows 上 min > 当前尺寸会立即左上锚定放大，导致跳变）
  await win.setMinSize(minSize);
}

/** 读取当前窗口屏幕位置（logical px；屏幕坐标换算用）。
 * 移动端无跨窗口坐标语义，恒返回原点（调用方均为桌面拖拽路径）。 */
export async function getCurrentOuterPosition(): Promise<{ x: number; y: number }> {
  if (!WINDOW_CONTROLS) return { x: 0, y: 0 };
  const win = getCurrentWindow();
  const pos = await win.outerPosition();
  return pos.toLogical(await win.scaleFactor());
}

/** 监听当前窗口移动（缓存窗口位置用；返回取消订阅函数）。移动端 no-op。 */
export async function onWindowMoved(handler: () => void): Promise<() => void> {
  if (!WINDOW_CONTROLS) return () => {};
  return getCurrentWindow().onMoved(handler);
}

/** 设置当前窗口标题（撕裂窗口随激活标签更新）。移动端无撕裂窗口，no-op。 */
export async function setWindowTitle(title: string): Promise<void> {
  if (!WINDOW_CONTROLS) return;
  await getCurrentWindow().setTitle(title);
}

/** 当前窗口 label（主窗口 "main"；撕裂窗口 "panel-<id>"；App 入口分流用）。 */
export function getCurrentWindowLabel(): string {
  return getCurrentWindow().label;
}

/** 鼠标左键当前是否按下（跨窗口拖拽释放检测；仅 Windows 支持，其他平台返回 null）。
 * 标签拖出窗口后 webview 收不到窗口外 pointerup，前端拖拽活跃期间轮询本命令物理判定释放。 */
export async function isMouseLeftDown(): Promise<boolean | null> {
  try {
    const down = await invoke<boolean | null>("is_mouse_left_down");
    return down ?? null;
  } catch (e) {
    console.error("查询鼠标左键状态失败", e);
    return null;
  }
}

/** 显示器几何（物理像素 + 缩放；多屏虚拟桌面坐标系，原点 = 主显示器左上角）。 */
export interface MonitorInfo {
  /** 会话内稳定的序号 id（显示器热插拔后重排，不跨会话持久）。 */
  id: string;
  /** 系统显示器名（拿不到时为空串）。 */
  name: string;
  x: number;
  y: number;
  width: number;
  height: number;
  /** 工作区（扣除任务栏等系统保留区域），同一坐标系。 */
  workX: number;
  workY: number;
  workWidth: number;
  workHeight: number;
  scaleFactor: number;
}

/** 列出全部显示器的几何（多窗口跨屏定位用）；移动端单屏语义返回空列表。 */
export async function listMonitors(): Promise<MonitorInfo[]> {
  if (!WINDOW_CONTROLS) return [];
  try {
    return await invoke<MonitorInfo[]>("list_monitors");
  } catch (e) {
    console.error("查询显示器几何失败", e);
    return [];
  }
}
