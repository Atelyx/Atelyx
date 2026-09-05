//! 布局迷你窗口管理器的纯模型层：类型 + 布局树/标签组纯操作 + 主页/默认布局构建 +
//! normalize/ensure_home。不碰窗口、不碰磁盘、不碰广播——副作用全在上层
//! （`layout.rs` 命令面 / `layout_drag.rs` / `layout_persist.rs` / `layout_window.rs`）。

use nanoid::nanoid;
use serde::{Deserialize, Serialize};

// ===== 类型（与前端 types/workspaceLayout.ts + types/uiState.ts 对齐）=====

/// 视图类型（插件视图为任意字符串，故用 String）。
pub type ViewKind = String;

/// 窗口位置尺寸（logical px；与前端 `DetachedWindow.bounds` 一致）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WindowBounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    /// DPI scale factor（权威 bounds 注册表的换算基准：OS 全局光标物理 px → logical px）。
    /// 前端 op 载荷（TearOff）不含此字段，缺失取默认 0。
    #[serde(default)]
    pub scale: f64,
}

/// 一个标签（停靠的视图实例）；view 恒非 empty。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TabItem {
    pub id: String,
    pub view: ViewKind,
    /// 锁定（固定）：禁拖/禁撕裂/禁关闭，需先解锁。
    pub locked: bool,
}

/// 布局树节点：Panel 叶子 = 停靠位置（标签组）；Split = 分割方向 + 子树 + 占比。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind")]
pub enum LayoutNode {
    #[serde(rename = "panel")]
    Panel {
        #[serde(rename = "id")]
        id: String,
        #[serde(rename = "tabs")]
        tabs: Vec<TabItem>,
        #[serde(rename = "activeTabId")]
        active_tab_id: Option<String>,
    },
    #[serde(rename = "split")]
    Split {
        #[serde(rename = "id")]
        id: String,
        #[serde(rename = "direction")]
        direction: String,
        #[serde(rename = "children")]
        children: Vec<LayoutNode>,
        #[serde(rename = "sizes")]
        sizes: Vec<f64>,
    },
}

/// 一套命名布局（布局列表的一项；只管主窗口面板树，撕裂窗口见 `DetachedWindow`）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceLayout {
    pub id: String,
    pub name: String,
    pub tree: LayoutNode,
}

/// 撕裂出去的独立窗口（应用级、跨布局共享）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DetachedWindow {
    pub id: String,
    /// 停靠在本窗口的标签组（拖空后条目自动移除）。
    pub tabs: Vec<TabItem>,
    pub active_tab_id: Option<String>,
    /// 窗口屏幕位置与尺寸（logical px）。
    pub bounds: WindowBounds,
}

/// 应用级 UI 使用状态（`app_data_dir/ui-state.json` 磁盘格式；本模块为唯一写者）。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct AppUiState {
    pub schema: String,
    /// 文件面板展开的文件夹相对路径列表（缺省 = 全部折叠；跨仓库按路径共享）。
    #[serde(default)]
    pub file_explorer_expanded: Vec<String>,
    /// 上次打开的画布/笔记/表格文件（相对仓库根路径）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_canvas_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_note_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_table_file: Option<String>,
    /// 工作区布局列表（缺省 = 默认布局）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_layouts: Vec<WorkspaceLayout>,
    /// 激活布局 id（缺省 = 布局列表第一个）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_layout_id: Option<String>,
    /// 聚焦面板 id（画布快捷键门控；由前端 JS 维护并 patch 到本模块）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused_panel_id: Option<String>,
    /// 撕裂出去的独立窗口（应用级、跨布局共享）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub detached_windows: Vec<DetachedWindow>,
    /// 最近打开的文件（跨仓库、去重置顶、上限截断；结构由前端自持，本模块原样透传）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_files: Vec<serde_json::Value>,
}

/// ui-state.json 的 schema 版本（与前端 `types/uiState.ts` 对齐）。
pub const UI_STATE_SCHEMA: &str = "atelyx-ui-state/v1";
/// 主页布局的稳定 id（固定置顶、不可删除/排序/重命名）。
pub const HOME_LAYOUT_ID: &str = "home";
/// 最近打开文件列表上限。
pub(crate) const MAX_RECENT_FILES: usize = 50;

// ===== 布局操作命令参数（前端 uiStateStore 逐条映射）=====

