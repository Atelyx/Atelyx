/**
 * 工作区布局树查询纯函数与默认结构种子。
 * 布局变更逻辑已下沉 Rust `layout.rs` 迷你窗口管理器，本文件不提供任何变异函数；
 * 查询函数只回答「某视图/标签/面板在哪里」——渲染、菜单禁用、协作宿主判定用。
 *
 * 语义：
 * - 布局 = 递归多叉树（Split = 分割方向 + 子树 + 占比；Panel 叶子 = 标签组）
 * - 撕裂窗口 = 应用级 `DetachedWindow`（跨布局共享）
 */
import {
  DEFAULT_SCENE_ID,
  HOME_LAYOUT_ID,
  type DetachedWindow,
  type LayoutNode,
  type PanelNode,
  type Scene,
  type TabItem,
  type ViewKind,
  type WorkspaceLayout,
} from "@/types";

/** 收集树中全部面板节点（深度优先，顺序稳定）。 */
export function collectPanels(tree: LayoutNode): PanelNode[] {
  if (tree.kind === "panel") return [tree];
  return tree.children.flatMap(collectPanels);
}

/** 按 id 查找面板节点（无则 null）。 */
export function findPanel(tree: LayoutNode, panelId: string): PanelNode | null {
  return collectPanels(tree).find((p) => p.id === panelId) ?? null;
}

/** 收集树中全部标签（深度优先，顺序稳定）。 */
export function collectTabs(tree: LayoutNode): TabItem[] {
  return collectPanels(tree).flatMap((p) => p.tabs);
}

/** 收集树中全部非空视图（不含重复——标签组内同视图不重复，跨面板也全局唯一）。 */
function collectViewsInTree(tree: LayoutNode): ViewKind[] {
  return collectTabs(tree).map((t) => t.view);
}

/** 树 + 撕裂窗口合计的全部非空视图（「已占用视图」判定依据，全局唯一约束）。 */
export function collectAllViews(tree: LayoutNode, detachedWindows: DetachedWindow[]): ViewKind[] {
  return [
    ...collectViewsInTree(tree),
    ...detachedWindows.flatMap((w) => w.tabs.map((t) => t.view)),
  ];
}

/** 视图当前所在宿主："main" = 主窗口面板树，string = 撕裂窗口 id，null = 未渲染。 */
export function findViewHost(
  tree: LayoutNode,
  detachedWindows: DetachedWindow[],
  view: ViewKind,
): "main" | string | null {
  if (collectViewsInTree(tree).includes(view)) return "main";
  for (const w of detachedWindows) {
    if (w.tabs.some((t) => t.view === view)) return w.id;
  }
  return null;
}

/** 面板的激活标签（tabs 空返回 null）。 */
export function activeTabOf(panel: PanelNode): TabItem | null {
  return panel.tabs.find((t) => t.id === panel.activeTabId) ?? panel.tabs[0] ?? null;
}

/**
 * 解析仓库级「启动时切换场景」配置：配置 id 在场景列表中存在 → 返回场景 id
 * （切换场景恢复该场景记忆的激活布局）；
 * 未配置 / 指定场景已删除（场景结构与仓库配置独立，悬挂引用常态存在）→ null = 不切换。
 */
export function resolveEntryScene(
  configuredId: string | null | undefined,
  scenes: Scene[],
): string | null {
  if (!configuredId) return null;
  return scenes.some((s) => s.id === configuredId) ? configuredId : null;
}

/** 新建标签（锁定恒 false；视图恒非 empty）。 */
export function createTab(view: ViewKind): TabItem {
  return { id: crypto.randomUUID(), view, locked: false };
}

/** 新建单标签面板。 */
export function createPanel(view: ViewKind): PanelNode {
  const tab = createTab(view);
  return { kind: "panel", id: crypto.randomUUID(), tabs: [tab], activeTabId: tab.id };
}

/** 主页布局（固定置顶；左窄右宽：左列 协作房间+最近打开，右区 日历+仓库历史；面板内部仍可自由调整）。 */
function createHomeLayout(): WorkspaceLayout {
  return {
    id: HOME_LAYOUT_ID,
    name: "主页",
    tree: {
      kind: "split",
      id: crypto.randomUUID(),
      direction: "horizontal",
      children: [
        {
          kind: "split",
          id: crypto.randomUUID(),
          direction: "vertical",
          children: [createPanel("collabroom"), createPanel("recent")],
          sizes: [50, 50],
        },
        {
          kind: "split",
          id: crypto.randomUUID(),
          direction: "vertical",
          children: [createPanel("calendar"), createPanel("repohistory")],
          sizes: [55, 45],
        },
      ],
      sizes: [22, 78],
    },
  };
}

/** 默认场景布局（三套：画布/笔记/表格，面板结构 文件 | [主区/副区]，均为单标签面板；
 *  主页走场景专属槽位不在此列）。首次进入仓库/布局损坏时回退。 */
function createDefaultLayouts(): WorkspaceLayout[] {
  const build = (name: string, left: ViewKind, main: ViewKind, right: ViewKind, sizes1: [number, number], sizes2: [number, number]): WorkspaceLayout => ({
    id: crypto.randomUUID(),
    name,
    tree: {
      kind: "split",
      id: crypto.randomUUID(),
      direction: "horizontal",
      children: [
        createPanel(left),
        {
          kind: "split",
          id: crypto.randomUUID(),
          direction: "horizontal",
          children: [createPanel(main), createPanel(right)],
          sizes: sizes2,
        },
      ],
      sizes: sizes1,
    },
  });
  return [
    build("画布", "files", "canvas", "inspector", [17, 83], [74, 26]),
    build("笔记", "files", "note", "aichat", [19, 81], [72, 28]),
    build("表格", "files", "table", "note", [18, 82], [77, 23]),
  ];
}

/** 默认场景列表（bootstrap 失败时的渲染兜底种子；专属主页 = createHomeLayout，与 Rust 同构）。 */
export function createDefaultScenes(): Scene[] {
  return [
    {
      id: DEFAULT_SCENE_ID,
      name: "默认",
      homeLayout: createHomeLayout(),
      activeLayoutId: HOME_LAYOUT_ID,
      layouts: createDefaultLayouts(),
    },
  ];
}
