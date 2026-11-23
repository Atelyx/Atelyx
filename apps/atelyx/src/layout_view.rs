//! 工作区 UI：标题栏（布局 tab / 场景选择器 / 窗口控制）+ 面板网格 + 状态栏
//! + 设置整页 + 弹层内容（菜单/选择器/就地确认）+ files/note 视图与占位视图。
//!
//! 几何计算与渲染共用同一套度量（UiMetrics）：把手/落点命中矩形由布局树 +
//! 窗口内容区尺寸纯推导，与 iced FillPortion 的分配规则保持一致。

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use iced::alignment::{Horizontal, Vertical};
use iced::widget::text::{LineHeight, Wrapping};
use iced::widget::{button, column, container, mouse_area, row, scrollable, space, stack, text_editor, text_input};
use iced::{Color, Element, Length, Padding, Point, Rectangle, Size, border, widget::container::Style};

use atelyx_core::global_config::RecentVault;
use atelyx_core::layout::{self, AppUiState, LayoutNode, TabItem};
use atelyx_core::open_files::{DocKind, Host};

use crate::hover_tip::with_tip;
use crate::ui::controls::{self, Variant};
use crate::ui::icon::{icon, icon_stroke};
use crate::ui::palette as pal;
use crate::ui::{mono, text};
use crate::ui::popup::{self as ui_popup, PopupSpec};
use crate::ui::notify;
use crate::ui::scroll;
use crate::ui::text::{ellipsize, ellipsize_estimated, estimate_width, measure};
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

/// 视图显示名（面板头标签）：注册表的一条查询——内置与插件条目同一条路径。
/// 未在册（贡献随插件停用消失）回落到插件 id。
pub fn view_label(app: &App, kind: &str) -> String {
    app.registry.label(kind)
}

/// 视图图标（壳内图标名）：注册表的一条查询；未在册回落拼图。
pub fn view_icon(app: &App, kind: &str) -> Cow<'static, str> {
    app.registry.icon(kind)
}

/// 可选视图 kind 清单：注册表里启用中的面板视图（登记序：内置在前、插件在后）。
pub fn available_view_kinds(app: &App) -> Vec<String> {
    app.registry.views().map(|entry| entry.id.clone()).collect()
}

/// 面板标签文字（文档槽 = 当前文件的名字；工具视图 = 视图名）。
pub fn tab_label(app: &App, tab: &TabItem) -> String {
    crate::open_route::tab_label(app, tab)
}

/// 面板标签近似宽度（拖拽命中几何用；渲染宽度由内容自适应，见 panel_tab）。
fn approx_tab_width(label: &str, locked: bool, has_file: bool) -> f32 {
    let text_w = estimate_width(label, 12.0).min(UiMetrics::TAB_TEXT_MAX_W);
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
    /// 输入框所在窗口：失焦提交与 Esc 取消按该窗口判定（页面内标题可在撕裂窗口就地改名）。
    pub window: iced::window::Id,
    /// 由页面内标题发起：输入框渲染在正文列标题处，文件树对应行不再渲染输入框。
    pub from_title: bool,
}

/// 重名去重提醒条：文案 + 出现时刻（3s 过期）+ 渲染位置（标题发起 = 贴标题，否则在文件树）。
pub struct RenameNotice {
    pub text: String,
    pub at: std::time::Instant,
    pub at_title: bool,
}

/// 弹层内容形态（几何与交互数据随形态携带）。
#[derive(Clone, Debug)]
pub(crate) enum PopupState {
    SceneSelector { confirm: Option<String> },
    LayoutTabMenu { layout_id: String },
    PanelMenu { panel_id: String },
    TabContext { panel_id: String, tab_id: String },
    /// 主要面板投影标签右键（id = 打开文件实例；菜单只有关闭 / 定位 / 复制路径）。
    PrimaryTabContext { instance_id: String },
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
    /// 属性面板类型切换菜单（属性键名）。
    PropTypeMenu { key: String },
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
    /// 画布：卡右键。
    CanvasCardMenu { node_id: String },
    /// 画布：分组右键（改色 / 改名 / 收起展开）。
    CanvasGroupMenu { node_id: String },
    /// 画布：多选右键（批量删除 / 复制 / 编组）。
    CanvasMultiMenu,
    /// 画布：空白右键（粘贴 / 新建分组）。
    CanvasBlankMenu { x: f64, y: f64 },
    /// 画布：分组改名（内联输入）。
    CanvasGroupRename { node_id: String },
    /// 表格：状态栏计算类型。
    TableStatMenu { field_id: String },
    /// 表格：全选角钮右键。
    TableSelectAllMenu,
    /// 表格：··· 菜单（历史 / 导出）。
    TableMoreMenu,
    /// 字体选择弹层（for_table = 选表格单元格字体，否则选界面字体）。
    FontPicker { for_table: bool },
    /// 表格：格式气泡下拉（字号 / 字色 / 底色）。
    TableSizeMenu,
    /// 取色器弹层（表格字色/底色、文件夹图标色、协作展示色）。
    ColorPicker {
        target: crate::color_picker_actions::ColorTarget,
        hsv: crate::ui::color_picker::Hsv,
        hex: String,
    },
    /// 表格：单选下拉（行 + 字段）。
    TableSelectMenu { row_id: String, field_id: String },
    /// 表格：灯箱图片右键菜单（下载）。
    TableLightboxMenu,
    /// 表格：工具条视图切换下拉（内置表格）。
    TableViewMenu,
    /// 设置-AI：话题命名模型下拉。
    AiModelMenu,
    /// AI 对话：模型 / 推理等级两级菜单；scope = 写入目标（面板覆盖或画布卡节点配置）。
    ChatModelMenu { scope: crate::chat_composer::ConfigScope },
    /// AI 对话：输入区「+」菜单（添加上下文 / 上传附件）；scope = 草稿作用域。
    ChatPlusMenu { scope: crate::chat_composer::ComposerScope },
    /// AI 对话：Agent 下拉（scope = 写入目标；缺省「对话」）。
    ChatAgentMenu { scope: crate::chat_composer::ConfigScope },
    /// 设置-AI：话题自动命名模式。
    AiNamingMenu,
    /// 设置-AI：默认模型下拉。
    AiDefaultModelMenu,
    /// 设置-AI：Agent 系统提示词选择。
    AiAgentPromptMenu,
    /// 设置-AI：删除供应商二段确认（pane 1 = 确认）。
    AiProviderDeleteMenu,
    /// 设置-AI：删除 Agent 二段确认（pane 1 = 确认）。
    AiAgentDeleteMenu { agent_id: String },
    /// 设置-AI：搜索源下拉。
    AiSearchProviderMenu,
    /// 历史记录弹层（居中模态；kind+file 定位侧文件）。
    HistoryBrowser { kind: atelyx_core::history::HistoryKind, file: String },
    /// AI 字段配置弹层（居中模态；字段 id 定位）。
    TableAiField { field_id: String },
    /// 只读正文 / 对话消息的选区右键菜单（复制）。
    SelectionMenu { target: crate::markdown_select::SelectionTarget },
}

