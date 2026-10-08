/**
 * ctx.rpc.attach 接线测试（services/cordis/kernel 的常驻运行时会话）：会话建立、握手、
 * 帧路由、会话结束收场与失败退订。运行时通道层用 services/hostRuntime 假件驱动（协议机制
 * 本身在 rpcChannel.test.ts 覆盖）：下行帧经 subscribeHostRuntime 捕获的处理器回注通道。
 */
import { describe, expect, it, vi, beforeEach, afterEach } from "vitest";

import {
  hostRuntimeAttach,
  hostRuntimeDetach,
  hostRuntimeSend,
} from "@/services/hostRuntime";
import { createKernel, resetKernel, type Kernel } from "./kernel";
import { mountPlugin, unmountAll } from "./loader";
import type { RpcChannel } from "./types";

/** 内核经 subscribeHostRuntime 登记的下行处理器（= 通道的喂入口）。 */
let downlink: {
  onFrame(sessionId: number, frame: string): void;
  onSessionEnded(sessionId: number, reason: string): void;
} | null = null;

/** 通道发出的上行帧（hostRuntimeSend 捕获）。 */
const sentFrames: string[] = [];

vi.mock("@/services/hostRuntime", () => ({
  hostRuntimeAttach: vi.fn(async () => ({ sessionId: 77, pid: 4321 })),
  hostRuntimeSend: vi.fn(async (_sessionId: number, frame: string) => {
    sentFrames.push(frame);
  }),
  hostRuntimeDetach: vi.fn(async () => {}),
  subscribeHostRuntime: vi.fn(async (handlers: NonNullable<typeof downlink>) => {
    downlink = handlers;
    return () => {
      downlink = null;
    };
  }),
}));

/** 冲刷微任务：attach 链与写队列都是异步落地。 */
const flush = (): Promise<void> => new Promise((resolve) => setTimeout(resolve, 0));

