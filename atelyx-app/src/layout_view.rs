//! 工作区 UI：标题栏（布局 tab / 场景选择器 / 窗口控制）+ 面板网格 + 状态栏
//! + 设置整页 + 弹层内容（菜单/选择器/就地确认）+ files/note 视图与占位视图。
//!
//! 几何计算与渲染共用同一套度量（UiMetrics）：把手/落点命中矩形由布局树 +
//! 窗口内容区尺寸纯推导，与 iced FillPortion 的分配规则保持一致。

use std::collections::HashSet;

use iced::alignment::{Horizontal, Vertical};
use iced::widget::{button, column, container, mouse_area, row, scrollable, space, stack, text};
use iced::{Element, Font, Length, Padding, Point, Rectangle, Size, border, widget::container::Style};

use atelyx_core::layout::{self, AppUiState, LayoutNode, TabItem};

use crate::ui::controls::{self, Variant};
use crate::ui::icon::icon;
use crate::ui::menu::MenuEntry;
use crate::ui::palette as pal;
use crate::ui::popup::{self, PopupSpec};
use crate::ui::{FONT_FAMILY, notify};
use crate::{App, DraftKind, Message, NoteMode, markdown_edit, markdown_view};

pub struct UiMetrics;

impl UiMetrics {
    pub const TITLEBAR_H: f32 = pal::TITLEBAR_H;
    pub const STATUSBAR_H: f32 = pal::STATUSBAR_H;
    pub const TAB_BAR_H: f32 = pal::TAB_BAR_H;
    pub const DIVIDER_W: f32 = 7.0;
    /// 标签定宽（拖拽命中与重排序的几何基准；标签显示名均为短词，定宽不截义）。
    pub const TAB_W: f32 = pal::TAB_W;
    /// 面板头容器内边距（标签条起点 = 面板左缘 + HEADER_INSET）。
    pub const HEADER_INSET: f32 = 4.0;
}

/// 内置视图 kind 顺序（右键菜单/选择器的呈现序）。
pub const VIEW_KINDS: &[&str] = &[
    "canvas", "note", "table", "files", "search", "inspector", "aichat", "collabroom",
    "calendar", "repohistory", "recent",
];

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

/// 内置视图图标（lucide 名；未知 kind 用拼图兜底）。
pub fn view_icon(kind: &str) -> &'static str {
    match kind {
        "canvas" => "palette",
        "note" => "file-text",
        "table" => "table",
        "files" => "files",
        "search" => "search",
        "inspector" => "info",
        "aichat" => "sparkles",
        "collabroom" => "users",
        "calendar" => "calendar-days",
        "repohistory" => "history",
        "recent" => "clock",
        "empty" => "layout-template",
        _ => "puzzle",
    }
}

/// 估算文字宽度（拉丁 ≈6.2px、CJK ≈12px / 12px 字号；按字号线性缩放）。
/// 弹层锚点与布局 tab 宽度用；与真实 shaping 有 ±10% 以内偏差，仅用于定位不用于截断。
pub fn estimate_text_width(text: &str, font_size: f32) -> f32 {
    let k = font_size / 12.0;
    let w: f32 = text
        .chars()
        .map(|c| {
            if c.is_ascii() {
                6.2
            } else {
                12.0
            }
        })
        .sum();
    w * k
}

/// 等宽字体（计数/扩展名标签）。
fn mono() -> Font {
    Font {
        family: iced::font::Family::Monospace,
        ..Font::with_name(FONT_FAMILY)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum RenameTarget {
    Layout(String),
    Scene(String),
    File(String),
    Folder(String),
    Draft { dir: String, kind: DraftKind },
}

pub struct RenameState {
    pub target: RenameTarget,
    pub value: String,
}

/// 弹层内容形态（几何与交互数据随形态携带）。
#[derive(Clone, Debug)]
pub enum PopupState {
    SceneSelector { confirm: Option<String> },
    LayoutTabMenu { layout_id: String },
    PanelMenu { panel_id: String },
    TabContext { panel_id: String, tab_id: String },
    ViewPicker { panel_id: String },
    DetachedPanelMenu { window_id: String },
    DetachedTabContext { window_id: String, tab_id: String },
    SortMenu,
    FileMenu { rel: String, is_dir: bool, is_root: bool },
    NoteMenu,
}

impl PopupState {
    fn width(&self) -> f32 {
        match self {
            PopupState::SceneSelector { .. } => 208.0,
            PopupState::LayoutTabMenu { .. } => 144.0,
            PopupState::PanelMenu { .. } => 160.0,
            PopupState::TabContext { .. } | PopupState::DetachedTabContext { .. } => 176.0,
            PopupState::ViewPicker { .. } => 160.0,
            PopupState::DetachedPanelMenu { .. } => 160.0,
            PopupState::SortMenu => 160.0,
            PopupState::FileMenu { .. } => 176.0,
            PopupState::NoteMenu => 144.0,
        }
    }
}

/// 打开弹层的请求：锚点（窗口局部坐标）+ 触发器顶边 + 内容形态。
#[derive(Clone, Debug)]
pub struct PopupOpen {
    pub point: Point,
    pub flip_y: Option<f32>,
    pub state: PopupState,
}

/// 按钮触发的弹层：锚点在按钮下缘（光标 y + 16 ≈ 28px 按钮的中点下移），
/// 翻转线在按钮上缘，弹层不盖住触发按钮。
fn btn_popup(window: iced::window::Id, cursor: Point, state: PopupState) -> Message {
    Message::PopupOpen(
        window,
        PopupOpen {
            point: Point::new(cursor.x, cursor.y + 16.0),
            flip_y: Some(cursor.y - 12.0),
            state,
        },
    )
}

/// 右键菜单类弹层：锚点 = 光标点，无翻转。
fn ctx_popup(window: iced::window::Id, cursor: Point, state: PopupState) -> Message {
    Message::PopupOpen(window, PopupOpen { point: cursor, flip_y: None, state })
}

/// 主窗口：标题栏 + 内容（设置整页或面板网格）+ 状态栏，顶层叠弹层/提示/通知。
pub fn main_view<'a>(app: &'a App, window: iced::window::Id) -> Element<'a, Message> {
    let content: Element<'a, Message> = if app.settings_open {
        settings_view(app)
    } else {
        render_node(app, active_tree(&app.ui))
    };

    let base = column![
        titlebar(app, window),
        hline(),
        container(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_t| Style {
                background: Some(pal::BG_PRIMARY.into()),
                ..Style::default()
            }),
        container(space())
            .width(Length::Fill)
            .height(Length::Fixed(1.0))
            .style(|_t| Style {
                background: Some(pal::BORDER_SUBTLE.into()),
                ..Style::default()
            }),
        status_bar(app),
    ]
    .width(Length::Fill)
    .height(Length::Fill);

    with_overlays(app, window, base.into())
}

/// 弹层 / 悬停提示 / 通知的窗口内覆盖。
fn with_overlays<'a>(
    app: &'a App,
    window: iced::window::Id,
    base: Element<'a, Message>,
) -> Element<'a, Message> {
    let viewport = viewport_of(app, window);
    let mut element = base;

    // 弹层（同窗口至多一个；捕获层点击 = 外点关闭）
    if let Some((w, open)) = &app.popup {
        if *w == window {
            let (content, height) = popup_element(app, &open.state, app.popup_pane);
            let spec = PopupSpec {
                anchor: open.point,
                width: open.state.width(),
                height,
                flip_y: open.flip_y,
            };
            let geom = popup::resolve(&spec, viewport);
            element = popup::layer(
                element,
                geom,
                content,
                Message::Ui(crate::ui::UiMsg::PopupClose { window }),
            );
        }
    }

    // 悬停提示（显现期锚定最近光标下方）
    if let Some((w, label, _, shown)) = &app.tooltip {
        if *w == window && *shown {
            let tip = container(text(label.clone()).size(pal::FS_MICRO).color(pal::TEXT_SECONDARY))
                .padding(Padding::ZERO.horizontal(8.0).vertical(4.0))
                .max_width(260)
                .style(|_t| Style {
                    background: Some(pal::BG_OVERLAY.into()),
                    border: border::rounded(pal::RADIUS_SM).color(pal::BORDER).width(1.0),
                    ..Style::default()
                });
            let pos = app.cursor_for(window);
            let positioned = container(tip)
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(Horizontal::Left)
                .align_y(Vertical::Top)
                .padding(Padding::ZERO.left(pos.x).top(pos.y + 18.0));
            element = stack![element, mouse_area(positioned)
                .on_exit(Message::Ui(crate::ui::UiMsg::HoverTipEnd {
                    window,
                    label: label.clone(),
                }))]
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        }
    }

    // 通知（仅主窗口承载）
    if app.main_id == Some(window) && !app.notifications.is_empty() {
        element = stack![element, notify::layer(&app.notifications)]
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
    }

    element
}

fn viewport_of(app: &App, window: iced::window::Id) -> Size {
    if app.main_id == Some(window) {
        app.main_size
    } else {
        app.os_sizes
            .get(&window)
            .copied()
            .unwrap_or(Size::new(420.0, 560.0))
    }
}

// ===== 标题栏 =====

