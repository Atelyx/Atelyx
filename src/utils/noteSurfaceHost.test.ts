/**
 * 笔记正文编辑能力注册表纯函数测试（utils/noteSurfaceHost.ts）。
 * 覆盖未注册态、注册/撤销、后注册者生效与变更通知。注册表为模块级单例，每测试 vi.resetModules 隔离。
 */
import { describe, it, expect, beforeEach, vi } from "vitest";
import type { NoteSurfaceProvider } from "@/types/noteSurface";

type NoteSurfaceHost = typeof import("./noteSurfaceHost");
let host: NoteSurfaceHost;

const stub = (): NoteSurfaceProvider => ({
  open: () => {
    throw new Error("未预期调用");
  },
  get: () => null,
  isConflicted: () => false,
  close: () => {},
});

beforeEach(async () => {
  vi.resetModules();
  host = await import("./noteSurfaceHost");
});

describe("提供者注册", () => {
  it("未注册：查表为 null", () => {
    expect(host.getNoteSurface()).toBeNull();
  });

  it("注册后可取，撤销后回到 null", () => {
    const off = host.registerNoteSurface(stub());
    expect(host.getNoteSurface()).not.toBeNull();
    off();
    expect(host.getNoteSurface()).toBeNull();
  });

  it("后注册者生效；先前注册者的撤销不影响后来者", () => {
    const first = stub();
    const second = stub();
    const offFirst = host.registerNoteSurface(first);
    host.registerNoteSurface(second);
    offFirst();
    expect(host.getNoteSurface()).toBe(second);
  });
});

describe("变更通知", () => {
  it("注册与撤销各通知一次；退订后不再通知", () => {
    const listener = vi.fn();
    const offChange = host.onNoteSurfaceChange(listener);
    const off = host.registerNoteSurface(stub());
    off();
    expect(listener).toHaveBeenCalledTimes(2);
    offChange();
    host.registerNoteSurface(stub());
    expect(listener).toHaveBeenCalledTimes(2);
  });
});
