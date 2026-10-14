//! 工作区 UI：标题栏（布局 tab / 场景选择器 / 窗口控制）+ 面板网格 + 状态栏
//! + 设置整页 + 弹层内容（菜单/选择器/就地确认）+ files/note 视图与占位视图。
//!
//! 几何计算与渲染共用同一套度量（UiMetrics）：把手/落点命中矩形由布局树 +
//! 窗口内容区尺寸纯推导，与 iced FillPortion 的分配规则保持一致。

use std::collections::HashSet;
use std::path::PathBuf;

use iced::alignment::{Horizontal, Vertical};
use iced::widget::{button, column, container, mouse_area, row, scrollable, space, stack, text, text_editor, text_input};
use iced::{Color, Element, Font, Length, Padding, Point, Rectangle, Size, border, widget::container::Style};

use atelyx_core::global_config::RecentVault;
use atelyx_core::layout::{self, AppUiState, LayoutNode, TabItem};
use atelyx_core::plugin_runtime::ContributionSlot;

use crate::settings_view::ai;
use crate::ui::controls::{self, Variant};
use crate::ui::icon::icon;
use crate::ui::menu::MenuEntry;
use crate::ui::palette as pal;
use crate::ui::popup::{self, PopupSpec};
use crate::ui::notify;
use crate::{App, DraftKind, Message, NoteMode, markdown_edit, markdown_view};

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

/// 内置视图 kind 顺序（右键菜单/选择器的呈现序）。
pub const VIEW_KINDS: &[&str] = &[
    "canvas", "note", "table", "files", "search", "inspector", "aichat", "collabroom",
    "calendar", "repohistory", "recent",
];

/// 内置视图显示名（面板头标签）；插件面板取贡献 label，贡献缺失（未装 / 停用）回落插件 id。
pub fn view_label(app: &App, kind: &str) -> String {
    let builtin = match kind {
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
        _ => "",
    };
    if !builtin.is_empty() {
        return builtin.to_string();
    }
    if let Some((plugin_id, id)) = crate::plugin_actions::parse_view_kind(kind) {
        return crate::plugin_actions::contribution(app, plugin_id, id)
            .map(|c| c.label.clone())
            .unwrap_or_else(|| plugin_id.to_string());
    }
    kind.to_string()
}

/// 内置视图图标（lucide 名）；插件面板取贡献 icon（缺省拼图）；未知 kind 拼图。
pub fn view_icon(app: &App, kind: &str) -> &'static str {
    let builtin = match kind {
        "canvas" => Some("palette"),
        "note" => Some("file-text"),
        "table" => Some("table"),
        "files" => Some("files"),
        "search" => Some("search"),
        "inspector" => Some("info"),
        "aichat" => Some("sparkles"),
        "collabroom" => Some("users"),
        "calendar" => Some("calendar-days"),
        "repohistory" => Some("history"),
        "recent" => Some("clock"),
        "empty" => Some("layout-template"),
        _ => None,
    };
    if let Some(name) = builtin {
        return name;
    }
    if let Some((plugin_id, id)) = crate::plugin_actions::parse_view_kind(kind) {
        return crate::plugin_actions::contribution_icon(app, plugin_id, id);
    }
    "puzzle"
}

/// 可选视图 kind 清单：内置视图 + 已注册的插件面板贡献（壳侧命名空间化 kind）。
pub fn available_view_kinds(app: &App) -> Vec<String> {
    let mut kinds: Vec<String> = VIEW_KINDS.iter().map(|kind| (*kind).to_string()).collect();
    kinds.extend(
        crate::plugin_actions::contributions_of(app, ContributionSlot::Panel)
            .map(|c| crate::plugin_actions::view_kind(&c.plugin_id, &c.id)),
    );
    kinds
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
/// 渲染宽度全部由内容自适应（Shrink），本估算仅服务于拖拽命中几何的插入位判定
/// （标题栏 tab 条起点/宽度、面板标签条插入索引）；与真实 shaping 存在逐字符
/// 累积的估宽偏差，标签越多插入位偏差越大，不影响渲染。
pub fn estimate_text_width(text: &str, font_size: f32) -> f32 {
    text.chars().map(|c| estimate_char_w(c, font_size)).sum()
}

/// 按估宽截断：超出 `max_w` 时逐字收缩并补省略号（截断口径与渲染定宽同源）。
pub fn ellipsize(text: &str, max_w: f32, font_size: f32) -> String {
    if estimate_text_width(text, font_size) <= max_w {
        return text.to_string();
    }
    for (i, _) in text.char_indices().rev() {
        let candidate = format!("{}…", &text[..i]);
        if estimate_text_width(&candidate, font_size) <= max_w {
            return candidate;
        }
    }
    "…".to_string()
}

/// 面板标签文字（笔记/表格视图打开文件 = 文件名去扩展名后缀，否则视图名）。
pub fn tab_label(app: &App, view: &str) -> String {
    let file_label = |f: &str, ext: &str| {
        f.rsplit('/').next().unwrap_or(f).strip_suffix(ext).map(|s| s.to_string()).unwrap_or_else(|| f.to_string())
    };
    if view == "note" {
        if let Some(f) = app.ui.last_note_file.as_deref() {
            return file_label(f, ".md");
        }
    }
    if view == "table" {
        if let Some(f) = app.ui.last_table_file.as_deref() {
            return file_label(f, ".atb");
        }
    }
    view_label(app, view)
}

/// 面板标签近似宽度（拖拽命中几何用；渲染宽度由内容自适应，见 panel_tab）。
fn approx_tab_width(label: &str, locked: bool, has_file: bool) -> f32 {
    let text_w = estimate_text_width(label, 12.0).min(UiMetrics::TAB_TEXT_MAX_W);
    13.0 + 4.0 + text_w + 16.0 + 2.0
        + if locked { 13.0 } else { 0.0 }
        + if has_file && !locked { 19.0 } else { 0.0 }
}

/// 等宽字体（计数/扩展名标签）。
pub(crate) fn mono() -> Font {
    crate::ui::mono()
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
    SortMenu,
    /// 设置-工作区：启动场景选择（场景列表 + 跟随当前）。
    EntrySceneMenu,
    FileMenu { rel: String, is_dir: bool, is_root: bool },
    /// 最近仓库行菜单（root = 目标仓库根）。
    VaultMenu { root: String },
    /// 协作空间菜单（登录入口 / 创建 / 邀请码加入）。
    SpaceMenu,
    /// 协作登录 / 注册表单（register = 注册模式）。
    SpaceLogin { register: bool },
    /// 创建空间（名称输入）。
    SpaceCreate,
    /// 邀请码加入（输入邀请码）。
    SpaceJoin,
    /// 协作空间行菜单（server_url + space_id）。
    SpaceRowMenu { server_url: String, space_id: String },
    /// 成员管理（激活空间；数据取 App.space_members 缓存）。
    SpaceMembers,
    /// 邀请码管理（激活空间且 owner；表单草稿在 App.space_invite_form）。
    SpaceInvite,
    NoteMenu,
    /// 属性面板类型切换菜单（属性行序号）。
    PropTypeMenu { index: usize },
    /// 表格：单元格右键（复制/剪切/粘贴/清空）。
    TableCellMenu,
    /// 表格：列头 ⋮ 字段管理。
    TableFieldMenu { field_id: String },
    /// 表格：字段改名（内联输入）。
    TableFieldRename { field_id: String },
    /// 表格：单选选项管理（多行文本，每行一项）。
    TableOptionsEdit { field_id: String },
    /// 表格：列头右键。
    TableColumnMenu { field_id: String },
    /// 表格：列插入（内联输入；side = -1 左插 / 1 右插）。
    TableColumnInsert { field_id: String, side: i32 },
    /// 表格：行号右键。
    TableRowMenu { row_id: String },
    /// 表格：添加字段（输入 + 类型列表）。
    TableAddField,
    /// 表格：状态栏计算类型。
    TableStatMenu { field_id: String },
    /// 表格：全选角钮右键。
    TableSelectAllMenu,
    /// 表格：··· 菜单（历史 / 导出）。
    TableMoreMenu,
    /// 表格：格式气泡下拉（字体 / 字号 / 字色 / 底色）。
    TableFontMenu,
    TableSizeMenu,
    TableColorMenu { bg: bool },
    /// 表格：单选下拉（行 + 字段）。
    TableSelectMenu { row_id: String, field_id: String },
    /// 表格：灯箱图片右键菜单（下载）。
    TableLightboxMenu,
    /// 表格：工具条视图切换下拉（内置表格）。
    TableViewMenu,
    /// 设置-AI：添加供应商（预设 / 自定义）。
    AiProviderAddMenu,
    /// 设置-AI：模型下拉（for_naming = 话题命名模型，选项不含「不指定」）。
    AiModelMenu { for_naming: bool },
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
    /// 设置-关于：开源许可条目（字体官方主页外链）。
    LicenseInfo,
    /// 历史记录弹层（居中模态；kind+file 定位侧文件）。
    HistoryBrowser { kind: atelyx_core::history::HistoryKind, file: String },
    /// 插件菜单 / 下拉节点的弹层（条目由插件界面树带来，点击回发节点事件）。
    PluginList { entries: Vec<crate::plugin_actions::PopupEntry> },
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
            PopupState::EntrySceneMenu => 176.0,
            // 文件/文件夹菜单 176；最近仓库行菜单 192
            PopupState::FileMenu { .. } => 176.0,
            PopupState::VaultMenu { .. } => 192.0,
            PopupState::SpaceMenu => 200.0,
            PopupState::SpaceLogin { .. } => 340.0,
            PopupState::SpaceCreate => 260.0,
            PopupState::SpaceJoin => 260.0,
            PopupState::SpaceRowMenu { .. } => 176.0,
            // 成员行带 @username 与角色徽标；邀请码表单含两个数字输入
            PopupState::SpaceMembers => 320.0,
            PopupState::SpaceInvite => 320.0,
            PopupState::NoteMenu => 144.0,
            PopupState::PropTypeMenu { .. } => 128.0,
            PopupState::TableCellMenu => 128.0,
            PopupState::TableFieldMenu { .. } => 176.0,
            PopupState::TableFieldRename { .. } => 200.0,
            PopupState::TableOptionsEdit { .. } => 208.0,
            PopupState::TableColumnMenu { .. } => 176.0,
            PopupState::TableColumnInsert { .. } => 200.0,
            PopupState::TableRowMenu { .. } => 144.0,
            PopupState::TableAddField => 192.0,
            PopupState::TableStatMenu { .. } => 128.0,
            PopupState::TableSelectAllMenu => 160.0,
            PopupState::TableMoreMenu => 144.0,
            PopupState::TableFontMenu => 128.0,
            PopupState::TableSizeMenu => 96.0,
            PopupState::TableColorMenu { .. } => 168.0,
            PopupState::TableSelectMenu { .. } => 144.0,
            PopupState::TableLightboxMenu => 128.0,
            PopupState::TableViewMenu => 96.0,
            // AI 设置：模型 id / 笔记名可能较长，给足一行的宽度
            PopupState::AiProviderAddMenu => 168.0,
            PopupState::AiModelMenu { .. } => 224.0,
            PopupState::AiNamingMenu => 144.0,
            PopupState::AiAgentPromptMenu => 208.0,
            PopupState::AiProviderDeleteMenu | PopupState::AiAgentDeleteMenu => 208.0,
            PopupState::AiSearchProviderMenu => 144.0,
            // 居中模态不走锚点几何，估高由各自 overlay 钳制
            PopupState::LicenseInfo => crate::settings_view::LICENSE_CARD_WIDTH,
            PopupState::HistoryBrowser { .. } => crate::history_browser::CARD_WIDTH,
            PopupState::PluginList { .. } => 176.0,
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
fn btn_popup_min(window: iced::window::Id, cursor: Point, state: PopupState, min_width: f32) -> Message {
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
                        icon(view_icon(app, &drag.view), 13.0, pal::TEXT_PRIMARY),
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
                let icon_name = if drag.is_dir { "folder" } else { file_icon(&drag.label) };
                let hint = app.file_drag_target.as_ref().map(|dir| {
                    if dir.is_empty() {
                        "移到根目录".to_string()
                    } else {
                        let name = dir.rsplit('/').next().unwrap_or(dir);
                        format!("移动到「{name}」")
                    }
                });
                let mut card = column![
                    row![
                        icon(icon_name, 13.0, pal::TEXT_PRIMARY),
                        text(drag.label.clone()).size(pal::fs_caption()).color(pal::TEXT_PRIMARY),
                    ]
                    .spacing(6),
                ]
                .spacing(2);
                if let Some(hint) = hint {
                    card = card.push(
                        text(hint).size(pal::fs_micro()).color(pal::TEXT_SECONDARY),
                    );
                }
                let ghost = container(card)
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
            out.push((l.id.clone(), l.name.clone(), layout_tab_width(&l.name)));
        }
    }
    (4.0 + brand_w + 4.0, out)
}

