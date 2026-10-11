//! Atelyx 原生壳：daemon 多窗口 + 布局模型（场景/布局/面板树）+ files/note 视图。
//!
//! 布局状态真源 = atelyx-core::layout（与老壳共用同一 ui-state.json 磁盘格式，
//! 本壳只读不改格式）；本壳负责窗口副作用（OS 窗口、落盘防抖、输入路由）。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod layout_view;
mod markdown_edit;
mod markdown_view;
mod ui;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use atelyx_core::layout::{
    self, apply_layout_op, normalize, op_passes_shape_check, AppUiState, DetachedWindow,
    LayoutOp, WindowBounds, UI_STATE_SCHEMA,
};
use atelyx_core::markdown::{LinkForm, MarkdownDocument};
use atelyx_core::vault;
use iced::widget::{space, text_editor};
use iced::window::{self, Level, Mode, Position, Settings};
use iced::{keyboard, mouse, Element, Event, Point, Size, Subscription, Task, Theme};

use markdown_edit::HitTarget;
use ui::notify::{Level as NoticeLevel, Notification};

use layout_view::{
    enumerate_dividers, panel_tab_ids_in, PopupOpen, RenameState, RenameTarget, UiMetrics,
};

/// 撕裂窗口建窗参数：bounds 恢复位置 + 条目 OS 选项（不进任务栏）；无系统边框。
fn panel_settings(w: &DetachedWindow) -> Settings {
    let mut settings = Settings {
        size: Size::new(w.bounds.width.max(200.0) as f32, w.bounds.height.max(160.0) as f32),
        position: Position::Specific(Point::new(w.bounds.x as f32, w.bounds.y as f32)),
        exit_on_close_request: false,
        decorations: false,
        ..Settings::default()
    };
    #[cfg(windows)]
    {
        settings.platform_specific.skip_taskbar = w.options.skip_taskbar;
    }
    settings
}

/// note 视图模式：只读渲染 / 预览编辑 / 源码。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteMode {
    Read,
    Edit,
    Source,
}

/// 剥离开头的 frontmatter 块（阅读面只渲染正文）；缺闭合定界符时按无 frontmatter 处理，全量正文。
fn strip_frontmatter(text: &str) -> &str {
    let head_end = match text.find('\n') {
        Some(i) => i + 1,
        None => return text,
    };
    if text[..head_end].trim_end() != "---" {
        return text;
    }
    let mut offset = head_end;
    for line in text[head_end..].split_inclusive('\n') {
        let end = offset + line.len();
        let marker = line.trim_end_matches(['\r', '\n']);
        if marker == "---" || marker == "..." {
            return &text[end..];
        }
        offset = end;
    }
    text
}

