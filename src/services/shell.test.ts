/**
 * 进程执行 service（services/shell）的 IPC 契约测试。
 *
 * `runProcess` 是插件进程启动的唯一前端出口，走宿主命令 `spawn_plugin_process`（不再是
 * tauri-plugin-shell 的 Command）：命令名、入参形状与事件名映射任一漂移都会让插件的进程
 * 静默起不来或收不到退出，故在此锁定。
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

/** 记录 invoke 调用；Channel 替身只保留 onmessage 供测试驱动事件。 */
const invokes: Array<{ command: string; args: Record<string, unknown> }> = [];
const channels: ChannelStub[] = [];
/** 置位后下一次 invoke 以该原因 reject（模拟启动失败）。 */
let nextFailure: string | null = null;

interface ChannelStub {
  onmessage: ((payload: unknown) => void) | null;
}

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (command: string, args: Record<string, unknown>) => {
    invokes.push({ command, args });
    if (nextFailure !== null) {
      const message = nextFailure;
      nextFailure = null;
      return Promise.reject(new Error(message));
    }
    return Promise.resolve(4321);
  },
  Channel: class {
    onmessage: ((payload: unknown) => void) | null = null;
    constructor() {
      channels.push(this as unknown as ChannelStub);
    }
  },
}));

vi.mock("@tauri-apps/plugin-shell", () => ({
  open: () => Promise.resolve(),
}));

import { runProcess, killProcessTree, type ProcessStreamHandlers } from "./shell";

/** 收集回调调用的测试替身。 */
function recorder(): ProcessStreamHandlers & {
  stdoutLines: string[];
  stderrLines: string[];
  codes: Array<number | null>;
  errors: string[];
} {
  const stdoutLines: string[] = [];
  const stderrLines: string[] = [];
  const codes: Array<number | null> = [];
  const errors: string[] = [];
  return {
    stdoutLines,
    stderrLines,
    codes,
    errors,
    stdout: (line) => stdoutLines.push(line),
    stderr: (line) => stderrLines.push(line),
    close: (code) => codes.push(code),
    error: (message) => errors.push(message),
  };
}

describe("进程执行 service", () => {
  beforeEach(() => {
    invokes.length = 0;
    channels.length = 0;
    nextFailure = null;
  });

  it("走宿主 spawn_plugin_process，并把事件名映射到对应回调", async () => {
    const handlers = recorder();
    const pid = await runProcess("cmd.exe", ["/C", "echo hi"], { cwd: "E:\\x" }, handlers);

    expect(pid).toBe(4321);
    expect(invokes).toHaveLength(1);
    expect(invokes[0].command).toBe("spawn_plugin_process");
    expect(invokes[0].args).toMatchObject({
      program: "cmd.exe",
      args: ["/C", "echo hi"],
      cwd: "E:\\x",
      env: null,
    });

    channels[0].onmessage!({ event: "stdout", data: "hi\n" });
    channels[0].onmessage!({ event: "stderr", data: "warn\n" });
    channels[0].onmessage!({ event: "terminated", code: 0 });
    channels[0].onmessage!({ event: "terminated", code: null });
    channels[0].onmessage!({ event: "error", message: "读取进程输出失败" });

    expect(handlers.stdoutLines).toEqual(["hi\n"]);
    expect(handlers.stderrLines).toEqual(["warn\n"]);
    // code 为 null = 被信号终止（Unix），须原样透传而不是当成 0
    expect(handlers.codes).toEqual([0, null]);
    expect(handlers.errors).toEqual(["读取进程输出失败"]);
  });

  it("env 原样转发；未给 cwd/env 时传 null（缺省字段与「显式无值」在命令侧同义）", async () => {
    await runProcess("sh", ["-c", "true"], { env: { A: "1", PATH: "/x" } }, recorder());
    expect(invokes[0].args).toMatchObject({ cwd: null, env: { A: "1", PATH: "/x" } });

    invokes.length = 0;
    await runProcess("sh", ["-c", "true"], {}, recorder());
    expect(invokes[0].args).toMatchObject({ cwd: null, env: null });
  });

  it("启动失败同时走 reject 与 handlers.error（流式面与返回值同口径）", async () => {
    const handlers = recorder();
    nextFailure = "不允许启动程序「python.exe」";

    await expect(runProcess("python.exe", [], {}, handlers)).rejects.toThrow("不允许启动程序");
    // error 回调与 reject 同因：调用方只看流式面时不会漏掉启动失败
    expect(handlers.errors).toEqual(["不允许启动程序「python.exe」"]);
  });

  it("killProcessTree 按 pid 调命令", async () => {
    await killProcessTree(99);
    expect(invokes[0]).toMatchObject({ command: "kill_process_tree", args: { pid: 99 } });
  });
});
