//! 工作区 UI：标题栏（布局 tab / 场景选择器 / 窗口控制）+ 面板网格 + 状态栏
//! + 设置整页 + 弹层内容（菜单/选择器/就地确认）+ 占位视图。
//!
//! 几何计算与渲染共用同一套度量（UiMetrics）：把手/落点命中矩形由布局树 +
//! 窗口内容区尺寸纯推导，与 iced FillPortion 的分配规则保持一致。

use std::borrow::Cow;

use iced::alignment::{Horizontal, Vertical};
use iced::widget::{button, column, container, mouse_area, row, scrollable, space, stack, Stack};
use iced::{Element, Length, Padding, Point, Rectangle, Size, border, widget::container::Style};

use atelyx_core::layout::{self, AppUiState, LayoutNode, TabItem};
use atelyx_core::plugin_runtime::ContributionSlot;

use crate::settings_view::ai;
use crate::ui::controls::{self, Variant};
use crate::ui::icon::{icon, icon_stroke};
use crate::ui::menu::MenuEntry;
use crate::ui::palette as pal;
use crate::ui::{mono, text};
use crate::ui::popup::{self, PopupSpec};
use crate::ui::notify;
use crate::ui::scroll;
use crate::{App, DraftKind, Message};

pub struct UiMetrics;

impl UiMetrics {
    pub const TITLEBAR_H: f32 = pal::TITLEBAR_H;
    pub const STATUSBAR_H: f32 = pal::STATUSBAR_H;
    pub const TAB_BAR_H: f32 = pal::TAB_BAR_H;
    pub const DIVIDER_W: f32 = 4.0;
    /// 分割子项最小占比（两侧钳制下限）。
    pub const MIN_SPLIT_FRAC: f64 = 0.12;
    /// 面板头容器内边距（标签条起点 = 面板左缘 + HEADER_INSET）。
    pub const HEADER_INSET: f32 = 6.0;
    /// 标签文字最大宽度（超出省略号截断）。
    pub const TAB_TEXT_MAX_W: f32 = 120.0;
    /// 窗口边缘缩放命中带厚度（四边）。
    pub const RESIZE_EDGE: f32 = 5.0;
    /// 窗口边缘缩放命中区边长（四角，叠在四边命中带之上）。
    pub const RESIZE_CORNER: f32 = 18.0;
}

/// 视图显示名（面板头标签）：注册表的一条查询——内置与插件条目同一条路径。
/// 未在册（贡献随插件停用消失）回落到插件 id。
pub fn view_label(app: &App, kind: &str) -> String {
    app.registry.label(kind)
}

/// 视图图标（壳内 lucide 名）：注册表的一条查询；未在册回落拼图。
pub fn view_icon(app: &App, kind: &str) -> Cow<'static, str> {
    app.registry.icon(kind)
}

/// 可选视图 kind 清单：注册表里启用中的面板视图（登记序：内置在前、插件在后）。
pub fn available_view_kinds(app: &App) -> Vec<String> {
    app.registry.views().map(|entry| entry.id.clone()).collect()
}

/// 估宽的单字符口径（估宽与表格行内换行估算共用：两处漂移即行高/命中失配）。
pub fn estimate_char_w(c: char, font_size: f32) -> f32 {
    let k = font_size / 12.0;
    if c.is_ascii() {
        6.5 * k
    } else {
        13.5 * k
    }
}

/// 估算文字宽度（拉丁 ≈6.5px、CJK ≈13.5px / 12px 字号；按字号线性缩放）。
/// 渲染宽度全部由内容自适应（Shrink），本估算服务于拖拽命中几何的插入位判定
/// （标题栏 tab 条起点/宽度、面板标签条插入索引）与弹层定宽（插件列表）；
/// 与真实 shaping 存在逐字符累积的估宽偏差，不影响渲染。
pub fn estimate_text_width(text: &str, font_size: f32) -> f32 {
    text.chars().map(|c| estimate_char_w(c, font_size)).sum()
}

/// 按估宽截断：超出 `max_w` 时逐字收缩并补省略号（截断口径与渲染定宽同源）。
pub fn ellipsize(text: &str, max_w: f32, font_size: f32) -> String {
    truncate_to_width(text, max_w, |s| estimate_text_width(s, font_size))
}

/// 截断到 max_w：超宽时逐字收缩并补省略号（measure 注入：估宽与实测宽共用同一截断口径）。
fn truncate_to_width(text: &str, max_w: f32, measure: impl Fn(&str) -> f32) -> String {
    if measure(text) <= max_w {
        return text.to_string();
    }
    for (i, _) in text.char_indices().rev() {
        let candidate = format!("{}…", &text[..i]);
        if measure(&candidate) <= max_w {
            return candidate;
        }
    }
    "…".to_string()
}

/// 画布视图（插件注册的面板视图 id）。
pub(crate) fn is_canvas_view(view: &str) -> bool {
    view == atelyx_core::layout::CANVAS_VIEW
}

/// 笔记视图（存量布局沿用旧内置 id `note`，与插件文档面 id 同判）。
fn is_note_view(view: &str) -> bool {
    view == "note" || view == atelyx_core::layout::NOTES_VIEW
}

/// 面板标签文字（笔记/表格/画布视图打开文件 = 文件名去扩展名后缀，否则视图名）。
pub fn tab_label(app: &App, view: &str) -> String {
    let open = match view {
        _ if is_note_view(view) => app.ui.last_note_file.as_deref(),
        "table" => app.ui.last_table_file.as_deref(),
        v if is_canvas_view(v) => app.ui.last_canvas_file.as_deref(),
        _ => None,
    };
    match open {
        Some(file) => file_label(file),
        None => view_label(app, view),
    }
}

/// 面板标签近似宽度（拖拽命中几何用；渲染宽度由内容自适应，见 panel_tab）。
fn approx_tab_width(label: &str, locked: bool, has_file: bool) -> f32 {
    let text_w = estimate_text_width(label, 12.0).min(UiMetrics::TAB_TEXT_MAX_W);
    13.0 + 4.0 + text_w + 16.0 + 2.0
        + if locked { 13.0 } else { 0.0 }
        + if has_file && !locked { 19.0 } else { 0.0 }
}

#[derive(Clone, Debug, PartialEq)]
pub enum RenameTarget {
    Layout(String),
    Scene(String),
    File(String),
    Folder(String),
    Draft { dir: String, kind: DraftKind },
}

#[derive(Clone, Debug)]
pub struct RenameState {
    pub target: RenameTarget,
    pub value: String,
    /// 输入框所在窗口：失焦提交与 Esc 取消按该窗口判定。
    pub window: iced::window::Id,
}

/// 重名去重提醒条：文案 + 出现时刻（3s 过期）。
pub struct RenameNotice {
    pub text: String,
    pub at: std::time::Instant,
}

/// 弹层内容形态（几何与交互数据随形态携带）。
#[derive(Clone, Debug)]
pub(crate) enum PopupState {
    SceneSelector { confirm: Option<String> },
    LayoutTabMenu { layout_id: String },
    PanelMenu { panel_id: String },
    TabContext { panel_id: String, tab_id: String },
    ViewPicker { panel_id: String },
    DetachedPanelMenu { window_id: String },
    DetachedTabContext { window_id: String, tab_id: String },
    /// 设置-工作区：启动场景选择（场景列表 + 跟随当前）。
    EntrySceneMenu,
    /// 协作空间菜单（登录入口 / 创建 / 邀请码加入）。
    SpaceMenu,
    /// 协作登录 / 注册表单（register = 注册模式）。
    SpaceLogin { register: bool },
    /// 创建空间（名称输入）。
    SpaceCreate,
    /// 邀请码加入（输入邀请码）。
    SpaceJoin,
    /// 成员管理（激活空间；数据取 App.space_members 缓存）。
    SpaceMembers,
    /// 邀请码管理（激活空间且 owner；表单草稿在 App.space_invite_form）。
    SpaceInvite,
    /// 插件声明的菜单（条目来自插件，宿主渲染与命中，选中后回传）。
    PluginMenu,
    /// 字体选择弹层（界面字体）。
    FontPicker,
    /// 取色器弹层（文件夹图标色、协作展示色、插件取色）。
    ColorPicker {
        target: crate::color_picker_actions::ColorTarget,
        hsv: crate::ui::color_picker::Hsv,
        hex: String,
    },
    /// 面板头部：「视图」下拉（按面板声明的贡献槽列出可切换视图）。
    PanelViewMenu { target: String },
    /// 面板头部：「⋯」菜单（条目来自插件声明，点击按 id 回发）。
    PanelItemsMenu { target: String },
    /// 设置-AI：话题命名模型下拉。
    AiModelMenu,
    /// 设置-AI：话题自动命名模式。
    AiNamingMenu,
    /// 设置-AI：Agent 系统提示词选择。
    AiAgentPromptMenu,
    /// 设置-AI：删除供应商二段确认（pane 1 = 确认）。
    AiProviderDeleteMenu,
    /// 设置-AI：删除 Agent 二段确认（pane 1 = 确认）。
    AiAgentDeleteMenu,
    /// 设置-AI：搜索源下拉。
    AiSearchProviderMenu,
    /// 设置-关于：开源许可条目（第三方资产官方主页外链）。
    LicenseInfo,
    /// 历史记录弹层（居中模态；kind+file 定位侧文件）。
    HistoryBrowser { kind: atelyx_core::history::HistoryKind, file: String },
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
            PopupState::PluginMenu => 200.0,
            PopupState::EntrySceneMenu => 176.0,
            PopupState::SpaceMenu => 200.0,
            PopupState::SpaceLogin { .. } => 340.0,
            PopupState::SpaceCreate => 260.0,
            PopupState::SpaceJoin => 260.0,
            // 成员行带 @username 与角色徽标；邀请码表单含两个数字输入
            PopupState::SpaceMembers => 320.0,
            PopupState::SpaceInvite => 320.0,
            PopupState::FontPicker => 240.0,
            PopupState::ColorPicker { .. } => crate::ui::color_picker::PANEL_W,
            PopupState::PanelViewMenu { .. } => 120.0,
            PopupState::PanelItemsMenu { .. } => 144.0,
            // AI 设置：模型 id / 笔记名可能较长，给足一行的宽度
            PopupState::AiModelMenu => 224.0,
            PopupState::AiNamingMenu => 144.0,
            PopupState::AiAgentPromptMenu => 208.0,
            PopupState::AiProviderDeleteMenu | PopupState::AiAgentDeleteMenu => 208.0,
            PopupState::AiSearchProviderMenu => 144.0,
            // 居中模态不走锚点几何，估高由各自 overlay 钳制
            PopupState::LicenseInfo => crate::settings_view::LICENSE_CARD_WIDTH,
            PopupState::HistoryBrowser { .. } => crate::history_browser::CARD_WIDTH,
        }
    }
}

/// 打开弹层的请求：锚点（窗口局部坐标）+ 触发器顶边 + 内容形态。
#[derive(Clone, Debug)]
pub(crate) struct PopupOpen {
    pub point: Point,
    pub flip_y: Option<f32>,
    pub state: PopupState,
    /// 宽度下限（弹层不窄于触发按钮；0 = 不限）。
    pub min_width: f32,
}

/// 按钮触发的弹层：锚点在按钮下缘（光标 y + 16 ≈ 28px 按钮的中点下移），
/// 翻转线在按钮上缘，弹层不盖住触发按钮。
fn btn_popup(window: iced::window::Id, cursor: Point, state: PopupState) -> Message {
    btn_popup_min(window, cursor, state, 0.0)
}

/// 带宽度下限的按钮弹层（锚定触发按钮：估宽下限保证弹层不窄于按钮）。
pub(crate) fn btn_popup_min(window: iced::window::Id, cursor: Point, state: PopupState, min_width: f32) -> Message {
    Message::PopupOpen(
        window,
        PopupOpen {
            point: Point::new(cursor.x, cursor.y + 16.0),
            flip_y: Some(cursor.y - 12.0),
            state,
            min_width,
        },
    )
}

/// 右键菜单类弹层：锚点 = 光标点，无翻转。
fn ctx_popup(window: iced::window::Id, cursor: Point, state: PopupState) -> Message {
    Message::PopupOpen(window, PopupOpen { point: cursor, flip_y: None, state, min_width: 0.0 })
}

/// 设置-关于：开源许可模态的打开消息（居中模态不走锚点几何）。
pub(crate) fn license_popup(window: iced::window::Id) -> Message {
    Message::PopupOpen(
        window,
        PopupOpen {
            point: Point::ORIGIN,
            flip_y: None,
            state: PopupState::LicenseInfo,
            min_width: 0.0,
        },
    )
}

/// 主窗口：标题栏 + 内容（设置整页或面板网格）+ 状态栏，顶层叠弹层/提示/通知。
pub fn main_view<'a>(app: &'a App, window: iced::window::Id) -> Element<'a, Message> {
    // 接管中的窗口：整窗视图由插件面替换（page 保留宿主标题栏与状态栏），退出入口常驻
    if let Some(t) = app.plugin_takeovers.get(&window) {
        return takeover_view(app, window, t);
    }
    let content: Element<'a, Message> = if app.settings_open {
        crate::settings_view::settings_view(app, window)
    } else {
        // 把手悬停态：光标命中检测与渲染同源（enumerate_dividers 几何）
        let cursor = app.cursor_for(window);
        let hovered = if app.main_id == Some(window) {
            enumerate_dividers(&layout::active_layout(&app.ui).tree, content_area_of(app))
                .into_iter()
                .find(|d| d.rect.contains(cursor))
                .map(|d| (d.split_id, d.gap))
        } else {
            None
        };
        render_node(app, active_tree(&app.ui), hovered.as_ref().map(|(s, g)| (s.as_str(), *g)))
    };

    let base = workspace_chrome(app, window, content);

    with_overlays(app, window, base)
}

/// 接管视图（按窗生效）：page = 宿主标题栏与状态栏保留、内容区归插件；full = 整窗归插件呈现
/// （含标题栏区域：缩放沿用边缘命中带、关闭走宿主关窗拦截链）。
/// 右上角退出入口常驻（Esc 为后备）。接管期间布局真源不动，退出即恢复原视图。
pub fn takeover_view<'a>(
    app: &'a App,
    window: iced::window::Id,
    t: &'a crate::plugin_actions::TakeoverActive,
) -> Element<'a, Message> {
    let content = crate::plugin_view::takeover_content(app, window, &t.plugin_id, &t.surface_id);
    let base: Element<'a, Message> = if t.full {
        container(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_t| Style {
                background: Some(pal::BG_PRIMARY.into()),
                ..Style::default()
            })
            .into()
    } else {
        workspace_chrome(app, window, content)
    };
    takeover_shell(app, window, base)
}

/// 插件窗口：面树铺满整窗，弹层 / 悬停提示 / 通知沿用工作区覆盖层（窗口内下拉与菜单依赖它）。
/// 无系统边框时按声明的标题栏归属决定由宿主补标题栏，还是把整窗交给插件自绘。
pub fn plugin_window_view<'a>(app: &'a App, window: iced::window::Id) -> Element<'a, Message> {
    let Some(entry) = app.plugin_windows.get(&window) else {
        return space().into();
    };
    let content = crate::plugin_view::window_view(app, window, entry);
    let base = if entry.decorations || !entry.titlebar {
        content
    } else {
        plugin_window_chrome(window, &entry.title, content)
    };
    with_overlays(app, window, base)
}

/// 无系统边框插件窗口标题栏：标题截断宽度上限与标题左缘内边距。
const PLUGIN_TITLEBAR_TITLE_W: f32 = 240.0;
const PLUGIN_TITLEBAR_PAD_X: f32 = 12.0;

/// 无系统边框的插件窗口标题栏：标题与空白区整体可拖（双击 = 最大化切换），
/// 右侧最小化 / 最大化 / 关闭。
fn plugin_window_chrome<'a>(
    window: iced::window::Id,
    title: &str,
    content: Element<'a, Message>,
) -> Element<'a, Message> {
    let titlebar = container(
        row![
            mouse_area(
                row![
                    text(ellipsize(title, PLUGIN_TITLEBAR_TITLE_W, pal::fs_caption()))
                        .size(pal::fs_caption())
                        .color(pal::TEXT_SECONDARY),
                    space().width(Length::Fill).height(Length::Fill),
                ]
                .height(Length::Fill)
                .align_y(Vertical::Center),
            )
            .on_press(Message::TitleBarPress(window)),
            controls::window_button(controls::WindowButtonKind::Minimize, Message::WindowMinimize(window)),
            controls::window_button(controls::WindowButtonKind::Maximize, Message::WindowMaximizeToggle(window)),
            controls::window_button(controls::WindowButtonKind::Close, Message::WindowCloseRequested(window)),
        ]
        .height(Length::Fill)
        .align_y(Vertical::Center),
    )
    .width(Length::Fill)
    .height(Length::Fixed(pal::TITLEBAR_H))
    .padding(Padding::ZERO.left(PLUGIN_TITLEBAR_PAD_X))
    .style(|_t| Style {
        background: Some(pal::BG_SECONDARY.into()),
        ..Style::default()
    });
    column![
        titlebar,
        container(content).width(Length::Fill).height(Length::Fill),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// 接管视图的退出浮层与覆盖层组装（base 之上叠右上角常驻退出入口，
/// 再挂工作区同一套弹层 / 悬停提示 / 通知 / 边缘缩放命中带）。
fn takeover_shell<'a>(
    app: &'a App,
    window: iced::window::Id,
    base: Element<'a, Message>,
) -> Element<'a, Message> {
    let exit: Element<'static, Message> = container(controls::btn(
        "退出接管",
        &controls::btn_sm(),
        controls::Variant::Ghost,
        Some(Message::PluginExitTakeover(window)),
    ))
    .padding(Padding::from(6.0))
    .into();
    let overlay = container(exit)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Horizontal::Right)
        .align_y(Vertical::Top);
    let inner = Stack::new()
        .push(base)
        .push(overlay)
        .width(Length::Fill)
        .height(Length::Fill);
    with_overlays(app, window, inner.into())
}

