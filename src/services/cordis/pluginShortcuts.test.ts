/**
 * 插件全局快捷键登记表测试（services/cordis/pluginShortcuts）：纯表行为——登记/摘除/分发命中与异常隔离/按插件释放/未挂载收尾，
 * 以及在途注册的处理（注册落地前停用须先等落地再注销，不漏键）。
 * 登记表按内核上下文隔离——用一个普通对象当内核根上下文即可直测。
 */
import { describe, expect, it, vi } from "vitest";
import {
  dispatchShortcutTrigger,
  releasePluginShortcuts,
  releaseUnmountedPluginShortcuts,
  trackPendingShortcutRegister,
  trackShortcut,
  untrackShortcut,
} from "./pluginShortcuts";

describe("插件全局快捷键登记表", () => {
  it("分发命中登记回调；未登记的键不命中", async () => {
    const kernelCtx = {};
    const handler = vi.fn();
    trackShortcut(kernelCtx, "com.test.a", "Shift+Alt+E", handler);

    expect(dispatchShortcutTrigger(kernelCtx, "Shift+Alt+E")).toBe(true);
    await vi.waitFor(() => expect(handler).toHaveBeenCalledTimes(1));

    expect(dispatchShortcutTrigger(kernelCtx, "Shift+Alt+X")).toBe(false);
  });

  it("回调抛错被隔离（不外冒未处理拒绝），其余键照常分发", async () => {
    const kernelCtx = {};
    const errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    trackShortcut(kernelCtx, "com.test.a", "KeyA", () => {
      throw new Error("boom");
    });
    const ok = vi.fn();
    trackShortcut(kernelCtx, "com.test.a", "KeyB", ok);

    expect(dispatchShortcutTrigger(kernelCtx, "KeyA")).toBe(true);
    await vi.waitFor(() => expect(errorSpy).toHaveBeenCalled());
    expect(dispatchShortcutTrigger(kernelCtx, "KeyB")).toBe(true);
    await vi.waitFor(() => expect(ok).toHaveBeenCalledTimes(1));
    errorSpy.mockRestore();
  });

  it("摘除后的键不再分发", () => {
    const kernelCtx = {};
    const handler = vi.fn();
    trackShortcut(kernelCtx, "com.test.a", "KeyA", handler);
    untrackShortcut(kernelCtx, "com.test.a", "KeyA");

    expect(dispatchShortcutTrigger(kernelCtx, "KeyA")).toBe(false);
  });

  it("释放等在途注册落地后才注销，不漏键", async () => {
    const kernelCtx = {};
    trackShortcut(kernelCtx, "com.test.a", "KeyA", () => {});
    let settle!: () => void;
    const inFlight = new Promise<void>((resolve) => {
      settle = resolve;
    });
    trackPendingShortcutRegister(kernelCtx, "com.test.a", inFlight);

    const release = vi.fn(async () => {});
    let outcome: Awaited<ReturnType<typeof releasePluginShortcuts>> | null = null;
    const releasing = releasePluginShortcuts(kernelCtx, "com.test.a", release).then((o) => {
      outcome = o;
    });
    // 在途注册未落地前，释放不得先动手（此刻键尚未在 OS 层注册成功，注销会被跳过）
    await Promise.resolve();
    expect(release).not.toHaveBeenCalled();

    settle();
    await releasing;
    expect(release).toHaveBeenCalledTimes(1);
    expect(outcome).toEqual({ released: 1, message: null });
    expect(dispatchShortcutTrigger(kernelCtx, "KeyA")).toBe(false);
  });

  it("本地登记为空也照常触达释放（跨窗口停用：OS 归属在别的窗口登记）", async () => {
    const kernelCtx = {};
    const failing = Promise.reject(new Error("已被占用"));
    trackPendingShortcutRegister(kernelCtx, "com.test.a", failing);

    const release = vi.fn(async () => {});
    const outcome = await releasePluginShortcuts(kernelCtx, "com.test.a", release);
    expect(outcome).toEqual({ released: 0, message: null });
    // Rust 侧释放幂等：本地没登记也要清 OS 归属（该插件的注册可能在别的窗口内核）
    expect(release).toHaveBeenCalledTimes(1);
    // 吞掉预期的拒绝，不冒未处理
    await failing.catch(() => {});
  });

  it("释放失败原因原样带回（不静默）", async () => {
    const kernelCtx = {};
    trackShortcut(kernelCtx, "com.test.a", "KeyA", () => {});

    const outcome = await releasePluginShortcuts(kernelCtx, "com.test.a", async () => {
      throw new Error("注销失败原因");
    });
    expect(outcome.released).toBe(1);
    expect(outcome.message).toBe("注销失败原因");
  });

  it("未挂载收尾只释放未挂载插件，已挂载的不动", async () => {
    const kernelCtx = {};
    trackShortcut(kernelCtx, "com.test.a", "KeyA", () => {});
    trackShortcut(kernelCtx, "com.test.b", "KeyB", () => {});

    const release = vi.fn(async () => {});
    const failures = await releaseUnmountedPluginShortcuts(kernelCtx, ["com.test.b"], release);
    expect(release).toHaveBeenCalledTimes(1);
    expect(release).toHaveBeenCalledWith("com.test.a");
    expect(failures.size).toBe(0);
    // 已挂载插件的键仍可分发
    expect(dispatchShortcutTrigger(kernelCtx, "KeyB")).toBe(true);
    expect(dispatchShortcutTrigger(kernelCtx, "KeyA")).toBe(false);
  });

  it("登记表按内核上下文隔离", () => {
    const kernelA = {};
    const kernelB = {};
    trackShortcut(kernelA, "com.test.a", "KeyA", () => {});

    expect(dispatchShortcutTrigger(kernelA, "KeyA")).toBe(true);
    expect(dispatchShortcutTrigger(kernelB, "KeyA")).toBe(false);
  });
});
