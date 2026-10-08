/**
 * ctx.rpc 接线测试（services/cordis/kernel rpc 服务）：连接握手、失败收尾与归属校验。
 * 进程层用 services/shell 假件驱动（机制本身在 rpcChannel.test.ts 覆盖）：假件捕获内核登记的
 * 进程回调，宿主半输出经同一路径（launchTrackedProcess 的 stdout 行事件）回注通道。
 */
import { describe, expect, it, vi, beforeEach, afterEach } from "vitest";

import {
  endProcessStdin,
  killProcessTree,
  runProcess,
  writeProcessStdin,
} from "@/services/shell";
import { createKernel, resetKernel, type Kernel } from "./kernel";
import { mountPlugin, unmountAll } from "./loader";
import type { RpcChannel } from "./types";

type ProcessHandlers = {
  stdout(line: string): void;
  stderr(line: string): void;
  close(code: number | null): void;
  error(message: string): void;
};

/** 内核经 runProcess 登记的进程回调（= 通道的喂入口）。 */
let kernelHandlers: ProcessHandlers | null = null;

vi.mock("@/services/shell", () => ({
  runProcess: vi.fn(async (_command: string, _args: string[], _opts: unknown, handlers: ProcessHandlers) => {
    kernelHandlers = handlers;
    return 4321;
  }),
  killProcessTree: vi.fn(async () => {}),
  writeProcessStdin: vi.fn(async () => {}),
  endProcessStdin: vi.fn(async () => {}),
}));

/** 以宿主半身份回一行 stdout（行事件自带换行终止符）。 */
function hostLine(message: unknown): void {
  kernelHandlers?.stdout(JSON.stringify(message) + "\n");
}

/** 冲刷微任务：写队列与握手链都是异步落地。 */
const flush = (): Promise<void> => new Promise((resolve) => setTimeout(resolve, 0));

describe("ctx.rpc 接线", () => {
  let kernel: Kernel | null = null;

  beforeEach(() => {
    vi.mocked(runProcess).mockClear();
    vi.mocked(killProcessTree).mockClear();
    vi.mocked(writeProcessStdin).mockClear();
    vi.mocked(endProcessStdin).mockClear();
    kernelHandlers = null;
  });

  afterEach(async () => {
    if (kernel) {
      await unmountAll(kernel);
      kernel.dispose();
      kernel = null;
    }
    resetKernel();
  });

  it("connect 握手成功返回就绪通道（进程经托管面启动，pid 记账）", async () => {
    kernel = createKernel();
    let captured: Promise<RpcChannel> | undefined;
    await mountPlugin(kernel, {
      id: "com.test.rpc",
      apply: (ctx) => {
        captured = ctx.rpc.connect({ command: "node", args: ["host.mjs"] });
      },
    });
    await flush();
    expect(vi.mocked(writeProcessStdin).mock.calls[0]?.[0]).toBe(4321);
    hostLine({ jsonrpc: "2.0", id: 1, result: { protocolVersion: 1, serverInfo: { name: "t" } } });
    if (!captured) throw new Error("apply 未发起连接");
    const channel = await captured;
    expect(channel.pid).toBe(4321);
    expect(channel.remoteInfo).toEqual({ name: "t" });
    expect(runProcess).toHaveBeenCalledWith(
      "node",
      ["host.mjs"],
      expect.anything(),
      expect.anything(),
    );
  });

  it("握手失败不留活进程（kill 收尾）且 connect reject", async () => {
    kernel = createKernel();
    let captured: Promise<RpcChannel> | undefined;
    await mountPlugin(kernel, {
      id: "com.test.rpc",
      apply: (ctx) => {
        captured = ctx.rpc.connect({ command: "node" });
      },
    });
    await flush();
    hostLine({ jsonrpc: "2.0", id: 1, result: { protocolVersion: 99 } });
    if (!captured) throw new Error("apply 未发起连接");
    await expect(captured).rejects.toThrow("协议版本不符");
    expect(killProcessTree).toHaveBeenCalledWith(4321);
  });

  it("非插件上下文调用拒绝", async () => {
    kernel = createKernel();
    await expect(kernel.ctx.rpc.connect({ command: "node" })).rejects.toThrow("插件上下文");
  });

  it("connect 传 input 显式拒绝（stdin 由协议占用）", async () => {
    kernel = createKernel();
    let rejected: unknown;
    await mountPlugin(kernel, {
      id: "com.test.rpc",
      apply: (ctx) => {
        rejected = ctx.rpc.connect({ command: "node", input: "x" }).then(
          () => undefined,
          (e: unknown) => e,
        );
      },
    });
    await expect(rejected).resolves.toBeInstanceOf(Error);
  });
});
