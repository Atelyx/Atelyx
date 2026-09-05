/**
 * 视图跨窗口交接（view handoff）：内容照常落盘（磁盘是共享真相），本模块额外转移
 * **轻量运行态**——当前先覆盖画布视口（旗舰视图，价值最高）；表格滚动/选中、笔记
 * 滚动、撤销栈等重量内存态跨窗口不保留（已知限制）。
 *
 * 机制：源窗口视图离开前（releaseView）把本窗口缓存的画布视口经 `view-handoff`
 * 事件广播给目标窗口；目标窗口画布挂载时一次性消费（takePending），若交接事件晚于
 * 挂载到达则经订阅回调补恢复。每窗口各自持有「按画布文件缓存视口」的 Map，交接事件
 * 只传输离开时的缓存值。
 */
import { emit, listen } from "@tauri-apps/api/event";
import type { Viewport } from "@xyflow/react";

/** 按画布文件缓存的本窗口视口（也是交接待恢复缓冲）。 */
const viewports = new Map<string, Viewport>();
/** 目标窗口订阅（画布挂载后收到本画布视口交接 → 补恢复）。 */
const watchers = new Set<(file: string, vp: Viewport) => void>();
let subscribed = false;

function ensureSubscribed(): void {
  if (subscribed) return;
  subscribed = true;
  void listen<{ file: string; viewport: Viewport }>("view-handoff", (e) => {
    const { file, viewport } = e.payload;
    viewports.set(file, viewport);
    watchers.forEach((fn) => fn(file, viewport));
  }).catch(() => {
    subscribed = false;
  });
}

/** 缓存本窗口某画布的视口（CanvasView onMoveEnd 写入；也是交接时取值的来源）。 */
export function cacheCanvasViewport(file: string, vp: Viewport): void {
  viewports.set(file, vp);
}

/** 取本窗口缓存的画布视口（画布挂载恢复用；无缓存返回 null）。 */
export function getCachedCanvasViewport(file: string): Viewport | null {
  return viewports.get(file) ?? null;
}

/** 画布视口交接：视图离开本窗口前调用（releaseView 传当前画布文件），广播给目标窗口。 */
export function emitCanvasViewportHandoff(file: string | null): void {
  if (!file) return;
  const vp = viewports.get(file);
  if (!vp) return;
  void emit("view-handoff", { file, viewport: vp });
}

/** 订阅「本画布视口交接到达」（目标窗口画布挂载后到达时补恢复；返回取消订阅函数）。 */
export function onCanvasViewportHandoff(fn: (file: string, vp: Viewport) => void): () => void {
  ensureSubscribed();
  watchers.add(fn);
  return () => {
    watchers.delete(fn);
  };
}
