//! 布局 UI 渲染：场景/布局条 + 分割树（面板/标签组/视图）+ 分割把手几何（拖宽命中用）。
//!
//! 几何计算与渲染必须共用同一套度量常量（UiMetrics）：把手命中矩形由布局树 +
//! 主窗口内容区尺寸纯推导，与 iced FillPortion 的分配规则保持一致。

use std::collections::HashSet;

use atelyx_core::layout::{self, AppUiState, LayoutNode, TabItem};
use iced::widget::{button, column, container, row, scrollable, space, text, text_editor, text_input};
use iced::{Element, Length, Rectangle};

use crate::{App, Message};

pub struct UiMetrics;

impl UiMetrics {
    pub const STATUS_H: f32 = 30.0;
    pub const SCENE_BAR_H: f32 = 32.0;
    pub const LAYOUT_BAR_H: f32 = 34.0;
    pub const HEADER_H: f32 = Self::STATUS_H + Self::SCENE_BAR_H + Self::LAYOUT_BAR_H;
    pub const TAB_BAR_H: f32 = 32.0;
    pub const DIVIDER_W: f32 = 7.0;
    /// 标签定宽（拖拽命中与重排序的几何基准；标签显示名均为短词，定宽不截义）。
    pub const TAB_W: f32 = 96.0;
    /// 面板头容器内边距（标签条起点 = 面板左缘 + HEADER_INSET）。
    pub const HEADER_INSET: f32 = 4.0;
}

