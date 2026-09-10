/**
 * 事件发射：store/插件经此把领域事件投递到内核事件总线（ctx.emit）。
 *
 * typed events 见 types.ts CordisEvents（vault:switch/canvas:changed/table:changed/
 * collab:changed/vault:changed）；内核未建（kernelRef 空）时 no-op——事件只在运行期发射，
 * 正常路径内核已由 pluginStore.load 创建。
 */
import type { Kernel } from "./kernel";

let kernelRef: Kernel | null = null;

/** 注入/复位内核引用（getKernel/resetKernel 维护；测试可直设）。 */
export function setKernelRef(kernel: Kernel | null): void {
  kernelRef = kernel;
}

/** 事件发射：同步投递到内核 ctx 事件总线；未建内核时忽略。
 *  经 events 服务直发（其 emit 为开放签名；typed events 见 types.ts CordisEvents）。
 *  异常隔离：插件监听器抛错不污染宿主调用方流程（写盘/订阅回调）；错误转 console 供排查。 */
export function emitPluginEvent(event: string, payload: unknown): void {
  try {
    kernelRef?.ctx.events.emit(event, payload);
  } catch (e) {
    console.error(`插件监听器处理事件 ${event} 抛错`, e);
  }
}