/// 前一个字符边界（`at` 为字符边界）。
fn prev_boundary(text: &str, at: usize) -> usize {
    let mut i = at.min(text.len());
    if i == 0 {
        return 0;
    }
    i -= 1;
    while i > 0 && !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// 后一个字符边界。
fn next_boundary(text: &str, at: usize) -> usize {
    let mut i = (at + 1).min(text.len());
    while i < text.len() && !text.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// `at` 所在行行首。
fn line_start_of(text: &str, at: usize) -> usize {
    let at = at.min(text.len());
    text[..at].rfind('\n').map_or(0, |i| i + 1)
}

/// `at` 所在行行尾。
fn line_end_of(text: &str, at: usize) -> usize {
    let at = at.min(text.len());
    text[at..].find('\n').map_or(text.len(), |i| at + i)
}

/// 百分号解码（链接 href）。
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// 外链交系统浏览器打开：只放行常见协议，命令行走原样拼接并整串加引号，
/// 拒绝引号/百分号/换行——避免 URL 内容被命令行解释成命令分隔或变量展开。
fn open_external(url: &str) {
    let scheme_ok = url.starts_with("http://") || url.starts_with("https://") || url.starts_with("mailto:");
    let clean = !url.contains('"') && !url.contains('%') && !url.contains('\n') && !url.contains('\r');
    if !scheme_ok || !clean {
        return;
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("cmd")
            .raw_arg(format!("/C start \"\" \"{url}\""))
            .spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
}

fn main() -> iced::Result {
    iced::daemon(App::boot, App::update, App::view)
        .title(App::title)
        .theme(|app: &App, _id| app.theme.clone())
        .default_font(iced::Font::with_name("IBM Plex Sans"))
        .subscription(App::subscription)
        .run()
}

const WINDOW_SIZE: Size = Size::new(1100.0, 760.0);
/// 防抖落盘周期：期间多次模型变更合并为一次写盘（拖宽/拖拽等连续操作不逐帧写）。
const PERSIST_TICK: std::time::Duration = std::time::Duration::from_millis(800);

/// ui-state.json 落盘路径（与老壳同一应用数据目录，bundle identifier = com.atelyx.desktop）。
fn ui_state_path() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var("APPDATA")
            .ok()
            .map(|d| PathBuf::from(d).join("com.atelyx.desktop").join("ui-state.json"))
    }
    #[cfg(not(windows))]
    {
        std::env::var("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|_| std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".local/share")))
            .ok()
            .map(|d| d.join("com.atelyx.desktop").join("ui-state.json"))
    }
}

/// 从磁盘恢复布局状态：缺失/损坏/schema 不符一律回落默认态（与老壳同口径）。
fn load_ui_state() -> AppUiState {
    let mut ui = match ui_state_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|json| serde_json::from_str::<AppUiState>(&json))
    {
        Some(Ok(ui)) if ui.schema == UI_STATE_SCHEMA => ui,
        _ => AppUiState::default(),
    };
    normalize(&mut ui);
    ui
}

/// 分割把手拖拽在途：尺寸调整基于按下时冻结的起点（轴坐标/区域总长/尺寸数组）。
struct DividerDrag {
    split_id: String,
    gap: usize,
    horizontal: bool,
    drag_start: f32,
    split_len: f32,
    sizes: Vec<f64>,
}

#[derive(Debug, Clone)]
enum Message {
    WindowOpened(window::Id),
    WindowCloseRequested(window::Id),
    WindowClosed(window::Id),
    PickVault,
    VaultPicked(Result<PathBuf, String>),
    ToggleDir(String),
    /// gen = 发起任务时的仓库世代；回调落地前与当前世代比对，过期即丢弃。
    ChildrenLoaded(String, u64, Result<Vec<(String, bool)>, String>),
    OpenFile(String),
    FileLoaded(u64, Result<(String, String), String>),
    Edited(text_editor::Action),
    Save,
    /// 携带保存发起时冻结的内容：落地后与编辑器现值比对，区分保存完成与在途新编辑。
    Saved(u64, String, Result<(), String>),
    /// 切换 note 视图模式（只读 / 预览编辑 / 源码）。
    NoteMode(NoteMode),
    /// 编辑面事件（部件只上报，正文/光标/选区真相在应用层）。
    Editor(markdown_edit::EditorEvent),
    /// 打开链接（外链 / 仓库路径 / wiki）。
    OpenLink(String, LinkForm),
    /// 切换任务勾选框（携带列表项源起点）。
    ToggleTask(usize),
    /// 复制代码块正文到系统剪贴板。
    CopyCode(String),
    PanelTab(String, String),
    /// 空面板右键添加视图。
    PanelAddView { panel_id: String, view: String },
    /// 右键切换标签视图。
    SetTabView { panel_id: String, tab_id: String, view: String },
    /// 右键切换撕裂窗口标签视图。
    DetachedSetTabView { window_id: String, tab_id: String, view: String },
    /// 关闭文件（×：摘下文件、标签保留回未打开态）。
    PanelTabFileClose,
    DetachedTabFileClose,
    /// 标签按下：拖拽候选由标签按钮自证命中（渲染自适应后几何不再可靠）。
    PanelTabPress { panel_id: String, tab_id: String },
    DetachedTabPress { window_id: String, tab_id: String },
    LayoutTabPress { layout_id: String, name: String },
    PanelTabClose(String, String),
    /// 整块锁定/解锁面板全部标签。
    PanelTabLock(String),
    PanelSplit(String, String),
    PanelClose(String),
    DetachedTab(String, String),
    DetachedTabClose(String, String),
    /// 整块锁定/解锁撕裂窗口全部标签。
    DetachedTabLock(String),
    DetachedDockBack(String),
    DetachedPin(String),
    DetachedFocusLost(window::Id),
    WindowMoved(window::Id, Point),
    ScaleKnown(window::Id, f32),
    LayoutActivate(String),
    LayoutNew,
    LayoutRenameStart(String),
    LayoutRenameInput(String),
    LayoutRenameCommit,
    LayoutDelete(String),
    /// 布局 tab 拖拽排序落点（1-based 合成序，主页 = 0 不可动）。
    LayoutDropped(usize, usize),
    /// 标签条横向滚动偏移（None = 标题栏布局条，Some = 主窗面板 id；拖拽命中几何对齐渲染）。
    TabStripScrolled(Option<String>, f32),
    SceneActivate(String),
    SceneNew,
    SceneRenameStart(String),
    SceneRenameInput(String),
    SceneDelete(String),
    /// 场景删除确认 pane 入口（弹层内二段确认）。
    SceneDeleteAsk(String),
    PersistTick,
    PersistDone(Result<(), String>),
    /// 通知过期、悬停提示显现/收起与提醒条过期的统一心跳。
    UiTick,
    WindowResized(window::Id, Size),
    /// 窗口位置播种（建窗后主动查询；OS 移动事件为增量权威）。
    PositionKnown(window::Id, Option<Point>),
    Press(window::Id),
    CursorMoved(window::Id, Point),
    Release,
    /// ui 层消息（悬停提示 / 弹层关闭与面板导航 / 通知关闭）。
    Ui(ui::UiMsg),
    /// 标题栏空白区按下：拖拽窗口 / 双击最大化。
    TitleBarPress(window::Id),
    WindowMinimize(window::Id),
    WindowMaximizeToggle(window::Id),
    /// 全屏切换（查询当前模式 → 反转）。
    FullscreenToggle(window::Id),
    ApplyFullscreen(window::Id, iced::window::Mode),
    /// Esc：先关弹层，再取消行内重命名。
    Escape(window::Id),
    /// 打开 / 关闭设置整页；设置左栏切 tab；折叠钮切换。
    SettingsToggle,
    SettingsTab(String),
    SettingsNavCollapse,
    /// 打开弹层（几何规格 + 内容形态由 layout_view 定义）。
    PopupOpen(window::Id, PopupOpen),
    /// 仓库文件总数（打开仓库后异步统计；gen 守卫）。
    FileCount(u64, Result<usize, String>),
    /// 文件树：新建草稿输入行（落点 = 目标目录内部）。
    DraftStart(String, DraftKind),
    /// 文件树：就地重命名起点。
    RenameStartFile(String),
    RenameStartFolder(String),
    /// 删除入口：文件直接进确认 pane；文件夹先计数（空目录免确认）。
    DeleteAsk(String),
    FolderDeleteAsk(String),
    /// 文件夹条目数落地（core 仅对非空目录回 needs_confirm；带条目数进确认 pane）。
    FolderCounted(String, usize),
    /// 取消删除确认。
    DeleteCancel,
    /// 文件/文件夹删除（菜单内确认后触发）。
    DeleteEntry(String, bool),
    /// 仓库行：在系统文件管理器中打开仓库根。
    RevealVault,
    /// 文件操作落地（gen 守卫 + 结果通知 + 刷新子项 + 打开文件跟随）。
    OpDone(u64, String, Result<String, String>, FollowUp),
    /// 展开全部/收起全部文件夹（懒加载树递归展开）。
    ExpandAllToggle,
    /// 递归展开过程中的子项落地（累计入展开集并链式下发）。
    ExpandChildrenLoaded(String, u64, Result<Vec<(String, bool)>, String>),
    /// 行内输入行被按下（重命名失焦提交的「点在输入行内」标志）。
    RenamePressInside,
    /// 分割把手键盘调整（上次拖动过的把手；horizontal + 正向）。
    DividerAdjust(bool, bool),
    /// 保存失败徽标的关闭。
    SaveErrorDismiss,
    /// 未实现功能的占位通知。
    NotImplemented(&'static str),
}

/// 文件操作成功后对打开中笔记的跟随语义。
#[derive(Debug, Clone)]
pub enum FollowUp {
    None,
    /// 改名/移动：打开中笔记按前缀跟随。
    Renamed { from: String, to: String },
    /// 删除：命中打开中笔记即清空编辑器。
    Deleted { rel: String, is_dir: bool },
}

/// 相对路径的父目录（根为 ""）。
fn parent_of(rel: &str) -> String {
    match rel.rfind('/') {
        Some(i) => rel[..i].to_string(),
        None => String::new(),
    }
}

/// 目录相对路径 + 条目名拼完整相对路径（dir = "" 即根）。
fn full_rel(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

/// 分割把手两侧配额钳制：双侧各不小于 min，和不足以容纳双下限时对半分。
fn clamp_split_pair(s0: f64, s1: f64, min: f64) -> (f64, f64) {
    let pair = s0 + s1;
    if pair < min * 2.0 {
        (pair / 2.0, pair / 2.0)
    } else {
        let c0 = s0.clamp(min, pair - min);
        (c0, pair - c0)
    }
}

/// 新建草稿种类（决定扩展名与占位文案）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DraftKind {
    Note,
    Folder,
}

/// 跨窗口标签/布局 tab 拖拽会话：源窗口只产生输入事件，落点由全局面板几何命中。
struct DragSession {
    tab_id: String,
    /// ghost 图标来源（视图 kind；布局 tab 拖拽为空串 → 拼图兜底）。
    view: String,
    /// ghost 文案（视图名 / 布局名）。
    label: String,
    from_panel: Option<String>,
    from_detached: Option<String>,
    /// 布局 tab 拖拽的 1-based 合成序（标签拖拽恒为 0）。
    layout_from: usize,
    /// 全局光标（源窗口位置 + 本地坐标）。
    current: Point,
    /// 光标当前所在窗口（ghost 显示归属）。
    cursor_window: window::Id,
}

/// 拖拽悬停落点。
struct DragHover {
    zone: layout_view::PanelZone,
    kind: layout_view::DropZone,
}

/// 拖拽候选来源（按下时由标签按钮自证命中，转正时再查锁定与标签信息）。
enum PressSource {
    Panel { panel_id: String, tab_id: String },
    Detached { window_id: String, tab_id: String },
    Layout { layout_id: String, name: String },
}

/// 拖拽按下候选（移动超阈值转正为拖拽会话；未转正释放 = 按钮自行激活）。
struct TabPress {
    window: window::Id,
    source: PressSource,
}

pub struct App {
    /// 布局状态真源（core 模型；与老壳共用磁盘格式）。
    pub ui: AppUiState,
    vault: Arc<vault::VaultState>,
    pub theme: Theme,
    /// 应用内通知（右下角；自动消失）。
    pub notifications: Vec<Notification>,
    notify_seq: u64,
    /// 悬停提示（图标按钮；显现/收起由心跳按光标-锚点距离判定；
    /// 事件 enter 仅负责启动——批量事件中 exit 会被捕获状态吞掉，不可靠）。
    pub tooltip: Option<(window::Id, String, Instant, bool, Option<Instant>, Point)>,
    /// 触发钮半边长（提示锚定其下缘；菜单项 = 半行高）。
    pub tooltip_half: Option<f32>,
    /// 重名自动加序号的提醒条（文案 + 出现时刻；3s 自动消失）。
    pub rename_notice: Option<(String, Instant)>,
    /// 设置左栏折叠态。
    pub settings_collapsed: bool,
    /// 删除确认流（目标相对路径、是否目录、文件夹条目数）。
    pub delete_confirm: Option<(String, bool, Option<usize>)>,
    /// 展开全部文件夹进行中（懒加载递归展开的图标/文案双态）。
    pub expand_all: bool,
    expand_pending: usize,
    /// 各窗口最近光标位置（右键菜单锚点 / 提示定位）。
    pub cursors: HashMap<window::Id, Point>,
    /// 打开的弹层（同窗口同时至多一个）。
    pub popup: Option<(window::Id, PopupOpen)>,
    /// 弹层面板导航（两 pane 菜单）。
    pub popup_pane: usize,
    /// 设置整页开启态与左栏 tab。
    pub settings_open: bool,
    pub settings_tab: String,
    /// 当前仓库根（打开仓库后与 vault.root() 一致；仅用于展示与 files 视图判空）。
    pub vault_root: Option<PathBuf>,
    /// 仓库文件总数（打开仓库后异步统计；统计在途为 None）。
    pub file_count: Option<usize>,
    /// 主窗口（渲染激活布局树）；None = 首帧在途。
    pub main_id: Option<window::Id>,
    /// 撕裂窗口 OS 注册：iced 窗口 id → 模型窗口 id（建窗前预注册，见 reconcile）。
    detached_os: HashMap<window::Id, String>,
    /// OS 窗口当前可见态（调和比对基准；建窗即视为可见）。
    os_shown: HashMap<window::Id, bool>,
    /// files 视图懒加载子项缓存（key = 目录相对路径，根为 ""；运行态不落盘）。
    pub children: HashMap<String, Vec<(String, bool)>>,
    /// note 正文真相（与磁盘逐字节一致；源码档与编辑档共用）。
    pub note_text: String,
    /// 源码档编辑器（进出档按正文重建）。
    pub source_editor: text_editor::Content,
    /// 编辑档状态（光标 / 选区 / 活动块 / 撤销栈 / 组合态）。
    pub edit: markdown_edit::EditorState,
    /// 编辑档解析产物与块元数据（正文或档位变更时重建）。
    pub edit_doc: Option<MarkdownDocument>,
    pub edit_meta: Vec<markdown_view::BlockMeta>,
    /// 编辑档解析选项（链接形态判定；进程内只建一次）。
    pub edit_opts: atelyx_core::markdown::ParseOptions,
    pub dirty: bool,
    /// 上次保存失败原因（None = 无；保存成功或再次编辑即清除）。
    pub save_error: Option<String>,
    /// note 视图模式（缺省只读）。
    pub note_mode: NoteMode,
    /// 只读档的解析产物：进入只读档或文件载入时重建，避免每帧重解析。
    pub preview_doc: Option<MarkdownDocument>,
    /// 防抖落盘：有未写盘的模型变更。
    persist_dirty: bool,
    /// 主窗口内容区尺寸（logical；Resized 驱动，把手命中几何用）。
    pub main_size: Size,
    /// 各窗口逻辑位置（建窗播种 + Moved 增量；拖拽全局坐标换算用）。
    pub os_positions: HashMap<window::Id, Point>,
    /// 撕裂窗口逻辑尺寸（建窗播种 + Resized 增量；落点矩形用）。
    pub os_sizes: HashMap<window::Id, Size>,
    divider: Option<DividerDrag>,
    /// 上次拖动过的把手（键盘方向键 ±2% 调整的承接对象）。
    last_divider: Option<(String, usize, bool)>,
    /// 按压起点（本地坐标；移动超阈值 = 拖标签手势）。
    press: Option<(window::Id, Point)>,
    /// 标签/布局 tab 拖拽按下候选（标签按钮按下时自证命中）。
    tab_press: Option<TabPress>,
    /// 行内输入行按压标志（RenamePressInside 置位，全局 Press 消费后清零）。
    rename_press_inside: bool,
    drag: Option<DragSession>,
    drag_hover: Option<DragHover>,
    /// 各仓库独立的文件树展开集（会话内记忆；ui-state 仍只持久化当前仓库一份）。
    expanded_by_vault: HashMap<PathBuf, Vec<String>>,
    pub rename: Option<RenameState>,
    /// 上次标题栏空白按下（窗口 id + 时刻；双击最大化判定）。
    last_titlebar_press: Option<(window::Id, Instant)>,
    /// 标题栏布局条横向滚动偏移（拖拽命中几何与渲染对齐）。
    titlebar_tab_scroll_x: f32,
    /// 各面板标签条横向滚动偏移（同上；key = 面板 id）。
    panel_tab_scroll_x: HashMap<String, f32>,
}

impl App {
    fn boot() -> (Self, Task<Message>) {
        let (main_os, open) = window::open(Settings {
            size: WINDOW_SIZE,
            exit_on_close_request: false,
            decorations: false,
            ..Settings::default()
        });
        ui::icon::init();
        // 字体加载完成前首帧以系统回退字体渲染，随后自动替换
        let fonts = iced::font::load(ui::FONT_REGULAR)
            .chain(iced::font::load(ui::FONT_MEDIUM))
            .chain(iced::font::load(ui::FONT_SEMIBOLD))
            .chain(iced::font::load(ui::FONT_DISPLAY))
            .discard();
        (
            Self {
                ui: load_ui_state(),
                vault: Arc::new(vault::VaultState::default()),
                theme: ui::palette::app_theme(),
                notifications: Vec::new(),
                notify_seq: 0,
                tooltip: None,
                tooltip_half: None,
                rename_notice: None,
                settings_collapsed: false,
                delete_confirm: None,
                expand_all: false,
                expand_pending: 0,
                cursors: HashMap::new(),
                popup: None,
                popup_pane: 0,
                settings_open: false,
                settings_tab: "general".into(),
                vault_root: None,
                file_count: None,
                main_id: None,
                detached_os: HashMap::new(),
                os_shown: HashMap::new(),
                children: HashMap::new(),
                note_text: String::new(),
                source_editor: text_editor::Content::new(),
                edit: markdown_edit::EditorState::default(),
                edit_doc: None,
                edit_meta: Vec::new(),
                edit_opts: markdown_view::link_options(),
                dirty: false,
                save_error: None,
                note_mode: NoteMode::Read,
                preview_doc: None,
                persist_dirty: false,
                main_size: WINDOW_SIZE,
                os_positions: HashMap::new(),
                os_sizes: HashMap::new(),
                divider: None,
                last_divider: None,
                press: None,
                tab_press: None,
                rename_press_inside: false,
                drag: None,
                drag_hover: None,
                expanded_by_vault: HashMap::new(),
                rename: None,
                last_titlebar_press: None,
                titlebar_tab_scroll_x: 0.0,
                panel_tab_scroll_x: HashMap::new(),
            },
            open.map(Message::WindowOpened)
                .chain(window::position(main_os).map(move |p| Message::PositionKnown(main_os, p)))
                .chain(fonts),
        )
    }

    /// 追加一条通知（右下角；容量上限防堆积）。
    fn notify(&mut self, level: NoticeLevel, text: impl Into<String>) {
        self.notify_seq += 1;
        self.notifications.push(Notification {
            id: self.notify_seq,
            level,
            text: text.into(),
            at: Instant::now(),
        });
        if self.notifications.len() > 5 {
            self.notifications.remove(0);
        }
    }

    /// 世代内的光标位置（按下事件不带坐标，命中判定用最近位置）。
    fn cursor_for(&self, window: window::Id) -> Point {
        self.cursors.get(&window).copied().unwrap_or(Point::ORIGIN)
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::WindowOpened(id) => {
                if self.main_id.is_none() {
                    self.main_id = Some(id);
                    // 主窗口就绪即调和：启动恢复 restore_on_launch 的撕裂窗口
                    return self.reconcile();
                }
                Task::none()
            }
            Message::WindowCloseRequested(id) => {
                if self.main_id == Some(id) {
                    if self.dirty {
                        self.notify(NoticeLevel::Warning, "有未保存改动，先 Ctrl+S 保存");
                        return Task::none();
                    }
                    return window::close(id);
                }
                // 撕裂窗口按条目选项分流：close_hides = 隐藏不销毁，否则销毁条目
                let Some(model_id) = self.detached_os.get(&id).cloned() else {
                    return window::close(id);
                };
                let close_hides = self
                    .ui
                    .detached_windows
                    .iter()
                    .find(|w| w.id == model_id)
                    .is_some_and(|w| w.options.close_hides);
                if close_hides {
                    self.apply(LayoutOp::HideDetachedWindow { window_id: model_id })
                } else {
                    self.apply(LayoutOp::RemoveDetachedWindow { window_id: model_id })
                }
            }
            Message::WindowClosed(id) => {
                if self.main_id == Some(id) {
                    iced::exit()
                } else {
                    self.detached_os.remove(&id);
                    self.os_shown.remove(&id);
                    // 光标记录随窗口消失；该窗在途悬停提示立即清除
                    // （cursors 缺失会让收起判定恒 false，提示与心跳订阅悬挂）
                    self.cursors.remove(&id);
                    if self.tooltip.as_ref().is_some_and(|(w, _, _, _, _, _)| *w == id) {
                        self.tooltip = None;
                    }
                    Task::none()
                }
            }
            Message::PickVault => {
                if self.dirty {
                    self.notify(NoticeLevel::Warning, "有未保存改动，先 Ctrl+S 保存");
                    return Task::none();
                }
                Task::perform(
                    async move {
                        match rfd::AsyncFileDialog::new().pick_folder().await {
                            Some(handle) => Ok(handle.path().to_path_buf()),
                            None => Err("未选择文件夹".into()),
                        }
                    },
                    Message::VaultPicked,
                )
            }
            Message::VaultPicked(Ok(path)) => {
                // 展开集按仓库独立：切走时记忆当前仓库的集合，切回时恢复
                if let Some(old) = self.vault_root.clone() {
                    self.expanded_by_vault.insert(old, self.ui.file_explorer_expanded.clone());
                }
                if let Err(e) = self.vault.set(path.clone(), Vec::new()) {
                    self.notify(NoticeLevel::Error, e);
                    return Task::none();
                }
                self.vault_root = self.vault.root().ok();
                self.notify(NoticeLevel::Success, "仓库已打开");
                self.children.clear();
                self.note_text.clear();
                self.source_editor = text_editor::Content::new();
                self.edit.reset(0);
                self.edit_doc = None;
                self.edit_meta.clear();
                self.preview_doc = None;
                self.dirty = false;
                self.save_error = None;
                self.file_count = None;
                self.expand_all = false;
                self.expand_pending = 0;
                self.rename_notice = None;
                // last_note_file 跨仓库沿用（相对路径语义）；仓库就绪即恢复其内容
                self.ui.file_explorer_expanded = self
                    .expanded_by_vault
                    .get(&path)
                    .cloned()
                    .unwrap_or_default();
                let mut task = self.load_children("").chain(self.count_files());
                if let Some(rel) = self.ui.last_note_file.clone() {
                    task = task.chain(self.load_note(&rel));
                }
                task
            }
            Message::VaultPicked(Err(e)) => {
                self.notify(NoticeLevel::Error, e);
                Task::none()
            }
            Message::ToggleDir(rel) => {
                let expanded = &mut self.ui.file_explorer_expanded;
                if let Some(i) = expanded.iter().position(|d| d == &rel) {
                    expanded.remove(i);
                } else {
                    expanded.push(rel.clone());
                }
                self.schedule_persist();
                if !self.children.contains_key(&rel) {
                    return self.load_children(&rel);
                }
                Task::none()
            }
            Message::ChildrenLoaded(dir, gen, Ok(entries)) => {
                if gen != self.epoch() {
                    return Task::none();
                }
                self.children.insert(dir, entries);
                Task::none()
            }
            Message::ChildrenLoaded(_, _, Err(e)) => {
                self.notify(NoticeLevel::Error, format!("读取目录失败：{e}"));
                Task::none()
            }
            Message::OpenFile(rel) => {
                if self.dirty {
                    self.notify(NoticeLevel::Warning, "有未保存改动，先 Ctrl+S 保存");
                    return Task::none();
                }
                self.ui.last_note_file = Some(rel.clone());
                // 打开文件复位只读：笔记视图按文件重新挂载，档位不跨文件延续
                self.note_mode = NoteMode::Read;
                // 清掉上一文件的解析产物：载入在途期间两档都不留旧内容
                self.preview_doc = None;
                self.edit_doc = None;
                self.edit_meta.clear();
                self.schedule_persist();
                self.load_note(&rel)
            }
            Message::FileLoaded(gen, Ok((rel, text))) => {
                if gen != self.epoch() || self.ui.last_note_file.as_deref() != Some(rel.as_str()) {
                    return Task::none();
                }
                self.note_text = text;
                self.source_editor = text_editor::Content::with_text(&self.note_text);
                self.edit.reset(0);
                self.dirty = false;
                self.refresh_preview();
                self.refresh_edit();
                self.notify(NoticeLevel::Success, "已打开");
                Task::none()
            }
            Message::FileLoaded(_, Err(e)) => {
                // 载入失败必须清空编辑器：last_note_file 已指向新文件，残留旧内容
                // 会让 Ctrl+S 把旧内容写进新路径
                self.note_text.clear();
                self.source_editor = text_editor::Content::new();
                self.edit.reset(0);
                self.edit_doc = None;
                self.edit_meta.clear();
                self.dirty = false;
                self.preview_doc = None;
                self.notify(NoticeLevel::Error, e);
                Task::none()
            }
            Message::Edited(action) => {
                // 动作须全部执行（widget 只发布不应用）；置脏只看内容是否实际变化
                let may_edit = action.is_edit();
                let before = may_edit.then(|| self.source_editor.text());
                self.source_editor.perform(action);
                if let Some(before) = before {
                    let after = self.source_editor.text();
                    if after != before {
                        self.note_text = after;
                        self.dirty = true;
                        self.save_error = None;
                        self.refresh_edit();
                    }
                }
                Task::none()
            }
            Message::Save => {
                let Some(rel) = self.ui.last_note_file.clone() else {
                    self.notify(NoticeLevel::Warning, "没有打开的文件");
                    return Task::none();
                };
                // 仓库根在发起时冻结：写盘目标固定为打开该文件时的仓库。
                let Ok(root) = self.vault.root() else {
                    self.notify(NoticeLevel::Warning, "仓库未打开");
                    return Task::none();
                };
                let gen = self.epoch();
                let content = self.note_text.clone();
                let saved = content.clone();
                Task::perform(
                    async move {
                        let path = vault::safe_join(&root, &rel, false)?;
                        vault::atomic_write(&path, &content)
                    },
                    move |result| Message::Saved(gen, saved, result),
                )
            }
            Message::Saved(gen, saved, Ok(())) => {
                if gen != self.epoch() {
                    return Task::none();
                }
                self.save_error = None;
                // 保存发起时冻结的内容与当前一致才算保存完成；在途期间的新编辑保持脏标记
                if self.note_text == saved {
                    self.dirty = false;
                    self.notify(NoticeLevel::Success, "已保存");
                } else {
                    self.notify(NoticeLevel::Info, "保存完成，期间有新修改");
                }
                Task::none()
            }
            Message::Saved(_, _, Err(e)) => {
                self.save_error = Some(e.clone());
                self.notify(NoticeLevel::Error, format!("保存失败：{e}"));
                Task::none()
            }
            Message::NoteMode(mode) => {
                match mode {
                    NoteMode::Read => {
                        self.note_mode = NoteMode::Read;
                        self.edit_doc = None;
                        self.edit_meta.clear();
                        self.edit.focused = false;
                        self.refresh_preview();
                    }
                    NoteMode::Edit => {
                        self.note_mode = NoteMode::Edit;
                        self.preview_doc = None;
                        // 进入编辑档不揭示任何块、不持光标：首帧即全篇按块渲染，无源码回显
                        self.edit.active = None;
                        self.edit.preedit = None;
                        self.edit.focused = false;
                        self.refresh_edit();
                    }
                    NoteMode::Source => {
                        self.note_mode = NoteMode::Source;
                        self.preview_doc = None;
                        self.edit_doc = None;
                        self.edit_meta.clear();
                        self.edit.focused = false;
                        self.source_editor = text_editor::Content::with_text(&self.note_text);
                    }
                }
                Task::none()
            }
            Message::Editor(event) => self.on_editor(event),
            Message::OpenLink(href, form) => self.open_link(&href, form),
            Message::ToggleTask(item_from) => {
                if let Some((next, at)) = markdown_edit::toggle_task(&self.note_text, item_from) {
                    self.edit.snapshot(&self.note_text);
                    self.note_text = next;
                    self.dirty = true;
                    self.edit.caret = at;
                    self.edit.anchor = at;
                    self.after_edit();
                }
                Task::none()
            }
            Message::CopyCode(code) => {
                self.notify(NoticeLevel::Success, "已复制代码");
                iced::clipboard::write(code)
            }
            Message::PanelTab(panel_id, tab_id) => {
                // 拖拽会话结束的按钮释放不触发激活
                if self.drag.is_some() {
                    return Task::none();
                }
                self.apply(LayoutOp::SetActive { panel_id, tab_id })
            }
            Message::PanelAddView { panel_id, view } => {
                self.apply(LayoutOp::AddView { panel_id, view })
            }
            Message::SetTabView { panel_id, tab_id, view } => {
                self.apply(LayoutOp::SetTabView { panel_id, tab_id, view })
            }
            Message::DetachedSetTabView { window_id, tab_id, view } => {
                self.apply(LayoutOp::DetachedSetTabView { window_id, tab_id, view })
            }
            // × = 关文件留标签：摘下文件回到未打开态（tab 不动）
            Message::PanelTabFileClose | Message::DetachedTabFileClose => {
                if self.dirty {
                    self.notify(NoticeLevel::Warning, "有未保存改动，先 Ctrl+S 保存");
                    return Task::none();
                }
                self.dismiss_popup();
                self.ui.last_note_file = None;
                self.note_text.clear();
                self.source_editor = text_editor::Content::new();
                self.edit.reset(0);
                self.edit_doc = None;
                self.edit_meta.clear();
                self.preview_doc = None;
                self.dirty = false;
                self.save_error = None;
                self.schedule_persist();
                Task::none()
            }
            Message::PanelTabPress { panel_id, tab_id } => {
                self.tab_press = Some(TabPress {
                    window: self.main_id.unwrap_or(iced::window::Id::unique()),
                    source: PressSource::Panel { panel_id, tab_id },
                });
                Task::none()
            }
            Message::DetachedTabPress { window_id, tab_id } => {
                self.tab_press = Some(TabPress {
                    window: self
                        .detached_os
                        .iter()
                        .find(|(_, mid)| **mid == window_id)
                        .map(|(os, _)| *os)
                        .unwrap_or(iced::window::Id::unique()),
                    source: PressSource::Detached { window_id, tab_id },
                });
                Task::none()
            }
            Message::LayoutTabPress { layout_id, name } => {
                self.tab_press = Some(TabPress {
                    window: self.main_id.unwrap_or(iced::window::Id::unique()),
                    source: PressSource::Layout { layout_id, name },
                });
                Task::none()
            }
            Message::PanelTabClose(panel_id, tab_id) => {
                self.apply(LayoutOp::CloseTab { panel_id, tab_id })
            }
            // 整块锁定：面板全部标签统一值，切换基准 = 是否已全部锁定
            Message::PanelTabLock(panel_id) => {
                let tab_ids = self
                    .active_tree()
                    .map(|tree| panel_tab_ids_in(&tree, &panel_id))
                    .unwrap_or_default();
                let locked = self.active_tree().is_some_and(|tree| {
                    panel_tab_ids_in(&tree, &panel_id).iter().all(|id| {
                        layout::find_tab_in_tree(&tree, id).is_some_and(|(_, t)| t.locked)
                    })
                });
                let mut task = Task::none();
                for id in tab_ids {
                    task = task.chain(self.apply(LayoutOp::SetLocked {
                        panel_id: panel_id.clone(),
                        tab_id: id,
                        locked: !locked,
                    }));
                }
                task
            }
            Message::PanelSplit(panel_id, direction) => self.apply(LayoutOp::SplitPanel {
                panel_id,
                direction,
                position: Some("after".into()),
            }),
            Message::PanelClose(panel_id) => self.apply(LayoutOp::ClosePanel { panel_id }),
            Message::DetachedTab(window_id, tab_id) => {
                if self.drag.is_some() {
                    return Task::none();
                }
                self.apply(LayoutOp::DetachedSetActive { window_id, tab_id })
            }
            Message::DetachedTabClose(window_id, tab_id) => {
                self.apply(LayoutOp::DetachedCloseTab { window_id, tab_id })
            }
            Message::DetachedTabLock(window_id) => {
                let locked = self
                    .ui
                    .detached_windows
                    .iter()
                    .find(|w| w.id == window_id)
                    .map(|w| w.tabs.iter().all(|t| t.locked))
                    .unwrap_or(false);
                let tab_ids = self
                    .ui
                    .detached_windows
                    .iter()
                    .find(|w| w.id == window_id)
                    .map(|w| w.tabs.iter().map(|t| t.id.clone()).collect::<Vec<_>>())
                    .unwrap_or_default();
                let mut task = Task::none();
                for id in tab_ids {
                    task = task.chain(self.apply(LayoutOp::DetachedSetLocked {
                        window_id: window_id.clone(),
                        tab_id: id,
                        locked: !locked,
                    }));
                }
                task
            }
            Message::DetachedDockBack(window_id) => {
                let Some(tab_id) = self
                    .ui
                    .detached_windows
                    .iter()
                    .find(|w| w.id == window_id)
                    .and_then(|w| w.active_tab_id.clone())
                else {
                    return Task::none();
                };
                let Some(tree) = self.active_tree() else { return Task::none() };
                let mut panels = Vec::new();
                layout::collect_panels(&tree, &mut panels);
                let Some(panel_id) = panels.first().map(|p| p.node_id().to_string()) else {
                    self.notify(NoticeLevel::Warning, "当前布局无面板，先分割出面板");
                    return Task::none();
                };
                self.apply(LayoutOp::DockIntoPanel { panel_id, tab_id, index: None })
            }
            Message::DetachedPin(window_id) => {
                let pinned = self
                    .ui
                    .detached_windows
                    .iter()
                    .find(|w| w.id == window_id)
                    .is_some_and(|w| w.pinned);
                self.apply(LayoutOp::SetDetachedWindowPinned { window_id, pinned: !pinned })
            }
            Message::DetachedFocusLost(os_id) => {
                let Some(model_id) = self.detached_os.get(&os_id).cloned() else {
                    return Task::none();
                };
                let hide = self
                    .ui
                    .detached_windows
                    .iter()
                    .find(|w| w.id == model_id)
                    .is_some_and(|w| w.options.hide_on_blur && !w.pinned && !w.hidden);
                if hide {
                    self.apply(LayoutOp::HideDetachedWindow { window_id: model_id })
                } else {
                    Task::none()
                }
            }
            Message::PositionKnown(id, pos) => {
                self.os_positions.insert(id, pos.unwrap_or(Point::ORIGIN));
                Task::none()
            }
            Message::WindowMoved(id, pos) => {
                self.os_positions.insert(id, pos);
                let Some(model_id) = self.detached_os.get(&id) else { return Task::none() };
                if let Some(w) = self.ui.detached_windows.iter_mut().find(|w| w.id == *model_id) {
                    w.bounds.x = pos.x as f64;
                    w.bounds.y = pos.y as f64;
                    self.schedule_persist();
                }
                Task::none()
            }
            Message::ScaleKnown(id, sf) => {
                let Some(model_id) = self.detached_os.get(&id) else { return Task::none() };
                if let Some(w) = self.ui.detached_windows.iter_mut().find(|w| w.id == *model_id) {
                    if (w.bounds.scale - sf as f64).abs() > f64::EPSILON {
                        w.bounds.scale = sf as f64;
                        self.schedule_persist();
                    }
                }
                Task::none()
            }
            Message::LayoutActivate(id) => {
                // 布局 tab 拖拽会话中的按钮释放不触发激活（拖拽结束时按钮仍在光标下）
                if self.drag.is_some() {
                    return Task::none();
                }
                self.apply(LayoutOp::ActivateLayout { id })
            }
            Message::LayoutNew => self.apply(LayoutOp::AddLayout),
            Message::LayoutRenameStart(id) => {
                let name = self
                    .active_scene()
                    .and_then(|s| s.layouts.iter().find(|l| l.id == id))
                    .map(|l| l.name.clone())
                    .unwrap_or_default();
                self.rename = Some(RenameState { target: RenameTarget::Layout(id), value: name });
                Task::none()
            }
            Message::LayoutRenameInput(value) => {
                if let Some(r) = &mut self.rename {
                    r.value = value;
                }
                Task::none()
            }
            Message::LayoutRenameCommit => {
                let Some(r) = self.rename.take() else { return Task::none() };
                match r.target {
                    RenameTarget::Layout(id) => self.apply(LayoutOp::RenameLayout { id, name: r.value }),
                    RenameTarget::Scene(id) => self.apply(LayoutOp::RenameScene { id, name: r.value }),
                    RenameTarget::File(_) | RenameTarget::Folder(_) | RenameTarget::Draft { .. } => {
                        self.commit_file_rename(r)
                    }
                }
            }
            Message::LayoutDelete(id) => self.apply(LayoutOp::DeleteLayout { id }),
            Message::TabStripScrolled(strip, x) => {
                match strip {
                    Some(panel_id) => { self.panel_tab_scroll_x.insert(panel_id, x); }
                    None => self.titlebar_tab_scroll_x = x,
                }
                Task::none()
            }
            Message::LayoutDropped(from_index, insert_index) => {
                // 合成序（主页 0 + 场景内布局 1..）→ MoveLayout（先移除后插入）：
                // 插入位在原位之后时，移除使后续左移一位
                let to = if insert_index <= from_index { insert_index } else { insert_index - 1 };
                if to == 0 || to == from_index {
                    return Task::none();
                }
                self.apply(LayoutOp::MoveLayout { from_index, to_index: to })
            }
            Message::SceneActivate(id) => self.apply(LayoutOp::ActivateScene { id }),
            Message::SceneNew => self.apply(LayoutOp::AddScene),
            Message::SceneRenameStart(id) => {
                let name = self
                    .ui
                    .scenes
                    .iter()
                    .find(|s| s.id == id)
                    .map(|s| s.name.clone())
                    .unwrap_or_default();
                self.rename = Some(RenameState { target: RenameTarget::Scene(id), value: name });
                Task::none()
            }
            Message::SceneRenameInput(value) => {
                if let Some(r) = &mut self.rename {
                    r.value = value;
                }
                Task::none()
            }
            Message::SceneDelete(id) => {
                // 弹层内已就地确认；apply 统一收弹层
                self.apply(LayoutOp::DeleteScene { id })
            }
            Message::PersistTick => {
                if !self.persist_dirty {
                    return Task::none();
                }
                self.persist_dirty = false;
                let Some(path) = ui_state_path() else { return Task::none() };
                match serde_json::to_string(&self.ui) {
                    Ok(json) => Task::perform(
                        async move { vault::atomic_write(&path, &json) },
                        Message::PersistDone,
                    ),
                    Err(e) => {
                        self.notify(NoticeLevel::Error, format!("布局序列化失败：{e}"));
                        Task::none()
                    }
                }
            }
            Message::PersistDone(Ok(())) => Task::none(),
            Message::PersistDone(Err(e)) => {
                // 写盘失败回到脏态，下个周期重试
                self.persist_dirty = true;
                self.notify(NoticeLevel::Error, format!("布局落盘失败：{e}"));
                Task::none()
            }
            Message::UiTick => {
                self.notifications.retain(|n| !n.expired());
                // 重名提醒条 3s 过期
                if self
                    .rename_notice
                    .as_ref()
                    .is_some_and(|(_, at)| at.elapsed().as_millis() >= 3000)
                {
                    self.rename_notice = None;
                }
                // 悬停提示收起判定：光标离开触发钮邻域 = 进入收起延迟
                // （与显现同源的距离判定；批量事件中 exit 会被捕获状态吞掉，事件驱动不可靠）
                let tip = self
                    .tooltip
                    .as_ref()
                    .map(|(w, _, at, shown, _, anchor)| (*w, *anchor, *shown, at.elapsed().as_millis() >= 400));
                if let Some((window, anchor, _shown, mature)) = tip {
                    let away = self
                        .cursors
                        .get(&window)
                        .is_some_and(|p| p.distance(anchor) > 60.0);
                    if let Some((_, _, _, shown, closed_at, _)) = &mut self.tooltip {
                        if !*shown && mature {
                            *shown = true;
                        }
                        if *shown && away && closed_at.is_none() {
                            *closed_at = Some(Instant::now());
                        }
                        if let Some(t) = closed_at {
                            if t.elapsed().as_millis() >= 120 {
                                self.tooltip = None;
                            }
                        }
                    }
                }
                Task::none()
            }
            Message::WindowResized(id, size) => {
                if self.main_id == Some(id) {
                    self.main_size = size;
                    return Task::none();
                }
                // 撕裂窗口尺寸回写模型 bounds（OS 窗口事件 = bounds 唯一写者）
                self.os_sizes.insert(id, size);
                let Some(model_id) = self.detached_os.get(&id) else { return Task::none() };
                if let Some(w) = self.ui.detached_windows.iter_mut().find(|w| w.id == *model_id) {
                    w.bounds.width = size.width as f64;
                    w.bounds.height = size.height as f64;
                    self.schedule_persist();
                }
                Task::none()
            }
            Message::Press(id) => {
                // 弹层打开期间按压交由捕获层处理（外点关闭），不触发拖拽手势
                if self.popup.as_ref().is_some_and(|(w, _)| *w == id) {
                    return Task::none();
                }
                // 拖拽会话中收到新按压 = 释放事件丢失的兜底：按当前悬停落点收尾
                if self.drag.is_some() {
                    if let Some(d) = self.drag.take() {
                        let hover = self.drag_hover.take();
                        return self.resolve_drop(d, hover);
                    }
                    return Task::none();
                }
                if self.divider.is_some() {
                    return Task::none();
                }
                let cursor = self.cursor_for(id);
                // 行内重命名失焦提交：按压不在输入行内 = 提交（Enter 同路径）
                if self.rename.is_some() {
                    if self.rename_press_inside {
                        self.rename_press_inside = false;
                    } else if self.main_id == Some(id) {
                        return Task::perform(async {}, |_| Message::LayoutRenameCommit);
                    }
                }
                // 把手命中优先（主窗口）；否则记录按压起点（移动超阈值 = 拖标签手势）
                if self.main_id == Some(id) {
                    let tree = layout::active_layout(&self.ui).tree;
                    let hit = enumerate_dividers(&tree, self.content_area())
                        .into_iter()
                        .find(|d| d.rect.contains(cursor));
                    if let Some(d) = hit {
                        self.last_divider = Some((d.split_id.clone(), d.gap, d.horizontal));
                        self.divider = Some(DividerDrag {
                            split_id: d.split_id,
                            gap: d.gap,
                            horizontal: d.horizontal,
                            drag_start: if d.horizontal { cursor.x } else { cursor.y },
                            split_len: d.split_len,
                            sizes: d.sizes,
                        });
                        return Task::none();
                    }
                    // 面板聚焦：按压落进面板区 = 聚焦该面板（快捷键门控基准）
                    let zones = layout_view::panel_zones(self);
                    if let Some(zone) = zones.iter().find(|z| {
                        z.panel_id.is_some() && z.rect.contains(cursor)
                    }) {
                        let pid = zone.panel_id.clone().unwrap();
                        if self.ui.focused_panel_id.as_deref() != Some(pid.as_str()) {
                            self.ui.focused_panel_id = Some(pid);
                            self.schedule_persist();
                        }
                    }
                }
                self.press = Some((id, cursor));
                Task::none()
            }
            Message::CursorMoved(id, pos) => {
                self.cursors.insert(id, pos);
                if self.divider.is_some() {
                    if self.main_id != Some(id) {
                        return Task::none();
                    }
                    let Some(d) = &mut self.divider else { return Task::none() };
                    let axis = if d.horizontal { pos.x } else { pos.y };
                    let sum: f64 = d.sizes.iter().sum();
                    let dfrac = ((axis - d.drag_start) as f64 / d.split_len as f64) * sum;
                    let s0 = d.sizes[d.gap] + dfrac;
                    let s1 = d.sizes[d.gap + 1] - dfrac;
                    // 双侧钳制 12%
                    let min = sum * UiMetrics::MIN_SPLIT_FRAC;
                    let (s0, s1) = clamp_split_pair(s0, s1, min);
                    let split_id = d.split_id.clone();
                    let mut sizes = d.sizes.clone();
                    sizes[d.gap] = s0;
                    sizes[d.gap + 1] = s1;
                    return self.apply(LayoutOp::SetLayoutSizes { split_id, sizes });
                }
                // 拖拽候选转正：标签按钮自证命中的候选 + 移动超阈值
                // （候选在转正前保留，小于阈值的移动不消费）
                let moved = self
                    .tab_press
                    .as_ref()
                    .zip(self.press.as_ref())
                    .is_some_and(|(tp, (w, p))| {
                        *w == tp.window
                            && ((pos.x - p.x).abs() > 5.0 || (pos.y - p.y).abs() > 5.0)
                    });
                if moved {
                    if let Some(tp) = self.tab_press.take() {
                        return self.start_drag_from_press(tp);
                    }
                }
                if self.drag.is_some() {
                    if let Some(wpos) = self.os_positions.get(&id) {
                        let global = Point::new(wpos.x + pos.x, wpos.y + pos.y);
                        if let Some(d) = self.drag.as_mut() {
                            d.current = global;
                            d.cursor_window = id;
                        }
                        let zones = layout_view::panel_zones(self);
                        self.drag_hover = zones
                            .iter()
                            .find(|z| z.rect.contains(global))
                            .map(|zone| DragHover {
                                // 撕裂窗口无面板树：整体 = 加标签 / 头部 = 排序两态（无边缘分割）
                                kind: match layout_view::drop_zone_at(zone, global) {
                                    layout_view::DropZone::Tab => layout_view::DropZone::Tab,
                                    _ if zone.detached_id.is_some() => layout_view::DropZone::Center,
                                    kind => kind,
                                },
                                zone: zone.clone(),
                            });
                    }
                    return Task::none();
                }
                // 无拖拽候选的普通按压：超阈值仅清除按压基准（无手势）
                if let Some((press_win, press_pos)) = self.press {
                    if press_win == id
                        && ((pos.x - press_pos.x).abs() > 5.0 || (pos.y - press_pos.y).abs() > 5.0)
                    {
                        self.press = None;
                    }
                }
                Task::none()
            }
            Message::Release => {
                self.press = None;
                self.tab_press = None;
                if self.divider.take().is_some() {
                    return Task::none();
                }
                let Some(d) = self.drag.take() else {
                    self.drag_hover = None;
                    return Task::none();
                };
                let hover = self.drag_hover.take();
                self.resolve_drop(d, hover)
            }
            Message::SceneDeleteAsk(id) => {
                // 进入场景删除确认 pane（弹层内二段确认）
                self.popup_pane = 1;
                if let Some((_, open)) = &mut self.popup {
                    if let layout_view::PopupState::SceneSelector { confirm } = &mut open.state {
                        *confirm = Some(id);
                    }
                }
                Task::none()
            }
            Message::Ui(msg) => self.on_ui(msg),
            Message::TitleBarPress(id) => {
                // 双击（350ms 内同窗口两次空白按下）= 最大化切换；否则进入窗口拖拽
                let now = Instant::now();
                let double = self
                    .last_titlebar_press
                    .take_if(|(w, t)| *w == id && now.duration_since(*t).as_millis() < 350)
                    .is_some();
                self.last_titlebar_press = Some((id, now));
                if double {
                    window::toggle_maximize(id)
                } else {
                    window::drag(id)
                }
            }
            Message::WindowMinimize(id) => window::minimize(id, true),
            Message::WindowMaximizeToggle(id) => window::toggle_maximize(id),
            Message::FullscreenToggle(id) => {
                window::mode(id).map(move |mode| Message::ApplyFullscreen(id, mode))
            }
            Message::ApplyFullscreen(id, mode) => window::set_mode(
                id,
                if mode == Mode::Fullscreen { Mode::Windowed } else { Mode::Fullscreen },
            ),
            Message::Escape(window) => {
                // Esc 作用域 = 事件来源窗口：先关该窗口弹层，再取消行内重命名
                //（行内重命名只渲染在主窗口）
                if self.popup.as_ref().is_some_and(|(w, _)| *w == window) {
                    self.dismiss_popup();
                } else if self.main_id == Some(window) && self.rename.take().is_some() {
                    // 行内重命名 Esc = 取消
                }
                Task::none()
            }
            Message::SettingsToggle => {
                self.settings_open = !self.settings_open;
                self.dismiss_popup();
                Task::none()
            }
            Message::SettingsTab(tab) => {
                self.settings_tab = tab;
                Task::none()
            }
            Message::PopupOpen(window, open) => {
                self.popup = Some((window, open));
                self.popup_pane = 0;
                Task::none()
            }
            Message::FileCount(gen, Ok(n)) => {
                if gen != self.epoch() {
                    return Task::none();
                }
                self.file_count = Some(n);
                Task::none()
            }
            Message::FileCount(_, Err(e)) => {
                self.file_count = None;
                self.notify(NoticeLevel::Error, format!("统计文件失败：{e}"));
                Task::none()
            }
            Message::DraftStart(dir, kind) => {
                self.dismiss_popup();
                self.rename = Some(RenameState {
                    target: RenameTarget::Draft { dir, kind },
                    value: String::new(),
                });
                Task::none()
            }
            Message::RenameStartFile(rel) => {
                self.dismiss_popup();
                // 笔记重命名初值去 .md 后缀（标题即文件名约定）
                let name = rel.rsplit('/').next().unwrap_or(&rel).to_string();
                let name = name.strip_suffix(".md").map(|s| s.to_string()).unwrap_or(name);
                self.rename = Some(RenameState { target: RenameTarget::File(rel), value: name });
                Task::none()
            }
            Message::RenameStartFolder(dir) => {
                self.dismiss_popup();
                let name = dir.rsplit('/').next().unwrap_or(&dir).to_string();
                self.rename = Some(RenameState { target: RenameTarget::Folder(dir), value: name });
                Task::none()
            }
            Message::DeleteAsk(rel) => {
                // 文件删除 = 直接进确认 pane（无标题行确认态）
                self.popup_pane = 1;
                self.delete_confirm = Some((rel, false, None));
                Task::none()
            }
            Message::FolderDeleteAsk(rel) => {
                // 文件夹删除 = 非强制删除先行：空目录直接删；非空返回条目数进确认 pane
                let Some(root) = self.vault.root().ok() else { return Task::none() };
                let gen = self.epoch();
                let rel_task = rel.clone();
                Task::perform(
                    async move { vault::delete_folder(&root, &rel_task, false).map(|r| (r.needs_confirm, r.item_count)).map_err(|e| e) },
                    move |result| match result {
                        Ok((false, _)) => Message::OpDone(gen, parent_of(&rel), Ok(rel), FollowUp::None),
                        Ok((true, n)) => Message::FolderCounted(rel, n),
                        Err(e) => Message::OpDone(gen, parent_of(&rel), Err(e), FollowUp::None),
                    },
                )
            }
            Message::FolderCounted(rel, count) => {
                self.popup_pane = 1;
                self.delete_confirm = Some((rel, true, Some(count)));
                Task::none()
            }
            Message::DeleteCancel => {
                self.delete_confirm = None;
                self.popup_pane = 0;
                Task::none()
            }
            Message::DeleteEntry(rel, is_dir) => {
                // 删除命中打开中的笔记且有待落盘输入：拒绝，防未保存内容被静默清空
                let hits_open = self.ui.last_note_file.as_deref().is_some_and(|f| {
                    if is_dir {
                        f == rel || f.starts_with(&format!("{rel}/"))
                    } else {
                        f == rel
                    }
                });
                if hits_open && self.dirty {
                    self.notify(NoticeLevel::Warning, "有未保存改动，先 Ctrl+S 保存");
                    return Task::none();
                }
                self.dismiss_popup();
                self.delete_confirm = None;
                self.delete_entry(&rel, is_dir)
            }
            Message::RevealVault => {
                let Some(root) = self.vault.root().ok() else { return Task::none() };
                #[cfg(windows)]
                {
                    let _ = std::process::Command::new("explorer").arg(&root).spawn();
                }
                #[cfg(not(windows))]
                {
                    let _ = std::process::Command::new("xdg-open").arg(&root).spawn();
                }
                Task::none()
            }
            Message::OpDone(gen, dir, Ok(final_rel), mut follow) => {
                if gen != self.epoch() {
                    return Task::none();
                }
                self.dismiss_popup();
                // 重命名去重判定：实际落点与请求名不同 = 发生了去重，提醒条告知，
                // 且「打开中笔记跟随」必须按实际落点（否则指向幽灵路径）
                if let FollowUp::Renamed { to, .. } = &mut follow {
                    if *to != final_rel {
                        let old_name = to.rsplit('/').next().unwrap_or(to);
                        let new_name = final_rel.rsplit('/').next().unwrap_or(&final_rel);
                        self.rename_notice =
                            Some((format!("「{old_name}」已存在，已重命名为「{new_name}」"), Instant::now()));
                        *to = final_rel;
                    }
                }
                if !matches!(follow, FollowUp::Renamed { .. }) {
                    self.notify(NoticeLevel::Success, "完成");
                }
                self.apply_follow(&follow);
                self.load_children(&dir)
            }
            Message::OpDone(gen, _, Err(e), _) => {
                if gen != self.epoch() {
                    return Task::none();
                }
                self.notify(NoticeLevel::Error, e);
                Task::none()
            }
            Message::ExpandAllToggle => {
                if self.expand_all {
                    // 收起全部：清空展开集
                    self.expand_all = false;
                    self.expand_pending = 0;
                    self.ui.file_explorer_expanded.clear();
                    self.schedule_persist();
                    Task::none()
                } else {
                    // 展开全部：从根递归加载（懒加载树逐层补齐）
                    self.expand_all = true;
                    self.expand_pending = 1;
                    self.load_children_for_expand("")
                }
            }
            Message::ExpandChildrenLoaded(dir, gen, Ok(entries)) => {
                if gen != self.epoch() || !self.expand_all {
                    return Task::none();
                }
                self.expand_pending = self.expand_pending.saturating_sub(1);
                if !dir.is_empty() && !self.ui.file_explorer_expanded.contains(&dir) {
                    self.ui.file_explorer_expanded.push(dir.clone());
                }
                self.children.insert(dir.clone(), entries.clone());
                let mut task = Task::none();
                for (rel, is_dir) in entries {
                    if !is_dir {
                        continue;
                    }
                    // 已缓存目录直接进展开集；未缓存的才下发加载（缓存 ≠ 已展开）
                    if !self.ui.file_explorer_expanded.contains(&rel) {
                        self.ui.file_explorer_expanded.push(rel.clone());
                    }
                    if !self.children.contains_key(&rel) {
                        self.expand_pending += 1;
                        task = task.chain(self.load_children_for_expand(&rel));
                    }
                }
                if self.expand_pending == 0 {
                    self.schedule_persist();
                }
                task
            }
            Message::ExpandChildrenLoaded(_, gen, Err(e)) => {
                if gen != self.epoch() || !self.expand_all {
                    return Task::none();
                }
                self.expand_pending = self.expand_pending.saturating_sub(1);
                self.notify(NoticeLevel::Error, format!("读取目录失败：{e}"));
                Task::none()
            }
            Message::RenamePressInside => {
                self.rename_press_inside = true;
                Task::none()
            }
            Message::SettingsNavCollapse => {
                self.settings_collapsed = !self.settings_collapsed;
                Task::none()
            }
            Message::SaveErrorDismiss => {
                self.save_error = None;
                Task::none()
            }
            Message::DividerAdjust(horizontal, positive) => {
                // 键盘调整上次拖动过的把手（±2%，双侧钳 12%）；
                // 门控：编辑面聚焦 / 源码档 / 行内重命名在途时方向键属文本操作，不调把手
                if self.edit.focused
                    || self.note_mode == NoteMode::Source
                    || self.rename.is_some()
                    || self.popup.is_some()
                {
                    return Task::none();
                }
                let Some((split_id, gap, h)) = self.last_divider.clone() else {
                    return Task::none();
                };
                if h != horizontal {
                    return Task::none();
                }
                let Some(info) = enumerate_dividers(&layout::active_layout(&self.ui).tree, self.content_area())
                    .into_iter()
                    .find(|d| d.split_id == split_id && d.gap == gap)
                else {
                    return Task::none();
                };
                let sum: f64 = info.sizes.iter().sum();
                let step = sum * 0.02 * if positive { 1.0 } else { -1.0 };
                let mut sizes = info.sizes.clone();
                let s0 = sizes[gap] + step;
                let s1 = sizes[gap + 1] - step;
                let min = sum * UiMetrics::MIN_SPLIT_FRAC;
                let (s0, s1) = clamp_split_pair(s0, s1, min);
                sizes[gap] = s0;
                sizes[gap + 1] = s1;
                self.apply(LayoutOp::SetLayoutSizes { split_id, sizes })
            }
            Message::NotImplemented(what) => {
                self.dismiss_popup();
                self.notify(NoticeLevel::Info, format!("「{what}」尚未实现"));
                Task::none()
            }
        }
    }

    /// 应用一个布局操作：形状校验 + 变更 + 调度落盘 + 撕裂窗口调和
    /// （iced 单状态树天然全窗口可见，无广播层）。布局操作全部伴随弹层收起。
    fn apply(&mut self, op: LayoutOp) -> Task<Message> {
        self.apply_op(op).0
    }

    /// 同 [`Self::apply`]，返回 (调和任务, 操作结果)；SplitPanel 新面板 id 等后续操作需要结果，
    /// 调和任务（建窗/搬位）必须继续上报运行时，丢弃会丢撕裂窗口。
    fn apply_op(&mut self, op: LayoutOp) -> (Task<Message>, layout::LayoutOpResult) {
        if !op_passes_shape_check(&self.ui, &op) {
            return (Task::none(), layout::LayoutOpResult::default());
        }
        let result = apply_layout_op(&mut self.ui, &op);
        self.dismiss_popup();
        self.schedule_persist();
        (self.reconcile(), result)
    }

    /// 收起当前弹层（菜单动作完成后的统一收尾）。
    fn dismiss_popup(&mut self) {
        self.popup = None;
        self.popup_pane = 0;
    }

    /// 拖拽候选转正：候选由标签按钮按下时自证命中，此处只补齐标签信息并校验锁定
    /// （锁定禁拖；布局 tab 主页固定不可拖在候选入口已挡）。
    fn start_drag_from_press(&mut self, tp: TabPress) -> Task<Message> {
        let wpos = self.os_positions.get(&tp.window).copied();
        let press_global = match (&self.press, wpos) {
            (Some((w, p)), Some(wpos)) if *w == tp.window => {
                Point::new(wpos.x + p.x, wpos.y + p.y)
            }
            _ => return Task::none(),
        };
        let drag = match tp.source {
            PressSource::Panel { panel_id, tab_id } => {
                if self.tab_locked(&tab_id, Some(&panel_id), None) {
                    return Task::none();
                }
                let Some((view, label)) = self
                    .active_tree()
                    .and_then(|tree| layout::find_tab_in_tree(&tree, &tab_id))
                    .map(|(_, t)| (t.view.clone(), layout_view::tab_label(self, &t.view)))
                else {
                    return Task::none();
                };
                DragSession {
                    tab_id,
                    view,
                    label,
                    from_panel: Some(panel_id),
                    from_detached: None,
                    layout_from: 0,
                    current: press_global,
                    cursor_window: tp.window,
                }
            }
            PressSource::Detached { window_id, tab_id } => {
                let locked = self
                    .ui
                    .detached_windows
                    .iter()
                    .find(|w| w.id == window_id)
                    .and_then(|w| w.tabs.iter().find(|t| t.id == tab_id))
                    .is_some_and(|t| t.locked);
                if locked {
                    return Task::none();
                }
                let Some((view, label)) = self
                    .ui
                    .detached_windows
                    .iter()
                    .find(|w| w.id == window_id)
                    .and_then(|w| w.tabs.iter().find(|t| t.id == tab_id))
                    .map(|t| (t.view.clone(), layout_view::tab_label(self, &t.view)))
                else {
                    return Task::none();
                };
                DragSession {
                    tab_id,
                    view,
                    label,
                    from_panel: None,
                    from_detached: Some(window_id),
                    layout_from: 0,
                    current: press_global,
                    cursor_window: tp.window,
                }
            }
            PressSource::Layout { layout_id, name } => {
                // 合成序（主页 = 0 不参与拖拽；入口已挡）；tabs 已含主页占 0 位
                let (_, tabs) = layout_view::titlebar_tab_strip(self);
                let from_index = tabs
                    .iter()
                    .position(|(id, _, _)| id == &layout_id)
                    .unwrap_or(0);
                if from_index == 0 {
                    return Task::none();
                }
                DragSession {
                    tab_id: layout_id,
                    view: String::new(),
                    label: name,
                    from_panel: None,
                    from_detached: None,
                    layout_from: from_index,
                    current: press_global,
                    cursor_window: tp.window,
                }
            }
        };
        self.drag = Some(drag);
        self.drag_hover = None;
        Task::none()
    }

    /// 标签是否锁定（主窗口树面板 / 撕裂窗口两处查找）。
    fn tab_locked(&self, tab_id: &str, panel_id: Option<&str>, detached_id: Option<&str>) -> bool {
        if let Some(pid) = panel_id {
            return self
                .active_tree()
                .and_then(|tree| layout::find_tab_in_tree(&tree, tab_id))
                .is_some_and(|(src, t)| src == pid && t.locked);
        }
        if let Some(mid) = detached_id {
            return self
                .ui
                .detached_windows
                .iter()
                .find(|w| w.id == mid)
                .and_then(|w| w.tabs.iter().find(|t| t.id == tab_id))
                .is_some_and(|t| t.locked);
        }
        false
    }

    /// 释放落点解析：布局 tab = 排序；同宿主标签条 = 重排序、跨宿主 = 移动/停靠、
    /// 四边缘 = 分割落点、桌面 = 撕出新窗口。
    fn resolve_drop(&mut self, d: DragSession, hover: Option<DragHover>) -> Task<Message> {
        // 布局 tab 拖拽（无宿主面板）：落回标题栏 tab 条 = 排序，其余取消
        // （titlebar_tab_strip 返回主窗口本地坐标，光标须换算回本地）
        if d.from_panel.is_none() && d.from_detached.is_none() {
            if let Some(main_id) = self.main_id {
                if let Some(wpos) = self.os_positions.get(&main_id) {
                    let local = Point::new(d.current.x - wpos.x, d.current.y - wpos.y);
                    if local.y <= UiMetrics::TITLEBAR_H {
                        let (strip_x, tabs) = layout_view::titlebar_tab_strip(self);
                        if local.x >= strip_x {
                            // 插入位 = 命中标签中点之前（内容坐标随滚动偏移平移）
                            let mut insert = tabs.len();
                            let mut x = strip_x - self.titlebar_tab_scroll_x;
                            for (i, (_, _, w)) in tabs.iter().enumerate() {
                                if local.x < x + w / 2.0 {
                                    insert = i;
                                    break;
                                }
                                x += w + 4.0;
                            }
                            return Task::done(Message::LayoutDropped(d.layout_from, insert));
                        }
                    }
                }
            }
            return Task::none();
        }
        let Some(h) = hover else {
            let bounds = WindowBounds {
                x: d.current.x as f64,
                y: d.current.y as f64,
                width: 420.0,
                height: 560.0,
                scale: 0.0,
            };
            return match (&d.from_panel, &d.from_detached) {
                (Some(panel_id), _) => self.apply(LayoutOp::TearOff {
                    panel_id: panel_id.clone(),
                    tab_id: d.tab_id,
                    bounds,
                }),
                (None, Some(window_id)) => self.apply(LayoutOp::TearOffFromDetached {
                    window_id: window_id.clone(),
                    tab_id: d.tab_id,
                    bounds,
                }),
                _ => Task::none(),
            };
        };
        let scroll_x = h
            .zone
            .panel_id
            .as_deref()
            .and_then(|pid| self.panel_tab_scroll_x.get(pid).copied())
            .unwrap_or(0.0);
        let index = (h.kind == layout_view::DropZone::Tab)
            .then(|| layout_view::tab_index_at(&h.zone, d.current.x, scroll_x));
        // 四边缘 = 分割落点（仅主窗口面板；撕裂窗口整体 = 加标签）
        if h.zone.panel_id.is_some()
            && matches!(
                h.kind,
                layout_view::DropZone::Left
                    | layout_view::DropZone::Right
                    | layout_view::DropZone::Top
                    | layout_view::DropZone::Bottom
            )
        {
            let direction = if matches!(h.kind, layout_view::DropZone::Left | layout_view::DropZone::Right) {
                "horizontal"
            } else {
                "vertical"
            }
            .to_string();
            let position = matches!(h.kind, layout_view::DropZone::Right | layout_view::DropZone::Bottom)
                .then(|| "after".to_string())
                .or_else(|| Some("before".to_string()));
            let target = h.zone.panel_id.clone().unwrap();
            // 分割出新面板 → 标签移入（返回值携带新面板 id；调和任务串接）
            let (task, result) = self.apply_op(LayoutOp::SplitPanel {
                panel_id: target,
                direction,
                position,
            });
            if let Some(new_panel) = result.split_panel_id {
                return task.chain(self.move_tab_into(d, new_panel, None));
            }
            return task;
        }
        // 同主窗口面板：标签条 = 重排序，面板体 = 取消
        if h.zone.panel_id.is_some() && h.zone.panel_id == d.from_panel {
            if let (Some(panel_id), Some(to_index)) = (d.from_panel, index) {
                return self.apply(LayoutOp::MoveTabWithin {
                    panel_id,
                    tab_id: d.tab_id,
                    to_index,
                });
            }
            return Task::none();
        }
        // 同撕裂窗口：标签条 = 组内排序，面板体 = 取消
        if h.zone.detached_id.is_some() && h.zone.detached_id == d.from_detached {
            if let (Some(window_id), Some(to_index)) = (d.from_detached, index) {
                return self.apply(LayoutOp::DetachedMoveTab {
                    window_id,
                    tab_id: d.tab_id,
                    to_index,
                });
            }
            return Task::none();
        }
        // 跨宿主
        if let Some(to_panel) = h.zone.panel_id {
            return self.move_tab_into(d, to_panel, index);
        }
        if let Some(window_id) = h.zone.detached_id {
            return self.apply(LayoutOp::DockIntoDetached {
                window_id,
                tab_id: d.tab_id,
                index,
            });
        }
        Task::none()
    }

    /// 把拖拽标签移入目标面板（来源主面板 / 撕裂窗口两路）。
    fn move_tab_into(
        &mut self,
        d: DragSession,
        to_panel: String,
        index: Option<usize>,
    ) -> Task<Message> {
        if let Some(from_panel_id) = d.from_panel {
            return self.apply(LayoutOp::MoveTabBetween {
                from_panel_id,
                to_panel_id: to_panel,
                tab_id: d.tab_id,
                index,
            });
        }
        if d.from_detached.is_some() {
            // 来源撕裂窗口条目由 op 内部拖空自动移除（reconcile 随即回收 OS 窗口）
            return self.apply(LayoutOp::DockIntoPanel {
                panel_id: to_panel,
                tab_id: d.tab_id,
                index,
            });
        }
        Task::none()
    }

    /// 撕裂窗口调和：模型条目 → OS 窗口。缺失即建（bounds/选项随条目）、
    /// hidden 态与 OS 可见性同步、模型已无条目的 OS 窗口（幽灵）回收。
    /// bounds 权威 = OS 窗口事件回写模型；恢复位置只读模型（show 时按模型搬位）。
    fn reconcile(&mut self) -> Task<Message> {
        let mut task = Task::none();
        for entry in self.ui.detached_windows.clone() {
            let known = self
                .detached_os
                .iter()
                .find(|(_, mid)| mid.as_str() == entry.id)
                .map(|(os, _)| *os);
            let Some(os_id) = known else {
                if entry.hidden {
                    continue;
                }
                let (os_id, open) = window::open(panel_settings(&entry));
                self.detached_os.insert(os_id, entry.id.clone());
                self.os_shown.insert(os_id, true);
                task = task
                    .chain(open.map(Message::WindowOpened))
                    .chain(window::position(os_id).map(move |p| Message::PositionKnown(os_id, p)))
                    .chain(window::set_level(
                        os_id,
                        if entry.options.always_on_top {
                            Level::AlwaysOnTop
                        } else {
                            Level::Normal
                        },
                    ))
                    .chain(window::scale_factor(os_id).map(move |sf| Message::ScaleKnown(os_id, sf)));
                continue;
            };
            let shown = self.os_shown.get(&os_id).copied().unwrap_or(true);
            if entry.hidden && shown {
                self.os_shown.insert(os_id, false);
                task = task.chain(window::set_mode(os_id, Mode::Hidden));
            } else if !entry.hidden && !shown {
                // 唤起：按模型 bounds 搬位（隐藏期间的位置变更以模型为准）
                self.os_shown.insert(os_id, true);
                task = task
                    .chain(window::set_mode(os_id, Mode::Windowed))
                    .chain(window::move_to(os_id, Point::new(entry.bounds.x as f32, entry.bounds.y as f32)))
                    .chain(window::resize(os_id, Size::new(entry.bounds.width as f32, entry.bounds.height as f32)))
                    .chain(window::set_level(
                        os_id,
                        if entry.options.always_on_top {
                            Level::AlwaysOnTop
                        } else {
                            Level::Normal
                        },
                    ))
                    .chain(window::gain_focus(os_id));
            }
        }
        let ghosts: Vec<window::Id> = self
            .detached_os
            .iter()
            .filter(|(_, mid)| !self.ui.detached_windows.iter().any(|w| w.id == mid.as_str()))
            .map(|(os, _)| *os)
            .collect();
        for os in ghosts {
            self.detached_os.remove(&os);
            self.os_shown.remove(&os);
            task = task.chain(window::close(os));
        }
        task
    }

    fn schedule_persist(&mut self) {
        self.persist_dirty = true;
    }

    /// 会话世代：core 在每次切换仓库时自增；在途任务的回调据此判定是否过期。
    fn epoch(&self) -> u64 {
        self.vault
            .generation
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    fn active_tree(&self) -> Option<layout::LayoutNode> {
        Some(layout::active_layout(&self.ui).tree)
    }

    fn active_scene(&self) -> Option<&layout::Scene> {
        layout::active_scene(&self.ui).into()
    }

    /// 主窗口分割树几何区域（标题栏与状态栏之间的内容区，logical px）。
    fn content_area(&self) -> iced::Rectangle {
        layout_view::content_area_of(self)
    }

    /// ui 层消息转译：提示显隐 / 弹层关闭与面板导航 / 通知关闭。
    fn on_ui(&mut self, msg: ui::UiMsg) -> Task<Message> {
        match msg {
            ui::UiMsg::HoverTip { window, label, box_px } => {
                let anchor = self.cursor_for(window);
                self.tooltip = Some((window, label, Instant::now(), false, None, anchor));
                self.tooltip_half = Some(box_px / 2.0);
                Task::none()
            }
            ui::UiMsg::HoverTipEnd { window, label } => {
                // 事件 exit 仍生效（未被捕获吞掉时立即进入收起延迟）
                if self
                    .tooltip
                    .as_ref()
                    .is_some_and(|(w, l, _, _, closed, _)| *w == window && l == &label && closed.is_none())
                {
                    if let Some((_, _, _, _, closed_at, _)) = &mut self.tooltip {
                        *closed_at = Some(Instant::now());
                    }
                }
                Task::none()
            }
            ui::UiMsg::PopupPane(pane) => {
                self.popup_pane = pane;
                Task::none()
            }
            ui::UiMsg::PopupClose { window } => {
                if self.popup.as_ref().is_some_and(|(w, _)| *w == window) {
                    self.popup = None;
                    self.popup_pane = 0;
                    self.delete_confirm = None;
                }
                Task::none()
            }
            ui::UiMsg::NotifyDismiss(id) => {
                self.notifications.retain(|n| n.id != id);
                Task::none()
            }
        }
    }

    /// 文件操作成功后对「打开中的笔记」的跟随：改名换前缀、删除清空。
    fn apply_follow(&mut self, follow: &FollowUp) {
        match follow {
            FollowUp::None => {}
            FollowUp::Renamed { from, to } => {
                if let Some(f) = &self.ui.last_note_file {
                    let next = if f == from {
                        Some(to.clone())
                    } else if f.starts_with(&format!("{from}/")) {
                        Some(f.replacen(from, to, 1))
                    } else {
                        None
                    };
                    if let Some(n) = next {
                        self.ui.last_note_file = Some(n);
                        self.schedule_persist();
                    }
                }
            }
            FollowUp::Deleted { rel, is_dir } => {
                let hit = self.ui.last_note_file.as_deref().is_some_and(|f| {
                    if *is_dir {
                        f == rel || f.starts_with(&format!("{rel}/"))
                    } else {
                        f == rel
                    }
                });
                if hit {
                    self.ui.last_note_file = None;
                    self.note_text.clear();
                    self.source_editor = text_editor::Content::new();
                    self.edit.reset(0);
                    self.edit_doc = None;
                    self.edit_meta.clear();
                    self.preview_doc = None;
                    self.dirty = false;
                    self.save_error = None;
                    self.schedule_persist();
                }
            }
        }
    }

    /// 仓库文件总数（排除隐藏/排除夹的树扫描；打开仓库后一次）。
    fn count_files(&self) -> Task<Message> {
        let Ok(root) = self.vault.root() else { return Task::none() };
        let Ok(exclude) = self.vault.exclude_folders() else { return Task::none() };
        let gen = self.epoch();
        Task::perform(
            async move {
                vault::list_vault_tree(&root, &exclude)
                    .map(|nodes| nodes.iter().filter(|n| !n.is_dir).count())
                    .map_err(|e| e.to_string())
            },
            move |result| Message::FileCount(gen, result),
        )
    }

    /// 文件树重命名/新建草稿提交：按目标种类分派到 core 的写入口；
    /// 新建与改名对同名做 `-2`/`-3` 加序号去重（以磁盘事实为准，改名排除自身），
    /// OpDone 携带实际落点供「已重命名为」提醒条判定。
    fn commit_file_rename(&mut self, r: RenameState) -> Task<Message> {
        let Some(root) = self.vault.root().ok() else { return Task::none() };
        let gen = self.epoch();
        match r.target {
            RenameTarget::Draft { dir, kind } => {
                let name = vault::sanitize_filename(r.value.trim());
                if name.is_empty() {
                    return Task::none();
                }
                // 新条目落 dir 内部：完成后刷新 dir 本身的子项缓存
                let parent = dir.clone();
                Task::perform(
                    async move {
                        // 同名加序号：name、name-2、name-3 …（存在性以磁盘为准，目录同名同样让位）
                        let base = match kind {
                            DraftKind::Note => format!("{name}.md"),
                            DraftKind::Folder => name.clone(),
                        };
                        let mut candidate = base.clone();
                        let mut seq = 2;
                        loop {
                            let path = vault::safe_join(&root, &full_rel(&dir, &candidate), false)?;
                            if !path.exists() {
                                break;
                            }
                            candidate = match kind {
                                DraftKind::Note => format!("{name}-{seq}.md"),
                                DraftKind::Folder => format!("{name}-{seq}"),
                            };
                            seq += 1;
                        }
                        let rel = full_rel(&dir, &candidate);
                        match kind {
                            DraftKind::Note => {
                                let path = vault::safe_join(&root, &rel, true)?;
                                vault::atomic_write(&path, "").map(|_| rel)
                            }
                            DraftKind::Folder => vault::create_folder(&root, &rel).map(|_| rel),
                        }
                    },
                    move |result| Message::OpDone(gen, parent, result, FollowUp::None),
                )
            }
            RenameTarget::File(old) => {
                let title = vault::sanitize_filename(r.value.trim());
                if title.is_empty() {
                    return Task::none();
                }
                // 无扩展名文件（或仅前导点的隐藏名）按原名改名，不补扩展名
                let new = match old.rfind('.') {
                    Some(0) | None => title,
                    Some(i) => format!("{title}.{}", &old[i + 1..]),
                };
                let parent = parent_of(&old);
                let parent_for_task = parent.clone();
                let old_for_task = old.clone();
                let new_for_task = new.clone();
                Task::perform(
                    async move {
                        // 目标已存在（排除自身）= 加序号静默落位，实际落点经 OpDone 回传
                        // （case-only 改名跳过：Windows exists() 大小写不敏感，会误判同名冲突）
                        let mut target = new_for_task.clone();
                        if target != old_for_task && !target.eq_ignore_ascii_case(&old_for_task) {
                            let path = vault::safe_join(&root, &full_rel(&parent_for_task, &target), false)?;
                            if path.exists() {
                                let (stem, ext) = match target.rfind('.') {
                                    Some(i) if i > 0 => (target[..i].to_string(), target[i..].to_string()),
                                    _ => (target.clone(), String::new()),
                                };
                                let mut seq = 2;
                                loop {
                                    target = format!("{stem}-{seq}{ext}");
                                    let p = vault::safe_join(&root, &full_rel(&parent_for_task, &target), false)?;
                                    if !p.exists() {
                                        break;
                                    }
                                    seq += 1;
                                }
                            }
                        }
                        if target == old_for_task {
                            return Ok(target);
                        }
                        vault::rename_note_file(&root, &old_for_task, &target).map(|_| target)
                    },
                    move |result| {
                        Message::OpDone(gen, parent, result, FollowUp::Renamed { from: old, to: new })
                    },
                )
            }
            RenameTarget::Folder(old_dir) => {
                let name = vault::sanitize_filename(r.value.trim());
                if name.is_empty() {
                    return Task::none();
                }
                let parent = parent_of(&old_dir);
                let new_dir = match old_dir.rfind('/') {
                    Some(i) => format!("{}/{name}", &old_dir[..i]),
                    None => name,
                };
                let old_for_task = old_dir.clone();
                let new_for_task = new_dir.clone();
                Task::perform(
                    async move {
                        // 目标已存在（排除自身）= 加序号静默落位；case-only 改名跳过查重
                        // （Windows exists() 大小写不敏感，会误判同名冲突）
                        let mut target = new_for_task.clone();
                        if target != old_for_task && !target.eq_ignore_ascii_case(&old_for_task) {
                            let path = vault::safe_join(&root, &target, false)?;
                            if path.exists() {
                                let mut seq = 2;
                                loop {
                                    target = match new_for_task.rfind('/') {
                                        Some(i) => format!("{}/{}-{seq}", &new_for_task[..i], &new_for_task[i + 1..]),
                                        None => format!("{new_for_task}-{seq}"),
                                    };
                                    let p = vault::safe_join(&root, &target, false)?;
                                    if !p.exists() {
                                        break;
                                    }
                                    seq += 1;
                                }
                            }
                        }
                        if target == old_for_task {
                            return Ok(target);
                        }
                        vault::rename_folder(&root, &old_for_task, &target).map(|_| target)
                    },
                    move |result| {
                        Message::OpDone(
                            gen,
                            parent,
                            result,
                            FollowUp::Renamed { from: old_dir, to: new_dir },
                        )
                    },
                )
            }
            RenameTarget::Layout(_) | RenameTarget::Scene(_) => Task::none(),
        }
    }

    /// 删除文件/文件夹（不可恢复；菜单内已就地确认）。Ok 携带目标相对路径。
    fn delete_entry(&mut self, rel: &str, is_dir: bool) -> Task<Message> {
        let Some(root) = self.vault.root().ok() else { return Task::none() };
        let gen = self.epoch();
        let parent = parent_of(rel);
        let rel_for_task = rel.to_string();
        let rel_for_follow = rel.to_string();
        Task::perform(
            async move {
                if is_dir {
                    vault::delete_folder(&root, &rel_for_task, true).map(|_| rel_for_task)
                } else {
                    vault::delete_vault_file(&root, &rel_for_task).map(|_| rel_for_task)
                }
            },
            move |result| {
                Message::OpDone(
                    gen,
                    parent,
                    result,
                    FollowUp::Deleted { rel: rel_for_follow, is_dir },
                )
            },
        )
    }

    /// 读目录子项（core 的统一过滤语义：隐藏项 / 排除夹 / .tmp 副产物）。
    fn load_children(&mut self, rel: &str) -> Task<Message> {
        let Ok(root) = self.vault.root() else {
            return Task::none();
        };
        let Ok(exclude) = self.vault.exclude_folders() else {
            self.notify(NoticeLevel::Error, "仓库配置读取失败");
            return Task::none();
        };
        let gen = self.epoch();
        let rel = rel.to_string();
        let rel_for_cb = rel.clone();
        Task::perform(
            async move { vault::read_dir_filtered(&root, &rel, &exclude) },
            move |result| Message::ChildrenLoaded(rel_for_cb, gen, result),
        )
    }

    /// 递归展开用的目录读取（带世代守卫：在途任务不跨仓库落地）。
    fn load_children_for_expand(&self, rel: &str) -> Task<Message> {
        let Ok(root) = self.vault.root() else {
            return Task::none();
        };
        let Ok(exclude) = self.vault.exclude_folders() else {
            return Task::none();
        };
        let rel = rel.to_string();
        let rel_for_cb = rel.clone();
        let gen = self.epoch();
        Task::perform(
            async move { vault::read_dir_filtered(&root, &rel, &exclude) },
            move |result| Message::ExpandChildrenLoaded(rel_for_cb, gen, result),
        )
    }

    /// 只读档的解析产物：进入只读档或文件载入时重建（渲染面不逐帧解析）。
    fn refresh_preview(&mut self) {
        if self.note_mode != NoteMode::Read {
            self.preview_doc = None;
            return;
        }
        let body = strip_frontmatter(&self.note_text).to_string();
        self.preview_doc = Some(markdown_view::parse_document(&body));
    }

    /// 编辑档解析产物与块元数据重建。
    fn refresh_edit(&mut self) {
        if self.note_mode != NoteMode::Edit {
            self.edit_doc = None;
            self.edit_meta.clear();
            return;
        }
        let doc = markdown_view::parse_document(&self.note_text);
        self.edit_meta = markdown_view::block_meta(&doc.source, &doc.blocks);
        // 空文档（无块）也要有光标宿主：补一个空块
        if self.edit_meta.is_empty() {
            self.edit_meta.push(markdown_view::empty_block_meta());
        }
        self.edit_doc = Some(doc);
    }

    /// 光标所在块序号（严格落在块区间内；空行 / 文外返回 None）。
    fn block_at(&self, offset: usize) -> Option<usize> {
        self.edit_meta.iter().position(|m| offset >= m.from && offset <= m.to)
    }

    /// 光标最近块（块间空行与文外兜底用）。
    fn nearest_block(&self, offset: usize) -> Option<usize> {
        if self.edit_meta.is_empty() {
            return None;
        }
        if let Some(i) = self.block_at(offset) {
            return Some(i);
        }
        if let Some(first) = self.edit_meta.first() {
            if offset < first.from {
                return Some(0);
            }
        }
        self.edit_meta.iter().rposition(|m| m.from <= offset).or(Some(0))
    }

    /// 命中目标 → 源偏移。
    fn resolve_hit(&self, target: HitTarget) -> usize {
        match target {
            HitTarget::BlockText { index, point, width } => match self.edit_meta.get(index) {
                Some(meta) => markdown_edit::offset_in_block(
                    &self.note_text,
                    meta.from,
                    meta.to,
                    &meta.source,
                    width,
                    point,
                    &self.edit_opts,
                ),
                None => 0,
            },
            HitTarget::BlockEdge { index, upper } => match self.edit_meta.get(index) {
                Some(meta) => if upper { meta.from } else { meta.to },
                None => 0,
            },
            HitTarget::GapLine { offset } => offset.min(self.note_text.len()),
            HitTarget::Start => self.edit_meta.first().map_or(0, |m| m.from),
            HitTarget::End => self.note_text.len(),
        }
    }

    /// 活动块 = 光标所在块。光标落在空白区（文首 / 块间 / 文末的空白行与文末虚拟行）
    /// 时不揭示任何块，键盘与光标由编辑面托管；其余文外位置回到最近块保住键盘宿主。
    fn sync_active(&mut self) {
        let len = self.note_text.len();
        let caret = self.edit.caret.min(len);
        if let Some(i) = self.block_at(caret) {
            self.edit.active = Some(i);
            return;
        }
        // 文末虚拟行：末块之后还有尾部字节时，caret == len 也由编辑面托管；
        // caret == 0 落在文首 Gap 行（能走到这里说明首块前有空白行，端点上无相邻块）
        let on_blank_line = caret == 0
            || caret < len
            || self.edit_meta.last().is_some_and(|last| len > last.to);
        self.edit.active = if on_blank_line { None } else { self.nearest_block(caret) };
    }

    /// 选区锚点所在块。
    fn anchor_block(&self) -> Option<usize> {
        self.block_at(self.edit.anchor.min(self.note_text.len()))
    }

    /// 一次编辑落地后的收尾。
    fn after_edit(&mut self) {
        self.refresh_edit();
        self.sync_active();
    }

    /// 需要回显源码的块：光标所在块 + 选区锚点所在块（无选区时不揭示锚点块）。
    pub fn editable_blocks(&self) -> Vec<usize> {
        let mut out = Vec::new();
        if let Some(i) = self.edit.active {
            out.push(i);
        }
        let (a, b) = self.edit.selection();
        if b > a {
            if let Some(i) = self.anchor_block() {
                if !out.contains(&i) {
                    out.push(i);
                }
            }
        }
        out
    }

    /// 光标是否在列表块内。
    fn is_in_list(&self) -> bool {
        self.edit
            .active
            .is_some_and(|i| self.edit_meta.get(i).is_some_and(|m| m.list))
    }

    /// 删除选区（有则删）。
    fn delete_selection(&mut self) -> bool {
        let (a, b) = self.edit.selection();
        if b <= a {
            return false;
        }
        self.edit.snapshot(&self.note_text);
        let mut next = String::with_capacity(self.note_text.len());
        next.push_str(&self.note_text[..a]);
        next.push_str(&self.note_text[b..]);
        self.note_text = next;
        self.dirty = true;
        self.edit.caret = a;
        self.edit.anchor = a;
        self.after_edit();
        true
    }

    /// 插入文本（替换选区）。
    fn edit_insert(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        self.edit.snapshot(&self.note_text);
        let (a, b) = self.edit.selection();
        let mut next = String::with_capacity(self.note_text.len() + text.len());
        next.push_str(&self.note_text[..a]);
        next.push_str(&text);
        next.push_str(&self.note_text[b..]);
        self.note_text = next;
        self.dirty = true;
        self.edit.caret = a + text.len();
        self.edit.anchor = self.edit.caret;
        self.after_edit();
    }

    /// 退格：先删选区；否则空列表项整段标记；再否则删前一个字符。
    fn edit_backspace(&mut self) {
        if self.delete_selection() {
            return;
        }
        if self.is_in_list() {
            if let Some((next, caret)) =
                markdown_edit::list_backspace_edit(&self.note_text, self.edit.caret)
            {
                self.edit.snapshot(&self.note_text);
                self.note_text = next;
                self.dirty = true;
                self.edit.caret = caret;
                self.edit.anchor = caret;
                self.after_edit();
                return;
            }
        }
        let caret = self.edit.caret.min(self.note_text.len());
        if caret == 0 {
            return;
        }
        let prev = prev_boundary(&self.note_text, caret);
        self.edit.snapshot(&self.note_text);
        let mut next = String::with_capacity(self.note_text.len());
        next.push_str(&self.note_text[..prev]);
        next.push_str(&self.note_text[caret..]);
        self.note_text = next;
        self.dirty = true;
        self.edit.caret = prev;
        self.edit.anchor = prev;
        self.after_edit();
    }

    /// 删除：先删选区；否则删后一个字符。
    fn edit_delete(&mut self) {
        if self.delete_selection() {
            return;
        }
        let caret = self.edit.caret.min(self.note_text.len());
        if caret >= self.note_text.len() {
            return;
        }
        let after = next_boundary(&self.note_text, caret);
        self.edit.snapshot(&self.note_text);
        let mut next = String::with_capacity(self.note_text.len());
        next.push_str(&self.note_text[..caret]);
        next.push_str(&self.note_text[after..]);
        self.note_text = next;
        self.dirty = true;
        self.after_edit();
    }

    /// 回车：列表块内延续项标记，否则插入换行。
    fn edit_newline(&mut self) {
        if self.delete_selection() {
            return;
        }
        if self.is_in_list() {
            if let Some((next, caret)) =
                markdown_edit::list_enter_edit(&self.note_text, self.edit.caret)
            {
                self.edit.snapshot(&self.note_text);
                self.note_text = next;
                self.dirty = true;
                self.edit.caret = caret;
                self.edit.anchor = caret;
                self.after_edit();
                return;
            }
        }
        self.edit_insert("\n".to_string());
    }

    /// Tab 缩进 / Shift+Tab 反缩进。
    fn edit_indent(&mut self, outdent: bool) {
        let (from, to) = self.edit.selection();
        let edit = if outdent {
            match markdown_edit::outdent_edit(&self.note_text, from, to) {
                Some(edit) => edit,
                None => return,
            }
        } else {
            markdown_edit::indent_edit(&self.note_text, from, to)
        };
        self.edit.snapshot(&self.note_text);
        self.note_text = edit.text;
        self.dirty = true;
        self.edit.anchor = edit.from;
        self.edit.caret = edit.to;
        self.after_edit();
    }

    /// 光标移动（上下越界时跨块）。
    fn edit_move(&mut self, motion: markdown_edit::Motion, extend: bool) {
        use markdown_edit::Motion;
        let len = self.note_text.len();
        let caret = self.edit.caret.min(len);
        let target = match motion {
            Motion::Left => prev_boundary(&self.note_text, caret),
            Motion::Right => next_boundary(&self.note_text, caret),
            Motion::Home => line_start_of(&self.note_text, caret),
            Motion::End => line_end_of(&self.note_text, caret),
            Motion::Up => {
                self.cross_block_move(false, extend);
                return;
            }
            Motion::Down => {
                self.cross_block_move(true, extend);
                return;
            }
        };
        if !extend {
            self.edit.anchor = target;
        }
        self.edit.caret = target;
        self.sync_active();
    }

    /// 上下越界时的跨块移动：块内跨到相邻块首 / 末或毗邻的文首 / 文末空白区；
    /// 空白区光标按行首逐行移动，越过端点落相邻块（见 `markdown_edit::vertical_target`）。
    fn cross_block_move(&mut self, forward: bool, extend: bool) {
        let len = self.note_text.len();
        let caret = self.edit.caret.min(len);
        let spans: Vec<(usize, usize)> =
            self.edit_meta.iter().map(|m| (m.from, m.to)).collect();
        let blank_starts = self.blank_starts_near(caret, &spans, forward);
        let Some(target) = markdown_edit::vertical_target(caret, &spans, &blank_starts, forward)
        else {
            return;
        };
        let target = target.min(len);
        if !extend {
            self.edit.anchor = target;
        }
        self.edit.caret = target;
        self.sync_active();
    }

    /// 光标所在或行进方向毗邻的空白行行首：光标在空白区取该区；在首块向上 /
    /// 末块向下时取毗邻的文首 / 文末空白区；其余为空。
    fn blank_starts_near(
        &self,
        caret: usize,
        spans: &[(usize, usize)],
        forward: bool,
    ) -> Vec<usize> {
        if let Some(i) = spans.iter().position(|&(f, t)| caret >= f && caret <= t) {
            let region = if !forward && i == 0 {
                spans.first().map(|&(f, _)| (0, f))
            } else if forward && i + 1 == spans.len() {
                spans.last().map(|&(_, t)| (t, self.note_text.len()))
            } else {
                None
            };
            return region.map_or(Vec::new(), |(f, t)| self.region_blank_starts(f, t));
        }
        match spans.iter().rposition(|&(f, _)| f <= caret) {
            None => self.region_blank_starts(0, spans.first().map_or(0, |&(f, _)| f)),
            Some(i) => {
                let from = spans[i].1;
                let to = spans.get(i + 1).map_or(self.note_text.len(), |&(_, f)| f);
                self.region_blank_starts(from, to)
            }
        }
    }

    /// 空白区间的空白行行首：文首区第一行也是空白行（不含缩进片段）；
    /// 文末区含虚拟文末行（文本以换行结尾时）。
    fn region_blank_starts(&self, from: usize, to: usize) -> Vec<usize> {
        if to == self.note_text.len() {
            markdown_edit::tail_blank_starts(&self.note_text, from)
        } else if from == 0 {
            markdown_edit::edge_blank_starts(&self.note_text, 0, to)
        } else {
            markdown_edit::gap_line_starts(&self.note_text, from, to)
        }
    }

    fn edit_undo(&mut self) {
        if let Some((text, caret, anchor)) = self.edit.undo(&self.note_text) {
            self.note_text = text;
            self.dirty = true;
            self.edit.caret = caret.min(self.note_text.len());
            self.edit.anchor = anchor.min(self.note_text.len());
            self.after_edit();
        }
    }

    fn edit_redo(&mut self) {
        if let Some((text, caret, anchor)) = self.edit.redo(&self.note_text) {
            self.note_text = text;
            self.dirty = true;
            self.edit.caret = caret.min(self.note_text.len());
            self.edit.anchor = anchor.min(self.note_text.len());
            self.after_edit();
        }
    }

    /// 编辑面事件处理。
    fn on_editor(&mut self, event: markdown_edit::EditorEvent) -> Task<Message> {
        use markdown_edit::EditorEvent as E;
        match event {
            E::Hit { target, extend } => {
                let offset = self.resolve_hit(target);
                if !extend {
                    self.edit.anchor = offset;
                }
                self.edit.caret = offset.min(self.note_text.len());
                self.edit.focused = true;
                self.sync_active();
                Task::none()
            }
            E::Blur => {
                // 失焦即收起光标与揭示（与只读档同观感），选区保留但不再有光标
                self.edit.focused = false;
                self.edit.preedit = None;
                self.edit.active = None;
                Task::none()
            }
            E::SetCaret { offset, extend } => {
                let offset = offset.min(self.note_text.len());
                if !extend {
                    self.edit.anchor = offset;
                }
                self.edit.caret = offset;
                self.sync_active();
                Task::none()
            }
            E::Insert(text) => {
                self.edit_insert(text);
                Task::none()
            }
            E::Backspace => {
                self.edit_backspace();
                Task::none()
            }
            E::Delete | E::DeleteSelection => {
                self.edit_delete();
                Task::none()
            }
            E::Newline => {
                self.edit_newline();
                Task::none()
            }
            E::Indent => {
                self.edit_indent(false);
                Task::none()
            }
            E::Outdent => {
                self.edit_indent(true);
                Task::none()
            }
            E::Move { motion, extend } => {
                self.edit_move(motion, extend);
                Task::none()
            }
            E::Undo => {
                self.edit_undo();
                Task::none()
            }
            E::Redo => {
                self.edit_redo();
                Task::none()
            }
            E::SelectAll => {
                self.edit.anchor = 0;
                self.edit.caret = self.note_text.len();
                self.edit.active = self.nearest_block(self.edit.caret);
                Task::none()
            }
            E::Preedit(preedit) => {
                self.edit.preedit = preedit;
                Task::none()
            }
        }
    }

    /// 打开链接：外链走系统浏览器，仓库路径/wiki 命中即打开笔记。
    fn open_link(&mut self, href: &str, form: LinkForm) -> Task<Message> {
        let decoded = percent_decode(href);
        let path = decoded.split('#').next().unwrap_or("").trim().to_string();
        match form {
            LinkForm::External => {
                open_external(&decoded);
                Task::none()
            }
            LinkForm::Path | LinkForm::Wiki | LinkForm::Create => {
                if self.dirty {
                    self.notify(NoticeLevel::Warning, "有未保存改动，先 Ctrl+S 保存");
                    return Task::none();
                }
                match self.resolve_note_path(&path) {
                    Some(rel) => {
                        self.ui.last_note_file = Some(rel.clone());
                        // 与文件树点击同口径：打开即复位只读
                        self.note_mode = NoteMode::Read;
                        self.preview_doc = None;
                        self.edit_doc = None;
                        self.edit_meta.clear();
                        self.schedule_persist();
                        self.notify(NoticeLevel::Success, "打开链接");
                        self.load_note(&rel)
                    }
                    None => {
                        self.notify(NoticeLevel::Error, format!("未找到笔记：{path}"));
                        Task::none()
                    }
                }
            }
            LinkForm::Plain => Task::none(),
        }
    }

    /// 链接路径 → 仓库内相对路径（不做 wiki 标题级检索）。
    fn resolve_note_path(&self, href: &str) -> Option<String> {
        let root = self.vault.root().ok()?;
        let rel = href.trim_start_matches('/');
        if rel.is_empty() {
            return None;
        }
        for candidate in [rel.to_string(), format!("{rel}.md")] {
            if let Ok(path) = vault::safe_join(&root, &candidate, false) {
                if path.is_file() {
                    return Some(candidate);
                }
            }
        }
        None
    }

    /// 读 note 视图文件内容（仓库切换恢复与文件点击共用；根在发起时冻结，
    /// 任务在途时切换仓库不会让读盘落到新仓库路径）。
    fn load_note(&self, rel: &str) -> Task<Message> {
        let Ok(root) = self.vault.root() else {
            return Task::none();
        };
        let gen = self.epoch();
        let rel = rel.to_string();
        Task::perform(
            async move {
                let path = vault::safe_join(&root, &rel, false)?;
                let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
                let text = String::from_utf8(bytes)
                    .map_err(|_| "非 UTF-8 文本文件，暂不支持打开".to_string())?;
                Ok((rel, text))
            },
            move |result| Message::FileLoaded(gen, result),
        )
    }

    fn title(&self, id: window::Id) -> String {
        if self.main_id == Some(id) {
            return "Atelyx".into();
        }
        // 撕裂窗口标题 = 激活视图名
        let Some(model_id) = self.detached_os.get(&id) else { return "Atelyx".into() };
        self.ui
            .detached_windows
            .iter()
            .find(|w| w.id == *model_id)
            .and_then(|w| {
                w.active_tab_id.as_deref().and_then(|tid| {
                    w.tabs.iter().find(|t| t.id == tid).map(|t| {
                        format!("Atelyx — {}", layout_view::view_label(&t.view))
                    })
                })
            })
            .unwrap_or_else(|| "Atelyx".into())
    }

    fn view(&self, id: window::Id) -> Element<'_, Message> {
        if self.main_id == Some(id) {
            layout_view::main_view(self, id)
        } else if let Some(model_id) = self.detached_os.get(&id) {
            layout_view::detached_view(self, id, model_id)
        } else {
            space().into()
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        // 心跳仅在有待过期通知、提醒条在途、悬停提示在途（距离收起判定）或编辑面聚焦时挂载
        let need_tick = !self.notifications.is_empty()
            || self.rename_notice.is_some()
            || self.tooltip.is_some()
            || self.edit.focused;
        Subscription::batch([
            iced::time::every(PERSIST_TICK).map(|_| Message::PersistTick),
            if need_tick {
                iced::time::every(std::time::Duration::from_millis(100)).map(|_| Message::UiTick)
            } else {
                Subscription::none()
            },
            // 分割把手键盘调整：上次拖动过的把手，方向键 ±2%（horizontal = 左右键）
            iced::event::listen_with(|event, _status, window| match event {
                Event::Keyboard(keyboard::Event::KeyPressed {
                    key: keyboard::Key::Named(keyboard::key::Named::ArrowLeft),
                    modifiers,
                    ..
                }) if modifiers.is_empty() => Some(Message::DividerAdjust(true, false)),
                Event::Keyboard(keyboard::Event::KeyPressed {
                    key: keyboard::Key::Named(keyboard::key::Named::ArrowRight),
                    modifiers,
                    ..
                }) if modifiers.is_empty() => Some(Message::DividerAdjust(true, true)),
                Event::Keyboard(keyboard::Event::KeyPressed {
                    key: keyboard::Key::Named(keyboard::key::Named::ArrowUp),
                    modifiers,
                    ..
                }) if modifiers.is_empty() => Some(Message::DividerAdjust(false, false)),
                Event::Keyboard(keyboard::Event::KeyPressed {
                    key: keyboard::Key::Named(keyboard::key::Named::ArrowDown),
                    modifiers,
                    ..
                }) if modifiers.is_empty() => Some(Message::DividerAdjust(false, true)),
                Event::Keyboard(keyboard::Event::KeyPressed {
                    key: keyboard::Key::Character(c),
                    modifiers,
                    ..
                }) if c == "s" && modifiers.control() => Some(Message::Save),
                Event::Keyboard(keyboard::Event::KeyPressed {
                    key: keyboard::Key::Named(keyboard::key::Named::Escape),
                    ..
                }) => Some(Message::Escape(window)),
                Event::Window(window::Event::CloseRequested) => {
                    Some(Message::WindowCloseRequested(window))
                }
                Event::Window(window::Event::Closed) => Some(Message::WindowClosed(window)),
                Event::Window(window::Event::Resized(size)) => {
                    Some(Message::WindowResized(window, size))
                }
                Event::Window(window::Event::Moved(pos)) => {
                    Some(Message::WindowMoved(window, pos))
                }
                Event::Window(window::Event::Unfocused) => Some(Message::DetachedFocusLost(window)),
                Event::Window(window::Event::Rescaled(sf)) => {
                    Some(Message::ScaleKnown(window, sf))
                }
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                    Some(Message::Press(window))
                }
                Event::Mouse(mouse::Event::CursorMoved { position }) => {
                    Some(Message::CursorMoved(window, position))
                }
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                    Some(Message::Release)
                }
                _ => None,
            }),
        ])
    }
}
