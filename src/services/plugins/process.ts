/**
 * 插件子进程运行时传输层（Python）：把子进程的 stdio 桥适配成 PluginTransport，
 * 让桥宿主（bridge.ts）像对待 worker 一样对待子进程——能力路由/审计/生命周期全部复用。
 *
 * 数据流：transport.post → invoke `plugin_process_write`（Rust 写子进程 stdin 一行 JSON）；
 * 子进程 stdout 行 → Rust 事件 `plugin-process-message` → 本 transport.onMessage 订阅者。
 *
 * 竞态处理（与 worker 平面不同，子进程由 Rust spawn、前端经事件收消息）：
 * - **先 listen 再 invoke start**：启动即注册消息监听，进程冷启期间的早期 call 不丢；
 * - onMessage/onCrash 订阅前到达的消息/退出事件先入队，订阅后补发
 *   （attachPlugin 接线前的窗口被覆盖）；
 * - stderr 保留最近几行，onCrash 时附在失败信息里（进程崩溃原因可见）。
 */
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { PluginRuntime } from "@/types";
import type { PluginTransport } from "./worker";

/** 崩溃信息里附带的 stderr 尾部行数上限。 */
const MAX_STDERR_TAIL = 8;

/** 启动插件子进程并返回其 transport；调用方随后 attachPlugin(manifest, transport)。 */
export async function startPluginProcess(id: string, runtime: PluginRuntime): Promise<PluginTransport> {
  const queue: unknown[] = [];
  const crashQueue: string[] = [];
  let handler: ((message: unknown) => void) | null = null;
  let crashHandler: ((message: string) => void) | null = null;
  let disposed = false;
  const stderrTail: string[] = [];
  // 先占位再赋值：listener 闭包在 invoke 返回前就会引用它（其他插件进程的事件也会到达），
  // 用可空值前置声明避免 TDZ ReferenceError。
  let processId: number | null = null;

  // 先订阅事件再启动子进程：启动瞬间的消息（插件 main 顶层的注册调用）不丢。
  const unlisteners: UnlistenFn[] = [];
  unlisteners.push(
    await listen<{ process_id: number; message: unknown }>("plugin-process-message", (e) => {
      if (disposed || processId === null || e.payload.process_id !== processId) return;
      if (handler) handler(e.payload.message);
      else queue.push(e.payload.message);
    }),
  );
  unlisteners.push(
    await listen<{ process_id: number; line: string }>("plugin-process-stderr", (e) => {
      if (processId === null || e.payload.process_id !== processId) return;
      stderrTail.push(e.payload.line);
      if (stderrTail.length > MAX_STDERR_TAIL) stderrTail.shift();
    }),
  );
  unlisteners.push(
    await listen<{ process_id: number; code: number | null }>("plugin-process-exit", (e) => {
      if (disposed || processId === null || e.payload.process_id !== processId) return;
      const detail = stderrTail.length > 0 ? `；stderr 尾部：\n${stderrTail.join("\n")}` : "";
      const message = `插件进程已退出${e.payload.code != null ? `（code ${e.payload.code}）` : ""}${detail}`;
      if (crashHandler) crashHandler(message);
      else crashQueue.push(message);
    }),
  );

  const pid = (processId = await invoke<number>("plugin_process_start", { id, runtime }));

  return {
    post: (message) => {
      if (disposed) return;
      void invoke("plugin_process_write", { processId: pid, line: JSON.stringify(message) }).catch(() => {
        // 写入失败（进程已退出）：post 无返回语义，交由 onCrash/dispose 处理。
      });
    },
    onMessage: (h) => {
      handler = h;
      const pending = queue.splice(0);
      for (const m of pending) handler(m);
      return () => {
        handler = null;
      };
    },
    onCrash: (h) => {
      crashHandler = h;
      const pending = crashQueue.splice(0);
      for (const m of pending) h(m);
      return () => {
        crashHandler = null;
      };
    },
    dispose: () => {
      if (disposed) return;
      disposed = true;
      void invoke("plugin_process_kill", { processId: pid }).catch(() => {});
      for (const un of unlisteners) un();
    },
  };
}
