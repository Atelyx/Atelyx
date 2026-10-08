/**
 * process 服务面测试（services/cordis/kernel 的 `ctx.process`）：进程按调用方插件记账（spawn/exec 启动都登记）、进程结束摘除而运行期 `error` 不摘（进程可能仍在跑）；
 * `spawn` 返回带 pid 的句柄，`cancel` 结束进程树并使登记失效，已结束句柄再 `cancel` 为 no-op（防 pid 复用误杀）；句柄 `write`/`endInput` 直通 stdin 写入面；
 * stdin 关闭策略：exec 不传 input 时 pid 落地即关（历史 null-stdin 语义），spawn 保持开放，input 写失败不进插件错误面；
 * 启动在途（pid 未落地）时停用也会等到落地再结束（不漏杀）；非插件上下文调用直接拒绝（tracker 绑定，API 不暴露插件 id）。`@/services/shell` 以替身替代（真跑进程交由 Rust 侧测试），只验证内核侧接线。
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Context } from "@atelyx/cordis";

/** 替身进程：pid 由测试指定，close/error 回调由测试触发。 */
interface FakeProcess {
  pid: number;
  options: unknown;
  handlers: {
    stdout: (line: string) => void;
    stderr: (line: string) => void;
    close: (code: number | null) => void;
    error: (message: string) => void;
  };
}

const spawned: FakeProcess[] = [];
const killProcessTree = vi.fn<(pid: number) => Promise<void>>(async () => {});
const writeProcessStdin = vi.fn<(pid: number, data: string) => Promise<void>>(async () => {});
const endProcessStdin = vi.fn<(pid: number) => Promise<void>>(async () => {});
/** 置位后下一次启动的 pid 延迟到 releaseDeferredLaunch() 才落地（模拟在途启动）。 */
let deferNextLaunch = false;
let releaseDeferredLaunch: (() => void) | null = null;

vi.mock("@/services/shell", () => ({
  openInExplorer: () => Promise.resolve(),
  openUrl: () => Promise.resolve(),
  runProcess: (
    _program: string,
    _args: string[],
    options: unknown,
    handlers: {
      stdout: (line: string) => void;
      stderr: (line: string) => void;
      close: (code: number | null) => void;
      error: (message: string) => void;
    },
  ) => {
    const proc: FakeProcess = { pid: 9000 + spawned.length, options, handlers };
    spawned.push(proc);
    if (!deferNextLaunch) return Promise.resolve(proc.pid);
    deferNextLaunch = false;
    return new Promise<number>((resolve) => {
      releaseDeferredLaunch = () => resolve(proc.pid);
    });
  },
  killProcessTree: (pid: number) => killProcessTree(pid),
  writeProcessStdin: (pid: number, data: string) => writeProcessStdin(pid, data),
  endProcessStdin: (pid: number) => endProcessStdin(pid),
}));

import { createKernel } from "./kernel";
import { mountPlugin, unmountAll } from "./loader";
import { killPluginProcesses, killUnmountedPluginProcesses } from "./pluginProcesses";

/** 结束该插件的在册进程（与 pluginStore 停用路径同口径），返回被结束的 pid。 */
async function killTracked(kernelCtx: object, pluginId: string): Promise<number[]> {
  const killed: number[] = [];
  await killPluginProcesses(kernelCtx, pluginId, async (pid) => {
    killed.push(pid);
  });
  return killed;
}

