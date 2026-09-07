/**
 * 主线程 UI 平面注册逻辑测试（services/plugins/ui）。
 *
 * 覆盖：facade 挂载、注册收集（panel/setting/command/tableview）、按插件撤销、视图候选合并与显示名兜底、
 * 统一视图贡献注册表（内置/插件同表、kind 全局唯一与内置保留拒绝）、facade 仓库访问方法（provider 转发 + 未接线降级）、
 * 表格数据访问（subscribeTableData/selectTableRow/resolveTableImage 经 provider 转发 + 未接线降级）。
 * 仅测注册逻辑（无 DOM）：用最小 window stub；loadUiPlugin 的脚本注入路径不在本测试覆盖。
 */
import { describe, it, expect, beforeEach, vi } from "vitest";
import {
  exposePluginFacade,
  getPluginCommands,
  getPluginSetting,
  getPluginSettings,
  getPluginTableView,
  getPluginTableViews,
  getPluginVaultAccess,
  getViewContribution,
  pluginViewKinds,
  pluginViewLabel,
  registerBuiltinView,
  setPluginTableAccess,
  setPluginVaultAccess,
  unregisterPluginUi,
} from "./ui";
import type { PluginTableSnapshot, VaultAccess } from "@/types";

// node 环境无 window：模块顶层不触 window（仅 exposePluginFacade 在调用时访问），stub 即可。
(globalThis as { window?: unknown }).window = {};

const Comp = () => null;
const EMPTY_SNAP: PluginTableSnapshot = {
  tableFile: null,
  fields: [],
  rows: [],
  selectedRowId: null,
  peerColorByRowId: {},
};
const RUNTIME: VaultAccess = {
  listFiles: () => Promise.resolve([]),
  openCanvasFile: () => {},
  openNote: () => {},
  openTable: () => {},
};

beforeEach(() => {
  unregisterPluginUi("com.test.any");
  unregisterPluginUi("com.test.hello");
  unregisterPluginUi("com.test.a");
  unregisterPluginUi("com.test.b");
  window.__atelyxPlugin__ = undefined;
  setPluginTableAccess(null);
  setPluginVaultAccess(null);
});