/// 布局操作（布局模型唯一变更入口）。op 标签 + 字段均 camelCase，与前端 `LayoutOp` 对齐。
#[derive(Deserialize, Debug)]
#[serde(tag = "op", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum LayoutOp {
    /// 添加视图到面板：组内已有该视图 = 激活；视图全局被占用 = 忽略；否则新建标签。
    AddView { panel_id: String, view: ViewKind },
    /// 激活面板中的标签。
    SetActive { panel_id: String, tab_id: String },
    /// 关闭面板中的标签（锁定拒关；最后一个标签关闭 → 面板留空）。
    CloseTab { panel_id: String, tab_id: String },
    /// 锁定/解锁面板中的标签。
    SetLocked { panel_id: String, tab_id: String, locked: bool },
    /// 切换面板中某标签的视图（锁定拒关；目标视图被其他位置占用 = 忽略）。
    SetTabView { panel_id: String, tab_id: String, view: ViewKind },
    /// 面板标签组内排序。
    MoveTabWithin { panel_id: String, tab_id: String, to_index: usize },
    /// 窗口内跨面板移动标签（面板 A → 面板 B 标签组，默认尾部）。
    MoveTabBetween {
        from_panel_id: String,
        to_panel_id: String,
        tab_id: String,
        index: Option<usize>,
    },
    /// 分割激活布局中的面板：父 split 方向匹配时同级插入新空面板，否则嵌套回退。
    SplitPanel {
        panel_id: String,
        direction: String,
        position: Option<String>,
    },
    /// 删除面板 = 整块移除（含全部标签）；最后一个面板不可删。
    ClosePanel { panel_id: String },
    /// 撕裂：从面板移除标签（面板留空）→ 挂到撕裂窗口列表，返回新窗口条目。
    TearOff {
        panel_id: String,
        tab_id: String,
        bounds: WindowBounds,
    },
    /// 撕裂窗口再撕裂：把标签从撕裂窗口移到新的撕裂窗口条目。
    TearOffFromDetached {
        window_id: String,
        tab_id: String,
        bounds: WindowBounds,
    },
    /// 拖回：把撕裂窗口中的标签停靠进主窗口面板（默认尾部；源窗口拖空自动移除）。
    DockIntoPanel {
        panel_id: String,
        tab_id: String,
        index: Option<usize>,
    },
    /// 拖入：把标签停靠进撕裂窗口（来源 = 树面板或另一撕裂窗口；同窗口 = 组内排序）。
    DockIntoDetached {
        window_id: String,
        tab_id: String,
        index: Option<usize>,
    },
    /// 向撕裂窗口添加新视图标签（视图全局唯一，已占用则忽略）。
    DetachedAddView { window_id: String, view: ViewKind },
    /// 激活撕裂窗口中的标签。
    DetachedSetActive { window_id: String, tab_id: String },
    /// 关闭撕裂窗口中的标签（锁定拒关；拖空后窗口条目移除）。
    DetachedCloseTab { window_id: String, tab_id: String },
    /// 锁定/解锁撕裂窗口中的标签。
    DetachedSetLocked { window_id: String, tab_id: String, locked: bool },
    /// 切换撕裂窗口中某标签的视图（锁定拒关；目标视图被其他位置占用 = 忽略）。
    DetachedSetTabView { window_id: String, tab_id: String, view: ViewKind },
    /// 撕裂窗口标签组内排序。
    DetachedMoveTab { window_id: String, tab_id: String, to_index: usize },
    /// 移除撕裂窗口条目（OS 窗口已关闭/拖空自动关窗时调用）。
    RemoveDetachedWindow { window_id: String },
    /// 拖拽调宽回写 Split 子树尺寸比例（百分数，和 = 100，长度 = children 长度）。
    SetLayoutSizes { split_id: String, sizes: Vec<f64> },
    /// 新建布局（复制当前激活布局），命名「布局 N」自动去重，并激活。
    AddLayout,
    /// 重命名布局（主页固定不可重命名）。
    RenameLayout { id: String, name: String },
    /// 删除布局（主页固定不可删；最后一个不可删）。
    DeleteLayout { id: String },
    /// 激活布局（切换布局：仅替换面板网格，文件状态与撕裂窗口不动）。
    ActivateLayout { id: String },
    /// 调整布局顺序（布局 tab 拖拽排序；主页固定置顶）。
    MoveLayout { from_index: usize, to_index: usize },
}

/// 布局操作的返回值（仅 splitPanel/tearOff 需要；其余操作前端靠广播收敛）。
#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct LayoutOpResult {
    /// SplitPanel 新建面板 id。
    pub split_panel_id: Option<String>,
    /// TearOff 创建的新撕裂窗口条目。
    pub detached_window: Option<DetachedWindow>,
}

/// 非布局字段补丁（前端 JS 拥有这些字段，patch 到本模块合并后由本模块落盘）。
/// 外层 `Option` = 字段是否在载荷中；内层 = 字段值（内层 None = 显式 null 清除）。
#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "camelCase")]
pub struct UiStatePatch {
    #[serde(default)]
    pub file_explorer_expanded: Option<Vec<String>>,
    #[serde(default)]
    pub last_canvas_file: Option<Option<String>>,
    #[serde(default)]
    pub last_note_file: Option<Option<String>>,
    #[serde(default)]
    pub last_table_file: Option<Option<String>>,
    #[serde(default)]
    pub focused_panel_id: Option<Option<String>>,
    #[serde(default)]
    pub recent_files: Option<Vec<serde_json::Value>>,
}

