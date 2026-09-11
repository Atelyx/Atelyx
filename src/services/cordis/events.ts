/**
 * 事件发射：store/插件经此把领域事件投递到内核事件总线（ctx.emit）。
 *
 * typed events 见 types.ts CordisEvents（vault:switch/canvas:changed/table:changed/
 * collab:changed/vault:changed）；内核未建（kernelRef 空）时 no-op——事件只在运行期发射，
 * 正常路径内核已由 pluginStore.load 创建。
 */
import { EventsService } from "@atelyx/cordis";
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

let emitIsolated = false;

/**
 * 逐监听器异常隔离：包装 `EventsService.emit`，单个监听器抛错（或返回 rejected Promise）
 * 只记录并继续投递给其余监听器。
 *
 * 为什么必须在宿主侧做：vendor 的 `emit` 是 `map(cb => cb(...args))`——一个监听器抛错即中断，
 * 同事件其余插件的监听器被静默跳过（且调用方看不到），故不能只依赖 vendor 行为，也不改 vendor
 * （vendor 保持上游 commit）。包装保持 vendor 的派发语义（同一 `dispatch("emit", args)` 取回调），
 * 只把「逐个调用」换成「逐个隔离调用」；`internal/*` 同样只是不打断其余，判定语义不变。
 *
 * 进程级幂等且不随内核复位撤销：`EventsService` 是所有 Context 共享的类，按内核装卸补丁会互相影响。
 */
export function installEventIsolation(): void {
  if (emitIsolated) return;
  emitIsolated = true;
  EventsService.prototype.emit = function (...args: unknown[]): void {
    // 事件名必须在 dispatch 前取：dispatch 会就地 shift 掉 thisArg 与 name（args 只剩载荷），
    // 事后取 args[0] 拿到的是载荷、日志会失去定位信息。判定规则与 vendor dispatch 一致。
    const name =
      typeof args[0] === "object" || typeof args[0] === "function" ? args[1] : args[0];
    const callbacks = this.dispatch("emit", args as unknown[]);
    for (const cb of callbacks) {
      try {
        const result = cb(...args);
        // 异步监听器的 rejection 不会被 emit 的同步调用捕获，单独兜底（否则变成未处理拒绝）
        if (result instanceof Promise) {
          void result.catch((e: unknown) =>
            console.error(`插件监听器处理事件 ${String(name)} 抛错（异步）`, e),
          );
        }
      } catch (e) {
        console.error(`插件监听器处理事件 ${String(name)} 抛错`, e);
      }
    }
  };
}