fn titlebar<'a>(app: &'a App, window: iced::window::Id) -> Element<'a, Message> {
    let cursor = app.cursor_for(window);

    // 布局 tab 条
    let mut tabs = row![].spacing(4);
    let home_active = app.ui.active_layout_id.as_deref() == Some(layout::HOME_LAYOUT_ID);
    tabs = tabs.push(layout_tab(app, "主页", layout::HOME_LAYOUT_ID, home_active, window, cursor));
    if let Some(scene) = app.active_scene() {
        for l in &scene.layouts {
            let is_active = app.ui.active_layout_id.as_deref() == Some(l.id.as_str());
            tabs = tabs.push(layout_tab(app, &l.name.clone(), &l.id.clone(), is_active, window, cursor));
        }
    }
    let tabs = scrollable(container(row![tabs,
        controls::icon_button("plus", 13.0, 28.0, Variant::Subtle,
            Some(Message::LayoutNew), Some((window, "新建布局".into()))),
    ]))
    .direction(scrollable::Direction::Horizontal(scrollable::Scrollbar::new().width(0.0).scroller_width(0.0)))
    .width(Length::Shrink)
    .height(Length::Fill);

    // 场景选择器
    let scene_name = app.active_scene().map(|s| s.name.clone()).unwrap_or_default();
    let selector = button(
        row![
            text(scene_name).size(pal::FS_CAPTION).color(pal::TEXT_SECONDARY),
            icon("chevron-down", 12.0, pal::TEXT_SECONDARY),
        ]
        .spacing(4),
    )
    .padding(Padding::ZERO.horizontal(8.0))
    .height(Length::Fixed(28.0))
    .style(hover_btn_style())
    .on_press(btn_popup(window, cursor, PopupState::SceneSelector { confirm: None }));

    // 右操作区
    let settings_active = app.settings_open;
    let settings_btn = controls::icon_button(
        "settings",
        16.0,
        controls::ICON_LG,
        if settings_active { Variant::AccentSoft } else { Variant::Ghost },
        Some(Message::SettingsToggle),
        Some((window, "设置".into())),
    );
    let fullscreen_btn = controls::icon_button(
        "maximize",
        16.0,
        controls::ICON_LG,
        Variant::Ghost,
        Some(Message::FullscreenToggle(window)),
        Some((window, "全屏".into())),
    );

    let brand = row![
        iced::widget::image(crate::ui::icon_handle())
            .width(Length::Fixed(16.0))
            .height(Length::Fixed(16.0)),
        text("Atelyx")
            .size(pal::FS_CAPTION)
            .color(pal::TEXT_SECONDARY)
            .font(crate::ui::semibold()),
    ]
    .spacing(6)
    .padding(Padding::ZERO.horizontal(8.0));

    // 空白区 = 拖拽区（按下拖动窗口 / 双击最大化）
    let drag_spring = mouse_area(space().width(Length::Fill).height(Length::Fill))
        .on_press(Message::TitleBarPress(window));

    // 右操作区单行排布：场景选择器 → 设置 → 全屏 → 窗口控制（无系统边框后为唯一标题栏）
    container(
        row![
            brand,
            tabs,
            drag_spring,
            selector,
            settings_btn,
            fullscreen_btn,
            controls::window_button(controls::WindowButtonKind::Minimize, Message::WindowMinimize(window)),
            controls::window_button(controls::WindowButtonKind::Maximize, Message::WindowMaximizeToggle(window)),
            controls::window_button(controls::WindowButtonKind::Close, Message::WindowCloseRequested(window)),
        ]
        .height(Length::Fill)
        .spacing(4)
        .align_y(Vertical::Center),
    )
    .width(Length::Fill)
    .height(Length::Fixed(pal::TITLEBAR_H))
    .style(|_t| Style {
        background: Some(pal::BG_SECONDARY.into()),
        ..Style::default()
    })
    .into()
}

/// 标题栏布局 tab（自适应宽度 48–120；激活 = 三档底 + 2px 金下边；右键 = tab 菜单，双击 = 重命名）。
fn layout_tab(
    app: &App,
    name: &str,
    id: &str,
    active: bool,
    window: iced::window::Id,
    cursor: Point,
) -> Element<'static, Message> {
    let width = (estimate_text_width(name, 12.0) + 20.0).clamp(48.0, 120.0);
    let text_color = if active { pal::TEXT_PRIMARY } else { pal::TEXT_SECONDARY };
    let renaming = matches!(
        &app.rename,
        Some(r) if r.target == RenameTarget::Layout(id.to_string())
    );
    let text_area: Element<'static, Message> = if renaming {
        controls::inline_input(
            "新名称",
            &app.rename.as_ref().map(|r| r.value.clone()).unwrap_or_default(),
            width - 20.0,
            Message::LayoutRenameInput,
            Message::LayoutRenameCommit,
        )
        .into()
    } else {
        container(
            text(name.to_string())
                .size(pal::FS_CAPTION)
                .color(text_color)
                .width(Length::Fill),
        )
        .width(Length::Fill)
        .height(Length::Fixed(24.0))
        .align_y(Vertical::Center)
        .padding(Padding::ZERO.left(12.0).right(8.0))
        .into()
    };
    let content = column![
        text_area,
        container(space())
            .height(Length::Fixed(2.0))
            .style(move |_t| Style {
                background: if active { Some(pal::ACCENT.into()) } else { None },
                ..Style::default()
            }),
    ]
    .width(Length::Fill);

    let underline = container(content)
        .width(Length::Fixed(width))
        .height(Length::Fixed(28.0))
        .style(move |_t| Style {
            background: if active { Some(pal::BG_TERTIARY.into()) } else { None },
            border: border::rounded(pal::RADIUS_SM),
            ..Style::default()
        });

    mouse_area(
        button(underline)
            .style(move |_t, status| button::Style {
                background: (!active
                    && matches!(status, button::Status::Hovered | button::Status::Pressed))
                .then_some(pal::BG_TERTIARY.into()),
                ..Default::default()
            })
            .on_press(Message::LayoutActivate(id.to_string())),
    )
    .on_right_press(ctx_popup(window, cursor, PopupState::LayoutTabMenu { layout_id: id.to_string() }))
    .on_double_click(Message::LayoutRenameStart(id.to_string()))
    .into()
}

/// 触发按钮/表单行的悬停底样式。
fn hover_btn_style() -> impl Fn(&iced::Theme, button::Status) -> button::Style {
    |_t, status| button::Style {
        background: matches!(status, button::Status::Hovered | button::Status::Pressed)
            .then_some(pal::BG_TERTIARY.into()),
        text_color: pal::TEXT_SECONDARY,
        ..Default::default()
    }
}

// ===== 状态栏 =====

fn status_bar(app: &App) -> Element<'static, Message> {
    let (icon_name, name) = match &app.vault_root {
        Some(root) => ("hard-drive", atelyx_core::vault::vault_display_name(root)),
        None => ("hard-drive", "未打开仓库".to_string()),
    };
    let mut left = row![
        icon(icon_name, 12.0, pal::TEXT_MUTED),
        text(name).size(pal::FS_MICRO).color(pal::TEXT_SECONDARY),
    ]
    .spacing(6);
    if app.vault_root.is_some() {
        left = left
            .push(vrule())
            .push(
                text(match app.file_count {
                    Some(n) => format!("{n} 文件"),
                    None => "…".into(),
                })
                .size(pal::FS_MICRO)
                .color(pal::TEXT_MUTED)
                .font(mono()),
            );
    }
    container(row![left, space().width(Length::Fill)])
        .width(Length::Fill)
        .height(Length::Fixed(pal::STATUSBAR_H))
        .padding(Padding::ZERO.horizontal(12.0))
        .align_y(Vertical::Center)
        .style(|_t| Style {
            background: Some(pal::BG_SECONDARY.into()),
            ..Style::default()
        })
        .into()
}

fn vrule() -> Element<'static, Message> {
    container(space())
        .width(Length::Fixed(1.0))
        .height(Length::Fixed(12.0))
        .style(|_t| Style {
            background: Some(pal::BORDER.into()),
            ..Style::default()
        })
        .into()
}

/// 通栏水平细线（分隔用；iced Border 无按边着色，分隔线一律走独立元素）。
fn hline() -> Element<'static, Message> {
    container(space())
        .width(Length::Fill)
        .height(Length::Fixed(1.0))
        .style(|_t| Style {
            background: Some(pal::BORDER.into()),
            ..Style::default()
        })
        .into()
}

/// 竖向细线（左右分栏分隔）。
fn vline() -> Element<'static, Message> {
    container(space())
        .width(Length::Fixed(1.0))
        .height(Length::Fill)
        .style(|_t| Style {
            background: Some(pal::BORDER.into()),
            ..Style::default()
        })
        .into()
}

// ===== 面板网格 =====

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
        .style(|_t| Style {
            background: Some(pal::BORDER.into()),
            ..Style::default()
        })
        .into()
}