/** 等到 initialize 请求上行后，以 supervisor 身份回握手响应。 */
async function respondInitialize(): Promise<void> {
  for (let i = 0; i < 100; i++) {
    const frame = sentFrames.find((f) => f.includes('"initialize"'));
    if (frame !== undefined) {
      const request = JSON.parse(frame) as { id: number };
      downlink?.onFrame(
        77,
        JSON.stringify({
          jsonrpc: "2.0",
          id: request.id,
          result: { protocolVersion: 1, serverInfo: { name: "t" } },
        }),
      );
      return;
    }
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
  throw new Error("未见 initialize 请求上行");
}

describe("ctx.rpc.attach 接线", () => {
  let kernel: Kernel | null = null;

  beforeEach(() => {
    vi.mocked(hostRuntimeAttach).mockClear();
    vi.mocked(hostRuntimeSend).mockClear();
    vi.mocked(hostRuntimeDetach).mockClear();
    sentFrames.length = 0;
    downlink = null;
  });

  afterEach(async () => {
    if (kernel) {
      await unmountAll(kernel);
      kernel.dispose();
      kernel = null;
    }
    resetKernel();
  });

  it("attach 握手成功返回就绪通道：pid 为运行时进程，收发按会话路由", async () => {
    kernel = createKernel();
    let captured: Promise<RpcChannel> | undefined;
    await mountPlugin(kernel, {
      id: "com.test.attach",
      apply: (ctx) => {
        captured = ctx.rpc.attach({ module: "C:\\plugins\\x\\data\\files\\host.mjs" });
      },
    });
    await flush();
    expect(hostRuntimeAttach).toHaveBeenCalledWith(
      "com.test.attach",
      "C:\\plugins\\x\\data\\files\\host.mjs",
      null,
    );
    await respondInitialize();
    if (!captured) throw new Error("apply 未发起 attach");
    const channel = await captured;
    expect(channel.pid).toBe(4321);
    expect(channel.remoteInfo).toEqual({ name: "t" });

    // call 上行带会话 id 封装（Rust 侧按 sessionId 路由）
    const pending = channel.call("ping", { v: 1 });
    await flush();
    expect(sentFrames[sentFrames.length - 1]).toContain('"ping"');
    downlink?.onFrame(77, JSON.stringify({ jsonrpc: "2.0", id: 2, result: "pong" }));
    await expect(pending).resolves.toBe("pong");

    // close → detach 命令（会话卸载）
    await channel.close();
    expect(hostRuntimeDetach).toHaveBeenCalledWith(77);
  });

  it("他窗会话的帧不串台：只喂本会话 id 的帧", async () => {
    kernel = createKernel();
    let captured: Promise<RpcChannel> | undefined;
    await mountPlugin(kernel, {
      id: "com.test.attach",
      apply: (ctx) => {
        captured = ctx.rpc.attach({ module: "C:\\x\\host.mjs" });
      },
    });
    await flush();
    // attach-ok 之前到达的其他会话帧进 backlog，建立后按 sessionId 过滤丢弃
    downlink?.onFrame(999, JSON.stringify({ jsonrpc: "2.0", method: "foreign" }));
    await respondInitialize();
    if (!captured) throw new Error("apply 未发起 attach");
    const channel = await captured;
    const seen: unknown[] = [];
    channel.on("foreign", (p) => seen.push(p));
    downlink?.onFrame(999, JSON.stringify({ jsonrpc: "2.0", method: "foreign", params: 1 }));
    downlink?.onFrame(77, JSON.stringify({ jsonrpc: "2.0", method: "foreign", params: 2 }));
    expect(seen).toEqual([2]);
  });

  it("会话结束事件关闭通道（done 带原因），登记随关闭出册", async () => {
    kernel = createKernel();
    let captured: Promise<RpcChannel> | undefined;
    await mountPlugin(kernel, {
      id: "com.test.attach",
      apply: (ctx) => {
        captured = ctx.rpc.attach({ module: "C:\\x\\host.mjs" });
      },
    });
    await flush();
    await respondInitialize();
    if (!captured) throw new Error("apply 未发起 attach");
    const channel = await captured;
    downlink?.onSessionEnded(77, "连续崩溃熔断");
    await expect(channel.done).resolves.toMatchObject({ code: null });
    // 收尾后 detach 幂等再触发一次（close 语义），退订完成
    await channel.close();
    expect(downlink).toBeNull();
  });

  it("会话建立失败：attach reject 且退订下行事件", async () => {
    vi.mocked(hostRuntimeAttach).mockImplementationOnce(async () => {
      throw new Error("模块加载失败");
    });
    kernel = createKernel();
    let captured: Promise<RpcChannel> | undefined;
    await mountPlugin(kernel, {
      id: "com.test.attach",
      apply: (ctx) => {
        captured = ctx.rpc.attach({ module: "C:\\x\\host.mjs" });
      },
    });
    await flush();
    await expect(captured).rejects.toThrow("模块加载失败");
    expect(downlink).toBeNull();
  });

  it("握手失败不留会话（detach 收尾）且退订", async () => {
    kernel = createKernel();
    let captured: Promise<RpcChannel> | undefined;
    await mountPlugin(kernel, {
      id: "com.test.attach",
      apply: (ctx) => {
        captured = ctx.rpc.attach({ module: "C:\\x\\host.mjs" });
      },
    });
    await flush();
    // 以不匹配的协议版本回握手
    for (let i = 0; i < 100; i++) {
      const frame = sentFrames.find((f) => f.includes('"initialize"'));
      if (frame !== undefined) {
        const request = JSON.parse(frame) as { id: number };
        downlink?.onFrame(
          77,
          JSON.stringify({ jsonrpc: "2.0", id: request.id, result: { protocolVersion: 99 } }),
        );
        break;
      }
      await new Promise((resolve) => setTimeout(resolve, 5));
    }
    await expect(captured).rejects.toThrow("协议版本不符");
    expect(hostRuntimeDetach).toHaveBeenCalledWith(77);
    expect(downlink).toBeNull();
  });

  it("非插件上下文调用拒绝；args 非数组拒绝", async () => {
    kernel = createKernel();
    await expect(kernel.ctx.rpc.attach({ module: "C:\\x\\host.mjs" })).rejects.toThrow("插件上下文");
    await mountPlugin(kernel, {
      id: "com.test.attach",
      apply: (ctx) => {
        void (ctx.rpc.attach({ module: "C:\\x\\host.mjs", args: "not-array" as unknown as unknown[] }) as Promise<unknown>).catch(
          (e: unknown) => e,
        );
      },
    });
    await flush();
    expect(vi.mocked(hostRuntimeAttach).mock.calls.length).toBe(0);
  });
});
