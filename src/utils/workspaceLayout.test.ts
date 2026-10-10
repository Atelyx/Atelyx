/**
 * 工作区布局树查询函数契约测试（utils/workspaceLayout）。
 *
 * 覆盖：面板/标签收集、面板查找、视图汇总、视图宿主判定、激活标签、
 * resolveEntryScene（仓库级「启动时切换场景」配置解析）。
 * （布局变异逻辑已下沉 Rust `layout.rs`，不再在此测试。）
 */
import { describe, expect, it } from "vitest";
import {
  activeTabOf,
  collectAllViews,
  collectPanels,
  collectTabs,
  createPanel,
  createTab,
  findPanel,
  findViewHost,
  resolveEntryScene,
} from "./workspaceLayout";
import {
  type DetachedWindow,
  type LayoutNode,
  type Scene,
  type WorkspaceLayout,
} from "@/types/workspaceLayout";
import { DEFAULT_SCENE_ID } from "@/constants/views";

function makePanel(views: string[], active = 0): LayoutNode {
  const panel = createPanel(views[0] as never);
  for (let i = 1; i < views.length; i++) {
    panel.tabs.push(createTab(views[i] as never));
  }
  panel.activeTabId = panel.tabs[active]?.id ?? panel.tabs[0]?.id ?? null;
  return panel;
}

describe("面板/标签收集", () => {
  it("collectPanels 深度优先收集全部面板", () => {
    const tree: LayoutNode = {
      kind: "split",
      id: "root",
      direction: "horizontal",
      sizes: [50, 50],
      children: [
        makePanel(["files"]),
        {
          kind: "split",
          id: "inner",
          direction: "vertical",
          sizes: [50, 50],
          children: [makePanel(["canvas"]), makePanel(["note"])],
        },
      ],
    };
    expect(collectPanels(tree).map((p) => p.tabs[0]!.view)).toEqual(["files", "canvas", "note"]);
  });

  it("collectTabs 汇总全部标签（顺序稳定）", () => {
    const tree = makePanel(["canvas", "note"]);
    expect(collectTabs(tree).map((t) => t.view)).toEqual(["canvas", "note"]);
  });

  it("findPanel 按 id 命中 / 未命中返回 null", () => {
    const tree = makePanel(["canvas"]);
    const panel = collectPanels(tree)[0]!;
    expect(findPanel(tree, panel.id)).toBe(panel);
    expect(findPanel(tree, "missing")).toBeNull();
  });

  it("activeTabOf 激活标签；activeTabId 失效回退第一个；空面板返回 null", () => {
    const tree = makePanel(["canvas", "note"], 1);
    const panel = collectPanels(tree)[0]!;
    expect(activeTabOf(panel)?.view).toBe("note");
    panel.activeTabId = "gone";
    expect(activeTabOf(panel)?.view).toBe("canvas");
    const empty = createPanel("canvas");
    empty.tabs = [];
    empty.activeTabId = null;
    expect(activeTabOf(empty)).toBeNull();
  });
});

describe("视图汇总与宿主判定", () => {
  it("collectAllViews 汇总树 + 撕裂窗口视图", () => {
    const tree = makePanel(["canvas", "note"]);
    const w: DetachedWindow = {
      id: "w1",
      tabs: [createTab("table")],
      activeTabId: null,
      bounds: { x: 0, y: 0, width: 0, height: 0 },
      hidden: false,
      restoreOnLaunch: true,
      options: {},
      pinned: false,
    };
    expect(collectAllViews(tree, [w])).toEqual(["canvas", "note", "table"]);
  });

  it("findViewHost：树内 = main；撕裂窗口 = 窗口 id；未渲染 = null", () => {
    const tree = makePanel(["canvas"]);
    const w: DetachedWindow = {
      id: "w1",
      tabs: [createTab("table")],
      activeTabId: null,
      bounds: { x: 0, y: 0, width: 0, height: 0 },
      hidden: false,
      restoreOnLaunch: true,
      options: {},
      pinned: false,
    };
    expect(findViewHost(tree, [w], "canvas")).toBe("main");
    expect(findViewHost(tree, [w], "table")).toBe("w1");
    expect(findViewHost(tree, [w], "note")).toBeNull();
  });
});

describe("resolveEntryScene（启动仓库时自动切换场景的解析）", () => {
  function layout(id: string): WorkspaceLayout {
    return {
      id,
      name: id,
      tree: { kind: "panel", id: `p-${id}`, tabs: [], activeTabId: null },
    };
  }
  function scene(id: string, layoutIds: string[]): Scene {
    return {
      id,
      name: id,
      homeLayout: layout("home"),
      activeLayoutId: layoutIds[0] ?? null,
      layouts: layoutIds.map(layout),
    };
  }
  const scenes = [
    scene(DEFAULT_SCENE_ID, ["home", "canvas", "note"]),
    scene("scene-ai", ["comfyui", "draw"]),
  ];

  it("未配置（null/undefined/空串）= 不切换", () => {
    expect(resolveEntryScene(null, scenes)).toBeNull();
    expect(resolveEntryScene(undefined, scenes)).toBeNull();
    expect(resolveEntryScene("", scenes)).toBeNull();
  });

  it("命中场景返回场景 id（默认场景与非默认场景同口径）", () => {
    expect(resolveEntryScene(DEFAULT_SCENE_ID, scenes)).toBe(DEFAULT_SCENE_ID);
    expect(resolveEntryScene("scene-ai", scenes)).toBe("scene-ai");
  });

  it("指定场景已删除（悬挂引用）= 不切换", () => {
    expect(resolveEntryScene("deleted", scenes)).toBeNull();
  });

  it("场景列表为空 = 不切换", () => {
    expect(resolveEntryScene("scene-ai", [])).toBeNull();
  });
});
