/**
 * 事件桥：把桥事件（emitPluginEvent）同步转发到 Cordis 事件总线。
 *
 * 桥事件载荷形状与 typed 事件定义（types.ts CordisEvents）一一对应——
 * 订阅方经 ctx.on("canvas:changed") 等收到与旧桥订阅者相同的事件。
 * 卸载时复位转发钩子（桥回到只投递旧桥订阅者）。
 */
import { setPluginEventForwarder } from "@/services/plugins";
import type { Context } from "@atelyx/cordis";

/** 安装事件桥（内核 createKernel 时调用）；返回撤销函数。 */
export function installEventBridge(ctx: Context): () => void {
  const emit = ctx.emit.bind(ctx) as (name: string, ...args: unknown[]) => void;
  setPluginEventForwarder((event, payload) => {
    emit(event, payload);
  });
  return () => {
    setPluginEventForwarder(null);
  };
}