/// 内置视图显示名（面板头标签；插件 kind 原样显示）。
pub fn view_label(kind: &str) -> &str {
    match kind {
        "canvas" => "画布",
        "note" => "笔记",
        "table" => "表格",
        "files" => "文件",
        "search" => "搜索",
        "inspector" => "属性",
        "aichat" => "AI 对话",
        "collabroom" => "协作房间",
        "calendar" => "日历",
        "repohistory" => "仓库历史",
        "recent" => "最近打开",
        "empty" => "空面板",
        other => other,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum RenameTarget {
    Layout(String),
    Scene(String),
}

pub struct RenameState {
    pub target: RenameTarget,
    pub value: String,
}

/// 主窗口：状态行 + 场景条 + 布局条 + 激活布局树。
pub fn main_view(app: &App) -> Element<'_, Message> {
    let status_bar = row![
        button(text("打开/切换仓库").size(12))
            .padding([3, 8])
            .on_press(Message::PickVault),
        button(text(if matches!(app.theme, iced::Theme::Dark) { "浅色" } else { "深色" }).size(12))
            .padding([3, 8])
            .on_press(Message::ToggleTheme),
        text(app.status.clone()).size(12),
    ]
    .spacing(8)
    .padding([0, 8])
    .height(Length::Fixed(UiMetrics::STATUS_H));

    column![
        status_bar,
        container(scene_bar(app)).height(Length::Fixed(UiMetrics::SCENE_BAR_H)),
        container(layout_bar(app)).height(Length::Fixed(UiMetrics::LAYOUT_BAR_H)),
        render_node(app, active_tree(&app.ui)),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// 激活布局树的引用（无克隆）：顶层激活指向主页或场景内布局。
fn active_tree(ui: &AppUiState) -> &LayoutNode {
    let scene = layout::active_scene(ui);
    match ui.active_layout_id.as_deref() {
        Some(id) if id != layout::HOME_LAYOUT_ID => scene
            .layouts
            .iter()
            .find(|l| l.id.as_str() == id)
            .map(|l| &l.tree)
            .unwrap_or(&scene.home_layout.tree),
        _ => &scene.home_layout.tree,
    }
}

/// 撕裂窗口：单面板标签组 + 条目级操作（拖回/图钉/关闭）。
pub fn detached_view<'a>(app: &'a App, window_id: &str) -> Element<'a, Message> {
    let Some(w) = app.ui.detached_windows.iter().find(|w| w.id == window_id) else {
        return space().into();
    };

    let mut tab_bar = row![].spacing(2);
    for t in &w.tabs {
        let is_active = w.active_tab_id.as_deref() == Some(t.id.as_str());
        let label = if t.locked {
            format!("🔒{}", view_label(&t.view))
        } else {
            view_label(&t.view).to_string()
        };
        tab_bar = tab_bar.push(
            button(text(label).size(12))
                .padding([3, 4])
                .width(Length::Fixed(UiMetrics::TAB_W))
                .style(if is_active { button::primary } else { button::text })
                .on_press_maybe(
                    (!is_active).then(|| Message::DetachedTab(w.id.clone(), t.id.clone())),
                ),
        );
    }

    let mut actions = row![].spacing(2);
    let active = w
        .tabs
        .iter()
        .find(|t| Some(t.id.as_str()) == w.active_tab_id.as_deref());
    if let Some(t) = active {
        actions = actions.push(small_button(
            if t.locked { "解锁" } else { "锁定" },
            Message::DetachedTabLock(w.id.clone(), t.id.clone()),
        ));
        actions = actions.push(
            button(text("关标签").size(11))
                .padding([3, 6])
                .on_press_maybe(
                    (!t.locked).then(|| Message::DetachedTabClose(w.id.clone(), t.id.clone())),
                ),
        );
        actions = actions.push(small_button("拖回主窗", Message::DetachedDockBack(w.id.clone())));
    }
    if w.options.hide_on_blur {
        actions = actions.push(small_button(
            if w.pinned { "已钉" } else { "图钉" },
            Message::DetachedPin(w.id.clone()),
        ));
    }
    actions = actions.push(small_button("关闭窗口", Message::DetachedClose(w.id.clone())));

    let content = active
        .map(|t| view_content(app, &t.view))
        .unwrap_or_else(|| placeholder("空面板"));

    let hovered = app.drag_hover.as_ref().is_some_and(|h| {
        app.drag.is_some() && h.zone.detached_id.as_deref() == Some(window_id)
    });
    container(
        column![
            container(
                row![
                    tab_bar,
                    iced::widget::Space::new().width(Length::Fill),
                    actions,
                ]
                .spacing(8)
            )
            .padding([2.0, UiMetrics::HEADER_INSET])
            .height(Length::Fixed(UiMetrics::TAB_BAR_H)),
            content,
        ]
        .width(Length::Fill)
        .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .style(move |theme: &iced::Theme| {
        if hovered {
            container::Style {
                border: iced::Border {
                    color: theme.palette().primary,
                    width: 2.0,
                    radius: 0.0.into(),
                },
                ..Default::default()
            }
        } else {
            container::Style::default()
        }
    })
    .into()
}

fn scene_bar(app: &App) -> Element<'_, Message> {
    let mut bar = row![].spacing(4);
    for s in &app.ui.scenes {
        let is_active = app.ui.active_scene_id.as_deref() == Some(s.id.as_str());
        bar = bar.push(
            button(text(&s.name).size(12))
                .padding([3, 8])
                .style(if is_active { button::primary } else { button::text })
                .on_press(Message::SceneActivate(s.id.clone())),
        );
    }
    bar = bar.push(
        button(text("＋场景").size(12))
            .padding([3, 8])
            .on_press(Message::SceneNew),
    );
    // 激活场景（非默认）的重命名/删除；重命名在途 = 输入框 + 确认
    if let Some(id) = app.ui.active_scene_id.clone() {
        if id.as_str() != layout::DEFAULT_SCENE_ID {
            if let Some(r) = &app.rename {
                if r.target == RenameTarget::Scene(id.clone()) {
                    bar = bar.push(rename_input(r));
                }
            } else {
                bar = bar
                    .push(small_button("重命名", Message::SceneRenameStart(id.clone())))
                    .push(small_button("删除场景", Message::SceneDelete(id)));
            }
        }
    }
    scrollable(bar)
        .direction(scrollable::Direction::Horizontal(
            scrollable::Scrollbar::new().width(6.0),
        ))
        .width(Length::Fill)
        .into()
}

fn layout_bar(app: &App) -> Element<'_, Message> {
    let mut bar = row![].spacing(4);
    // 合成序 0 = 固定主页，场景内布局从 1 起（与 MoveLayout 索引语义一致）
    let active_layout_id = app.ui.active_layout_id.clone();
    bar = bar.push(
        button(text("主页").size(12))
            .padding([3, 8])
            .style(if active_layout_id.as_deref() == Some(layout::HOME_LAYOUT_ID) {
                button::primary
            } else {
                button::text
            })
            .on_press(Message::LayoutActivate(layout::HOME_LAYOUT_ID.into())),
    );
    let scene = layout::active_scene(&app.ui);
    for l in &scene.layouts {
        let is_active = active_layout_id.as_deref() == Some(l.id.as_str());
        bar = bar.push(
            button(text(&l.name).size(12))
                .padding([3, 8])
                .style(if is_active { button::primary } else { button::text })
                .on_press(Message::LayoutActivate(l.id.clone())),
        );
    }
    bar = bar.push(
        button(text("＋布局").size(12))
            .padding([3, 8])
            .on_press(Message::LayoutNew),
    );
    // 激活布局（非主页）的重命名/删除/排序；重命名在途 = 输入框 + 确认
    if let Some(id) = active_layout_id {
        if id.as_str() != layout::HOME_LAYOUT_ID {
            if let Some(r) = &app.rename {
                if r.target == RenameTarget::Layout(id) {
                    bar = bar.push(rename_input(r));
                }
            } else if scene.layouts.iter().any(|l| l.id == id) {
                bar = bar
                    .push(small_button("◀", Message::LayoutShift(id.clone(), true)))
                    .push(small_button("▶", Message::LayoutShift(id.clone(), false)))
                    .push(small_button("重命名", Message::LayoutRenameStart(id.clone())))
                    .push(small_button("删除布局", Message::LayoutDelete(id)));
            }
        }
    }
    scrollable(bar)
        .direction(scrollable::Direction::Horizontal(
            scrollable::Scrollbar::new().width(6.0),
        ))
        .width(Length::Fill)
        .into()
}

