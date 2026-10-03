/**
 * 开机自启 service 测试：安卓无 autostart 插件 → 读取恒 false、写入 no-op（不触碰插件）；
 * 桌面 → 读取与开关透传插件（enable/disable/isEnabled）。
 */
import { afterEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  enable: vi.fn(async () => {}),
  disable: vi.fn(async () => {}),
  isEnabled: vi.fn(async () => false),
}));

vi.mock("@tauri-apps/plugin-autostart", () => ({
  enable: mocks.enable,
  disable: mocks.disable,
  isEnabled: mocks.isEnabled,
}));

import { isAutoLaunchEnabled, setAutoLaunchEnabled } from "@/services/autostart";

/** 临时替换 UA（node 环境无 WebView，显式注入模拟各端）；返回还原函数。 */
function stubUserAgent(ua: string): () => void {
  const descriptor = Object.getOwnPropertyDescriptor(globalThis, "navigator");
  Object.defineProperty(globalThis, "navigator", { value: { userAgent: ua }, configurable: true });
  return () => {
    if (descriptor) Object.defineProperty(globalThis, "navigator", descriptor);
    else delete (globalThis as { navigator?: unknown }).navigator;
  };
}

const ANDROID_UA =
  "Mozilla/5.0 (Linux; Android 15; Pixel 9) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Mobile Safari/537.36";
const DESKTOP_UA =
  "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36";

describe("开机自启 service", () => {
  let restore: () => void = () => {};
  afterEach(() => {
    restore();
    vi.clearAllMocks();
  });

  it("安卓：不触碰 autostart 插件（读取恒 false、写入 no-op）", async () => {
    restore = stubUserAgent(ANDROID_UA);
    expect(await isAutoLaunchEnabled()).toBe(false);
    await setAutoLaunchEnabled(true);
    await setAutoLaunchEnabled(false);
    expect(mocks.isEnabled).not.toHaveBeenCalled();
    expect(mocks.enable).not.toHaveBeenCalled();
    expect(mocks.disable).not.toHaveBeenCalled();
  });

  it("桌面：读取与开关透传插件", async () => {
    restore = stubUserAgent(DESKTOP_UA);
    mocks.isEnabled.mockResolvedValueOnce(true);
    expect(await isAutoLaunchEnabled()).toBe(true);
    await setAutoLaunchEnabled(true);
    await setAutoLaunchEnabled(false);
    expect(mocks.enable).toHaveBeenCalledTimes(1);
    expect(mocks.disable).toHaveBeenCalledTimes(1);
  });
});