impl PopupState {
    /// 宽度随各域渲染体就近定义（popup/<域>::width），这里只保留入口。
    fn width(&self) -> f32 {
        crate::popup::width(self)
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

/// 光标锚定的弹层打开消息（表单下拉统一入口；锚点与翻转口径同按钮弹层）。
pub(crate) fn open_popup(app: &App, window: iced::window::Id, state: PopupState) -> Message {
    btn_popup(window, app.cursor_for(window), state)
}

/// 右键菜单类弹层：锚点 = 光标点，无翻转。
fn ctx_popup(window: iced::window::Id, cursor: Point, state: PopupState) -> Message {
    Message::PopupOpen(window, PopupOpen { point: cursor, flip_y: None, state, min_width: 0.0 })
}

/// 只读正文右键：选区复制菜单（锚点 = 光标点）。
pub(crate) fn selection_popup(
    window: iced::window::Id,
    cursor: Point,
    target: crate::markdown_select::SelectionTarget,
) -> Message {
    ctx_popup(window, cursor, PopupState::SelectionMenu { target })
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

    let base = workspace_chrome(app, window, content);

    with_overlays(app, window, base)
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
                    text(ellipsize_estimated(title, PLUGIN_TITLEBAR_TITLE_W, pal::fs_caption()))
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

/// 工作区 chrome 骨架（标题栏 + 分隔线 + 内容 + 分隔线 + 状态栏）。
fn workspace_chrome<'a>(
    app: &'a App,
    window: iced::window::Id,
    content: Element<'a, Message>,
) -> Element<'a, Message> {
    column![
        titlebar(app, window),
        controls::divider(false, pal::BORDER, Length::Fill),
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

    // 表格格式气泡：叠在面板之上、弹层之下——宽度只受窗口约束，越出面板也不被压扁
    if let Some(el) = crate::table_view::format_bar_overlay(app, window, viewport) {
        root = root.push(el);
    }

    // 双链候选：锚在正文光标右下方，须叠在正文滚动区之外（滚动区会裁掉越界内容）
    if let Some(el) = wiki_picker_overlay(app, window) {
        root = root.push(el);
    }

    // 属性候选：贴编辑槽输入框弹出（不参与面板布局，属性多时不把正文挤到 0）
    if let Some(el) = crate::properties_view::suggest_overlay(app, window, viewport) {
        root = root.push(el);
    }

    // 弹层（同窗口至多一个；捕获层点击 = 外点关闭；居中模态另行渲染，见下方）
    if let Some((w, open)) = &app.popup {
        if *w == window
            && !matches!(
                open.state,
                PopupState::HistoryBrowser { .. } | PopupState::TableAiField { .. }
            )
        {
            let (content, height) = crate::popup::element(app, &open.state, app.popup_pane, window);
            // 输入盒底行的触发钮菜单（+ / Agent / 模型）恒贴钮上方展开：面板底缘与画布卡
            // 下方的可读区都在钮上方。用硬边（放不下时不换边），模型菜单换子面板时高度变化不跳边。
            let flip = match &open.state {
                PopupState::ChatPlusMenu { .. }
                | PopupState::ChatAgentMenu { .. }
                | PopupState::ChatModelMenu { .. } => ui_popup::VFlip::Above,
                _ => ui_popup::VFlip::BelowFirst,
            };
            let spec = PopupSpec {
                anchor: open.point,
                width: open.state.width(),
                height,
                flip_y: open.flip_y,
                min_width: open.min_width,
            };
            let geom = ui_popup::resolve_preferring(&spec, viewport, flip);
            root = root.push(ui_popup::overlay(
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

    // AI 字段配置模态（与历史记录同级）
    if let Some(el) = crate::table_ai_view::overlay(app, window, viewport) {
        root = root.push(el);
    }

    // 插件页模态（详情 / 市场安装确认）
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
            let tip_w = (estimate_width(label, pal::fs_micro()) + 16.0).min(260.0);
            let tip_h = 16.0 + 8.0;
            let half = app.tooltip_half.unwrap_or(14.0);
            let spec = PopupSpec {
                anchor: Point::new(pos.x, pos.y + half + 4.0),
                width: tip_w,
                height: tip_h,
                flip_y: Some(pos.y - half - 4.0),
                min_width: 0.0,
            };
            let geom = ui_popup::resolve(&spec, viewport);
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
                        icon_stroke(&view_icon(app, &drag.view), 13.0, pal::TEXT_PRIMARY, 2.0),
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
                // 表格图片格落点与目录落点互斥（同一时刻只会有一个悬停区）
                let image_hint = app
                    .table
                    .as_ref()
                    .and_then(|s| s.view.image_drop.as_ref())
                    .filter(|t| t.window == window)
                    .map(|_| "添加到图片单元格".to_string());
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
                    }).or(image_hint),
                };
                let reject = reject.is_some();
                let mut card = column![
                    row![
                        icon_stroke(&icon_name, 13.0, pal::TEXT_PRIMARY, 2.0),
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

/// 窗口视口尺寸（弹层 / 格式气泡钳制与翻转判据的基准）。
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
    let brand_w = 8.0 + 16.0 + 6.0 + estimate_width(BRAND_TEXT, 12.0) + 8.0;
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
    (estimate_width(name, 12.0) + 20.0).clamp(48.0, 120.0)
}

/// 布局 tab 改名编辑态宽度：按当前输入随内容增长，钳制 140–260（提交后收回正常宽度）。
fn layout_tab_edit_width(value: &str) -> f32 {
    (estimate_width(value, 12.0) + 20.0).clamp(140.0, 260.0)
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
    let trigger_w = estimate_width(&scene_name, 12.0) + 16.0 + 4.0 + 12.0;
    let selector = button(
        container(
            row![
                text(ellipsize_estimated(&scene_name, 120.0, pal::fs_caption()))
                    .size(pal::fs_caption())
                    .color(pal::TEXT_SECONDARY),
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
            .width(Length::Fixed(estimate_width(BRAND_TEXT, 12.0))),
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
    let display = ellipsize_estimated(name, UiMetrics::TAB_TEXT_MAX_W, 12.0);
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
        with_tip(area, window, name.to_string(), 28.0)
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
            .push(controls::divider(true, pal::BORDER, Length::Fixed(12.0)))
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
        LayoutNode::Panel { id, tabs, active_tab_id } => {
            if layout::primary_panel_of_active(&app.ui) == Some(id.as_str()) {
                // 主要面板标签 = 打开文件集合的投影（模型里没有标签）
                let projected = crate::open_route::primary_tabs(app);
                let active = crate::open_route::primary_active(app);
                panel_view(app, id, &projected, &active, true)
            } else {
                panel_view(app, id, tabs, active_tab_id, false)
            }
        }
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
/// 主要面板的标签由打开文件集合投影生成（`tabs` 为投影结果，模型里没有标签）。
fn panel_view<'a>(
    app: &'a App,
    panel_id: &str,
    tabs: &[TabItem],
    active_tab_id: &Option<String>,
    primary: bool,
) -> Element<'a, Message> {
    let window = app.main_id.unwrap_or(iced::window::Id::unique());
    let mut tab_row = row![].spacing(2).height(Length::Fill);
    // 拖拽插入条：命中本面板标签条时在插入位渲染 2px 强调竖条（随行内元素移位）
    let insert_at = if primary {
        None
    } else {
        app.drag.as_ref().and_then(|d| {
            let hover = app.drag_hover.as_ref()?;
            (hover.zone.panel_id.as_deref() == Some(panel_id) && hover.kind == DropZone::Tab)
                .then(|| {
                    let scroll_x = app.panel_tab_scroll_x.get(panel_id).copied().unwrap_or(0.0);
                    tab_index_at(&hover.zone, d.current.x, scroll_x)
                })
        })
    };
    for (i, t) in tabs.iter().enumerate() {
        if insert_at == Some(i) {
            tab_row = tab_row.push(insertion_marker());
        }
        tab_row = tab_row.push(panel_tab(app, panel_id, t, active_tab_id, primary));
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
        container(controls::divider(false, pal::BORDER, Length::Fill))
            .height(Length::Fill)
            .align_y(Vertical::Bottom),
        {
            let mut head = row![].height(Length::Fill).align_y(Vertical::Center);
            if primary {
                head = head.push(primary_badge());
            }
            head.push(
                scrollable(container(row![tab_row]).padding(Padding::ZERO.left(UiMetrics::HEADER_INSET)))
                    .direction(scrollable::Direction::Horizontal(scroll::hidden()))
                    .on_scroll({
                        let panel_id = panel_id.to_string();
                        move |v| Message::TabStripScrolled(Some(panel_id.clone()), v.absolute_offset().x)
                    })
                    .width(Length::Fill)
                    .height(Length::Fill),
            )
            .push(right)
            .height(Length::Fixed(UiMetrics::TAB_BAR_H))
        },
    ]
    .width(Length::Fill)
    .height(Length::Fixed(UiMetrics::TAB_BAR_H));
    // 头部空白右键：主要面板只有文档集会承载，右键不出视图选择器（普通面板仍走视图选择菜单）
    let cursor = app.cursor_for(window);
    let header: Element<'a, Message> = if primary {
        mouse_area(header).into()
    } else {
        mouse_area(header)
            .on_right_press(ctx_popup(
                window,
                cursor,
                PopupState::ViewPicker { panel_id: panel_id.to_string() },
            ))
            .into()
    };

    let content: Element<'a, Message> = match active {
        // 主要面板空集合 = 空态（打开文件）；普通空面板 = 纯空白（右键唤起视图选择器）
        None if primary => controls::empty_state(
            "files",
            "没有打开的文件",
            "从文件面板打开笔记、表格或画布",
            Some(("打开文件".to_string(), Message::OpenFilePick)),
        )
        .into(),
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

/// 主要面板标识：左侧强调竖条 + 「主要面板」小标题（固定在标签条最左，不随标签滚动）。
fn primary_badge() -> Element<'static, Message> {
    container(
        row![
            container(space())
                .width(Length::Fixed(3.0))
                .height(Length::Fixed(14.0))
                .style(|_t| Style {
                    background: Some(pal::ACCENT.into()),
                    border: border::rounded(1.0),
                    ..Style::default()
                }),
            text("主要面板").size(pal::fs_micro()).color(pal::TEXT_MUTED),
        ]
        .spacing(6)
        .align_y(Vertical::Center),
    )
    .padding(Padding::ZERO.left(6.0).right(4.0))
    .height(Length::Fill)
    .align_y(Vertical::Center)
    .into()
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

/// 标签外框：视图图标 + 省略后的标题 + 锁标记 + 关文件 ×，激活时内容同色底并以 2px 金线压底。
/// 关闭动作由调用方给（主窗面板、主要面板投影与撕裂窗各有关文件消息）。
fn tab_button<'a>(
    app: &'a App,
    view: &str,
    label: String,
    has_file: bool,
    locked: bool,
    active: bool,
    close: Message,
) -> iced::widget::Button<'a, Message> {
    let text_color = if active { pal::TEXT_PRIMARY } else { pal::TEXT_SECONDARY };
    let mut label_row = row![].spacing(4);
    label_row = label_row.push(icon_stroke(&view_icon(app, view), 13.0, text_color, 2.0));
    label_row = label_row.push(
        text(ellipsize_estimated(&label, UiMetrics::TAB_TEXT_MAX_W, 12.0))
            .size(pal::fs_caption())
            .color(text_color),
    );
    if locked {
        label_row = label_row.push(icon("lock", 9.0, text_color));
    }
    if has_file && !locked {
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
                .on_press(close),
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
}

/// 面板标签（激活 = 内容区同色底 + 2px 金下边盖线；打开文件的 × = 关文件留标签）。
/// 主要面板的标签是集合投影：按下/右键走实例通道，不做标签拖拽。
fn panel_tab<'a>(
    app: &'a App,
    panel_id: &str,
    t: &TabItem,
    active_tab_id: &Option<String>,
    primary: bool,
) -> Element<'a, Message> {
    let active = active_tab_id.as_deref() == Some(t.id.as_str());
    let window = app.main_id.unwrap_or(iced::window::Id::unique());
    let cursor = app.cursor_for(window);
    let label = tab_label(app, t);
    let has_file = crate::open_route::tab_has_file(app, t);
    let close = if primary {
        Message::DocInstanceClose(t.id.clone())
    } else {
        Message::PanelTabFileClose(t.view.clone())
    };
    let btn = tab_button(app, &t.view, label, has_file, t.locked, active, close).on_press_maybe(
        (!active).then(|| {
            if primary {
                Message::DocInstanceActivate(t.id.clone())
            } else {
                Message::PanelTab(panel_id.to_string(), t.id.clone())
            }
        }),
    );
    let menu = if primary {
        PopupState::PrimaryTabContext { instance_id: t.id.clone() }
    } else {
        PopupState::TabContext { panel_id: panel_id.to_string(), tab_id: t.id.clone() }
    };
    let area = mouse_area(btn).on_right_press(ctx_popup(window, cursor, menu));
    if primary {
        return area.into();
    }
    // 按下候选经 PressReporter 上报（mouse_area 对被按钮捕获的按下事件不触发）；
    // 锁定标签右键仍开菜单（iced mouse_area 无 maybe 变体），条目全部禁用表达锁定语义
    controls::PressReporter::new(
        area.into(),
        Message::PanelTabPress {
            panel_id: panel_id.to_string(),
            tab_id: t.id.clone(),
        },
    )
    .into()
}

/// 笔记保存状态徽标：错误 = ErrorPill（红调底 + 重试/关闭），未保存 = 圆点徽标。
fn note_status_pill(app: &App) -> Option<Element<'static, Message>> {
    app.open_files.recent_of_kind(&DocKind::Note, &Host::Main)?;
    if app.save_error.is_some() {
        Some(
            container(
                row![
                    text(ellipsize_estimated(
                        app.save_error.as_deref().unwrap_or("保存失败"),
                        160.0,
                        11.0,
                    ))
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

/// 按视图 kind 分派内容（壳内原生视图按 id 查渲染表；插件面板按整面帧渲染）。
fn view_content<'a>(app: &'a App, window: iced::window::Id, kind: &str) -> Element<'a, Message> {
    if crate::plugin_actions::parse_view_kind(kind).is_some() {
        return crate::plugin_view::panel_view(app, window, kind);
    }
    crate::pack::render(kind, app, window).unwrap_or_else(|| placeholder_view(app, kind))
}

/// 协作房间视图：连接状态头部 + 空间成员名册 + 在线设备（打开文件可点开）+ 设置入口。
pub(crate) fn collab_room_view(app: &App, _window: iced::window::Id) -> Element<'_, Message> {
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
                        col = col.push(controls::card(
                            column![
                                row![
                                    text(m.display_name.clone())
                                        .size(pal::fs_body())
                                        .color(pal::TEXT_PRIMARY)
                                        .font(crate::ui::bold()),
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
                            Padding::ZERO.horizontal(10.0).vertical(6.0),
                        ));
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
                app,
                app.global
                    .last_identity
                    .as_ref()
                    .map(|i| i.display_name.clone())
                    .unwrap_or_else(|| "用户".into()),
                app.global
                    .collab_color
                    .clone()
                    .unwrap_or_else(|| crate::space_actions::DEFAULT_COLLAB_COLOR.into()),
                Some(crate::space_actions::client_platform().to_string()),
                Some(crate::space_actions::CLIENT_VERSION.to_string()),
                crate::space_actions::open_file_paths(app),
                true,
            ));
            for peer in merge_peers(&app.space_peers) {
                col = col.push(device_row(
                    app,
                    peer.nickname,
                    peer.color,
                    peer.platform,
                    peer.version,
                    peer.files,
                    false,
                ));
            }
            if app.space_peers.is_empty() {
                col = col.push(
                    text(if app.space_connected { "当前空间没有其他设备在线" } else { "连接中…" })
                        .size(pal::fs_micro())
                        .color(pal::TEXT_MUTED),
                );
            }
        }
        // 未进空间：整面板居中空态（不进滚动列，否则只按固有高度贴顶）
        crate::RepoKind::Local => {
            return controls::empty_state(
                "users",
                "未进入协作空间",
                "登录并进入空间后在这里查看成员与在线设备",
                Some(("协作空间".into(), Message::SpaceMenuOpen)),
            );
        }
    }
    scroll::v(container(col).width(Length::Fill).height(Length::Fill))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// 在线设备卡片：色点 + 昵称（自己标「我」）一行，平台 · 版本次行，打开文件清单可点击。
fn device_row(
    app: &App,
    nickname: String,
    color_hex: String,
    platform: Option<String>,
    version: Option<String>,
    files: Vec<String>,
    is_self: bool,
) -> Element<'static, Message> {
    let color = parse_hex_color(&color_hex).unwrap_or(pal::TEXT_MUTED);
    let mut head = row![
        controls::dot(color, 8.0),
        text(nickname).size(pal::fs_body()).color(pal::TEXT_PRIMARY).font(crate::ui::bold()),
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
        text(device_meta_line(platform.as_deref(), version.as_deref()))
            .size(pal::fs_micro())
            .color(pal::TEXT_MUTED),
    );
    if files.is_empty() {
        col = col.push(text("暂无打开文件").size(pal::fs_micro()).color(pal::TEXT_MUTED));
    } else {
        for rel in files {
            let name = rel.rsplit('/').next().unwrap_or(&rel).to_string();
            let icon_key = file_icon(app, &name);
            col = col.push(
                button(
                    row![
                        icon(&icon_key, 11.0, pal::TEXT_SECONDARY),
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
    controls::card(col, Padding::ZERO.horizontal(10.0).vertical(6.0))
}

/// 平台 · 版本展示行：旧端未上报的字段显式标注未知。
fn device_meta_line(platform: Option<&str>, version: Option<&str>) -> String {
    let platform = platform
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(crate::space_actions::platform_label)
        .unwrap_or_else(|| "未知平台".into());
    let version = version.map(str::trim).filter(|s| !s.is_empty()).unwrap_or("未知版本");
    format!("{platform} · {version}")
}

/// 合并后的在线设备行（`device_name` 仅作合并键，不进卡片）。
struct PeerRow {
    nickname: String,
    device_name: String,
    platform: Option<String>,
    version: Option<String>,
    color: String,
    files: Vec<String>,
}

/// 在线设备按「昵称 + 设备」合并（同身份多连接）；打开文件并集去重，色取首条，
/// 平台 / 版本取首个已知值。
fn merge_peers(peers: &[atelyx_core::collab::PeerInfo]) -> Vec<PeerRow> {
    let mut rows: Vec<PeerRow> = Vec::new();
    for p in peers {
        let files = crate::space_actions::peer_open_files(p.presence.as_ref());
        match rows
            .iter_mut()
            .find(|r| r.nickname == p.nickname && r.device_name == p.device_name)
        {
            Some(entry) => {
                for f in files {
                    if !entry.files.contains(&f) {
                        entry.files.push(f);
                    }
                }
                if entry.platform.is_none() {
                    entry.platform = p.platform.clone();
                }
                if entry.version.is_none() {
                    entry.version = p.version.clone();
                }
            }
            None => rows.push(PeerRow {
                nickname: p.nickname.clone(),
                device_name: p.device_name.clone(),
                platform: p.platform.clone(),
                version: p.version.clone(),
                color: p.color.clone(),
                files,
            }),
        }
    }
    rows
}

// ===== 搜索面板 =====

/// 结果行左右内衬（行体本身满宽，悬停底一直铺到滚动条内缘）。
const SEARCH_ROW_PAD_X: f32 = 8.0;
/// 结果行纵向内衬。
const SEARCH_ROW_PAD_Y: f32 = 5.0;
/// 结果行主次行间距。
const SEARCH_ROW_LINE_GAP: f32 = 1.0;
/// 结果行图标边长与图标到文字的间距（次行缩进量由此派生，与主行首字对齐）。
const SEARCH_ROW_ICON: f32 = 13.0;
const SEARCH_ROW_GAP: f32 = 6.0;
/// 单次渲染的结果行上限：列表非虚拟化，上限即渲染预算（超出在计数行说明）。
const SEARCH_MAX_ROWS: usize = 200;
/// 清空钮边长（空查询时同一尺寸占位，输入框宽度不随清空钮显隐变化）。
const SEARCH_CLEAR_BOX: f32 = 20.0;
/// 搜索输入框 id（清空后回焦到它）。
pub(crate) const SEARCH_INPUT_ID: &str = "search-input";

/// 结果行渲染快照：文字可用宽在布局期才知道，故先把命中项取成自持数据再进 `responsive`。
struct SearchHit {
    rel: String,
    name: String,
    /// 所在文件夹（仓库根下的文件为空串 = 不出次行）。
    parent: String,
    icon: Cow<'static, str>,
    /// 有面板内打开面（附件没有，行不挂点击）。
    openable: bool,
}

/// 搜索视图：模式胶囊 + 查询输入（固定在顶部）+ 结果列表（滚动条贴面板右缘）。
pub(crate) fn search_view(app: &App, window: iced::window::Id) -> Element<'_, Message> {
    let hits_all = atelyx_core::vault::search_file_hits(&app.note_files, &app.search_query);
    let total = hits_all.len();
    let hits: Vec<SearchHit> = hits_all
        .iter()
        .take(SEARCH_MAX_ROWS)
        .map(|&i| {
            let (rel, name) = &app.note_files[i];
            SearchHit {
                rel: rel.clone(),
                name: name.clone(),
                parent: rel.rsplit_once('/').map(|(dir, _)| dir.to_string()).unwrap_or_default(),
                icon: file_icon(app, name),
                // 图片附件由画布媒体卡承载，笔记面读不了二进制：这类行只呈现、不挂点击
                openable: atelyx_core::vault::image_ext_of(name).is_none(),
            }
        })
        .collect();

    let mut header = column![search_mode_row(window), search_input_row(&app.search_query, window)]
        .spacing(6);
    if let Some(count) = search_count_label(total, hits.len(), &app.search_query) {
        header = header.push(text(count).size(pal::fs_micro()).color(pal::TEXT_MUTED));
    }
    let header = container(header)
        .width(Length::Fill)
        .padding(Padding::ZERO.top(8.0).horizontal(8.0).bottom(6.0));

    let body: Element<'static, Message> = if app.note_files.is_empty() {
        controls::empty_state(
            "files",
            "还没有可搜索的文件",
            "在文件面板打开仓库后，在这里按名称检索文件",
            None,
        )
    } else if total == 0 {
        controls::empty_state(
            "search",
            "无匹配文件",
            "换个关键词，或输入所在文件夹的名字",
            None,
        )
    } else {
        let list = iced::widget::responsive(move |size| {
            // 行文字可用宽 = 列表宽（已扣掉滚动条占位）- 行内衬 - 图标列
            let name_w =
                (size.width - SEARCH_ROW_PAD_X * 2.0 - SEARCH_ROW_ICON - SEARCH_ROW_GAP).max(0.0);
            let mut col = iced::widget::Column::<Message>::new()
                .width(Length::Fill)
                // 列表底部留白：末行不贴面板下缘
                .padding(Padding::ZERO.bottom(8.0));
            for hit in &hits {
                col = col.push(search_row(hit, name_w, window));
            }
            col.into()
        })
        .width(Length::Fill)
        .height(Length::Shrink);
        scroll::v(list).width(Length::Fill).height(Length::Fill).into()
    };

    column![header, controls::h_divider_subtle(), body]
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// 计数行：空白查询给文件总数，有查询给命中数；超渲染上限说明只列前几条。
/// 无文件 / 无匹配由空态承载，此处不出行。
fn search_count_label(total: usize, shown: usize, query: &str) -> Option<String> {
    if total == 0 {
        return None;
    }
    Some(if query.trim().is_empty() {
        format!("共 {total} 个文件")
    } else if shown < total {
        format!("{total} 个结果，仅列前 {shown} 个")
    } else {
        format!("{total} 个结果")
    })
}

/// 模式胶囊：当前档位实底；未实现档位只有文字（不可点，悬停说明原因）。
fn search_mode_row(window: iced::window::Id) -> Element<'static, Message> {
    let chip = |label: &'static str, active: bool| {
        let (fg, bg) = if active {
            (pal::TEXT_PRIMARY, Some(pal::BG_TERTIARY))
        } else {
            (pal::TEXT_MUTED, None)
        };
        container(text(label).size(pal::fs_micro()).color(fg))
            .padding(Padding::ZERO.horizontal(8.0).vertical(3.0))
            .style(move |_t| Style {
                background: bg.map(Into::into),
                border: border::rounded(pal::RADIUS_SM),
                ..Style::default()
            })
    };
    let mut modes = row![chip("按文件名", true)].spacing(4);
    for (label, tip) in [
        ("全局搜索", "暂未实现：搜索正文内容"),
        ("按标签搜索", "暂未实现：按标签筛选文件"),
    ] {
        modes = modes.push(with_tip(mouse_area(chip(label, false)), window, tip.to_string(), 22.0));
    }
    modes.into()
}

/// 输入行：输入框满宽 + 固定宽度清空位（清空钮出现与否不改变输入框宽度）。
fn search_input_row(query: &str, window: iced::window::Id) -> Element<'static, Message> {
    let input = iced::widget::text_input("输入文件名过滤…", query)
        .id(iced::widget::Id::new(SEARCH_INPUT_ID))
        .size(pal::fs_caption())
        .font(crate::ui::regular())
        .padding(Padding::ZERO.horizontal(8.0).vertical(4.0))
        .width(Length::Fill)
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
        });
    let clear: Element<'static, Message> = if query.is_empty() {
        space().width(Length::Fixed(SEARCH_CLEAR_BOX)).into()
    } else {
        controls::icon_button(
            "x",
            12.0,
            SEARCH_CLEAR_BOX,
            Variant::Ghost,
            Some(Message::SearchClear),
            Some((window, "清空".to_string())),
        )
    };
    row![input, clear].spacing(4).align_y(Vertical::Center).into()
}

/// 结果行：主行文件名（超宽省略）+ 次行所在文件夹（缩进到文件名首字）；可打开者整行可点，
/// 附件没有面板内打开面，只呈现不响应点击。
fn search_row(hit: &SearchHit, name_w: f32, window: iced::window::Id) -> Element<'static, Message> {
    // 不可打开 ≡ 禁用态：文字降一级，与列表里其它禁用表达一致
    let name_color = if hit.openable { pal::TEXT_PRIMARY } else { pal::TEXT_MUTED };
    let mut lines = column![
        row![
            icon(hit.icon.as_ref(), SEARCH_ROW_ICON, pal::TEXT_MUTED),
            text(ellipsize_estimated(&hit.name, name_w, pal::fs_caption()))
                .size(pal::fs_caption())
                .color(name_color),
        ]
        .spacing(SEARCH_ROW_GAP)
        .align_y(Vertical::Center),
    ]
    .spacing(SEARCH_ROW_LINE_GAP);
    if !hit.parent.is_empty() {
        // 次行与主行首字对齐，可用宽相应减去图标列
        let parent_w = (name_w - SEARCH_ROW_ICON - SEARCH_ROW_GAP).max(0.0);
        lines = lines.push(
            container(
                text(ellipsize_estimated(&hit.parent, parent_w, pal::fs_micro()))
                    .size(pal::fs_micro())
                    .color(pal::TEXT_MUTED),
            )
            .padding(Padding::ZERO.left(SEARCH_ROW_ICON + SEARCH_ROW_GAP)),
        );
    }
    let pad = Padding::ZERO.horizontal(SEARCH_ROW_PAD_X).vertical(SEARCH_ROW_PAD_Y);
    let body: Element<'static, Message> = if hit.openable {
        button(lines)
            .width(Length::Fill)
            .padding(pad)
            .style(controls::row_style(false, pal::RADIUS_SM))
            .on_press(Message::OpenFile(hit.rel.clone()))
            .into()
    } else {
        container(lines).width(Length::Fill).padding(pad).into()
    };
    // 悬停给完整相对路径：行内文件名与所在文件夹都可能被省略号截断。
    // 提示锚在光标下方「半行高 + 4」处，故按双行行高给锚点。
    with_tip(mouse_area(body), window, hit.rel.clone(), 40.0).into()
}