/// 面板：标签条（底色 + 1px 底线 + 激活标签盖线）+ 内容区。
fn panel_view<'a>(
    app: &'a App,
    panel_id: &str,
    tabs: &'a [TabItem],
    active_tab_id: &Option<String>,
) -> Element<'a, Message> {
    let mut tab_row = row![].spacing(2).height(Length::Fill);
    for t in tabs {
        tab_row = tab_row.push(panel_tab(app, panel_id, t, active_tab_id));
    }
    let active = tabs.iter().find(|t| Some(t.id.as_str()) == active_tab_id.as_deref());

    // 右侧：状态指示 + ≡ 菜单
    let mut right = row![].spacing(4).padding(Padding::ZERO.right(6.0));
    if let Some(t) = active {
        if t.view == "note" {
            if let Some(pill) = note_status_pill(app) {
                right = right.push(pill);
            }
        }
        right = right.push(mouse_area(
            button(icon("menu", 13.0, pal::TEXT_MUTED))
                .padding(Padding::ZERO.horizontal(6.0).vertical(4.0))
                .style(|_t, status| button::Style {
                    background: matches!(status, button::Status::Hovered | button::Status::Pressed)
                        .then_some(pal::HOVER.into()),
                    ..Default::default()
                }),
        )
        .on_press(btn_popup(
            app.main_id.unwrap_or(iced::window::Id::unique()),
            app.cursor_for(app.main_id.unwrap_or(iced::window::Id::unique())),
            PopupState::PanelMenu { panel_id: panel_id.to_string() },
        )));
    }

    let header = stack![
        container(space())
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_t| Style {
                background: Some(pal::BG_SECONDARY.into()),
                ..Style::default()
            }),
        container(space())
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(Padding::ZERO.top(UiMetrics::TAB_BAR_H as u16 as f32 - 1.0))
            .style(|_t| Style {
                background: Some(pal::BORDER.into()),
                ..Style::default()
            }),
        row![tab_row, space().width(Length::Fill), right]
            .height(Length::Fixed(UiMetrics::TAB_BAR_H))
            .padding(Padding::ZERO.left(UiMetrics::HEADER_INSET)),
    ]
    .width(Length::Fill)
    .height(Length::Fixed(UiMetrics::TAB_BAR_H));

    let content: Element<'a, Message> = match active {
        None => mouse_area(placeholder_panel("空面板"))
            .on_right_press(Message::PopupOpen(
                app.main_id.unwrap_or(iced::window::Id::unique()),
                PopupOpen {
                    point: app.cursor_for(app.main_id.unwrap_or(iced::window::Id::unique())),
                    flip_y: None,
                    state: PopupState::ViewPicker { panel_id: panel_id.to_string() },
                },
            ))
            .into(),
        Some(t) => view_content(app, &t.view),
    };

    let hovered = app.drag_hover.as_ref().is_some_and(|h| {
        app.drag.is_some() && h.zone.panel_id.as_deref() == Some(panel_id)
    });
    container(
        column![header, container(content).width(Length::Fill).height(Length::Fill)]
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .style(move |_t| {
        if hovered {
            Style {
                border: border::rounded(0.0).color(pal::ACCENT).width(2.0),
                ..Style::default()
            }
        } else {
            Style::default()
        }
    })
    .into()
}

/// 面板标签：定宽；激活 = 内容区同色底 + 2px 金下边（盖住标签条底线）。
fn panel_tab<'a>(
    app: &'a App,
    panel_id: &str,
    t: &'a TabItem,
    active_tab_id: &Option<String>,
) -> Element<'a, Message> {
    let active = active_tab_id.as_deref() == Some(t.id.as_str());
    // 文件视图打开文件时显示文件名，否则视图名
    let label = if t.view == "note" {
        app.ui
            .last_note_file
            .as_deref()
            .and_then(|f| f.rsplit('/').next())
            .unwrap_or(view_label(&t.view))
            .to_string()
    } else {
        view_label(&t.view).to_string()
    };
    let is_note_open = t.view == "note" && app.ui.last_note_file.is_some();

    let mut label_row = row![].spacing(4);
    label_row = label_row.push(
        text(label)
            .size(pal::FS_CAPTION)
            .color(if active { pal::TEXT_PRIMARY } else { pal::TEXT_SECONDARY }),
    );
    if t.locked {
        label_row = label_row.push(icon("lock", 9.0, pal::TEXT_MUTED));
    }
    if is_note_open && !t.locked {
        label_row = label_row.push(
            button(icon("x", 11.0, pal::TEXT_MUTED))
                .padding(2.0)
                .style(|_t, status| button::Style {
                    background: matches!(status, button::Status::Hovered | button::Status::Pressed)
                        .then_some(pal::HOVER.into()),
                    ..Default::default()
                })
                .on_press(Message::PanelTabClose(panel_id.to_string(), t.id.clone())),
        );
    }

    let window = app.main_id.unwrap_or(iced::window::Id::unique());
    let cursor = app.cursor_for(window);
    let content = container(label_row)
        .width(Length::Fixed(UiMetrics::TAB_W))
        .height(Length::Fill)
        .padding(Padding::ZERO.horizontal(8.0))
        .align_y(Vertical::Center);

    let btn = button(container(column![
        content,
        container(space()).height(Length::Fixed(2.0)).style(move |_t| Style {
            background: if active { Some(pal::ACCENT.into()) } else { None },
            ..Style::default()
        }),
    ])
    .height(Length::Fill))
    .width(Length::Fixed(UiMetrics::TAB_W))
    .height(Length::Fixed(UiMetrics::TAB_BAR_H))
    .style(move |_t, status| button::Style {
        background: if active {
            Some(pal::BG_PRIMARY.into())
        } else if matches!(status, button::Status::Hovered | button::Status::Pressed) {
            Some(pal::HOVER.into())
        } else {
            None
        },
        ..Default::default()
    })
    .on_press_maybe((!active).then(|| Message::PanelTab(panel_id.to_string(), t.id.clone())));

    mouse_area(btn)
        .on_right_press(ctx_popup(window, cursor, PopupState::TabContext { panel_id: panel_id.to_string(), tab_id: t.id.clone() }))
        .into()
}

/// 笔记保存状态徽标（错误 > 未保存；干净与未打开文件不显示）。
fn note_status_pill(app: &App) -> Option<Element<'static, Message>> {
    if app.ui.last_note_file.is_none() {
        return None;
    }
    if app.save_error.is_some() {
        Some(controls::status_pill(controls::PillStatus::Error, "保存失败"))
    } else if app.dirty {
        Some(controls::status_pill(controls::PillStatus::Pending, "未保存"))
    } else {
        None
    }
}

/// 按视图 kind 分派内容（files/note 可用，其余占位）。
fn view_content<'a>(app: &'a App, kind: &str) -> Element<'a, Message> {
    match kind {
        "files" => files_view(app),
        "note" => note_view(app),
        kind => placeholder_view(kind),
    }
}

/// 占位视图：视图图标 + 名称 + 未实现说明。
fn placeholder_view(kind: &str) -> Element<'static, Message> {
    controls::empty_state(
        view_icon(kind),
        view_label(kind).to_string(),
        "该视图尚未实现".to_string(),
        None,
    )
}

fn placeholder_panel(label: &str) -> Element<'static, Message> {
    container(text(label.to_string()).size(pal::FS_UI).color(pal::TEXT_MUTED))
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into()
}

// ===== 撕裂窗口 =====

/// 撕裂窗口：标题栏（拖拽区 + 标题 + 图钉/最小化/关闭）+ 标签条 + 内容。
pub fn detached_view<'a>(
    app: &'a App,
    window: iced::window::Id,
    model_id: &str,
) -> Element<'a, Message> {
    let Some(w) = app.ui.detached_windows.iter().find(|w| w.id == model_id) else {
        return space().into();
    };
    let cursor = app.cursor_for(window);

    let mut tab_row = row![].spacing(2).height(Length::Fill);
    for t in &w.tabs {
        let active = w.active_tab_id.as_deref() == Some(t.id.as_str());
        let mut label_row = row![].spacing(4);
        label_row = label_row.push(
            text(view_label(&t.view).to_string())
                .size(pal::FS_CAPTION)
                .color(if active { pal::TEXT_PRIMARY } else { pal::TEXT_SECONDARY }),
        );
        if t.locked {
            label_row = label_row.push(icon("lock", 9.0, pal::TEXT_MUTED));
        }
        let content = container(label_row)
            .width(Length::Fixed(UiMetrics::TAB_W))
            .height(Length::Fill)
            .padding(Padding::ZERO.horizontal(8.0))
            .align_y(Vertical::Center);
        tab_row = tab_row.push(
            mouse_area(
                button(container(column![
                    content,
                    container(space()).height(Length::Fixed(2.0)).style(move |_t| Style {
                        background: if active { Some(pal::ACCENT.into()) } else { None },
                        ..Style::default()
                    }),
                ])
                .height(Length::Fill))
                .width(Length::Fixed(UiMetrics::TAB_W))
                .height(Length::Fixed(UiMetrics::TAB_BAR_H))
                .style(move |_t, status| button::Style {
                    background: if active {
                        Some(pal::BG_PRIMARY.into())
                    } else if matches!(status, button::Status::Hovered | button::Status::Pressed) {
                        Some(pal::HOVER.into())
                    } else {
                        None
                    },
                    ..Default::default()
                })
                .on_press_maybe(
                    (!active).then(|| Message::DetachedTab(w.id.clone(), t.id.clone())),
                ),
            )
            .on_right_press(ctx_popup(window, cursor, PopupState::DetachedTabContext { window_id: w.id.clone(), tab_id: t.id.clone(), })),
        );
    }

    let mut right = row![].spacing(4).padding(Padding::ZERO.right(6.0));
    right = right.push(
        button(icon("menu", 13.0, pal::TEXT_MUTED))
            .padding(Padding::ZERO.horizontal(6.0).vertical(4.0))
            .style(|_t, status| button::Style {
                background: matches!(status, button::Status::Hovered | button::Status::Pressed)
                    .then_some(pal::HOVER.into()),
                ..Default::default()
            })
            .on_press(btn_popup(window, cursor, PopupState::DetachedPanelMenu { window_id: w.id.clone() })),
    );

    let header = stack![
        container(space()).width(Length::Fill).height(Length::Fill).style(|_t| Style {
            background: Some(pal::BG_SECONDARY.into()),
            ..Style::default()
        }),
        container(space())
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(Padding::ZERO.top(UiMetrics::TAB_BAR_H as u16 as f32 - 1.0))
            .style(|_t| Style {
                background: Some(pal::BORDER.into()),
                ..Style::default()
            }),
        row![
            container(tab_row).padding(Padding::ZERO.left(UiMetrics::HEADER_INSET)),
            space().width(Length::Fill),
            right,
        ]
        .height(Length::Fixed(UiMetrics::TAB_BAR_H)),
    ]
    .width(Length::Fill)
    .height(Length::Fixed(UiMetrics::TAB_BAR_H));

    // 撕裂窗口标题栏：标题文本 + 弹性拖拽区 + 图钉/最小化/关闭
    let active_title = w
        .active_tab_id
        .as_deref()
        .and_then(|tid| w.tabs.iter().find(|t| t.id == tid))
        .map(|t| view_label(&t.view).to_string())
        .unwrap_or_else(|| "Atelyx".into());
    let pin_btn: Element<'static, Message> = mouse_area(
        button(icon("lock-open", 14.0, pal::TEXT_SECONDARY))
            .padding(6.0)
            .style(|_t, status| button::Style {
                background: matches!(status, button::Status::Hovered | button::Status::Pressed)
                    .then_some(pal::HOVER.into()),
                ..Default::default()
            }),
    )
    .on_press(Message::DetachedPin(w.id.clone()))
    .into();
    let titlebar = container(
        row![
            text(active_title).size(pal::FS_CAPTION).color(pal::TEXT_SECONDARY),
            mouse_area(space().width(Length::Fill).height(Length::Fill))
                .on_press(Message::TitleBarPress(window)),
            pin_btn,
            controls::window_button(controls::WindowButtonKind::Minimize, Message::WindowMinimize(window)),
            controls::window_button(controls::WindowButtonKind::Close, Message::WindowCloseRequested(window)),
        ],
    )
    .width(Length::Fill)
    .height(Length::Fixed(pal::TITLEBAR_H))
    .align_y(Vertical::Center)
    .style(|_t| Style {
        background: Some(pal::BG_SECONDARY.into()),
        ..Style::default()
    });

    let content = w
        .active_tab_id
        .as_deref()
        .and_then(|tid| w.tabs.iter().find(|t| t.id == tid))
        .map(|t| view_content(app, &t.view))
        .unwrap_or_else(|| placeholder_panel("空面板"));

    let hovered = app.drag_hover.as_ref().is_some_and(|h| {
        app.drag.is_some() && h.zone.detached_id.as_deref() == Some(model_id)
    });
    let base = column![titlebar, header, container(content).width(Length::Fill).height(Length::Fill)]
        .width(Length::Fill)
        .height(Length::Fill);
    let element: Element<'a, Message> = container(base)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_t| {
            if hovered {
                Style {
                    border: border::rounded(0.0).color(pal::ACCENT).width(2.0),
                    ..Style::default()
                }
            } else {
                Style::default()
            }
        })
        .into();
    with_overlays(app, window, element)
}

