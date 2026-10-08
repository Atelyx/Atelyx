/** rpcChannel 机制测试：握手、分派、未决结算与关闭传播（传输端用假件注入）。 */
import { describe, expect, it, vi } from "vitest";

import {
  createRpcChannel,
  RPC_ERROR_INTERNAL,
  RPC_ERROR_METHOD_NOT_FOUND,
  RPC_PROTOCOL_VERSION,
  RpcRemoteError,
  type RpcTransport,
} from "./rpcChannel";

/** 假传输端：记录写入、可注入 kill；响应经 feed.receiveLine 回注模拟宿主半。 */
function makeTransport() {
  const writes: string[] = [];
  const transport: RpcTransport = {
    write: vi.fn(async (data: string) => {
      writes.push(data);
    }),
    kill: vi.fn(async () => {}),
  };
  return { transport, writes };
}

function setup(opts?: { initializeTimeoutMs?: number }) {
  const { transport, writes } = makeTransport();
  const feed = createRpcChannel(42, transport, opts);
  return { feed, channel: feed.channel, transport, writes };
}

/** 冲刷微任务：写队列与分派链都是异步落地，读线上内容前先等它们跑完。 */
const flush = (): Promise<void> => new Promise((resolve) => setTimeout(resolve, 0));

/** 取最后一条写入并解析为线上消息。 */
function lastMessage(writes: string[]): Record<string, unknown> {
  expect(writes.length).toBeGreaterThan(0);
  return JSON.parse(writes[writes.length - 1]) as Record<string, unknown>;
}

/** 以宿主半身份回一行。 */
function hostSends(feed: { receiveLine(line: string): void }, message: unknown): void {
  feed.receiveLine(JSON.stringify(message) + "\n");
}

/** 完成一次握手（响应 id 取实际写入）。 */
async function handshake(
  feed: { receiveLine(line: string): void; initialize(): Promise<void> },
  writes: string[],
): Promise<void> {
  const ready = feed.initialize();
  await flush();
  const request = lastMessage(writes);
  feed.receiveLine(
    JSON.stringify({ jsonrpc: "2.0", id: request.id, result: { protocolVersion: 1 } }) + "\n",
  );
  await ready;
}

describe("rpcChannel 握手", () => {
  it("initialize 成功：发协议版本，收 serverInfo", async () => {
    const { feed, writes } = setup();
    const ready = feed.initialize({ name: "atelyx" });
    await flush();
    const request = lastMessage(writes);
    expect(request.method).toBe("initialize");
    expect(request.params).toEqual({
      protocolVersion: RPC_PROTOCOL_VERSION,
      client: { name: "atelyx" },
    });
    hostSends(feed, {
      jsonrpc: "2.0",
      id: request.id,
      result: { protocolVersion: 1, serverInfo: { name: "probe", version: "0.1.0" } },
    });
    await ready;
    expect(feed.channel.remoteInfo).toEqual({ name: "probe", version: "0.1.0" });
    expect(feed.channel.protocolVersion).toBe(1);
    expect(feed.channel.pid).toBe(42);
  });

  it("协议版本不符即失败", async () => {
    const { feed, writes } = setup();
    const ready = feed.initialize();
    await flush();
    hostSends(feed, { jsonrpc: "2.0", id: lastMessage(writes).id, result: { protocolVersion: 99 } });
    await expect(ready).rejects.toThrow("协议版本不符");
  });

  it("握手超时即失败", async () => {
    const { feed } = setup({ initializeTimeoutMs: 20 });
    await expect(feed.initialize()).rejects.toThrow("rpc 握手失败");
  });
});

describe("rpcChannel 客户端 → 宿主半", () => {
  it("call 往返：请求带自增 id 与 params，响应 resolve result", async () => {
    const { feed, channel, writes } = setup();
    await handshake(feed, writes);
    const pending = channel.call<string>("ping", { n: 1 });
    await flush();
    const request = lastMessage(writes);
    expect(request).toMatchObject({ jsonrpc: "2.0", method: "ping", params: { n: 1 } });
    hostSends(feed, { jsonrpc: "2.0", id: request.id, result: "pong" });
    await expect(pending).resolves.toBe("pong");
  });

  it("远端错误映射为 RpcRemoteError（code/data 保留）", async () => {
    const { feed, channel, writes } = setup();
    await handshake(feed, writes);
    const pending = channel.call("boom");
    await flush();
    const request = lastMessage(writes);
    hostSends(feed, {
      jsonrpc: "2.0",
      id: request.id,
      error: { code: -32000, message: "自定义失败", data: { detail: 7 } },
    });
    const error = await pending.then(
      () => {
        throw new Error("应 reject");
      },
      (e: unknown) => e as RpcRemoteError,
    );
    expect(error).toBeInstanceOf(RpcRemoteError);
    expect(error.code).toBe(-32000);
    expect(error.message).toBe("自定义失败");
    expect(error.data).toEqual({ detail: 7 });
  });

  it("call 超时 reject 且不再结算", async () => {
    const { feed, channel, writes } = setup();
    await handshake(feed, writes);
    await expect(channel.call("slow", undefined, { timeoutMs: 20 })).rejects.toThrow("超时");
    // 迟到响应不误投（未决表已摘除）
    hostSends(feed, { jsonrpc: "2.0", id: lastMessage(writes).id, result: "late" });
  });

  it("notify 发送不带 id 的通知", async () => {
    const { feed, channel, writes } = setup();
    await handshake(feed, writes);
    await channel.notify("progress", { step: 2 });
    expect(lastMessage(writes)).toEqual({ jsonrpc: "2.0", method: "progress", params: { step: 2 } });
  });
});

