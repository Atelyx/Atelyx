/**
 * 窗口控制 service 测试：关窗守卫两种收尾（destroy = 撕裂窗口销毁 / hideAll = 主窗口
 * 驻留托盘）的时序与副作用，托盘退出请求的注册与回报；移动端（windowControls 缺失）一律 no-op。
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  windowControls: { value: true },
  destroy: vi.fn(async () => {}),
  onCloseRequested: vi.fn(),
  invoke: vi.fn(async () => null),
  listen: vi.fn(async (_event?: string, _cb?: () => void) => () => {}),
}));

vi.mock("@/services/platform", () => ({
  platformCapabilities: () => ({ windowControls: mocks.windowControls.value }),
}));

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    onCloseRequested: mocks.onCloseRequested,
    destroy: mocks.destroy,
  }),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: mocks.invoke,
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: mocks.listen,
}));

/** 按能力表取值重新求值模块（WINDOW_CONTROLS 是模块级常量，导入时定格）。 */
async function loadSvc(windowControls: boolean) {
  mocks.windowControls.value = windowControls;
  vi.resetModules();
  return await import("@/services/window");
}

/** 安装关窗监听捕获：记录注册进 tauri 的关闭请求回调。 */
function stubCloseRegistration(): { captured: (event: { preventDefault: () => void }) => Promise<void> } {
  const box: { captured?: (event: { preventDefault: () => void }) => Promise<void> } = {};
  mocks.onCloseRequested.mockImplementation(async (cb: typeof box.captured) => {
    box.captured = cb;
    return () => {};
  });
  return {
    get captured() {
      return box.captured!;
    },
  };
}

describe("窗口控制 service", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.windowControls.value = true;
  });

  it("destroy 模式（撕裂窗口）：阻止默认关闭 → 回调完成 → 销毁窗口", async () => {
    const svc = await loadSvc(true);
    const reg = stubCloseRegistration();
    let handlerDone = false;
    const unlisten = await svc.onCloseRequested(async () => {
      // 回调进行中不得提前销毁（防 debounce 窗口内丢改动）
      expect(mocks.destroy).not.toHaveBeenCalled();
      handlerDone = true;
    });
    expect(typeof unlisten).toBe("function");

    const preventDefault = vi.fn();
    await reg.captured({ preventDefault });
    expect(preventDefault).toHaveBeenCalledTimes(1);
    expect(handlerDone).toBe(true);
    expect(mocks.destroy).toHaveBeenCalledTimes(1);
    expect(mocks.invoke).not.toHaveBeenCalled();
  });

  it("hideAll 模式（主窗口驻留托盘）：回调完成后隐藏全部窗口，不销毁", async () => {
    const svc = await loadSvc(true);
    const reg = stubCloseRegistration();
    await svc.onCloseRequested(async () => {}, "hideAll");

    const preventDefault = vi.fn();
    await reg.captured({ preventDefault });
    expect(preventDefault).toHaveBeenCalledTimes(1);
    expect(mocks.destroy).not.toHaveBeenCalled();
    expect(mocks.invoke).toHaveBeenCalledWith("hide_to_tray");
  });

  it("hideAll 模式：hide_to_tray 调用失败不向上抛（回调 finally 内消化）", async () => {
    const svc = await loadSvc(true);
    const reg = stubCloseRegistration();
    mocks.invoke.mockRejectedValueOnce(new Error("ipc 失败"));
    await svc.onCloseRequested(async () => {}, "hideAll");
    await expect(reg.captured({ preventDefault: vi.fn() })).resolves.toBeUndefined();
  });

  it("托盘退出请求：按事件名注册监听，事件到达执行收尾回调；回报经 exit_flush_done", async () => {
    const svc = await loadSvc(true);
    const box: { cb?: () => void } = {};
    mocks.listen.mockImplementationOnce(async (_event?: string, cb?: () => void) => {
      box.cb = cb;
      return () => {};
    });
    const handler = vi.fn(async () => {
      await svc.ackExitFlushDone();
    });
    svc.onTrayExitRequested(handler);

    expect(mocks.listen).toHaveBeenCalledWith("atelyx:tray-exit-requested", expect.any(Function));
    box.cb!();
    await vi.waitFor(() => expect(handler).toHaveBeenCalledTimes(1));
    await vi.waitFor(() => expect(mocks.invoke).toHaveBeenCalledWith("exit_flush_done"));
  });

  it("移动端（无窗口控制）：关窗与托盘退出注册一律 no-op", async () => {
    const svc = await loadSvc(false);
    const unlisten = await svc.onCloseRequested(async () => {});
    expect(typeof unlisten).toBe("function");
    expect(mocks.onCloseRequested).not.toHaveBeenCalled();

    const off = svc.onTrayExitRequested(async () => {});
    expect(typeof off).toBe("function");
    expect(mocks.listen).not.toHaveBeenCalled();

    await svc.ackExitFlushDone();
    expect(mocks.invoke).not.toHaveBeenCalled();
  });
});
