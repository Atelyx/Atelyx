/**
 * 内核能力层测试：平台判定（UA）与能力表逐项口径。
 * 桌面按端取舍（Windows 可应用内更新，Linux 改用下载页）；安卓 = 按端取舍，缺失即 false，
 * 不得回退桌面值。
 */
import { afterEach, describe, expect, it } from "vitest";
import { isAndroidPlatform, isLinuxPlatform, platformCapabilities } from "@/services/platform";

/** 临时替换 UA（node 环境无 WebView，显式注入模拟各端）；返回还原函数。 */
function stubUserAgent(ua: string | undefined): () => void {
  const descriptor = Object.getOwnPropertyDescriptor(globalThis, "navigator");
  Object.defineProperty(globalThis, "navigator", {
    value: ua === undefined ? undefined : { userAgent: ua },
    configurable: true,
  });
  return () => {
    if (descriptor) Object.defineProperty(globalThis, "navigator", descriptor);
    else delete (globalThis as { navigator?: unknown }).navigator;
  };
}

const WINDOWS_USER_AGENT =
  "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36 Edg/126.0";
const LINUX_USER_AGENT =
  "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36";

describe("内核能力层", () => {
  let restore: () => void = () => {};
  afterEach(() => restore());

  it("Windows = 全量能力，仅文件监听缺失", () => {
    restore = stubUserAgent(WINDOWS_USER_AGENT);
    expect(isAndroidPlatform()).toBe(false);
    expect(isLinuxPlatform()).toBe(false);
    expect(platformCapabilities()).toEqual({
      multiWindow: true,
      windowControls: true,
      directoryPicker: true,
      credentialStorage: true,
      autoLaunch: true,
      processExecution: true,
      fileWatching: false,
      inAppUpdate: true,
    });
  });

  it("Linux 桌面 = 同桌面，但不开应用内更新（安装语义因发行版而异）", () => {
    restore = stubUserAgent(LINUX_USER_AGENT);
    expect(isAndroidPlatform()).toBe(false);
    expect(isLinuxPlatform()).toBe(true);
    expect(platformCapabilities()).toEqual({
      multiWindow: true,
      windowControls: true,
      directoryPicker: true,
      credentialStorage: true,
      autoLaunch: true,
      processExecution: true,
      fileWatching: false,
      inAppUpdate: false,
    });
  });

  it("安卓 = 按端取舍：单窗口、无窗口控制/目录选择器/开机自启/进程执行，但可应用内更新", () => {
    restore = stubUserAgent(
      "Mozilla/5.0 (Linux; Android 15; Pixel 9) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Mobile Safari/537.36",
    );
    expect(isAndroidPlatform()).toBe(true);
    // 安卓 UA 同样含 Linux 标识，不得被误判成 Linux 桌面
    expect(isLinuxPlatform()).toBe(false);
    expect(platformCapabilities()).toEqual({
      multiWindow: false,
      windowControls: false,
      directoryPicker: false,
      credentialStorage: true,
      autoLaunch: false,
      processExecution: false,
      fileWatching: false,
      inAppUpdate: true,
    });
  });

  it("UA 不可得（非 WebView 环境）按桌面兜底", () => {
    restore = stubUserAgent(undefined);
    expect(isAndroidPlatform()).toBe(false);
    expect(isLinuxPlatform()).toBe(false);
    expect(platformCapabilities().multiWindow).toBe(true);
  });
});