/// 布局 tab 宽度：文字估宽 + 内边距 20，钳制 48–120。
fn layout_tab_width(name: &str) -> f32 {
    (estimate_text_width(name, 12.0) + 20.0).clamp(48.0, 120.0)
}

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
        controls::icon_button("plus", 13.0, 28.0, Variant::SubtleBg,
            Some(Message::LayoutNew), Some((window, "新建布局".into()))),
    ]))
    .direction(scrollable::Direction::Horizontal(scrollable::Scrollbar::new().width(0.0).scroller_width(0.0)))
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
            iced::widget::image(crate::ui::icon_handle())
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
            .font(crate::ui::display())
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
/// 右键 = tab 菜单、双击 = 重命名）。
fn layout_tab(
    app: &App,
    name: &str,
    id: &str,
    active: bool,
    window: iced::window::Id,
    cursor: Point,
) -> Element<'static, Message> {
    let text_color = if active { pal::TEXT_PRIMARY } else { pal::TEXT_SECONDARY };
    // 重命名在途 = 与激活态同款高亮（底色 + 金下边），无论该 tab 是否激活；
    // 输入框需要定宽，仅编辑态退回估宽定宽
    let renaming = matches!(
        &app.rename,
        Some(r) if r.target == RenameTarget::Layout(id.to_string())
    );
    let highlighted = active || renaming;
    let edit_width = layout_tab_width(name);
    let text_area: Element<'static, Message> = if renaming {
        controls::inline_input(
            iced::widget::Id::new(controls::INLINE_RENAME_INPUT),
            "新名称",
            &app.rename.as_ref().map(|r| r.value.clone()).unwrap_or_default(),
            edit_width - 20.0,
            Message::LayoutRenameInput,
            Message::LayoutRenameCommit,
            Message::RenamePressInside,
        )
    } else {
        text(ellipsize(name, UiMetrics::TAB_TEXT_MAX_W, 12.0).to_string())
            .size(pal::fs_caption())
            .color(text_color)
            .into()
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

    if id != layout::HOME_LAYOUT_ID {
        // 按下候选经 PressReporter 上报（主页固定不可拖不挂）；右键/双击照常
        return controls::PressReporter::new(
            mouse_area(
                // 显式零内边距：估宽（layout_tab_width）按内容记账，默认内边距会让命中几何偏窄
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
            .on_double_click(Message::LayoutRenameStart(id.to_string()))
            .on_right_press(ctx_popup(
                window,
                cursor,
                PopupState::LayoutTabMenu { layout_id: id.to_string() },
            ))
            .into(),
            Message::LayoutTabPress {
                layout_id: id.to_string(),
                name: name.to_string(),
            },
        )
        .into();
    }
    mouse_area(
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
    .on_double_click(Message::LayoutRenameStart(id.to_string()))
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

/// 分割把手（宽 4px；悬停 = 强调色 50%）；光标形态 col-resize / row-resize。
fn divider(hovered: bool, horizontal: bool) -> Element<'static, Message> {
    let interaction = if horizontal {
        iced::mouse::Interaction::ResizingColumn
    } else {
        iced::mouse::Interaction::ResizingRow
    };
    mouse_area(
        container(space())
            .width(Length::Fixed(UiMetrics::DIVIDER_W))
            .height(Length::Fill)
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
        if t.view == "note" {
            if let Some(pill) = note_status_pill(app) {
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
                .direction(scrollable::Direction::Horizontal(
                    scrollable::Scrollbar::new().width(0.0).scroller_width(0.0)
                ))
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
    let has_file = (t.view == "note" && app.ui.last_note_file.is_some())
        || (t.view == "table" && app.ui.last_table_file.is_some());
    let window = app.main_id.unwrap_or(iced::window::Id::unique());
    let cursor = app.cursor_for(window);

    let text_color = if active { pal::TEXT_PRIMARY } else { pal::TEXT_SECONDARY };
    let mut label_row = row![].spacing(4);
    label_row = label_row.push(icon(view_icon(app, &t.view), 13.0, text_color));
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
fn note_status_pill(app: &App) -> Option<Element<'static, Message>> {
    app.ui.last_note_file.as_ref()?;
    if app.save_error.is_some() {
        Some(
            container(
                row![
                    text(ellipsize(app.save_error.as_deref().unwrap_or("保存失败"), 160.0, 11.0))
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
    } else if app.dirty {
        Some(controls::status_pill(controls::PillStatus::Pending, "未保存"))
    } else {
        None
    }
}

/// 按视图 kind 分派内容（内置视图按 kind；插件面板按贡献 kind 走 ViewNode 解释渲染）。
fn view_content<'a>(app: &'a App, window: iced::window::Id, kind: &str) -> Element<'a, Message> {
    if crate::plugin_actions::parse_view_kind(kind).is_some() {
        return crate::plugin_view::panel_view(app, window, kind);
    }
    match kind {
        "files" => files_view(app),
        "note" => note_view(app),
        "search" => search_view(app),
        "inspector" => inspector_view(app, window),
        "table" => crate::table_view::table_view(app, window),
        "collabroom" => collab_room_view(app),
        "recent" => crate::recent_view::recent_view(app),
        "repohistory" => crate::repohistory_view::repohistory_view(app, window),
        "calendar" => crate::calendar_view::calendar_view(app, window),
        kind => placeholder_view(app, kind),
    }
}

/// 协作房间视图：连接状态头部 + 空间成员名册 + 在线设备（打开文件可点开）+ 设置入口。
fn collab_room_view(app: &App) -> Element<'_, Message> {
    let mut col = iced::widget::Column::<Message>::new()
        .spacing(6)
        .padding(Padding::ZERO.horizontal(12.0).vertical(10.0));
    match &app.repo_kind {
        crate::RepoKind::Space(_) => {
            // 连接状态头部（帧泵 Ready 置位 / 断开与摘泵清零）+ 设置入口
            let (net_icon, net_label, net_color) = if app.space_connected {
                ("wifi", "已连接", pal::SUCCESS)
            } else {
                ("wifi-off", "未连接", pal::TEXT_MUTED)
            };
            col = col.push(
                container(
                    row![
                        icon(net_icon, 13.0, net_color),
                        text(net_label).size(pal::fs_caption()).color(net_color),
                        space().width(Length::Fill),
                        button(
                            row![
                                icon("settings", 12.0, pal::TEXT_SECONDARY),
                                text("协作设置").size(pal::fs_micro()).color(pal::TEXT_SECONDARY),
                            ]
                            .spacing(4),
                        )
                        .padding(Padding::ZERO.horizontal(8.0).vertical(3.0))
                        .style(|_t, status| button::Style {
                            background: matches!(status, button::Status::Hovered | button::Status::Pressed)
                                .then_some(pal::HOVER.into()),
                            border: border::rounded(pal::RADIUS_SM),
                            ..Default::default()
                        })
                        .on_press(Message::Space(crate::SpaceMsg::OpenCollabSettings)),
                    ]
                    .spacing(6),
                )
                .width(Length::Fill)
                .padding(Padding::ZERO.vertical(2.0)),
            );

            // 空间成员名册（list_members 缓存，展示中由 members_tick 懒装载）
            let roster_count = app.space_members.as_ref().map(Vec::len).unwrap_or(0);
            col = col.push(crate::settings_view::group_head("空间成员", &format!("{roster_count} 人"), false));
            match (&app.space_members_error, app.space_members.as_ref()) {
                (Some(err), _) => {
                    col = col.push(
                        text(format!("成员名册加载失败：{err}"))
                            .size(pal::fs_micro())
                            .color(pal::DANGER),
                    );
                }
                (None, Some(list)) => {
                    for m in list {
                        // 成员卡片：显示名 + 角色徽标一行，@username 次行
                        col = col.push(
                            container(
                                column![
                                    row![
                                        text(m.display_name.clone())
                                            .size(pal::fs_body())
                                            .color(pal::TEXT_PRIMARY)
                                            .font(iced::Font {
                                                weight: iced::font::Weight::Medium,
                                                ..iced::Font::default()
                                            }),
                                        space().width(Length::Fill),
                                        role_badge(m.role),
                                    ]
                                    .spacing(6)
                                    .align_y(iced::alignment::Vertical::Center),
                                    text(format!("@{}", m.username))
                                        .size(pal::fs_micro())
                                        .color(pal::TEXT_MUTED),
                                ]
                                .spacing(2),
                            )
                            .width(Length::Fill)
                            .padding(Padding::ZERO.horizontal(10.0).vertical(6.0))
                            .style(|_t| Style {
                                background: Some(pal::BG_TERTIARY.into()),
                                border: border::rounded(pal::RADIUS_MD),
                                ..Style::default()
                            }),
                        );
                    }
                }
                (None, None) => {
                    col = col.push(
                        text(if app.space_members_loading { "加载中…" } else { "暂无名册" })
                            .size(pal::fs_micro())
                            .color(pal::TEXT_MUTED),
                    );
                }
            }

            // 在线设备（presence）：自己一行 + 他人按「昵称::设备」合并，打开文件点击即开
            col = col.push(crate::settings_view::group_head(
                "在线设备",
                &format!("{} 台在线", app.space_peers.len() + 1),
                false,
            ));
            col = col.push(device_row(
                app.global
                    .last_identity
                    .as_ref()
                    .map(|i| i.display_name.clone())
                    .unwrap_or_else(|| "用户".into()),
                app.global
                    .collab_color
                    .clone()
                    .unwrap_or_else(|| crate::space_actions::DEFAULT_COLLAB_COLOR.into()),
                crate::space_actions::DEFAULT_DEVICE_NAME.to_string(),
                crate::space_actions::open_file_paths(app),
                true,
            ));
            for (nickname, device, color, files) in merge_peers(&app.space_peers) {
                col = col.push(device_row(nickname, color, device, files, false));
            }
            if app.space_peers.is_empty() {
                col = col.push(
                    text(if app.space_connected { "当前空间没有其他设备在线" } else { "连接中…" })
                        .size(pal::fs_micro())
                        .color(pal::TEXT_MUTED),
                );
            }
        }
        crate::RepoKind::Local => {
            col = col.push(controls::empty_state(
                "users",
                "未进入协作空间",
                "登录并进入空间后显示在线成员",
                Some(("协作空间".into(), Message::SpaceMenuOpen)),
            ));
        }
    }
    scrollable(container(col).width(Length::Fill).height(Length::Fill))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// 在线设备卡片：色点 + 昵称（自己标「我」）一行，设备名次行，打开文件清单可点击。
fn device_row(
    nickname: String,
    color_hex: String,
    device: String,
    files: Vec<String>,
    is_self: bool,
) -> Element<'static, Message> {
    let color = parse_hex_color(&color_hex).unwrap_or(pal::TEXT_MUTED);
    let mut head = row![
        container(space())
            .width(Length::Fixed(8.0))
            .height(Length::Fixed(8.0))
            .style(move |_t| Style {
                background: Some(color.into()),
                border: border::rounded(999.0),
                ..Style::default()
            }),
        text(nickname).size(pal::fs_body()).color(pal::TEXT_PRIMARY).font(iced::Font {
            weight: iced::font::Weight::Medium,
            ..iced::Font::default()
        }),
    ]
    .spacing(6)
    .align_y(iced::alignment::Vertical::Center);
    if is_self {
        head = head.push(mini_badge("我", pal::ACCENT));
    }
    let mut col = iced::widget::Column::<Message>::new()
        .spacing(3)
        .push(head);
    col = col.push(
        text(device)
            .size(pal::fs_micro())
            .color(pal::TEXT_MUTED),
    );
    if files.is_empty() {
        col = col.push(text("暂无打开文件").size(pal::fs_micro()).color(pal::TEXT_MUTED));
    } else {
        for rel in files {
            let name = rel.rsplit('/').next().unwrap_or(&rel).to_string();
            col = col.push(
                button(
                    row![
                        icon(file_icon(&name), 11.0, pal::TEXT_SECONDARY),
                        text(name).size(pal::fs_micro()).color(pal::TEXT_SECONDARY),
                    ]
                    .spacing(4),
                )
                .padding(Padding::ZERO.horizontal(4.0).vertical(1.0))
                .style(|_t, status| button::Style {
                    background: matches!(status, button::Status::Hovered | button::Status::Pressed)
                        .then_some(pal::HOVER.into()),
                    border: border::rounded(pal::RADIUS_XS),
                    ..Default::default()
                })
                .on_press(Message::OpenFile(rel)),
            );
        }
    }
    container(col)
        .width(Length::Fill)
        .padding(Padding::ZERO.horizontal(10.0).vertical(6.0))
        .style(|_t| Style {
            background: Some(pal::BG_TERTIARY.into()),
            border: border::rounded(pal::RADIUS_MD),
            ..Style::default()
        })
        .into()
}

/// 在线设备按「昵称::设备」合并（同身份多连接）；打开文件并集去重，色取首条。
fn merge_peers(
    peers: &[atelyx_core::collab::PeerInfo],
) -> Vec<(String, String, String, Vec<String>)> {
    let mut rows: Vec<(String, String, String, Vec<String>)> = Vec::new();
    for p in peers {
        let files = crate::space_actions::peer_open_files(p.presence.as_ref());
        match rows
            .iter_mut()
            .find(|(n, d, _, _)| *n == p.nickname && *d == p.device_name)
        {
            Some(entry) => {
                for f in files {
                    if !entry.3.contains(&f) {
                        entry.3.push(f);
                    }
                }
            }
            None => rows.push((p.nickname.clone(), p.device_name.clone(), p.color.clone(), files)),
        }
    }
    rows
}

/// 搜索视图：模式下拉（按文件名可用；全局/按标签为占位禁用）+ 实时过滤 + 结果行点击打开。
fn search_view(app: &App) -> Element<'_, Message> {
    let mode_chip = |label: &'static str, enabled: bool| {
        let color = if enabled { pal::TEXT_PRIMARY } else { pal::TEXT_MUTED };
        button(text(label).size(pal::fs_micro()).color(color))
            .padding(Padding::ZERO.horizontal(8.0).vertical(3.0))
            .style(move |_t, _s| button::Style {
                background: Some(pal::BG_TERTIARY.into()),
                border: border::rounded(pal::RADIUS_SM),
                text_color: color,
                ..button::Style::default()
            })
    };
    let mut rows = iced::widget::Column::<Message>::new().spacing(2);
    let q = app.search_query.trim().to_lowercase();
    let matched: Vec<(String, String)> = app
        .note_files
        .iter()
        .filter(|(_, name)| q.is_empty() || name.to_lowercase().contains(&q))
        .cloned()
        .collect();
    let empty = matched.is_empty();
    for (rel, name) in matched {
        let is_note = rel.ends_with(".md");
        let open = is_note.then(|| Message::OpenFile(rel.clone()));
        rows = rows.push(
            button(
                row![
                    icon(if is_note { "file-text" } else { "paperclip" }, 12.0, pal::TEXT_MUTED),
                    text(name.clone()).size(pal::fs_caption()).color(pal::TEXT_PRIMARY),
                    space(),
                    text(rel.clone()).size(pal::fs_micro()).color(pal::TEXT_MUTED),
                ]
                .spacing(6),
            )
            .width(Length::Fill)
            .padding(Padding::ZERO.horizontal(8.0).vertical(4.0))
            .style(|_t, status| button::Style {
                background: matches!(status, button::Status::Hovered | button::Status::Pressed)
                    .then_some(pal::HOVER.into()),
                ..button::Style::default()
            })
            .on_press_maybe(open),
        );
    }
    if empty {
        rows = rows.push(
            text(if app.note_files.is_empty() { "打开仓库后可搜索文件" } else { "无匹配文件" })
                .size(pal::fs_caption())
                .color(pal::TEXT_MUTED),
        );
    }
    container(
        column![
            row![
                mode_chip("按文件名", true),
                mode_chip("全局搜索", false),
                mode_chip("按标签搜索", false),
            ]
            .spacing(4),
            iced::widget::text_input("输入文件名过滤…", &app.search_query)
                .size(pal::fs_caption())
                .padding(Padding::ZERO.horizontal(8.0).vertical(4.0))
                .on_input(Message::SearchInput)
                .style(|_theme, status| text_input::Style {
                    background: pal::BG_SUNKEN.into(),
                    border: border::rounded(pal::RADIUS_SM)
                        .color(if matches!(status, text_input::Status::Focused { .. }) { pal::ACCENT } else { pal::BORDER })
                        .width(1.0),
                    icon: pal::TEXT_MUTED,
                    placeholder: pal::INPUT_PLACEHOLDER,
                    value: pal::TEXT_PRIMARY,
                    selection: pal::SELECTION,
                }),
            scrollable(rows).width(Length::Fill).height(Length::Fill),
        ]
        .spacing(8),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .padding(8.0)
    .into()
}

/// 属性面板视图：笔记属性（与编辑器属性区同一组件，改动经同一保存链落盘）。
fn inspector_view<'a>(app: &'a App, window: iced::window::Id) -> Element<'a, Message> {
    if app.ui.last_note_file.is_none() {
        return controls::view_placeholder(
            "info",
            "笔记属性",
            "打开一篇笔记后在此查看与编辑其属性。",
        );
    }
    crate::properties_view::props_panel(app, window, true)
        .unwrap_or_else(|| text("当前笔记无属性").size(pal::fs_caption()).color(pal::TEXT_MUTED).into())
}

/// 占位视图：大图标 + 名称 + 未实现说明（PanelPlaceholder 同款形态）。
fn placeholder_view(app: &App, kind: &str) -> Element<'static, Message> {
    controls::view_placeholder(
        view_icon(app, kind),
        view_label(app, kind),
        "该视图尚未实现".to_string(),
    )
}

fn placeholder_panel(label: &str) -> Element<'static, Message> {
    container(text(label.to_string()).size(pal::fs_ui()).color(pal::TEXT_MUTED))
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
        let has_file = (t.view == "note" && app.ui.last_note_file.is_some())
            || (t.view == "table" && app.ui.last_table_file.is_some());
        let text_color = if active { pal::TEXT_PRIMARY } else { pal::TEXT_SECONDARY };
        let mut label_row = row![].spacing(4);
        label_row = label_row.push(icon(view_icon(app, &t.view), 13.0, text_color));
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

// ===== files 视图 =====

/// 一条侧栏可见行（`guides` = 该行要画的展开指示线 x 坐标）。
struct TreeRow {
    rel: String,
    name: String,
    depth: usize,
    is_dir: bool,
    expanded: bool,
    active: bool,
    guides: Vec<f32>,
}

fn files_view(app: &App) -> Element<'_, Message> {
    let window = app.main_id.unwrap_or(iced::window::Id::unique());
    let cursor = app.cursor_for(window);

    // 工具条（未开仓库也渲染：空态文案指向工具条的打开按钮）
    let (expand_icon, expand_label) = if app.expand_all {
        ("chevrons-down-up", "收起全部文件夹")
    } else {
        ("chevrons-up-down", "展开全部文件夹")
    };
    let toolbar = container(
        row![
            controls::icon_button("folder-open", 15.0, controls::ICON_MD, Variant::Ghost,
                Some(Message::PickVault), Some((window, "打开文件夹为仓库".into()))),
            controls::icon_button("cloud", 15.0, controls::ICON_MD, Variant::Ghost,
                Some(Message::SpaceMenuOpen), Some((window, "协作空间".into()))),
            controls::icon_button("arrow-up-down", 14.0, controls::ICON_MD, Variant::Ghost,
                Some(btn_popup(window, cursor, PopupState::SortMenu)), Some((window, "排序方式".into()))),
            space().width(Length::Fill),
            controls::icon_button(expand_icon, 14.0, controls::ICON_MD, Variant::Ghost,
                Some(Message::ExpandAllToggle), Some((window, expand_label.into()))),
        ]
        .spacing(2),
    )
    .width(Length::Fill)
    .padding(Padding::ZERO.horizontal(8.0).vertical(6.0))
    .style(|_t| Style {
        background: Some(pal::BG_SECONDARY.into()),
        ..Style::default()
    });

    // 空间列表已拉到（首登尚未产生任何最近记录）时不早退：空间区渲染服务端账号空间
    if app.global.recent_vaults.is_empty()
        && app.global.recent_spaces.is_empty()
        && app.space_list.is_empty()
    {
        return column![
            toolbar,
            controls::h_divider_subtle(),
            controls::empty_state(
                "folder-open",
                "还没有仓库",
                "打开一个文件夹作为仓库，或经工具条云钮登录协作空间。",
                Some(("打开文件夹".into(), Message::PickVault)),
            ),
        ]
        .width(Length::Fill)
        .height(Length::Fill)
        .into();
    }

    // 列表体：每个最近仓库一行，激活仓库就地展开文件树，其余收起
    let expanded: HashSet<&str> =
        app.ui.file_explorer_expanded.iter().map(|s| s.as_str()).collect();
    let active_root = app.vault_root.as_ref().map(|r| r.to_string_lossy().into_owned());
    // 新建草稿输入行：dir = "" 时落在激活仓库行之后，否则落在对应目录行之后
    let draft_dir = match &app.rename {
        Some(rs) => match &rs.target {
            RenameTarget::Draft { dir, .. } => Some(dir.clone()),
            _ => None,
        },
        None => None,
    };
    let mut tree_col = column![].padding(Padding::ZERO.top(6.0).bottom(12.0));
    for v in &app.global.recent_vaults {
        let active = active_root.as_deref() == Some(v.root.as_str());
        tree_col = tree_col.push(vault_row(app, window, cursor, v, active, &expanded));
        if !active {
            continue;
        }
        // 重名自动加序号的提醒条（3s 自动消失）
        if let Some((notice, _)) = &app.rename_notice {
            tree_col = tree_col.push(
                container(text(notice.clone()).size(pal::fs_caption()).color(pal::WARNING))
                    .width(Length::Fill)
                    .padding(Padding::ZERO.horizontal(12.0).vertical(4.0)),
            );
        }
        if draft_dir.as_deref() == Some("") {
            tree_col = tree_col.push(draft_input_row(app, 1));
        }
        if expanded.contains("") {
            for r in tree_rows(app, &expanded) {
                // 就地重命名的输入行替换对应行
                if let Some(input) = file_inline_input(app, &r) {
                    tree_col = tree_col.push(input);
                    continue;
                }
                tree_col = tree_col.push(tree_row_element(app, &r, window, cursor));
                if draft_dir.as_deref() == Some(r.rel.as_str()) {
                    tree_col = tree_col.push(draft_input_row(app, r.depth + 1));
                }
            }
        }
    }

    // 协作空间区：服务端空间列表在前（登录即见全部账号空间），最近空间兜底补齐
    // （服务端未加载 / 非本机最近的空间仍可见可点；key = server#spaceId 去重）。
    // 激活空间就地展开文件树
    let mut space_entries: Vec<atelyx_core::global_config::RecentSpace> = Vec::new();
    let mut seen_space = std::collections::HashSet::new();
    if let Some(identity) = &app.global.last_identity {
        for s in &app.space_list {
            if seen_space.insert(format!("{}#{}", identity.server_url, s.space_id)) {
                space_entries.push(atelyx_core::global_config::RecentSpace {
                    server_url: identity.server_url.clone(),
                    space_id: s.space_id.clone(),
                    name: s.name.clone(),
                    last_opened_at: 0,
                });
            }
        }
    }
    for s in &app.global.recent_spaces {
        if seen_space.insert(format!("{}#{}", s.server_url, s.space_id)) {
            space_entries.push(s.clone());
        }
    }
    for s in &space_entries {
        let active = match &app.repo_kind {
            crate::RepoKind::Space(session) => {
                session.server_url == s.server_url && session.space_id == s.space_id
            }
            crate::RepoKind::Local => false,
        };
        tree_col = tree_col.push(space_row(window, cursor, s, active, &expanded));
        if !active {
            continue;
        }
        if let Some((notice, _)) = &app.rename_notice {
            tree_col = tree_col.push(
                container(text(notice.clone()).size(pal::fs_caption()).color(pal::WARNING))
                    .width(Length::Fill)
                    .padding(Padding::ZERO.horizontal(12.0).vertical(4.0)),
            );
        }
        if draft_dir.as_deref() == Some("") {
            tree_col = tree_col.push(draft_input_row(app, 1));
        }
        if expanded.contains("") {
            for r in tree_rows(app, &expanded) {
                if let Some(input) = file_inline_input(app, &r) {
                    tree_col = tree_col.push(input);
                    continue;
                }
                tree_col = tree_col.push(tree_row_element(app, &r, window, cursor));
                if draft_dir.as_deref() == Some(r.rel.as_str()) {
                    tree_col = tree_col.push(draft_input_row(app, r.depth + 1));
                }
            }
        }
    }

    let body = mouse_area(
        scrollable(
            container(tree_col)
                .width(Length::Fill)
                .padding(Padding::ZERO.horizontal(4.0)),
        )
        .width(Length::Fill)
        .height(Length::Fill),
    );
    // 空白处右键 = 在激活仓库/空间根目录新建（未激活时无根可建，不挂菜单）
    let tree: Element<'static, Message> = if app.vault_root.is_some()
        || matches!(app.repo_kind, crate::RepoKind::Space(_))
    {
        body.on_right_press(ctx_popup(
            window,
            cursor,
            PopupState::FileMenu { rel: String::new(), is_dir: true, is_root: true },
        ))
        .into()
    } else {
        body.into()
    };

    column![toolbar, controls::h_divider_subtle(), tree]
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// 最近仓库行（激活行金色淡底 + 就地展开；非激活行点击进入；悬停显示完整路径；右键 = 仓库行菜单）。
fn vault_row(
    app: &App,
    window: iced::window::Id,
    cursor: Point,
    vault: &RecentVault,
    active: bool,
    expanded: &HashSet<&str>,
) -> Element<'static, Message> {
    let vault_open = active && expanded.contains("");
    // 拖拽悬停激活仓库行 = 移入根：底色加深提示可放入
    let drag_target = active && app.file_drag_target.as_deref() == Some("");
    let row = row![
        icon(
            if vault_open { "chevron-down" } else { "chevron-right" },
            14.0,
            pal::TEXT_MUTED,
        ),
        icon("hard-drive", 14.0, if active { pal::ACCENT } else { pal::TEXT_MUTED }),
        text(vault.name.clone())
            .size(pal::fs_caption())
            .color(pal::TEXT_PRIMARY)
            .font(iced::Font { weight: iced::font::Weight::Medium, ..iced::Font::default() }),
    ]
    .spacing(8)
    .padding(Padding::ZERO.horizontal(8.0).vertical(4.0))
    .width(Length::Fill);

    let content = container(row)
        .width(Length::Fill)
        .height(Length::Fixed(32.0))
        .align_y(Vertical::Center)
        .style(move |_t| Style {
            // 激活行 = 强调淡底（拖拽落点更深）；非激活行透明（悬停高亮由按钮样式给）
            background: Some(if active {
                if drag_target { pal::ACCENT.scale_alpha(0.35) } else { pal::ACCENT.scale_alpha(0.2) }
            } else {
                Color::TRANSPARENT
            }
            .into()),
            border: border::rounded(pal::RADIUS_SM),
            ..Style::default()
        });

    let on_press = if active {
        Message::ToggleDir(String::new())
    } else {
        Message::SelectVault { path: PathBuf::from(&vault.root), from_boot: false }
    };
    let row_elem = mouse_area(
        button(content)
            .width(Length::Fill)
            .style(|_t, status| button::Style {
                background: matches!(status, button::Status::Hovered | button::Status::Pressed)
                    .then_some(pal::HOVER.into()),
                ..Default::default()
            })
            .on_press(on_press),
    );
    // 悬停提示完整路径；文件拖拽会话中悬停激活仓库行 = 落点上报（拖至该行 = 移入根）
    let row_elem = if app.file_drag.is_some() && active {
        row_elem
            .on_enter(Message::FileDragEnter { rel: String::new(), is_dir: true })
            .on_exit(Message::FileDragExit { rel: String::new() })
    } else {
        let path = vault.root.clone();
        row_elem
            .on_enter(Message::Ui(crate::ui::UiMsg::HoverTip {
                window,
                label: path.clone(),
                box_px: 32.0,
            }))
            .on_exit(Message::Ui(crate::ui::UiMsg::HoverTipEnd { window, label: path }))
    };
    row_elem
        .on_right_press(ctx_popup(window, cursor, PopupState::VaultMenu { root: vault.root.clone() }))
        .into()
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
                    iced::widget::text(name.clone())
                        .size(pal::fs_micro())
                        .color(pal::TEXT_PRIMARY),
                ]
                .spacing(5),
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
                iced::widget::text(format!("+{}", members.len() - MAX_SHOWN))
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