/// 工作区 chrome 骨架（标题栏 + 分隔线 + 内容 + 分隔线 + 状态栏）；主窗与整页接管共用。
fn workspace_chrome<'a>(
    app: &'a App,
    window: iced::window::Id,
    content: Element<'a, Message>,
) -> Element<'a, Message> {
    column![
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
    .height(Length::Fill)
    .into()
}

/// 主窗口分割树几何区域（标题栏与状态栏之间的内容区，窗口局部坐标；
/// 与渲染列的两条 1px 分隔线同源：上下各让 1px）。
pub fn content_area_of(app: &App) -> Rectangle {
    Rectangle {
        x: 0.0,
        y: UiMetrics::TITLEBAR_H + 1.0,
        width: app.main_size.width,
        height: (app.main_size.height
            - UiMetrics::TITLEBAR_H
            - UiMetrics::STATUSBAR_H
            - 2.0)
            .max(0.0),
    }
}

/// 窗口边缘缩放命中带（四边 + 四角）：悬停显示对应方向缩放光标，按下进入 OS 原生
/// 缩放循环；最大化态由 OS 拒绝该循环，无需自行跟踪。命中带只在内容之上、弹层之下，
/// 弹层打开期间其捕获层先于命中带承接外点。
fn edge_resize_bands(window: iced::window::Id) -> Element<'static, Message> {
    use iced::window::Direction;

    let strip =
        |dir: Direction, width: Length, height: Length, cursor: iced::mouse::Interaction| {
            mouse_area(container(space()).width(width).height(height))
                .on_press(Message::WindowResizeStart(window, dir))
                .interaction(cursor)
                .into()
        };
    let anchored =
        |el: Element<'static, Message>, x: Horizontal, y: Vertical| -> Element<'static, Message> {
            container(el)
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(x)
                .align_y(y)
                .into()
        };

    let edge = Length::Fixed(UiMetrics::RESIZE_EDGE);
    let corner = Length::Fixed(UiMetrics::RESIZE_CORNER);

    // 显式 Fill×Fill：Stack 缺省 Shrink、尺寸取第 0 子（北边条），
    // 其余锚定子会被压缩到 5px 高——必须撑满窗口后各带才能贴边
    stack![
        strip(Direction::North, Length::Fill, edge, iced::mouse::Interaction::ResizingVertically),
        anchored(
            strip(Direction::South, Length::Fill, edge, iced::mouse::Interaction::ResizingVertically),
            Horizontal::Left,
            Vertical::Bottom,
        ),
        strip(Direction::West, edge, Length::Fill, iced::mouse::Interaction::ResizingHorizontally),
        anchored(
            strip(Direction::East, edge, Length::Fill, iced::mouse::Interaction::ResizingHorizontally),
            Horizontal::Right,
            Vertical::Top,
        ),
        // 四角命中区叠在四边之上（后推 = 事件优先），角内斜向光标
        strip(Direction::NorthWest, corner, corner, iced::mouse::Interaction::ResizingDiagonallyDown),
        anchored(
            strip(Direction::NorthEast, corner, corner, iced::mouse::Interaction::ResizingDiagonallyUp),
            Horizontal::Right,
            Vertical::Top,
        ),
        anchored(
            strip(Direction::SouthWest, corner, corner, iced::mouse::Interaction::ResizingDiagonallyUp),
            Horizontal::Left,
            Vertical::Bottom,
        ),
        anchored(
            strip(Direction::SouthEast, corner, corner, iced::mouse::Interaction::ResizingDiagonallyDown),
            Horizontal::Right,
            Vertical::Bottom,
        ),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// 弹层 / 悬停提示 / 通知 / 拖拽 ghost 的窗口内覆盖。
fn with_overlays<'a>(
    app: &'a App,
    window: iced::window::Id,
    base: Element<'a, Message>,
) -> Element<'a, Message> {
    let viewport = viewport_of(app, window);
    // 覆盖层（弹层/提示/拖拽 ghost/通知）统一作为同一 Stack 的追加子，内容恒为第 0 子：
    // 任一覆盖显隐只增删尾部子，内容子树状态不重建——滚动位置与输入焦点得以保留。
    let mut root = iced::widget::Stack::new().push(base);

    // 边缘缩放命中带：标签/文件拖拽会话进行中不挂（避免拖拽光标被边缘命中干扰）
    if app.drag.is_none() && app.file_drag.is_none() {
        root = root.push(edge_resize_bands(window));
    }

    // 窗口级插件浮层：在所有面板之上、宿主菜单与模态之下
    if let Some(el) = crate::plugin_overlay::overlay(app, window) {
        root = root.push(el);
    }

    // 弹层（同窗口至多一个；捕获层点击 = 外点关闭；居中模态另行渲染，见下方）
    if let Some((w, open)) = &app.popup {
        if *w == window
            && !matches!(open.state, PopupState::HistoryBrowser { .. } | PopupState::LicenseInfo)
        {
            let (content, height) = popup_element(app, &open.state, app.popup_pane, window);
            let spec = PopupSpec {
                anchor: open.point,
                width: open.state.width(),
                height,
                flip_y: open.flip_y,
                min_width: open.min_width,
            };
            let geom = popup::resolve(&spec, viewport);
            root = root.push(popup::overlay(
                geom,
                content,
                Message::Ui(crate::ui::UiMsg::PopupClose { window }),
            ));
        }
    }

    // 历史记录模态（优先级在锚点弹层之上、通知之下）
    if let Some(el) = crate::history_browser::overlay(app, window, viewport) {
        root = root.push(el);
    }

    // 开源许可模态（与历史记录同级）
    if let Some(el) = crate::settings_view::license_overlay(app, window) {
        root = root.push(el);
    }

    // 插件页模态（详情 / 市场安装确认；与开源许可同级）
    if let Some(el) = crate::settings_view::plugins::overlay(app, window) {
        root = root.push(el);
    }

    // 悬停提示：跟随光标（锚在光标下方半边长 + 4），视口钳制 + 上下翻转
    if let Some((w, label, _, shown, _, _)) = &app.tooltip {
        if *w == window && *shown {
            let tip = container(text(label.clone()).size(pal::fs_micro()).color(pal::TEXT_SECONDARY))
                .padding(Padding::ZERO.horizontal(8.0).vertical(4.0))
                .style(|_t| Style {
                    background: Some(pal::BG_OVERLAY.into()),
                    border: border::rounded(pal::RADIUS_MD).color(pal::BORDER).width(1.0),
                    ..Style::default()
                });
            let pos = app.cursor_for(window);
            let tip_w = (estimate_text_width(label, pal::fs_micro()) + 16.0).min(260.0);
            let tip_h = 16.0 + 8.0;
            let half = app.tooltip_half.unwrap_or(14.0);
            let spec = PopupSpec {
                anchor: Point::new(pos.x, pos.y + half + 4.0),
                width: tip_w,
                height: tip_h,
                flip_y: Some(pos.y - half - 4.0),
                min_width: 0.0,
            };
            let geom = popup::resolve(&spec, viewport);
            let positioned = container(tip)
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(Horizontal::Left)
                .align_y(Vertical::Top)
                .padding(Padding::ZERO.left(geom.x).top(geom.y));
            root = root.push(mouse_area(positioned).on_exit(Message::Ui(
                crate::ui::UiMsg::HoverTipEnd { window, label: label.clone() },
            )));
        }
    }

    // 拖拽 ghost：跟随光标的预览卡（会话由源窗口发起，显示在光标所在窗口）
    if let Some(drag) = &app.drag {
        if drag.cursor_window == window {
            if let Some(wpos) = app.os_positions.get(&window) {
                let local = Point::new(drag.current.x - wpos.x, drag.current.y - wpos.y);
                let ghost = container(
                    row![
                        icon_stroke(&view_icon(app, &drag.view), 13.0, pal::TEXT_PRIMARY, 1.5),
                        text(drag.label.clone()).size(pal::fs_caption()).color(pal::TEXT_PRIMARY),
                    ]
                    .spacing(6),
                )
                .padding(Padding::ZERO.horizontal(10.0).vertical(4.0))
                .style(|_t| Style {
                    background: Some(pal::BG_OVERLAY.into()),
                    border: border::rounded(pal::RADIUS_SM).color(pal::ACCENT).width(1.0),
                    ..Style::default()
                });
                let positioned = container(ghost)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .align_x(Horizontal::Left)
                    .align_y(Vertical::Top)
                    .padding(Padding::ZERO.left(local.x + 14.0).top(local.y + 18.0));
                root = root.push(mouse_area(positioned));
            }
        }
    }

    // 文件树拖拽 ghost：跟随光标的预览卡 + 悬停目标的动作提示（无目标 = 只显示文件名）。
    // 不包 mouse_area 捕获——释放走全局订阅，悬停事件须穿透到行上才能上报落点
    // （与标签拖拽的拦事件语义不同）。
    if let Some(drag) = &app.file_drag {
        if drag.cursor_window == window {
            if let Some(wpos) = app.os_positions.get(&window) {
                let local = Point::new(drag.current.x - wpos.x, drag.current.y - wpos.y);
                let icon_name = if drag.is_dir {
                    Cow::Borrowed("folder")
                } else {
                    file_icon(app, &drag.label)
                };
                let reject = drag.reject.as_ref().map(|(_, why)| *why);
                let hint = match reject {
                    Some(crate::DropReject::NotADir) => Some("只能放进文件夹".to_string()),
                    Some(crate::DropReject::SameDir) => Some("已在此目录".to_string()),
                    Some(crate::DropReject::SelfTree) => Some("不能放进自身".to_string()),
                    None => app.file_drag_target.as_ref().map(|dir| {
                        if dir.is_empty() {
                            "移到根目录".to_string()
                        } else {
                            let name = dir.rsplit('/').next().unwrap_or(dir);
                            format!("移动到「{name}」")
                        }
                    }),
                };
                let reject = reject.is_some();
                let mut card = column![
                    row![
                        icon_stroke(&icon_name, 13.0, pal::TEXT_PRIMARY, 1.5),
                        text(drag.label.clone()).size(pal::fs_caption()).color(pal::TEXT_PRIMARY),
                    ]
                    .spacing(6),
                ]
                .spacing(2);
                if let Some(hint) = hint {
                    card = card.push(
                        text(hint)
                            .size(pal::fs_micro())
                            .color(if reject { pal::DANGER } else { pal::TEXT_SECONDARY }),
                    );
                }
                let ghost = container(card)
                    .padding(Padding::ZERO.horizontal(10.0).vertical(4.0))
                    .style(move |_t| Style {
                        background: Some(pal::BG_OVERLAY.into()),
                        border: border::rounded(pal::RADIUS_SM)
                            .color(if reject { pal::DANGER } else { pal::ACCENT })
                            .width(1.0),
                        ..Style::default()
                    });
                let positioned = container(ghost)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .align_x(Horizontal::Left)
                    .align_y(Vertical::Top)
                    .padding(Padding::ZERO.left(local.x + 14.0).top(local.y + 18.0));
                root = root.push(positioned);
            }
        }
    }

    // 通知（仅主窗口承载）
    if app.main_id == Some(window) && !app.notifications.is_empty() {
        root = root.push(notify::layer(&app.notifications));
    }

    root.width(Length::Fill).height(Length::Fill).into()
}

/// 窗口视口尺寸（弹层钳制与翻转判据的基准）。
pub fn viewport_of(app: &App, window: iced::window::Id) -> Size {
    if app.main_id == Some(window) {
        return app.main_size;
    }
    if let Some(entry) = app.plugin_windows.get(&window) {
        return entry.size;
    }
    app.os_sizes
        .get(&window)
        .copied()
        .unwrap_or(Size::new(420.0, 560.0))
}

// ===== 标题栏 =====

/// 品牌名估宽（标题栏布局 tab 条起点几何的组成部分；文字定宽渲染保证几何同源）。
const BRAND_TEXT: &str = "Atelyx";

/// 标题栏布局 tab 条的条带起点 x 与各 tab (id, 名称, 宽度)（拖拽命中几何；与渲染同源估算）。
/// 渲染序 = 内边距 4 + 品牌（内衬 8 + 图标 16 + 间距 6 + 定宽文字）+ 行间距 4。
pub fn titlebar_tab_strip(app: &App) -> (f32, Vec<(String, String, f32)>) {
    // 品牌行宽 = 左右内衬 8 + 图标 16 + 间距 6 + 定宽文字
    let brand_w = 8.0 + 16.0 + 6.0 + estimate_text_width(BRAND_TEXT, 12.0) + 8.0;
    let mut out = vec![(
        layout::HOME_LAYOUT_ID.to_string(),
        "主页".to_string(),
        layout_tab_width("主页"),
    )];
    if let Some(scene) = app.active_scene() {
        for l in &scene.layouts {
            // 改名在途的 tab 按输入内容定宽（与渲染同源，见 layout_tab_edit_width）
            let width = match &app.rename {
                Some(r) => match &r.target {
                    RenameTarget::Layout(rid) if rid == &l.id => layout_tab_edit_width(&r.value),
                    _ => layout_tab_width(&l.name),
                },
                None => layout_tab_width(&l.name),
            };
            out.push((l.id.clone(), l.name.clone(), width));
        }
    }
    (4.0 + brand_w + 4.0, out)
}

/// 布局 tab 宽度：文字估宽 + 内边距 20，钳制 48–120。
fn layout_tab_width(name: &str) -> f32 {
    (estimate_text_width(name, 12.0) + 20.0).clamp(48.0, 120.0)
}

/// 布局 tab 改名编辑态宽度：按当前输入随内容增长，钳制 140–260（提交后收回正常宽度）。
fn layout_tab_edit_width(value: &str) -> f32 {
    (estimate_text_width(value, 12.0) + 20.0).clamp(140.0, 260.0)
}

/// 布局 tab 条释放插入位：光标在标题栏区域且越过条带起点时，命中标签中点之前为插入位
/// （内容坐标随滚动偏移平移）；条带外 = None（落位取消）。插入位标渲染与释放落点共用本判定。
pub fn titlebar_tab_insert(app: &App, local: Point) -> Option<usize> {
    if local.y > UiMetrics::TITLEBAR_H {
        return None;
    }
    let (strip_x, tabs) = titlebar_tab_strip(app);
    if local.x < strip_x {
        return None;
    }
    let mut x = strip_x - app.titlebar_tab_scroll_x;
    for (i, (_, _, w)) in tabs.iter().enumerate() {
        if local.x < x + w / 2.0 {
            return Some(i);
        }
        x += w + 4.0;
    }
    Some(tabs.len())
}

