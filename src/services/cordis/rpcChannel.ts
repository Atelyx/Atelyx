/**
 * 插件双半的 stdio RPC 通道（JSON-RPC 2.0 over NDJSON）：帧化、消息分派、握手与未决请求表。
 * 纯协议机制，进程托管由 kernel 接线注入；线上契约规范见 docs/plugins/rpc.md。
 */

/** 通道协议版本：initialize 握手双方各自声明，不一致即连接失败（响亮失败，不静默错配）。 */
export const RPC_PROTOCOL_VERSION = 1;

/** initialize 握手缺省超时（毫秒）：宿主半起不来到位即失败，不让 connect 悬挂。 */
const DEFAULT_INITIALIZE_TIMEOUT_MS = 30_000;

/** JSON-RPC 2.0 标准错误码（通道使用子集）。 */
export const RPC_ERROR_METHOD_NOT_FOUND = -32601;
export const RPC_ERROR_INTERNAL = -32603;

/** 宿主半返回的错误（JSON-RPC error 对象的异常形态）。 */
export class RpcRemoteError extends Error {
  readonly code: number;
  readonly data: unknown;

  constructor(code: number, message: string, data?: unknown) {
    super(message);
    this.name = "RpcRemoteError";
    this.code = code;
    this.data = data;
  }
}

/** 通道对传输端的操作（kernel 接线：写 stdin / 结束进程树）。 */
export interface RpcTransport {
  write(data: string): Promise<void>;
  kill(): Promise<void>;
}

/** 宿主半自报的实现信息（initialize 响应的 serverInfo，形状由宿主半自定）。 */
export interface RpcRemoteInfo {
  name?: string;
  version?: string;
}

/** 通道结束信息：code = 进程退出码（主动关闭或非进程原因为 null）。 */
export interface RpcChannelEnd {
  code: number | null;
  reason: string;
}

/** 客户端半的连接参数（进程启动参数 + 握手选项）。 */
export interface RpcConnectOptions {
  initializeTimeoutMs?: number;
}

/** 客户端半暴露给插件的就绪通道：请求 / 通知双向可用。 */
export interface RpcChannel {
  readonly pid: number;
  readonly protocolVersion: number;
  readonly remoteInfo: RpcRemoteInfo | undefined;
  /** 调用宿主半方法（等响应）；timeoutMs 缺省不设超时（通道关闭仍会结算）。 */
  call<T = unknown>(method: string, params?: unknown, opts?: { timeoutMs?: number }): Promise<T>;
  /** 发单向通知（不等响应）。 */
  notify(method: string, params?: unknown): Promise<void>;
  /** 订阅宿主半通知；返回退订函数。 */
  on(method: string, handler: (params: unknown) => void): () => void;
  /** 注册宿主半反向请求的处理（返回值即响应 result，抛错回 -32603）；返回注销函数。 */
  onRequest(method: string, handler: (params: unknown) => unknown): () => void;
  /** 结束宿主半进程（整树）并关闭通道；幂等。 */
  close(): Promise<void>;
  /** 通道关闭时 resolve（含主动关闭）；其后一切收发被拒。 */
  readonly done: Promise<RpcChannelEnd>;
}

interface PendingEntry {
  resolve: (value: unknown) => void;
  reject: (error: Error) => void;
  timer?: ReturnType<typeof setTimeout>;
}

/** 通道 + 喂入口：kernel 把进程事件接进来，插件拿到 channel。 */
export interface RpcChannelFeed {
  channel: RpcChannel;
  /** 进程 stdout 行事件（Rust read_line 契约：一条完整行，含 \r?\n 终止符）。 */
  receiveLine(line: string): void;
  /** 进程退出。 */
  transportClosed(code: number | null): void;
  /** 启动/运行期错误（管道断裂等）。 */
  transportFailed(message: string): void;
  /** 握手：发 initialize 并校验协议版本；失败由调用方负责收尾进程。 */
  initialize(clientInfo?: { name?: string; version?: string }): Promise<void>;
}