/// 属性面板视图：画布正作为激活标签展示（任一窗口前台）= 画布属性区，否则笔记属性
/// （与编辑器属性区同一组件，改动经同一保存链落盘）。
pub(crate) fn inspector_view<'a>(app: &'a App, window: iced::window::Id) -> Element<'a, Message> {
    // 画布属性只在画布处于前台时呈现：其他标签在前台时不得被画布属性顶掉
    if app.canvas.is_some() && view_displayed(app, "canvas") {
        return crate::canvas_view::props_panel(app);
    }
    if app.open_files.recent_of_kind(&DocKind::Note, &Host::Main).is_none() {
        return controls::empty_state(
            "info",
            "笔记属性",
            "打开一篇笔记后在此查看与编辑其属性",
            None,
        );
    }
    crate::properties_view::props_panel(app, window, true)
        .unwrap_or_else(|| text("当前笔记无属性").size(pal::fs_caption()).color(pal::TEXT_MUTED).into())
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
        let label = tab_label(app, t);
        let has_file = crate::open_route::tab_has_file(app, t);
        let btn = tab_button(
            app,
            &t.view,
            label,
            has_file,
            t.locked,
            active,
            Message::DetachedTabFileClose(t.view.clone()),
        )
        .on_press_maybe((!active).then(|| Message::DetachedTab(w.id.clone(), t.id.clone())));
        tab_row = tab_row.push(controls::PressReporter::new(
            mouse_area(btn)
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
        container(controls::divider(false, pal::BORDER, Length::Fill))
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

/// 行侧缝宽度：条目不满宽的视觉留白，同时是拖拽回仓库根的落点。
const FILE_GUTTER: f32 = 8.0;
/// 行体水平内衬（与 iced Button 默认左右内衬一致）；上下内衬恒为 0，行内叠层贯穿整行。
const FILE_ROW_PAD_X: f32 = 10.0;
/// 行内图标与名称槽位、名称槽位与后缀徽标之间的间距。
const FILE_ROW_GAP: f32 = 8.0;

/// 行侧缝（根落点）：行体两侧各一条，自下而上每一行都有，任意滚动位置都能放下。
fn root_seam(rel: &str, right: bool) -> Element<'static, Message> {
    let key = crate::FileDropKey::Gutter { rel: rel.to_string(), right };
    mouse_area(space().width(Length::Fixed(FILE_GUTTER)).height(Length::Fill))
        .on_enter(Message::FileDragEnter {
            drop: key.clone(),
            rel: String::new(),
            is_dir: true,
        })
        .on_exit(Message::FileDragExit { drop: key })
        .into()
}

/// 列表下方空白的根落点（仅拖拽会话渲染；悬停 = 释放后移到仓库根）。
fn root_drop_zone(app: &App) -> Element<'static, Message> {
    let hovered = app
        .file_drag
        .as_ref()
        .is_some_and(|d| d.hover == Some(crate::FileDropKey::Bottom));
    let label = if hovered { "松手移到根目录" } else { "移到根目录" };
    let zone = container(
        text(label)
            .size(pal::fs_caption())
            .color(if hovered { pal::ACCENT } else { pal::TEXT_MUTED }),
    )
    .width(Length::Fill)
    .height(Length::Fixed(120.0))
    .align_x(Horizontal::Center)
    .align_y(Vertical::Center)
    .style(move |_t| Style {
        background: hovered.then_some(pal::ACCENT_SOFT.into()),
        border: border::rounded(pal::RADIUS_SM)
            .color(if hovered { pal::ACCENT } else { pal::TEXT_MUTED.scale_alpha(0.45) })
            .width(1.0),
        ..Style::default()
    });
    mouse_area(zone)
        .on_enter(Message::FileDragEnter {
            drop: crate::FileDropKey::Bottom,
            rel: String::new(),
            is_dir: true,
        })
        .on_exit(Message::FileDragExit { drop: crate::FileDropKey::Bottom })
        .into()
}