fn titlebar<'a>(app: &'a App, window: iced::window::Id) -> Element<'a, Message> {
    let cursor = app.cursor_for(window);

    // 布局 tab 条；布局 tab 拖拽悬停时按与释放落点同一判定渲染插入位标
    let insert_at = match app.drag.as_ref() {
        Some(d)
            if d.from_panel.is_none() && d.from_detached.is_none() && d.cursor_window == window =>
        {
            app.os_positions.get(&window).and_then(|wpos| {
                let local = Point::new(d.current.x - wpos.x, d.current.y - wpos.y);
                titlebar_tab_insert(app, local)
            })
        }
        _ => None,
    };
    let mut entries: Vec<(String, String, bool)> = vec![(
        "主页".to_string(),
        layout::HOME_LAYOUT_ID.to_string(),
        app.ui.active_layout_id.as_deref() == Some(layout::HOME_LAYOUT_ID),
    )];
    if let Some(scene) = app.active_scene() {
        for l in &scene.layouts {
            let is_active = app.ui.active_layout_id.as_deref() == Some(l.id.as_str());
            entries.push((l.name.clone(), l.id.clone(), is_active));
        }
    }
    let mut tabs = row![].spacing(4);
    for (i, (name, id, active)) in entries.iter().enumerate() {
        if insert_at == Some(i) {
            tabs = tabs.push(titlebar_insert_marker());
        }
        tabs = tabs.push(layout_tab(app, name, id, *active, window, cursor));
    }
    if insert_at == Some(entries.len()) {
        tabs = tabs.push(titlebar_insert_marker());
    }
    let tabs = scrollable(container(row![tabs,
        controls::icon_button("plus", 13.0, 28.0, Variant::SubtleBg,
            Some(Message::LayoutNew), Some((window, "新建布局".into()))),
    ]))
    .direction(scrollable::Direction::Horizontal(scroll::hidden()))
    .on_scroll(|v| Message::TabStripScrolled(None, v.absolute_offset().x))
    .width(Length::Shrink)
    .height(Length::Shrink);

    // 场景选择器
    let scene_name = app.active_scene().map(|s| s.name.clone()).unwrap_or_default();
    let trigger_w = estimate_text_width(&scene_name, 12.0) + 16.0 + 4.0 + 12.0;
    let selector = button(
        container(
            row![
                text(ellipsize(&scene_name, 120.0, pal::fs_caption())).size(pal::fs_caption()).color(pal::TEXT_SECONDARY),
                icon("chevron-down", 12.0, pal::TEXT_SECONDARY),
            ]
            .spacing(4),
        )
        .height(Length::Fill)
        .align_y(Vertical::Center),
    )
    .padding(Padding::ZERO.horizontal(8.0))
    .height(Length::Fixed(28.0))
    .style(hover_btn_style())
    .on_press(btn_popup_min(window, cursor, PopupState::SceneSelector { confirm: None }, trigger_w));

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
        container(
            iced::widget::image(crate::app_icon::icon_handle())
                .width(Length::Fixed(16.0))
                .height(Length::Fixed(16.0)),
        )
        .width(Length::Fixed(16.0))
        .height(Length::Fixed(16.0))
        .clip(true)
        .style(|_t| Style {
            border: border::rounded(pal::RADIUS_XS),
            ..Style::default()
        }),
        text(BRAND_TEXT)
            .size(pal::fs_caption())
            .color(pal::TEXT_SECONDARY)
            .font(crate::ui::bold())
            .width(Length::Fixed(estimate_text_width(BRAND_TEXT, 12.0))),
    ]
    .spacing(6)
    .padding(Padding::ZERO.horizontal(8.0));

    // 空白区 = 拖拽区（按下拖动窗口 / 双击最大化）
    let drag_spring = mouse_area(space().width(Length::Fill).height(Length::Fill))
        .on_press(Message::TitleBarPress(window));

    // 窗口控制三钮贴合（组内无间距）
    let window_controls = row![
        controls::window_button(controls::WindowButtonKind::Minimize, Message::WindowMinimize(window)),
        controls::window_button(controls::WindowButtonKind::Maximize, Message::WindowMaximizeToggle(window)),
        controls::window_button(controls::WindowButtonKind::Close, Message::WindowCloseRequested(window)),
    ];

    // 右操作区单行排布：场景选择器 → 设置 → 全屏 → 窗口控制（无系统边框后为唯一标题栏）
    container(
        row![
            brand,
            tabs,
            drag_spring,
            selector,
            settings_btn,
            fullscreen_btn,
            window_controls,
        ]
        .height(Length::Fill)
        .spacing(4)
        .align_y(Vertical::Center),
    )
    .width(Length::Fill)
    .height(Length::Fixed(pal::TITLEBAR_H))
    .padding(Padding::ZERO.horizontal(4.0))
    .style(|_t| Style {
        background: Some(pal::BG_SECONDARY.into()),
        ..Style::default()
    })
    .into()
}

/// 标题栏布局 tab（宽度随内容自适应；激活/重命名在途 = 三档底 + 底部金条
/// （左右缩进 8 圆头）；点击激活（拖拽会话期间忽略）、按下上报拖拽候选（主页除外）、
/// 右键 = tab 菜单、双击 = 重命名；名称被省略时悬停显示全称）。
fn layout_tab(
    app: &App,
    name: &str,
    id: &str,
    active: bool,
    window: iced::window::Id,
    cursor: Point,
) -> Element<'static, Message> {
    let text_color = if active { pal::TEXT_PRIMARY } else { pal::TEXT_SECONDARY };
    // 重命名在途 = 与激活态同款高亮（底色 + 金下边），无论该 tab 是否激活
    let draft = app.rename.as_ref().and_then(|r| match &r.target {
        RenameTarget::Layout(lid) if lid == id => Some(r.value.as_str()),
        _ => None,
    });
    let highlighted = active || draft.is_some();
    let display = ellipsize(name, UiMetrics::TAB_TEXT_MAX_W, 12.0);
    let truncated = draft.is_none() && display != name;
    let text_area: Element<'static, Message> = match draft {
        Some(value) => controls::inline_input(
            iced::widget::Id::new(controls::INLINE_RENAME_INPUT),
            "新名称",
            value,
            layout_tab_edit_width(value) - 20.0,
            Message::LayoutRenameInput,
            Message::LayoutRenameCommit,
            Message::RenamePressInside,
        ),
        None => text(display)
            .size(pal::fs_caption())
            .color(text_color)
            .into(),
    };
    // 列外套一层撑满的容器，列内的 Fill 才拿到真实高度
    let content = container(column![
        // 文字左右内边距即 tab 底色盒的留白，取对称值才不偏；宽度保持 Shrink——
        // tab 条 scrollable 为 Shrink，列在交叉轴被压缩时 Fill 宽会被算成 0 宽
        container(text_area)
            .height(Length::Fill)
            .padding(Padding::ZERO.horizontal(10.0))
            .align_y(Vertical::Center),
        // 金条：圆头，贴 tab 底缘；定高才会延迟到宽度已解析的那轮布局
        container(
            container(space())
                .width(Length::Fill)
                .height(Length::Fixed(2.0))
                .style(move |_t| Style {
                    background: highlighted.then_some(pal::ACCENT.into()),
                    border: border::rounded(1.0),
                    ..Style::default()
                }),
        )
        .width(Length::Fill)
        .height(Length::Fixed(2.0))
        .padding(Padding::ZERO.horizontal(2.0)),
    ])
    .height(Length::Fill);

    let underline = container(content)
        .height(Length::Fixed(28.0))
        .style(move |_t| Style {
            background: highlighted.then_some(pal::BG_TERTIARY.into()),
            border: border::rounded(pal::RADIUS_SM),
            ..Style::default()
        });

    // 主页固定不可拖、不可右键、不可双击改名
    if id == layout::HOME_LAYOUT_ID {
        return button(underline)
            .padding(0.0)
            .style(move |_t, status| button::Style {
                background: (!highlighted
                    && matches!(status, button::Status::Hovered | button::Status::Pressed))
                .then_some(pal::BG_TERTIARY.into()),
                border: border::rounded(pal::RADIUS_SM),
                ..Default::default()
            })
            .on_press(Message::LayoutActivate(id.to_string()))
            .into();
    }
    // 显式零内边距：估宽（layout_tab_width）按内容记账，默认内边距会让命中几何偏窄
    let area = mouse_area(
        button(underline)
            .padding(0.0)
            .style(move |_t, status| button::Style {
                background: (!highlighted
                    && matches!(status, button::Status::Hovered | button::Status::Pressed))
                .then_some(pal::BG_TERTIARY.into()),
                border: border::rounded(pal::RADIUS_SM),
                ..Default::default()
            })
            .on_press(Message::LayoutActivate(id.to_string())),
    )
    .on_right_press(ctx_popup(
        window,
        cursor,
        PopupState::LayoutTabMenu { layout_id: id.to_string() },
    ));
    // 省略号截断时悬停给完整名称（改名前先能看清叫什么）
    let area = if truncated {
        let tip = name.to_string();
        area.on_enter(Message::Ui(crate::ui::UiMsg::HoverTip {
            window,
            label: tip.clone(),
            box_px: 28.0,
        }))
        .on_exit(Message::Ui(crate::ui::UiMsg::HoverTipEnd { window, label: tip }))
    } else {
        area
    };
    // 按下候选经 PressReporter 上报：拖拽候选与双击改名都按该按下时序判定
    // （按钮先捕获左键按下，mouse_area 对已捕获事件跳过，双击回调不会触发）
    controls::PressReporter::new(
        area.into(),
        Message::LayoutTabPress {
            layout_id: id.to_string(),
            name: name.to_string(),
        },
    )
    .into()
}

/// 触发按钮/表单行的悬停底样式。
fn hover_btn_style() -> impl Fn(&iced::Theme, button::Status) -> button::Style {
    |_t, status| button::Style {
        background: matches!(status, button::Status::Hovered | button::Status::Pressed)
            .then_some(pal::BG_TERTIARY.into()),
        text_color: pal::TEXT_SECONDARY,
        border: border::rounded(pal::RADIUS_SM),
        ..Default::default()
    }
}

// ===== 状态栏 =====

fn status_bar(app: &App) -> Element<'static, Message> {
    // 仓库身份三态：协作空间（空间名 @ 服务器） / 本地仓库 / 未打开
    let (icon_name, name) = match (&app.vault_root, &app.repo_kind) {
        (None, crate::RepoKind::Space(s)) => ("cloud", format!("{} @ {}", s.name, s.server_url)),
        (Some(root), _) => ("hard-drive", atelyx_core::vault::vault_display_name(root)),
        (None, crate::RepoKind::Local) => ("hard-drive", "未打开仓库".to_string()),
    };
    // 组间 12px、组内 6px 两级间距；文件数统计在途不渲染占位
    let mut left = row![
        row![
            icon(icon_name, 12.0, pal::TEXT_MUTED),
            text(name).size(pal::fs_micro()).color(pal::TEXT_SECONDARY),
        ]
        .spacing(6),
    ]
    .spacing(12);
    if let Some(n) = app.file_count {
        left = left
            .push(vrule())
            .push(
                text(format!("{n} 文件"))
                    .size(pal::fs_micro())
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
/// `hovered_divider` = 光标当前命中的 (split_id, gap)，把手悬停转强调色。
pub fn render_node<'a>(
    app: &'a App,
    node: &'a LayoutNode,
    hovered_divider: Option<(&str, usize)>,
) -> Element<'a, Message> {
    match node {
        LayoutNode::Split { id, direction, children, sizes, .. } => {
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
            let hovered_gap = hovered_divider
                .filter(|(sid, _)| *sid == id.as_str())
                .map(|(_, gap)| gap);
            if horizontal {
                let mut r = row![].height(Length::Fill);
                for (i, child) in children.iter().enumerate() {
                    r = r.push(sized(render_node(app, child, hovered_divider), i));
                    if i + 1 < children.len() {
                        r = r.push(divider(hovered_gap == Some(i), horizontal));
                    }
                }
                r.into()
            } else {
                let mut c = column![].width(Length::Fill);
                for (i, child) in children.iter().enumerate() {
                    c = c.push(sized(render_node(app, child, hovered_divider), i));
                    if i + 1 < children.len() {
                        c = c.push(divider(hovered_gap == Some(i), horizontal));
                    }
                }
                c.into()
            }
        }
        LayoutNode::Panel { id, tabs, active_tab_id } => panel_view(app, id, tabs, active_tab_id),
    }
}

/// 分割把手（粗 4px；悬停 = 强调色 50%）；光标形态 col-resize / row-resize。
fn divider(hovered: bool, horizontal: bool) -> Element<'static, Message> {
    let interaction = if horizontal {
        iced::mouse::Interaction::ResizingColumn
    } else {
        iced::mouse::Interaction::ResizingRow
    };
    let (width, height) = divider_size(horizontal);
    mouse_area(
        container(space())
            .width(width)
            .height(height)
            .style(move |_t| Style {
                background: if hovered {
                    Some(pal::ACCENT.scale_alpha(0.5).into())
                } else {
                    Some(pal::BORDER.into())
                },
                ..Style::default()
            }),
    )
    .interaction(interaction)
    .into()
}

/// 把手轴向尺寸：左右分割为竖条（宽 4px 撑满高），上下分割为横条（撑满宽 高 4px）。
/// 厚度不能取 Fill——会被同轴的面板占比挤成约 1px。
fn divider_size(horizontal: bool) -> (Length, Length) {
    if horizontal {
        (Length::Fixed(UiMetrics::DIVIDER_W), Length::Fill)
    } else {
        (Length::Fill, Length::Fixed(UiMetrics::DIVIDER_W))
    }
}

/// 面板：标签条（底色 + 1px 底线 + 激活标签盖线）+ 内容区；拖拽悬停期叠 drop 指示。
fn panel_view<'a>(
    app: &'a App,
    panel_id: &str,
    tabs: &'a [TabItem],
    active_tab_id: &Option<String>,
) -> Element<'a, Message> {
    let window = app.main_id.unwrap_or(iced::window::Id::unique());
    let mut tab_row = row![].spacing(2).height(Length::Fill);
    // 拖拽插入条：命中本面板标签条时在插入位渲染 2px 强调竖条（随行内元素移位）
    let insert_at = app.drag.as_ref().and_then(|d| {
        let hover = app.drag_hover.as_ref()?;
        (hover.zone.panel_id.as_deref() == Some(panel_id) && hover.kind == DropZone::Tab)
            .then(|| {
                let scroll_x = app.panel_tab_scroll_x.get(panel_id).copied().unwrap_or(0.0);
                tab_index_at(&hover.zone, d.current.x, scroll_x)
            })
    });
    for (i, t) in tabs.iter().enumerate() {
        if insert_at == Some(i) {
            tab_row = tab_row.push(insertion_marker());
        }
        tab_row = tab_row.push(panel_tab(app, panel_id, t, active_tab_id));
    }
    if insert_at == Some(tabs.len()) {
        tab_row = tab_row.push(insertion_marker());
    }
    let active = tabs.iter().find(|t| Some(t.id.as_str()) == active_tab_id.as_deref());

    // 右侧：状态指示 + ≡ 菜单（空面板也有 ≡：删除/分割仍可用）
    let mut right = row![].spacing(4).padding(Padding::ZERO.right(6.0));
    if let Some(t) = active {
        if is_note_view(&t.view) {
            if let Some(pill) = doc_status_pill(app) {
                right = right.push(pill);
            }
        }
    }
    right = right.push(
        mouse_area(
            button(icon("menu", 13.0, pal::TEXT_MUTED))
                .padding(Padding::ZERO.horizontal(6.0).vertical(2.0))
                .style(|_t, status| button::Style {
                    background: None,
                    text_color: if matches!(status, button::Status::Hovered | button::Status::Pressed) {
                        pal::TEXT_MUTED.scale_alpha(0.7)
                    } else {
                        pal::TEXT_MUTED
                    },
                    border: border::rounded(pal::RADIUS_XS),
                    ..Default::default()
                }),
        )
        .on_press(btn_popup(
            window,
            app.cursor_for(window),
            PopupState::PanelMenu { panel_id: panel_id.to_string() },
        )),
    );

    let header = stack![
        container(space())
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_t| Style {
                background: Some(pal::BG_SECONDARY.into()),
                ..Style::default()
            }),
        // 底边线贴底 1px：container 背景铺满自身 bounds，靠上内边距只会把整块染成线色
        container(hline())
            .height(Length::Fill)
            .align_y(Vertical::Bottom),
        row![
            scrollable(container(row![tab_row]).padding(Padding::ZERO.left(UiMetrics::HEADER_INSET)))
                .direction(scrollable::Direction::Horizontal(scroll::hidden()))
                .on_scroll({
                    let panel_id = panel_id.to_string();
                    move |v| Message::TabStripScrolled(Some(panel_id.clone()), v.absolute_offset().x)
                })
                .width(Length::Fill)
                .height(Length::Fill),
            right,
        ]
        .height(Length::Fixed(UiMetrics::TAB_BAR_H))
        .align_y(Vertical::Center),
    ]
    .width(Length::Fill)
    .height(Length::Fixed(UiMetrics::TAB_BAR_H));
    // 头部空白右键 = 视图选择菜单（标签右键为空档位，同样落到此处；与空面板内容区一致）
    let cursor = app.cursor_for(window);
    let header = mouse_area(header).on_right_press(ctx_popup(
        window,
        cursor,
        PopupState::ViewPicker { panel_id: panel_id.to_string() },
    ));

    let content: Element<'a, Message> = match active {
        // 空面板 = 纯空白（右键唤起视图选择器）
        None => mouse_area(space().width(Length::Fill).height(Length::Fill))
            .on_right_press(Message::PopupOpen(
                window,
                PopupOpen {
                    point: app.cursor_for(window),
                    flip_y: None,
                    state: PopupState::ViewPicker { panel_id: panel_id.to_string() },
                    min_width: 0.0,
                },
            ))
            .into(),
        Some(t) => view_content(app, window, &t.view),
    };

    // 拖拽 drop 指示：中部 = 强调 22% 填充 + 60% 外框圆角 4；四边缘 = 12% 条带
    // （指示作为追加子叠放，显隐不重建面板内容子树——拖拽会话不丢滚动与焦点）
    let base = container(
        column![header, container(content).width(Length::Fill).height(Length::Fill)]
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill);
    let mut root = iced::widget::Stack::new().push(base);
    if let Some(kind) = app
        .drag
        .as_ref()
        .and(app.drag_hover.as_ref())
        .filter(|h| h.zone.panel_id.as_deref() == Some(panel_id))
        .map(|h| h.kind)
    {
        root = root.push(drop_indicator(kind));
    }
    root.width(Length::Fill).height(Length::Fill).into()
}

