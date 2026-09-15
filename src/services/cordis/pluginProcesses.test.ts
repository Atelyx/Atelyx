/**
 * 插件进程登记表测试（services/cordis/pluginProcesses）。
 *
 * 纯表行为：登记/摘除/按插件结束/未挂载收尾，以及失败逐个隔离（一个 pid 失败不放弃其余）。
 * 登记表按内核上下文隔离——用一个普通对象当内核根上下文即可直测。
 */
import { describe, expect, it, vi } from "vitest";
import {
  killPluginProcesses,
  killUnmountedPluginProcesses,
  trackPluginProcess,
  untrackPluginProcess,
} from "./pluginProcesses";

describe("插件进程登记表", () => {
  it("结束该插件的全部在册进程并清空登记", async () => {
    const kernelCtx = {};
    trackPluginProcess(kernelCtx, "com.test.a", 101);
    trackPluginProcess(kernelCtx, "com.test.a", 102);
    trackPluginProcess(kernelCtx, "com.test.b", 201);

    const kill = vi.fn<(pid: number) => Promise<void>>(async () => {});
    const outcome = await killPluginProcesses(kernelCtx, "com.test.a", kill);

    expect(kill.mock.calls.map((c) => c[0]).sort()).toEqual([101, 102]);
    expect(outcome).toEqual({ killed: 2, failed: [] });
    // 已清空：再次结束为 no-op（且不再发起结束调用）
    const again = await killPluginProcesses(kernelCtx, "com.test.a", kill);
    expect(again).toEqual({ killed: 0, failed: [] });
    expect(kill).toHaveBeenCalledTimes(2);
  });

  it("未登记的插件结束为 no-op（不发结束调用）", async () => {
    const kill = vi.fn<(pid: number) => Promise<void>>(async () => {});
    const outcome = await killPluginProcesses({}, "com.test.unknown", kill);
    expect(outcome).toEqual({ killed: 0, failed: [] });
    expect(kill).not.toHaveBeenCalled();
  });

  it("摘除后的 pid 不再被结束（防 pid 复用误杀）", async () => {
    const kernelCtx = {};
    trackPluginProcess(kernelCtx, "com.test.a", 101);
    untrackPluginProcess(kernelCtx, "com.test.a", 101);

    const kill = vi.fn<(pid: number) => Promise<void>>(async () => {});
    await killPluginProcesses(kernelCtx, "com.test.a", kill);
    expect(kill).not.toHaveBeenCalled();
  });

  it("单个 pid 结束失败不中断其余，失败原因带回且登记照样清空", async () => {
    const kernelCtx = {};
    trackPluginProcess(kernelCtx, "com.test.a", 101);
    trackPluginProcess(kernelCtx, "com.test.a", 102);

    const kill = vi.fn(async (pid: number) => {
      if (pid === 101) throw new Error("拒绝访问");
    });
    const outcome = await killPluginProcesses(kernelCtx, "com.test.a", kill);

    expect(kill).toHaveBeenCalledTimes(2);
    expect(outcome.killed).toBe(1);
    expect(outcome.failed).toEqual([{ pid: 101, message: "拒绝访问" }]);
    // 失败也清空：留着会让下次重载反复尝试同一个结束不了的 pid
    const again = await killPluginProcesses(kernelCtx, "com.test.a", kill);
    expect(again.killed).toBe(0);
  });

  it("未挂载插件收尾只结束不在挂载集里的插件", async () => {
    const kernelCtx = {};
    trackPluginProcess(kernelCtx, "com.test.mounted", 301);
    trackPluginProcess(kernelCtx, "com.test.stopped", 401);
    trackPluginProcess(kernelCtx, "com.test.failed", 501);

    const kill = vi.fn<(pid: number) => Promise<void>>(async () => {});
    const failures = await killUnmountedPluginProcesses(kernelCtx, ["com.test.mounted"], kill);

    expect(kill.mock.calls.map((c) => c[0]).sort()).toEqual([401, 501]);
    expect(failures.size).toBe(0);
    // 已挂载插件的进程不受影响
    const mountedLeft = await killPluginProcesses(kernelCtx, "com.test.mounted", kill);
    expect(mountedLeft.killed).toBe(1);
  });

  it("未挂载收尾把失败按插件归集", async () => {
    const kernelCtx = {};
    trackPluginProcess(kernelCtx, "com.test.failed", 501);

    const kill = vi.fn(async () => {
      throw new Error("进程仍在运行");
    });
    const failures = await killUnmountedPluginProcesses(kernelCtx, [], kill);
    expect([...failures.keys()]).toEqual(["com.test.failed"]);
    expect(failures.get("com.test.failed")?.failed).toEqual([{ pid: 501, message: "进程仍在运行" }]);
  });

  it("不同内核上下文的登记互不可见", async () => {
    const kernelA = {};
    const kernelB = {};
    trackPluginProcess(kernelA, "com.test.a", 101);
    trackPluginProcess(kernelB, "com.test.a", 201);

    const kill = vi.fn<(pid: number) => Promise<void>>(async () => {});
    await killPluginProcesses(kernelA, "com.test.a", kill);
    expect(kill.mock.calls.map((c) => c[0])).toEqual([101]);
  });
});