pub(crate) fn files_view(app: &App, _window: iced::window::Id) -> Element<'_, Message> {
    let window = app.main_id.unwrap_or(iced::window::Id::unique());
    let cursor = app.cursor_for(window);
    // 树可刷新 = 已在容器内（本地仓库或协作空间）
    let in_repo = app.container_ready();

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
            controls::icon_button("refresh", 14.0, controls::ICON_MD, Variant::Ghost,
                in_repo.then_some(Message::FileTreeRefresh), Some((window, "重新读取文件树".into()))),
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
        // 重名自动加序号的提醒条（3s 自动消失）；标题发起的改名提示贴标题，不在此重复
        if let Some(n) = app.rename_notice.as_ref().filter(|n| !n.at_title) {
            tree_col = tree_col.push(
                container(text(n.text.clone()).size(pal::fs_caption()).color(pal::WARNING))
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
        tree_col = tree_col.push(space_row(app, window, cursor, s, active, &expanded));
        if !active {
            continue;
        }
        // 重名自动加序号的提醒条（3s 自动消失）；标题发起的改名提示贴标题，不在此重复
        if let Some(n) = app.rename_notice.as_ref().filter(|n| !n.at_title) {
            tree_col = tree_col.push(
                container(text(n.text.clone()).size(pal::fs_caption()).color(pal::WARNING))
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

    // 拖拽会话：列表下方空白 = 根落点（行侧缝之外的第二个通道；非拖拽期不渲染）
    if app.file_drag.is_some() && in_repo {
        tree_col = tree_col.push(root_drop_zone(app));
    }

    let body = mouse_area(
        scroll::v(container(tree_col).width(Length::Fill))
            .width(Length::Fill)
            .height(Length::Fill),
    );
    // 空白处右键 = 在激活仓库/空间根目录新建（未激活时无根可建，不挂菜单）
    let tree: Element<'static, Message> = if app.container_ready() {
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
    // 拖拽悬停激活仓库行 = 移入根：底色加深提示可放入（根被拒时转警示色）
    let hovered = app
        .file_drag
        .as_ref()
        .is_some_and(|d| d.hover == Some(crate::FileDropKey::Vault));
    let drag_target = active && hovered && app.file_drag_target.as_deref() == Some("");
    let rejected = active && hovered && app.file_drag.as_ref().is_some_and(|d| d.reject_at("").is_some());
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
            .font(crate::ui::bold()),
    ]
    .spacing(8)
    .padding(Padding::ZERO.horizontal(8.0).vertical(4.0))
    .width(Length::Fill);

    let content = container(row)
        .width(Length::Fill)
        .height(Length::Fixed(32.0))
        .align_y(Vertical::Center)
        .style(move |_t| Style {
            // 激活行 = 强调淡底（拖拽落点更深 / 根被拒 = 警示色）；非激活行透明
            background: Some(if !active {
                Color::TRANSPARENT
            } else if rejected {
                pal::DANGER_SOFT
            } else if drag_target {
                pal::ACCENT.scale_alpha(0.35)
            } else {
                pal::ACCENT.scale_alpha(0.2)
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
            .on_enter(Message::FileDragEnter {
                drop: crate::FileDropKey::Vault,
                rel: String::new(),
                is_dir: true,
            })
            .on_exit(Message::FileDragExit { drop: crate::FileDropKey::Vault })
    } else {
        with_tip(row_elem, window, vault.root.clone(), 32.0)
    };
    let row_elem: Element<'static, Message> = row_elem
        .on_right_press(ctx_popup(window, cursor, PopupState::VaultMenu { root: vault.root.clone() }))
        .into();
    row![root_seam("", false), row_elem, root_seam("", true)]
        .height(Length::Fixed(32.0))
        .into()
}

/// AI 协作者胶囊（sparkles + 「AI · Agent 名」；编辑中带脉冲点，悬停给操作者与状态）。
pub(crate) fn ai_pill_row(
    remotes: &[crate::ai_presence::RemoteAi],
    pulse: bool,
    window: iced::window::Id,
) -> Element<'static, Message> {
    const MAX_SHOWN: usize = 2;
    let mut row = iced::widget::Row::<Message>::new().spacing(4);
    for remote in remotes.iter().take(MAX_SHOWN) {
        let dot = if remote.editing && pulse {
            pal::ACCENT
        } else {
            pal::ACCENT.scale_alpha(0.4)
        };
        let label = if remote.editing {
            format!("AI · {}", remote.agent_name)
        } else {
            format!("AI · {} · 刚刚修改", remote.agent_name)
        };
        let tip = if remote.editing {
            format!("由 {} 的 AI（{}）编辑中", remote.operator_name, remote.agent_name)
        } else {
            match &remote.last_edit_summary {
                Some(summary) => format!(
                    "刚刚由 {} 的 AI（{}）修改：{summary}",
                    remote.operator_name, remote.agent_name
                ),
                None => format!("刚刚由 {} 的 AI（{}）修改", remote.operator_name, remote.agent_name),
            }
        };
        let pill = container(
            iced::widget::row![
                icon("sparkles", 11.0, pal::ACCENT),
                controls::dot(dot, 8.0),
                text(label).size(pal::fs_micro()).color(pal::TEXT_PRIMARY),
            ]
            .spacing(5)
            .align_y(Vertical::Center),
        )
        .padding(Padding::ZERO.horizontal(8.0).vertical(4.0))
        .style(|_t| Style {
            background: Some(pal::ACCENT_SOFT.into()),
            border: border::rounded(999.0),
            ..Style::default()
        });
        row = row.push(with_tip(iced::widget::mouse_area(pill), window, tip, 22.0));
    }
    if remotes.len() > MAX_SHOWN {
        row = row.push(
            container(
                text(format!("+{}", remotes.len() - MAX_SHOWN))
                    .size(pal::fs_micro())
                    .color(pal::TEXT_SECONDARY),
            )
            .padding(Padding::ZERO.horizontal(8.0).vertical(4.0))
            .style(|_t| Style {
                background: Some(pal::ACCENT_SOFT.into()),
                border: border::rounded(999.0),
                ..Style::default()
            }),
        );
    }
    row.into()
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
                    controls::dot(dot, 8.0),
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

/// 最近空间行（与仓库行同构：激活行就地展开文件树；云图标 + 空间名）。
fn space_row(
    app: &App,
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
            .font(crate::ui::bold()),
    ]
    .spacing(8)
    .padding(Padding::ZERO.horizontal(8.0).vertical(4.0))
    .width(Length::Fill);

    // 拖拽悬停激活空间行 = 移入根：底色加深提示可放入（根被拒时转警示色）
    let hovered = app
        .file_drag
        .as_ref()
        .is_some_and(|d| d.hover == Some(crate::FileDropKey::Space));
    let drag_target = active && hovered && app.file_drag_target.as_deref() == Some("");
    let rejected = active && hovered && app.file_drag.as_ref().is_some_and(|d| d.reject_at("").is_some());
    let content = container(row)
        .width(Length::Fill)
        .height(Length::Fixed(32.0))
        .align_y(Vertical::Center)
        .style(move |_t| Style {
            background: Some(if !active {
                Color::TRANSPARENT
            } else if rejected {
                pal::DANGER_SOFT
            } else if drag_target {
                pal::ACCENT.scale_alpha(0.35)
            } else {
                pal::ACCENT.scale_alpha(0.2)
            }
            .into()),
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
    // 文件拖拽会话中悬停激活空间行 = 落点上报（拖至该行 = 移入根）
    let row_elem = if app.file_drag.is_some() && active {
        row_elem
            .on_enter(Message::FileDragEnter {
                drop: crate::FileDropKey::Space,
                rel: String::new(),
                is_dir: true,
            })
            .on_exit(Message::FileDragExit { drop: crate::FileDropKey::Space })
    } else {
        with_tip(row_elem, window, format!("{} · {}", space.name, space.server_url), 32.0)
    };
    let row_elem: Element<'static, Message> = row_elem
        .on_right_press(ctx_popup(
            window,
            cursor,
            PopupState::SpaceRowMenu {
                server_url: space.server_url.clone(),
                space_id: space.space_id.clone(),
            },
        ))
        .into();
    row![root_seam("", false), row_elem, root_seam("", true)]
        .height(Length::Fixed(32.0))
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
        (Cow::Borrowed("folder"), folder_color.unwrap_or(pal::TEXT_MUTED))
    } else {
        let active_icon = pal::ACCENT;
        let color = if r.active { active_icon } else { pal::TEXT_SECONDARY };
        (file_icon(app, &r.name), color)
    };
    let name_color = if r.is_dir {
        pal::TEXT_SECONDARY
    } else if r.active {
        pal::ACCENT
    } else {
        pal::TEXT_SECONDARY
    };

    let mut content = row![].spacing(FILE_ROW_GAP).width(Length::Fill);
    if r.is_dir {
        content = content.push(icon(
            if r.expanded { "chevron-down" } else { "chevron-right" },
            14.0,
            pal::TEXT_MUTED,
        ));
    }
    content = content.push(icon_stroke(&icon_name, 14.0, icon_color, 2.0));
    let ext_color = if r.active { pal::ACCENT } else { pal::TEXT_MUTED };
    content = content.push(tree_name_slot(r, name_color, ext_color));

    let rejected = app
        .file_drag
        .as_ref()
        .is_some_and(|d| d.reject_at(r.rel.as_str()).is_some());
    let bg = if rejected {
        pal::DANGER_SOFT
    } else if r.active || drag_target {
        pal::ACCENT_SOFT
    } else {
        iced::Color::TRANSPARENT
    };
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
            // 上下内衬置 0：指示线与激活轨贯穿整行，相邻行的线首尾相接
            .padding(Padding::ZERO.horizontal(FILE_ROW_PAD_X))
            .style(|_t, status| button::Style {
                background: matches!(status, button::Status::Hovered | button::Status::Pressed)
                    .then_some(pal::HOVER.into()),
                ..Default::default()
            })
            .on_press(msg),
    )
    .on_right_press(Message::PopupOpen(window, PopupOpen { point: cursor, flip_y: None, state, min_width: 0.0 }));
    // 拖拽悬停：目录行 = 可落点，文件行 = 拒绝提示（都上报，判定在会话侧）
    let row = row
        .on_enter(Message::FileDragEnter {
            drop: crate::FileDropKey::Row(r.rel.clone()),
            rel: r.rel.clone(),
            is_dir: r.is_dir,
        })
        .on_exit(Message::FileDragExit { drop: crate::FileDropKey::Row(r.rel.clone()) });
    let row: Element<'static, Message> = controls::PressReporter::new(
        row.into(),
        Message::FileRowPress { window, rel: r.rel.clone(), is_dir: r.is_dir },
    )
    .into();
    // 两侧缝 = 仓库根落点（行体不占满宽度，拖到缝隙即移到根）
    let element: Element<'static, Message> =
        row![root_seam(&r.rel, false), row, root_seam(&r.rel, true)]
            .height(Length::Fixed(32.0))
            .into();
    // 名称按可用宽省略：悬停给完整文件名（拖拽会话中不出提示）
    if app.file_drag.is_some() {
        return element;
    }
    with_tip(mouse_area(element), window, r.name.clone(), 32.0).into()
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
                    DraftKind::Canvas => "画布名称",
                    DraftKind::Doc { label, .. } => label,
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
    .padding(Padding::ZERO.left(12.0 + depth as f32 * 16.0 + FILE_GUTTER).right(8.0 + FILE_GUTTER))
    .align_y(Vertical::Center)
    .into()
}

/// 就地重命名的输入行（渲染在对应行位置）。
fn file_inline_input(app: &App, r: &TreeRow) -> Option<Element<'static, Message>> {
    let indent = 12.0 + r.depth as f32 * 16.0;
    let rs = app.rename.as_ref()?;
    // 页面内标题发起的改名：输入框渲染在正文列标题处，此处不渲染（同一输入框 id 同帧唯一）
    if rs.from_title {
        return None;
    }
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
        .padding(Padding::ZERO.left(indent + FILE_GUTTER).right(8.0 + FILE_GUTTER))
        .align_y(Vertical::Center)
        .into(),
    )
}

fn file_icon(app: &App, name: &str) -> Cow<'static, str> {
    // 插件领域文档类型按注册图标呈现（扩展名大小写不敏感），其余走内置映射
    // （注册表随贡献刷新，避免文件树逐行渲染时重查运行时）
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

/// 内置扩展名图标映射（无 app 上下文的调用点用；映射表在注册表旁，与最近/历史面板同源）。
fn file_icon_base(name: &str) -> &'static str {
    let lower = name.to_lowercase();
    if atelyx_core::vault::image_ext_of(&lower).is_some() {
        return "image";
    }
    let ext = lower.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    crate::registry::doc_icon(ext)
}