/// 标签排序插入位标记（2px 强调竖条，随标签条拉伸）。
fn insertion_marker() -> Element<'static, Message> {
    container(space())
        .width(Length::Fixed(2.0))
        .height(Length::Fill)
        .style(|_t| Style {
            background: Some(pal::ACCENT.into()),
            ..Style::default()
        })
        .into()
}

/// 标题栏布局 tab 条的插入位标记（高度同 tab 盒，条带行内不参与拉伸）。
fn titlebar_insert_marker() -> Element<'static, Message> {
    container(insertion_marker()).height(Length::Fixed(28.0)).into()
}

/// drop 指示覆盖层：中部 = 强调 22% 填充块 + 60% 1px 外框圆角 4；四边缘 = 12% 条带
/// （FillPortion 行列实现，带宽随面板尺寸自适应）。
fn drop_indicator(kind: DropZone) -> Element<'static, Message> {
    let band_style = |_t: &iced::Theme| Style {
        background: Some(pal::ACCENT.scale_alpha(0.22).into()),
        ..Style::default()
    };
    let band = || {
        container(space())
            .width(Length::Fill)
            .height(Length::Fill)
            .style(band_style)
    };
    let rest = || container(space()).width(Length::Fill).height(Length::Fill);
    match kind {
        DropZone::Center => container(space())
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_t| Style {
                background: Some(pal::ACCENT.scale_alpha(0.22).into()),
                border: iced::border::Border {
                    color: pal::ACCENT.scale_alpha(0.6),
                    width: 1.0,
                    radius: iced::border::Radius::from(4.0),
                },
                ..Style::default()
            })
            .into(),
        DropZone::Left => row![band().width(Length::FillPortion(12)), rest().width(Length::FillPortion(88))]
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
        DropZone::Right => row![rest().width(Length::FillPortion(88)), band().width(Length::FillPortion(12))]
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
        DropZone::Top => column![band().height(Length::FillPortion(12)), rest().height(Length::FillPortion(88))]
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
        DropZone::Bottom => column![rest().height(Length::FillPortion(88)), band().height(Length::FillPortion(12))]
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
        DropZone::Tab => space().into(),
    }
}

/// 面板标签（自适应宽 + 视图图标 + 顶部圆角；激活 = 内容区同色底 + 2px 金下边盖线；
/// 打开文件的 × = 关文件留标签）。
fn panel_tab<'a>(
    app: &'a App,
    panel_id: &str,
    t: &'a TabItem,
    active_tab_id: &Option<String>,
) -> Element<'a, Message> {
    let active = active_tab_id.as_deref() == Some(t.id.as_str());
    let label = tab_label(app, &t.view);
    let has_file = (is_note_view(&t.view) && app.ui.last_note_file.is_some())
        || (t.view == "table" && app.ui.last_table_file.is_some())
        || (is_canvas_view(&t.view) && app.ui.last_canvas_file.is_some())
        || app
            .doc_open
            .as_ref()
            .is_some_and(|d| crate::plugin_actions::doc_view_kind(&d.plugin_id, &d.contribution_id) == t.view);
    let window = app.main_id.unwrap_or(iced::window::Id::unique());
    let cursor = app.cursor_for(window);

    let text_color = if active { pal::TEXT_PRIMARY } else { pal::TEXT_SECONDARY };
    let mut label_row = row![].spacing(4);
    label_row = label_row.push(icon_stroke(&view_icon(app, &t.view), 13.0, text_color, 1.5));
    label_row = label_row.push(
        text(ellipsize(&label, UiMetrics::TAB_TEXT_MAX_W, 12.0))
            .size(pal::fs_caption())
            .color(text_color),
    );
    if t.locked {
        label_row = label_row.push(icon("lock", 9.0, text_color));
    }
    if has_file && !t.locked {
        label_row = label_row.push(
            button(icon("x", 11.0, pal::TEXT_MUTED))
                .padding(2.0)
                .style(|_t, status| button::Style {
                    background: None,
                    text_color: if matches!(status, button::Status::Hovered | button::Status::Pressed) {
                        pal::TEXT_MUTED.scale_alpha(0.7)
                    } else {
                        pal::TEXT_MUTED
                    },
                    border: border::rounded(pal::RADIUS_XS),
                    ..Default::default()
                })
                .on_press(Message::PanelTabFileClose(t.view.clone())),
        );
    }

    // 顶部圆角（rounded-t）：激活标签底色与内容区同色、直抵底边
    let top_radius = iced::border::Radius {
        top_left: pal::RADIUS_SM,
        top_right: pal::RADIUS_SM,
        bottom_right: 0.0,
        bottom_left: 0.0,
    };
    let content = container(label_row)
        .height(Length::Fill)
        .padding(Padding::ZERO.horizontal(8.0))
        .align_y(Vertical::Center);

    // 显式零内边距：估宽（approx_tab_width）按内容记账，默认内边距会让命中几何偏窄
    let btn = button(container(column![
        content,
        container(space())
            .width(Length::Fill)
            .height(Length::Fixed(2.0))
            .style(move |_t| Style {
                background: if active { Some(pal::ACCENT.into()) } else { None },
                ..Style::default()
            }),
    ])
    .height(Length::Fill))
    .padding(0.0)
    .height(Length::Fixed(UiMetrics::TAB_BAR_H))
    .style(move |_t, status| button::Style {
        background: if active {
            Some(pal::BG_PRIMARY.into())
        } else if matches!(status, button::Status::Hovered | button::Status::Pressed) {
            Some(pal::HOVER.into())
        } else {
            None
        },
        border: iced::border::Border { color: iced::Color::TRANSPARENT, width: 0.0, radius: top_radius },
        ..Default::default()
    })
    .on_press_maybe((!active).then(|| Message::PanelTab(panel_id.to_string(), t.id.clone())));

    // 按下候选经 PressReporter 上报（mouse_area 对被按钮捕获的按下事件不触发）；
    // 锁定标签右键仍开菜单（iced mouse_area 无 maybe 变体），条目全部禁用表达锁定语义
    controls::PressReporter::new(
        mouse_area(btn)
            .on_right_press(ctx_popup(
                window,
                cursor,
                PopupState::TabContext {
                    panel_id: panel_id.to_string(),
                    tab_id: t.id.clone(),
                },
            ))
            .into(),
        Message::PanelTabPress {
            panel_id: panel_id.to_string(),
            tab_id: t.id.clone(),
        },
    )
    .into()
}

/// 笔记保存状态徽标：错误 = ErrorPill（红调底 + 重试/关闭），未保存 = 圆点徽标。
fn doc_status_pill(app: &App) -> Option<Element<'static, Message>> {
    app.ui.last_note_file.as_ref()?;
    if app.doc_save_error.is_some() {
        Some(
            container(
                row![
                    text(ellipsize(app.doc_save_error.as_deref().unwrap_or("保存失败"), 160.0, 11.0))
                        .size(pal::fs_micro())
                        .color(pal::DANGER),
                    button(text("重试").size(pal::fs_micro()).color(pal::DANGER))
                        .padding(Padding::ZERO.horizontal(6.0).vertical(1.0))
                        .style(|_t, _s| button::Style {
                            background: Some(pal::DANGER.scale_alpha(0.2).into()),
                            border: border::rounded(pal::RADIUS_XS),
                            ..Default::default()
                        })
                        .on_press(Message::Save),
                    button(icon("x", 11.0, pal::DANGER))
                        .padding(1.0)
                        .style(|_t, _s| button::Style::default())
                        .on_press(Message::SaveErrorDismiss),
                ]
                .spacing(4),
            )
            .padding(Padding::ZERO.horizontal(6.0).vertical(2.0))
            .style(|_t| Style {
                background: Some(pal::DANGER.scale_alpha(0.12).into()),
                border: border::rounded(pal::RADIUS_XS),
                ..Style::default()
            })
            .into(),
        )
    } else if app.doc_dirty {
        Some(controls::status_pill(controls::PillStatus::Pending, "未保存"))
    } else {
        None
    }
}

/// 按视图 kind 分派内容（所属内置插件停用时先给占位；预置插件视图按 id 查渲染表；插件面板按整面帧渲染）。
fn view_content<'a>(app: &'a App, window: iced::window::Id, kind: &str) -> Element<'a, Message> {
    let disabled_builtin = app
        .registry
        .get(kind)
        .is_some_and(|entry| entry.source.is_builtin() && !entry.enabled);
    if disabled_builtin {
        return disabled_view(app, kind);
    }
    let content = if crate::plugin_actions::parse_view_kind(kind).is_some() {
        crate::plugin_view::panel_view(app, window, kind)
    } else {
        crate::pack::render(kind, app, window).unwrap_or_else(|| placeholder_view(app, kind))
    };
    // 插件为该面板声明了头部、且内容槽当前确实是插件面时才渲染头部：
    // 内置内容自带工具条，两条并存会重复。
    let plugin_content = crate::plugin_actions::parse_view_kind(kind).is_some();
    let header = plugin_content
        .then(|| crate::panel_header::header_row(app, window, kind, kind))
        .flatten();
    match header {
        Some(header) => iced::widget::column![header, content]
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
        None => content,
    }
}

/// 停用视图的占位：标签保留（不关标签、不动布局），内容区就地给重新启用入口。
fn disabled_view(app: &App, kind: &str) -> Element<'static, Message> {
    let plugin_id = app
        .registry
        .get(kind)
        .map(|entry| entry.source.plugin_id().to_string())
        .unwrap_or_default();
    let action = (!plugin_id.is_empty()).then(|| {
        Element::from(controls::btn(
            "重新启用",
            &controls::btn_sm(),
            Variant::Primary,
            Some(Message::Plugins(
                crate::settings_view::plugins::PluginsMsg::SetBuiltinPluginEnabled(plugin_id, true),
            )),
        ))
    });
    controls::view_placeholder_action(
        &view_icon(app, kind),
        view_label(app, kind),
        "所属内置插件已停用".to_string(),
        action,
    )
}

/// 占位视图：大图标 + 名称 + 未实现说明（PanelPlaceholder 同款形态）。
fn placeholder_view(app: &App, kind: &str) -> Element<'static, Message> {
    let icon_key = view_icon(app, kind);
    controls::view_placeholder(
        &icon_key,
        view_label(app, kind),
        "该视图尚未实现".to_string(),
    )
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
    // 接管中的撕裂窗口：整窗视图由插件面替换（与主窗同一套接管组装）
    if let Some(t) = app.plugin_takeovers.get(&window) {
        return takeover_view(app, window, t);
    }
    let cursor = app.cursor_for(window);

    let mut tab_row = row![].spacing(2).height(Length::Fill);
    // 拖拽插入条：命中本撕裂窗标签条时在插入位渲染 2px 强调竖条（与主窗面板一致）
    let insert_at = app.drag.as_ref().and_then(|d| {
        let hover = app.drag_hover.as_ref()?;
        (hover.zone.detached_id.as_deref() == Some(model_id) && hover.kind == DropZone::Tab)
            .then(|| tab_index_at(&hover.zone, d.current.x, 0.0))
    });
    for (i, t) in w.tabs.iter().enumerate() {
        if insert_at == Some(i) {
            tab_row = tab_row.push(insertion_marker());
        }
        let active = w.active_tab_id.as_deref() == Some(t.id.as_str());
        let label = tab_label(app, &t.view);
        let has_file = (is_note_view(&t.view) && app.ui.last_note_file.is_some())
            || (t.view == "table" && app.ui.last_table_file.is_some())
            || (is_canvas_view(&t.view) && app.ui.last_canvas_file.is_some())
            || app
                .doc_open
                .as_ref()
                .is_some_and(|d| crate::plugin_actions::doc_view_kind(&d.plugin_id, &d.contribution_id) == t.view);
        let text_color = if active { pal::TEXT_PRIMARY } else { pal::TEXT_SECONDARY };
        let mut label_row = row![].spacing(4);
        label_row = label_row.push(icon_stroke(&view_icon(app, &t.view), 13.0, text_color, 1.5));
        label_row = label_row.push(
            text(ellipsize(&label, UiMetrics::TAB_TEXT_MAX_W, 12.0))
                .size(pal::fs_caption())
                .color(text_color),
        );
        if t.locked {
            label_row = label_row.push(icon("lock", 9.0, text_color));
        }
        if has_file && !t.locked {
            label_row = label_row.push(
                button(icon("x", 11.0, pal::TEXT_MUTED))
                    .padding(2.0)
                    .style(|_t, status| button::Style {
                        background: None,
                        text_color: if matches!(status, button::Status::Hovered | button::Status::Pressed) {
                            pal::TEXT_MUTED.scale_alpha(0.7)
                        } else {
                            pal::TEXT_MUTED
                        },
                        border: border::rounded(pal::RADIUS_XS),
                        ..Default::default()
                    })
                    .on_press(Message::DetachedTabFileClose(t.view.clone())),
            );
        }
        let top_radius = iced::border::Radius {
            top_left: pal::RADIUS_SM,
            top_right: pal::RADIUS_SM,
            bottom_right: 0.0,
            bottom_left: 0.0,
        };
        let content = container(label_row)
            .height(Length::Fill)
            .padding(Padding::ZERO.horizontal(8.0))
            .align_y(Vertical::Center);
        tab_row = tab_row.push(controls::PressReporter::new(
            mouse_area(
                button(container(column![
                    content,
                    container(space())
                        .width(Length::Fill)
                        .height(Length::Fixed(2.0))
                        .style(move |_t| Style {
                            background: if active { Some(pal::ACCENT.into()) } else { None },
                            ..Style::default()
                        }),
                ])
                .height(Length::Fill))
                .padding(0.0)
                .height(Length::Fixed(UiMetrics::TAB_BAR_H))
                .style(move |_t, status| button::Style {
                    background: if active {
                        Some(pal::BG_PRIMARY.into())
                    } else if matches!(status, button::Status::Hovered | button::Status::Pressed) {
                        Some(pal::HOVER.into())
                    } else {
                        None
                    },
                    border: iced::border::Border { color: iced::Color::TRANSPARENT, width: 0.0, radius: top_radius },
                    ..Default::default()
                })
                .on_press_maybe(
                    (!active).then(|| Message::DetachedTab(w.id.clone(), t.id.clone())),
                ),
            )
            .on_right_press(ctx_popup(window, cursor, PopupState::DetachedTabContext { window_id: w.id.clone(), tab_id: t.id.clone(), }))
            .into(),
            Message::DetachedTabPress {
                window_id: w.id.clone(),
                tab_id: t.id.clone(),
            },
        ));
    }
    if insert_at == Some(w.tabs.len()) {
        tab_row = tab_row.push(insertion_marker());
    }

    let mut right = row![].spacing(4).padding(Padding::ZERO.right(6.0));
    right = right.push(
        mouse_area(
            button(icon("menu", 13.0, pal::TEXT_MUTED))
                .padding(Padding::ZERO.horizontal(6.0).vertical(2.0))
                .style(|_t, status| button::Style {
                    background: None,
                    text_color: if matches!(status, button::Status::Hovered | button::Status::Pressed) {
                        pal::TEXT_MUTED.scale_alpha(0.7)
                    } else {
                        pal::TEXT_MUTED
                    },
                    border: border::rounded(pal::RADIUS_XS),
                    ..Default::default()
                }),
        )
        .on_press(btn_popup(window, cursor, PopupState::DetachedPanelMenu { window_id: w.id.clone() })),
    );

    let header = stack![
        container(space()).width(Length::Fill).height(Length::Fill).style(|_t| Style {
            background: Some(pal::BG_SECONDARY.into()),
            ..Style::default()
        }),
        // 底边线贴底 1px：container 背景铺满自身 bounds，靠上内边距只会把整块染成线色
        container(hline())
            .height(Length::Fill)
            .align_y(Vertical::Bottom),
        row![
            container(tab_row).padding(Padding::ZERO.left(UiMetrics::HEADER_INSET)),
            space().width(Length::Fill),
            right,
        ]
        .height(Length::Fixed(UiMetrics::TAB_BAR_H))
        .align_y(Vertical::Center),
    ]
    .width(Length::Fill)
    .height(Length::Fixed(UiMetrics::TAB_BAR_H));

    // 撕裂窗口标题栏：标题文本 + 弹性拖拽区 + 图钉/最小化/关闭
    let active_title = w
        .active_tab_id
        .as_deref()
        .and_then(|tid| w.tabs.iter().find(|t| t.id == tid))
        .map(|t| view_label(app, &t.view))
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
            text(active_title).size(pal::fs_caption()).color(pal::TEXT_SECONDARY),
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
        .map(|t| view_content(app, window, &t.view))
        .unwrap_or_else(|| space().width(Length::Fill).height(Length::Fill).into());

    // 拖拽 drop 指示（撕裂窗口 = 整体加标签：中部填充 + 外框）
    let drop = app.drag.as_ref().and_then(|_| {
        app.drag_hover
            .as_ref()
            .filter(|h| h.zone.detached_id.as_deref() == Some(model_id))
            .map(|h| h.kind)
    });
    let base = column![titlebar, header, container(content).width(Length::Fill).height(Length::Fill)]
        .width(Length::Fill)
        .height(Length::Fill);
    // drop 指示追加子叠放：显隐不重建内容子树（同面板网格的处理）
    let mut root = iced::widget::Stack::new().push(base);
    if let Some(kind) = drop {
        root = root.push(drop_indicator(kind));
    }
    with_overlays(app, window, root.width(Length::Fill).height(Length::Fill).into())
}