/** 构造通道：receiveLine/transportClosed/transportFailed 由 kernel 接到进程事件上。 */
export function createRpcChannel(
  pid: number,
  transport: RpcTransport,
  opts?: RpcConnectOptions,
): RpcChannelFeed {
  let closed = false;
  let end: RpcChannelEnd = { code: null, reason: "通道已关闭" };
  let doneResolve: ((value: RpcChannelEnd) => void) | undefined;
  const done = new Promise<RpcChannelEnd>((resolve) => {
    doneResolve = resolve;
  });

  const pending = new Map<string, PendingEntry>();
  const notificationHandlers = new Map<string, Set<(params: unknown) => void>>();
  const requestHandlers = new Map<string, (params: unknown) => unknown>();
  let nextId = 1;
  let remoteInfo: RpcRemoteInfo | undefined;
  // 串行化 stdin 写入：异步写并发时保持发出顺序
  let writeQueue: Promise<void> = Promise.resolve();

  function closeChannel(info: RpcChannelEnd): void {
    if (closed) return;
    closed = true;
    end = info;
    for (const [key, entry] of pending) {
      if (entry.timer) clearTimeout(entry.timer);
      entry.reject(new Error(info.reason));
      pending.delete(key);
    }
    doneResolve?.(info);
  }

  /** 通道故障收场（写失败 / 运行期错误）：非进程退出原因，code 记 null。 */
  function failTransport(message: string): void {
    closeChannel({ code: null, reason: message });
  }

  /** 发一条线上消息；写失败按通道故障收场（结算未决请求），异常同时抛给直接调用方。 */
  function send(message: Record<string, unknown>): Promise<void> {
    if (closed) return Promise.reject(new Error(end.reason));
    const run = writeQueue.then(() => transport.write(JSON.stringify(message) + "\n"));
    writeQueue = run.then(
      () => undefined,
      () => undefined,
    );
    run.catch((error) => {
      failTransport(`写入对端失败：${error instanceof Error ? error.message : String(error)}`);
    });
    return run;
  }

  function respondError(id: unknown, code: number, message: string): void {
    if (closed) return;
    void send({ jsonrpc: "2.0", id, error: { code, message } }).catch(() => {});
  }

  function dispatch(message: unknown): void {
    if (typeof message !== "object" || message === null) return;
    const msg = message as Record<string, unknown>;
    const hasId = typeof msg.id === "number" || typeof msg.id === "string";
    const hasMethod = typeof msg.method === "string";
    if (hasId && hasMethod) {
      handleRemoteRequest(msg);
      return;
    }
    if (hasId) {
      handleResponse(msg);
      return;
    }
    if (hasMethod) handleNotification(msg.method as string, msg.params);
  }

  function handleResponse(msg: Record<string, unknown>): void {
    const key = String(msg.id);
    const entry = pending.get(key);
    if (!entry) return;
    pending.delete(key);
    if (entry.timer) clearTimeout(entry.timer);
    const err = msg.error;
    if (typeof err === "object" && err !== null) {
      const e = err as Record<string, unknown>;
      entry.reject(
        new RpcRemoteError(
          typeof e.code === "number" ? e.code : RPC_ERROR_INTERNAL,
          typeof e.message === "string" ? e.message : "宿主半返回错误",
          e.data,
        ),
      );
      return;
    }
    entry.resolve(msg.result);
  }

  function handleRemoteRequest(msg: Record<string, unknown>): void {
    const method = msg.method as string;
    const handler = requestHandlers.get(method);
    if (!handler) {
      respondError(msg.id, RPC_ERROR_METHOD_NOT_FOUND, `方法未注册：${method}`);
      return;
    }
    Promise.resolve()
      .then(() => handler(msg.params))
      .then(
        (result) => {
          if (closed) return;
          void send({ jsonrpc: "2.0", id: msg.id, result: result === undefined ? null : result }).catch(
            () => {},
          );
        },
        (error) => {
          respondError(
            msg.id,
            RPC_ERROR_INTERNAL,
            error instanceof Error ? error.message : String(error),
          );
        },
      );
  }

  function handleNotification(method: string, params: unknown): void {
    const handlers = notificationHandlers.get(method);
    if (!handlers) return;
    for (const handler of handlers) {
      // 单个处理器异常不拖累其余订阅者（与内核事件投递同一隔离口径）
      try {
        handler(params);
      } catch (error) {
        console.error(`rpc 通知处理器异常（${method}）`, error);
      }
    }
  }

  function assertMethodName(method: unknown): string | undefined {
    return typeof method !== "string" || method === "" ? "rpc 方法名须为非空字符串" : undefined;
  }

  const channel: RpcChannel = {
    pid,
    protocolVersion: RPC_PROTOCOL_VERSION,
    get remoteInfo() {
      return remoteInfo;
    },
    get done() {
      return done;
    },
    call<T>(method: string, params?: unknown, callOpts?: { timeoutMs?: number }): Promise<T> {
      const invalid = assertMethodName(method);
      if (invalid) return Promise.reject(new Error(invalid));
      if (closed) return Promise.reject(new Error(end.reason));
      const timeoutMs = callOpts?.timeoutMs;
      if (timeoutMs !== undefined && (!Number.isFinite(timeoutMs) || timeoutMs <= 0)) {
        return Promise.reject(new Error("rpc.call 的 timeoutMs 须为正数"));
      }
      const id = nextId++;
      // 响应值类型由调用方断言（线上只是 JSON 值）；机制层按 unknown 结算
      return new Promise<unknown>((resolve, reject) => {
        const entry: PendingEntry = { resolve, reject };
        if (timeoutMs !== undefined) {
          entry.timer = setTimeout(() => {
            pending.delete(String(id));
            reject(new Error(`rpc.call 超时（${timeoutMs}ms）：${method}`));
          }, timeoutMs);
        }
        pending.set(String(id), entry);
        // 写失败已由 transportFailed 统一结算未决请求，这里吞掉 send 自身的拒绝防 unhandled
        const message: Record<string, unknown> = { jsonrpc: "2.0", id, method };
        if (params !== undefined) message.params = params;
        void send(message).catch(() => {});
      }) as Promise<T>;
    },
    async notify(method, params) {
      const invalid = assertMethodName(method);
      if (invalid) throw new Error(invalid);
      if (closed) throw new Error(end.reason);
      const message: Record<string, unknown> = { jsonrpc: "2.0", method };
      if (params !== undefined) message.params = params;
      await send(message);
    },
    on(method, handler) {
      const invalid = assertMethodName(method);
      if (invalid) throw new Error(invalid);
      let handlers = notificationHandlers.get(method);
      if (!handlers) {
        handlers = new Set();
        notificationHandlers.set(method, handlers);
      }
      handlers.add(handler);
      return () => {
        handlers?.delete(handler);
        if (handlers && handlers.size === 0) notificationHandlers.delete(method);
      };
    },
    onRequest(method, handler) {
      const invalid = assertMethodName(method);
      if (invalid) throw new Error(invalid);
      requestHandlers.set(method, handler);
      return () => {
        if (requestHandlers.get(method) === handler) requestHandlers.delete(method);
      };
    },
    async close() {
      if (closed) return;
      closeChannel({ code: null, reason: "通道已被主动关闭" });
      await transport.kill();
    },
  };

  return {
    channel,
    receiveLine(line) {
      if (closed) return;
      const text = line.replace(/\r?\n$/, "");
      if (text.trim() === "") return;
      let parsed: unknown;
      try {
        parsed = JSON.parse(text);
      } catch {
        // 坏行跳过不杀通道：协议外输出走 stderr 与退出诊断
        return;
      }
      dispatch(parsed);
    },
    transportClosed(code) {
      closeChannel({ code, reason: `宿主半进程已退出（code=${code}）` });
    },
    transportFailed(message) {
      failTransport(message);
    },
    async initialize(clientInfo) {
      const timeoutMs = opts?.initializeTimeoutMs ?? DEFAULT_INITIALIZE_TIMEOUT_MS;
      let result: unknown;
      try {
        result = await channel.call(
          "initialize",
          { protocolVersion: RPC_PROTOCOL_VERSION, client: clientInfo ?? {} },
          { timeoutMs },
        );
      } catch (error) {
        throw new Error(
          `rpc 握手失败：${error instanceof Error ? error.message : String(error)}`,
        );
      }
      if (typeof result !== "object" || result === null) {
        throw new Error("rpc 握手失败：initialize 响应不是对象");
      }
      const res = result as Record<string, unknown>;
      if (res.protocolVersion !== RPC_PROTOCOL_VERSION) {
        throw new Error(
          `rpc 握手失败：协议版本不符（本端 ${RPC_PROTOCOL_VERSION}，宿主半 ${String(res.protocolVersion)}）`,
        );
      }
      if (typeof res.serverInfo === "object" && res.serverInfo !== null) {
        const info = res.serverInfo as Record<string, unknown>;
        remoteInfo = {
          name: typeof info.name === "string" ? info.name : undefined,
          version: typeof info.version === "string" ? info.version : undefined,
        };
      }
    },
  };
}