/// 最近空间行（与仓库行同构：激活行就地展开文件树；云图标 + 空间名）。
fn space_row(
    window: iced::window::Id,
    cursor: Point,
    space: &atelyx_core::global_config::RecentSpace,
    active: bool,
    expanded: &HashSet<&str>,
) -> Element<'static, Message> {
    let row = row![
        icon(
            if active && expanded.contains("") { "chevron-down" } else { "chevron-right" },
            14.0,
            pal::TEXT_MUTED,
        ),
        icon("cloud", 14.0, if active { pal::ACCENT } else { pal::TEXT_MUTED }),
        text(space.name.clone())
            .size(pal::fs_caption())
            .color(pal::TEXT_PRIMARY)
            .font(iced::Font { weight: iced::font::Weight::Medium, ..iced::Font::default() }),
    ]
    .spacing(8)
    .padding(Padding::ZERO.horizontal(8.0).vertical(4.0))
    .width(Length::Fill);

    let content = container(row)
        .width(Length::Fill)
        .height(Length::Fixed(32.0))
        .align_y(Vertical::Center)
        .style(move |_t| Style {
            background: Some(if active { pal::ACCENT.scale_alpha(0.2) } else { Color::TRANSPARENT }.into()),
            border: border::rounded(pal::RADIUS_SM),
            ..Style::default()
        });

    let on_press = if active {
        Message::ToggleDir(String::new())
    } else {
        Message::Space(crate::SpaceMsg::SelectSpace {
            server_url: space.server_url.clone(),
            space_id: space.space_id.clone(),
            name: space.name.clone(),
            from_boot: false,
        })
    };
    mouse_area(
        button(content)
            .width(Length::Fill)
            .style(|_t, status| button::Style {
                background: matches!(status, button::Status::Hovered | button::Status::Pressed)
                    .then_some(pal::HOVER.into()),
                ..Default::default()
            })
            .on_press(on_press),
    )
    .on_enter(Message::Ui(crate::ui::UiMsg::HoverTip {
        window,
        label: format!("{} · {}", space.name, space.server_url),
        box_px: 32.0,
    }))
    .on_exit(Message::Ui(crate::ui::UiMsg::HoverTipEnd {
        window,
        label: format!("{} · {}", space.name, space.server_url),
    }))
    .on_right_press(ctx_popup(
        window,
        cursor,
        PopupState::SpaceRowMenu {
            server_url: space.server_url.clone(),
            space_id: space.space_id.clone(),
        },
    ))
    .into()
}