/// 协作者胶囊条（色点 + 昵称；超过 3 个折叠为「+N」）。笔记工具条与表格工具条共用，
/// 展示顺序由调用方给定的成员序决定（不可点击的纯展示）。
pub(crate) fn collab_pill_row(members: &[(iced::Color, String)]) -> Element<'static, Message> {
    const MAX_SHOWN: usize = 3;
    let mut row = iced::widget::Row::<Message>::new().spacing(4);
    for (color, name) in members.iter().take(MAX_SHOWN) {
        let dot = *color;
        row = row.push(
            container(
                iced::widget::row![
                    container(space())
                        .width(Length::Fixed(8.0))
                        .height(Length::Fixed(8.0))
                        .style(move |_t| Style {
                            background: Some(dot.into()),
                            border: border::rounded(999.0),
                            ..Style::default()
                        }),
                    text(name.clone())
                        .size(pal::fs_micro())
                        .color(pal::TEXT_PRIMARY),
                ]
                .spacing(5)
                // 色点与昵称同基准线对齐：色点固定 8px，靠行内居中抵消文字行高差
                .align_y(Vertical::Center),
            )
            .padding(Padding::ZERO.horizontal(8.0).vertical(4.0))
            .style(|_t| Style {
                background: Some(pal::HOVER.into()),
                border: border::rounded(999.0),
                ..Style::default()
            }),
        );
    }
    if members.len() > MAX_SHOWN {
        row = row.push(
            container(
                text(format!("+{}", members.len() - MAX_SHOWN))
                    .size(pal::fs_micro())
                    .color(pal::TEXT_SECONDARY),
            )
            .padding(Padding::ZERO.horizontal(8.0).vertical(4.0))
            .style(|_t| Style {
                background: Some(pal::HOVER.into()),
                border: border::rounded(999.0),
                ..Style::default()
            }),
        );
    }
    row.into()
}

/// 内置扩展名 → 行图标键。
fn file_icon_base(name: &str) -> &'static str {
    let lower = name.to_lowercase();
    if atelyx_core::vault::image_ext_of(&lower).is_some() {
        "image"
    } else if lower.ends_with(".atlx") {
        "palette"
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

/// 文件行的图标键：插件注册的领域文档类型按注册图标，其余走内置映射。
pub(crate) fn file_icon(app: &App, name: &str) -> Cow<'static, str> {
    if let Some(ext) = name.rsplit('.').next() {
        if let Some(info) = app
            .registry
            .doc_types()
            .iter()
            .find(|i| i.registration.extension.eq_ignore_ascii_case(ext))
        {
            return crate::plugin_actions::plugin_icon_key(
                Some(&info.registration.icon),
                "paperclip",
            );
        }
    }
    Cow::Borrowed(file_icon_base(name))
}

/// 文件名的显示主名（去掉目录与扩展名）。
pub fn file_label(rel: &str) -> String {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    atelyx_core::vault::file_stem(name).to_string()
}

/// 十六进制颜色串（`#rrggbb`）→ Color；非法输入返回 None（回落默认色）。
pub(crate) fn parse_hex_color(hex: &str) -> Option<iced::Color> {
    if hex.len() != 7 || !hex.starts_with('#') {
        return None;
    }
    crate::ui::color_picker::parse_hex(hex)
}

/// 毫秒时间戳 → 本地短文本（YYYY-MM-DD HH:MM；纪元前显示原值）。
pub(crate) fn format_ts(ms: i64) -> String {
    // 天数 → 日期（与 core 历史模块同一 civil 算法的简版；仅展示用，按本地时区偏移折算）
    let secs = (ms + atelyx_core::history::local_offset_ms()).div_euclid(1000);
    let days = secs.div_euclid(86400);
    let secs_of_day = secs.rem_euclid(86400);
    let (h, m) = (secs_of_day / 3600, (secs_of_day % 3600) / 60);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mon = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = y + i64::from(mon <= 2);
    format!("{year:04}-{mon:02}-{d:02} {h:02}:{m:02}")
}

// ===== 弹层内容 =====

/// 弹层内容构造：按形态产出（元素, 估算高度）。
fn popup_element<'a>(
    app: &'a App,
    state: &PopupState,
    pane: usize,
    window: iced::window::Id,
) -> (Element<'a, Message>, f32) {
    let entries: Vec<(usize, MenuEntry<Message>)> = match state {
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
                        Message::DetachedTabLock(wid.clone())),
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
            let mut entries: Vec<(usize, MenuEntry<Message>)> = vec![
                (
                    0,
                    MenuEntry::Action {
                        label: "关闭该标签".into(),
                        icon: Some("x".into()),
                        danger: false,
                        disabled: locked,
                        on_press: (!locked).then(|| Message::DetachedTabClose(wid.clone(), tid.clone())),
                        note: None,
                        hint: locked.then(|| "已锁定，需先解锁".to_string()),
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
            // 视图清单：当前视图停用（角标「当前」）；他处占用停用（角标「已占用」）
            let current_view = app
                .ui
                .detached_windows
                .iter()
                .find(|w| w.id == *window_id)
                .and_then(|w| w.tabs.iter().find(|t| t.id == *tab_id))
                .map(|t| t.view.clone());
            for kind in available_view_kinds(app) {
                let entry = if current_view.as_deref() == Some(kind.as_str()) {
                    MenuEntry::noted(view_label(app, &kind), view_icon(app, &kind), "当前", true, None)
                } else if view_occupied(app, &kind, None) {
                    MenuEntry::noted(view_label(app, &kind), view_icon(app, &kind), "已占用", true, None)
                        .hinted("该视图已在其他位置（全局唯一）")
                } else {
                    MenuEntry::action_icon(
                        view_label(app, &kind),
                        view_icon(app, &kind),
                        Message::DetachedSetTabView {
                            window_id: wid.clone(),
                            tab_id: tid.clone(),
                            view: kind,
                        },
                    )
                };
                entries.push((1, entry));
            }
            entries
        }
        PopupState::EntrySceneMenu => {
            let pick = |id: Option<String>| Message::EntryScenePick(id);
            let current = app.entry_scene_id.as_deref();
            let mut entries: Vec<(usize, MenuEntry<'static, Message>)> = vec![(
                0,
                if current.is_none() {
                    MenuEntry::Checked { label: "跟随当前".into(), on_press: pick(None) }
                } else {
                    MenuEntry::action("跟随当前", pick(None))
                },
            )];
            for s in &app.ui.scenes {
                let is_current = current == Some(s.id.as_str());
                let (id, name) = (s.id.clone(), s.name.clone());
                entries.push((
                    0,
                    if is_current {
                        MenuEntry::Checked { label: name, on_press: pick(Some(id)) }
                    } else {
                        MenuEntry::action(name, pick(Some(id)))
                    },
                ));
            }
            entries
        }
        PopupState::PluginMenu => plugin_menu_entries(app, pane),
        PopupState::SpaceMenu => space_menu_entries(app, window),
        PopupState::SpaceLogin { register } => space_login_entries(app, *register),
        PopupState::SpaceCreate => vec![
            (
                0,
                MenuEntry::Custom(
                    space_form_input("空间名", &app.space_form.name.clone(), |v| {
                        Message::Space(crate::SpaceMsg::LoginField(crate::SpaceField::Name, v))
                    }, false),
                    30.0,
                ),
            ),
            (
                0,
                MenuEntry::Custom(space_hint("创建后你将是该空间的拥有者（owner）"), 18.0),
            ),
            (
                0,
                MenuEntry::Custom(space_action_row(
                    "创建空间",
                    app.space_busy,
                    Message::Space(crate::SpaceMsg::CreateSpace),
                ), 30.0),
            ),
        ],
        PopupState::SpaceJoin => vec![
            (
                0,
                MenuEntry::Custom(
                    space_form_input("邀请码", &app.space_form.code.clone(), |v| {
                        Message::Space(crate::SpaceMsg::LoginField(crate::SpaceField::Code, v))
                    }, false),
                    30.0,
                ),
            ),
            (
                0,
                MenuEntry::Custom(space_action_row(
                    "加入空间",
                    app.space_busy,
                    Message::Space(crate::SpaceMsg::JoinSpace),
                ), 30.0),
            ),
        ],
        PopupState::SpaceMembers => space_members_entries(app),
        PopupState::SpaceInvite => space_invite_entries(app),
        PopupState::FontPicker => font_picker_entries(app, window),
        PopupState::ColorPicker { target, hsv, hex } => {
            let (panel, height) = crate::color_picker_actions::panel(app, target, *hsv, hex);
            vec![(0, MenuEntry::Custom(panel, height))]
        }
        PopupState::PanelViewMenu { target } => {
            let active = app.panel_view.get(target).cloned().flatten();
            let label = view_label(app, target);
            let default_pick = Message::PanelViewSelect { target: target.clone(), view: None };
            // 缺省项就是表格插件的网格视图：下拉里不再重复列同一条贡献
            let is_default = active
                .as_deref()
                .is_none_or(|kind| kind == crate::plugin_actions::TABLES_VIEW);
            let mut entries: Vec<(usize, MenuEntry<'static, Message>)> = vec![(
                0,
                if is_default {
                    MenuEntry::Checked { label, on_press: default_pick }
                } else {
                    MenuEntry::action(label, default_pick)
                },
            )];
            let views: Vec<(String, String)> = app
                .registry
                .views_in(ContributionSlot::TableView)
                .filter(|entry| entry.id != crate::plugin_actions::TABLES_VIEW)
                .map(|entry| (entry.label.clone(), entry.id.clone()))
                .collect();
            if !views.is_empty() {
                entries.push((0, MenuEntry::Divider));
                for (label, kind) in views {
                    let pick = Message::PanelViewSelect {
                        target: target.clone(),
                        view: Some(kind.clone()),
                    };
                    entries.push((
                        0,
                        if active.as_deref() == Some(kind.as_str()) {
                            MenuEntry::Checked { label, on_press: pick }
                        } else {
                            MenuEntry::action(label, pick)
                        },
                    ));
                }
            }
            entries
        }
        PopupState::PanelItemsMenu { target } => crate::panel_header::item_entries(app, target),
        PopupState::AiModelMenu => {
            let mut entries: Vec<(usize, MenuEntry<Message>)> = Vec::new();
            for p in app.repo_config.providers.iter().flatten() {
                if p.models.is_empty() {
                    continue;
                }
                entries.push((0, MenuEntry::Group(p.name.clone())));
                for m in &p.models {
                    let is_current = app
                        .repo_config
                        .auto_naming_model
                        .as_ref()
                        .is_some_and(|o| o.provider_id == p.id && o.model == m.id);
                    let pick =
                        Message::Ai(ai::AiMsg::NamingModelPick(Some((p.id.clone(), m.id.clone()))));
                    entries.push((0, if is_current {
                        MenuEntry::Checked { label: model_display(m), on_press: pick }
                    } else {
                        MenuEntry::action(model_display(m), pick)
                    }));
                }
            }
            if entries.is_empty() {
                entries.push((0, MenuEntry::Custom(popup_hint("先在「供应商」添加模型"), 18.0)));
            }
            entries
        }
        PopupState::AiNamingMenu => {
            let mode = ai::naming_mode(app);
            [
                (ai::NamingMode::Off, "不启用"),
                (ai::NamingMode::FollowDefault, "跟随默认"),
                (ai::NamingMode::Custom, "指定模型"),
            ]
            .into_iter()
            .map(|(m, label)| {
                let pick = Message::Ai(ai::AiMsg::NamingModePick(m));
                (0, if mode == m {
                    MenuEntry::Checked { label: label.into(), on_press: pick }
                } else {
                    MenuEntry::action(label, pick)
                })
            })
            .collect()
        }
        PopupState::AiAgentPromptMenu => {
            // 弹层由 Agent 表单触发：当前值以草稿优先（可能含未保存的改动）
            let current = app.ai.agent_draft.as_ref().and_then(|d| d.prompt_file.clone());
            let mut entries: Vec<(usize, MenuEntry<Message>)> = vec![(
                0,
                if current.is_none() {
                    MenuEntry::Checked { label: "无".into(), on_press: Message::Ai(ai::AiMsg::AgentPromptPick(None)) }
                } else {
                    MenuEntry::action("无", Message::Ai(ai::AiMsg::AgentPromptPick(None)))
                },
            )];
            for rel in &app.prompt_notes {
                let label = rel.rsplit('/').next().unwrap_or(rel).trim_end_matches(".md").to_string();
                let is_current = current.as_deref() == Some(rel.as_str());
                let pick = Message::Ai(ai::AiMsg::AgentPromptPick(Some(rel.clone())));
                entries.push((0, if is_current {
                    MenuEntry::Checked { label, on_press: pick }
                } else {
                    MenuEntry::action(label, pick)
                }));
            }
            if app.prompt_notes.is_empty() {
                entries.push((0, MenuEntry::Custom(popup_hint("暂无提示词标记（文件右键可注册）"), 18.0)));
            }
            entries
        }
        PopupState::AiProviderDeleteMenu => {
            vec![
                (
                    0,
                    MenuEntry::Submenu { label: "删除供应商".into(), icon: Some("trash-2".into()), disabled: false, pane: 1 },
                ),
                (
                    1,
                    MenuEntry::danger("确认删除（并清除其密钥链条目）", "trash-2", Message::Ai(ai::AiMsg::ProviderDelete)),
                ),
                (1, MenuEntry::action("取消", Message::Ui(crate::ui::UiMsg::PopupClose { window }))),
            ]
        }
        PopupState::AiAgentDeleteMenu => {
            vec![
                (
                    0,
                    MenuEntry::Submenu { label: "删除 Agent".into(), icon: Some("trash-2".into()), disabled: false, pane: 1 },
                ),
                (1, MenuEntry::danger("确认删除", "trash-2", Message::Ai(ai::AiMsg::AgentDelete))),
                (1, MenuEntry::action("取消", Message::Ui(crate::ui::UiMsg::PopupClose { window }))),
            ]
        }
        PopupState::AiSearchProviderMenu => {
            let current = app
                .ai
                .search_draft
                .as_ref()
                .map(|d| d.provider.clone())
                .or_else(|| app.repo_config.search.as_ref().map(|s| s.provider.clone()))
                .unwrap_or_default();
            [("tavily", "Tavily"), ("searxng", "SearXNG")]
                .into_iter()
                .map(|(key, label)| {
                    // 空值 = 未配置，缺省语义按 Tavily 展示
                    let is_current = current == key || (current.is_empty() && key == "tavily");
                    let pick = Message::Ai(ai::AiMsg::SearchProviderPick(key.to_string()));
                    (0, if is_current {
                        MenuEntry::Checked { label: label.into(), on_press: pick }
                    } else {
                        MenuEntry::action(label, pick)
                    })
                })
                .collect()
        }
        // 居中模态不经锚点渲染（with_overlays 分流到各自 overlay）
        PopupState::HistoryBrowser { .. } | PopupState::LicenseInfo => Vec::new(),
    };
    crate::ui::menu::pane_view(entries, pane, window)
}

/// 菜单搜索行高度（px）：字体弹层与插件声明搜索的菜单共用。
const MENU_SEARCH_H: f32 = 30.0;

/// 弹层内联输入（单行；提交由菜单项动作携带草稿）。
fn popup_text_input(
    placeholder: &str,
    draft: &str,
    on_input: impl Fn(String) -> Message + 'static,
) -> Element<'static, Message> {
    iced::widget::text_input(placeholder, draft)
        .size(pal::fs_caption())
        .font(crate::ui::regular())
        .padding(Padding::ZERO.horizontal(12.0).vertical(6.0))
        .on_input(on_input)
        .style(|_theme, status| controls::form_input_style(status))
        .into()
}

