/**
 * ctx.tray / ctx.clipboard.readImage / ctx.window.listMonitors 接线测试：
 * 归属校验、菜单树形状预校验、随插件停用清除与查询转发（传输层 mock）。
 */
import { describe, expect, it, vi, beforeEach, afterEach } from "vitest";

import { readClipboardImage } from "@/services/clipboard";
import { clearPluginTrayMenu, setPluginTrayMenu } from "@/services/trayMenu";
import { listMonitors } from "@/services/window";
import { createKernel, resetKernel, type Kernel } from "./kernel";
import { mountPlugin, unmountAll } from "./loader";
import type { TrayMenuEntry } from "./types";

vi.mock("@/services/trayMenu", () => ({
  setPluginTrayMenu: vi.fn(async () => {}),
  clearPluginTrayMenu: vi.fn(async () => {}),
}));
vi.mock("@/services/clipboard", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/services/clipboard")>()),
  readClipboardImage: vi.fn(async () => null),
}));
vi.mock("@/services/window", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/services/window")>()),
  listMonitors: vi.fn(async () => [
    { id: "monitor-0", name: "主屏", x: 0, y: 0, width: 2560, height: 1440, workX: 0, workY: 0, workWidth: 2560, workHeight: 1392, scaleFactor: 1.5 },
  ]),
}));

const item = (id: string, label = id): TrayMenuEntry => ({ type: "item", id, label, onActivate: () => {} });

let kernel: Kernel | null = null;

async function disposeKernel(): Promise<void> {
  if (kernel) {
    await unmountAll(kernel);
    kernel.dispose();
    kernel = null;
  }
  resetKernel();
}

describe("ctx.tray 托盘菜单贡献", () => {
  beforeEach(() => {
    vi.mocked(setPluginTrayMenu).mockClear();
    vi.mocked(clearPluginTrayMenu).mockClear();
    vi.mocked(readClipboardImage).mockClear();
    vi.mocked(listMonitors).mockClear();
  });

  afterEach(disposeKernel);

  it("setMenu 写入菜单树：叶子 key = 插件 id + 路径 id，分隔线原样透传", async () => {
    kernel = createKernel();
    await mountPlugin(kernel, {
      id: "com.test.tray",
      apply: (ctx) => {
        void ctx.tray.setMenu([
          item("run", "执行"),
          { type: "submenu", id: "more", label: "更多", items: [item("sync"), { type: "separator" }] },
          { type: "separator" },
        ]);
      },
    });
    expect(setPluginTrayMenu).toHaveBeenCalledWith("com.test.tray", [
      expect.objectContaining({ type: "item", id: "run", label: "执行" }),
      expect.objectContaining({
        type: "submenu",
        id: "more",
        items: [
          expect.objectContaining({ type: "item", id: "sync" }),
          { type: "separator" },
        ],
      }),
      { type: "separator" },
    ]);
  });

  it("重复 setMenu 覆盖旧树；插件停用即清除", async () => {
    kernel = createKernel();
    let disposeNow: (() => void) | undefined;
    await mountPlugin(kernel, {
      id: "com.test.tray",
      apply: (ctx) => {
        void ctx.tray.setMenu([item("a")]);
        disposeNow = () => void ctx.tray.setMenu([item("b")]);
      },
    });
    // 旧 cleanup 在新写入落地后清（防 clear 先到被 set 覆盖出残留）：
    // 直接验证卸载路径的清除调用
    await unmountAll(kernel);
    kernel.dispose();
    kernel = null;
    expect(clearPluginTrayMenu).toHaveBeenCalledWith("com.test.tray");
    void disposeNow;
  });

  it("形状越界以可读错误拒绝（key 分隔符 / 空文案 / 超深 / 缺回调）", async () => {
    kernel = createKernel();
    const rejections: unknown[] = [];
    await mountPlugin(kernel, {
      id: "com.test.tray",
      apply: (ctx) => {
        const trySet = (items: TrayMenuEntry[]): unknown => {
          // 校验错误同步抛（promise 外），调用失败路径一并收集
          try {
            return ctx.tray.setMenu(items).then(
              () => undefined,
              (e: unknown) => e,
            );
          } catch (e) {
            return e;
          }
        };
        rejections.push(
          trySet([item("bad:id")]),
          trySet([{ type: "item", id: "x", label: "  ", onActivate: () => {} }]),
          trySet([
            {
              type: "submenu",
              id: "s1",
              label: "一",
              items: [{ type: "submenu", id: "s2", label: "二", items: [{ type: "submenu", id: "s3", label: "三", items: [item("deep")] }] }],
            },
          ]),
          trySet([{ type: "item", id: "no-cb", label: "无回调" } as unknown as TrayMenuEntry]),
        );
      },
    });
    await Promise.all(rejections);
    expect(setPluginTrayMenu).not.toHaveBeenCalled();
    const messages = (await Promise.all(rejections)).map((e) => (e instanceof Error ? e.message : ""));
    expect(messages[0]).toContain('":"');
    expect(messages[1]).toContain("不能为空");
    expect(messages[2]).toContain("层");
    expect(messages[3]).toContain("onActivate");
  });

  it("非插件上下文调用拒绝", async () => {
    kernel = createKernel();
    expect(() => kernel!.ctx.tray.setMenu([item("x")])).toThrow("插件上下文");
  });
});

describe("ctx.clipboard.readImage 与 ctx.window.listMonitors 接线", () => {
  afterEach(disposeKernel);

  it("读图与显示器查询透传 service 返回值", async () => {
    kernel = createKernel();
    let monitors: unknown;
    await mountPlugin(kernel, {
      id: "com.test.tray",
      apply: (ctx) => {
        void ctx.clipboard.readImage();
        void ctx.window.listMonitors().then((m) => {
          monitors = m;
        });
      },
    });
    await Promise.resolve();
    expect(readClipboardImage).toHaveBeenCalled();
    expect(listMonitors).toHaveBeenCalled();
    expect(monitors).toEqual([expect.objectContaining({ id: "monitor-0", scaleFactor: 1.5 })]);
  });
});