/// 目录/文件行（行高 32、缩进 12+16×depth、展开指示线；激活文件 = 金淡底 + 左缘 2px 金轨）。
/// 目录行 = 拖拽落点（悬停高亮）；全部行 = 拖拽源候选（PressReporter 上报按下）。
fn tree_row_element(
    app: &App,
    r: &TreeRow,
    window: iced::window::Id,
    cursor: Point,
) -> Element<'static, Message> {
    let indent = 12.0 + r.depth as f32 * 16.0;
    let drag_target = app.file_drag_target.as_deref() == Some(r.rel.as_str());
    let folder_color = if r.is_dir {
        app.folder_colors.get(&r.rel).and_then(|hex| parse_hex_color(hex))
    } else {
        None
    };
    let (icon_name, icon_color) = if r.is_dir {
        ("folder", folder_color.unwrap_or(pal::TEXT_MUTED))
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
    }
    content = content.push(icon(icon_name, 14.0, icon_color));
    content = content.push(text(r.name.clone()).size(pal::fs_caption()).color(name_color));
    if !r.is_dir {
        content = content.push(space().width(Length::Fill));
        content = content.push(
            text(ext_label(&r.name))
                .size(pal::fs_micro())
                .color(if r.active { pal::ACCENT } else { pal::TEXT_MUTED })
                .font(mono()),
        );
    }

    let bg = if r.active || drag_target { pal::ACCENT_SOFT } else { iced::Color::TRANSPARENT };
    let active = r.active;
    // 行体：底色 + 圆角
    let row_body = container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(Padding::ZERO.left(indent).top(4.0).bottom(4.0).right(8.0))
        .align_y(Vertical::Center)
        .style(move |_t: &iced::Theme| Style {
            background: Some(bg.into()),
            border: border::rounded(pal::RADIUS_SM),
            ..Style::default()
        });
    // 叠层（自下而上）：行体 → 金轨（激活行左缘 2px 内嵌；拖拽落点同款高亮）→ 展开指示线
    let mut layers = stack![row_body];
    if active || drag_target {
        layers = layers.push(inset_line(2.0, 0.0, pal::ACCENT));
    }
    for x in &r.guides {
        layers = layers.push(inset_line(1.0, *x, pal::TEXT_MUTED.scale_alpha(0.6)));
    }
    let body = layers.width(Length::Fill).height(Length::Fill);

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
    // 拖拽候选：行按下自证命中（PressReporter；mouse_area 对被按钮捕获的按下不触发）。
    // 目录行另挂拖拽悬停进出（文件行不是落点不挂）。
    let row = mouse_area(
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
    .on_right_press(Message::PopupOpen(window, PopupOpen { point: cursor, flip_y: None, state, min_width: 0.0 }));
    let row = if r.is_dir {
        row.on_enter(Message::FileDragEnter { rel: r.rel.clone(), is_dir: true })
            .on_exit(Message::FileDragExit { rel: r.rel.clone() })
    } else {
        row
    };
    controls::PressReporter::new(
        row.into(),
        Message::FileRowPress { window, rel: r.rel.clone(), is_dir: r.is_dir },
    )
    .into()
}

/// 行内叠加竖线（宽 `w` px、左偏移 `x`；行体之上的叠层画法）。
fn inset_line(w: f32, x: f32, color: iced::Color) -> Element<'static, Message> {
    container(
        container(space())
            .width(Length::Fixed(w))
            .height(Length::Fill)
            .style(move |_t| Style {
                background: Some(color.into()),
                ..Style::default()
            }),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .padding(Padding::ZERO.left(x))
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
                    DraftKind::Table => "表格名称",
                },
            ),
            _ => (String::new(), ""),
        },
        None => (String::new(), ""),
    };
    container(
        controls::inline_input(
            iced::widget::Id::new(controls::INLINE_RENAME_INPUT),
            placeholder,
            &value,
            160.0,
            Message::LayoutRenameInput,
            Message::LayoutRenameCommit,
            Message::RenamePressInside,
        ),
    )
    .width(Length::Fill)
    .height(Length::Fixed(32.0))
    .padding(Padding::ZERO.left(12.0 + depth as f32 * 16.0).right(8.0))
    .align_y(Vertical::Center)
    .into()
}

/// 就地重命名的输入行（渲染在对应行位置）。
fn file_inline_input(app: &App, r: &TreeRow) -> Option<Element<'static, Message>> {
    let indent = 12.0 + r.depth as f32 * 16.0;
    let rs = app.rename.as_ref()?;
    let (value, placeholder) = match &rs.target {
        RenameTarget::File(f) if *f == r.rel => (rs.value.clone(), ""),
        RenameTarget::Folder(d) if *d == r.rel => (rs.value.clone(), ""),
        _ => return None,
    };
    Some(
        container(
            controls::inline_input(
                iced::widget::Id::new(controls::INLINE_RENAME_INPUT),
                placeholder,
                &value,
                160.0,
                Message::LayoutRenameInput,
                Message::LayoutRenameCommit,
                Message::RenamePressInside,
            ),
        )
        .width(Length::Fill)
        .height(Length::Fixed(32.0))
        .padding(Padding::ZERO.left(indent).right(8.0))
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

/// 扩展名标签（ATLX / CANVAS / MD / ATB / 大写扩展名；前导点文件 = 空串不显示）。
fn ext_label(name: &str) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 => name[i + 1..].to_uppercase(),
        _ => String::new(),
    }
}

/// 十六进制颜色串（`#rrggbb`）→ Color；非法输入返回 None（回落默认图标色）。
pub(crate) fn parse_hex_color(hex: &str) -> Option<iced::Color> {
    let hex = hex.strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let value = u32::from_str_radix(hex, 16).ok()?;
    Some(iced::Color::from_rgb8(
        ((value >> 16) & 0xff) as u8,
        ((value >> 8) & 0xff) as u8,
        (value & 0xff) as u8,
    ))
}

/// 由子项缓存 + 展开集摊平侧栏行（每层按仓库排序键排列；根子项 depth 从 1 起，
/// 指示线 x 与行缩进由此推导）。
fn tree_rows(app: &App, expanded: &HashSet<&str>) -> Vec<TreeRow> {
    let mut rows = Vec::new();
    push_children(app, &mut rows, "", 1, expanded);
    // 展开指示线：仓库层（x=15）覆盖全部行；目录层 = 祖先目录展开时画在其子树各行
    for row in &mut rows {
        let mut guides = vec![15.0];
        for k in 1..row.depth {
            let ancestor = row.rel.split('/').take(k).collect::<Vec<_>>().join("/");
            if expanded.contains(ancestor.as_str()) {
                guides.push(19.0 + k as f32 * 16.0);
            }
        }
        row.guides = guides;
    }
    rows
}

fn push_children(
    app: &App,
    rows: &mut Vec<TreeRow>,
    dir: &str,
    depth: usize,
    expanded: &HashSet<&str>,
) {
    let mut entries: Vec<atelyx_core::vault::DirChild> = app
        .children
        .get(dir)
        .cloned()
        .unwrap_or_default();
    atelyx_core::vault::sort_children(&mut entries, &app.file_sort);
    for child in entries {
        let rel = child.rel;
        let is_dir = child.is_dir;
        let is_expanded = expanded.contains(rel.as_str());
        let name = rel.rsplit('/').next().unwrap_or(&rel).to_string();
        let active = !is_dir
            && (app.ui.last_note_file.as_deref() == Some(rel.as_str())
                || app.ui.last_table_file.as_deref() == Some(rel.as_str()));
        rows.push(TreeRow { name, depth, is_dir, expanded: is_expanded, rel: rel.clone(), active, guides: Vec::new() });
        if is_dir && is_expanded {
            push_children(app, rows, &rel, depth + 1, expanded);
        }
    }
}

// ===== note 视图 =====