fn rename_input(r: &RenameState) -> Element<'_, Message> {
    row![
        text_input("新名称", &r.value)
            .size(12)
            .width(Length::Fixed(140.0))
            .on_input(match r.target {
                RenameTarget::Layout(_) => Message::LayoutRenameInput,
                RenameTarget::Scene(_) => Message::SceneRenameInput,
            })
            .on_submit(Message::LayoutRenameCommit),
        small_button("确定", Message::LayoutRenameCommit),
    ]
    .spacing(4)
    .into()
}

fn small_button(label: &str, msg: Message) -> iced::widget::Button<'_, Message> {
    button(text(label.to_string()).size(11)).padding([3, 6])
        .on_press_maybe(Some(msg))
}

/// 布局树节点渲染：Split = 按 sizes 比例分配 + 把手；Panel = 标签组 + 面板操作。
pub fn render_node<'a>(app: &'a App, node: &'a LayoutNode) -> Element<'a, Message> {
    match node {
        LayoutNode::Split { direction, children, sizes, .. } => {
            let horizontal = direction == "horizontal";
            let sum = sizes.iter().sum::<f64>().max(1e-9);
            let portions: Vec<u16> = sizes
                .iter()
                .map(|s| (((s / sum) * 1000.0).round() as u16).max(1))
                .collect();
            // 子区域容器按方向定主轴尺寸，几何（walk_dividers）与此分配规则保持一致
            let sized = |elem: Element<'a, Message>, i: usize| {
                if horizontal {
                    container(elem)
                        .width(Length::FillPortion(portions[i]))
                        .height(Length::Fill)
                } else {
                    container(elem)
                        .width(Length::Fill)
                        .height(Length::FillPortion(portions[i]))
                }
            };
            if horizontal {
                let mut r = row![].height(Length::Fill);
                for (i, child) in children.iter().enumerate() {
                    r = r.push(sized(render_node(app, child), i));
                    if i + 1 < children.len() {
                        r = r.push(divider());
                    }
                }
                r.into()
            } else {
                let mut c = column![].width(Length::Fill);
                for (i, child) in children.iter().enumerate() {
                    c = c.push(sized(render_node(app, child), i));
                    if i + 1 < children.len() {
                        c = c.push(divider());
                    }
                }
                c.into()
            }
        }
        LayoutNode::Panel { id, tabs, active_tab_id } => panel_view(app, id, tabs, active_tab_id),
    }
}

fn divider() -> Element<'static, Message> {
    container(space())
        .width(Length::Fixed(UiMetrics::DIVIDER_W))
        .height(Length::Fill)
        .style(|theme: &iced::Theme| container::Style {
            background: Some(theme.palette().text.scale_alpha(0.12).into()),
            ..Default::default()
        })
        .into()
}

