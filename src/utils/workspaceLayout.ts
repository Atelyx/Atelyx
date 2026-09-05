/**
 * 工作区布局树查询纯函数（只读；布局变更逻辑已下沉 Rust `layout.rs` 迷你窗口管理器，
 * 本文件不再提供任何变异函数）。
 *
 * 语义：
 * - 布局 = 递归多叉树（Split = 分割方向 + 子树 + 占比；Panel 叶子 = 标签组）
 * - 撕裂窗口 = 应用级 `DetachedWindow`（跨布局共享）
 * - 本文件只回答「某视图/标签/面板在哪里」——渲染、菜单禁用、协作宿主判定用
 */
import type {
  DetachedWindow,
  LayoutNode,
  PanelNode,
  TabItem,
  ViewKind,
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
export function collectViewsInTree(tree: LayoutNode): ViewKind[] {
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