/// note 视图：工具条（模式分段 + ···菜单）+ 对应档位的内容面。
fn note_view(app: &App) -> Element<'_, Message> {
    if app.ui.last_note_file.is_none() {
        return controls::view_placeholder(
            "file-text",
            "打开笔记",
            "在文件面板或搜索面板单击一个 .md 笔记开始编辑。",
        );
    }
    let window = app.main_id.unwrap_or(iced::window::Id::unique());
    let cursor = app.cursor_for(window);

    let mode_control = container(
        row![
            mode_button(window, "eye", "只读", "只读：仅查看，双击正文进入编辑", NoteMode::Read, app.note_mode),
            mode_button(window, "pencil", "预览编辑", "预览编辑：正文可直接修改", NoteMode::Edit, app.note_mode),
            mode_button(window, "code", "源码", "源码：直接编辑 Markdown 源文", NoteMode::Source, app.note_mode),
        ]
        .spacing(2),
    )
    .padding(2.0)
    .style(|_t| Style {
        background: Some(pal::BG_SUNKEN.into()),
        border: border::rounded(pal::RADIUS_SM).color(pal::BORDER).width(1.0),
        ..Style::default()
    });

    // ··· 打开态着色（弹层开着 = 强调色）+ subtle 悬停（仅文字提亮）
    let menu_open = app
        .popup
        .as_ref()
        .is_some_and(|(_, open)| matches!(open.state, PopupState::NoteMenu));
    // 协作者胶囊（空间模式；远端 awareness 在场者，色点 + 昵称，多时折叠；名册见协作房间视图）
    let mut toolbar_row = row![space().width(Length::Fill)].spacing(4);
    if matches!(app.repo_kind, crate::RepoKind::Space(_)) && !app.remote_cursors.is_empty() {
        let members: Vec<(iced::Color, String)> = app
            .remote_cursors
            .iter()
            .map(|c| {
                // 畸形色串回落灰点（与协作房间名册同兜底）
                (parse_hex_color(&c.color).unwrap_or(pal::TEXT_MUTED), c.name.clone())
            })
            .collect();
        toolbar_row = toolbar_row.push(collab_pill_row(&members));
    }
    let toolbar = container(
        toolbar_row
            .push(mode_control)
            .push(button(icon("ellipsis", 15.0, if menu_open { pal::ACCENT } else { pal::TEXT_MUTED }))
                .padding(Padding::ZERO.horizontal(4.0).vertical(4.0))
                .style(|_t, status| button::Style {
                    background: None,
                    text_color: if matches!(status, button::Status::Hovered | button::Status::Pressed) {
                        pal::TEXT_PRIMARY
                    } else {
                        pal::TEXT_SECONDARY
                    },
                    ..Default::default()
                })
                .on_press(btn_popup(window, cursor, PopupState::NoteMenu)),
        )
        .spacing(8),
    )
    .width(Length::Fill)
    .padding(Padding::ZERO.horizontal(12.0).vertical(6.0))
    .style(|_t| Style {
        background: Some(pal::BG_SECONDARY.into()),
        ..Style::default()
    });

    // 只读 / 预览编辑两档共用同一 scrollable（同型同位，iced 保留滚动状态）：切档不回顶，
    // 两档块渲染逐点一致，偏移直接对齐。源码档 text_editor 自带内部滚动，不进共享层。
    let content: Element<'_, Message> = match app.note_mode {
        NoteMode::Source => iced::widget::text_editor(&app.source_editor)
            .height(Length::Fill)
            .on_action(Message::Edited)
            .style(|_t, _s| text_editor::Style {
                background: iced::Background::Color(iced::Color::TRANSPARENT),
                border: border::Border::default(),
                placeholder: pal::TEXT_MUTED,
                value: pal::TEXT_PRIMARY,
                selection: pal::SELECTION,
            })
            .into(),
        NoteMode::Read | NoteMode::Edit => {
            let inner: Element<'_, Message> = match app.note_mode {
                NoteMode::Edit => edit_view(app),
                _ => match app.preview_doc.as_ref() {
                    Some(doc) => {
                        markdown_view::document_view(doc, &app.theme, &app.note_images, app.global.soft_wrap)
                    }
                    None => placeholder_panel("正在渲染…"),
                },
            };
            // 页面内标题：正文列顶部（两档共用；源码档不渲染）
            let mut body_col = column![].width(Length::Fill);
            if app.global.in_page_title {
                body_col = body_col.push(page_title(app));
            }
            body_col = body_col.push(inner);
            // 正文行宽：>0 时钳制只读/预览编辑正文区最大宽度并水平居中（源码档不受限）
            let inner: Element<'_, Message> = if app.global.content_width > 0 {
                container(body_col)
                    .width(Length::Fill)
                    .max_width(app.global.content_width as f32)
                    .center_x(Length::Fill)
                    .into()
            } else {
                body_col.into()
            };
            let scrolled = scrollable(inner).width(Length::Fill).height(Length::Fill);
            // 两档同包一层 mouse_area（同型同位保滚动状态）；双击进编辑只在只读档挂处理器
            // （编辑档不挂：编辑面按压即捕获，双击语义由块内自管）
            if app.note_mode == NoteMode::Edit {
                wiki_picker_overlay(app, mouse_area(scrolled).into())
            } else {
                // 只读档双击正文进预览编辑档（光标落点由编辑面按释放点换算）
                mouse_area(scrolled)
                    .on_double_click(Message::NoteEnterEdit)
                    .into()
            }
        }
    };
    // 属性面板（只读 / 预览编辑档显示；源码档不渲染）
    let panel = crate::properties_view::props_panel(app, window, false);

    let mut body = column![toolbar, controls::h_divider_subtle()];
    if let Some(p) = panel {
        body = body.push(p);
    }
    body = body.push(container(content).width(Length::Fill).height(Length::Fill));
    body = body.push(controls::h_divider_subtle());
    body = body.push(backlinks_section(app));
    // 面板内按压转播：编辑档「点击其他地方退出」的面板内外判定源。
    // 不用 mouse_area：其子部件（按钮 / 输入框 / 编辑面）会捕获按压令其跳过上报。
    controls::PressReporter::new(
        body.width(Length::Fill).height(Length::Fill).into(),
        Message::NotePressInside,
    )
    .into()
}

/// 页面内标题：正文列顶部的文件名标题（不含扩展名）；点击进入重命名（复用文件树行内改名）。
fn page_title(app: &App) -> Element<'_, Message> {
    let rel = app.ui.last_note_file.clone().unwrap_or_default();
    let name = rel.rsplit('/').next().unwrap_or(&rel);
    let name = name.strip_suffix(".md").unwrap_or(name);
    mouse_area(
        container(
            text(name.to_string())
                .size(pal::fs_h1())
                .color(pal::TEXT_PRIMARY)
                .font(crate::ui::semibold()),
        )
        .padding(Padding::ZERO.top(12.0).bottom(4.0)),
    )
    .on_press(Message::RenameStartFile(rel))
    .into()
}

/// 模式分段按钮（Button sm 档：高 24 / px-8 / 11px / medium；激活 = 强调淡底，悬停文字提亮）。
fn mode_button(
    window: iced::window::Id,
    icon_name: &'static str,
    label: &str,
    hint: &str,
    mode: NoteMode,
    current: NoteMode,
) -> Element<'static, Message> {
    let active = mode == current;
    let btn = button(
        container(
            row![
                icon(icon_name, 12.0, if active { pal::ACCENT } else { pal::TEXT_SECONDARY }),
                text(label.to_string())
                    .size(pal::fs_micro())
                    .font(iced::Font { weight: iced::font::Weight::Medium, ..iced::Font::default() })
                    .color(if active { pal::ACCENT } else { pal::TEXT_SECONDARY }),
            ]
            .spacing(4),
        )
        .height(Length::Fill)
        .align_y(Vertical::Center),
    )
    .padding(Padding::ZERO.horizontal(8.0))
    .height(Length::Fixed(24.0))
    .style(move |_t, status| button::Style {
        background: active.then_some(pal::ACCENT_SOFT.into()),
        text_color: if matches!(status, button::Status::Hovered | button::Status::Pressed) && !active {
            pal::TEXT_PRIMARY
        } else {
            pal::TEXT_SECONDARY
        },
        border: border::rounded(pal::RADIUS_SM),
        ..Default::default()
    })
    .on_press(Message::NoteMode(mode));
    iced::widget::mouse_area(btn)
        .on_enter(Message::Ui(crate::ui::UiMsg::HoverTip {
            window,
            label: hint.to_string(),
            box_px: 24.0,
        }))
        .on_exit(Message::Ui(crate::ui::UiMsg::HoverTipEnd { window, label: hint.to_string() }))
        .into()
}

