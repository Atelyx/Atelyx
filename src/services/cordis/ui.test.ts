/**
 * 宿主侧 UI 贡献注册表测试（services/cordis/ui）。
 * 覆盖：设置项/应用页/命令/主题设置项注册/读取/按插件撤销 + 变更通知；
 * 节点/边/表格视图现走 slots 注册表（single 胜出），此处覆盖 ui 键值平面 + slot 读取集成。
 */
import { describe, expect, it, afterEach } from "vitest";
import {
  getPluginAppPages,
  getPluginCommands,
  getPluginNodes,
  getPluginSettings,
  getPluginTableViews,
  getPluginThemeSettings,
  onPluginUiChange,
  registerPluginAppPage,
  registerPluginCommand,
  registerPluginSetting,
  registerPluginThemeSetting,
  unregisterPluginUi,
} from "./ui";
import { registerNodeSlot, registerTableViewSlot, unregisterSlot } from "./slots";

const slotIds: string[] = [];
afterEach(() => {
  for (const id of slotIds) unregisterSlot(id);
  slotIds.length = 0;
});

describe("宿主 UI 贡献注册表", () => {
  it("设置项注册/读取/按插件撤销", () => {
    registerPluginSetting("com.test.a", "com.test.a.key", "我的设置", () => null);
    // 设置页 tab 以注册时的裸 key 并入左侧栏，据该 key 取回组件（不得按实现内部复合键断言）
    const tab = getPluginSettings().find((t) => t.key === "com.test.a.key");
    expect(tab?.label).toBe("我的设置");
    expect(tab?.component).not.toBeNull();

    unregisterPluginUi("com.test.a");
    expect(getPluginSettings().find((t) => t.key === "com.test.a.key")).toBeUndefined();
    expect(getPluginSettings()).toHaveLength(0);
  });

  it("应用页/命令注册与读取", () => {
    registerPluginAppPage("com.test.a", "com.test.a.app", "应用页", () => null);
    registerPluginCommand("com.test.a", "say", "打招呼", () => "hi");
    expect(getPluginAppPages()).toHaveLength(1);
    expect(getPluginCommands()).toHaveLength(1);
    expect(getPluginCommands()[0]).toMatchObject({ pluginId: "com.test.a", id: "say", label: "打招呼" });

    unregisterPluginUi("com.test.a");
    expect(getPluginAppPages()).toHaveLength(0);
    expect(getPluginCommands()).toHaveLength(0);
  });

  it("主题设置项注册/读取", () => {
    registerPluginThemeSetting("com.test.t", "accent", "强调色", () => null);
    expect(getPluginThemeSettings("com.test.t")).toHaveLength(1);
    expect(getPluginThemeSettings("other")).toHaveLength(0);

    unregisterPluginUi("com.test.t");
    expect(getPluginThemeSettings("com.test.t")).toHaveLength(0);
  });

  it("onPluginUiChange 变更通知", () => {
    let notified = 0;
    const off = onPluginUiChange(() => {
      notified += 1;
    });
    registerPluginSetting("com.test.b", "com.test.b.key", "B", () => null);
    expect(notified).toBe(1);
    unregisterPluginUi("com.test.b");
    expect(notified).toBe(2);
    off();
  });

  it("节点/边/表格视图槽经 slots 注册表读取（single 胜出）", () => {
    const off = registerNodeSlot("conversation", "com.test.n", (() => null) as never, { priority: 5 });
    slotIds.push("com.test.n:node/conversation");
    const off2 = registerTableViewSlot("tl", "com.test.t", { label: "时间线", component: (() => null) as never }, { priority: 0 });
    slotIds.push("com.test.t:tableview/tl");
    expect(getPluginNodes().map((n) => n.type)).toContain("conversation");
    expect(getPluginTableViews().map((t) => t.kind)).toContain("tl");
    off();
    off2();
  });
});