describe("主线程平面 facade 与注册", () => {
  it("exposePluginFacade 幂等挂载", () => {
    exposePluginFacade();
    expect(window.__atelyxPlugin__).toBeDefined();
    exposePluginFacade();
    expect(window.__atelyxPlugin__).toBeDefined();
  });

  it("registerPanel/registerSetting/registerCommand 收集与读取", () => {
    exposePluginFacade();
    const bridge = window.__atelyxPlugin__!.forPlugin("com.test.hello");
    bridge.registerPanel({ kind: "com.test.hello.dashboard", label: "仪表盘", component: Comp });
    bridge.registerSetting({ key: "config", label: "配置", component: Comp });
    bridge.registerCommand({ id: "run", label: "运行", run: () => 1 });

    expect(getViewContribution("com.test.hello.dashboard")?.label).toBe("仪表盘");
    expect(getViewContribution("com.test.hello.dashboard")?.provider).toBe("plugin");
    expect(getPluginSettings().map((s) => s.key)).toContain("com.test.hello:config");
    expect(getPluginSetting("com.test.hello:config")?.label).toBe("配置");
    expect(getPluginCommands().map((c) => `${c.pluginId}:${c.id}`)).toContain("com.test.hello:run");
  });

  it("unregisterPluginUi 撤销某插件全部贡献", () => {
    exposePluginFacade();
    const a = window.__atelyxPlugin__!.forPlugin("com.test.a");
    const b = window.__atelyxPlugin__!.forPlugin("com.test.b");
    a.registerPanel({ kind: "com.test.a.p1", label: "A1", component: Comp });
    a.registerPanel({ kind: "com.test.a.p2", label: "A2", component: Comp });
    b.registerPanel({ kind: "com.test.b.p1", label: "B1", component: Comp });
    unregisterPluginUi("com.test.a");
    expect(getViewContribution("com.test.a.p1")).toBeUndefined();
    expect(getViewContribution("com.test.a.p2")).toBeUndefined();
    expect(getViewContribution("com.test.b.p1")?.label).toBe("B1");
  });

  it("视图候选合并与显示名兜底", () => {
    exposePluginFacade();
    expect(pluginViewKinds()).toContain("canvas"); // 内建保留
    window.__atelyxPlugin__!.forPlugin("com.test.hello").registerPanel({
      kind: "com.test.hello.panel",
      label: "我的面板",
      component: Comp,
    });
    expect(pluginViewKinds()).toContain("com.test.hello.panel");
    expect(pluginViewLabel("com.test.hello.panel")).toBe("我的面板");
    expect(pluginViewLabel("canvas")).toBe("画布");
    expect(pluginViewLabel("未知视图")).toBe("未知视图"); // 未知视图原样兜底
  });

  it("registerTableView 收集/读取/按插件撤销", () => {
    exposePluginFacade();
    window.__atelyxPlugin__!.forPlugin("com.test.a").registerTableView({
      kind: "com.test.a.tl",
      label: "时间线",
      component: Comp,
    });
    expect(getPluginTableView("com.test.a.tl")?.label).toBe("时间线");
    expect(getPluginTableViews().map((t) => t.kind)).toContain("com.test.a.tl");
    unregisterPluginUi("com.test.a");
    expect(getPluginTableView("com.test.a.tl")).toBeUndefined();
    expect(getPluginTableViews()).toHaveLength(0);
  });

  it("subscribeTableData 立即推一次 + 变更推 + 退订生效", () => {
    exposePluginFacade();
    const listeners = new Set<(snap: PluginTableSnapshot) => void>();
    setPluginTableAccess({
      subscribeSnapshot: (cb) => {
        listeners.add(cb);
        cb(EMPTY_SNAP); // 接线后立即推一次（宿主语义）
        return () => {
          listeners.delete(cb);
        };
      },
      selectRow: () => {},
      resolveImage: () => Promise.resolve("data:image/png;base64,x"),
    });
    const bridge = window.__atelyxPlugin__!.forPlugin("com.test.a");
    const calls: PluginTableSnapshot[] = [];
    const unsub = bridge.subscribeTableData((snap) => calls.push(snap));
    expect(calls).toHaveLength(1);
    for (const cb of [...listeners]) cb({ ...EMPTY_SNAP, tableFile: "a.atb", selectedRowId: "r1" });
    expect(calls).toHaveLength(2);
    unsub();
    for (const cb of [...listeners]) cb({ ...EMPTY_SNAP, tableFile: "a.atb", selectedRowId: "r2" });
    expect(calls).toHaveLength(2); // 退订后不再推
  });

  it("selectTableRow/resolveTableImage 委托 provider", async () => {
    exposePluginFacade();
    const selectRow = vi.fn();
    const resolveImage = vi.fn((entry: string) => Promise.resolve(`data:${entry}`));
    setPluginTableAccess({
      subscribeSnapshot: () => () => {},
      selectRow,
      resolveImage,
    });
    const bridge = window.__atelyxPlugin__!.forPlugin("com.test.a");
    bridge.selectTableRow("r1");
    bridge.selectTableRow(null);
    expect(selectRow).toHaveBeenNthCalledWith(1, "r1");
    expect(selectRow).toHaveBeenNthCalledWith(2, null);
    await expect(bridge.resolveTableImage("p.png")).resolves.toBe("data:p.png");
    expect(resolveImage).toHaveBeenCalledWith("p.png");
  });

  it("未接线时安全降级（订阅/选中 no-op、resolve 拒绝）", async () => {
    exposePluginFacade();
    const bridge = window.__atelyxPlugin__!.forPlugin("com.test.a");
    expect(() => bridge.subscribeTableData(() => {})()).not.toThrow();
    expect(() => bridge.selectTableRow("r1")).not.toThrow();
    await expect(bridge.resolveTableImage("x")).rejects.toThrow("插件表格访问未就绪");
  });

  it("内置视图贡献注册 + 统一读取（内置与插件面板同表）", () => {
    exposePluginFacade();
    // 内置贡献：kind "search" 与插件面板同表注册，provider 标注来源
    registerBuiltinView({ kind: "search", label: "搜索", component: Comp });
    const builtin = getViewContribution("search");
    expect(builtin?.provider).toBe("builtin");
    expect(builtin?.label).toBe("搜索");
    // 插件面板经 registerPanel 进同一注册表（provider = plugin）
    window.__atelyxPlugin__!.forPlugin("com.test.a").registerPanel({
      kind: "com.test.a.search",
      label: "搜索（插件）",
      component: Comp,
    });
    const plugin = getViewContribution("com.test.a.search");
    expect(plugin?.provider).toBe("plugin");
    expect(plugin?.pluginId).toBe("com.test.a");
    // 注册表驱动视图菜单与显示名（不同 kind 并列）
    expect(pluginViewKinds()).toContain("search");
    expect(pluginViewKinds()).toContain("com.test.a.search");
    expect(pluginViewLabel("com.test.a.search")).toBe("搜索（插件）");
  });

  it("kind 全局唯一 + 内置保留：重复注册与占用内置 kind 均拒绝", () => {
    exposePluginFacade();
    registerBuiltinView({ kind: "com.test.builtin.x", label: "内置 X", component: Comp });
    expect(() =>
      registerBuiltinView({ kind: "com.test.builtin.x", label: "内置 X", component: Comp }),
    ).toThrow(/全局唯一/);
    // 内置注册不拦截（provider=builtin 允许注册 VIEW_KINDS 内 kind，见上一条用例），插件占用内置 kind 拒绝
    expect(() =>
      window.__atelyxPlugin__!.forPlugin("com.test.a").registerPanel({
        kind: "canvas",
        label: "画布（插件）",
        component: Comp,
      }),
    ).toThrow(/内置视图保留/);
    // "empty" 占位哨兵 kind 同样保留（ViewKind 但不含于 VIEW_KINDS，需显式拒绝）
    expect(() =>
      window.__atelyxPlugin__!.forPlugin("com.test.a").registerPanel({
        kind: "empty",
        label: "空面板（插件）",
        component: Comp,
      }),
    ).toThrow(/内置视图保留/);
    expect(() =>
      window.__atelyxPlugin__!.forPlugin("com.test.a").registerPanel({
        kind: "com.test.builtin.x",
        label: "插件同名",
        component: Comp,
      }),
    ).toThrow(/全局唯一/);
    expect(() => registerBuiltinView({ kind: "", label: "空", component: Comp })).toThrow(/非空 kind/);
    // 内置贡献不被插件卸载误删（provider=builtin 与 pluginId 隔离）
    unregisterPluginUi("com.test.a");
    expect(getViewContribution("com.test.builtin.x")?.provider).toBe("builtin");
  });

  it("facade 仓库访问方法：经 provider 转发 + 未接线安全降级", async () => {
    exposePluginFacade();
    const openNote = vi.fn();
    const openTable = vi.fn();
    const openCanvasFile = vi.fn();
    const listFiles = vi.fn(() =>
      Promise.resolve([{ name: "a.md", path: "a.md", isDir: false, updatedAt: 0, children: [] }]),
    );
    setPluginVaultAccess({ ...RUNTIME, listFiles, openNote, openTable, openCanvasFile });
    const bridge = window.__atelyxPlugin__!.forPlugin("com.test.a");
    await expect(bridge.listFiles()).resolves.toHaveLength(1);
    bridge.openNote("a.md", "A");
    bridge.openTable("t.atb", "T");
    bridge.openCanvasFile({ id: "c.atlx", title: "C", file: "c.atlx", updatedAt: 0 });
    expect(openNote).toHaveBeenCalledWith("a.md", "A");
    expect(openTable).toHaveBeenCalledWith("t.atb", "T");
    expect(openCanvasFile).toHaveBeenCalledWith({ id: "c.atlx", title: "C", file: "c.atlx", updatedAt: 0 });
    // 未接线降级：listFiles 空数组、open* no-op
    setPluginVaultAccess(null);
    await expect(bridge.listFiles()).resolves.toEqual([]);
    expect(() => bridge.openNote("b.md", "B")).not.toThrow();
    expect(() => bridge.openTable("b.atb", "B")).not.toThrow();
    expect(() => bridge.openCanvasFile({ id: "b.atlx", title: "B", file: "b.atlx", updatedAt: 0 })).not.toThrow();
    expect(getPluginVaultAccess()).toBeNull();
  });
});