describe("ctx.process 进程记账与句柄", () => {
  beforeEach(() => {
    // 替身状态跨用例复位：defer 标志若泄漏到下一个用例会让它整体挂起
    deferNextLaunch = false;
    releaseDeferredLaunch = null;
    killProcessTree.mockClear();
    writeProcessStdin.mockClear();
    endProcessStdin.mockClear();
  });

  it("spawn 返回 pid 并按调用方插件登记；cancel 结束进程树且登记随之失效", async () => {
    spawned.length = 0;
    killProcessTree.mockClear();
    const kernel = createKernel();
    let handle: { pid: number; cancel: () => Promise<void> } | undefined;

    await mountPlugin(kernel, {
      id: "com.test.spawn",
      apply: async (ctx: Context) => {
        handle = await ctx.process.spawn({ command: "sh", args: ["-c", "serve"] });
      },
    });

    expect(handle?.pid).toBe(9000);
    expect(await killTracked(kernel.ctx, "com.test.spawn")).toEqual([9000]);

    await handle!.cancel();
    expect(killProcessTree).toHaveBeenCalledWith(9000);
    // cancel 后登记已清（再次结束为 no-op）
    expect(await killTracked(kernel.ctx, "com.test.spawn")).toEqual([]);
    kernel.dispose();
  });

  it("spawn 句柄 write/endInput 直通 stdin 写入面（按 pid 定向）", async () => {
    spawned.length = 0;
    const kernel = createKernel();
    let handle: { write: (data: string) => Promise<void>; endInput: () => Promise<void> } | undefined;

    await mountPlugin(kernel, {
      id: "com.test.stdin",
      apply: async (ctx: Context) => {
        handle = await ctx.process.spawn({ command: "sh", args: ["-c", "rpc"] });
      },
    });

    await handle!.write("{\"id\":1}\n");
    await handle!.endInput();
    expect(writeProcessStdin).toHaveBeenCalledWith(9000, "{\"id\":1}\n");
    expect(endProcessStdin).toHaveBeenCalledWith(9000);
    kernel.dispose();
  });

  it("exec 的 input：pid 落地后写一次 stdin 并关闭（写入/关闭由内核 stdin 策略负责）", async () => {
    spawned.length = 0;
    writeProcessStdin.mockClear();
    endProcessStdin.mockClear();
    const kernel = createKernel();

    await mountPlugin(kernel, {
      id: "com.test.input",
      apply: async (ctx: Context) => {
        const pending = ctx.process.exec({ command: "sh", args: ["-c", "filter"], input: "payload\n" });
        spawned[0].handlers.close(0);
        await pending;
      },
    });

    await vi.waitFor(() => expect(writeProcessStdin).toHaveBeenCalledWith(9000, "payload\n"));
    await vi.waitFor(() => expect(endProcessStdin).toHaveBeenCalledWith(9000));
    kernel.dispose();
  });

  it("exec 不传 input：pid 落地即关 stdin（与历史 null-stdin 语义一致，读 stdin 的程序拿到 EOF）", async () => {
    spawned.length = 0;
    endProcessStdin.mockClear();
    const kernel = createKernel();

    await mountPlugin(kernel, {
      id: "com.test.exec-eof",
      apply: async (ctx: Context) => {
        const pending = ctx.process.exec({ command: "sh", args: ["-c", "cat"] });
        spawned[0].handlers.close(0);
        await pending;
      },
    });

    await vi.waitFor(() => expect(endProcessStdin).toHaveBeenCalledWith(9000));
    kernel.dispose();
  });

  it("spawn 不传 input：stdin 保持开放（等句柄 write/endInput），不主动关闭", async () => {
    spawned.length = 0;
    endProcessStdin.mockClear();
    const kernel = createKernel();

    await mountPlugin(kernel, {
      id: "com.test.spawn-keep",
      apply: async (ctx: Context) => {
        await ctx.process.spawn({ command: "sh", args: ["-c", "rpc"] });
      },
    });

    // pid 已落地（spawn 已 await），关闭策略不该触发
    await new Promise((r) => setTimeout(r, 20));
    expect(endProcessStdin).not.toHaveBeenCalled();
    kernel.dispose();
  });

  it("exec input 写失败不进插件错误面（进程自身退出码与输出即结果），只记宿主诊断", async () => {
    spawned.length = 0;
    writeProcessStdin.mockRejectedValueOnce(new Error("写入进程 9000 的 stdin 失败：管道已关闭"));
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const kernel = createKernel();
    let result: { code: number | null; stdout: string; stderr: string } | undefined;

    try {
      await mountPlugin(kernel, {
        id: "com.test.input-fail",
        apply: async (ctx: Context) => {
          const pending = ctx.process.exec({ command: "sh", args: ["-c", "quick"], input: "x\n" });
          spawned[0].handlers.stdout("done\n");
          spawned[0].handlers.close(0);
          result = await pending;
        },
      });

      await vi.waitFor(() => expect(warn).toHaveBeenCalled());
      expect(result).toMatchObject({ code: 0, stdout: "done\n" });
    } finally {
      warn.mockRestore();
      kernel.dispose();
    }
  });

  it("进程退出后取消句柄为 no-op（pid 可能已被系统复用给别的进程）", async () => {
    spawned.length = 0;
    killProcessTree.mockClear();
    const kernel = createKernel();
    let handle: { pid: number; cancel: () => Promise<void> } | undefined;

    await mountPlugin(kernel, {
      id: "com.test.exited",
      apply: async (ctx: Context) => {
        handle = await ctx.process.spawn({ command: "sh", args: ["-c", "quick"] });
      },
    });
    // 进程自行退出：登记立即摘除
    spawned[0].handlers.close(0);
    expect(await killTracked(kernel.ctx, "com.test.exited")).toEqual([]);

    await handle!.cancel();
    expect(killProcessTree).not.toHaveBeenCalled();
    kernel.dispose();
  });

  it("进程极快退出（先于 pid 到位）时不登记，也不残留", async () => {
    spawned.length = 0;
    const kernel = createKernel();
    let spawnedHandle = false;

    await mountPlugin(kernel, {
      id: "com.test.race",
      apply: async (ctx: Context) => {
        const pending = ctx.process.spawn({ command: "sh", args: ["-c", "instant"] });
        // 退出事件先到（真实时序里进程可能在 spawn resolve 前就结束）
        spawned[0].handlers.close(1);
        await pending;
        spawnedHandle = true;
      },
    });

    expect(spawnedHandle).toBe(true);
    expect(await killTracked(kernel.ctx, "com.test.race")).toEqual([]);
    kernel.dispose();
  });

  it("启动在途时停用：结束流程等 pid 落地后再收，不漏杀", async () => {
    spawned.length = 0;
    killProcessTree.mockClear();
    const kernel = createKernel();

    deferNextLaunch = true;
    await mountPlugin(kernel, {
      id: "com.test.pending",
      apply: (ctx: Context) => {
        // 故意不 await：pid 登记晚于 apply 返回（真实插件里的 fire-and-forget 写法）
        void ctx.process.spawn({ command: "sh", args: ["-c", "serve"] });
      },
    });

    // 此刻 pid 还没落地（登记为空），结束流程必须等在途启动落地后再收
    const killing = killTracked(kernel.ctx, "com.test.pending");
    // 用非空断言而非可选调用：defer 路径没被走到时应直接断言失败，而不是退化成超时
    expect(releaseDeferredLaunch).not.toBeNull();
    releaseDeferredLaunch!();
    expect(await killing).toEqual([9000]);
    kernel.dispose();
  });

  it("未挂载收尾也覆盖在途启动（pid 未落地也不会漏杀）", async () => {
    spawned.length = 0;
    const kernel = createKernel();

    deferNextLaunch = true;
    await mountPlugin(kernel, {
      id: "com.test.pending-sweep",
      apply: (ctx: Context) => {
        void ctx.process.spawn({ command: "sh", args: ["-c", "serve"] });
      },
    });

    // 已挂载表里没有它（模拟跨窗口停用/未挂载残留）：扫尾必须把它算作候选并等 pid 落地
    const swept = killUnmountedPluginProcesses(kernel.ctx, [], async (pid) => {
      await killProcessTree(pid);
    });
    expect(releaseDeferredLaunch).not.toBeNull();
    releaseDeferredLaunch!();
    await swept;
    expect(killProcessTree).toHaveBeenCalledWith(9000);
    kernel.dispose();
  });

  it("运行期错误不摘除登记（进程可能仍在跑），cancel 仍能结束它", async () => {
    spawned.length = 0;
    killProcessTree.mockClear();
    const kernel = createKernel();
    let handle: { pid: number; cancel: () => Promise<void> } | undefined;

    await mountPlugin(kernel, {
      id: "com.test.runtime-error",
      apply: async (ctx: Context) => {
        handle = await ctx.process.spawn({ command: "sh", args: ["-c", "serve"] }, {
          chunk: () => {},
          end: () => {},
          error: () => {},
        });
      },
    });

    // 运行期错误（如管道读失败）：进程可能还活着，登记不能摘
    spawned[0].handlers.error("管道读取失败");
    expect(await killTracked(kernel.ctx, "com.test.runtime-error")).toEqual([9000]);

    await handle!.cancel();
    expect(killProcessTree).toHaveBeenCalledWith(9000);
    kernel.dispose();
  });

  it("流式 exec 启动的进程同样登记（插件用 exec 起长驻服务也能被收尾）", async () => {
    spawned.length = 0;
    const kernel = createKernel();

    await mountPlugin(kernel, {
      id: "com.test.exec",
      apply: (ctx: Context) => {
        // 流式 exec：不 await（等进程结束才 resolve）
        void ctx.process.exec({ command: "sh", args: ["-c", "serve"] }, {
          chunk: () => {},
          end: () => {},
          error: () => {},
        });
      },
    });

    expect(await killTracked(kernel.ctx, "com.test.exec")).toEqual([9000]);
    kernel.dispose();
  });

  it("非流式 exec 的进程在结束后不残留登记", async () => {
    spawned.length = 0;
    const kernel = createKernel();

    await mountPlugin(kernel, {
      id: "com.test.aggregate",
      apply: async (ctx: Context) => {
        const pending = ctx.process.exec({ command: "sh", args: ["-c", "echo"] });
        spawned[0].handlers.close(0);
        await pending;
      },
    });

    expect(await killTracked(kernel.ctx, "com.test.aggregate")).toEqual([]);
    kernel.dispose();
  });

  it("exec 聚合输出 = 行事件原样拼接（行事件自带换行终止符，不再补 \\n）", async () => {
    spawned.length = 0;
    const kernel = createKernel();
    let result: { code: number | null; stdout: string; stderr: string } | undefined;

    await mountPlugin(kernel, {
      id: "com.test.exec-raw",
      apply: async (ctx: Context) => {
        const pending = ctx.process.exec({ command: "sh", args: ["-c", "data"] });
        // Rust 侧 read_line 的行事件自带换行终止符（services/shell.ts 契约），末行可能没有
        const handlers = spawned[0].handlers;
        handlers.stdout("hello\n");
        handlers.stdout("world\n");
        handlers.stdout("tail-without-terminator");
        handlers.stderr("boom\n");
        handlers.close(0);
        result = await pending;
      },
    });

    expect(result?.code).toBe(0);
    expect(result?.stdout).toBe("hello\nworld\ntail-without-terminator");
    expect(result?.stderr).toBe("boom\n");
    kernel.dispose();
  });

  it("插件停用后，其进程仍在册（由停用路径统一结束）", async () => {
    spawned.length = 0;
    const kernel = createKernel();
    await mountPlugin(kernel, {
      id: "com.test.longrun",
      apply: async (ctx: Context) => {
        await ctx.process.spawn({ command: "sh", args: ["-c", "serve"] });
      },
    });

    await unmountAll(kernel);

    // 登记不随 fiber 撤销自动消失（进程属插件而非 fiber）；结束由 stopPlugin 负责
    expect(await killTracked(kernel.ctx, "com.test.longrun")).toEqual([9000]);
    kernel.dispose();
  });

  it("缺调用方归属（非插件上下文）的 process 调用直接拒绝", async () => {
    const kernel = createKernel();
    const process = kernel.ctx.get("process" as never) as unknown as {
      spawn: (opts: { command: string }) => unknown;
      exec: (opts: { command: string }) => unknown;
    };
    // 进程记账按调用方插件归属：无归属的调用拒绝（同 state/storage/fs 口径）
    expect(() => process.spawn({ command: "sh" })).toThrow("只能在插件上下文中使用");
    expect(() => process.exec({ command: "sh" })).toThrow("只能在插件上下文中使用");
    kernel.dispose();
  });
});