describe("rpcChannel 宿主半 → 客户端", () => {
  it("通知分派给订阅者，退订后不再收", async () => {
    const { feed, channel, writes } = setup();
    await handshake(feed, writes);
    const seen: unknown[] = [];
    const off = channel.on("tick", (p) => seen.push(p));
    hostSends(feed, { jsonrpc: "2.0", method: "tick", params: 1 });
    off();
    hostSends(feed, { jsonrpc: "2.0", method: "tick", params: 2 });
    expect(seen).toEqual([1]);
  });

  it("反向请求：handler 返回值即 result，抛错回内部错误", async () => {
    const { feed, channel, writes } = setup();
    await handshake(feed, writes);
    channel.onRequest("ask", (p) => `answer:${String(p)}`);
    channel.onRequest("fail", () => {
      throw new Error("处理失败");
    });
    hostSends(feed, { jsonrpc: "2.0", id: "r1", method: "ask", params: "q" });
    await flush();
    expect(lastMessage(writes)).toEqual({ jsonrpc: "2.0", id: "r1", result: "answer:q" });
    hostSends(feed, { jsonrpc: "2.0", id: "r2", method: "fail" });
    await flush();
    const error = lastMessage(writes).error as Record<string, unknown>;
    expect(error.code).toBe(RPC_ERROR_INTERNAL);
    expect(error.message).toBe("处理失败");
  });

  it("未注册的反向请求回方法未注册错误", async () => {
    const { feed, writes } = setup();
    await handshake(feed, writes);
    hostSends(feed, { jsonrpc: "2.0", id: 9, method: "nope" });
    await flush();
    const error = lastMessage(writes).error as Record<string, unknown>;
    expect(error.code).toBe(RPC_ERROR_METHOD_NOT_FOUND);
  });
});

describe("rpcChannel 关闭与容错", () => {
  it("进程退出：未决请求统一失败，done 带退出码，后续收发被拒", async () => {
    const { feed, channel, writes } = setup();
    await handshake(feed, writes);
    const pending = channel.call("hang");
    const closed = channel.done;
    feed.transportClosed(3);
    await expect(pending).rejects.toThrow("code=3");
    await expect(closed).resolves.toEqual({ code: 3, reason: expect.stringContaining("3") });
    await expect(channel.call("x")).rejects.toThrow();
    await expect(channel.notify("x")).rejects.toThrow();
  });

  it("坏行与空行跳过，不杀通道", async () => {
    const { feed, channel, writes } = setup();
    await handshake(feed, writes);
    feed.receiveLine("这不是 JSON\n");
    feed.receiveLine("\n");
    feed.receiveLine("   \n");
    feed.receiveLine("[1,2]\n");
    const pending = channel.call("after");
    await flush();
    hostSends(feed, { jsonrpc: "2.0", id: lastMessage(writes).id, result: "ok" });
    await expect(pending).resolves.toBe("ok");
  });

  it("写失败按通道故障收场", async () => {
    const { feed, channel, transport, writes } = setup();
    await handshake(feed, writes);
    transport.write = vi.fn(async () => {
      throw new Error("管道断裂");
    });
    const pending = channel.call("x");
    await expect(pending).rejects.toThrow("管道断裂");
    await expect(channel.done).resolves.toMatchObject({ code: null });
  });

  it("close 结束进程并关闭通道（幂等）", async () => {
    const { feed, channel, transport, writes } = setup();
    await handshake(feed, writes);
    await channel.close();
    expect(transport.kill).toHaveBeenCalledTimes(1);
    await expect(channel.done).resolves.toEqual({
      code: null,
      reason: expect.stringContaining("主动关闭"),
    });
    await channel.close();
    expect(transport.kill).toHaveBeenCalledTimes(1);
  });
});

describe("rpcChannel 软复位（常驻运行时崩溃语义）", () => {
  it("host:crashed 通知结算未决请求但通道保持打开，后续调用照常", async () => {
    const { feed, channel, writes } = setup();
    await handshake(feed, writes);
    const first = channel.call("slow");
    await flush();
    // 模块崩溃：supervisor 发保留通知
    hostSends(feed, { jsonrpc: "2.0", method: "host:crashed", params: { reason: "模块抛出未捕获异常" } });
    await expect(first).rejects.toThrow("模块抛出未捕获异常");
    // 通道未关闭：done 未 resolve、收发照常（重启完成后恢复）
    let ended: unknown = "pending";
    void channel.done.then((v) => (ended = v));
    await flush();
    expect(ended).toBe("pending");
    const second = channel.call("ping");
    await flush();
    hostSends(feed, { jsonrpc: "2.0", id: 3, result: "pong" });
    await expect(second).resolves.toBe("pong");
  });

  it("host:crashed 同时分派给插件订阅者（状态恢复钩子）", async () => {
    const { feed, channel, writes } = setup();
    await handshake(feed, writes);
    const seen: unknown[] = [];
    channel.on("host:crashed", (p) => seen.push(p));
    hostSends(feed, { jsonrpc: "2.0", method: "host:crashed", params: { reason: "x" } });
    expect(seen).toEqual([{ reason: "x" }]);
  });

  it("无 reason 的崩溃通知回退缺省原因", async () => {
    const { feed, channel, writes } = setup();
    await handshake(feed, writes);
    const pending = channel.call("x");
    await flush();
    hostSends(feed, { jsonrpc: "2.0", method: "host:crashed" });
    await expect(pending).rejects.toThrow("宿主半模块已崩溃");
    // 通道未关闭：done 未 resolve（哨兵模式）
    let ended: unknown = "pending";
    void channel.done.then((v) => (ended = v));
    await flush();
    expect(ended).toBe("pending");
  });
});
