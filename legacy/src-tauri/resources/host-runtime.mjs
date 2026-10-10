// 常驻运行时 supervisor：承载插件宿主半模块的全应用单例脚本运行时（ctx.rpc.attach 的后端）。
// 由捆绑 Node 启动，进程托管与退出收尾在 Rust 侧（PluginProcessHost）；本脚本只做会话多路复用
// 与崩溃监督。会话内协议 = JSON-RPC 2.0 over NDJSON（docs/plugins/rpc.md 的 v1 契约）。
//
// 与 Rust 宿主的线上协议（stdin/stdout 每行一帧）：
//   会话帧  `<sessionId> <json>`   会话的 JSON-RPC 消息，原样在会话内转发；
//   控制帧  `# <json>`             会话生命周期（attach/detach 入站，attach-ok/attach-err/end 出站）。
// `host:` 前缀的方法名为 supervisor 保留（host:crashed / host:restarted），插件方法不得占用。

import { createInterface } from "node:readline";
import { stdin, stdout } from "node:process";
import { Worker } from "node:worker_threads";

const RPC_PROTOCOL_VERSION = 1;
/** 崩溃重启的退避间隔。 */
const RESTART_DELAY_MS = 1_000;
/** 熔断窗口与次数：窗口内连续崩溃达到次数即停止自动重启并结束会话。 */
const CRASH_WINDOW_MS = 60_000;
const CRASH_LIMIT = 5;
/** detach 的 dispose 宽限：到期仍在跑的线程强收。 */
const DETACH_GRACE_MS = 2_000;

function writeLine(line) {
  stdout.write(line + "\n");
}

/** 向会话发一条 JSON-RPC 消息（请求/响应/通知通用）。 */
function sendFrame(session, message) {
  writeLine(`${session} ${JSON.stringify(message)}`);
}

/** 向 Rust 宿主发一条控制帧。 */
function sendControl(payload) {
  writeLine(`# ${JSON.stringify(payload)}`);
}

const sessions = new Map();

// worker 线程的入口代码（eval 形态，CJS 环境）：动态 import 插件模块 → activate 取描述符 →
// 按描述符分派会话消息。每个会话一个独立线程：崩溃（未捕获异常/process.exit）只结束本会话。
const WORKER_BOOTSTRAP = `
const { parentPort, workerData } = require("node:worker_threads");
const { pathToFileURL } = require("node:url");

let descriptor = null;
const pending = new Map(); // 宿主半反向请求（调客户端半）的未决表
let nextId = 1;

function send(message) {
  parentPort.postMessage({ kind: "frame", message });
}

function callClient(method, params) {
  const id = \`h\${nextId++}\`;
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject });
    send({ jsonrpc: "2.0", id, method, params });
  });
}

parentPort.on("message", (data) => {
  if (data.kind === "dispose") {
    Promise.resolve()
      .then(() => descriptor?.dispose?.())
      .catch(() => {})
      .finally(() => parentPort.close());
    return;
  }
  if (data.kind !== "frame" || typeof data.message !== "object" || data.message === null) return;
  const message = data.message;
  if (message.id !== undefined && message.method !== undefined) {
    // 客户端半请求：未注册方法按契约回 -32601
    Promise.resolve()
      .then(() => {
        const handler = descriptor?.methods?.[message.method];
        if (typeof handler !== "function") {
          send({ jsonrpc: "2.0", id: message.id, error: { code: -32601, message: \`方法未注册：\${message.method}\` } });
          return undefined;
        }
        return Promise.resolve(handler(message.params)).then((result) => {
          send({ jsonrpc: "2.0", id: message.id, result: result === undefined ? null : result });
        });
      })
      .catch((error) => {
        send({
          jsonrpc: "2.0",
          id: message.id,
          error: { code: -32603, message: String(error?.message ?? error) },
        });
      });
    return;
  }
  if (message.id !== undefined) {
    // 客户端半对反向请求的响应
    const waiter = pending.get(String(message.id));
    if (!waiter) return;
    pending.delete(String(message.id));
    if (message.error) waiter.reject(new Error(message.error?.message ?? "客户端半返回错误"));
    else waiter.resolve(message.result);
    return;
  }
  // 客户端半通知：单个处理器异常不拖累线程
  try {
    descriptor?.notify?.[message.method]?.(message.params);
  } catch (error) {
    console.error(error);
  }
});

import(pathToFileURL(workerData.module).href)
  .then(async (mod) => {
    const activate = mod.default;
    if (typeof activate !== "function") {
      throw new Error("宿主半模块的默认导出须为 activate 函数");
    }
    descriptor = (await activate(
      { call: callClient, notify: (method, params) => send({ jsonrpc: "2.0", method, params }) },
      workerData.args ?? [],
    )) ?? {};
    parentPort.postMessage({ kind: "ready", serverInfo: descriptor.serverInfo });
  })
  .catch((error) => {
    parentPort.postMessage({ kind: "failed", reason: String(error?.message ?? error) });
  });
`;