/// 扩展名标签（ATLX / CANVAS / MD / ATB / 大写扩展名；前导点文件 = 空串不显示）。
fn ext_label(name: &str) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 => name[i + 1..].to_uppercase(),
        _ => String::new(),
    }
}

/// 名称槽位宽 = 行内容宽 − 后缀徽标预留（不小于 0）。
fn name_slot(row_w: f32, ext: &str) -> f32 {
    (row_w - ext_badge_reserve(ext)).max(0.0)
}

/// 后缀徽标预留宽：等宽字比估宽口径宽，按估宽 ×1.3 取保守值，再加与名称的间距。
fn ext_badge_reserve(ext: &str) -> f32 {
    if ext.is_empty() {
        0.0
    } else {
        estimate_width(ext, pal::fs_micro()) * 1.3 + FILE_ROW_GAP
    }
}

/// 行名槽位：占满图标之后的剩余宽，超宽按槽位宽尾部省略；
/// 文件行的后缀徽标接在槽位之后，恒由行的剩余宽定在右缘。
fn tree_name_slot(r: &TreeRow, name_color: Color, ext_color: Color) -> Element<'static, Message> {
    let name = r.name.clone();
    let ext = if r.is_dir { String::new() } else { ext_label(&r.name) };
    iced::widget::responsive(move |size| {
        // 极窄面板下可用宽可能为负：槽位按 0 兜底
        let slot_w = size.width.max(0.0);
        let name_w = name_slot(slot_w, &ext);
        let mut slot = row![
            container(
                text(ellipsize_estimated(&name, name_w, pal::fs_caption()))
                    .size(pal::fs_caption())
                    .color(name_color)
                    .wrapping(Wrapping::None),
            )
            .width(Length::Fill)
            .clip(true),
        ];
        if !ext.is_empty() {
            slot = slot.push(
                text(ext.clone()).size(pal::fs_micro()).color(ext_color).font(mono()),
            );
        }
        // 外层行按固有宽压缩 Fill：内层行定宽为分到的剩余宽，徽标才恒贴行右缘
        slot.spacing(FILE_ROW_GAP).width(Length::Fixed(slot_w)).into()
    })
    .width(Length::Fill)
    .height(Length::Shrink)
    .into()
}

/// 十六进制颜色串（`#rrggbb`）→ Color；非法输入返回 None（回落默认图标色）。
pub(crate) fn parse_hex_color(hex: &str) -> Option<iced::Color> {
    if hex.len() != 7 || !hex.starts_with('#') {
        return None;
    }
    crate::ui::color_picker::parse_hex(hex)
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
        let active = !is_dir && app.open_files.find_by_path(&rel).is_some();
        rows.push(TreeRow { name, depth, is_dir, expanded: is_expanded, rel: rel.clone(), active, guides: Vec::new() });
        if is_dir && is_expanded {
            push_children(app, rows, &rel, depth + 1, expanded);
        }
    }
}

// ===== note 视图 =====

/// 文件显示标题（改名输入框只给主名，后缀由提交时按原名补回）。
pub use atelyx_core::vault::file_label;

/// note 视图：工具条（模式分段 + ···菜单）+ 对应档位的内容面。
pub(crate) fn note_view(app: &App, window: iced::window::Id) -> Element<'_, Message> {
    if app.open_files.recent_of_kind(&DocKind::Note, &Host::Main).is_none() {
        return controls::empty_state(
            "sticky-note",
            "打开笔记",
            "在文件面板或搜索面板单击一个 .md 笔记开始编辑",
            None,
        );
    }
    let cursor = app.cursor_for(window);
    // 只读身份（协作空间 Viewer）：编辑入口在渲染层即禁用，常驻提示条说明原因
    let readonly = app.note_readonly();

    let mode_control = container(
        row![
            mode_button(window, "eye", "只读", "只读：仅查看，双击正文进入编辑", NoteMode::Read, app.note_mode, readonly),
            mode_button(window, "pencil", "预览编辑", "预览编辑：正文可直接修改", NoteMode::Edit, app.note_mode, readonly),
            mode_button(window, "code", "源码", "源码：直接编辑 Markdown 源文", NoteMode::Source, app.note_mode, readonly),
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
    // 协作者胶囊（空间模式；同文件在场者，色点 + 昵称，多时折叠；名册见协作房间视图）
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
    // AI 协作者胶囊（与成员胶囊并列；按当前笔记过滤）
    let ai_remotes: Vec<crate::ai_presence::RemoteAi> = app
        .remote_ai
        .iter()
        .filter(|r| r.view == "note" && r.file.as_deref() == crate::open_route::note_path(app).as_deref())
        .cloned()
        .collect();
    if matches!(app.repo_kind, crate::RepoKind::Space(_)) && !ai_remotes.is_empty() {
        toolbar_row = toolbar_row.push(ai_pill_row(&ai_remotes, app.ai_pulse, window));
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
        NoteMode::Source => bounded_column(
            iced::widget::text_editor(&app.source_editor)
                .height(Length::Fill)
                .font(crate::ui::regular())
                .on_action(Message::Edited)
                .style(|_t, _s| text_editor::Style {
                    background: iced::Background::Color(iced::Color::TRANSPARENT),
                    border: border::Border::default(),
                    placeholder: pal::TEXT_MUTED,
                    value: pal::TEXT_PRIMARY,
                    selection: pal::SELECTION,
                })
                .into(),
            app.global.content_width,
            Length::Fill,
        ),
        NoteMode::Read | NoteMode::Edit => {
            let inner: Element<'_, Message> = match app.note_mode {
                NoteMode::Edit => edit_view(app),
                _ => match app.preview_doc.as_ref() {
                    Some(doc) => {
                        markdown_view::document_view(
                            doc,
                            &app.theme,
                            &app.note_images,
                            app.global.soft_wrap,
                            app.note_selection.as_ref(),
                            &app.note_code_expanded,
                            &app.note_block_metrics,
                            &app.note_cell_cache,
                            app.note_read_window(),
                            app.note_image_epoch,
                        )
                    }
                    None => placeholder_panel("正在渲染…"),
                },
            };
            // 正文行宽：>0 时钳制正文区最大宽度并水平居中（三档同一列宽）
            let scrolled =
                scroll::v(bounded_column(inner, app.global.content_width, Length::Shrink))
                    .id(note_scroll_id())
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .on_scroll(Message::NoteScrolled);
            // 两档同包一层 mouse_area（同型同位保滚动状态）；双击进编辑只在只读档挂处理器
            // （编辑档不挂：编辑面按压即捕获，双击语义由块内自管）
            if app.note_mode == NoteMode::Edit {
                mouse_area(scrolled).into()
            } else {
                // 只读档双击正文进预览编辑档（光标落点由编辑面按释放点换算）；右键 = 选区复制菜单。
                // 只读身份不挂双击：进编辑档的入口连渲染层都不存在；无选区不挂右键（菜单无事可做）
                let has_selection = app
                    .note_selection
                    .as_ref()
                    .is_some_and(|sel| sel.range().0 < sel.range().1);
                let area = mouse_area(scrolled);
                let area = if has_selection {
                    area.on_right_press(selection_popup(
                        window,
                        cursor,
                        crate::markdown_select::SelectionTarget::Note,
                    ))
                } else {
                    area
                };
                if readonly {
                    area.into()
                } else {
                    area.on_double_click(Message::NoteEnterEdit).into()
                }
            }
        }
    };
    // 属性面板（只读 / 预览编辑档显示；源码档不渲染）
    let panel = crate::properties_view::props_panel(app, window, false);

    // 编辑态高亮边框：预览编辑 / 源码档给正文区一圈 1px 强调边；只读档同宽透明，切档不位移。
    let frame = if app.note_mode == NoteMode::Read {
        Color::TRANSPARENT
    } else {
        pal::ACCENT
    };

    // 页面内标题：钉在正文列顶部（只有正文滚动），与属性区 / 正文同宽居中；源码档不渲染
    let title = (app.global.in_page_title && app.note_mode != NoteMode::Source)
        .then(|| bounded_column(page_title(app, window), app.global.content_width, Length::Shrink));

    let mut body = column![toolbar, controls::h_divider_subtle()];
    if readonly {
        body = body.push(readonly_banner());
    }
    if let Some(t) = title {
        body = body.push(t);
    }
    if let Some(p) = panel {
        body = body.push(p);
    }
    body = body.push(
        container(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_t| Style {
                border: border::rounded(0.0).color(frame).width(1.0),
                ..Style::default()
            }),
    );
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

/// 笔记正文滚动容器 id（视图挂载与光标跟随滚动共用）。
pub(crate) fn note_scroll_id() -> iced::widget::Id {
    iced::widget::Id::new("note-scroll")
}

/// 正文行宽钳制：宽度 > 0 时给正文列加上限宽并水平居中，0 = 不限（铺满面板）。
/// 居中须由外层容器承担——iced 的 `center_x` 只摆自身矩形内的子项，容器自身被
/// `max_width` 钳窄后仍贴父容器左缘，单层写法只会「变窄」而不会居中。
fn bounded_column<'a>(
    el: Element<'a, Message>,
    width: u32,
    height: Length,
) -> Element<'a, Message> {
    if width == 0 {
        return el;
    }
    container(
        container(el)
            .width(Length::Fill)
            .height(height)
            .max_width(width as f32),
    )
    .width(Length::Fill)
    .height(height)
    .center_x(Length::Fill)
    .into()
}

/// 页面内标题度量：左右内边距与正文 16px 内边距对齐；行高 1.4 = 20px 字 / 28px 行。
const TITLE_PAD_X: f32 = 16.0;
const TITLE_PAD_TOP: f32 = 12.0;
const TITLE_PAD_BOTTOM: f32 = 4.0;
const TITLE_LINE_RATIO: f32 = 1.4;
/// 标题截断缓存条目上限（键 = 原文 + 可用宽 + 字号 + 字体；超限整体清空）。
const TITLE_CLIP_CAP: usize = 32;

thread_local! {
    /// 标题截断结果缓存：标题只在切文件 / 改宽 / 改字号 / 换字体族时变，避免每帧重跑 shaping 与逐字试宽。
    static TITLE_CLIP: std::cell::RefCell<HashMap<(String, u32, u32, iced::Font), String>> =
        std::cell::RefCell::new(HashMap::new());
}

