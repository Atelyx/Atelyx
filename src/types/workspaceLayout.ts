/**
 * 工作区可自定义布局（多叉停靠 + 可撕裂多窗口）。
 *
 * 布局 = 一棵递归多叉树：Split 节点表达「分割方向 + 若干子树 + 各自占比」
 * （左中右等多面板可同级并列），Panel 叶子节点表达「一个停靠位置（标签组）」。
 * 全局 chrome（标题栏/最左功能栏）不参与面板网格。任意视图标签可撕裂出主窗口
 * 成为独立 OS 窗口（`DetachedWindow`），原面板保留为空位；也可拖回/拖入其他
 * 位置组成标签组。
 *
 * 约束（产品决策）：
 * - 每种视图全局最多一处渲染（树内面板 + 撕裂窗口合计；文件状态全局唯一，面板只是渲染入口）
 * - 打开文件与布局解耦：布局中没有对应视图面板时文件照常打开，只是无处显示
 * - 撕裂窗口为应用级（`types/uiState.ts` 的 `AppUiState.detachedWindows`），跨布局共享：
 *   切换布局不动撕裂窗口；被撕裂的视图在主窗口所有布局中均不可再添加
 * - 面板空位（tabs 为空）保留在树中（撕裂/关闭最后一个标签后），渲染空面板占位，
 *   可再添加视图或经 ≡ 菜单「删除面板」移除
 *
 * 布局列表 + 激活布局 + 聚焦面板 + 撕裂窗口应用级持久化到 `app_data_dir/ui-state.json`
 * （见 `types/uiState.ts` 的 `AppUiState`，Rust 侧 `commands/global.rs` 同步字段）。
 */

/** 内置视图类型（宿主随附；面板承载的内容）。 */
export type BuiltinViewKind =
  | "canvas"
  | "note"
  | "table"
  | "files"
  | "search"
  | "inspector"
  | "aichat"
  | "collabroom"
  | "calendar"
  | "repohistory"
  | "recent"
  | "empty";

/** 视图类型：内置视图或插件注册的开放 kind（Rust 侧 view 恒为 String，见 layout_model.rs）。
 *  非封闭枚举——插件可注册任意 kind 并出现在「添加面板」菜单/被 ViewHost 承载。 */
export type ViewKind = BuiltinViewKind | (string & {});

/** 分割方向：horizontal = 左右并排，vertical = 上下叠放。 */
export type SplitDirection = "horizontal" | "vertical";

/** 一个标签（停靠的视图实例）；view 恒不为 "empty"（空面板 = tabs 为空数组）。 */
export interface TabItem {
  id: string;
  view: ViewKind;
  /** 锁定（固定）：禁拖/禁撕裂/禁关闭，需先解锁。 */
  locked: boolean;
}

export interface PanelNode {
  kind: "panel";
  id: string;
  /** 停靠在此位置的标签组（空 = 空面板占位，仍留在树中）。 */
  tabs: TabItem[];
  /** 激活标签 id（tabs 为空时 null）。 */
  activeTabId: string | null;
}

export interface SplitNode {
  kind: "split";
  id: string;
  direction: SplitDirection;
  /** 至少两棵子树（多叉：左中右等 ≥2 同级面板；子树自身可为 Split，允许嵌套）。 */
  children: LayoutNode[];
  /** 各子树的相对尺寸（百分比，和 = 100；长度 = children 长度，相对父 Split，随拖拽实时回写）。 */
  sizes: number[];
}

export type LayoutNode = PanelNode | SplitNode;

/** 一套命名布局（布局列表的一项；只管主窗口面板树，撕裂窗口见 `DetachedWindow`）。 */
export interface WorkspaceLayout {
  id: string;
  name: string;
  tree: LayoutNode;
}

/** 撕裂出去的独立窗口（应用级，存 `AppUiState.detachedWindows`，跨布局共享）。 */
export interface DetachedWindow {
  id: string;
  /** 停靠在本窗口的标签组（拖空后窗口自动关闭，无空窗口概念）。 */
  tabs: TabItem[];
  activeTabId: string | null;
  /** 窗口屏幕位置与尺寸（logical px，创建/恢复/移动/缩放时更新）。 */
  bounds: { x: number; y: number; width: number; height: number };
}

/** 视图类型清单（视图选择器选项顺序）。 */
export const VIEW_KINDS: ViewKind[] = [
  "canvas",
  "note",
  "table",
  "files",
  "search",
  "inspector",
  "aichat",
  "collabroom",
  "calendar",
  "repohistory",
  "recent",
];

/** 新建标签（锁定恒 false；视图恒非 empty）。 */
export function createTab(view: ViewKind): TabItem {
  return { id: crypto.randomUUID(), view, locked: false };
}

/** 新建单标签面板。 */
export function createPanel(view: ViewKind): PanelNode {
  const tab = createTab(view);
  return { kind: "panel", id: crypto.randomUUID(), tabs: [tab], activeTabId: tab.id };
}

/** 主页布局的稳定 id（固定置顶、不可删除/排序/重命名；uiState 加载时缺失即补入，幂等只补一次）。 */
export const HOME_LAYOUT_ID = "home";

/** 主页布局（固定置顶；左窄右宽：左列 协作房间+最近打开，右区 日历+仓库历史；面板内部仍可自由调整）。 */
export function createHomeLayout(): WorkspaceLayout {
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

/**
 * 默认布局（主页固定置顶 + 三套：画布/笔记/表格，面板结构 文件 | [主区/副区]，均为单标签面板）。
 * 首次进入仓库/布局损坏时回退；激活布局缺省 = 列表第一个（主页）。
 */
export function createDefaultLayouts(): WorkspaceLayout[] {
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
    createHomeLayout(),
    build("画布", "files", "canvas", "inspector", [17, 83], [74, 26]),
    build("笔记", "files", "note", "aichat", [19, 81], [72, 28]),
    build("表格", "files", "table", "note", [18, 82], [77, 23]),
  ];
}