/// 预览编辑档：块行与空行行交给编辑面容器统一排布与承接鼠标；非活动块由只读块
/// 控件承载（视觉逐点一致），仅活动块与选区锚点块回显源码。
/// 返回裸编辑面（不含滚动容器与双链浮层——由 note_view 统一包外层，切档共用滚动状态）。
fn edit_view(app: &App) -> Element<'_, Message> {
    use markdown_edit::SurfaceRow;

    let Some(doc) = app.edit_doc.as_ref() else {
        return placeholder_panel("正在解析…");
    };
    let editable = app.editable_blocks();
    let pal_edit = markdown_edit::EditPalette::from_theme(&app.theme);
    let note_bindings = crate::commands::note_bindings(&app.global.command_shortcuts);
    let (sel_from, sel_to) = app.edit.selection();
    let mut rows = Vec::new();
    let mut prev: Option<usize> = None;
    let caret = app.edit.caret.min(doc.source.len());
    // 远端光标以 note_text 全文为参照，编辑面坐标相对正文
    let body_start = atelyx_core::frontmatter::block_end(&app.note_text).unwrap_or(0);
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
                cursors: &app.remote_cursors,
                body_start,
                bindings: note_bindings.clone(),
            }
            .into(),
        });
        return markdown_edit::EditSurface::new(
            rows,
            pal_edit,
            &doc.source,
            caret,
            app.edit.anchor,
            app.edit.preedit.clone(),
            app.edit.focused,
            app.note_edit_caret_pending,
            note_bindings,
        )
        .into();
    }
    // 文首空白行（首块之前）：文首区每行都是空白行，末尾不带换行的片段是缩进
    if let Some(first) = app.edit_meta.first() {
        if first.from > 0 {
            let line_starts = markdown_edit::edge_blank_starts(&doc.source, 0, first.from);
            let lines = line_starts.len();
            if lines > 0 {
                let covered =
                    markdown_edit::gap_covered_lines(&line_starts, first.from, sel_from, sel_to);
                rows.push(SurfaceRow::Gap {
                    from: 0,
                    to: first.from,
                    line_starts,
                    covered,
                    element: markdown_view::gap(lines),
                });
            }
        }
    }
    for (index, chunk) in
        markdown_view::block_chunks(&doc.source, &doc.blocks, &app.theme, true, &app.note_images, true)
            .into_iter()
            .enumerate()
    {
        let editable = editable.contains(&index);
        let meta = app.edit_meta.get(index);
        if chunk.gap_before > 0 {
            if let Some(p) = prev {
                let from = app.edit_meta[p].to;
                let line_starts = markdown_edit::gap_line_starts(&doc.source, from, chunk.from);
                let covered =
                    markdown_edit::gap_covered_lines(&line_starts, chunk.from, sel_from, sel_to);
                rows.push(SurfaceRow::Gap {
                    from,
                    to: chunk.from,
                    line_starts,
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
                cursors: &app.remote_cursors,
                body_start,
                bindings: note_bindings.clone(),
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
    // 文末空白行（末块之后）：块区间未覆盖的尾部空白行；文本以换行结尾时
    // 末行下方还有一行虚拟空行（点击落光标、可直接输入，对齐整篇文本面行为）
    if let Some(last) = app.edit_meta.last() {
        if doc.source.len() > last.to {
            let line_starts = markdown_edit::tail_blank_starts(&doc.source, last.to);
            let lines = line_starts.len();
            if lines > 0 {
                // 托管区间含虚拟文末行：光标可停在 len（右开）
                let to = doc.source.len() + 1;
                let covered =
                    markdown_edit::gap_covered_lines(&line_starts, to, sel_from, sel_to);
                rows.push(SurfaceRow::Gap {
                    from: last.to,
                    to,
                    line_starts,
                    covered,
                    element: markdown_view::gap(lines),
                });
            }
        }
    }
    let surface = markdown_edit::EditSurface::new(
        rows,
        pal_edit,
        &doc.source,
        caret,
        app.edit.anchor,
        app.edit.preedit.clone(),
        app.edit.focused,
        app.note_edit_caret_pending,
        note_bindings,
    );
    surface.into()
}

/// 双链候选浮层（编辑面顶部右侧覆盖；↑↓ 导航、Enter 选中、Esc 关闭）。
fn wiki_picker_overlay<'a>(app: &'a App, surface: Element<'a, Message>) -> Element<'a, Message> {
    let Some(picker) = &app.wiki_picker else { return surface };
    let candidates = app.wiki_candidates();
    let mut col = iced::widget::Column::<Message>::new().spacing(0);
    if candidates.is_empty() {
        col = col.push(
            container(text("无匹配笔记").size(pal::fs_caption()).color(pal::TEXT_MUTED))
                .padding(Padding::ZERO.horizontal(12.0).vertical(6.0)),
        );
    } else {
        let active = picker.active.min(candidates.len() - 1);
        for (pos, fi) in candidates.iter().enumerate() {
            let (_, name) = &app.note_files[*fi];
            let title = name.trim_end_matches(".md").to_string();
            let is_active = pos == active;
            col = col.push(
                button(
                    row![
                        icon("file-text", 13.0, if is_active { pal::TEXT_PRIMARY } else { pal::TEXT_MUTED }),
                        text(title.clone())
                            .size(pal::fs_caption())
                            .color(if is_active { pal::TEXT_PRIMARY } else { pal::TEXT_SECONDARY }),
                    ]
                    .spacing(6),
                )
                .width(Length::Fill)
                .padding(Padding::ZERO.horizontal(12.0).vertical(5.0))
                .style(move |_t, _s| button::Style {
                    background: is_active.then_some(pal::ACCENT.into()),
                    ..button::Style::default()
                })
                .on_press(Message::WikiPick(*fi)),
            );
        }
    }
    let panel = container(
        scrollable(col)
            .width(Length::Fill)
            .height(Length::Fixed(264.0))
            .direction(scrollable::Direction::Vertical(scrollable::Scrollbar::new())),
    )
    .width(Length::Fixed(288.0))
    .style(|_t| Style {
        background: Some(pal::BG_SECONDARY.into()),
        border: border::rounded(pal::RADIUS_MD).color(pal::BORDER).width(1.0),
        ..Style::default()
    });
    stack![
        surface,
        container(panel)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(iced::alignment::Horizontal::Right)
            .align_y(iced::alignment::Vertical::Top)
            .padding(8.0),
    ]
    .into()
}

/// 反链区（常驻）：引用当前笔记的笔记列表，点击打开。
fn backlinks_section(app: &App) -> Element<'_, Message> {
    let n = app.backlinks.len();
    let mut col = iced::widget::Column::<Message>::new()
        .padding(Padding::ZERO.horizontal(16.0).vertical(6.0))
        .spacing(2);
    col = col.push(
        row![
            icon("unlink", 12.0, pal::TEXT_MUTED),
            text(format!("反链 ({n})")).size(pal::fs_caption()).color(pal::TEXT_SECONDARY),
        ]
        .spacing(6),
    );
    for link in &app.backlinks {
        let rel = link.file.clone();
        col = col.push(
            button(
                row![
                    icon("file-text", 12.0, pal::TEXT_MUTED),
                    text(link.title.clone()).size(pal::fs_caption()).color(pal::TEXT_PRIMARY),
                ]
                .spacing(6),
            )
            .padding(Padding::ZERO.horizontal(4.0).vertical(2.0))
            .style(|_t, status| button::Style {
                background: matches!(status, button::Status::Hovered | button::Status::Pressed)
                    .then_some(pal::HOVER.into()),
                ..button::Style::default()
            })
            .on_press(Message::OpenFile(rel)),
        );
    }
    if n == 0 {
        col = col.push(text("暂无其他笔记引用当前笔记").size(pal::fs_micro()).color(pal::TEXT_MUTED));
    }
    container(col).width(Length::Fill).into()
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
            let mut entries: Vec<(usize, MenuEntry)> = vec![
                (
                    0,
                    MenuEntry::Action {
                        label: "关闭该标签".into(),
                        icon: Some("x"),
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
        PopupState::SortMenu => {
            const OPTIONS: [(&str, &str); 4] = [
                ("name-asc", "名称 (A-Z)"),
                ("name-desc", "名称 (Z-A)"),
                ("mtime-desc", "编辑时间 (从新到旧)"),
                ("mtime-asc", "编辑时间 (从旧到新)"),
            ];
            OPTIONS
                .iter()
                .map(|(key, label)| {
                    (
                        0,
                        MenuEntry::Action {
                            label: (*label).to_string(),
                            icon: None,
                            danger: false,
                            disabled: false,
                            on_press: Some(Message::SetSort((*key).to_string())),
                            note: (app.file_sort == *key).then_some("当前"),
                            hint: None,
                        },
                    )
                })
                .collect()
        }
        PopupState::EntrySceneMenu => {
            let pick = |id: Option<String>| Message::EntryScenePick(id);
            let current = app.entry_scene_id.as_deref();
            let mut entries: Vec<(usize, MenuEntry<'static>)> = vec![(
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
        PopupState::FileMenu { rel, is_dir, is_root } => {
            file_menu_entries(app, rel, *is_dir, *is_root)
        }
        PopupState::VaultMenu { root } => vault_menu_entries(app, root),
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
        PopupState::SpaceRowMenu { server_url, space_id } => {
            let active = matches!(
                app.repo_kind,
                crate::RepoKind::Space(ref s)
                    if s.server_url == *server_url && s.space_id == *space_id
            );
            // 权限口径对齐服务端：名册对全部成员可见（进空间后可看），
            // 邀请码（列出/生成/撤销）与管理操作为 owner 专属
            let owner = active
                && matches!(&app.repo_kind, crate::RepoKind::Space(s) if s.role == atelyx_core::space_client::SpaceRole::Owner);
            let mut entries: Vec<(usize, MenuEntry<'static>)> = vec![(
                0,
                MenuEntry::Action {
                    label: "成员管理".into(),
                    icon: Some("users"),
                    danger: false,
                    disabled: !active,
                    on_press: active.then(|| {
                        Message::PopupOpen(
                            window,
                            PopupOpen {
                                point: app.cursor_for(window),
                                flip_y: None,
                                state: PopupState::SpaceMembers,
                                min_width: 0.0,
                            },
                        )
                    }),
                    note: None,
                    hint: (!active).then(|| "进入空间后可查看成员".to_string()),
                },
            )];
            entries.push((
                0,
                MenuEntry::Action {
                    label: "邀请码".into(),
                    icon: Some("user-plus"),
                    danger: false,
                    disabled: !owner,
                    on_press: owner.then(|| {
                        Message::PopupOpen(
                            window,
                            PopupOpen {
                                point: app.cursor_for(window),
                                flip_y: None,
                                state: PopupState::SpaceInvite,
                                min_width: 0.0,
                            },
                        )
                    }),
                    note: None,
                    hint: Some(if active {
                        "邀请码仅空间所有者可管理".to_string()
                    } else {
                        "进入空间后可管理".to_string()
                    }),
                },
            ));
            entries.push((0, MenuEntry::Divider));
            entries.push((
                0,
                MenuEntry::Action {
                    label: "从列表移除".into(),
                    icon: Some("x"),
                    danger: true,
                    disabled: active,
                    on_press: (!active).then(|| Message::RemoveSpaceAsk {
                        server_url: server_url.clone(),
                        space_id: space_id.clone(),
                    }),
                    note: None,
                    hint: active.then(|| "空间已激活，先进入其他仓库或空间".to_string()),
                },
            ));
            entries
        }
        PopupState::SpaceMembers => space_members_entries(app),
        PopupState::SpaceInvite => space_invite_entries(app),
        PopupState::NoteMenu => {
            let note_open = app.ui.last_note_file.is_some();
            vec![
                (0, MenuEntry::action("添加笔记属性", Message::Props(crate::properties_view::PropsMsg::StartAdd))),
                (
                    0,
                    MenuEntry::Action {
                        label: "历史记录".into(),
                        icon: Some("history"),
                        danger: false,
                        disabled: !note_open,
                        on_press: app.ui.last_note_file.clone().map(|file| {
                            Message::History(crate::history_browser::HistoryBrowserMsg::Open {
                                window,
                                kind: atelyx_core::history::HistoryKind::Note,
                                file,
                            })
                        }),
                        note: None,
                        hint: (!note_open).then(|| "未打开笔记".to_string()),
                    },
                ),
            ]
        }
        PopupState::PropTypeMenu { index } => {
            // 固定顺序六类型；徽章键锁定标签（其余项禁用）
            use atelyx_core::frontmatter::{infer_type, PropType, Yaml};
            let badge = matches!(app.props.parsed.as_ref(), Some((Yaml::Map(pairs), true)) if pairs.get(*index).is_some_and(|(k, _)| crate::properties_view::is_badge_key(k)));
            let current = app.props.parsed.as_ref().and_then(|(data, _)| match data {
                Yaml::Map(pairs) => pairs.get(*index).map(|(_, v)| infer_type(badge, v)),
                _ => None,
            });
            const TYPES: [(PropType, &str, &str); 6] = [
                (PropType::Text, "type", "文本"),
                (PropType::Tag, "tag", "标签"),
                (PropType::Date, "calendar-days", "日期"),
                (PropType::Number, "hash", "数字"),
                (PropType::Boolean, "toggle-left", "布尔"),
                (PropType::List, "list", "列表"),
            ];
            TYPES
                .iter()
                .map(|(pt, icon_name, label)| {
                    let locked = badge && *pt != PropType::Tag;
                    let is_current = current == Some(*pt);
                    (
                        0,
                        MenuEntry::Action {
                            label: (*label).to_string(),
                            icon: Some(icon_name),
                            danger: false,
                            disabled: locked,
                            on_press: (!locked)
                                .then(|| Message::Props(crate::properties_view::PropsMsg::ConvertType(*index, *pt))),
                            note: is_current.then_some("当前"),
                            hint: locked.then(|| "预置属性键固定".to_string()),
                        },
                    )
                })
                .collect()
        }
        PopupState::TableCellMenu => vec![
            (0, MenuEntry::action_icon("复制", "copy", Message::Table(crate::table_view::TableMsg::Menu(Box::new(crate::table_view::TableMsg::Copy))))),
            (0, MenuEntry::action_icon("剪切", "scissors", Message::Table(crate::table_view::TableMsg::Menu(Box::new(crate::table_view::TableMsg::Cut))))),
            (0, MenuEntry::action_icon("粘贴", "clipboard-paste", Message::Table(crate::table_view::TableMsg::Menu(Box::new(crate::table_view::TableMsg::Paste))))),
            (0, MenuEntry::action_icon("格式", "type", Message::Table(crate::table_view::TableMsg::ShowStyleBar))),
        ],
        PopupState::TableFieldMenu { field_id } => {
            let fid = field_id.clone();
            let ftype = app.table.as_ref().and_then(|s| {
                s.data.fields.iter().find(|f| f.id == *field_id).map(|f| f.field_type.clone())
            });
            let mut entries = vec![
                (
                    0,
                    MenuEntry::action(
                        "重命名",
                        Message::PopupOpen(
                            window,
                            PopupOpen {
                                point: app.cursor_for(window),
                                flip_y: None,
                                state: PopupState::TableFieldRename { field_id: fid.clone() },
                                min_width: 0.0,
                            },
                        ),
                    ),
                ),
                (
                    0,
                    MenuEntry::Submenu { label: "字段类型".into(), icon: None, disabled: false, pane: 2 },
                ),
            ];
            if ftype.as_deref() == Some("singleSelect") {
                entries.push((
                    0,
                    MenuEntry::action(
                        "单选选项管理",
                        Message::PopupOpen(
                            window,
                            PopupOpen {
                                point: app.cursor_for(window),
                                flip_y: None,
                                state: PopupState::TableOptionsEdit { field_id: fid.clone() },
                                min_width: 0.0,
                            },
                        ),
                    ),
                ));
            }
            entries.push((0, MenuEntry::Divider));
            entries.push((
                0,
                MenuEntry::Submenu { label: "删除字段".into(), icon: None, disabled: false, pane: 3 },
            ));
            // pane 2：类型列表；pane 3：删除确认
            const FTYPES: [(&str, &str); 5] = [
                ("text", "文本"),
                ("number", "数字"),
                ("duration", "时长（秒）"),
                ("singleSelect", "单选"),
                ("image", "图片"),
            ];
            for (ft, label) in FTYPES {
                let is_current = ftype.as_deref() == Some(ft);
                entries.push((
                    2,
                    MenuEntry::Action {
                        label: label.to_string(),
                        icon: None,
                        danger: false,
                        disabled: false,
                        on_press: Some(Message::Table(crate::table_view::TableMsg::RetypeField {
                            field_id: fid.clone(),
                            ftype: ft.to_string(),
                        })),
                        note: is_current.then_some("当前"),
                        hint: None,
                    },
                ));
            }
            entries.push((3, MenuEntry::action("确认删除（清空该列全部值）", Message::Table(crate::table_view::TableMsg::DeleteField(fid.clone())))));
            entries.push((3, MenuEntry::action("取消", Message::Ui(crate::ui::UiMsg::PopupClose { window }))));
            entries
        }
        PopupState::TableFieldRename { field_id } => {
            let fid = field_id.clone();
            let current = app
                .table
                .as_ref()
                .and_then(|s| s.data.fields.iter().find(|f| f.id == *field_id))
                .map(|f| f.name.clone())
                .unwrap_or_default();
            let draft = if app.table_popup_draft.is_empty() { current.clone() } else { app.table_popup_draft.clone() };
            vec![
                (0, MenuEntry::Custom(popup_text_input(&draft), 30.0)),
                (
                    0,
                    MenuEntry::action("重命名", Message::Table(crate::table_view::TableMsg::RenameField {
                        field_id: fid,
                        name: draft,
                    })),
                ),
                (0, MenuEntry::Custom(popup_hint(&format!("当前：{current}")), 18.0)),
            ]
        }
        PopupState::TableOptionsEdit { field_id } => {
            let fid = field_id.clone();
            let current = app
                .table
                .as_ref()
                .and_then(|s| s.data.fields.iter().find(|f| f.id == *field_id))
                .and_then(|f| f.options.clone())
                .unwrap_or_default();
            let draft = app.table_popup_draft.clone();
            // 逐项维护：输入 + 追加；已有选项逐条删除（保持弹层打开，外点关闭）
            let mut entries = vec![
                (0, MenuEntry::Custom(popup_text_input(&draft), 30.0)),
                (
                    0,
                    MenuEntry::action("添加选项", Message::Table(crate::table_view::TableMsg::OptionsAdd {
                        field_id: fid.clone(),
                        name: draft,
                    })),
                ),
                (0, MenuEntry::Divider),
            ];
            for (i, o) in current.iter().enumerate() {
                entries.push((
                    0,
                    MenuEntry::Action {
                        label: o.clone(),
                        icon: Some("x"),
                        danger: true,
                        disabled: false,
                        on_press: Some(Message::Table(crate::table_view::TableMsg::OptionsRemove {
                            field_id: fid.clone(),
                            index: i,
                        })),
                        note: None,
                        hint: None,
                    },
                ));
            }
            if current.is_empty() {
                entries.push((0, MenuEntry::Custom(popup_hint("暂无选项"), 18.0)));
            }
            entries
        }
        PopupState::TableColumnMenu { field_id } => {
            let fid = field_id.clone();
            let entries = vec![
                (
                    0,
                    MenuEntry::action_icon(
                        "列宽自适应",
                        "columns-3",
                        Message::Table(crate::table_view::TableMsg::ColumnAutoWidth(Some(fid.clone()))),
                    ),
                ),
                (
                    0,
                    MenuEntry::action_icon(
                        "格式",
                        "type",
                        Message::Table(crate::table_view::TableMsg::ShowStyleBar),
                    ),
                ),
                (
                    0,
                    MenuEntry::action_icon(
                        "左侧插入字段",
                        "arrow-left",
                        Message::PopupOpen(
                            window,
                            PopupOpen {
                                point: app.cursor_for(window),
                                flip_y: None,
                                state: PopupState::TableColumnInsert { field_id: fid.clone(), side: -1 },
                                min_width: 0.0,
                            },
                        ),
                    ),
                ),
                (
                    0,
                    MenuEntry::action_icon(
                        "右侧插入字段",
                        "arrow-right",
                        Message::PopupOpen(
                            window,
                            PopupOpen {
                                point: app.cursor_for(window),
                                flip_y: None,
                                state: PopupState::TableColumnInsert { field_id: fid.clone(), side: 1 },
                                min_width: 0.0,
                            },
                        ),
                    ),
                ),
            ];
            entries
        }
        PopupState::TableColumnInsert { field_id, side } => {
            let side = *side;
            let draft = app.table_popup_draft.clone();
            let idx = app.table.as_ref().and_then(|s| {
                s.data.fields.iter().position(|f| f.id == *field_id).map(|i| if side < 0 { i } else { i + 1 })
            });
            vec![
                (0, MenuEntry::Custom(popup_text_input(&draft), 30.0)),
                (
                    0,
                    MenuEntry::action("添加", Message::Table(crate::table_view::TableMsg::InsertField {
                        index: idx,
                        name: draft,
                    })),
                ),
                (0, MenuEntry::action("取消", Message::Ui(crate::ui::UiMsg::PopupClose { window }))),
            ]
        }
        PopupState::TableRowMenu { row_id } => {
            let rid = row_id.clone();
            vec![
                (
                    0,
                    MenuEntry::action_icon("复制行", "columns-3", Message::Table(crate::table_view::TableMsg::DuplicateRow(rid.clone()))),
                ),
                (
                    0,
                    MenuEntry::action("上移", Message::Table(crate::table_view::TableMsg::MoveRow { row_id: rid.clone(), delta: -1 })),
                ),
                (
                    0,
                    MenuEntry::action("下移", Message::Table(crate::table_view::TableMsg::MoveRow { row_id: rid.clone(), delta: 1 })),
                ),
                (
                    0,
                    MenuEntry::action_icon("行高自适应", "align-vertical-space-around", Message::Table(crate::table_view::TableMsg::RowHeightAuto(Some(rid.clone())))),
                ),
                (
                    0,
                    MenuEntry::action_icon("格式", "type", Message::Table(crate::table_view::TableMsg::ShowStyleBar)),
                ),
                (0, MenuEntry::Divider),
                (
                    0,
                    MenuEntry::danger("删除行", "trash-2", Message::Table(crate::table_view::TableMsg::DeleteRows(vec![rid]))),
                ),
            ]
        }
        PopupState::TableAddField => {
            let draft = app.table_popup_draft.clone();
            let mut entries = vec![(0, MenuEntry::Custom(popup_text_input(&draft), 30.0))];
            const FTYPES: [(&str, &str); 5] = [
                ("text", "文本"),
                ("number", "数字"),
                ("duration", "时长（秒）"),
                ("singleSelect", "单选"),
                ("image", "图片"),
            ];
            for (ft, label) in FTYPES {
                entries.push((
                    0,
                    MenuEntry::Action {
                        label: format!("添加{label}字段"),
                        icon: None,
                        danger: false,
                        disabled: false,
                        on_press: Some(Message::Table(crate::table_view::TableMsg::AddField {
                            name: draft.clone(),
                            ftype: ft.to_string(),
                        })),
                        note: None,
                        hint: None,
                    },
                ));
            }
            entries
        }
        PopupState::TableStatMenu { field_id } => {
            let fid = field_id.clone();
            let field = app
                .table
                .as_ref()
                .and_then(|s| s.data.fields.iter().find(|f| f.id == *field_id).cloned());
            let mut entries = vec![(
                0,
                MenuEntry::Action {
                    label: "无".into(),
                    icon: None,
                    danger: false,
                    disabled: false,
                    on_press: Some(Message::Table(crate::table_view::TableMsg::SetCalc {
                        field_id: fid.clone(),
                        calc: None,
                    })),
                    note: field.as_ref().and_then(|f| f.calc_type.as_deref()).is_none().then_some("当前"),
                    hint: None,
                },
            )];
            if let Some(f) = field {
                for calc in atelyx_core::table_ops::calc_types_for(&f.field_type) {
                    let is_current = f.calc_type.as_deref() == Some(*calc);
                    entries.push((
                        0,
                        MenuEntry::Action {
                            label: atelyx_core::table_ops::calc_type_label(calc).to_string(),
                            icon: None,
                            danger: false,
                            disabled: false,
                            on_press: Some(Message::Table(crate::table_view::TableMsg::SetCalc {
                                field_id: fid.clone(),
                                calc: Some(calc.to_string()),
                            })),
                            note: is_current.then_some("当前"),
                            hint: None,
                        },
                    ));
                }
            }
            entries
        }
        PopupState::TableSelectAllMenu => vec![
            (
                0,
                MenuEntry::action_icon("列宽自适应", "columns-3", Message::Table(crate::table_view::TableMsg::ColumnAutoWidth(None))),
            ),
            (
                0,
                MenuEntry::action_icon("行高自适应", "align-vertical-space-around", Message::Table(crate::table_view::TableMsg::RowHeightAuto(None))),
            ),
            (
                0,
                MenuEntry::action_icon("格式", "type", Message::Table(crate::table_view::TableMsg::ShowStyleBar)),
            ),
        ],
        PopupState::TableMoreMenu => {
            let table_open = app.ui.last_table_file.is_some();
            vec![
                (
                    0,
                    MenuEntry::Action {
                        label: "历史记录".into(),
                        icon: Some("history"),
                        danger: false,
                        disabled: !table_open,
                        on_press: app.ui.last_table_file.clone().map(|file| {
                            Message::History(crate::history_browser::HistoryBrowserMsg::Open {
                                window,
                                kind: atelyx_core::history::HistoryKind::Table,
                                file,
                            })
                        }),
                        note: None,
                        hint: (!table_open).then(|| "未打开表格".to_string()),
                    },
                ),
                (0, MenuEntry::action_icon("导出 xlsx", "download", Message::Table(crate::table_view::TableMsg::ExportXlsxPick))),
            ]
        }
        PopupState::TableFontMenu => {
            const FONTS: [(&str, &str); 8] = [
                ("default", "默认"),
                ("sans", "无衬线"),
                ("serif", "衬线"),
                ("mono", "等宽"),
                ("hei", "黑体"),
                ("song", "宋体"),
                ("kai", "楷体"),
                ("fang", "仿宋"),
            ];
            FONTS
                .iter()
                .map(|(key, label)| {
                    (
                        0,
                        MenuEntry::Action {
                            label: label.to_string(),
                            icon: None,
                            danger: false,
                            disabled: false,
                            on_press: Some(Message::Table(crate::table_view::TableMsg::SetFont(
                                (*key).to_string().into(),
                            ))),
                            note: (*key == "default").then_some("清除"),
                            hint: None,
                        },
                    )
                })
                .collect()
        }
        PopupState::TableSizeMenu => {
            const SIZES: [f64; 7] = [12.0, 13.0, 14.0, 16.0, 18.0, 20.0, 24.0];
            let mut entries = vec![(
                0,
                MenuEntry::Action {
                    label: "默认".into(),
                    icon: None,
                    danger: false,
                    disabled: false,
                    on_press: Some(Message::Table(crate::table_view::TableMsg::SetSize(None))),
                    note: Some("清除"),
                    hint: None,
                },
            )];
            for s in SIZES {
                entries.push((
                    0,
                    MenuEntry::Action {
                        label: format!("{}", s as i32),
                        icon: None,
                        danger: false,
                        disabled: false,
                        on_press: Some(Message::Table(crate::table_view::TableMsg::SetSize(Some(s)))),
                        note: None,
                        hint: None,
                    },
                ));
            }
            entries
        }
        PopupState::TableColorMenu { bg } => {
            let bg = *bg;
            let presets: [&str; 10] = if bg {
                ["#fde2e2", "#fde7d8", "#fdf0cf", "#e3f3e5", "#dcf1f4", "#e0ecfb", "#ece3fa", "#fbe3f0", "#f0e6d8", "#e8e8e8"]
            } else {
                ["#e05252", "#e07b39", "#c99a16", "#4fae6a", "#2fa3b3", "#4f8fd0", "#8a5ad0", "#d94f9c", "#8a6d4a", "#8a8a8a"]
            };
            let mut entries = vec![(
                0,
                MenuEntry::Action {
                    label: "默认".into(),
                    icon: None,
                    danger: false,
                    disabled: false,
                    on_press: Some(Message::Table(crate::table_view::TableMsg::SetColor { bg, color: None })),
                    note: Some("清除"),
                    hint: None,
                },
            )];
            for hex in presets {
                entries.push((
                    0,
                    MenuEntry::Action {
                        label: hex.to_string(),
                        icon: None,
                        danger: false,
                        disabled: false,
                        on_press: Some(Message::Table(crate::table_view::TableMsg::SetColor {
                            bg,
                            color: Some(hex.to_string()),
                        })),
                        note: None,
                        hint: None,
                    },
                ));
            }
            entries
        }
        PopupState::TableSelectMenu { row_id, field_id } => {
            let (rid, fid) = (row_id.clone(), field_id.clone());
            let current = app
                .table
                .as_ref()
                .and_then(|s| s.data.rows.iter().find(|r| r.id == *row_id))
                .and_then(|r| r.values.get(field_id))
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let options = app
                .table
                .as_ref()
                .and_then(|s| s.data.fields.iter().find(|f| f.id == *field_id))
                .and_then(|f| f.options.clone())
                .unwrap_or_default();
            let select = |rid: String, fid: String, value: Option<String>| {
                Message::Table(crate::table_view::TableMsg::SelectOption { row_id: rid, field_id: fid, value })
            };
            let mut entries = vec![(
                0,
                match current {
                    None => MenuEntry::Checked { label: "未选择".into(), on_press: select(rid.clone(), fid.clone(), None) },
                    Some(_) => MenuEntry::Action {
                        label: "未选择".into(),
                        icon: None,
                        danger: false,
                        disabled: false,
                        on_press: Some(select(rid.clone(), fid.clone(), None)),
                        note: None,
                        hint: None,
                    },
                },
            )];
            for o in options {
                let is_current = current.as_deref() == Some(o.as_str());
                entries.push((
                    0,
                    if is_current {
                        MenuEntry::Checked { label: o.clone(), on_press: select(rid.clone(), fid.clone(), Some(o)) }
                    } else {
                        MenuEntry::Action {
                            label: o.clone(),
                            icon: None,
                            danger: false,
                            disabled: false,
                            on_press: Some(select(rid.clone(), fid.clone(), Some(o))),
                            note: None,
                            hint: None,
                        }
                    },
                ));
            }
            entries
        }
        PopupState::TableLightboxMenu => vec![(
            0,
            MenuEntry::action_icon("下载图片", "download", Message::Table(crate::table_view::TableMsg::LightboxDownload)),
        )],
        PopupState::TableViewMenu => {
            let mut entries: Vec<(usize, MenuEntry)> = vec![(
                0,
                MenuEntry::Action {
                    label: "表格".into(),
                    icon: None,
                    danger: false,
                    disabled: false,
                    on_press: Some(Message::Ui(crate::ui::UiMsg::PopupClose { window })),
                    note: Some("当前"),
                    hint: None,
                },
            )];
            // 插件表格视图挂点：声明与入口已就绪，表格服务域开放前选中只给未开放提示
            let plugins: Vec<String> = crate::plugin_actions::contributions_of(app, ContributionSlot::TableView)
                .map(|c| c.label.clone())
                .collect();
            if !plugins.is_empty() {
                entries.push((0, MenuEntry::Divider));
                entries.extend(
                    plugins
                        .into_iter()
                        .map(|label| (0usize, MenuEntry::action(label, Message::NotImplemented("插件表格视图（表格服务未开放）")))),
                );
            }
            entries
        }
        PopupState::AiProviderAddMenu => {
            let mut entries: Vec<(usize, MenuEntry)> = ai::PROVIDER_PRESETS
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    (0, MenuEntry::action(p.name, Message::Ai(ai::AiMsg::ProviderAddPreset(i))))
                })
                .collect();
            entries.push((0, MenuEntry::Divider));
            entries.push((0, MenuEntry::action("自定义", Message::Ai(ai::AiMsg::ProviderAddCustom))));
            entries
        }
        PopupState::AiModelMenu { for_naming } => {
            let mut entries: Vec<(usize, MenuEntry)> = Vec::new();
            if !for_naming {
                let unset = app.repo_config.model.is_none();
                let pick = Message::Ai(ai::AiMsg::DefaultModelPick(None));
                entries.push((0, if unset {
                    MenuEntry::Checked { label: "不指定".into(), on_press: pick }
                } else {
                    MenuEntry::action("不指定", pick)
                }));
            }
            for p in app.repo_config.providers.iter().flatten() {
                if p.models.is_empty() {
                    continue;
                }
                entries.push((0, MenuEntry::Group(p.name.clone())));
                for m in &p.models {
                    let is_current = if *for_naming {
                        app.repo_config.auto_naming_model.as_ref().is_some_and(|o| o.provider_id == p.id && o.model == m.id)
                    } else {
                        app.repo_config.model.as_deref() == Some(m.id.as_str())
                            && app.repo_config.model_provider_id.as_deref() == Some(p.id.as_str())
                    };
                    let pick = Message::Ai(if *for_naming {
                        ai::AiMsg::NamingModelPick(Some((p.id.clone(), m.id.clone())))
                    } else {
                        ai::AiMsg::DefaultModelPick(Some((p.id.clone(), m.id.clone())))
                    });
                    entries.push((0, if is_current {
                        MenuEntry::Checked { label: m.id.clone(), on_press: pick }
                    } else {
                        MenuEntry::action(m.id.clone(), pick)
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
            let mut entries: Vec<(usize, MenuEntry)> = vec![(
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
                    MenuEntry::Submenu { label: "删除供应商".into(), icon: Some("trash-2"), disabled: false, pane: 1 },
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
                    MenuEntry::Submenu { label: "删除 Agent".into(), icon: Some("trash-2"), disabled: false, pane: 1 },
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
        // 插件菜单 / 下拉节点：条目由插件界面树带来（选中项带「当前」角标）
        PopupState::PluginList { entries } => entries
            .iter()
            .map(|entry| {
                (
                    0usize,
                    MenuEntry::Action {
                        label: entry.label.clone(),
                        icon: None,
                        danger: entry.danger,
                        disabled: entry.disabled,
                        on_press: (!entry.disabled).then(|| entry.on_press.clone()),
                        note: entry.current.then_some("当前"),
                        hint: None,
                    },
                )
            })
            .collect(),
    };
    crate::ui::menu::pane_view(entries, pane, window)
}

/// 表格弹层内联输入（单行；提交由菜单项动作携带草稿）。
fn popup_text_input<'a>(draft: &str) -> Element<'a, Message> {
    iced::widget::text_input("名称", draft)
        .size(pal::fs_caption())
        .padding(Padding::ZERO.horizontal(12.0).vertical(6.0))
        .on_input(|v: String| Message::Table(crate::table_view::TableMsg::PopupDraft(v)))
        .style(|_theme, status| controls::form_input_style(status))
        .into()
}

fn popup_hint(text_content: &str) -> Element<'static, Message> {
    container(iced::widget::text(text_content.to_string()).size(pal::fs_micro()).color(pal::TEXT_MUTED))
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

    // 确认面板：标题 medium，按钮区与文案间距 12
    let confirm_entries: Vec<(usize, MenuEntry<'static>)> = if let Some(id) = confirm {
        let scene = app.ui.scenes.iter().find(|s| s.id == *id);
        let name = scene.map(|s| s.name.clone()).unwrap_or_default();
        let count = scene.map(|s| s.layouts.len()).unwrap_or(0);
        let pane = column![
            text(format!("删除场景「{name}」？"))
                .size(pal::fs_caption())
                .color(pal::TEXT_PRIMARY)
                .font(iced::Font { weight: iced::font::Weight::Medium, ..iced::Font::default() }),
            text(format!("其下 {count} 套布局将一并删除。"))
                .size(pal::fs_caption())
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
fn panel_menu_entries(app: &App, panel_id: &str) -> Vec<(usize, MenuEntry<'static>)> {
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
fn view_picker_entries(app: &App, panel_id: &str) -> Vec<(usize, MenuEntry<'static>)> {
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

/// 最近仓库行菜单：在系统文件管理器中打开 / 从列表移除（激活仓库行不可移除）。
/// 移除进二段确认 pane（只移出列表，不删磁盘文件）。
fn vault_menu_entries(app: &App, root: &str) -> Vec<(usize, MenuEntry<'static>)> {
    let active = app
        .vault_root
        .as_ref()
        .map(|r| r.to_string_lossy().into_owned())
        .as_deref()
        == Some(root);
    let mut entries: Vec<(usize, MenuEntry)> = vec![
        (0, MenuEntry::action_icon(
            "在文件管理器中打开",
            "folder-open",
            Message::RevealVaultAt(root.to_string()),
        )),
        (0, MenuEntry::Divider),
        (0, MenuEntry::Action {
            label: "从列表移除".to_string(),
            icon: Some("unlink"),
            danger: true,
            disabled: active,
            on_press: (!active).then(|| Message::RemoveVaultAsk(root.to_string())),
            note: active.then_some("当前仓库"),
            hint: None,
        }),
    ];
    if app.remove_vault_confirm.as_deref() == Some(root) {
        let pane = column![text("只从列表移除，不影响磁盘文件")
            .size(pal::fs_caption())
            .color(pal::TEXT_MUTED)];
        entries.push((1, MenuEntry::Custom(pane.into(), 12.0 + 16.0 + 4.0)));
        entries.push((1, MenuEntry::action_icon(
            "确认移除",
            "unlink",
            Message::RemoveVaultConfirm(root.to_string()),
        )));
        entries.push((1, MenuEntry::action("取消", Message::RemoveVaultCancel)));
    }
    entries
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
fn space_menu_entries(app: &App, window: iced::window::Id) -> Vec<(usize, MenuEntry<'static>)> {
    let mut entries: Vec<(usize, MenuEntry)> = Vec::new();
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
    entries
}

/// 协作登录 / 注册表单（地址 + 账号 + 密码 + 设备名）。
fn space_login_entries(
    app: &App,
    register: bool,
) -> Vec<(usize, MenuEntry<'static>)> {
    let form = &app.space_form;
    let field = |f: crate::SpaceField| move |v: String| Message::Space(crate::SpaceMsg::LoginField(f, v));
    let mut entries: Vec<(usize, MenuEntry)> = vec![
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
        .padding(Padding::ZERO.horizontal(12.0).vertical(6.0))
        .on_input(on_input)
        .style(|_theme, status| controls::form_input_style(status));
    if secure { input.secure(true).into() } else { input.into() }
}

/// 弹层表单的说明行。
fn space_hint(text_content: &str) -> Element<'static, Message> {
    container(iced::widget::text(text_content.to_string()).size(pal::fs_micro()).color(pal::TEXT_MUTED))
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
) -> Vec<(usize, MenuEntry<'static>)> {
    use atelyx_core::space_client::SpaceRole;
    let mut entries: Vec<(usize, MenuEntry)> = Vec::new();
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
) -> Vec<(usize, MenuEntry<'static>)> {
    use atelyx_core::space_client::SpaceRole;
    let mut entries: Vec<(usize, MenuEntry)> = vec![
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

/// 文件树右键菜单：文件夹 / 文件 / 根空白；删除 = 应用层确认流
/// （文件进确认 pane；文件夹先计数——空目录直接删、非空带条目数进确认 pane）。
fn file_menu_entries(
    app: &App,
    rel: &str,
    is_dir: bool,
    is_root: bool,
) -> Vec<(usize, MenuEntry<'static>)> {
    let mut entries: Vec<(usize, MenuEntry)> = Vec::new();
    if is_dir {
        entries.push((0, MenuEntry::action_icon("新建画布", "file-text", Message::NotImplemented("画布"))));
        entries.push((0, MenuEntry::action_icon("新建笔记", "sticky-note",
            Message::DraftStart(rel.to_string(), DraftKind::Note))));
        entries.push((0, MenuEntry::action_icon("新建表格", "table", Message::DraftStart(rel.to_string(), crate::DraftKind::Table))));
        entries.push((0, MenuEntry::action_icon("新建文件夹", "folder-plus",
            Message::DraftStart(rel.to_string(), DraftKind::Folder))));
        if !is_root {
            entries.push((0, MenuEntry::Divider));
            entries.push((0, MenuEntry::Submenu {
                label: "图标颜色".into(),
                icon: Some("palette"),
                disabled: false,
                pane: 2,
            }));
            entries.push((0, MenuEntry::Divider));
            entries.push((0, MenuEntry::action_icon("创建副本", "copy",
                Message::DuplicateEntry(rel.to_string(), true))));
            entries.push((0, MenuEntry::Divider));
            entries.push((0, MenuEntry::action_icon("重命名", "pencil",
                Message::RenameStartFolder(rel.to_string()))));
            entries.push((0, MenuEntry::danger("删除", "trash-2",
                Message::FolderDeleteAsk(rel.to_string()))));
        }
    } else {
        let lower = rel.to_lowercase();
        if lower.ends_with(".canvas") {
            entries.push((0, MenuEntry::action_icon("转换为画布", "file-output", Message::NotImplemented("画布转换"))));
            entries.push((0, MenuEntry::Divider));
        }
        if lower.ends_with(".md") {
            // 标记状态决定条目形态（注册/注销）；副本不继承标记；viewer 只读降级（与 AI 设置同口径）
            let viewer = matches!(
                &app.repo_kind,
                crate::RepoKind::Space(s) if !s.role.can_write()
            );
            let marked = app.prompt_notes.iter().any(|n| n == rel);
            entries.push((0, MenuEntry::Action {
                label: if marked { "注销提示词" } else { "注册为提示词" }.into(),
                icon: Some(if marked { "bookmark-minus" } else { "bookmark-plus" }),
                danger: false,
                disabled: viewer,
                on_press: (!viewer).then(|| Message::PromptToggle(rel.to_string())),
                note: None,
                hint: viewer.then(|| "当前角色为只读，无法修改提示词".to_string()),
            }));
            entries.push((0, MenuEntry::Divider));
        }
        entries.push((0, MenuEntry::action_icon("创建副本", "copy",
            Message::DuplicateEntry(rel.to_string(), false))));
        entries.push((0, MenuEntry::Divider));
        entries.push((0, MenuEntry::action_icon("重命名", "pencil",
            Message::RenameStartFile(rel.to_string()))));
        entries.push((0, MenuEntry::danger("删除", "trash-2",
            Message::DeleteAsk(rel.to_string()))));
    }

    // pane 2 = 图标颜色色板（预设色 + 当前标注 + 恢复默认）
    if is_dir && !rel.is_empty() {
        const PRESETS: [&str; 8] = [
            "#e05252", "#e07b39", "#e0b436", "#4fae6a", "#2f9e8f", "#4f8fd0", "#7a6fd0", "#c05fa8",
        ];
        let current = app.folder_colors.get(rel);
        for hex in PRESETS {
            let Some(color) = parse_hex_color(hex) else { continue };
            let is_current = current == Some(&hex.to_string());
            let swatch = container(space().width(Length::Fixed(12.0)).height(Length::Fixed(12.0)))
                .style(move |_t| Style {
                    background: Some(color.into()),
                    // 当前色 = 强调描边加粗（子面板无行尾角标，选中态画在色块上）
                    border: border::rounded(3.0)
                        .color(if is_current { pal::ACCENT } else { pal::BORDER })
                        .width(if is_current { 2.0 } else { 1.0 }),
                    ..Style::default()
                });
            let row = row![
                swatch,
                text(hex.to_string()).size(pal::fs_micro()).color(pal::TEXT_SECONDARY).font(mono()),
            ]
            .spacing(8)
            .padding(Padding::ZERO.horizontal(12.0))
            .width(Length::Fill)
            .align_y(Vertical::Center);
            entries.push((2, MenuEntry::Custom(iced::widget::button(row)
                .width(Length::Fill)
                .style(|_t, status| button::Style {
                    background: matches!(status, button::Status::Hovered | button::Status::Pressed)
                        .then_some(pal::HOVER.into()),
                    ..Default::default()
                })
                .on_press(Message::FolderColorSet(rel.to_string(), Some(hex.to_string())))
                .into(), pal::MENU_ITEM_H)));
        }
        entries.push((2, MenuEntry::action_icon("恢复默认", "rotate-ccw",
            Message::FolderColorSet(rel.to_string(), None))));
    }

    // pane 1 = 删除就地确认（无标题行：说明文字 + 菜单项形态的确认/取消；
    // 内容来自应用层 delete_confirm——文件夹计数完成前 pane 1 不渲染）
    if !rel.is_empty() {
        if let Some((r, dir, count)) = app
            .delete_confirm
            .as_ref()
            .filter(|(cr, _, _)| cr == rel)
        {
            let mut height = 12.0 + 16.0 + 4.0;
            let mut pane_col = column![].spacing(2).padding(Padding::ZERO.vertical(6.0)).width(Length::Fill);
            if let Some(n) = count {
                pane_col = pane_col.push(
                    text(format!("文件夹包含 {n} 个文件/文件夹"))
                        .size(pal::fs_caption())
                        .color(pal::TEXT_SECONDARY),
                );
                height += 16.0;
            }
            pane_col = pane_col.push(
                text("删除后不可恢复")
                    .size(pal::fs_caption())
                    .color(pal::TEXT_MUTED),
            );
            entries.push((1, MenuEntry::Custom(pane_col.into(), height)));
            entries.push((1, MenuEntry::danger("确认删除", "trash-2",
                Message::DeleteEntry(r.to_string(), *dir))));
            entries.push((1, MenuEntry::action("取消", Message::DeleteCancel)));
        }
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

/// 枚举全部窗口的可停靠面板矩形（拖拽会话期间位置尺寸视为稳定）。
/// 指定窗口当前是否渲染表格视图（活动标签 view = "table"；表格视图全局唯一，同时只在一窗渲染）。
pub fn window_shows_table(app: &App, window: iced::window::Id) -> bool {
    let active_is_table = |tabs: &[atelyx_core::layout::TabItem], active: Option<&str>| {
        active
            .and_then(|tid| tabs.iter().find(|t| t.id == tid))
            .is_some_and(|t| t.view == "table")
    };
    if app.main_id == Some(window) {
        fn scan(node: &atelyx_core::layout::LayoutNode) -> bool {
            match node {
                atelyx_core::layout::LayoutNode::Panel { tabs, active_tab_id, .. } => {
                    active_tab_id
                        .as_deref()
                        .and_then(|tid| tabs.iter().find(|t| t.id == tid))
                        .is_some_and(|t| t.view == "table")
                }
                atelyx_core::layout::LayoutNode::Split { children, .. } => children.iter().any(scan),
            }
        }
        scan(&atelyx_core::layout::active_layout(&app.ui).tree)
    } else {
        app.detached_os
            .get(&window)
            .and_then(|model_id| app.ui.detached_windows.iter().find(|w| &w.id == model_id))
            .is_some_and(|w| active_is_table(&w.tabs, w.active_tab_id.as_deref()))
    }
}

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
                                (t.view == "note" && app.ui.last_note_file.is_some())
                                    || (t.view == "table" && app.ui.last_table_file.is_some()),
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
                        (t.view == "note" && app.ui.last_note_file.is_some())
                            || (t.view == "table" && app.ui.last_table_file.is_some()),
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
