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
  getPluginEdge,
  getPluginEdges,
  getPluginSetting,
  getPluginSettings,
  getPluginTableView,
  getPluginTableViews,
  getPluginThemeSettings,
  getPluginVaultAccess,
  getViewContribution,
  pluginViewKinds,
  pluginViewLabel,
  registerBuiltinView,
  setBuiltinPluginIds,
  setPluginTableAccess,
  setPluginThemeSettingsAccess,
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
  setBuiltinPluginIds(null);
  window.__atelyxPlugin__ = undefined;
  setPluginTableAccess(null);
  setPluginVaultAccess(null);
  setPluginThemeSettingsAccess(null);
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
    expect(getViewContribution("com.test.hello.dashboard")?.pluginId).toBe("com.test.hello");
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

  it("registerThemeSetting 收集/读取/按插件撤销 + facade 读写经 provider 转发", () => {
    exposePluginFacade();
    const bridge = window.__atelyxPlugin__!.forPlugin("com.test.a");
    bridge.registerThemeSetting({ key: "variant", label: "配色", component: Comp });
    expect(getPluginThemeSettings("com.test.a").map((s) => s.key)).toContain("variant");
    expect(getPluginThemeSettings("com.test.b")).toHaveLength(0);

    // facade 读写经注入 provider 转发（未接线降级为空对象/静默）
    expect(bridge.getThemeSettings()).toEqual({});
    bridge.setThemeSetting("accentColor", "#123456"); // 未接线静默

    const store: Record<string, Record<string, unknown>> = { "com.test.a": { colorMode: "dark" } };
    setPluginThemeSettingsAccess({
      getSettings: (pluginId) => store[pluginId] ?? {},
      setSetting: (pluginId, key, value) => {
        store[pluginId] = { ...(store[pluginId] ?? {}), [key]: value };
      },
    });
    expect(bridge.getThemeSettings()).toEqual({ colorMode: "dark" });
    bridge.setThemeSetting("accentColor", "#123456");
    expect(store["com.test.a"].accentColor).toBe("#123456");

    unregisterPluginUi("com.test.a");
    expect(getPluginThemeSettings("com.test.a")).toHaveLength(0);
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

  it("内置/插件面板视图贡献同表注册 + 统一读取", () => {
    exposePluginFacade();
    // 内置插件（注入的种子 id）可注册 VIEW_KINDS 内 kind（search 是内建视图），与插件面板同表注册
    setBuiltinPluginIds(new Set(["builtin.search"]));
    registerBuiltinView("builtin.search", { kind: "search", label: "搜索", component: Comp });
    const fp = getViewContribution("search");
    expect(fp?.pluginId).toBe("builtin.search");
    // 插件面板经 registerPanel 进同一注册表（pluginId 溯源）
    window.__atelyxPlugin__!.forPlugin("com.test.a").registerPanel({
      kind: "com.test.a.search",
      label: "搜索（插件）",
      component: Comp,
    });
    const plugin = getViewContribution("com.test.a.search");
    expect(plugin?.pluginId).toBe("com.test.a");
    // 注册表驱动视图菜单与显示名（不同 kind 并列）
    expect(pluginViewKinds()).toContain("search");
    expect(pluginViewKinds()).toContain("com.test.a.search");
    expect(pluginViewLabel("com.test.a.search")).toBe("搜索（插件）");
  });

  it("kind 全局唯一 + 内置保留：重复注册与占用内置 kind 均拒绝", () => {
    exposePluginFacade();
    setBuiltinPluginIds(new Set(["builtin.x"]));
    registerBuiltinView("builtin.x", { kind: "com.test.fp.x", label: "内置 X", component: Comp });
    expect(() =>
      registerBuiltinView("builtin.x", { kind: "com.test.fp.x", label: "内置 X", component: Comp }),
    ).toThrow(/全局唯一/);
    // 未注入为内置的插件（第三方）占用 VIEW_KINDS kind 拒绝（防冒名劫持封闭枚举）
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
        kind: "com.test.fp.x",
        label: "插件同名",
        component: Comp,
      }),
    ).toThrow(/全局唯一/);
    expect(() => registerBuiltinView("builtin.x", { kind: "", label: "空", component: Comp })).toThrow(/非空 kind/);
    // 内置贡献不被第三方插件卸载误删（pluginId 隔离）
    unregisterPluginUi("com.test.a");
    expect(getViewContribution("com.test.fp.x")?.pluginId).toBe("builtin.x");
  });

  it("内置插件可注册 VIEW_KINDS 内 kind；按 pluginId 卸载互不误删；非内置占用拒绝", () => {
    exposePluginFacade();
    setBuiltinPluginIds(new Set(["builtin.recent", "builtin.calendar"]));
    registerBuiltinView("builtin.recent", { kind: "recent", label: "最近打开", component: Comp });
    registerBuiltinView("builtin.calendar", { kind: "calendar", label: "日历", component: Comp });
    expect(getViewContribution("recent")?.pluginId).toBe("builtin.recent");
    // 未注入为内置的插件注册 VIEW_KINDS kind → 拒绝（内置 kind 是平台保留命名空间）
    expect(() =>
      window.__atelyxPlugin__!.forPlugin("com.test.a").registerPanel({
        kind: "recent",
        label: "最近打开（插件）",
        component: Comp,
      }),
    ).toThrow(/内置视图保留/);
    // 停用一个内置插件 = 按 pluginId 撤销，只删自己的贡献
    unregisterPluginUi("builtin.recent");
    expect(getViewContribution("recent")).toBeUndefined();
    expect(getViewContribution("calendar")?.pluginId).toBe("builtin.calendar");
  });

  it("registerBuiltinView render-only 承载（重型视图经 hostId 渲染）；component 与 render 至少其一", () => {
    exposePluginFacade();
    setBuiltinPluginIds(new Set(["builtin.canvas"]));
    // 画布视图需要宿主面板 id（聚焦门控）：只注册 render，不填 component
    registerBuiltinView("builtin.canvas", {
      kind: "canvas",
      label: "画布",
      render: (hostId) => `host:${hostId}`,
    });
    const c = getViewContribution("canvas");
    expect(c?.pluginId).toBe("builtin.canvas");
    expect(c?.component).toBeUndefined();
    // render 按 hostId 承载（ViewHost 分派路径：render 优先于 component）
    expect(c?.render?.("panel-1")).toBe("host:panel-1");
    // component 与 render 全缺 → 拒绝（无渲染入口的视图贡献无意义）
    expect(() => registerBuiltinView("builtin.canvas", { kind: "empty", label: "无渲染" })).toThrow(
      /component 或 render/,
    );
  });

  it("registerEdge 收集/读取/同名 last-wins 覆盖/按插件撤销", () => {
    exposePluginFacade();
    const edgeA = () => null;
    const edgeB = () => null;
    const bridgeA = window.__atelyxPlugin__!.forPlugin("com.test.a");
    const bridgeB = window.__atelyxPlugin__!.forPlugin("com.test.b");
    bridgeA.registerEdge({ type: "com.test.a.edge", component: edgeA });
    bridgeA.registerEdge({ type: "custom", component: edgeA });
    bridgeB.registerEdge({ type: "custom", component: edgeB });
    // 读取 + last-wins（与节点同语义：同名 type 后注册者覆盖，不拒绝）
    expect(getPluginEdges().map((e) => e.type)).toContain("com.test.a.edge");
    expect(getPluginEdge("custom")?.component).toBe(edgeB);
    expect(getPluginEdge("custom")?.pluginId).toBe("com.test.b");
    // 按插件撤销只删自己的边注册
    unregisterPluginUi("com.test.a");
    expect(getPluginEdge("com.test.a.edge")).toBeUndefined();
    expect(getPluginEdge("custom")?.pluginId).toBe("com.test.b");
    unregisterPluginUi("com.test.b");
    expect(getPluginEdge("custom")).toBeUndefined();
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