/// 字体选择弹层条目：过滤输入 + 复位项 + 命中族清单（清单定高，超出走内滚动）。
fn font_picker_entries(
    app: &App,
    window: iced::window::Id,
) -> Vec<(usize, MenuEntry<'static, Message>)> {
    /// 清单可视区行数上下限（再多也只给滚动区，不让弹层顶满屏幕）。
    const MIN_ROWS: usize = 2;
    const MAX_ROWS: usize = 16;

    let hits = crate::ui::font::filter(&app.font_picker_query);
    let current = app.global.font_family.clone();
    let resolved_current = current.as_deref().and_then(crate::ui::font::lookup);
    let reset = Message::InterfaceFontSet(None);
    let reset_label = "跟随系统";
    let mut entries: Vec<(usize, MenuEntry<'static, Message>)> = vec![
        (
            0,
            MenuEntry::Custom(
                popup_text_input("搜索字体…", &app.font_picker_query, Message::FontPickerQuery),
                MENU_SEARCH_H,
            ),
        ),
        (0, MenuEntry::Divider),
    ];
    let reset_entry = if current.is_none() {
        MenuEntry::Checked { label: reset_label.to_string(), on_press: reset }
    } else {
        MenuEntry::Action {
            label: reset_label.to_string(),
            icon: None,
            danger: false,
            disabled: false,
            on_press: Some(reset),
            note: None,
            hint: None,
        }
    };
    entries.push((0, reset_entry));
    if hits.is_empty() {
        entries.push((0, MenuEntry::Custom(popup_hint("没有匹配的字体"), 18.0)));
        return entries;
    }
    // 可视区高度 = 窗口高 − 弹层固定部分（上下内边距 + 搜索行 + 分隔线 + 复位行）− 阴影环 − 上下留白
    let chrome = pal::POPUP_PAD_Y * 2.0
        + MENU_SEARCH_H
        + pal::MENU_DIVIDER_H
        + pal::MENU_ITEM_H
        + popup::SHADOW_PAD_TOP
        + popup::SHADOW_PAD_BOTTOM
        + pal::VIEWPORT_GAP * 2.0;
    let budget = (viewport_of(app, window).height - chrome).max(MIN_ROWS as f32 * pal::MENU_ITEM_H);
    let list_h = (hits.len().min(MAX_ROWS) as f32 * pal::MENU_ITEM_H).min(budget);
    let rows = hits.into_iter().map(|hit| {
        let name = hit.name.clone();
        let pick = Message::InterfaceFontSet(Some(name.clone()));
        // 存量值可能是同族的另一语言写法（如 SimSun 与 宋体），按解析后的展示名认当前项
        if resolved_current.as_deref() == Some(name.as_str()) {
            MenuEntry::Checked { label: name, on_press: pick }
        } else {
            MenuEntry::action(name, pick)
        }
    });
    entries.push((0, MenuEntry::Scroll { entries: rows.collect(), height: list_h }));
    entries
}


/// 模型显示名：昵称优先（空昵称回落 id）。
fn model_display(m: &atelyx_core::vault::VaultModel) -> String {
    m.nickname
        .clone()
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| m.id.clone())
}

fn popup_hint(text_content: &str) -> Element<'static, Message> {
    container(text(text_content.to_string()).size(pal::fs_micro()).color(pal::TEXT_MUTED))
        .padding(Padding::ZERO.horizontal(12.0))
        .width(Length::Fill)
        .into()
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

/// 该视图是否正展示在任一窗口（主窗口激活布局的各面板激活标签 / 撕裂窗口激活标签）；
/// 面板懒加载（PersistTick 搭载）据此判定是否发起数据任务。
pub fn view_displayed(app: &App, kind: &str) -> bool {
    fn tree_shows(node: &LayoutNode, kind: &str) -> bool {
        match node {
            LayoutNode::Panel { tabs, active_tab_id, .. } => tabs
                .iter()
                .any(|t| t.view == kind && active_tab_id.as_deref() == Some(t.id.as_str())),
            LayoutNode::Split { children, .. } => children.iter().any(|c| tree_shows(c, kind)),
        }
    }
    tree_shows(&layout::active_layout(&app.ui).tree, kind)
        || app.ui.detached_windows.iter().any(|w| {
            w.active_tab_id.as_deref().is_some_and(|aid| {
                w.tabs.iter().any(|t| t.id == aid && t.view == kind)
            })
        })
}

/// 场景选择器：根面板（场景行 + 新建）与删除确认面板。
fn scene_selector_entries(app: &App, confirm: &Option<String>) -> Vec<(usize, MenuEntry<'static, Message>)> {
    let mut root: Vec<(usize, MenuEntry<'static, Message>)> = vec![(0, MenuEntry::Group("场景".into()))];
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
                iced::widget::Id::new(controls::INLINE_RENAME_INPUT),
                "新名称",
                &app.rename.as_ref().map(|r| r.value.clone()).unwrap_or_default(),
                96.0,
                Message::SceneRenameInput,
                Message::LayoutRenameCommit,
                Message::RenamePressInside,
            )
        } else {
            button(
                row![
                    text(name.clone())
                        .size(pal::fs_caption())
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
            // 行尾动作钮：圆角 + 悬停底（显形时机 = 常显）
            row = row.push(
                button(icon("pencil", 11.0, pal::TEXT_MUTED))
                    .padding(2.0)
                    .style(|_t, status| button::Style {
                        background: matches!(status, button::Status::Hovered | button::Status::Pressed)
                            .then_some(pal::HOVER.into()),
                        border: border::rounded(pal::RADIUS_XS),
                        ..Default::default()
                    })
                    .on_press(Message::SceneRenameStart(id.clone())),
            );
            row = row.push(
                button(icon("trash-2", 11.0, pal::DANGER))
                    .padding(2.0)
                    .style(|_t, status| button::Style {
                        background: matches!(status, button::Status::Hovered | button::Status::Pressed)
                            .then_some(pal::HOVER.into()),
                        border: border::rounded(pal::RADIUS_XS),
                        ..Default::default()
                    })
                    .on_press(Message::SceneDeleteAsk(id.clone())),
            );
        }
        root.push((0, MenuEntry::Custom(
            container(row).padding(Padding::ZERO.horizontal(4.0)).width(Length::Fill).into(),
            34.0,
        )));
    }
    root.push((0, MenuEntry::action_icon("新建场景", "plus", Message::SceneNew)));

    // 确认面板：标题用强调字重，按钮区与文案间距 12
    let confirm_entries: Vec<(usize, MenuEntry<'static, Message>)> = if let Some(id) = confirm {
        let scene = app.ui.scenes.iter().find(|s| s.id == *id);
        let name = scene.map(|s| s.name.clone()).unwrap_or_default();
        let count = scene.map(|s| s.layouts.len()).unwrap_or(0);
        let pane = column![
            text(format!("删除场景「{name}」？"))
                .size(pal::fs_caption())
                .color(pal::TEXT_PRIMARY)
                .font(crate::ui::bold()),
            text(format!("其下 {count} 套布局将一并删除。"))
                .size(pal::fs_caption())
                .color(pal::TEXT_MUTED),
            row![
                container(button(text("取消"))
                    .padding(Padding::ZERO.horizontal(10.0).vertical(4.0))
                    .style(|_t, _s| button::Style {
                        text_color: pal::TEXT_SECONDARY,
                        border: border::rounded(pal::RADIUS_SM).color(pal::BORDER).width(1.0),
                        ..Default::default()
                    })
                    .on_press(Message::Ui(crate::ui::UiMsg::PopupPane(0))))
                .align_x(Horizontal::Right),
                button(text("删除"))
                    .padding(Padding::ZERO.horizontal(10.0).vertical(4.0))
                    .style(|_t, _s| button::Style {
                        text_color: pal::DANGER,
                        border: border::rounded(pal::RADIUS_SM).color(pal::DANGER).width(1.0),
                        ..Default::default()
                    })
                    .on_press(Message::SceneDelete(id.clone())),
            ]
            .spacing(8)
            .padding(Padding::ZERO.top(8.0))
            .align_y(Vertical::Center),
        ]
        .spacing(4)
        .padding(12.0)
        .width(Length::Fill);
        vec![(1, MenuEntry::Custom(pane.into(), 114.0))]
    } else {
        Vec::new()
    };
    root.into_iter().chain(confirm_entries).collect()
}

/// 面板 ≡ 菜单：锁定（整块）/ 删除面板（最后一个禁用）/ 分割；空面板无锁定项。
fn panel_menu_entries(app: &App, panel_id: &str) -> Vec<(usize, MenuEntry<'static, Message>)> {
    let has_active = find_active_tab_in(app, panel_id).is_some();
    let main_tree = layout::active_layout(&app.ui).tree;
    let panel_count = count_panels(&main_tree);
    // 整块锁定基准 = 面板全部标签已锁
    let locked = has_active
        && panel_tab_ids_in(&layout::active_layout(&app.ui).tree, panel_id)
            .iter()
            .all(|id| {
                layout::find_tab_in_tree(&main_tree, id).is_some_and(|(_, t)| t.locked)
            });
    let pid = panel_id.to_string();
    let mut entries = Vec::new();
    if has_active {
        entries.push((
            0,
            MenuEntry::action_icon(
                if locked { "解锁" } else { "锁定" },
                if locked { "lock-open" } else { "lock" },
                Message::PanelTabLock(pid.clone()),
            ),
        ));
    }
    if !locked {
        entries.push((
            0,
            MenuEntry::Action {
                label: "删除面板".into(),
                icon: None,
                danger: true,
                disabled: panel_count <= 1,
                on_press: (panel_count > 1).then(|| Message::PanelClose(pid.clone())),
                note: None,
                hint: (panel_count <= 1).then(|| "最后一个面板不可删除".to_string()),
            },
        ));
        entries.push((0, MenuEntry::Divider));
        entries.push((0, MenuEntry::action("左右分割", Message::PanelSplit(pid.clone(), "horizontal".into()))));
        entries.push((0, MenuEntry::action("上下分割", Message::PanelSplit(pid, "vertical".into()))));
    }
    entries
}

/// 主树面板的全部标签 id（整块锁定判定用）。
pub fn panel_tab_ids_in(tree: &LayoutNode, panel_id: &str) -> Vec<String> {
    match tree {
        LayoutNode::Panel { id, tabs, .. } if id == panel_id => {
            tabs.iter().map(|t| t.id.clone()).collect()
        }
        LayoutNode::Split { children, .. } => children
            .iter()
            .find_map(|c| {
                let ids = panel_tab_ids_in(c, panel_id);
                (!ids.is_empty()).then_some(ids)
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
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
) -> Vec<(usize, MenuEntry<'static, Message>)> {
    let tab = find_tab_in_active(app, tab_id);
    let locked = tab.as_ref().is_some_and(|(_, locked)| *locked);
    let current_view = tab.as_ref().map(|(view, _)| view.clone());
    let (pid, tid) = (panel_id.to_string(), tab_id.to_string());
    let mut entries: Vec<(usize, MenuEntry<'static, Message>)> = vec![
        (
            0,
            MenuEntry::Action {
                label: "关闭该标签".into(),
                icon: Some("x".into()),
                danger: false,
                disabled: locked,
                on_press: (!locked).then(|| Message::PanelTabClose(pid.clone(), tid.clone())),
                note: None,
                hint: locked.then(|| "已锁定，需先解锁".to_string()),
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
    for kind in available_view_kinds(app) {
        let entry = if current_view.as_deref() == Some(kind.as_str()) {
            MenuEntry::noted(view_label(app, &kind), view_icon(app, &kind), "当前", true, None)
        } else if view_occupied(app, &kind, Some(tab_id)) {
            MenuEntry::noted(view_label(app, &kind), view_icon(app, &kind), "已占用", true, None)
                .hinted("该视图已在其他位置（全局唯一）")
        } else {
            MenuEntry::action_icon(
                view_label(app, &kind),
                view_icon(app, &kind),
                Message::SetTabView { panel_id: pid.clone(), tab_id: tid.clone(), view: kind },
            )
        };
        entries.push((1, entry));
    }
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

/// 空面板右键的视图选择器：本组 = 点击激活已有标签（角标「本组」）；已占用停用；其余添加。
fn view_picker_entries(app: &App, panel_id: &str) -> Vec<(usize, MenuEntry<'static, Message>)> {
    let pid = panel_id.to_string();
    available_view_kinds(app)
        .into_iter()
        .map(|kind| {
            let entry = if let Some(tab_id) = panel_tab_with_view(app, panel_id, &kind) {
                MenuEntry::noted(
                    view_label(app, &kind),
                    view_icon(app, &kind),
                    "本组",
                    false,
                    Some(Message::PanelTab(pid.clone(), tab_id)),
                )
            } else if view_occupied(app, &kind, None) {
                MenuEntry::noted(view_label(app, &kind), view_icon(app, &kind), "已占用", true, None)
                    .hinted("该视图已在其他位置（全局唯一）")
            } else {
                MenuEntry::action_icon(
                    view_label(app, &kind),
                    view_icon(app, &kind),
                    Message::PanelAddView { panel_id: pid.clone(), view: kind },
                )
            };
            (0, entry)
        })
        .collect()
}

/// 面板内持有该视图的标签 id。
fn panel_tab_with_view(app: &App, panel_id: &str, kind: &str) -> Option<String> {
    fn walk(node: &LayoutNode, panel_id: &str, kind: &str) -> Option<String> {
        match node {
            LayoutNode::Panel { id, tabs, .. } if id == panel_id => {
                tabs.iter().find(|t| t.view == kind).map(|t| t.id.clone())
            }
            LayoutNode::Split { children, .. } => {
                children.iter().find_map(|c| walk(c, panel_id, kind))
            }
            _ => None,
        }
    }
    walk(&layout::active_layout(&app.ui).tree, panel_id, kind)
}

/// 插件声明的菜单条目：形态与内置菜单同一套渲染（动作 / 勾选 / 分隔线 / 子面板 / 只读文字 / 分组头）。
fn plugin_menu_entries(app: &App, pane: usize) -> Vec<(usize, MenuEntry<'static, Message>)> {
    let Some((_, params)) = app.plugin_menu.as_ref() else { return Vec::new() };
    plugin_entries(params, &app.plugin_menu_query, pane)
}

/// 声明了 `search` 时在当前面板顶部加搜索行，并按输入过滤（匹配规则见 `ui::menu::filter_entries`）。
/// 选中经 `HostRequest` 对应的回传通道交回发起插件。
fn plugin_entries(
    params: &atelyx_core::plugin_runtime::MenuOpenParams,
    query: &str,
    pane: usize,
) -> Vec<(usize, MenuEntry<'static, Message>)> {
    use atelyx_core::plugin_runtime::MenuItem;

    let mut entries: Vec<(usize, MenuEntry<'static, Message>)> = params
        .items
        .iter()
        .map(|item| {
            let entry = match item {
                MenuItem::Action { id, label, icon, danger, disabled, note, hint, .. } => MenuEntry::Action {
                    label: label.clone(),
                    icon: icon.clone(),
                    danger: *danger,
                    disabled: *disabled,
                    on_press: Some(Message::PluginMenuClosed { selected: Some(id.clone()) }),
                    note: note.as_deref().map(leak_note),
                    hint: hint.clone(),
                },
                MenuItem::Checked { id, label, .. } => MenuEntry::Checked {
                    label: label.clone(),
                    on_press: Message::PluginMenuClosed { selected: Some(id.clone()) },
                },
                MenuItem::Submenu { label, icon, disabled, target, .. } => MenuEntry::Submenu {
                    label: label.clone(),
                    icon: icon.clone(),
                    disabled: *disabled,
                    pane: *target,
                },
                MenuItem::Text { text, .. } => MenuEntry::Custom(
                    container(
                        iced::widget::text(text.clone())
                            .size(pal::fs_caption())
                            .color(pal::TEXT_SECONDARY),
                    )
                    .width(Length::Fill)
                    .padding(Padding::ZERO.horizontal(10.0).vertical(2.0))
                    .into(),
                    18.0,
                ),
                MenuItem::Group { label, .. } => MenuEntry::Group(label.clone()),
                MenuItem::Divider { .. } => MenuEntry::Divider,
            };
            (item.pane(), entry)
        })
        .collect();
    let Some(search) = params.search.as_ref() else { return entries };
    entries = crate::ui::menu::filter_entries(entries, query);
    // 空态按当前面板判定：过滤后该面板没有存活条目就提示
    let has_result = entries.iter().any(|(entry_pane, _)| *entry_pane == pane);
    if !has_result && !query.trim().is_empty() {
        if let Some(empty) = search.empty.as_deref() {
            entries.push((pane, MenuEntry::Custom(popup_hint(empty), 18.0)));
        }
    }
    entries.insert(0, (pane, MenuEntry::Divider));
    entries.insert(
        0,
        (
            pane,
            MenuEntry::Custom(
                popup_text_input(&search.placeholder, query, Message::PluginMenuQuery),
                MENU_SEARCH_H,
            ),
        ),
    );
    entries
}

/// 菜单角标是 `&'static str`（内置条目都用字面量）；插件给的是运行时字符串，泄漏一处换取同一形态。
fn leak_note(note: &str) -> &'static str {
    Box::leak(note.to_string().into_boxed_str())
}

// ===== 协作空间（文件面板行 + 弹层表单）=====

/// 打开新弹层（同窗口二段导航：菜单 → 表单）。
fn space_sub_popup(window: iced::window::Id, app: &App, state: PopupState) -> Message {
    Message::PopupOpen(
        window,
        PopupOpen {
            point: app.cursor_for(window),
            flip_y: None,
            state,
            min_width: 0.0,
        },
    )
}

/// 协作空间菜单：身份概览 / 登录（切换账号）/ 创建 / 邀请码加入。
/// 账号下的空间直接列在文件面板空间区（登录即见），不在弹层中转。
fn space_menu_entries(app: &App, window: iced::window::Id) -> Vec<(usize, MenuEntry<'static, Message>)> {
    let mut entries: Vec<(usize, MenuEntry<Message>)> = Vec::new();
    if let Some(identity) = &app.global.last_identity {
        entries.push((
            0,
            MenuEntry::Custom(
                space_hint(&format!("{} @ {}", identity.display_name, identity.server_url)),
                18.0,
            ),
        ));
    }
    entries.push((
        0,
        MenuEntry::action_icon(
            if app.global.last_identity.is_some() { "登录其他账号" } else { "登录协作服务器" },
            "log-in",
            space_sub_popup(window, app, PopupState::SpaceLogin { register: false }),
        ),
    ));
    if app.global.last_identity.is_some() {
        entries.push((0, MenuEntry::action_icon("创建空间", "plus", space_sub_popup(window, app, PopupState::SpaceCreate))));
        entries.push((0, MenuEntry::action_icon("邀请码加入", "user-plus", space_sub_popup(window, app, PopupState::SpaceJoin))));
    }
    // 已在空间内：成员管理对全部成员可见，邀请码只给所有者（与服务端权限口径一致）
    if let crate::RepoKind::Space(session) = &app.repo_kind {
        let owner = session.role == atelyx_core::space_client::SpaceRole::Owner;
        entries.push((0, MenuEntry::Divider));
        entries.push((0, MenuEntry::action_icon(
            "成员管理",
            "users",
            space_sub_popup(window, app, PopupState::SpaceMembers),
        )));
        entries.push((
            0,
            MenuEntry::Action {
                label: "邀请码".into(),
                icon: Some("user-plus".into()),
                danger: false,
                disabled: !owner,
                on_press: owner.then(|| space_sub_popup(window, app, PopupState::SpaceInvite)),
                note: None,
                hint: (!owner).then(|| "邀请码仅空间所有者可管理".to_string()),
            },
        ));
    }
    entries
}

/// 协作登录 / 注册表单（地址 + 账号 + 密码 + 设备名）。
fn space_login_entries(
    app: &App,
    register: bool,
) -> Vec<(usize, MenuEntry<'static, Message>)> {
    let form = &app.space_form;
    let field = |f: crate::SpaceField| move |v: String| Message::Space(crate::SpaceMsg::LoginField(f, v));
    let mut entries: Vec<(usize, MenuEntry<Message>)> = vec![
        (
            0,
            MenuEntry::Custom(
                space_form_input("服务器地址（如 192.168.1.5:11224）", &form.server.clone(), field(crate::SpaceField::Server), false),
                30.0,
            ),
        ),
        (
            0,
            MenuEntry::Custom(
                space_form_input("用户名", &form.username.clone(), field(crate::SpaceField::Username), false),
                30.0,
            ),
        ),
        (
            0,
            MenuEntry::Custom(
                space_form_input("密码", &form.password.clone(), field(crate::SpaceField::Password), true),
                30.0,
            ),
        ),
        (
            0,
            MenuEntry::Custom(
                space_form_input("设备名（可选）", &form.device.clone(), field(crate::SpaceField::Device), false),
                30.0,
            ),
        ),
        (
            0,
            MenuEntry::Custom(
                space_action_row(
                    if register { "注册并登录" } else { "登录" },
                    app.space_busy,
                    Message::Space(crate::SpaceMsg::LoginSubmit { register }),
                ),
                30.0,
            ),
        ),
    ];
    let mode = if register {
        ("已有账号？去登录", false)
    } else {
        ("没有账号？去注册", true)
    };
    entries.push((0, MenuEntry::action(mode.0, Message::SpaceLoginToggle(mode.1))));
    entries.push((0, MenuEntry::Custom(space_hint("令牌保存在系统密钥链，不落明文"), 18.0)));
    entries
}

/// 弹层表单输入行（占位文案 + 字段映射；secure = 掩码显示，用于密码）。
fn space_form_input<'a>(
    placeholder: &str,
    draft: &str,
    on_input: impl Fn(String) -> Message + 'a,
    secure: bool,
) -> Element<'a, Message> {
    let input = iced::widget::text_input(placeholder, draft)
        .size(pal::fs_caption())
        .font(crate::ui::regular())
        .padding(Padding::ZERO.horizontal(12.0).vertical(6.0))
        .on_input(on_input)
        .style(|_theme, status| controls::form_input_style(status));
    if secure { input.secure(true).into() } else { input.into() }
}

/// 弹层表单的说明行。
fn space_hint(text_content: &str) -> Element<'static, Message> {
    container(text(text_content.to_string()).size(pal::fs_micro()).color(pal::TEXT_MUTED))
        .padding(Padding::ZERO.horizontal(12.0))
        .width(Length::Fill)
        .into()
}

/// 弹层表单的提交行（按钮全宽；在途禁用）。
fn space_action_row(
    label: &str,
    busy: bool,
    on_press: Message,
) -> Element<'static, Message> {
    container(
        controls::btn(
            label,
            &controls::btn_sm(),
            controls::Variant::Secondary,
            (!busy).then_some(on_press),
        )
        .width(Length::Fill),
    )
    .padding(Padding::ZERO.horizontal(12.0))
    .width(Length::Fill)
    .into()
}

/// 弹层红字错误条（数据加载失败的非静默呈现）。
fn popup_error_bar(message: &str) -> Element<'static, Message> {
    container(
        row![
            icon("info", 12.0, pal::DANGER),
            text(message.to_string()).size(pal::fs_micro()).color(pal::DANGER),
        ]
        .spacing(6),
    )
    .padding(Padding::ZERO.horizontal(12.0).vertical(3.0))
    .width(Length::Fill)
    .into()
}

/// 角色徽标（所有者 / 编辑者 / 只读）。
fn role_badge(role: atelyx_core::space_client::SpaceRole) -> Element<'static, Message> {
    use atelyx_core::space_client::SpaceRole;
    let (label, color) = match role {
        SpaceRole::Owner => ("所有者", pal::ACCENT),
        SpaceRole::Editor => ("编辑者", pal::TEXT_SECONDARY),
        SpaceRole::Viewer => ("只读", pal::TEXT_MUTED),
    };
    mini_badge(label, color)
}

/// 小徽标（淡底圆角 + 着色文字）。
fn mini_badge(label: &str, color: iced::Color) -> Element<'static, Message> {
    container(text(label.to_string()).size(pal::fs_micro()).color(color))
        .padding(Padding::ZERO.horizontal(6.0).vertical(1.0))
        .style(move |_t| Style {
            background: Some(color.scale_alpha(0.14).into()),
            border: border::rounded(pal::RADIUS_XS),
            ..Style::default()
        })
        .into()
}