/// 页面内标题：正文列顶部的文件名（去扩展名）；点击就地改名，重命名在途 = 原位输入框。
fn page_title(app: &App, window: iced::window::Id) -> Element<'_, Message> {
    let rel = crate::open_route::note_path(app).unwrap_or_default();
    let title = file_label(&rel);
    let (renaming, value) = match &app.rename {
        Some(r) if r.from_title => (true, r.value.clone()),
        _ => (false, String::new()),
    };
    // 重名去重提示渲染位置随提醒条自身：标题发起的改名贴标题下方
    let notice = app
        .rename_notice
        .as_ref()
        .filter(|n| n.at_title)
        .map(|n| n.text.clone());
    // 可用宽在布局期才知：省略号截断按 responsive 给出的宽计算
    iced::widget::responsive(move |size| {
        let mut col = column![].padding(
            Padding::ZERO
                .left(TITLE_PAD_X)
                .right(TITLE_PAD_X)
                .top(TITLE_PAD_TOP)
                .bottom(TITLE_PAD_BOTTOM),
        );
        if renaming {
            col = col.push(title_input(&value));
        } else {
            let avail = (size.width - TITLE_PAD_X * 2.0).max(0.0);
            col = col.push(title_label(window, clipped_title(&title, avail)));
        }
        if let Some(notice) = &notice {
            col = col.push(
                text(notice.clone())
                    .size(pal::fs_caption())
                    .color(pal::WARNING),
            );
        }
        col.into()
    })
    .width(Length::Fill)
    .height(Length::Shrink)
    .into()
}

/// 显示态标题：整行可点；按下在释放时上报，避开同批全局按压（失焦提交判定）把刚起的改名立即提交。
/// 光标由按钮给出（Pointer）：MouseArea 的显式 interaction 仅在其子部件无 interaction 时生效。
fn title_label(window: iced::window::Id, label: String) -> Element<'static, Message> {
    let label = text(label)
        .size(pal::fs_h1())
        .font(crate::ui::bold())
        .line_height(LineHeight::Relative(TITLE_LINE_RATIO))
        .wrapping(Wrapping::None);
    mouse_area(
        button(label)
            .padding(0)
            .width(Length::Fill)
            .style(|_t, status| button::Style {
                background: None,
                text_color: if matches!(status, button::Status::Hovered | button::Status::Pressed) {
                    pal::TEXT_SECONDARY
                } else {
                    pal::TEXT_PRIMARY
                },
                ..Default::default()
            })
            .on_press(Message::NoteTitleRenameStart(window)),
    )
    .into()
}

/// 编辑态标题：与显示态同字阶 / 字重 / 行高与内边距（进入编辑文字不跳位），下沿 1px 色带表改名中。
fn title_input(value: &str) -> Element<'static, Message> {
    let input = text_input("", value)
        .id(iced::widget::Id::new(controls::INLINE_RENAME_INPUT))
        .size(pal::fs_h1())
        .font(crate::ui::bold())
        .line_height(LineHeight::Relative(TITLE_LINE_RATIO))
        .padding(Padding::ZERO)
        .width(Length::Fill)
        .on_input(Message::LayoutRenameInput)
        .on_submit(Message::LayoutRenameCommit)
        .style(|_t, _s| text_input::Style {
            background: Color::TRANSPARENT.into(),
            border: border::Border::default(),
            icon: pal::TEXT_MUTED,
            placeholder: pal::INPUT_PLACEHOLDER,
            value: pal::TEXT_PRIMARY,
            selection: pal::SELECTION,
        });
    column![
        // 输入框内按压 = 不触发失焦提交（与文件树行内输入同一标志）
        controls::PressReporter::new(input.into(), Message::RenamePressInside),
        controls::divider(false, pal::BORDER_STRONG, Length::Fill),
    ]
    .into()
}

/// 单行标题按可用宽截断补省略号（结果缓存：同文同宽同字号只算一次 shaping）。
fn clipped_title(title: &str, avail_w: f32) -> String {
    let avail_w = avail_w.max(0.0);
    let key = (title.to_string(), (avail_w * 100.0) as u32, pal::fs_h1().to_bits(), crate::ui::bold());
    if let Some(hit) = TITLE_CLIP.with(|c| c.borrow().get(&key).cloned()) {
        return hit;
    }
    let clipped = ellipsize(title, avail_w, |s| measure(s, pal::fs_h1(), crate::ui::bold()));
    TITLE_CLIP.with(|c| {
        let mut cache = c.borrow_mut();
        if cache.len() >= TITLE_CLIP_CAP {
            cache.clear();
        }
        cache.insert(key, clipped.clone());
    });
    clipped
}

/// 模式分段按钮（Button sm 档：高 24 / px-8 / 11px / 强调字重；激活 = 强调淡底，悬停文字提亮）。
/// 只读身份下非只读档禁用（不挂 on_press，文字降为弱色，提示改为只读原因）。
fn mode_button(
    window: iced::window::Id,
    icon_name: &'static str,
    label: &str,
    hint: &str,
    mode: NoteMode,
    current: NoteMode,
    readonly: bool,
) -> Element<'static, Message> {
    let active = mode == current;
    let disabled = readonly && mode != NoteMode::Read;
    let tint = if disabled {
        pal::TEXT_MUTED
    } else if active {
        pal::ACCENT
    } else {
        pal::TEXT_SECONDARY
    };
    let tip = if disabled { "当前身份为只读，无法编辑" } else { hint };
    let btn = button(
        container(
            row![
                icon(icon_name, 12.0, tint),
                text(label.to_string())
                    .size(pal::fs_micro())
                    .font(crate::ui::bold())
                    .color(tint),
            ]
            .spacing(4)
            .align_y(Vertical::Center),
        )
        .height(Length::Fill)
        .align_y(Vertical::Center),
    )
    .padding(Padding::ZERO.horizontal(8.0))
    .height(Length::Fixed(24.0))
    .style(move |_t, status| button::Style {
        background: active.then_some(pal::ACCENT_SOFT.into()),
        text_color: match status {
            _ if disabled => pal::TEXT_MUTED,
            button::Status::Hovered | button::Status::Pressed if !active => pal::TEXT_PRIMARY,
            _ => pal::TEXT_SECONDARY,
        },
        border: border::rounded(pal::RADIUS_SM),
        ..Default::default()
    });
    // 禁用档不挂按压：只读身份的编辑入口在渲染层就不存在
    let btn = if disabled { btn } else { btn.on_press(Message::NoteMode(mode)) };
    with_tip(iced::widget::mouse_area(btn), window, tip, 24.0).into()
}

/// 只读身份提示条：常驻工具条下，说明为什么编辑入口是禁用的。
fn readonly_banner() -> Element<'static, Message> {
    container(
        row![
            icon("lock", 13.0, pal::WARNING),
            text("当前身份为只读，无法编辑".to_string())
                .size(pal::fs_caption())
                .color(pal::TEXT_SECONDARY),
        ]
        .spacing(6)
        .align_y(Vertical::Center),
    )
    .width(Length::Fill)
    .padding(Padding::ZERO.horizontal(12.0).vertical(6.0))
    .style(|_t| Style {
        background: Some(pal::WARNING_SOFT.into()),
        ..Style::default()
    })
    .into()
}

/// 揭示态的源码面元素（行装饰由渲染面给出的包装沿用）。
#[allow(clippy::too_many_arguments)]
fn source_block_element<'a>(
    app: &'a App,
    body: &'a str,
    from: usize,
    to: usize,
    source: markdown_view::SourceKind,
    pal: markdown_edit::EditPalette,
    bindings: crate::commands::NoteBindings,
    body_start: usize,
) -> Element<'a, Message> {
    markdown_edit::SourceBlock {
        from,
        to,
        text: body,
        caret: app.edit.caret,
        anchor: app.edit.anchor,
        preedit: app.edit.preedit.clone(),
        source,
        pal,
        opts: &app.edit_opts,
        focused: app.edit_surface_focused() && app.edit.caret >= from && app.edit.caret <= to,
        cursors: &app.remote_cursors,
        body_start,
        bindings,
        probe: app.note_caret_probe.clone(),
        picker_open: app.wiki_picker.is_some(),
    }
    .into()
}

/// 编辑面视口窗口：按行高缓存截出可见行（上下各留一段余量）。
/// 返回（行区间，区间前缀高，区间后缀高）；缓存不可用返回 None（整篇构建）。
fn note_row_window(app: &App) -> Option<(std::ops::Range<usize>, f32, f32)> {
    if app.note_viewport_h <= 0.0 {
        return None;
    }
    let metrics = app.note_row_metrics.borrow();
    if !app.note_scroll_ready {
        return None;
    }
    let rows = metrics.rows;
    if rows == 0 || metrics.heights.len() != rows || metrics.width <= 0.0 {
        return None;
    }
    if metrics.heights.iter().any(|height| *height <= 0.0) {
        return None;
    }
    row_window(&metrics.heights, app.note_scroll_y, app.note_viewport_h)
}

/// 行高表 + 滚动偏移 → 可见行区间（上下各留 0.75 屏余量）与区间前后高度。
fn row_window(
    heights: &[f32],
    scroll_y: f32,
    viewport_h: f32,
) -> Option<(std::ops::Range<usize>, f32, f32)> {
    if heights.is_empty() || viewport_h <= 0.0 {
        return None;
    }
    let top_limit = (scroll_y - viewport_h * 0.75).max(0.0);
    let bottom_limit = scroll_y + viewport_h * 1.75;
    let mut y = 0.0;
    let mut start = None;
    let mut end = heights.len();
    for (index, height) in heights.iter().enumerate() {
        let next = y + height;
        if start.is_none() && next >= top_limit {
            start = Some(index);
        }
        if next >= bottom_limit {
            end = index + 1;
            break;
        }
        y = next;
    }
    let start = start?;
    let prefix: f32 = heights[..start].iter().sum();
    let suffix: f32 = heights[end..].iter().sum();
    Some((start..end.max(start + 1), prefix, suffix))
}

