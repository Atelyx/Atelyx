/**
 * 更新 service（services/updater）的分派与事件契约测试。
 *
 * 三端分派（应用内下载 / 打开下载页）、下载入参形状、进度事件映射、取消与安装前落盘顺序任一漂移
 * 都会让更新静默失效或多下几十 MB，故在此锁定。
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

const harness = vi.hoisted(() => ({
  invokes: [] as Array<{ command: string; args: Record<string, unknown> }>,
  channels: [] as Array<{ onmessage: ((payload: unknown) => void) | null }>,
  /** 命令与调用方回调的全局时序（用于断言「安装前落盘」的先后）。 */
  log: [] as string[],
  platform: { android: false, inAppUpdate: true },
  canInstallPackages: true,
  /** download_update_package 的返回：null = 用户取消。 */
  downloadResult: "/data/updates/Atelyx_0.5.8_x64-setup.exe" as string | null,
  openedUrls: [] as string[],
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (command: string, args: Record<string, unknown>) => {
    harness.invokes.push({ command, args });
    harness.log.push(command);
    if (command === "download_update_package") return Promise.resolve(harness.downloadResult);
    if (command === "android_can_install_packages") return Promise.resolve(harness.canInstallPackages);
    return Promise.resolve(undefined);
  },
  Channel: class {
    onmessage: ((payload: unknown) => void) | null = null;
    constructor() {
      harness.channels.push(this);
    }
  },
}));

vi.mock("@/services/platform", () => ({
  isAndroidPlatform: () => harness.platform.android,
  platformCapabilities: () => ({ inAppUpdate: harness.platform.inAppUpdate }),
}));

vi.mock("@/services/app", () => ({ getAppVersion: () => Promise.resolve("0.5.7") }));

vi.mock("@/services/shell", () => ({
  openUrl: (url: string) => {
    harness.openedUrls.push(url);
    harness.log.push("openUrl");
    return Promise.resolve();
  },
}));

import { cancelUpdateDownload, installUpdate } from "./index";

const RELEASE = {
  tag_name: "v0.5.8",
  html_url: "https://github.com/Atelyx/Atelyx/releases/tag/v0.5.8",
  assets: [
    {
      name: "Atelyx_0.5.8_x64-setup.exe",
      browser_download_url: "https://example.com/Atelyx_0.5.8_x64-setup.exe",
      digest: "sha256:abc",
    },
    {
      name: "Atelyx_0.5.8.apk",
      browser_download_url: "https://example.com/Atelyx_0.5.8.apk",
      digest: "sha256:def",
    },
  ],
};

/** 安装流程记录器（进度 / 校验 / 安装前落盘）。 */
function recorder() {
  const progress: Array<[number, number | null]> = [];
  const verifying: string[] = [];
  const handlers = {
    progress,
    verifying,
    onProgress: (received: number, total: number | null) => progress.push([received, total]),
    onVerifying: () => verifying.push("verifying"),
    onBeforeInstall: () => {
      harness.log.push("flush");
      return Promise.resolve();
    },
  };
  return handlers;
}