// ===== files 视图 =====

/// 一条侧栏可见行。
struct TreeRow {
    rel: String,
    name: String,
    depth: usize,
    is_dir: bool,
    expanded: bool,
    active: bool,
}

fn files_view(app: &App) -> Element<'_, Message> {
    if app.vault_root.is_none() {
        return controls::empty_state(
            "folder-open",
            "还没有仓库",
            "打开一个文件夹作为仓库。",
            Some(("打开文件夹".into(), Message::PickVault)),
        );
    }
    let window = app.main_id.unwrap_or(iced::window::Id::unique());
    let cursor = app.cursor_for(window);

    // 工具条
    let toolbar = container(
        row![
            controls::icon_button("folder-open", 15.0, controls::ICON_MD, Variant::Ghost,
                Some(Message::PickVault), Some((window, "打开文件夹为仓库".into()))),
            controls::icon_button("cloud", 15.0, controls::ICON_MD, Variant::Ghost,
                Some(Message::NotImplemented("协作空间")), Some((window, "新增协作空间".into()))),
            controls::icon_button("arrow-up-down", 14.0, controls::ICON_MD, Variant::Ghost,
                Some(btn_popup(window, cursor, PopupState::SortMenu)), Some((window, "排序方式".into()))),
            space().width(Length::Fill),
            controls::icon_button("chevrons-up-down", 14.0, controls::ICON_MD, Variant::Ghost,
                Some(Message::NotImplemented("展开全部文件夹")), Some((window, "展开全部文件夹".into()))),
        ]
        .spacing(2),
    )
    .width(Length::Fill)
    .padding(Padding::ZERO.horizontal(8.0).vertical(6.0))
    .style(|_t| Style {
        background: Some(pal::BG_SECONDARY.into()),
        ..Style::default()
    });

    // 树体（根空白右键 = 根目录新建菜单）
    let expanded: HashSet<&str> =
        app.ui.file_explorer_expanded.iter().map(|s| s.as_str()).collect();
    let mut tree_col = column![].padding(Padding::ZERO.top(6.0).bottom(12.0));
    tree_col = tree_col.push(vault_row(app, window, cursor, &expanded));
    // 新建草稿输入行：dir = "" 时落在仓库行之后，否则落在对应目录行之后
    let draft_dir = match &app.rename {
        Some(rs) => match &rs.target {
            RenameTarget::Draft { dir, .. } => Some(dir.clone()),
            _ => None,
        },
        None => None,
    };
    if draft_dir.as_deref() == Some("") {
        tree_col = tree_col.push(draft_input_row(app, 0));
    }
    if expanded.contains("") {
        for r in tree_rows(app, &expanded) {
            // 就地重命名的输入行替换对应行
            if let Some(input) = file_inline_input(app, &r) {
                tree_col = tree_col.push(input);
                continue;
            }
            tree_col = tree_col.push(tree_row_element(&r, window, cursor));
            if draft_dir.as_deref() == Some(r.rel.as_str()) {
                tree_col = tree_col.push(draft_input_row(app, r.depth + 1));
            }
        }
    }

    let tree: Element<'static, Message> = mouse_area(
        scrollable(container(tree_col).width(Length::Fill))
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .on_right_press(ctx_popup(window, cursor, PopupState::FileMenu { rel: String::new(), is_dir: true, is_root: true }))
    .into();

    column![toolbar, hline(), tree]
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// 仓库顶行（激活行金色淡底；右键 = 仓库行菜单）。
fn vault_row(
    app: &App,
    window: iced::window::Id,
    cursor: Point,
    expanded: &HashSet<&str>,
) -> Element<'static, Message> {
    let name = app
        .vault_root
        .as_ref()
        .map(|r| atelyx_core::vault::vault_display_name(r))
        .unwrap_or_default();
    let vault_open = expanded.contains("");
    let row = row![
        icon(
            if vault_open { "chevron-down" } else { "chevron-right" },
            14.0,
            pal::TEXT_MUTED,
        ),
        icon("hard-drive", 14.0, pal::ACCENT),
        text(name)
            .size(pal::FS_CAPTION)
            .color(pal::TEXT_PRIMARY)
            .font(crate::ui::semibold()),
    ]
    .spacing(8)
    .padding(Padding::ZERO.horizontal(8.0).vertical(4.0))
    .width(Length::Fill);

    let content = container(row)
        .width(Length::Fill)
        .height(Length::Fixed(32.0))
        .align_y(Vertical::Center)
        .style(|_t| Style {
            background: Some(pal::ACCENT.scale_alpha(0.2).into()),
            border: border::rounded(pal::RADIUS_XS),
            ..Style::default()
        });

    mouse_area(
        button(content)
            .width(Length::Fill)
            .style(|_t, status| button::Style {
                background: matches!(status, button::Status::Hovered | button::Status::Pressed)
                    .then_some(pal::HOVER.into()),
                ..Default::default()
            })
            .on_press(Message::ToggleDir(String::new())),
    )
    .on_right_press(ctx_popup(window, cursor, PopupState::FileMenu { rel: String::new(), is_dir: true, is_root: false }))
    .into()
}

/// 目录/文件行（行高 32、缩进 12+16×depth；激活文件 = 金淡底 + 左侧 2px 金轨）。
fn tree_row_element(
    r: &TreeRow,
    window: iced::window::Id,
    cursor: Point,
) -> Element<'static, Message> {
    let indent = 12.0 + r.depth as f32 * 16.0;
    let (icon_name, icon_color) = if r.is_dir {
        ("folder", pal::TEXT_MUTED)
    } else {
        let active_icon = pal::ACCENT;
        let color = if r.active { active_icon } else { pal::TEXT_SECONDARY };
        (file_icon(&r.name), color)
    };
    let name_color = if r.is_dir {
        pal::TEXT_SECONDARY
    } else if r.active {
        pal::ACCENT
    } else {
        pal::TEXT_SECONDARY
    };

    let mut content = row![].spacing(8);
    if r.is_dir {
        content = content.push(icon(
            if r.expanded { "chevron-down" } else { "chevron-right" },
            14.0,
            pal::TEXT_MUTED,
        ));
    } else {
        // 文件行让出 chevron 槽位，保持与目录行文字对齐
        content = content.push(space().width(Length::Fixed(14.0)));
    }
    content = content.push(icon(icon_name, 14.0, icon_color));
    content = content.push(text(r.name.clone()).size(pal::FS_CAPTION).color(name_color));
    if !r.is_dir {
        content = content.push(space().width(Length::Fill));
        content = content.push(
            text(ext_label(&r.name))
                .size(pal::FS_MICRO)
                .color(if r.active { pal::ACCENT } else { pal::TEXT_MUTED })
                .font(mono()),
        );
    }

    let bg = if r.active { pal::ACCENT_SOFT } else { iced::Color::TRANSPARENT };
    let active = r.active;
    let line = container(space())
        .width(Length::Fixed(2.0))
        .height(Length::Fill)
        .style(move |_t| Style {
            background: active.then_some(pal::ACCENT.into()),
            ..Style::default()
        });
    let body = stack![
        container(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(Padding::ZERO.left(indent + 4.0).top(4.0).bottom(4.0).right(8.0))
            .align_y(Vertical::Center)
            .style(move |_t| Style {
                background: Some(bg.into()),
                border: border::rounded(pal::RADIUS_XS),
                ..Style::default()
            }),
        line,
    ];

    let msg = if r.is_dir {
        Message::ToggleDir(r.rel.clone())
    } else {
        Message::OpenFile(r.rel.clone())
    };
    let state = PopupState::FileMenu {
        rel: r.rel.clone(),
        is_dir: r.is_dir,
        is_root: false,
    };
    mouse_area(
        button(body)
            .width(Length::Fill)
            .height(Length::Fixed(32.0))
            .style(|_t, status| button::Style {
                background: matches!(status, button::Status::Hovered | button::Status::Pressed)
                    .then_some(pal::HOVER.into()),
                ..Default::default()
            })
            .on_press(msg),
    )
    .on_right_press(Message::PopupOpen(window, PopupOpen { point: cursor, flip_y: None, state }))
    .into()
}