/// 预览编辑档：行清单由渲染面产出（富渲染件 + 源锚 + 可见片段），已揭示的行以源码面
/// 替换其内容、行装饰照旧；块内空行与块间空行同为可点的空行行（光标由编辑面自绘）。
fn edit_view(app: &App) -> Element<'_, Message> {
    use markdown_edit::SurfaceRow;
    use markdown_view::RowKind;

    let Some(doc) = app.edit_doc.as_ref() else {
        return placeholder_panel("正在解析…");
    };
    let pal_edit = markdown_edit::EditPalette::from_theme(&app.theme);
    let note_bindings = crate::commands::note_bindings(&app.global.command_shortcuts);
    let body = doc.source.as_str();
    let caret = app.edit.caret.min(body.len());
    let (sel_from, sel_to) = app.edit.selection();
    let has_selection = sel_to > sel_from;
    // 光标停在空白行时不揭示所在行（空行行自己托管光标）
    let caret_blank = markdown_edit::line_is_blank(body, caret);
    // 远端光标以 note_text 全文为参照，编辑面坐标相对正文
    let body_start = atelyx_core::frontmatter::block_end(&app.note_text).unwrap_or(0);
    let mut rows = Vec::new();
    // 空文档：补一个空源码行，给光标一个宿主
    if doc.blocks.is_empty() {
        rows.push(SurfaceRow::Row {
            from: 0,
            to: 0,
            revealed: true,
            runs: Vec::new(),
            indent: 0.0,
            source: markdown_view::empty_block_meta().source,
            code: None,
            covered: false,
            element: source_block_element(
                app,
                body,
                0,
                0,
                markdown_view::empty_block_meta().source,
                pal_edit,
                note_bindings.clone(),
                body_start,
            ),
        });
        return markdown_edit::EditSurface::new(
            rows,
            pal_edit,
            body,
            caret,
            app.edit.anchor,
            app.edit.preedit.clone(),
            app.edit_surface_focused(),
            app.note_edit_caret_pending,
            note_bindings,
            &app.remote_cursors,
            body_start,
            app.note_caret_probe.clone(),
            app.wiki_picker.is_some(),
        )
        .with_window(app.note_row_metrics.clone(), 0, 0.0, 0.0, 1)
        .into();
    }

    let metrics = app.note_row_metrics.clone();
    let known_rows = app.note_row_metrics.borrow().rows;
    let window = note_row_window(app);
    let mut all_rows = match &window {
        Some((range, _, _)) => markdown_view::edit_rows_window(
            body,
            &doc.blocks,
            &app.theme,
            &app.note_images,
            Some(range.clone()),
        ),
        None => markdown_view::edit_rows_window(
            body,
            &doc.blocks,
            &app.theme,
            &app.note_images,
            None,
        ),
    };
    let (start, end, top, bottom) = match window {
        Some((range, top, bottom)) if all_rows.len() == known_rows => {
            (range.start, range.end.min(all_rows.len()), top, bottom)
        }
        _ => {
            all_rows = markdown_view::edit_rows_window(
                body,
                &doc.blocks,
                &app.theme,
                &app.note_images,
                None,
            );
            (0, all_rows.len(), 0.0, 0.0)
        }
    };
    // 块内空行（代码块 / 引用里的空行）不生成独立空行行：光标落在其中时仍须揭示所在块，
    // 否则源码面不存在、键盘没有宿主，该行永远打不进字
    let caret_on_gap = all_rows
        .iter()
        .any(|row| matches!(row.kind, RowKind::Gap) && row.from <= caret && caret < row.to);
    // 落在空行行上才由空行行托管光标（不揭示块）；否则空行在块内，必须揭示
    let caret_blank = caret_blank && !caret_on_gap;
    let total_rows = all_rows.len();
    for (index, row) in all_rows.into_iter().enumerate() {
        if index < start || index >= end || row.element.is_none() {
            continue;
        }
        let (from, to) = (row.from, row.to);
        match row.kind {
            RowKind::Gap => {
                let covered =
                    markdown_edit::gap_covered_lines(&row.gap_lines, to, sel_from, sel_to);
                rows.push(SurfaceRow::Gap {
                    from,
                    to,
                    line_starts: row.gap_lines,
                    covered,
                    indent: row.indent,
                    element: row.element.expect("窗口内的行有元素"),
                });
            }
            RowKind::Deco => {
                // 装饰行不承载光标：单击落点吸到该行起点（应用层按行首换算）
                rows.push(SurfaceRow::Row {
                    from: row.reveal.0,
                    to: row.reveal.1,
                    revealed: false,
                    runs: Vec::new(),
                    indent: row.indent,
                    source: row.source,
                    code: None,
                    covered: false,
                    element: row.element.expect("窗口内的行有元素"),
                });
            }
            RowKind::Block | RowKind::Unit => {
                let (reveal_from, reveal_to) = row.reveal;
                let in_caret = caret >= reveal_from && caret <= reveal_to;
                let in_anchor =
                    has_selection && app.edit.anchor >= reveal_from && app.edit.anchor <= reveal_to;
                let revealed = (in_caret && !caret_blank) || in_anchor;
                let covered =
                    !revealed && sel_to > sel_from && sel_from < to && sel_to > from;
                let source = row.source;
                // 揭示态源码面的左内缩只余行装饰：标记列由源码自身承担，
                // 沿用富渲染行的内缩会把落点与光标整体右移一个标记列
                let indent = row.indent_for(revealed);
                let element: Element<'_, Message> = if revealed {
                    row.revealed(source_block_element(
                        app,
                        body,
                        reveal_from,
                        reveal_to,
                        source,
                        pal_edit,
                        note_bindings.clone(),
                        body_start,
                    ))
                } else {
                    row.element.expect("窗口内的行有元素")
                };
                rows.push(SurfaceRow::Row {
                    from,
                    to,
                    revealed,
                    runs: row.runs,
                    indent,
                    source,
                    code: row.code,
                    covered,
                    element,
                });
            }
        }
    }
    markdown_edit::EditSurface::new(
        rows,
        pal_edit,
        body,
        caret,
        app.edit.anchor,
        app.edit.preedit.clone(),
        app.edit_surface_focused(),
        app.note_edit_caret_pending,
        note_bindings,
        &app.remote_cursors,
        body_start,
        app.note_caret_probe.clone(),
        app.wiki_picker.is_some(),
    )
    .with_window(metrics, start, top, bottom, total_rows)
    .into()
}

/// 双链候选浮层宽度与行度量：整行可点，行高随界面字号放大（大字号下不裁行）。
const SUGGEST_W: f32 = 288.0;
const SUGGEST_MAX_H: f32 = 264.0;
const SUGGEST_ROW_H: f32 = 26.0;
const SUGGEST_PAD_X: f32 = 10.0;
const SUGGEST_PAD_Y: f32 = 4.0;
/// 浮层与光标的间距（横向让出光标竖条，纵向落到下一行）。
const SUGGEST_GAP_X: f32 = 6.0;
const SUGGEST_GAP_Y: f32 = 2.0;

/// 双链候选浮层内容（锚在光标右下方；↑↓ 导航并接管 Enter、Enter 选中、Esc 关闭）。
/// 落位在窗口级浮层里完成——浮层不能被正文滚动区裁剪。
fn wiki_picker_panel<'a>(app: &'a App, window: iced::window::Id) -> Option<Element<'a, Message>> {
    // 浮层挂在窗口层：只在预览编辑档且编辑面持键盘时出现，切档 / 失焦即不再显示
    if app.note_mode != NoteMode::Edit || !app.edit.focused {
        return None;
    }
    let picker = app.wiki_picker.as_ref()?;
    // 坐标是开启窗口的光标局部坐标：别的窗口按同一坐标再画一份会跑到无关位置，且点一下就把链接
    // 插进另一窗口正在编辑的正文
    if picker.window != window {
        return None;
    }
    let candidates = app.wiki_candidates();
    // 候选里出现同名笔记时补出所在目录（跨目录同名才需要区分）
    let duplicates = {
        let mut names: Vec<&str> = candidates
            .iter()
            .map(|i| app.note_files[*i].1.as_str())
            .collect();
        names.sort_unstable();
        names.windows(2).any(|w| w[0] == w[1])
    };
    let rows = candidates.len().max(1);
    let panel_h = (rows as f32 * SUGGEST_ROW_H).min(SUGGEST_MAX_H) + SUGGEST_PAD_Y * 2.0;
    let mut col = iced::widget::Column::<Message>::new().spacing(0);
    if candidates.is_empty() {
        col = col.push(
            container(text("无匹配笔记").size(pal::fs_caption()).color(pal::TEXT_MUTED))
                .padding(Padding::ZERO.horizontal(SUGGEST_PAD_X).vertical(6.0)),
        );
    } else {
        let active = picker.active.min(candidates.len() - 1);
        for (pos, fi) in candidates.iter().enumerate() {
            let (rel, name) = &app.note_files[*fi];
            let title = name.trim_end_matches(".md").to_string();
            // 同名笔记跨目录：同名时补出所在目录，候选才分得清
            let dir = rel.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
            let sub = (!dir.is_empty() && duplicates).then_some(dir);
            let is_active = pos == active;
            let mut body = column![row![
                icon("file-text", 13.0, if is_active { pal::TEXT_PRIMARY } else { pal::TEXT_MUTED }),
                text(title.clone())
                    .size(pal::fs_caption())
                    .wrapping(Wrapping::None)
                    .color(if is_active { pal::TEXT_PRIMARY } else { pal::TEXT_SECONDARY }),
            ]
            .spacing(6)
            .align_y(iced::alignment::Vertical::Center)]
            .spacing(1);
            if let Some(dir) = sub {
                body = body.push(
                    container(
                        text(dir.to_string())
                            .size(pal::fs_micro())
                            .wrapping(Wrapping::None)
                            .color(pal::TEXT_MUTED),
                    )
                    .padding(Padding::ZERO.left(19.0)),
                );
            }
            // 键盘高亮 = ACCENT 底；鼠标悬停 / 按下 = HOVER 底（与对话面板 # 引用候选同一套行样式）
            col = col.push(
                button(body)
                    .width(Length::Fill)
                    .padding(Padding::ZERO.horizontal(SUGGEST_PAD_X).vertical(5.0))
                    .style(move |_t, status| button::Style {
                        background: if is_active {
                            Some(pal::ACCENT.into())
                        } else if matches!(status, button::Status::Hovered | button::Status::Pressed) {
                            Some(pal::HOVER.into())
                        } else {
                            None
                        },
                        border: border::rounded(pal::RADIUS_SM),
                        text_color: pal::TEXT_PRIMARY,
                        ..button::Style::default()
                    })
                    .on_press(Message::WikiPick(*fi)),
            );
        }
    }
    Some(
        container(scroll::v(col).width(Length::Fill).height(Length::Shrink))
            .width(Length::Fixed(SUGGEST_W))
            .max_height(panel_h)
            .style(|_t| Style {
                background: Some(pal::BG_SECONDARY.into()),
                border: border::rounded(pal::RADIUS_MD).color(pal::BORDER).width(1.0),
                ..Style::default()
            })
            .into(),
    )
}

/// 候选浮层落位：锚在光标右下方（下方放不下时翻到光标上方，越右缘时左移）。
/// 光标几何由编辑面每帧回写，坐标为窗口局部坐标。
pub(crate) fn wiki_picker_overlay<'a>(
    app: &'a App,
    window: iced::window::Id,
) -> Option<Element<'a, Message>> {
    let panel = wiki_picker_panel(app, window)?;
    let probe = *app.note_caret_probe.borrow();
    let geom = wiki_picker_geom(probe, viewport_of(app, window));
    // 浮层在窗口层、已脱离笔记面板的 PressReporter：点候选会被判成「面板外按压」而先退出
    // 编辑档（候选随之消失，按钮收不到释放）。卡片自身自报面板内按压，与面板内控件同语义
    // （只包卡片，不包整窗——否则弹层开着时点任何地方都不再退出编辑档）。
    let card = controls::PressReporter::new(panel, Message::NotePressInside);
    Some(
        container(Element::from(card))
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(iced::alignment::Horizontal::Left)
            .align_y(iced::alignment::Vertical::Top)
            .padding(Padding::ZERO.left(geom.x).top(geom.y))
            .into(),
    )
}

/// 候选浮层落位：锚在光标右下方，下方放不下且上方放得下时翻到光标上方，
/// 越出右缘时由弹层壳左移钳回（同一套视口钳制）。
fn wiki_picker_geom(
    probe: markdown_edit::CaretProbe,
    viewport: Size,
) -> ui_popup::PopupGeom {
    ui_popup::resolve(
        &PopupSpec {
            anchor: Point::new(probe.x + SUGGEST_GAP_X, probe.y + probe.line_h + SUGGEST_GAP_Y),
            width: SUGGEST_W,
            height: SUGGEST_MAX_H,
            flip_y: Some(probe.y - SUGGEST_GAP_Y),
            min_width: 0.0,
        },
        viewport,
    )
}

/// 反链区（常驻）：引用当前笔记的笔记列表，点击打开。
fn backlinks_section(app: &App) -> Element<'_, Message> {
    // 索引不可用时手上的清单是上一次成功的结果：计数如实标成上次值，不冒充当前反链数
    let n = app.backlinks.len();
    let count_label = match &app.backlinks_error {
        Some(_) => format!("反链 (上次 {n})"),
        None => format!("反链 ({n})"),
    };
    let mut col = iced::widget::Column::<Message>::new()
        .padding(Padding::ZERO.horizontal(16.0).vertical(6.0))
        .spacing(2);
    col = col.push(
        row![
            icon("unlink", 12.0, pal::TEXT_MUTED),
            text(count_label).size(pal::fs_caption()).color(pal::TEXT_SECONDARY),
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
    if let Some(reason) = &app.backlinks_error {
        // 索引不可用与「确实没有反链」分开显示：读不出来不得抹平成空态
        col = col.push(
            row![
                icon("alert", 12.0, pal::WARNING),
                text(format!("索引不可用：{reason}"))
                    .size(pal::fs_micro())
                    .color(pal::WARNING),
            ]
            .spacing(4),
        );
    } else if n == 0 {
        col = col.push(text("暂无其他笔记引用当前笔记").size(pal::fs_micro()).color(pal::TEXT_MUTED));
    }
    container(col).width(Length::Fill).into()
}

// ===== 弹层触发与共享显示名 / 视图查询（弹层内容见 popup/）=====

/// 面板当前生效模型的显示名：覆盖的模型（跨供应商同名带供应商名消歧）；未选择 = 提示文案。
pub(crate) fn chat_model_trigger_label(app: &App) -> String {
    let Some(v) = &app.chat.model_override else {
        return "选择模型".to_string();
    };
    match (
        v.get("providerId").and_then(serde_json::Value::as_str),
        v.get("model").and_then(serde_json::Value::as_str),
    ) {
        (Some(pid), Some(model)) => model_pick_label(app, pid, model),
        _ => "选择模型".to_string(),
    }
}

/// 画布卡当前生效模型显示名：卡指定优先，回落仓库默认模型；未配置 = 提示文案。
pub(crate) fn canvas_card_model_label(app: &App, node_id: &str) -> String {
    match card_model_pick(app, node_id) {
        Some((pid, model)) => model_pick_label(app, &pid, &model),
        None => "选择模型".to_string(),
    }
}

/// 卡模型选择：节点 `providerId` + `model` 成对有效优先，回落仓库默认模型。
pub(crate) fn card_model_pick(app: &App, node_id: &str) -> Option<(String, String)> {
    let specified = app
        .canvas
        .as_ref()
        .and_then(|s| s.data.nodes.iter().find(|n| n.id == node_id))
        .and_then(|n| match (
            n.data.get("providerId").and_then(serde_json::Value::as_str),
            n.data.get("model").and_then(serde_json::Value::as_str),
        ) {
            (Some(p), Some(m)) if !p.is_empty() && !m.is_empty() => {
                Some((p.to_string(), m.to_string()))
            }
            _ => None,
        });
    specified.or_else(|| match (&app.repo_config.model, &app.repo_config.model_provider_id) {
        (Some(m), Some(p)) => Some((p.clone(), m.clone())),
        _ => None,
    })
}

/// 模型显示名（昵称；同名模型跨供应商时带供应商名消歧，供应商已删回落 id）。
fn model_pick_label(app: &App, pid: &str, model: &str) -> String {
    // 昵称取自所指向的供应商（同名模型跨供应商昵称可能不同）
    let display = app
        .repo_config
        .providers
        .iter()
        .flatten()
        .find(|p| p.id == pid)
        .and_then(|p| p.models.iter().find(|m| m.id == model).map(model_display))
        .unwrap_or_else(|| model.to_string());
    // 同名模型跨供应商时带供应商名消歧
    let ambiguous = app
        .repo_config
        .providers
        .iter()
        .flatten()
        .any(|p| p.id != pid && p.models.iter().any(|m| m.id == model));
    if ambiguous {
        let provider_name = app
            .repo_config
            .providers
            .iter()
            .flatten()
            .find(|p| p.id == pid)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| pid.to_string());
        format!("{provider_name} · {display}")
    } else {
        display
    }
}