/// 成员管理弹层：名册行（显示名 + @username + 角色徽标）+ owner 的行内二段确认 + 错误条。
/// 管理操作仅 owner 可见（服务端同口径）；owner 行无操作。
fn space_members_entries(
    app: &App,
) -> Vec<(usize, MenuEntry<'static, Message>)> {
    use atelyx_core::space_client::SpaceRole;
    let mut entries: Vec<(usize, MenuEntry<Message>)> = Vec::new();
    if let Some(err) = &app.space_members_error {
        entries.push((0, MenuEntry::Custom(popup_error_bar(err), 22.0)));
    } else if app.space_members.is_none() {
        entries.push((0, MenuEntry::Custom(space_hint(if app.space_members_loading { "加载中…" } else { "暂无名册" }), 18.0)));
    }
    let owner = matches!(
        &app.repo_kind,
        crate::RepoKind::Space(s) if s.role == SpaceRole::Owner
    );
    for m in app.space_members.iter().flatten() {
        // 行内二段确认态：整行替换为问题 + 确认/取消
        if let Some(op) = app
            .space_member_confirm
            .as_ref()
            .filter(|(uid, _)| uid == &m.user_id)
            .map(|(_, op)| *op)
        {
            let (question, ok_label, variant, ok_msg) = match op {
                crate::space_actions::MemberConfirm::Transfer => (
                    "转让管理权给该成员？",
                    "确认转让",
                    Variant::Secondary,
                    Message::Space(crate::SpaceMsg::MemberTransfer(m.user_id.clone())),
                ),
                crate::space_actions::MemberConfirm::Remove => (
                    "将该成员移出空间？",
                    "确认移除",
                    Variant::Danger,
                    Message::Space(crate::SpaceMsg::MemberRemove(m.user_id.clone())),
                ),
            };
            entries.push((
                0,
                MenuEntry::Custom(
                    container(
                        row![
                            text(question).size(pal::fs_caption()).color(pal::TEXT_SECONDARY),
                            space().width(Length::Fill),
                            controls::btn(ok_label, &controls::btn_sm(), variant, Some(ok_msg)),
                            controls::btn(
                                "取消",
                                &controls::btn_sm(),
                                Variant::Ghost,
                                Some(Message::Space(crate::SpaceMsg::MemberAskCancel)),
                            ),
                        ]
                        .spacing(6),
                    )
                    .padding(Padding::ZERO.horizontal(12.0))
                    .width(Length::Fill)
                    .height(Length::Fixed(pal::MENU_ITEM_H))
                    .align_y(Vertical::Center)
                    .into(),
                    pal::MENU_ITEM_H,
                ),
            ));
            continue;
        }
        let mut row = row![
            text(m.display_name.clone()).size(pal::fs_caption()).color(pal::TEXT_PRIMARY),
            text(format!("@{}", m.username)).size(pal::fs_micro()).color(pal::TEXT_MUTED),
            role_badge(m.role),
            space().width(Length::Fill),
        ]
        .spacing(6);
        if owner && m.role != SpaceRole::Owner {
            let uid = m.user_id.clone();
            row = row
                .push(controls::btn(
                    "转让",
                    &controls::btn_sm(),
                    Variant::Secondary,
                    Some(Message::Space(crate::SpaceMsg::MemberAsk(
                        uid.clone(),
                        crate::space_actions::MemberConfirm::Transfer,
                    ))),
                ))
                .push(controls::btn(
                    "移除",
                    &controls::btn_sm(),
                    Variant::Danger,
                    Some(Message::Space(crate::SpaceMsg::MemberAsk(
                        uid,
                        crate::space_actions::MemberConfirm::Remove,
                    ))),
                ));
        }
        entries.push((
            0,
            MenuEntry::Custom(
                container(row)
                    .padding(Padding::ZERO.horizontal(12.0))
                    .width(Length::Fill)
                    .height(Length::Fixed(pal::MENU_ITEM_H))
                    .align_y(Vertical::Center)
                    .into(),
                pal::MENU_ITEM_H,
            ),
        ));
    }
    entries
}

/// 邀请码元信息行：{角色} · 有效期 · 次数。
fn invite_meta_line(info: &atelyx_core::space_client::InviteInfo) -> String {
    use atelyx_core::space_client::SpaceRole;
    let role = match info.role {
        SpaceRole::Owner => "所有者",
        SpaceRole::Editor => "编辑者",
        SpaceRole::Viewer => "只读",
    };
    let expires = match info.expires_at {
        None => "永久有效".to_string(),
        Some(secs) => format!("过期于 {}", format_ts(secs * 1000)),
    };
    let uses = match info.max_uses {
        None => "不限次数".to_string(),
        Some(n) => format!("限用 {n} 次"),
    };
    format!("{role} · {expires} · {uses}")
}

/// 邀请码红字撤销入口 / 二段确认行。
fn invite_revoke_row(code: String, confirmed: bool) -> Element<'static, Message> {
    if confirmed {
        return container(
            row![
                text("撤销后该邀请码立即失效").size(pal::fs_micro()).color(pal::TEXT_SECONDARY),
                space().width(Length::Fill),
                controls::btn(
                    "确认撤销",
                    &controls::btn_sm(),
                    Variant::Danger,
                    Some(Message::Space(crate::SpaceMsg::RevokeInvite(code))),
                ),
                controls::btn(
                    "取消",
                    &controls::btn_sm(),
                    Variant::Ghost,
                    Some(Message::Space(crate::SpaceMsg::InviteRevokeCancel)),
                ),
            ]
            .spacing(6),
        )
        .padding(Padding::ZERO.horizontal(12.0).vertical(2.0))
        .width(Length::Fill)
        .into();
    }
    container(
        button(
            text("撤销该邀请码").size(pal::fs_micro()).color(pal::DANGER),
        )
        .padding(Padding::ZERO.horizontal(6.0).vertical(2.0))
        .style(|_t, status| button::Style {
            background: matches!(status, button::Status::Hovered | button::Status::Pressed)
                .then_some(pal::DANGER.scale_alpha(0.12).into()),
            border: border::rounded(pal::RADIUS_XS),
            ..Default::default()
        })
        .on_press(Message::Space(crate::SpaceMsg::InviteRevokeAsk(code))),
    )
    .padding(Padding::ZERO.horizontal(12.0).vertical(2.0))
    .into()
}

