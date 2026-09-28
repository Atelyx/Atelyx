/**
 * 插件宿主平台探测测试：安卓 WebView 的 UA 形如 `(Linux; Android 15; …)`，
 * 内含 "Linux" 字样，必须先于 linux 判定，否则安卓会被误判成 linux-x64。
 */
import { afterEach, describe, expect, it } from "vitest";
import { ANDROID_PLATFORM, detectPlatform } from "@/utils/pluginHost";

/** 临时替换 UA（node 环境无 WebView，显式注入模拟各端）；返回还原函数。 */
function stubUserAgent(ua: string): () => void {
  const descriptor = Object.getOwnPropertyDescriptor(globalThis, "navigator");
  Object.defineProperty(globalThis, "navigator", { value: { userAgent: ua }, configurable: true });
  return () => {
    if (descriptor) Object.defineProperty(globalThis, "navigator", descriptor);
    else delete (globalThis as { navigator?: unknown }).navigator;
  };
}

describe("detectPlatform", () => {
  let restore: () => void = () => {};
  afterEach(() => restore());

  it("安卓 UA 判为 android（不被其中的 Linux 抢先判成 linux-x64）", () => {
    restore = stubUserAgent(
      "Mozilla/5.0 (Linux; Android 15; Pixel 9) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Mobile Safari/537.36",
    );
    expect(detectPlatform()).toBe(ANDROID_PLATFORM);
  });

  it("桌面两端与未知平台", () => {
    restore = stubUserAgent(
      "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36",
    );
    expect(detectPlatform()).toBe("windows-x64");
    restore = stubUserAgent(
      "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36",
    );
    expect(detectPlatform()).toBe("linux-x64");
    restore = stubUserAgent("SomeUnknown/1.0");
    expect(detectPlatform()).toBe("unknown");
  });
});