/// 新建草稿的输入行（渲染在目标目录内首个位置）。
fn draft_input_row(app: &App, depth: usize) -> Element<'static, Message> {
    let (value, placeholder) = match &app.rename {
        Some(rs) => match &rs.target {
            RenameTarget::Draft { kind, .. } => (
                rs.value.clone(),
                match kind {
                    DraftKind::Note => "笔记名称",
                    DraftKind::Folder => "文件夹名称",
                },
            ),
            _ => (String::new(), ""),
        },
        None => (String::new(), ""),
    };
    container(
        controls::inline_input(
            placeholder,
            &value,
            160.0,
            Message::LayoutRenameInput,
            Message::LayoutRenameCommit,
        ),
    )
    .width(Length::Fill)
    .height(Length::Fixed(32.0))
    .padding(Padding::ZERO.left(12.0 + depth as f32 * 16.0 + 22.0).right(8.0))
    .align_y(Vertical::Center)
    .into()
}

/// 就地重命名的输入行（渲染在对应行位置）。
fn file_inline_input(app: &App, r: &TreeRow) -> Option<Element<'static, Message>> {
    let indent = 12.0 + r.depth as f32 * 16.0;
    let (value, placeholder) = match &app.rename {
        Some(rs) => match &rs.target {
            RenameTarget::File(f) if *f == r.rel => (rs.value.clone(), ""),
            RenameTarget::Folder(d) if *d == r.rel => (rs.value.clone(), ""),
            _ => return None,
        },
        None => return None,
    };
    Some(
        container(
            controls::inline_input(
                placeholder,
                &value,
                160.0,
                Message::LayoutRenameInput,
                Message::LayoutRenameCommit,
            ),
        )
        .width(Length::Fill)
        .height(Length::Fixed(32.0))
        .padding(Padding::ZERO.left(indent + 22.0).right(8.0))
        .align_y(Vertical::Center)
        .into(),
    )
}

fn file_icon(name: &str) -> &'static str {
    let lower = name.to_lowercase();
    if lower.ends_with(".atlx") {
        "file-text"
    } else if lower.ends_with(".canvas") {
        "layout-dashboard"
    } else if lower.ends_with(".md") {
        "sticky-note"
    } else if lower.ends_with(".atb") {
        "table"
    } else {
        "paperclip"
    }
}

/// 扩展名标签（ATLX / CANVAS / MD / ATB / 大写扩展名）。
fn ext_label(name: &str) -> String {
    match name.rsplit('.').next() {
        Some(ext) if !ext.is_empty() && ext != name => ext.to_uppercase(),
        _ => String::new(),
    }
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
        let active = !is_dir && app.ui.last_note_file.as_deref() == Some(rel.as_str());
        rows.push(TreeRow { name, depth, is_dir, expanded: is_expanded, rel: rel.clone(), active });
        if is_dir && is_expanded {
            push_children(app, rows, &rel, depth + 1, expanded);
        }
    }
}

// ===== note 视图 =====

/// note 视图：工具条（模式分段 + ···菜单）+ 对应档位的内容面。
fn note_view(app: &App) -> Element<'_, Message> {
    if app.ui.last_note_file.is_none() {
        return controls::empty_state(
            "file-text",
            "笔记",
            "在左侧文件面板选择文件打开。",
            None,
        );
    }
    let window = app.main_id.unwrap_or(iced::window::Id::unique());
    let cursor = app.cursor_for(window);

    let mode_control = container(
        row![
            mode_button("只读", NoteMode::Read, app.note_mode),
            mode_button("预览编辑", NoteMode::Edit, app.note_mode),
            mode_button("源码", NoteMode::Source, app.note_mode),
        ]
        .spacing(2),
    )
    .padding(2.0)
    .style(|_t| Style {
        background: Some(pal::BG_SUNKEN.into()),
        border: border::rounded(pal::RADIUS_SM).color(pal::BORDER).width(1.0),
        ..Style::default()
    });

    let toolbar = container(
        row![
            space().width(Length::Fill),
            mode_control,
            button(icon("ellipsis", 15.0, pal::TEXT_MUTED))
                .padding(Padding::ZERO.horizontal(4.0).vertical(4.0))
                .style(|_t, status| button::Style {
                    background: matches!(status, button::Status::Hovered | button::Status::Pressed)
                        .then_some(pal::HOVER.into()),
                    ..Default::default()
                })
                .on_press(btn_popup(window, cursor, PopupState::NoteMenu)),
        ]
        .spacing(8),
    )
    .width(Length::Fill)
    .padding(Padding::ZERO.horizontal(12.0).vertical(4.0))
    .style(|_t| Style {
        background: Some(pal::BG_SECONDARY.into()),
        ..Style::default()
    });

    let content: Element<'_, Message> = match app.note_mode {
        NoteMode::Source => iced::widget::text_editor(&app.source_editor)
            .height(Length::Fill)
            .on_action(Message::Edited)
            .into(),
        NoteMode::Read => match app.preview_doc.as_ref() {
            Some(doc) => markdown_view::document_view(doc, &app.theme),
            None => placeholder_panel("正在渲染…"),
        },
        NoteMode::Edit => edit_view(app),
    };

    column![toolbar, hline(), container(content).width(Length::Fill).height(Length::Fill)]
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// 模式分段按钮：当前档 = 强调淡底 + 强调字，其余档次级字色。
fn mode_button(label: &str, mode: NoteMode, current: NoteMode) -> Element<'static, Message> {
    let active = mode == current;
    button(
        text(label.to_string())
            .size(pal::FS_CAPTION)
            .color(if active { pal::ACCENT } else { pal::TEXT_SECONDARY }),
    )
    .padding(Padding::ZERO.horizontal(10.0).vertical(3.0))
    .style(move |_t, _status| button::Style {
        background: active.then_some(pal::ACCENT_SOFT.into()),
        ..Default::default()
    })
    .on_press(Message::NoteMode(mode))
    .into()
}

/// 预览编辑档：块行与空行行交给编辑面容器统一排布与承接鼠标；非活动块由只读块
/// 控件承载（视觉逐点一致），仅活动块与选区锚点块回显源码。
fn edit_view(app: &App) -> Element<'_, Message> {
    use markdown_edit::SurfaceRow;

    let Some(doc) = app.edit_doc.as_ref() else {
        return placeholder_panel("正在解析…");
    };
    let editable = app.editable_blocks();
    let pal_edit = markdown_edit::EditPalette::from_theme(&app.theme);
    let (sel_from, sel_to) = app.edit.selection();
    let mut rows = Vec::new();
    let mut prev: Option<usize> = None;
    let caret = app.edit.caret.min(doc.source.len());
    // 空文档：补一个空源码块，给光标一个宿主
    if doc.blocks.is_empty() {
        rows.push(SurfaceRow::Block {
            index: 0,
            editable: true,
            exact: true,
            covered: false,
            element: markdown_edit::SourceBlock {
                from: 0,
                to: 0,
                text: &doc.source,
                caret,
                anchor: app.edit.anchor,
                preedit: app.edit.preedit.clone(),
                source: markdown_view::empty_block_meta().source,
                pal: pal_edit,
                opts: &app.edit_opts,
                focused: app.edit.focused,
            }
            .into(),
        });
        return scrollable(markdown_edit::EditSurface::new(
            rows,
            pal_edit,
            caret,
            app.edit.focused,
        ))
        .width(Length::Fill)
        .height(Length::Fill)
        .into();
    }
    for (index, chunk) in markdown_view::block_chunks(&doc.source, &doc.blocks, &app.theme, true)
        .into_iter()
        .enumerate()
    {
        let editable = editable.contains(&index);
        let meta = app.edit_meta.get(index);
        if chunk.gap_before > 0 {
            if let Some(p) = prev {
                let from = app.edit_meta[p].to;
                let covered = sel_to > sel_from && sel_from <= from && sel_to >= chunk.from;
                rows.push(SurfaceRow::Gap {
                    from,
                    to: chunk.from,
                    covered,
                    element: markdown_view::gap(chunk.gap_before),
                });
            }
        }
        if editable {
            rows.push(SurfaceRow::Block {
                index,
                editable: true,
                exact: true,
                covered: false,
                element: markdown_edit::SourceBlock {
                    from: chunk.from,
                    to: chunk.to,
                    text: &doc.source,
                    caret,
                    anchor: app.edit.anchor,
                    preedit: app.edit.preedit.clone(),
                    source: chunk.source,
                    pal: pal_edit,
                    opts: &app.edit_opts,
                    focused: app.edit.focused && caret >= chunk.from && caret <= chunk.to,
                }
                .into(),
            });
        } else {
            let covered = sel_to > sel_from && sel_from < chunk.to && sel_to > chunk.from;
            rows.push(SurfaceRow::Block {
                index,
                editable: false,
                exact: meta.is_some_and(|m| m.exact),
                covered,
                element: chunk.element,
            });
        }
        prev = Some(index);
    }
    scrollable(markdown_edit::EditSurface::new(
        rows,
        pal_edit,
        caret,
        app.edit.focused,
    ))
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

// ===== 设置整页 =====

struct SettingsItem {
    id: &'static str,
    label: &'static str,
    icon: &'static str,
}