/// 邀请码弹层：表单（角色 / 过期小时 / 次数）+ 最近生成结果块 + 已有邀请码列表 + 错误条。
fn space_invite_entries(
    app: &App,
) -> Vec<(usize, MenuEntry<'static, Message>)> {
    use atelyx_core::space_client::SpaceRole;
    let mut entries: Vec<(usize, MenuEntry<Message>)> = vec![
        (
            0,
            MenuEntry::Submenu {
                label: format!("角色：{}", match app.space_invite_form.role {
                    SpaceRole::Owner | SpaceRole::Editor => "编辑者",
                    SpaceRole::Viewer => "只读",
                }),
                icon: None,
                disabled: false,
                pane: 1,
            },
        ),
        (
            0,
            MenuEntry::Custom(
                space_form_input(
                    "过期小时（空 = 永久有效）",
                    &app.space_invite_form.hours.clone(),
                    |v| Message::Space(crate::SpaceMsg::LoginField(crate::SpaceField::InviteHours, v)),
                    false,
                ),
                30.0,
            ),
        ),
        (
            0,
            MenuEntry::Custom(
                space_form_input(
                    "次数上限（空 = 不限）",
                    &app.space_invite_form.uses.clone(),
                    |v| Message::Space(crate::SpaceMsg::LoginField(crate::SpaceField::InviteUses, v)),
                    false,
                ),
                30.0,
            ),
        ),
        (
            0,
            MenuEntry::Custom(
                space_action_row(
                    if app.space_invite_result.is_some() { "重新生成" } else { "生成邀请码" },
                    app.space_busy,
                    Message::Space(crate::SpaceMsg::CreateInvite),
                ),
                30.0,
            ),
        ),
    ];

    // 最近生成结果块：等宽 code + 复制（点击转「已复制」）+ 元信息 + 红字撤销
    if let Some(info) = &app.space_invite_result {
        let copy_label = if app.space_invite_copied { "已复制" } else { "复制" };
        entries.push((
            0,
            MenuEntry::Custom(
                column![
                    container(
                        row![
                            text(info.code.clone()).size(pal::fs_body()).color(pal::ACCENT).font(mono()),
                            space().width(Length::Fill),
                            controls::btn(
                                copy_label,
                                &controls::btn_sm(),
                                Variant::Secondary,
                                Some(Message::Space(crate::SpaceMsg::InviteCopy(info.code.clone()))),
                            ),
                        ]
                        .spacing(8),
                    )
                    .padding(Padding::ZERO.horizontal(12.0))
                    .width(Length::Fill),
                    container(
                        text(invite_meta_line(info)).size(pal::fs_micro()).color(pal::TEXT_MUTED),
                    )
                    .padding(Padding::ZERO.horizontal(12.0))
                    .width(Length::Fill),
                    invite_revoke_row(info.code.clone(), app.space_invite_confirm.as_deref() == Some(info.code.as_str())),
                ]
                .spacing(4)
                .padding(Padding::ZERO.vertical(4.0))
                .width(Length::Fill)
                .into(),
                92.0,
            ),
        ));
    }

    // 已有邀请码（owner 端点列出；结果块中的那一条不重复展示）
    if let Some(list) = &app.space_invites {
        let rows: Vec<_> = list
            .iter()
            .filter(|i| app.space_invite_result.as_ref().is_none_or(|r| r.code != i.code))
            .collect();
        if !rows.is_empty() {
            entries.push((0, MenuEntry::Group("已有邀请码".into())));
            for info in rows {
                let confirmed = app.space_invite_confirm.as_deref() == Some(info.code.as_str());
                entries.push((
                    0,
                    MenuEntry::Custom(
                        if confirmed {
                            container(
                                row![
                                    text("撤销该邀请码？").size(pal::fs_caption()).color(pal::TEXT_SECONDARY),
                                    space().width(Length::Fill),
                                    controls::btn(
                                        "确认撤销",
                                        &controls::btn_sm(),
                                        Variant::Danger,
                                        Some(Message::Space(crate::SpaceMsg::RevokeInvite(info.code.clone()))),
                                    ),
                                    controls::btn(
                                        "取消",
                                        &controls::btn_sm(),
                                        Variant::Ghost,
                                        Some(Message::Space(crate::SpaceMsg::InviteRevokeCancel)),
                                    ),
                                ]
                                .spacing(6),
                            )
                            .padding(Padding::ZERO.horizontal(12.0))
                            .width(Length::Fill)
                            .height(Length::Fixed(pal::MENU_ITEM_H))
                            .align_y(Vertical::Center)
                            .into()
                        } else {
                            container(
                                row![
                                    text(info.code.clone()).size(pal::fs_micro()).color(pal::TEXT_PRIMARY).font(mono()),
                                    text(invite_meta_line(info)).size(pal::fs_micro()).color(pal::TEXT_MUTED),
                                    space().width(Length::Fill),
                                    button(
                                        text("撤销").size(pal::fs_micro()).color(pal::DANGER),
                                    )
                                    .padding(Padding::ZERO.horizontal(6.0).vertical(2.0))
                                    .style(|_t, status| button::Style {
                                        background: matches!(status, button::Status::Hovered | button::Status::Pressed)
                                            .then_some(pal::DANGER.scale_alpha(0.12).into()),
                                        border: border::rounded(pal::RADIUS_XS),
                                        ..Default::default()
                                    })
                                    .on_press(Message::Space(crate::SpaceMsg::InviteRevokeAsk(info.code.clone()))),
                                ]
                                .spacing(8),
                            )
                            .padding(Padding::ZERO.horizontal(12.0))
                            .width(Length::Fill)
                            .height(Length::Fixed(pal::MENU_ITEM_H))
                            .align_y(Vertical::Center)
                            .into()
                        },
                        pal::MENU_ITEM_H,
                    ),
                ));
            }
        }
    }

    if let Some(err) = &app.space_invites_error {
        entries.push((0, MenuEntry::Custom(popup_error_bar(err), 22.0)));
    } else if app.space_invites_loading && app.space_invites.is_none() {
        entries.push((0, MenuEntry::Custom(space_hint("加载中…"), 18.0)));
    }

    // pane 1 = 角色下拉（服务端仅接受 editor / viewer 两档）
    for (role, label) in [(SpaceRole::Editor, "编辑者"), (SpaceRole::Viewer, "只读")] {
        let pick = Message::Space(crate::SpaceMsg::InviteRolePick(role));
        entries.push((
            1,
            if app.space_invite_form.role == role {
                MenuEntry::Checked { label: label.into(), on_press: pick }
            } else {
                MenuEntry::action(label, pick)
            },
        ));
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
#[derive(Clone)]
pub struct PanelZone {
    /// 主窗口树面板 id（撕裂窗口为 None）。
    pub panel_id: Option<String>,
    /// 撕裂窗口条目 id（主窗口面板为 None）。
    pub detached_id: Option<String>,
    /// 面板客户区矩形（含标签条）。
    pub rect: Rectangle,
    /// 各标签宽度（自适应标签的命中几何基准，与渲染同源估算）。
    pub tab_widths: Vec<f32>,
}

/// 拖拽 drop 的落点种类（标签条排序 / 中部加标签 / 四边缘分割）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropZone {
    Tab,
    Center,
    Left,
    Right,
    Top,
    Bottom,
}

/// 窗口当前活动视图是否为指定类型（主窗扫描活动布局树；撕裂窗查其标签）。
fn window_shows_view(app: &App, window: iced::window::Id, view: &str) -> bool {
    let active_is = |tabs: &[atelyx_core::layout::TabItem], active: Option<&str>| {
        active
            .and_then(|tid| tabs.iter().find(|t| t.id == tid))
            .is_some_and(|t| t.view == view)
    };
    if app.main_id == Some(window) {
        fn scan(node: &atelyx_core::layout::LayoutNode, view: &str) -> bool {
            match node {
                atelyx_core::layout::LayoutNode::Panel { tabs, active_tab_id, .. } => {
                    active_tab_id
                        .as_deref()
                        .and_then(|tid| tabs.iter().find(|t| t.id == tid))
                        .is_some_and(|t| t.view == view)
                }
                atelyx_core::layout::LayoutNode::Split { children, .. } => {
                    children.iter().any(|n| scan(n, view))
                }
            }
        }
        scan(&atelyx_core::layout::active_layout(&app.ui).tree, view)
    } else {
        app.detached_os
            .get(&window)
            .and_then(|model_id| app.ui.detached_windows.iter().find(|w| &w.id == model_id))
            .is_some_and(|w| active_is(&w.tabs, w.active_tab_id.as_deref()))
    }
}


/// 窗口当前活动视图是否为画布（画布键盘命令的作用域判定用）。
pub fn window_shows_canvas(app: &App, window: iced::window::Id) -> bool {
    window_shows_view(app, window, atelyx_core::layout::CANVAS_VIEW)
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
            walk_panels(&layout::active_layout(&app.ui).tree, area, &mut out, app);
            // 面板矩形向外扩半把手并钳回内容区：拖拽/按压落进面板间 4px 把手间隙时
            // 归属邻近面板，不再按悬停为空撕出新窗
            let half = UiMetrics::DIVIDER_W / 2.0;
            for z in &mut out {
                let r = &mut z.rect;
                r.x = (r.x - half).max(area.x);
                r.y = (r.y - half).max(area.y);
                r.width = (r.width + 2.0 * half).min(area.x + area.width - r.x);
                r.height = (r.height + 2.0 * half).min(area.y + area.height - r.y);
            }
        }
    }
    // 撕裂窗口落点矩形（标题栏不含分隔线，无 1px 偏差）
    for (os, mid) in &app.detached_os {
        if let (Some(pos), Some(sz)) = (app.os_positions.get(os), app.os_sizes.get(os)) {
            let widths = app
                .ui
                .detached_windows
                .iter()
                .find(|w| &w.id == mid)
                .map(|w| {
                    w.tabs
                        .iter()
                        .map(|t| {
                            approx_tab_width(
                                &tab_label(app, &t.view),
                                t.locked,
                                (is_note_view(&t.view) && app.ui.last_note_file.is_some())
                                    || (t.view == "table" && app.ui.last_table_file.is_some())
                                    || (is_canvas_view(&t.view) && app.ui.last_canvas_file.is_some()),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            out.push(PanelZone {
                panel_id: None,
                detached_id: Some(mid.clone()),
                rect: Rectangle {
                    x: pos.x,
                    y: pos.y + UiMetrics::TITLEBAR_H,
                    width: sz.width,
                    height: (sz.height - UiMetrics::TITLEBAR_H).max(0.0),
                },
                tab_widths: widths,
            });
        }
    }
    out
}

/// 指定窗口内全局坐标所在面板的活动视图 kind（落在面板外 = `None`）。
/// 主窗与撕裂窗的矩形可能重叠，因此必须按窗口筛掉别的窗口的面板。
pub fn view_at(app: &App, window: iced::window::Id, point: Point) -> Option<String> {
    let zone = panel_zones(app)
        .into_iter()
        .find(|zone| zone.rect.contains(point) && zone_window(app, zone) == Some(window))?;
    match (&zone.panel_id, &zone.detached_id) {
        (Some(panel_id), _) => active_view_in(&layout::active_layout(&app.ui).tree, panel_id),
        (_, Some(detached_id)) => app
            .ui
            .detached_windows
            .iter()
            .find(|w| &w.id == detached_id)
            .and_then(|w| {
                let active = w.active_tab_id.as_deref()?;
                w.tabs.iter().find(|t| t.id == active)
            })
            .map(|t| t.view.clone()),
        _ => None,
    }
}

/// 落点矩形所属窗口：主窗面板归主窗，撕裂窗面板归该撕裂窗口。
fn zone_window(app: &App, zone: &PanelZone) -> Option<iced::window::Id> {
    match (&zone.panel_id, &zone.detached_id) {
        (Some(_), _) => app.main_id,
        (_, Some(detached_id)) => {
            app.detached_os.iter().find(|(_, id)| *id == detached_id).map(|(os, _)| *os)
        }
        _ => None,
    }
}

/// 主树某面板的活动标签视图 kind（无活动标签 = `None`）。
fn active_view_in(node: &LayoutNode, panel_id: &str) -> Option<String> {
    match node {
        LayoutNode::Panel { id, tabs, active_tab_id } if id == panel_id => {
            let active = active_tab_id.as_deref()?;
            tabs.iter().find(|t| t.id == active).map(|t| t.view.clone())
        }
        LayoutNode::Split { children, .. } => {
            children.iter().find_map(|child| active_view_in(child, panel_id))
        }
        _ => None,
    }
}

fn walk_panels(node: &LayoutNode, area: Rectangle, out: &mut Vec<PanelZone>, app: &App) {
    match node {
        LayoutNode::Panel { id, tabs, .. } => out.push(PanelZone {
            panel_id: Some(id.clone()),
            detached_id: None,
            rect: area,
            tab_widths: tabs
                .iter()
                .map(|t| {
                    approx_tab_width(
                        &tab_label(app, &t.view),
                        t.locked,
                        (is_note_view(&t.view) && app.ui.last_note_file.is_some())
                            || (t.view == "table" && app.ui.last_table_file.is_some())
                            || (is_canvas_view(&t.view) && app.ui.last_canvas_file.is_some()),
                    )
                })
                .collect(),
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
                walk_panels(child, child_area, out, app);
                pos += len + UiMetrics::DIVIDER_W;
            }
        }
    }
}

/// 光标在标签条内的插入索引（按各标签近似宽度 + 行内 2px 间距，半宽细分：
/// 左半 = 该标签前，右半 = 该标签后；`scroll_x` = 该标签条横向滚动偏移，
/// 估宽与真实 shaping 的偏差只影响插入位差半格以内）。
pub fn tab_index_at(zone: &PanelZone, global_x: f32, scroll_x: f32) -> usize {
    let mut x = zone.rect.x + UiMetrics::HEADER_INSET - scroll_x;
    for (i, w) in zone.tab_widths.iter().enumerate() {
        if global_x < x + w / 2.0 {
            return i;
        }
        x += w + 2.0;
    }
    zone.tab_widths.len()
}

/// 光标是否落在标签条内（标签条整行高度，动作按钮区同样算入）。
pub fn in_tab_strip(zone: &PanelZone, global: iced::Point) -> bool {
    let rel_y = global.y - zone.rect.y;
    (0.0..=UiMetrics::TAB_BAR_H).contains(&rel_y)
}

/// 光标落点种类（主窗口面板）：头部 = 标签排序；四边缘 = 12% 分割带（左→右→上→下判定序）；
/// 其余 = 中部加标签。
pub fn drop_zone_at(zone: &PanelZone, global: iced::Point) -> DropZone {
    if in_tab_strip(zone, global) {
        return DropZone::Tab;
    }
    let (w, h) = (zone.rect.width, zone.rect.height);
    if global.x - zone.rect.x < w * 0.12 {
        DropZone::Left
    } else if zone.rect.x + w - global.x < w * 0.12 {
        DropZone::Right
    } else if global.y - zone.rect.y < h * 0.12 {
        DropZone::Top
    } else if zone.rect.y + h - global.y < h * 0.12 {
        DropZone::Bottom
    } else {
        DropZone::Center
    }
}

#[cfg(test)]
mod tests {
    use super::{
        active_view_in, divider_size, file_label, plugin_entries, truncate_to_width, UiMetrics,
        MENU_SEARCH_H,
    };
    use atelyx_core::layout::{LayoutNode, TabItem};
    use atelyx_core::plugin_runtime::{MenuItem, MenuOpenParams, MenuSearch};
    use iced::Length;

    use crate::ui::menu::MenuEntry;

    #[test]
    fn divider_thickness_follows_split_axis() {
        let bar = Length::Fixed(UiMetrics::DIVIDER_W);
        assert_eq!(divider_size(true), (bar, Length::Fill));
        assert_eq!(divider_size(false), (Length::Fill, bar));
    }

    /// 落点面取面板的活动标签：无活动标签的面板不接收拖入。
    #[test]
    fn active_view_follows_the_panel_active_tab() {
        let panel = |id: &str, active: Option<&str>| LayoutNode::Panel {
            id: id.into(),
            tabs: vec![
                TabItem { id: "t1".into(), view: "plugin:canvas:canvas".into(), locked: false },
                TabItem { id: "t2".into(), view: "note".into(), locked: false },
            ],
            active_tab_id: active.map(str::to_string),
        };
        let tree = LayoutNode::Split {
            id: "s1".into(),
            direction: "row".into(),
            children: vec![panel("p1", Some("t2")), panel("p2", None)],
            sizes: vec![1.0, 1.0],
        };
        assert_eq!(active_view_in(&tree, "p1").as_deref(), Some("note"));
        assert_eq!(active_view_in(&tree, "p2"), None);
        assert_eq!(active_view_in(&tree, "missing"), None);
    }

    /// 固定度量：每字符 1 单位（截断只按字符数收缩，与字体 shaping 解耦）。
    fn per_char(s: &str) -> f32 {
        s.chars().count() as f32
    }

    #[test]
    fn title_label_takes_file_stem() {
        assert_eq!(file_label("a/b/笔记.md"), "笔记");
        assert_eq!(file_label("笔记.md"), "笔记");
        assert_eq!(file_label("a/b/v1.2.md"), "v1.2");
        // 各类文件一律去扩展名（重命名输入框与标题都只给主名）
        assert_eq!(file_label("a/表.atb"), "表");
        assert_eq!(file_label("a/画布.atlx"), "画布");
        // 无扩展名与隐藏名按整名
        assert_eq!(file_label("readme"), "readme");
        assert_eq!(file_label("a/.env"), ".env");
    }

    #[test]
    fn keeps_title_that_fits() {
        assert_eq!(truncate_to_width("abc", 3.0, per_char), "abc");
        assert_eq!(truncate_to_width("", 0.0, per_char), "");
    }

    #[test]
    fn clips_and_appends_ellipsis() {
        // 宽 4：保留 3 字 + 省略号（省略号自身占 1 单位）
        assert_eq!(truncate_to_width("abcdef", 4.0, per_char), "abc…");
    }

    #[test]
    fn clips_to_ellipsis_only_when_no_room() {
        assert_eq!(truncate_to_width("abcdef", 1.0, per_char), "…");
        assert_eq!(truncate_to_width("abcdef", 0.0, per_char), "…");
    }

    #[test]
    fn clips_at_char_boundary() {
        assert_eq!(truncate_to_width("中文测试", 3.0, per_char), "中文…");
    }

    /// 字体菜单的条目形状：默认项 + 系统字体族，全部是勾选行。
    fn font_menu(search: Option<MenuSearch>) -> MenuOpenParams {
        let mut items = vec![MenuItem::Checked {
            id: "style:font:".into(),
            label: "默认".into(),
            pane: 0,
        }];
        for label in ["Sans Serif", "Sarasa Mono SC", "Segoe UI"] {
            items.push(MenuItem::Checked {
                id: format!("style:font:{label}"),
                label: label.into(),
                pane: 0,
            });
        }
        MenuOpenParams { items, width: None, search }
    }

    fn font_search() -> MenuSearch {
        MenuSearch { placeholder: "搜索字体".into(), empty: Some("没有匹配的字体".into()) }
    }

    fn labels<'a>(entries: &'a [(usize, MenuEntry<'static, crate::Message>)]) -> Vec<&'a str> {
        entries
            .iter()
            .filter_map(|(_, entry)| match entry {
                MenuEntry::Action { label, .. }
                | MenuEntry::Checked { label, .. }
                | MenuEntry::Submenu { label, .. } => Some(label.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn font_menu_search_keeps_only_trimmed_case_folded_matches() {
        let params = font_menu(Some(font_search()));
        for query in ["sarasa", "SARASA", "  sarasa  "] {
            let entries = plugin_entries(&params, query, 0);
            assert_eq!(labels(&entries), ["Sarasa Mono SC"], "查询「{query}」应只留下命中的字体");
            // 顶部是先插入的搜索行，随后一条分隔线，然后才是命中项
            let MenuEntry::Custom(_, height) = &entries[0].1 else {
                panic!("搜索行应是最顶部的自定义行")
            };
            assert!((*height - MENU_SEARCH_H).abs() < f32::EPSILON);
            assert!(matches!(&entries[1].1, MenuEntry::Divider));
            assert_eq!(entries.len(), 3);
        }
    }

    #[test]
    fn font_menu_search_shows_empty_hint_when_nothing_matches() {
        let params = font_menu(Some(font_search()));
        let entries = plugin_entries(&params, "不存在的字体", 0);
        assert!(labels(&entries).is_empty(), "无匹配时不应留下条目");
        assert_eq!(entries.len(), 3);
        let MenuEntry::Custom(_, height) = &entries[2].1 else {
            panic!("无匹配时应在搜索行与分隔线之后补空态行")
        };
        assert!((*height - 18.0).abs() < f32::EPSILON);
    }

    #[test]
    fn menu_without_search_keeps_items_regardless_of_query() {
        let params = font_menu(None);
        let entries = plugin_entries(&params, "sarasa", 0);
        assert_eq!(
            labels(&entries),
            ["默认", "Sans Serif", "Sarasa Mono SC", "Segoe UI"],
            "未声明 search 的旧调用方不插入搜索行、不按输入过滤"
        );
        assert!(matches!(&entries[0].1, MenuEntry::Checked { .. }));
        assert_eq!(entries.len(), 4);
    }
}