/// 归一化：schema 补齐 + 布局列表非空 + 主页固定置顶 + 激活布局有效 + 撕裂窗口过滤 + 最近打开截断。
pub(crate) fn normalize(ui: &mut AppUiState) {
    ui.schema = UI_STATE_SCHEMA.to_string();
    if ui.workspace_layouts.is_empty() {
        ui.workspace_layouts = create_default_layouts();
    }
    ui.workspace_layouts = ensure_home_layout(std::mem::take(&mut ui.workspace_layouts));
    let first = ui.workspace_layouts[0].id.clone();
    if !ui.workspace_layouts.iter().any(|l| Some(&l.id) == ui.active_layout_id.as_ref()) {
        ui.active_layout_id = Some(first);
    }
    // 撕裂窗口过滤空条目（tabs 非空；损坏条目在 serde 层已整文件回退）
    ui.detached_windows.retain(|w| !w.tabs.is_empty());
    if ui.recent_files.len() > MAX_RECENT_FILES {
        ui.recent_files.truncate(MAX_RECENT_FILES);
    }
}

/// 保证主页布局存在且恒置顶（主页不可删除/排序；已存在的保留用户面板调整，缺失只补一次）。
fn ensure_home_layout(layouts: Vec<WorkspaceLayout>) -> Vec<WorkspaceLayout> {
    let home = layouts.iter().find(|l| l.id == HOME_LAYOUT_ID).cloned();
    let mut rest: Vec<WorkspaceLayout> = layouts.into_iter().filter(|l| l.id != HOME_LAYOUT_ID).collect();
    let home = home.unwrap_or_else(create_home_layout);
    let mut out = vec![home];
    out.append(&mut rest);
    out
}

/// 「布局 N」命名自动去重（N = 最小未占用序号）。
pub(crate) fn next_layout_name(names: &[String]) -> String {
    let mut n = 1usize;
    while names.iter().any(|x| x == &format!("布局 {n}")) {
        n += 1;
    }
    format!("布局 {n}")
}

/// 新建标签（锁定恒 false；视图恒非 empty）。
pub(crate) fn create_tab(view: &str) -> TabItem {
    TabItem { id: nanoid!(), view: view.to_string(), locked: false }
}

/// 新建单标签面板。
fn create_panel(view: &str) -> LayoutNode {
    let tab = create_tab(view);
    let id = tab.id.clone();
    LayoutNode::Panel { id: nanoid!(), tabs: vec![tab], active_tab_id: Some(id) }
}

/// 主页布局（固定置顶；左窄右宽：左列 协作房间+最近打开，右区 日历+仓库历史）。
fn create_home_layout() -> WorkspaceLayout {
    WorkspaceLayout {
        id: HOME_LAYOUT_ID.to_string(),
        name: "主页".to_string(),
        tree: LayoutNode::Split {
            id: nanoid!(),
            direction: "horizontal".to_string(),
            children: vec![
                LayoutNode::Split {
                    id: nanoid!(),
                    direction: "vertical".to_string(),
                    children: vec![create_panel("collabroom"), create_panel("recent")],
                    sizes: vec![50.0, 50.0],
                },
                LayoutNode::Split {
                    id: nanoid!(),
                    direction: "vertical".to_string(),
                    children: vec![create_panel("calendar"), create_panel("repohistory")],
                    sizes: vec![55.0, 45.0],
                },
            ],
            sizes: vec![22.0, 78.0],
        },
    }
}

/// 默认布局（主页固定置顶 + 三套：画布/笔记/表格，面板结构 文件 | [主区/副区]）。
fn create_default_layouts() -> Vec<WorkspaceLayout> {
    let build = |name: &str, left: &str, main: &str, right: &str, s1: (f64, f64), s2: (f64, f64)| {
        WorkspaceLayout {
            id: nanoid!(),
            name: name.to_string(),
            tree: LayoutNode::Split {
                id: nanoid!(),
                direction: "horizontal".to_string(),
                children: vec![
                    create_panel(left),
                    LayoutNode::Split {
                        id: nanoid!(),
                        direction: "horizontal".to_string(),
                        children: vec![create_panel(main), create_panel(right)],
                        sizes: vec![s2.0, s2.1],
                    },
                ],
                sizes: vec![s1.0, s1.1],
            },
        }
    };
    vec![
        create_home_layout(),
        build("画布", "files", "canvas", "inspector", (17.0, 83.0), (74.0, 26.0)),
        build("笔记", "files", "note", "aichat", (19.0, 81.0), (72.0, 28.0)),
        build("表格", "files", "table", "note", (18.0, 82.0), (77.0, 23.0)),
    ]
}