function newSessionRec(control) {
  return {
    id: control.session,
    module: control.module,
    args: Array.isArray(control.args) ? control.args : [],
    worker: null,
    serverInfo: undefined,
    everReady: false,
    ready: false,
    queue: [],
    crashes: [],
    detaching: false,
    /** 当前 worker 实例的崩溃已受理标记（error 与 exit 事件可能对同一次崩溃都触发）。 */
    crashHandled: false,
    restartTimer: null,
  };
}

/** 起一个 worker 线程加载模块（首次 attach 与崩溃重启共用）。 */
function spawnWorker(rec) {
  rec.crashHandled = false;
  rec.ready = false;
  let worker;
  try {
    worker = new Worker(WORKER_BOOTSTRAP, {
      eval: true,
      workerData: { module: rec.module, args: rec.args },
    });
  } catch (error) {
    onSessionFailure(rec, String(error?.message ?? error));
    return;
  }
  rec.worker = worker;
  worker.on("message", (data) => {
    if (rec.detaching || rec.worker !== worker) return;
    if (data.kind === "ready") {
      const firstReady = !rec.everReady;
      rec.ready = true;
      rec.everReady = true;
      const info = data.serverInfo;
      rec.serverInfo =
        info && typeof info === "object"
          ? {
              name: typeof info.name === "string" ? info.name : undefined,
              version: typeof info.version === "string" ? info.version : undefined,
            }
          : undefined;
      if (firstReady) sendControl({ type: "attach-ok", session: rec.id });
      if (rec.queue.length > 0) {
        const queued = rec.queue.splice(0);
        for (const message of queued) dispatchSessionMessage(rec, message);
      }
      return;
    }
    if (data.kind === "failed") {
      onSessionFailure(rec, String(data.reason ?? "宿主半模块加载失败"));
      return;
    }
    if (data.kind === "frame" && data.message && typeof data.message === "object") {
      sendFrame(rec.id, data.message);
    }
  });
  worker.on("error", (error) => {
    if (!rec.detaching && rec.worker === worker && !rec.crashHandled) {
      handleCrash(rec, String(error?.message ?? error));
    }
  });
  worker.on("exit", (code) => {
    if (rec.worker === worker) rec.worker = null;
    // 非 detach 的退出（未捕获异常 / process.exit）：按崩溃受理（error 事件先到则 crashHandled 已置位）
    if (!rec.detaching && !rec.crashHandled) {
      handleCrash(rec, `宿主半线程已退出（code=${code}）`);
    }
  });
}

/** 会话级失败：从未就绪过 = attach 失败（应答宿主 + 收线程 + 撤会话）；就绪后的失败 = 崩溃监督。 */
function onSessionFailure(rec, reason) {
  if (!rec.everReady) {
    rec.detaching = true;
    rec.worker?.terminate().catch(() => {});
    sessions.delete(rec.id);
    sendControl({ type: "attach-err", session: rec.id, error: reason });
    return;
  }
  handleCrash(rec, reason);
}

/** 崩溃受理：告知客户端半（未决请求由通道机制结算）→ 退避重启 → 连续崩溃熔断。 */
function handleCrash(rec, reason) {
  rec.crashHandled = true;
  rec.ready = false;
  rec.serverInfo = undefined;
  if (rec.detaching) return;
  sendFrame(rec.id, { jsonrpc: "2.0", method: "host:crashed", params: { reason } });
  const now = Date.now();
  rec.crashes.push(now);
  rec.crashes = rec.crashes.filter((t) => now - t <= CRASH_WINDOW_MS);
  if (rec.crashes.length >= CRASH_LIMIT) {
    endSession(
      rec,
      `宿主半模块在 ${CRASH_WINDOW_MS / 1000} 秒内连续崩溃 ${rec.crashes.length} 次，已停止自动重启：${reason}`,
    );
    return;
  }
  rec.restartTimer = setTimeout(() => {
    rec.restartTimer = null;
    if (!rec.detaching) spawnWorker(rec);
  }, RESTART_DELAY_MS);
}