/// 面板推理等级显示名（触发器/根菜单行用；未选 = 默认）。
pub(crate) fn chat_effort_label(app: &App) -> &'static str {
    effort_label(app.chat.effort.as_deref())
}

/// 画布卡推理等级显示名（节点 `data.reasoningEffort`；未设 = 默认）。
pub(crate) fn canvas_card_effort_label(app: &App, node_id: &str) -> &'static str {
    effort_label(card_effort(app, node_id).as_deref())
}

/// 推理等级线格式 → 显示名。
fn effort_label(effort: Option<&str>) -> &'static str {
    match effort {
        Some("off") => "关闭",
        Some("low") => "低",
        Some("medium") => "中",
        Some("high") => "高",
        _ => "默认",
    }
}

/// 模型显示名：昵称优先（空昵称回落 id）。
pub(crate) fn model_display(m: &atelyx_core::vault::VaultModel) -> String {
    m.nickname
        .clone()
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| m.id.clone())
}

/// 卡节点推理等级（线格式；未设 = None）。
pub(crate) fn card_effort(app: &App, node_id: &str) -> Option<String> {
    app.canvas
        .as_ref()
        .and_then(|s| s.data.nodes.iter().find(|n| n.id == node_id))
        .and_then(|n| {
            n.data
                .get("reasoningEffort")
                .and_then(serde_json::Value::as_str)
        })
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// 角色徽标（所有者 / 编辑者 / 只读）。
pub(crate) fn role_badge(role: atelyx_core::space_client::SpaceRole) -> Element<'static, Message> {
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

/// 该视图是否正展示在任一窗口（主窗口激活布局的各面板激活标签 / 撕裂窗口激活标签）；
/// 面板懒加载（PersistTick 搭载）据此判定是否发起数据任务。
pub fn view_displayed(app: &App, kind: &str) -> bool {
    fn tree_shows(node: &LayoutNode, kind: &str, app: &App) -> bool {
        match node {
            LayoutNode::Panel { id, tabs, active_tab_id } => {
                if layout::primary_panel_of_active(&app.ui) == Some(id.as_str()) {
                    // 主要面板的「前台标签」 = 集合激活项投影出的视图
                    return crate::open_route::primary_active(app)
                        .and_then(|iid| app.open_files.get(&iid).map(|d| crate::open_route::view_of_kind(&d.kind).to_string()))
                        .is_some_and(|v| v == kind);
                }
                tabs.iter()
                    .any(|t| t.view == kind && active_tab_id.as_deref() == Some(t.id.as_str()))
            }
            LayoutNode::Split { children, .. } => children.iter().any(|c| tree_shows(c, kind, app)),
        }
    }
    tree_shows(&layout::active_layout(&app.ui).tree, kind, app)
        || app.ui.detached_windows.iter().any(|w| {
            w.active_tab_id.as_deref().is_some_and(|aid| {
                w.tabs.iter().any(|t| t.id == aid && t.view == kind)
            })
        })
}

/// 当前布局内持有该视图的（面板 id, 标签 id）。
pub fn panel_tab_with_view_any(ui: &crate::layout::AppUiState, view: &str) -> Option<(String, String)> {
    fn find(node: &LayoutNode, view: &str) -> Option<(String, String)> {
        match node {
            LayoutNode::Panel { id, tabs, .. } => tabs
                .iter()
                .find(|t| t.view == view)
                .map(|t| (id.clone(), t.id.clone())),
            LayoutNode::Split { children, .. } => children.iter().find_map(|c| find(c, view)),
        }
    }
    find(&crate::layout::active_layout(ui).tree, view)
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

/// 当前布局内是否存在某视图的面板标签（外部文件拖入的路由判据）。
pub(crate) fn window_has_view(app: &App, kind: &str) -> bool {
    let tree = layout::active_layout(&app.ui).tree;
    let mut tabs = Vec::new();
    layout::collect_tabs(&tree, &mut tabs);
    tabs.iter().any(|t| t.view == kind)
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
/// 窗口当前活动视图是否为指定类型（主窗扫描活动布局树；撕裂窗查其标签）。
pub(crate) fn window_shows_view(app: &App, window: iced::window::Id, view: &str) -> bool {
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

pub fn window_shows_table(app: &App, window: iced::window::Id) -> bool {
    window_shows_view(app, window, "table")
}

/// 窗口当前活动视图是否为画布（画布键盘命令的作用域判定用）。
pub fn window_shows_canvas(app: &App, window: iced::window::Id) -> bool {
    window_shows_view(app, window, "canvas")
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
                                &tab_label(app, t),
                                t.locked,
                                crate::open_route::tab_has_file(app, t),
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
        LayoutNode::Panel { id, tabs, .. } => {
            // 主要面板的标签条宽度按集合投影记账（命中几何与渲染同源）
            let widths: Vec<f32> = if layout::primary_panel_of_active(&app.ui) == Some(id.as_str()) {
                crate::open_route::primary_tabs(app)
                    .iter()
                    .map(|t| approx_tab_width(&tab_label(app, t), false, true))
                    .collect()
            } else {
                tabs.iter()
                    .map(|t| {
                        approx_tab_width(&tab_label(app, t), t.locked, crate::open_route::tab_has_file(app, t))
                    })
                    .collect()
            };
            out.push(PanelZone {
                panel_id: Some(id.clone()),
                detached_id: None,
                rect: area,
                tab_widths: widths,
            })
        }
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
        device_meta_line, divider_size, ext_badge_reserve, file_label, merge_peers, name_slot,
        row_window, wiki_picker_geom, UiMetrics, SUGGEST_W,
    };
    use crate::ui::palette as pal;
    use crate::ui::text::{ellipsize_estimated, estimate_width};
    use atelyx_core::collab::PeerInfo;
    use iced::Length;

    #[test]
    fn wiki_picker_lands_below_and_right_of_caret() {
        // 候选浮层锚在输入点右下方：x 让出光标竖条，y 落到光标行之下
        let probe = crate::markdown_edit::CaretProbe { x: 200.0, y: 300.0, line_h: 22.0 };
        let geom = wiki_picker_geom(probe, iced::Size::new(1200.0, 800.0));
        assert!(geom.x > probe.x, "浮层应在光标右侧");
        assert!(geom.y >= probe.y + probe.line_h, "浮层应在光标行下方");
        assert_eq!(geom.w, SUGGEST_W);
    }

    #[test]
    fn wiki_picker_flips_above_near_bottom() {
        // 贴窗口底边时翻到光标上方，仍留在视口内
        let probe = crate::markdown_edit::CaretProbe { x: 200.0, y: 780.0, line_h: 22.0 };
        let geom = wiki_picker_geom(probe, iced::Size::new(1200.0, 800.0));
        assert!(geom.y < probe.y, "下方放不下应翻到上方");
    }

    #[test]
    fn wiki_picker_clamps_inside_right_edge() {
        // 贴右缘时整体左移，不溢出视口
        let probe = crate::markdown_edit::CaretProbe { x: 1150.0, y: 100.0, line_h: 22.0 };
        let geom = wiki_picker_geom(probe, iced::Size::new(1200.0, 800.0));
        assert!(geom.x + geom.w <= 1200.0, "浮层右缘不得越出视口");
    }

    #[test]
    fn divider_thickness_follows_split_axis() {
        let bar = Length::Fixed(UiMetrics::DIVIDER_W);
        assert_eq!(divider_size(true), (bar, Length::Fill));
        assert_eq!(divider_size(false), (Length::Fill, bar));
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
    fn device_meta_line_marks_missing_fields_unknown() {
        assert_eq!(device_meta_line(Some("windows"), Some("0.6.1")), "Windows · 0.6.1");
        assert_eq!(device_meta_line(Some("linux"), None), "Linux · 未知版本");
        assert_eq!(device_meta_line(None, Some("0.6.1")), "未知平台 · 0.6.1");
        assert_eq!(device_meta_line(None, None), "未知平台 · 未知版本");
        assert_eq!(device_meta_line(Some(""), Some(" ")), "未知平台 · 未知版本");
    }

    fn peer(nick: &str, device: &str, platform: Option<&str>, files: &[&str]) -> PeerInfo {
        PeerInfo {
            peer_id: nick.to_string(),
            nickname: nick.to_string(),
            color: "#30bced".into(),
            device_name: device.into(),
            platform: platform.map(str::to_string),
            version: None,
            presence: Some(serde_json::json!({
                "openFiles": files.iter().map(|f| serde_json::json!({ "file": f })).collect::<Vec<_>>()
            })),
        }
    }

    #[test]
    fn merge_peers_unions_files_and_keeps_known_identity() {
        let rows = merge_peers(&[
            peer("小明", "桌面端", None, &["a.md"]),
            peer("小明", "桌面端", Some("windows"), &["b.atb", "a.md"]),
            peer("小明", "笔记本", Some("linux"), &[]),
        ]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].files, ["a.md", "b.atb"]);
        assert_eq!(rows[0].platform.as_deref(), Some("windows"));
        assert_eq!(rows[1].platform.as_deref(), Some("linux"));
    }

    #[test]
    fn name_slot_reserves_room_for_ext_badge() {
        // 后缀越长预留越宽；无后缀不预留，槽位吃满整行
        assert_eq!(ext_badge_reserve(""), 0.0);
        assert!(ext_badge_reserve("CANVAS") > ext_badge_reserve("MD"));
        assert_eq!(name_slot(120.0, ""), 120.0);
        assert!(name_slot(120.0, "MD") < 120.0);
        // 窄行钳到 0：名称退到省略号，徽标仍按行宽贴右缘
        assert_eq!(name_slot(2.0, "CANVAS"), 0.0);
    }

    #[test]
    fn ellipsized_name_fits_the_badge_slot() {
        let slot = name_slot(120.0, "CANVAS");
        let shown = ellipsize_estimated("一个很长的文件名用来测槽位宽度.md", slot, pal::fs_caption());
        assert!(estimate_width(&shown, pal::fs_caption()) <= slot + 0.01);
    }

    #[test]
    fn row_window_keeps_viewport_with_overscan() {
        let heights = vec![100.0; 100];
        let (range, top, bottom) = row_window(&heights, 1000.0, 400.0).expect("窗口可用");
        assert!(range.start <= 10, "起点应向前留余量：{range:?}");
        assert!(range.end >= 14, "终点应覆盖视口下缘：{range:?}");
        assert_eq!(top, range.start as f32 * 100.0);
        assert_eq!(bottom, (100 - range.end) as f32 * 100.0);
    }

    #[test]
    fn row_window_at_top_starts_at_zero() {
        let heights = vec![20.0; 10];
        let (range, top, _) = row_window(&heights, 0.0, 60.0).expect("窗口可用");
        assert_eq!(range.start, 0);
        assert_eq!(top, 0.0);
        assert!(range.end >= 3);
    }

    #[test]
    fn row_window_requires_rows_and_viewport() {
        assert!(row_window(&[], 0.0, 100.0).is_none());
        assert!(row_window(&[10.0], 0.0, 0.0).is_none());
    }
}