// ===== 布局树/标签组纯操作（行为与前端 utils/workspaceLayout.ts 一致）=====

impl LayoutNode {
    pub fn node_id(&self) -> &str {
        match self {
            LayoutNode::Panel { id, .. } | LayoutNode::Split { id, .. } => id,
        }
    }
}

/// 收集树中全部面板节点（深度优先，顺序稳定）。
pub(crate) fn collect_panels<'a>(tree: &'a LayoutNode, out: &mut Vec<&'a LayoutNode>) {
    match tree {
        LayoutNode::Panel { .. } => out.push(tree),
        LayoutNode::Split { children, .. } => {
            for c in children {
                collect_panels(c, out);
            }
        }
    }
}

/// 树中全部标签。
pub(crate) fn collect_tabs<'a>(tree: &'a LayoutNode, out: &mut Vec<&'a TabItem>) {
    let mut panels = Vec::new();
    collect_panels(tree, &mut panels);
    for p in panels {
        if let LayoutNode::Panel { tabs, .. } = p {
            out.extend(tabs.iter());
        }
    }
}

/// 在树中按面板 id 查找（返回该面板标签克隆；无则 None）。
pub(crate) fn find_panel(tree: &LayoutNode, panel_id: &str) -> Option<Vec<TabItem>> {
    let mut panels = Vec::new();
    collect_panels(tree, &mut panels);
    panels.into_iter().find(|p| p.node_id() == panel_id).map(|p| match p {
        LayoutNode::Panel { tabs, .. } => tabs.clone(),
        _ => unreachable!("collect_panels 只产出 Panel 节点"),
    })
}

/// 在树中按标签 id 查找（返回所在面板 id + 标签克隆）。
pub(crate) fn find_tab_in_tree(tree: &LayoutNode, tab_id: &str) -> Option<(String, TabItem)> {
    let mut panels = Vec::new();
    collect_panels(tree, &mut panels);
    for p in panels {
        if let LayoutNode::Panel { id, tabs, .. } = p {
            if let Some(tab) = tabs.iter().find(|t| t.id == tab_id) {
                return Some((id.clone(), tab.clone()));
            }
        }
    }
    None
}

/// 在撕裂窗口中按标签 id 查找。
pub(crate) fn find_tab_in_detached(detached: &[DetachedWindow], tab_id: &str) -> Option<(String, TabItem)> {
    for w in detached {
        if let Some(tab) = w.tabs.iter().find(|t| t.id == tab_id) {
            return Some((w.id.clone(), tab.clone()));
        }
    }
    None
}

/// 查找面板节点的父 split 与下标（无父（根面板）返回 None；分割/关闭用）。
fn parent_split_of(tree: &LayoutNode, panel_id: &str) -> Option<(String, usize)> {
    match tree {
        LayoutNode::Panel { .. } => None,
        LayoutNode::Split { id, children, .. } => {
            for (i, child) in children.iter().enumerate() {
                match child {
                    LayoutNode::Panel { id: cid, .. } => {
                        if cid == panel_id {
                            return Some((id.clone(), i));
                        }
                    }
                    LayoutNode::Split { .. } => {
                        if let Some(hit) = parent_split_of(child, panel_id) {
                            return Some(hit);
                        }
                    }
                }
            }
            None
        }
    }
}

/// 按 id 查找节点（SplitPanel 判定父方向用）。
fn tree_node_by_id<'a>(tree: &'a LayoutNode, id: &str) -> Option<&'a LayoutNode> {
    if tree.node_id() == id {
        return Some(tree);
    }
    if let LayoutNode::Split { children, .. } = tree {
        for c in children {
            if let Some(hit) = tree_node_by_id(c, id) {
                return Some(hit);
            }
        }
    }
    None
}

/// 激活布局（activeLayoutId 失效/未设时回退第一个；列表恒非空）。
pub(crate) fn active_layout(ui: &AppUiState) -> WorkspaceLayout {
    ui.workspace_layouts
        .iter()
        .find(|l| Some(&l.id) == ui.active_layout_id.as_ref())
        .cloned()
        .unwrap_or_else(|| ui.workspace_layouts[0].clone())
}

/// 直接替换激活布局的区域树。
pub(crate) fn set_active_tree(ui: &mut AppUiState, tree: LayoutNode) {
    let active_id = active_layout(ui).id;
    for l in &mut ui.workspace_layouts {
        if l.id == active_id {
            l.tree = tree;
            break;
        }
    }
}