fn panel_view<'a>(
    app: &'a App,
    panel_id: &str,
    tabs: &'a [TabItem],
    active_tab_id: &Option<String>,
) -> Element<'a, Message> {
    let mut tab_bar = row![].spacing(2);
    for t in tabs {
        let is_active = active_tab_id.as_deref() == Some(t.id.as_str());
        let label = if t.locked {
            format!("🔒{}", view_label(&t.view))
        } else {
            view_label(&t.view).to_string()
        };
        tab_bar = tab_bar.push(
            button(text(label).size(12))
                .padding([3, 4])
                .width(Length::Fixed(UiMetrics::TAB_W))
                .style(if is_active { button::primary } else { button::text })
                .on_press_maybe(
                    (!is_active).then(|| Message::PanelTab(panel_id.to_string(), t.id.clone())),
                ),
        );
    }

    let mut actions = row![].spacing(2);
    let active = tabs.iter().find(|t| Some(t.id.as_str()) == active_tab_id.as_deref());
    if let Some(t) = active {
        actions = actions.push(small_button(
            if t.locked { "解锁" } else { "锁定" },
            Message::PanelTabLock(panel_id.to_string(), t.id.clone()),
        ));
        actions = actions.push(
            button(text("关标签").size(11))
                .padding([3, 6])
                .on_press_maybe(
                    (!t.locked).then(|| Message::PanelTabClose(panel_id.to_string(), t.id.clone())),
                ),
        );
    }
    actions = actions
        .push(small_button("右分", Message::PanelSplit(panel_id.to_string(), "horizontal".into())))
        .push(small_button("下分", Message::PanelSplit(panel_id.to_string(), "vertical".into())))
        .push(small_button("关面板", Message::PanelClose(panel_id.to_string())));
    if let Some(t) = active.filter(|t| !t.locked) {
        actions = actions.push(small_button(
            "撕出",
            Message::PanelTearOff(panel_id.to_string(), t.id.clone()),
        ));
    }

    let content: Element<'_, Message> = match active {
        None => placeholder("空面板"),
        Some(t) => view_content(app, &t.view),
    };

    let hovered = app.drag_hover.as_ref().is_some_and(|h| {
        app.drag.is_some() && h.zone.panel_id.as_deref() == Some(panel_id)
    });
    container(
        column![
            container(
                row![
                    tab_bar,
                    iced::widget::Space::new().width(Length::Fill),
                    actions,
                ]
                .spacing(8)
            )
            .padding([2.0, UiMetrics::HEADER_INSET])
            .height(Length::Fixed(UiMetrics::TAB_BAR_H)),
            content,
        ]
        .width(Length::Fill)
        .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .style(move |theme: &iced::Theme| {
        if hovered {
            container::Style {
                border: iced::Border {
                    color: theme.palette().primary,
                    width: 2.0,
                    radius: 0.0.into(),
                },
                ..Default::default()
            }
        } else {
            container::Style::default()
        }
    })
    .into()
}

/// 按视图 kind 分派内容（M2 阶段 files/note 可用，其余占位）。
fn view_content<'a>(app: &'a App, kind: &str) -> Element<'a, Message> {
    match kind {
        "files" => files_view(app),
        "note" => note_view(app),
        kind => placeholder(&format!("视图待实现：{}", view_label(kind))),
    }
}

fn note_view(app: &App) -> Element<'_, Message> {
    if app.ui.last_note_file.is_some() {
        text_editor(&app.editor)
            .height(Length::Fill)
            .on_action(Message::Edited)
            .into()
    } else {
        placeholder("在文件面板选择文件打开")
    }
}

/// 一条侧栏可见行：由展开集 + 子目录缓存摊平而来。
struct TreeRow {
    rel: String,
    name: String,
    depth: usize,
    is_dir: bool,
    expanded: bool,
}