const APP_SETTINGS: &[SettingsItem] = &[
    SettingsItem { id: "general", label: "通用", icon: "settings" },
    SettingsItem { id: "theme", label: "主题", icon: "palette" },
    SettingsItem { id: "collab", label: "多人协作", icon: "users" },
    SettingsItem { id: "editorPrefs", label: "编辑器", icon: "pen-line" },
    SettingsItem { id: "shortcuts", label: "快捷键", icon: "keyboard" },
    SettingsItem { id: "plugins", label: "插件", icon: "puzzle" },
];

const REPO_SETTINGS: &[SettingsItem] = &[
    SettingsItem { id: "providers", label: "模型供应商", icon: "server" },
    SettingsItem { id: "modelServices", label: "模型服务", icon: "bot" },
    SettingsItem { id: "agents", label: "Agent", icon: "sparkles" },
    SettingsItem { id: "search", label: "联网搜索", icon: "search" },
    SettingsItem { id: "files", label: "文件与路径", icon: "folder-tree" },
    SettingsItem { id: "workspace", label: "工作区", icon: "panels-top-left" },
    SettingsItem { id: "editor", label: "编辑器", icon: "pen-line" },
];

fn settings_item(item: &SettingsItem, active_tab: &str) -> Element<'static, Message> {
    let active = item.id == active_tab;
    button(
        row![
            icon(item.icon, 14.0, if active { pal::ACCENT } else { pal::TEXT_SECONDARY }),
            text(item.label)
                .size(pal::FS_CAPTION)
                .color(if active { pal::ACCENT } else { pal::TEXT_SECONDARY }),
        ]
        .spacing(8),
    )
    .padding(Padding::ZERO.horizontal(10.0).vertical(6.0))
    .width(Length::Fill)
    .style(move |_t, status| button::Style {
        background: if active {
            Some(pal::ACCENT_SOFT.into())
        } else if matches!(status, button::Status::Hovered | button::Status::Pressed) {
            Some(pal::HOVER.into())
        } else {
            None
        },
        text_color: pal::TEXT_PRIMARY,
        border: border::rounded(pal::RADIUS_SM),
        ..Default::default()
    })
    .on_press(Message::SettingsTab(item.id.to_string()))
    .into()
}

/// 设置整页：左栏分组导航（应用级 / 仓库级 / 关于）+ 右侧分组内容占位。
fn settings_view(app: &App) -> Element<'static, Message> {
    let active = app.settings_tab.as_str();
    let mut nav = column![].spacing(2);
    nav = nav.push(group_head("应用级", "全局共享", false));
    for item in APP_SETTINGS {
        nav = nav.push(settings_item(item, active));
    }
    if app.vault_root.is_some() {
        let vault_name = app
            .vault_root
            .as_ref()
            .map(|r| atelyx_core::vault::vault_display_name(r))
            .unwrap_or_default();
        nav = nav.push(
            container(controls::h_divider()).padding(Padding::ZERO.top(6.0).bottom(6.0)),
        );
        nav = nav.push(group_head("仓库级", &vault_name, true));
        for item in REPO_SETTINGS {
            nav = nav.push(settings_item(item, active));
        }
    }
    nav = nav.push(container(controls::h_divider()).padding(Padding::ZERO.top(6.0).bottom(6.0)));
    nav = nav.push(settings_item(
        &SettingsItem { id: "about", label: "关于", icon: "info" },
        active,
    ));

    let aside = container(
        column![
            row![
                text("设置").size(pal::FS_BODY).color(pal::TEXT_PRIMARY).font(crate::ui::semibold()),
                space().width(Length::Fill),
                button(icon("x", 14.0, pal::TEXT_SECONDARY))
                    .padding(4.0)
                    .style(|_t, status| button::Style {
                        background: matches!(status, button::Status::Hovered | button::Status::Pressed)
                            .then_some(pal::HOVER.into()),
                        ..Default::default()
                    })
                    .on_press(Message::SettingsToggle),
            ]
            .padding(Padding::ZERO.horizontal(12.0).vertical(12.0)),
            scrollable(nav).width(Length::Fill).height(Length::Fill),
        ]
        .width(Length::Fill)
        .height(Length::Fill),
    )
    .width(Length::Fixed(220.0))
    .height(Length::Fill)
    .style(|_t| Style {
        background: Some(pal::BG_PRIMARY.into()),
        ..Style::default()
    });

    let (title, desc) = tab_meta(active);
    let body = column![
        container(
            column![
                text(title)
                    .size(pal::FS_H2)
                    .color(pal::TEXT_PRIMARY)
                    .font(crate::ui::semibold()),
                text(desc).size(pal::FS_CAPTION).color(pal::TEXT_MUTED),
            ]
            .spacing(4),
        )
        .width(Length::Fill)
        .padding(Padding::ZERO.horizontal(20.0).vertical(12.0)),
        hline(),
        controls::empty_state(
            "settings",
            "无设置项",
            "此分区当前没有可配置的设置项。",
            None,
        ),
    ]
    .width(Length::Fill)
    .height(Length::Fill);

    row![aside, vline(), container(body).width(Length::Fill).height(Length::Fill)]
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn group_head(label: &str, hint: &str, repo_level: bool) -> Element<'static, Message> {
    let bar_color = if repo_level { pal::ACCENT } else { pal::BORDER_STRONG };
    let text_color = if repo_level { pal::ACCENT } else { pal::TEXT_MUTED };
    container(
        row![
            container(space())
                .width(Length::Fixed(3.0))
                .height(Length::Fixed(11.0))
                .style(move |_t| Style {
                    background: Some(bar_color.into()),
                    border: border::rounded(2.0),
                    ..Style::default()
                }),
            text(label.to_string())
                .size(pal::FS_MICRO)
                .color(text_color)
                .font(crate::ui::semibold()),
            text(hint.to_string()).size(pal::FS_MICRO).color(pal::TEXT_MUTED).font(mono()),
        ]
        .spacing(6),
    )
    .padding(Padding::ZERO.left(8.0).top(10.0).bottom(6.0))
    .into()
}

fn tab_meta(tab: &str) -> (&'static str, &'static str) {
    match tab {
        "general" => ("通用", "全库共享 · 存于本机应用配置"),
        "theme" => ("主题", "全库共享 · 存于本机应用配置"),
        "collab" => ("多人协作", "全库共享 · 存于本机应用配置"),
        "editorPrefs" => ("编辑器", "全库共享 · 存于本机应用配置"),
        "shortcuts" => ("快捷键", "全库共享 · 存于本机应用配置"),
        "plugins" => ("插件", "全库共享 · 存于本机应用配置"),
        "providers" => ("模型供应商", "作用于当前仓库 · 配置随该仓库走"),
        "modelServices" => ("模型服务", "作用于当前仓库 · 配置随该仓库走"),
        "agents" => ("Agent", "作用于当前仓库 · 配置随该仓库走"),
        "search" => ("联网搜索", "作用于当前仓库 · 配置随该仓库走"),
        "files" => ("文件与路径", "作用于当前仓库 · 配置随该仓库走"),
        "workspace" => ("工作区", "作用于当前仓库 · 配置随该仓库走"),
        "editor" => ("编辑器", "作用于当前仓库 · 配置随该仓库走"),
        "about" => ("关于", "Logo · 版本 · 检查更新 · 项目主页"),
        _ => ("设置", ""),
    }
}

// ===== 弹层内容 =====

/// 弹层内容构造：按形态产出（元素, 估算高度）。
fn popup_element<'a>(
    app: &'a App,
    state: &PopupState,
    pane: usize,
) -> (Element<'a, Message>, f32) {
    let entries: Vec<(usize, MenuEntry)> = match state {
        PopupState::SceneSelector { confirm } => scene_selector_entries(app, confirm),
        PopupState::LayoutTabMenu { layout_id } => {
            let deletable = app
                .active_scene()
                .map(|s| s.layouts.len() > 1)
                .unwrap_or(false);
            let id = layout_id.clone();
            let mut entries = vec![(
                0usize,
                MenuEntry::action("重命名", Message::LayoutRenameStart(id.clone())),
            )];
            // 布局 tab 拖拽排序未复刻前，排序经菜单承担
            entries.push((0, MenuEntry::action("左移", Message::LayoutShift(id.clone(), true))));
            entries.push((0, MenuEntry::action("右移", Message::LayoutShift(id.clone(), false))));
            if deletable {
                entries.push((0, MenuEntry::danger("删除", "trash-2", Message::LayoutDelete(id))));
            }
            entries
        }
        PopupState::PanelMenu { panel_id } => panel_menu_entries(app, panel_id),
        PopupState::TabContext { panel_id, tab_id } => {
            tab_context_entries(app, panel_id, tab_id)
        }
        PopupState::ViewPicker { panel_id } => view_picker_entries(app, panel_id),
        PopupState::DetachedPanelMenu { window_id } => {
            let locked = app
                .ui
                .detached_windows
                .iter()
                .find(|w| w.id == *window_id)
                .and_then(|w| w.active_tab_id.clone())
                .map(|tid| {
                    app.ui
                        .detached_windows
                        .iter()
                        .find(|w| w.id == *window_id)
                        .and_then(|w| w.tabs.iter().find(|t| t.id == tid))
                        .is_some_and(|t| t.locked)
                })
                .unwrap_or(false);
            let wid = window_id.clone();
            vec![
                (
                    0,
                    MenuEntry::action(if locked { "解锁" } else { "锁定" },
                        Message::DetachedTabLock(wid.clone(), String::new())),
                ),
                (0, MenuEntry::action("拖回主窗", Message::DetachedDockBack(wid))),
            ]
        }
        PopupState::DetachedTabContext { window_id, tab_id } => {
            let locked = app
                .ui
                .detached_windows
                .iter()
                .find(|w| w.id == *window_id)
                .and_then(|w| w.tabs.iter().find(|t| t.id == *tab_id))
                .is_some_and(|t| t.locked);
            let (wid, tid) = (window_id.clone(), tab_id.clone());
            let mut entries: Vec<(usize, MenuEntry)> = vec![
                (
                    0,
                    MenuEntry::Action {
                        label: "关闭该标签".into(),
                        icon: Some("x"),
                        danger: false,
                        disabled: locked,
                        on_press: (!locked).then(|| Message::DetachedTabClose(wid.clone(), tid.clone())),
                    },
                ),
                (
                    0,
                    MenuEntry::Submenu {
                        label: "切换标签视图".into(),
                        icon: None,
                        disabled: locked,
                        pane: 1,
                    },
                ),
            ];
            // 视图清单：当前视图停用；他处占用停用；其余可切换
            let current_view = app
                .ui
                .detached_windows
                .iter()
                .find(|w| w.id == *window_id)
                .and_then(|w| w.tabs.iter().find(|t| t.id == *tab_id))
                .map(|t| t.view.clone());
            for kind in VIEW_KINDS {
                let entry = if current_view.as_deref() == Some(*kind) {
                    MenuEntry::disabled(format!("{}（当前）", view_label(kind)), Some(view_icon(kind)))
                } else if view_occupied(app, kind, None) {
                    MenuEntry::disabled(format!("{}（已占用）", view_label(kind)), Some(view_icon(kind)))
                } else {
                    MenuEntry::action_icon(
                        view_label(kind),
                        view_icon(kind),
                        Message::DetachedSetTabView {
                            window_id: wid.clone(),
                            tab_id: tid.clone(),
                            view: kind.to_string(),
                        },
                    )
                };
                entries.push((1, entry));
            }
            entries.push((1, MenuEntry::Back));
            entries
        }
        PopupState::SortMenu => vec![
            (0, MenuEntry::action("文件名 (A-Z)", Message::NotImplemented("排序"))),
            (0, MenuEntry::action("文件名 (Z-A)", Message::NotImplemented("排序"))),
            (0, MenuEntry::action("编辑时间 (从新到旧)", Message::NotImplemented("排序"))),
            (0, MenuEntry::action("编辑时间 (从旧到新)", Message::NotImplemented("排序"))),
        ],
        PopupState::FileMenu { rel, is_dir, is_root } => {
            file_menu_entries(rel, *is_dir, *is_root)
        }
        PopupState::NoteMenu => vec![
            (0, MenuEntry::action("添加笔记属性", Message::NotImplemented("笔记属性"))),
            (0, MenuEntry::action("历史记录", Message::NotImplemented("历史记录"))),
        ],
    };
    crate::ui::menu::pane_view(entries, pane)
}