/// 标签组投影（Panel 与 DetachedWindow 的 {tabs, activeTabId} 同构）。
pub(crate) struct TabGroup {
    pub tabs: Vec<TabItem>,
    pub active_tab_id: Option<String>,
}

pub(crate) fn group_of(panel: &LayoutNode) -> TabGroup {
    match panel {
        LayoutNode::Panel { tabs, active_tab_id, .. } => TabGroup { tabs: tabs.clone(), active_tab_id: active_tab_id.clone() },
        // map_panel 的 f 只对命中面板调用，这里恒为 Panel
        _ => unreachable!("group_of 只接受 Panel 节点"),
    }
}

pub(crate) fn group_of_detached(w: &DetachedWindow) -> TabGroup {
    TabGroup { tabs: w.tabs.clone(), active_tab_id: w.active_tab_id.clone() }
}

/// 把标签组补丁写回 Panel 节点（patch 为 None 时原样返回）。
pub(crate) fn apply_tab_group_panel(panel: &LayoutNode, patch: Option<TabGroup>) -> LayoutNode {
    match patch {
        // 组操作未命中（如移除不存在的标签）→ 原样返回
        None => panel.clone(),
        Some(p) => match panel {
            LayoutNode::Panel { id, .. } => LayoutNode::Panel { id: id.clone(), tabs: p.tabs, active_tab_id: p.active_tab_id },
            // map_panel 的 f 只对命中面板调用，这里恒为 Panel
            _ => unreachable!("apply_tab_group_panel 只接受 Panel 节点"),
        },
    }
}

/// 把标签组补丁写回撕裂窗口。
pub(crate) fn apply_tab_group_detached(win: &DetachedWindow, patch: Option<TabGroup>) -> DetachedWindow {
    match patch {
        Some(p) => DetachedWindow { id: win.id.clone(), tabs: p.tabs, active_tab_id: p.active_tab_id, bounds: win.bounds.clone() },
        None => win.clone(),
    }
}

/// 递归更新目标面板（未命中返回原树）。
pub(crate) fn map_panel(tree: &LayoutNode, panel_id: &str, f: &dyn Fn(&LayoutNode) -> LayoutNode) -> LayoutNode {
    match tree {
        LayoutNode::Panel { .. } => {
            if tree.node_id() == panel_id {
                f(tree)
            } else {
                tree.clone()
            }
        }
        LayoutNode::Split { id, direction, children, sizes } => {
            let children: Vec<LayoutNode> = children.iter().map(|c| map_panel(c, panel_id, f)).collect();
            LayoutNode::Split { id: id.clone(), direction: direction.clone(), children, sizes: sizes.clone() }
        }
    }
}

/// 递归更新目标撕裂窗口（未命中返回原数组）。
pub(crate) fn map_detached(detached: &[DetachedWindow], window_id: &str, f: &dyn Fn(&DetachedWindow) -> DetachedWindow) -> Vec<DetachedWindow> {
    detached.iter().map(|w| if w.id == window_id { f(w) } else { w.clone() }).collect()
}

/// 移除被拖空/关空的撕裂窗口条目（标签全走后窗口无意义，自动回收）。
pub(crate) fn prune_empty_windows(windows: Vec<DetachedWindow>) -> Vec<DetachedWindow> {
    windows.into_iter().filter(|w| !w.tabs.is_empty()).collect()
}

// ---- 标签组共享核心（行为与前端 TabGroup 核心一致）----

/// 插入标签（默认尾部）并激活。
pub(crate) fn group_add_tab(g: &TabGroup, tab: TabItem, index: Option<usize>) -> TabGroup {
    let mut tabs = g.tabs.clone();
    let i = index.unwrap_or(tabs.len()).min(tabs.len());
    tabs.insert(i, tab);
    let active = tabs.get(i).map(|t| t.id.clone());
    TabGroup { tabs, active_tab_id: active }
}

/// 移除标签（被移除的是激活标签时激活右邻；移除后空 → active 置 None）。
pub(crate) fn group_remove_tab(g: &TabGroup, tab_id: &str) -> Option<TabGroup> {
    let i = g.tabs.iter().position(|t| t.id == tab_id)?;
    let mut tabs = g.tabs.clone();
    tabs.remove(i);
    let mut active_tab_id = g.active_tab_id.clone();
    if active_tab_id.as_deref() == Some(tab_id) {
        active_tab_id = if tabs.is_empty() {
            None
        } else {
            tabs.get(i.min(tabs.len() - 1)).or_else(|| tabs.first()).map(|t| t.id.clone())
        };
    }
    Some(TabGroup { tabs, active_tab_id })
}

/// 激活标签（不存在返回 None）。
pub(crate) fn group_activate_tab(g: &TabGroup, tab_id: &str) -> Option<TabGroup> {
    if g.tabs.iter().any(|t| t.id == tab_id) {
        Some(TabGroup { tabs: g.tabs.clone(), active_tab_id: Some(tab_id.to_string()) })
    } else {
        None
    }
}