fn files_view(app: &App) -> Element<'_, Message> {
    if app.vault_root.is_none() {
        return placeholder("未打开仓库（顶部按钮选择）");
    }
    let expanded: HashSet<&str> = app
        .ui
        .file_explorer_expanded
        .iter()
        .map(|s| s.as_str())
        .collect();
    let mut tree_col = column![];
    for r in tree_rows(app, &expanded) {
        let indent = iced::Padding {
            left: 8.0 + r.depth as f32 * 16.0,
            ..Default::default()
        };
        let marker = if app.ui.last_note_file.as_deref() == Some(r.rel.as_str()) {
            "• "
        } else {
            ""
        };
        let label = if r.is_dir {
            format!("{} {}", if r.expanded { "▾" } else { "▸" }, r.name)
        } else {
            format!("  {marker}{0}", r.name)
        };
        let msg = if r.is_dir {
            Message::ToggleDir(r.rel.clone())
        } else {
            Message::OpenFile(r.rel.clone())
        };
        tree_col = tree_col.push(
            button(text(label).size(13).width(Length::Fill))
                .padding(indent)
                .style(button::text)
                .on_press(msg)
                .width(Length::Fill),
        );
    }
    scrollable(container(tree_col).width(Length::Fill))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// 由子项缓存 + 展开集摊平侧栏行（目录在前、忽略大小写按名升序）。
fn tree_rows(app: &App, expanded: &HashSet<&str>) -> Vec<TreeRow> {
    let mut rows = Vec::new();
    push_children(app, &mut rows, "", 0, expanded);
    rows
}

fn push_children(
    app: &App,
    rows: &mut Vec<TreeRow>,
    dir: &str,
    depth: usize,
    expanded: &HashSet<&str>,
) {
    let mut entries: Vec<(String, bool)> = app
        .children
        .get(dir)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .collect();
    entries.sort_by(|a, b| {
        b.1.cmp(&a.1).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase()))
    });
    for (rel, is_dir) in entries {
        let is_expanded = expanded.contains(rel.as_str());
        let name = rel.rsplit('/').next().unwrap_or(&rel).to_string();
        rows.push(TreeRow {
            name,
            depth,
            is_dir,
            expanded: is_expanded,
            rel: rel.clone(),
        });
        if is_dir && is_expanded {
            push_children(app, rows, &rel, depth + 1, expanded);
        }
    }
}

fn placeholder(label: &str) -> Element<'static, Message> {
    container(text(label.to_string()).size(13))
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into()
}

// ===== 分割把手几何（拖宽命中）=====

/// 一个把手的命中信息：矩形（窗口 logical 坐标）+ 所属 split 的调整参数。
pub struct DividerInfo {
    pub split_id: String,
    pub gap: usize,
    pub horizontal: bool,
    /// 分割区域总长（调整比例的换算基准）。
    pub split_len: f32,
    pub sizes: Vec<f64>,
    pub rect: Rectangle,
}

/// 枚举布局树中所有把手矩形（与 render_node 的 FillPortion 分配规则一致）。
pub fn enumerate_dividers(root: &LayoutNode, area: Rectangle) -> Vec<DividerInfo> {
    let mut out = Vec::new();
    walk_dividers(root, area, &mut out);
    out
}

fn walk_dividers(node: &LayoutNode, area: Rectangle, out: &mut Vec<DividerInfo>) {
    let LayoutNode::Split { id, direction, children, sizes } = node else {
        return;
    };
    let n = children.len();
    if n < 2 || sizes.len() != n {
        return;
    }
    let horizontal = direction == "horizontal";
    let total = if horizontal { area.width } else { area.height };
    let avail = total - (n as f32 - 1.0) * UiMetrics::DIVIDER_W;
    if avail <= 0.0 {
        return;
    }
    let sum = sizes.iter().sum::<f64>().max(1e-9);
    let mut pos = if horizontal { area.x } else { area.y };
    for (i, child) in children.iter().enumerate() {
        let len = (avail as f64 * (sizes[i] / sum)) as f32;
        let child_area = if horizontal {
            Rectangle { x: pos, y: area.y, width: len, height: area.height }
        } else {
            Rectangle { x: area.x, y: pos, width: area.width, height: len }
        };
        walk_dividers(child, child_area, out);
        pos += len;
        if i + 1 < n {
            let rect = if horizontal {
                Rectangle { x: pos, y: area.y, width: UiMetrics::DIVIDER_W, height: area.height }
            } else {
                Rectangle { x: area.x, y: pos, width: area.width, height: UiMetrics::DIVIDER_W }
            };
            out.push(DividerInfo {
                split_id: id.clone(),
                gap: i,
                horizontal,
                split_len: total,
                sizes: sizes.clone(),
                rect,
            });
            pos += UiMetrics::DIVIDER_W;
        }
    }
}