/// 视图 kind 是否已被占用（主树全部标签 + 撕裂窗口全部标签；`except_tab` 排除自身）。
fn view_occupied(app: &App, kind: &str, except_tab: Option<&str>) -> bool {
    fn tree_has(node: &LayoutNode, kind: &str, except: Option<&str>) -> bool {
        match node {
            LayoutNode::Panel { tabs, .. } => tabs
                .iter()
                .any(|t| t.view == kind && except != Some(t.id.as_str())),
            LayoutNode::Split { children, .. } => {
                children.iter().any(|c| tree_has(c, kind, except))
            }
        }
    }
    let main_tree = layout::active_layout(&app.ui).tree;
    let main_hit = tree_has(&main_tree, kind, except_tab);
    let detached_hit = app.ui.detached_windows.iter().any(|w| {
        w.tabs
            .iter()
            .any(|t| t.view == kind && except_tab != Some(t.id.as_str()))
    });
    main_hit || detached_hit
}

/// 场景选择器：根面板（场景行 + 新建）与删除确认面板。
fn scene_selector_entries(app: &App, confirm: &Option<String>) -> Vec<(usize, MenuEntry<'static>)> {
    let mut root: Vec<(usize, MenuEntry<'static>)> = vec![(0, MenuEntry::Group("场景".into()))];
    let active_id = app.ui.active_scene_id.clone();
    for s in &app.ui.scenes {
        let is_active = active_id.as_deref() == Some(s.id.as_str());
        let id = s.id.clone();
        let name = s.name.clone();
        let is_default = s.id == layout::DEFAULT_SCENE_ID;
        // 场景行 = 激活按钮 + 行尾动作（默认场景无动作；重命名在途 = 行内输入）
        let renaming = matches!(
            &app.rename,
            Some(r) if r.target == RenameTarget::Scene(id.clone())
        );
        let name_area: Element<'static, Message> = if renaming {
            controls::inline_input(
                "新名称",
                &app.rename.as_ref().map(|r| r.value.clone()).unwrap_or_default(),
                96.0,
                Message::SceneRenameInput,
                Message::LayoutRenameCommit,
            )
            .into()
        } else {
            button(
                row![
                    text(name.clone())
                        .size(pal::FS_CAPTION)
                        .color(if is_active { pal::ACCENT } else { pal::TEXT_PRIMARY }),
                ]
                .push(if is_active {
                    icon("check", 12.0, pal::ACCENT)
                } else {
                    space().width(0.0).into()
                })
                .spacing(4),
            )
            .padding(Padding::ZERO.horizontal(8.0).vertical(6.0))
            .width(Length::Fill)
            .style(|_t, status| button::Style {
                background: matches!(status, button::Status::Hovered | button::Status::Pressed)
                    .then_some(pal::HOVER.into()),
                ..Default::default()
            })
            .on_press(Message::SceneActivate(id.clone()))
            .into()
        };
        let mut row = row![name_area].spacing(2);
        if !is_default {
            row = row.push(
                button(icon("pencil", 11.0, pal::TEXT_MUTED))
                    .padding(2.0)
                    .style(|_t, _s| button::Style::default())
                    .on_press(Message::SceneRenameStart(id.clone())),
            );
            row = row.push(
                button(icon("trash-2", 11.0, pal::DANGER))
                    .padding(2.0)
                    .style(|_t, _s| button::Style::default())
                    .on_press(Message::SceneDeleteAsk(id.clone())),
            );
        }
        root.push((0, MenuEntry::Custom(container(row).width(Length::Fill).into(), 34.0)));
    }
    root.push((0, MenuEntry::action_icon("新建场景", "plus", Message::SceneNew)));

    // 确认面板
    let confirm_entries: Vec<(usize, MenuEntry<'static>)> = if let Some(id) = confirm {
        let scene = app.ui.scenes.iter().find(|s| s.id == *id);
        let name = scene.map(|s| s.name.clone()).unwrap_or_default();
        let count = scene.map(|s| s.layouts.len()).unwrap_or(0);
        let pane = column![
            text(format!("删除场景「{name}」？"))
                .size(pal::FS_CAPTION)
                .color(pal::TEXT_PRIMARY),
            text(format!("其下 {count} 套布局将一并删除。"))
                .size(pal::FS_CAPTION)
                .color(pal::TEXT_MUTED),
            row![
                container(button("取消")
                    .padding(Padding::ZERO.horizontal(10.0).vertical(4.0))
                    .style(|_t, _s| button::Style {
                        text_color: pal::TEXT_SECONDARY,
                        border: border::rounded(pal::RADIUS_SM).color(pal::BORDER).width(1.0),
                        ..Default::default()
                    })
                    .on_press(Message::Ui(crate::ui::UiMsg::PopupPane(0))))
                .align_x(Horizontal::Right),
                button("删除")
                    .padding(Padding::ZERO.horizontal(10.0).vertical(4.0))
                    .style(|_t, _s| button::Style {
                        text_color: pal::DANGER,
                        border: border::rounded(pal::RADIUS_SM).color(pal::DANGER).width(1.0),
                        ..Default::default()
                    })
                    .on_press(Message::SceneDelete(id.clone())),
            ]
            .spacing(8)
            .align_y(Vertical::Center),
        ]
        .spacing(4)
        .padding(12.0)
        .width(Length::Fill);
        vec![(1, MenuEntry::Custom(pane.into(), 110.0))]
    } else {
        Vec::new()
    };
    root.into_iter().chain(confirm_entries).collect()
}

/// 面板 ≡ 菜单：锁定 / 删除面板（最后一个禁用）/ 分割。
fn panel_menu_entries(app: &App, panel_id: &str) -> Vec<(usize, MenuEntry<'static>)> {
    let active = find_active_tab_in(app, panel_id);
    let locked = active.as_ref().is_some_and(|(_, locked)| *locked);
    let main_tree = layout::active_layout(&app.ui).tree;
    let panel_count = count_panels(&main_tree);
    let pid = panel_id.to_string();
    let mut entries = Vec::new();
    if let Some((tab_id, _)) = &active {
        entries.push((
            0,
            MenuEntry::action_icon(
                if locked { "解锁" } else { "锁定" },
                if locked { "lock-open" } else { "lock" },
                Message::PanelTabLock(pid.clone(), tab_id.clone()),
            ),
        ));
    }
    if !locked {
        entries.push((
            0,
            MenuEntry::Action {
                label: "删除面板".into(),
                icon: Some("trash-2"),
                danger: true,
                disabled: panel_count <= 1,
                on_press: (panel_count > 1).then(|| Message::PanelClose(pid.clone())),
            },
        ));
        entries.push((0, MenuEntry::Divider));
        entries.push((0, MenuEntry::action("左右分割", Message::PanelSplit(pid.clone(), "horizontal".into()))));
        entries.push((0, MenuEntry::action("上下分割", Message::PanelSplit(pid, "vertical".into()))));
    }
    entries
}