/// 切换某标签的视图（view 恒非 empty）。
pub(crate) fn group_set_tab_view(g: &TabGroup, tab_id: &str, view: &str) -> TabGroup {
    TabGroup {
        tabs: g
            .tabs
            .iter()
            .map(|t| if t.id == tab_id { TabItem { id: t.id.clone(), view: view.to_string(), locked: t.locked } } else { t.clone() })
            .collect(),
        active_tab_id: g.active_tab_id.clone(),
    }
}

/// 锁定/解锁某标签。
pub(crate) fn group_set_tab_locked(g: &TabGroup, tab_id: &str, locked: bool) -> TabGroup {
    TabGroup {
        tabs: g
            .tabs
            .iter()
            .map(|t| if t.id == tab_id { TabItem { id: t.id.clone(), view: t.view.clone(), locked } } else { t.clone() })
            .collect(),
        active_tab_id: g.active_tab_id.clone(),
    }
}

/// 组内排序（把 tab_id 移到 toIndex，前移后移均按移除后下标处理）。
pub(crate) fn group_move_tab(g: &TabGroup, tab_id: &str, to_index: usize) -> Option<TabGroup> {
    let from = g.tabs.iter().position(|t| t.id == tab_id)?;
    let mut tabs = g.tabs.clone();
    let moved = tabs.remove(from);
    let target = if from < to_index { to_index.saturating_sub(1) } else { to_index };
    tabs.insert(target.min(tabs.len()), moved);
    Some(TabGroup { tabs, active_tab_id: g.active_tab_id.clone() })
}

/// 同级插入：把新面板插到父 split children 的 index 相邻位，尺寸从该面板均分一半。
fn insert_sibling(tree: &LayoutNode, split_id: &str, index: usize, new_panel: &LayoutNode, position: &str) -> LayoutNode {
    match tree {
        LayoutNode::Panel { .. } => tree.clone(),
        LayoutNode::Split { id, direction, children, sizes } => {
            if id == split_id {
                let insert_at = if position == "before" { index } else { index + 1 };
                let half = sizes.get(index).copied().unwrap_or(100.0) / 2.0;
                let mut children = children.clone();
                let mut sizes = sizes.clone();
                children.insert(insert_at, new_panel.clone());
                sizes.insert(insert_at, half);
                let other = if position == "before" { index + 1 } else { index };
                sizes[other] = half;
                LayoutNode::Split { id: id.clone(), direction: direction.clone(), children, sizes }
            } else {
                LayoutNode::Split {
                    id: id.clone(),
                    direction: direction.clone(),
                    children: children.iter().map(|c| insert_sibling(c, split_id, index, new_panel, position)).collect(),
                    sizes: sizes.clone(),
                }
            }
        }
    }
}

/// 嵌套回退：把面板替换为 Split[该面板, 新空面板]（direction，新面板按 position 放前/后）。
fn wrap_panel(tree: &LayoutNode, panel_id: &str, direction: &str, new_panel: &LayoutNode, position: &str) -> LayoutNode {
    match tree {
        LayoutNode::Panel { .. } => {
            if tree.node_id() == panel_id {
                let (a, b) = if position == "before" { (new_panel.clone(), tree.clone()) } else { (tree.clone(), new_panel.clone()) };
                LayoutNode::Split { id: nanoid!(), direction: direction.to_string(), children: vec![a, b], sizes: vec![50.0, 50.0] }
            } else {
                tree.clone()
            }
        }
        LayoutNode::Split { id, direction: d, children, sizes } => {
            LayoutNode::Split {
                id: id.clone(),
                direction: d.clone(),
                children: children.iter().map(|c| wrap_panel(c, panel_id, direction, new_panel, position)).collect(),
                sizes: sizes.clone(),
            }
        }
    }
}

/// 分割面板：插入新空面板承载新面板（父方向匹配同级插入，否则嵌套回退）。返回新树与新面板 id。
pub(crate) fn split_panel_op(tree: &LayoutNode, panel_id: &str, direction: &str, position: &str) -> (LayoutNode, String) {
    let new_panel = LayoutNode::Panel { id: nanoid!(), tabs: vec![], active_tab_id: None };
    let new_id = new_panel.node_id().to_string();
    if let Some((split_id, index)) = parent_split_of(tree, panel_id) {
        if let Some(LayoutNode::Split { direction: d, .. }) = tree_node_by_id(tree, &split_id) {
            if d.as_str() == direction {
                return (insert_sibling(tree, &split_id, index, &new_panel, position), new_id);
            }
        }
    }
    (wrap_panel(tree, panel_id, direction, &new_panel, position), new_id)
}

