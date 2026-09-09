/**
 * 宿主侧 UI 贡献注册表测试（services/cordis/ui）。
 * 覆盖：表格视图注册/读取/按插件撤销 + 变更通知（其余注册点由 ctx.slots 扩展时覆盖）。
 */
import { describe, expect, it } from "vitest";
import {
  getPluginTableView,
  getPluginTableViews,
  onPluginUiChange,
  registerPluginTableView,
  unregisterPluginUi,
} from "./ui";

describe("宿主 UI 贡献注册表", () => {
  it("表格视图注册/读取/按插件撤销", () => {
    registerPluginTableView("com.test.a", "com.test.a.tl", "时间线", () => null);
    expect(getPluginTableView("com.test.a.tl")?.label).toBe("时间线");
    expect(getPluginTableViews().map((t) => t.kind)).toContain("com.test.a.tl");

    unregisterPluginUi("com.test.a");
    expect(getPluginTableView("com.test.a.tl")).toBeUndefined();
    expect(getPluginTableViews()).toHaveLength(0);
  });

  it("onPluginUiChange 变更通知", () => {
    let notified = 0;
    const off = onPluginUiChange(() => {
      notified += 1;
    });
    registerPluginTableView("com.test.b", "com.test.b.tl", "时间线", () => null);
    expect(notified).toBe(1);
    unregisterPluginUi("com.test.b");
    expect(notified).toBe(2);
    off();
  });
});