/** 结束会话（熔断）：强收线程 + 撤会话 + 应答宿主。 */
function endSession(rec, reason) {
  rec.detaching = true;
  if (rec.restartTimer) clearTimeout(rec.restartTimer);
  rec.worker?.terminate().catch(() => {});
  sessions.delete(rec.id);
  sendControl({ type: "end", session: rec.id, reason });
}

/** 卸载会话（宿主发起）：先给 dispose 的机会，宽限后强收。 */
function detachSession(session) {
  const rec = sessions.get(session);
  if (!rec) return; // 已结束/从未建立：幂等
  sessions.delete(session);
  rec.detaching = true;
  if (rec.restartTimer) clearTimeout(rec.restartTimer);
  const worker = rec.worker;
  if (!worker) return;
  try {
    worker.postMessage({ kind: "dispose" });
  } catch {
    // 线程已在退出中：宽限兜底收掉
  }
  setTimeout(() => {
    worker.terminate().catch(() => {});
  }, DETACH_GRACE_MS);
}

/** 会话消息分派：initialize 握手由 supervisor 按描述符应答，其余转发给模块线程。 */
function dispatchSessionMessage(rec, message) {
  if (message.method === "initialize" && message.id !== undefined) {
    const result = { protocolVersion: RPC_PROTOCOL_VERSION };
    if (rec.serverInfo) result.serverInfo = rec.serverInfo;
    sendFrame(rec.id, { jsonrpc: "2.0", id: message.id, result });
    return;
  }
  rec.worker?.postMessage({ kind: "frame", message });
}

function attachSession(control) {
  if (!Number.isInteger(control.session) || control.session <= 0) return;
  if (typeof control.module !== "string" || control.module === "") {
    sendControl({ type: "attach-err", session: control.session, error: "模块路径缺失" });
    return;
  }
  if (sessions.has(control.session)) return; // 同 id 重复 attach：忽略（宿主不会这样做）
  const rec = newSessionRec(control);
  sessions.set(control.session, rec);
  spawnWorker(rec);
}

function onSessionFrame(session, text) {
  const rec = sessions.get(session);
  if (!rec) return; // 已结束会话的迟到帧
  let message;
  try {
    message = JSON.parse(text);
  } catch (error) {
    console.error(`会话 ${session} 的坏帧已跳过：${String(error)}`);
    return; // 坏行跳过：诊断进 stderr（supervisor 的 stderr），不参与协议
  }
  if (message === null || typeof message !== "object") return;
  if (!rec.ready) {
    rec.queue.push(message);
    return;
  }
  dispatchSessionMessage(rec, message);
}

createInterface({ input: stdin }).on("line", (line) => {
  if (!line.trim()) return;
  if (line.startsWith("#")) {
    let control;
    try {
      control = JSON.parse(line.slice(1));
    } catch {
      return; // 坏控制行跳过
    }
    if (control.type === "attach") attachSession(control);
    else if (control.type === "detach") detachSession(control.session);
    return; // 未知控制类型跳过（前向兼容）
  }
  const sp = line.indexOf(" ");
  if (sp <= 0) return;
  const session = Number(line.slice(0, sp));
  if (!Number.isInteger(session) || session <= 0) return;
  onSessionFrame(session, line.slice(sp + 1));
}).on("close", () => {
  // 宿主侧 stdin 关闭（宿主进程结束/管道断裂）：命令通道已不可达，结束全部会话线程并退出——
  // worker 会撑住事件循环，不主动退出就成了无人管辖的常驻残留
  for (const rec of sessions.values()) {
    rec.detaching = true;
    if (rec.restartTimer) clearTimeout(rec.restartTimer);
    rec.worker?.terminate().catch(() => {});
  }
  process.exit(0);
});