// ===== 面板落点几何（拖拽命中）=====

/// 一个可停靠面板的落点信息（全局 logical 坐标；与渲染分配规则一致）。
pub struct PanelZone {
    /// 主窗口树面板 id（撕裂窗口为 None）。
    pub panel_id: Option<String>,
    /// 撕裂窗口条目 id（主窗口面板为 None）。
    pub detached_id: Option<String>,
    /// 面板客户区矩形（含标签条）。
    pub rect: Rectangle,
    /// 标签数（重排序插入索引上限）。
    pub tab_count: usize,
}

/// 枚举全部窗口的可停靠面板矩形（拖拽会话期间位置尺寸视为稳定）。
pub fn panel_zones(app: &App) -> Vec<PanelZone> {
    let mut out = Vec::new();
    if let Some(main_id) = app.main_id {
        if let Some(pos) = app.os_positions.get(&main_id) {
            let area = Rectangle {
                x: pos.x,
                y: pos.y + UiMetrics::HEADER_H,
                width: app.main_size.width,
                height: (app.main_size.height - UiMetrics::HEADER_H).max(0.0),
            };
            walk_panels(&layout::active_layout(&app.ui).tree, area, &mut out);
        }
    }
    for (os, mid) in &app.detached_os {
        if let (Some(pos), Some(sz)) = (app.os_positions.get(os), app.os_sizes.get(os)) {
            let tab_count = app
                .ui
                .detached_windows
                .iter()
                .find(|w| &w.id == mid)
                .map(|w| w.tabs.len())
                .unwrap_or(0);
            out.push(PanelZone {
                panel_id: None,
                detached_id: Some(mid.clone()),
                rect: Rectangle { x: pos.x, y: pos.y, width: sz.width, height: sz.height },
                tab_count,
            });
        }
    }
    out
}

fn walk_panels(node: &LayoutNode, area: Rectangle, out: &mut Vec<PanelZone>) {
    match node {
        LayoutNode::Panel { id, tabs, .. } => out.push(PanelZone {
            panel_id: Some(id.clone()),
            detached_id: None,
            rect: area,
            tab_count: tabs.len(),
        }),
        LayoutNode::Split { direction, children, sizes, .. } => {
            let n = children.len();
            if n == 0 || sizes.len() != n {
                return;
            }
            let horizontal = direction == "horizontal";
            let total = if horizontal { area.width } else { area.height };
            let avail = total - (n as f32 - 1.0) * UiMetrics::DIVIDER_W;
            if avail <= 0.0 {
                return;
            }
            let sum = sizes.iter().sum::<f64>().max(1e-9);
            let mut pos = if horizontal { area.x } else { area.y };
            for (i, child) in children.iter().enumerate() {
                let len = (avail as f64 * (sizes[i] / sum)) as f32;
                let child_area = if horizontal {
                    Rectangle { x: pos, y: area.y, width: len, height: area.height }
                } else {
                    Rectangle { x: area.x, y: pos, width: area.width, height: len }
                };
                walk_panels(child, child_area, out);
                pos += len + UiMetrics::DIVIDER_W;
            }
        }
    }
}

/// 光标在标签条内的插入索引（半宽细分：左半 = 该标签前，右半 = 该标签后）。
pub fn tab_index_at(zone: &PanelZone, global_x: f32) -> usize {
    let idx = ((global_x - zone.rect.x - UiMetrics::HEADER_INSET) / UiMetrics::TAB_W).floor();
    let half_bumped = if (global_x - zone.rect.x - UiMetrics::HEADER_INSET) % UiMetrics::TAB_W
        > UiMetrics::TAB_W / 2.0
    {
        idx + 1.0
    } else {
        idx
    };
    (half_bumped.max(0.0) as usize).min(zone.tab_count)
}

/// 光标是否落在标签条内（含标签区横向范围；动作按钮区不算）。
pub fn in_tab_strip(zone: &PanelZone, global: iced::Point) -> bool {
    let rel_y = global.y - zone.rect.y;
    let rel_x = global.x - zone.rect.x;
    (0.0..=UiMetrics::TAB_BAR_H).contains(&rel_y)
        && (UiMetrics::HEADER_INSET
            ..=UiMetrics::HEADER_INSET + zone.tab_count as f32 * UiMetrics::TAB_W)
            .contains(&rel_x)
}