describe("更新 service", () => {
  beforeEach(() => {
    harness.invokes.length = 0;
    harness.channels.length = 0;
    harness.log.length = 0;
    harness.openedUrls.length = 0;
    harness.platform = { android: false, inAppUpdate: true };
    harness.canInstallPackages = true;
    harness.downloadResult = "/data/updates/Atelyx_0.5.8_x64-setup.exe";
    vi.stubGlobal(
      "fetch",
      vi.fn(() => Promise.resolve({ ok: true, json: () => Promise.resolve(RELEASE) })),
    );
  });

  it("Windows：下载后拉起安装器，进度事件映射为续传起点与累计字节", async () => {
    const handlers = recorder();
    const outcome = await installUpdate(handlers);

    expect(outcome).toBe("started");
    expect(harness.invokes.map((call) => call.command)).toEqual([
      "download_update_package",
      "install_downloaded_update",
    ]);
    // 安装包取自 -setup.exe 资产，摘要原样透传（Rust 侧据此校验）
    expect(harness.invokes[0].args).toMatchObject({
      url: "https://example.com/Atelyx_0.5.8_x64-setup.exe",
      fileName: "Atelyx_0.5.8_x64-setup.exe",
      sha256: "sha256:abc",
    });
    expect(harness.invokes[0].args.onEvent).toBe(harness.channels[0]);

    harness.channels[0].onmessage!({ event: "started", total: 1000, resumingFrom: 400 });
    harness.channels[0].onmessage!({ event: "progress", received: 700, total: 1000 });
    harness.channels[0].onmessage!({ event: "verifying" });

    expect(handlers.progress).toEqual([
      [400, 1000],
      [700, 1000],
    ]);
    expect(handlers.verifying).toEqual(["verifying"]);
  });

  it("安装前落盘排在拉起安装器之前（下载期间的编辑不能被安装器吃掉）", async () => {
    await installUpdate(recorder());
    expect(harness.log).toEqual([
      "download_update_package",
      "flush",
      "install_downloaded_update",
    ]);
  });

  it("用户取消：返回 cancelled 且不拉起安装器", async () => {
    harness.downloadResult = null;
    const outcome = await installUpdate(recorder());

    expect(outcome).toBe("cancelled");
    expect(harness.invokes.map((call) => call.command)).toEqual(["download_update_package"]);
  });

  it("Linux：不下载，直接打开下载页", async () => {
    harness.platform = { android: false, inAppUpdate: false };
    const outcome = await installUpdate(recorder());

    expect(outcome).toBe("started");
    expect(harness.invokes).toHaveLength(0);
    expect(harness.openedUrls).toEqual(["https://github.com/Atelyx/Atelyx/releases/tag/v0.5.8"]);
  });

  it("安卓未授权「安装未知应用」：引导设置页并报错，不下载", async () => {
    harness.platform = { android: true, inAppUpdate: true };
    harness.canInstallPackages = false;

    await expect(installUpdate(recorder())).rejects.toThrow("安装未知应用");
    expect(harness.invokes.map((call) => call.command)).toEqual([
      "android_can_install_packages",
      "android_request_install_permission",
    ]);
  });

  it("安卓已授权：下载 APK 后交系统安装器", async () => {
    harness.platform = { android: true, inAppUpdate: true };
    harness.downloadResult = "/data/updates/Atelyx_0.5.8.apk";

    const outcome = await installUpdate(recorder());

    expect(outcome).toBe("started");
    expect(harness.invokes.map((call) => call.command)).toEqual([
      "android_can_install_packages",
      "download_update_package",
      "android_install_apk",
    ]);
    expect(harness.invokes[1].args).toMatchObject({
      fileName: "Atelyx_0.5.8.apk",
      sha256: "sha256:def",
    });
    expect(harness.invokes[2].args).toEqual({ path: "/data/updates/Atelyx_0.5.8.apk" });
  });

  it("已是最新版本：抛错且不发任何命令", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(() =>
        Promise.resolve({
          ok: true,
          json: () => Promise.resolve({ ...RELEASE, tag_name: "v0.5.7" }),
        }),
      ),
    );
    await expect(installUpdate(recorder())).rejects.toThrow("已是最新版本");
    expect(harness.invokes).toHaveLength(0);
  });

  it("检查失败：HTTP 非 2xx 原样上报", async () => {
    vi.stubGlobal("fetch", vi.fn(() => Promise.resolve({ ok: false, status: 503 })));
    await expect(installUpdate(recorder())).rejects.toThrow("HTTP 503");
  });

  it("取消下载走 cancel_update_download 命令", async () => {
    await cancelUpdateDownload();
    expect(harness.invokes.map((call) => call.command)).toEqual(["cancel_update_download"]);
  });
});