/// 关闭面板 = 从父 split 移除（children 剩 1 个时父塌缩；根即该面板时返回 None）。
pub(crate) fn close_panel_op(tree: &LayoutNode, panel_id: &str) -> Option<LayoutNode> {
    struct R {
        node: Option<LayoutNode>,
        removed: bool,
    }
    fn remove_from_split(node: &LayoutNode, panel_id: &str) -> R {
        match node {
            LayoutNode::Panel { .. } => {
                if node.node_id() == panel_id {
                    R { node: None, removed: true }
                } else {
                    R { node: Some(node.clone()), removed: false }
                }
            }
            LayoutNode::Split { id, direction, children, sizes } => {
                let mut removed = false;
                let mut kept_children = Vec::new();
                let mut kept_sizes = Vec::new();
                for (i, child) in children.iter().enumerate() {
                    let r = remove_from_split(child, panel_id);
                    if let Some(n) = r.node {
                        kept_children.push(n);
                        kept_sizes.push(sizes.get(i).copied().unwrap_or(0.0));
                    }
                    if r.removed {
                        removed = true;
                    }
                }
                if !removed {
                    return R { node: Some(node.clone()), removed: false };
                }
                if kept_children.len() == 1 {
                    return R { node: kept_children.into_iter().next(), removed: true };
                }
                if kept_children.is_empty() {
                    return R { node: None, removed: true };
                }
                let total: f64 = kept_sizes.iter().sum();
                let total = if total == 0.0 { 1.0 } else { total };
                let sizes = kept_sizes.iter().map(|s| s / total * 100.0).collect();
                R { node: Some(LayoutNode::Split { id: id.clone(), direction: direction.clone(), children: kept_children, sizes }), removed: true }
            }
        }
    }
    let r = remove_from_split(tree, panel_id);
    if r.removed {
        r.node
    } else {
        Some(tree.clone())
    }
}

/// 回写 Split 子树尺寸比例。
pub(crate) fn set_layout_sizes_op(tree: &LayoutNode, split_id: &str, sizes: &[f64]) -> LayoutNode {
    match tree {
        LayoutNode::Panel { .. } => tree.clone(),
        LayoutNode::Split { id, direction, children, sizes: old } => {
            let children: Vec<LayoutNode> = children.iter().map(|c| set_layout_sizes_op(c, split_id, sizes)).collect();
            let next_sizes = if id == split_id { sizes.to_vec() } else { old.clone() };
            LayoutNode::Split { id: id.clone(), direction: direction.clone(), children, sizes: next_sizes }
        }
    }
}

/// 撕裂：从面板移除标签（面板留空），返回 { 新树, 被移除的标签 }。
pub(crate) fn tear_off_from_panel_op(tree: &LayoutNode, panel_id: &str, tab_id: &str) -> Option<(LayoutNode, TabItem)> {
    let (hit_panel, tab) = find_tab_in_tree(tree, tab_id)?;
    if hit_panel != panel_id {
        return None;
    }
    let new_tree = map_panel(tree, panel_id, &|p| {
        apply_tab_group_panel(p, group_remove_tab(&group_of(p), tab_id))
    });
    Some((new_tree, tab))
}

/// 复制布局树时全部节点与标签重新生成 id（布局复制 = 独立副本，id 全局唯一约定）。
pub(crate) fn regenerate_ids(node: &LayoutNode) -> LayoutNode {
    match node {
        LayoutNode::Panel { tabs, .. } => {
            let tabs: Vec<TabItem> = tabs.iter().map(|t| TabItem { id: nanoid!(), view: t.view.clone(), locked: t.locked }).collect();
            let active = tabs.first().map(|t| t.id.clone());
            LayoutNode::Panel { id: nanoid!(), tabs, active_tab_id: active }
        }
        LayoutNode::Split { direction, children, sizes, .. } => {
            LayoutNode::Split {
                id: nanoid!(),
                direction: direction.clone(),
                children: children.iter().map(regenerate_ids).collect(),
                sizes: sizes.clone(),
            }
        }
    }
}