/// 主树活动标签信息（(tab_id, locked)）。
fn find_active_tab_in(app: &App, panel_id: &str) -> Option<(String, bool)> {
    fn walk(node: &LayoutNode, panel_id: &str) -> Option<(String, bool)> {
        match node {
            LayoutNode::Panel { id, tabs, active_tab_id } if id == panel_id => {
                let active_id = active_tab_id.as_deref()?;
                tabs.iter()
                    .find(|t| t.id == active_id)
                    .map(|t| (t.id.clone(), t.locked))
            }
            LayoutNode::Split { children, .. } => children.iter().find_map(|c| walk(c, panel_id)),
            _ => None,
        }
    }
    // active_layout 返回 owned（克隆）；树上查询只取owned 数据，无借用外泄
    let tree = layout::active_layout(&app.ui).tree;
    walk(&tree, panel_id)
}

fn count_panels(node: &LayoutNode) -> usize {
    match node {
        LayoutNode::Panel { .. } => 1,
        LayoutNode::Split { children, .. } => children.iter().map(count_panels).sum(),
    }
}

/// 标签右键菜单：根面板（关闭 / 切换视图）与视图清单面板。
fn tab_context_entries(
    app: &App,
    panel_id: &str,
    tab_id: &str,
) -> Vec<(usize, MenuEntry<'static>)> {
    let tab = find_tab_in_active(app, tab_id);
    let locked = tab.as_ref().is_some_and(|(_, locked)| *locked);
    let current_view = tab.as_ref().map(|(view, _)| view.clone());
    let (pid, tid) = (panel_id.to_string(), tab_id.to_string());
    let mut entries: Vec<(usize, MenuEntry<'static>)> = vec![
        (
            0,
            MenuEntry::Action {
                label: "关闭该标签".into(),
                icon: Some("x"),
                danger: false,
                disabled: locked,
                on_press: (!locked).then(|| Message::PanelTabClose(pid.clone(), tid.clone())),
            },
        ),
        (
            0,
            MenuEntry::Submenu {
                label: "切换标签视图".into(),
                icon: None,
                disabled: locked,
                pane: 1,
            },
        ),
    ];
    for kind in VIEW_KINDS {
        let entry = if current_view.as_deref() == Some(*kind) {
            MenuEntry::disabled(format!("{}（当前）", view_label(kind)), Some(view_icon(kind)))
        } else if view_occupied(app, kind, Some(tab_id)) {
            MenuEntry::disabled(format!("{}（已占用）", view_label(kind)), Some(view_icon(kind)))
        } else {
            MenuEntry::action_icon(
                view_label(kind),
                view_icon(kind),
                Message::SetTabView { panel_id: pid.clone(), tab_id: tid.clone(), view: kind.to_string() },
            )
        };
        entries.push((1, entry));
    }
    entries.push((1, MenuEntry::Back));
    entries
}

/// 主树标签信息（(view, locked)）。
fn find_tab_in_active(app: &App, tab_id: &str) -> Option<(String, bool)> {
    fn walk(node: &LayoutNode, tab_id: &str) -> Option<(String, bool)> {
        match node {
            LayoutNode::Panel { tabs, .. } => tabs
                .iter()
                .find(|t| t.id == tab_id)
                .map(|t| (t.view.clone(), t.locked)),
            LayoutNode::Split { children, .. } => children.iter().find_map(|c| walk(c, tab_id)),
        }
    }
    let tree = layout::active_layout(&app.ui).tree;
    walk(&tree, tab_id)
}

/// 空面板右键的视图选择器：本组激活 / 已占用停用 / 其余添加。
fn view_picker_entries(app: &App, panel_id: &str) -> Vec<(usize, MenuEntry<'static>)> {
    let pid = panel_id.to_string();
    VIEW_KINDS
        .iter()
        .map(|kind| {
            let entry = if panel_has_view(app, panel_id, kind) {
                MenuEntry::disabled(format!("{}（本组）", view_label(kind)), Some(view_icon(kind)))
            } else if view_occupied(app, kind, None) {
                MenuEntry::disabled(format!("{}（已占用）", view_label(kind)), Some(view_icon(kind)))
            } else {
                MenuEntry::action_icon(
                    view_label(kind),
                    view_icon(kind),
                    Message::PanelAddView { panel_id: pid.clone(), view: kind.to_string() },
                )
            };
            (0, entry)
        })
        .collect()
}

fn panel_has_view(app: &App, panel_id: &str, kind: &str) -> bool {
    fn walk(node: &LayoutNode, panel_id: &str, kind: &str) -> bool {
        match node {
            LayoutNode::Panel { id, tabs, .. } if id == panel_id => {
                tabs.iter().any(|t| t.view == kind)
            }
            LayoutNode::Split { children, .. } => {
                children.iter().any(|c| walk(c, panel_id, kind))
            }
            _ => false,
        }
    }
    walk(&layout::active_layout(&app.ui).tree, panel_id, kind)
}

/// 文件树右键菜单：文件夹 / 文件 / 仓库行 / 根空白；删除走 pane 1 就地确认。
fn file_menu_entries(
    rel: &str,
    is_dir: bool,
    is_root: bool,
) -> Vec<(usize, MenuEntry<'static>)> {
    if rel.is_empty() && !is_root {
        // 仓库行
        return vec![
            (0, MenuEntry::action_icon("在文件管理器中打开", "folder-open", Message::RevealVault)),
            (0, MenuEntry::danger("从列表移除", "unlink", Message::NotImplemented("仓库列表管理"))),
        ];
    }

    let mut entries: Vec<(usize, MenuEntry)> = Vec::new();
    if is_dir {
        entries.push((0, MenuEntry::action_icon("新建画布", "file-text", Message::NotImplemented("画布"))));
        entries.push((0, MenuEntry::action_icon("新建笔记", "sticky-note",
            Message::DraftStart(rel.to_string(), DraftKind::Note))));
        entries.push((0, MenuEntry::action_icon("新建表格", "table", Message::NotImplemented("表格"))));
        entries.push((0, MenuEntry::action_icon("新建文件夹", "folder-plus",
            Message::DraftStart(rel.to_string(), DraftKind::Folder))));
        if !is_root {
            entries.push((0, MenuEntry::Divider));
            entries.push((0, MenuEntry::action_icon("图标颜色", "palette", Message::NotImplemented("文件夹颜色"))));
            entries.push((0, MenuEntry::Divider));
            entries.push((0, MenuEntry::action_icon("创建副本", "copy", Message::NotImplemented("创建副本"))));
            entries.push((0, MenuEntry::Divider));
            entries.push((0, MenuEntry::action_icon("重命名", "pencil",
                Message::RenameStartFolder(rel.to_string()))));
            entries.push((0, MenuEntry::danger("删除", "trash-2",
                Message::Ui(crate::ui::UiMsg::PopupPane(1)))));
        }
    } else {
        let lower = rel.to_lowercase();
        if lower.ends_with(".canvas") {
            entries.push((0, MenuEntry::action_icon("转换为画布", "file-output", Message::NotImplemented("画布转换"))));
            entries.push((0, MenuEntry::Divider));
        }
        if lower.ends_with(".md") {
            entries.push((0, MenuEntry::action_icon("注册为提示词", "bookmark-plus", Message::NotImplemented("提示词"))));
            entries.push((0, MenuEntry::Divider));
        }
        entries.push((0, MenuEntry::action_icon("创建副本", "copy", Message::NotImplemented("创建副本"))));
        entries.push((0, MenuEntry::Divider));
        entries.push((0, MenuEntry::action_icon("重命名", "pencil",
            Message::RenameStartFile(rel.to_string()))));
        entries.push((0, MenuEntry::danger("删除", "trash-2", Message::Ui(crate::ui::UiMsg::PopupPane(1)))));
    }

    // pane 1 = 删除就地确认
    if !rel.is_empty() {
        let kind = if is_dir { "文件夹" } else { "文件" };
        let confirm = column![
            text(format!("删除{kind}「{}」？", rel.rsplit('/').next().unwrap_or(rel)))
                .size(pal::FS_CAPTION)
                .color(pal::TEXT_PRIMARY),
            text("删除后不可恢复")
                .size(pal::FS_CAPTION)
                .color(pal::TEXT_MUTED),
            row![
                button("取消")
                    .padding(Padding::ZERO.horizontal(10.0).vertical(4.0))
                    .style(|_t, _s| button::Style {
                        text_color: pal::TEXT_SECONDARY,
                        border: border::rounded(pal::RADIUS_SM).color(pal::BORDER).width(1.0),
                        ..Default::default()
                    })
                    .on_press(Message::Ui(crate::ui::UiMsg::PopupPane(0))),
                button("确认删除")
                    .padding(Padding::ZERO.horizontal(10.0).vertical(4.0))
                    .style(|_t, _s| button::Style {
                        text_color: pal::DANGER,
                        border: border::rounded(pal::RADIUS_SM).color(pal::DANGER).width(1.0),
                        ..Default::default()
                    })
                    .on_press(Message::DeleteEntry(rel.to_string(), is_dir)),
            ]
            .spacing(8),
        ]
        .spacing(4)
        .padding(12.0)
        .width(Length::Fill);
        entries.push((1, MenuEntry::Custom(confirm.into(), 104.0)));
    }
    entries
}

// ===== 分割把手与落点几何（拖宽/拖拽命中）=====

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
            // 与 content_area 同源：标题栏下与状态栏上的两条 1px 分隔线各让 1px
            let area = Rectangle {
                x: pos.x,
                y: pos.y + UiMetrics::TITLEBAR_H + 1.0,
                width: app.main_size.width,
                height: (app.main_size.height
                    - UiMetrics::TITLEBAR_H
                    - UiMetrics::STATUSBAR_H
                    - 2.0)
                    .max(0.0),
            };
            walk_panels(&layout::active_layout(&app.ui).tree, area, &mut out);
        }
    }
    // 撕裂窗口落点矩形（标题栏不含分隔线，无 1px 偏差）
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
                rect: Rectangle {
                    x: pos.x,
                    y: pos.y + UiMetrics::TITLEBAR_H,
                    width: sz.width,
                    height: (sz.height - UiMetrics::TITLEBAR_H).max(0.0),
                },
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
