/**
 * 常驻运行时会话的前端面（`ctx.rpc.attach` 的通道承载）：会话建立、上行帧与卸载走宿主命令，
 * 下行帧与会话结束经事件按会话定向投递。纯转发面，协议机制在 services/cordis/rpcChannel。
 */
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

/** 会话建立结果（session id 为收发帧的路由键；pid = 承载会话的运行时进程）。 */
export interface HostSessionStarted {
  sessionId: number;
  pid: number;
}

/** 建立会话并加载插件宿主半模块（模块加载在时限内不到位即 reject）。 */
export function hostRuntimeAttach(
  pluginId: string,
  module: string,
  args: unknown[] | null,
): Promise<HostSessionStarted> {
  return invoke("host_runtime_attach", { pluginId, module, args });
}

/** 上行一帧到指定会话（单行 JSON 文本）。 */
export function hostRuntimeSend(sessionId: number, frame: string): Promise<void> {
  return invoke("host_runtime_send", { sessionId, frame });
}

/** 卸载会话（模块随卸载结束）。幂等。 */
export function hostRuntimeDetach(sessionId: number): Promise<void> {
  return invoke("host_runtime_detach", { sessionId });
}

/** 下行帧事件载荷（会话的 JSON-RPC 消息原文）。 */
interface HostFrameEvent {
  sessionId: number;
  frame: string;
}

/** 会话结束事件载荷（运行时侧主动结束：模块连续崩溃熔断等）。 */
interface HostSessionEndedEvent {
  sessionId: number;
  reason: string;
}

/** 订阅本窗口的常驻运行时下行事件；返回退订函数（通道关闭时由调用方撤销）。 */
export async function subscribeHostRuntime(handlers: {
  onFrame(sessionId: number, frame: string): void;
  onSessionEnded(sessionId: number, reason: string): void;
}): Promise<() => void> {
  let offFrame: (() => void) | undefined;
  try {
    offFrame = await listen<HostFrameEvent>("host-runtime-frame", (event) => {
      handlers.onFrame(event.payload.sessionId, event.payload.frame);
    });
    const offEnd = await listen<HostSessionEndedEvent>("host-runtime-session-ended", (event) => {
      handlers.onSessionEnded(event.payload.sessionId, event.payload.reason);
    });
    return () => {
      offFrame?.();
      offEnd();
    };
  } catch (error) {
    // 第二路订阅失败时退订已就位的第一路，不留无人消费的监听
    offFrame?.();
    throw error;
  }
}