// ===== 单元测试 =====

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::apply_layout_op;

    fn ui_with(tree: LayoutNode) -> AppUiState {
        AppUiState {
            schema: UI_STATE_SCHEMA.into(),
            workspace_layouts: vec![WorkspaceLayout { id: "l1".into(), name: "L".into(), tree }],
            active_layout_id: Some("l1".into()),
            ..Default::default()
        }
    }

    fn panel(id: &str, views: &[&str]) -> LayoutNode {
        let tabs: Vec<TabItem> = views
            .iter()
            .map(|v| TabItem { id: format!("t-{v}"), view: v.to_string(), locked: false })
            .collect();
        let active = tabs.first().map(|t| t.id.clone());
        LayoutNode::Panel { id: id.to_string(), tabs, active_tab_id: active }
    }

    fn two_panels_horizontal() -> LayoutNode {
        LayoutNode::Split {
            id: "s1".into(),
            direction: "horizontal".into(),
            children: vec![panel("p1", &["files"]), panel("p2", &["canvas"])],
            sizes: vec![20.0, 80.0],
        }
    }

    #[test]
    fn split_and_close_panel() {
        let tree = two_panels_horizontal();
        let (tree, new_id) = split_panel_op(&tree, "p2", "horizontal", "after");
        assert!(!new_id.is_empty());
        let mut panels = Vec::new();
        collect_panels(&tree, &mut panels);
        assert_eq!(panels.len(), 3);
        let closed = close_panel_op(&tree, &new_id);
        assert!(closed.is_some());
        let closed = closed.unwrap();
        let mut panels = Vec::new();
        collect_panels(&closed, &mut panels);
        assert_eq!(panels.len(), 2);
        // 根即该面板（单面板树）不可关闭
        let root = panel("p1", &["files"]);
        assert!(close_panel_op(&root, "p1").is_none());
    }

    #[test]
    fn home_layout_pinned() {
        let mut ui = AppUiState {
            schema: UI_STATE_SCHEMA.into(),
            workspace_layouts: vec![
                WorkspaceLayout { id: "x".into(), name: "X".into(), tree: two_panels_horizontal() },
                WorkspaceLayout { id: HOME_LAYOUT_ID.into(), name: "主页".into(), tree: two_panels_horizontal() },
            ],
            active_layout_id: Some("x".into()),
            ..Default::default()
        };
        normalize(&mut ui);
        assert_eq!(ui.workspace_layouts[0].id, HOME_LAYOUT_ID);
        assert_eq!(ui.workspace_layouts.len(), 2);
        let _ = apply_layout_op(&mut ui, &LayoutOp::MoveLayout { from_index: 0, to_index: 1 });
        assert_eq!(ui.workspace_layouts[0].id, HOME_LAYOUT_ID);
        let _ = apply_layout_op(&mut ui, &LayoutOp::DeleteLayout { id: HOME_LAYOUT_ID.into() });
        assert_eq!(ui.workspace_layouts.len(), 2);
    }

    #[test]
    fn move_layout_reorders_but_home_stays_first() {
        let mut ui = ui_with(two_panels_horizontal());
        let _ = apply_layout_op(&mut ui, &LayoutOp::AddLayout); // 布局 1
        let _ = apply_layout_op(&mut ui, &LayoutOp::AddLayout); // 布局 2
        // [l1, 布局1, 布局2]；布局2 前移 → index 1
        let _ = apply_layout_op(&mut ui, &LayoutOp::MoveLayout { from_index: 2, to_index: 1 });
        assert_eq!(ui.workspace_layouts[1].name, "布局 2");
        assert_eq!(ui.workspace_layouts[2].name, "布局 1");
        // 越界忽略
        let _ = apply_layout_op(&mut ui, &LayoutOp::MoveLayout { from_index: 0, to_index: 5 });
        assert_eq!(ui.workspace_layouts[0].name, "L");
        // 主页固定置顶：normalize 补入主页后，主页不可移动、不可拖到主页之前
        normalize(&mut ui);
        assert_eq!(ui.workspace_layouts[0].id, HOME_LAYOUT_ID);
        let _ = apply_layout_op(&mut ui, &LayoutOp::MoveLayout { from_index: 0, to_index: 2 });
        assert_eq!(ui.workspace_layouts[0].id, HOME_LAYOUT_ID);
        let _ = apply_layout_op(&mut ui, &LayoutOp::MoveLayout { from_index: 2, to_index: 0 });
        assert_eq!(ui.workspace_layouts[0].id, HOME_LAYOUT_ID);
    }

    #[test]
    fn normalize_falls_back_active_and_filters_empty_detached() {
        // 激活布局失效 → 回退第一个（主页恒置顶）
        let mut ui = ui_with(two_panels_horizontal());
        ui.active_layout_id = Some("gone".into());
        ui.detached_windows.push(DetachedWindow {
            id: "w1".into(),
            tabs: vec![],
            active_tab_id: None,
            bounds: WindowBounds { x: 0.0, y: 0.0, width: 100.0, height: 100.0, scale: 0.0 },
        });
        normalize(&mut ui);
        assert_eq!(ui.schema, UI_STATE_SCHEMA);
        assert_eq!(ui.workspace_layouts[0].id, HOME_LAYOUT_ID);
        assert_eq!(ui.active_layout_id.as_deref(), Some(HOME_LAYOUT_ID));
        // 撕裂窗口空条目过滤
        assert!(ui.detached_windows.is_empty());
    }
}
