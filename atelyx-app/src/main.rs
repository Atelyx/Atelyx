//! Atelyx 原生壳：daemon 多窗口 + 布局模型（场景/布局/面板树）+ files/note 视图。
//!
//! 布局状态真源 = atelyx-core::layout：落盘 ui-state.json（磁盘格式由 schema
//! 版本标识，本壳只读不改格式）；本壳负责窗口副作用（OS 窗口、落盘防抖、输入路由）。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod autostart;
mod calendar_actions;
mod calendar_day;
mod calendar_grid;
mod calendar_heat;
mod calendar_month;
mod calendar_view;
mod chat_actions;
mod chat_input;
mod chat_view;
mod commands;
mod history_browser;
mod layout_view;
mod markdown_edit;
mod markdown_view;
mod plugin_actions;
mod plugin_canvas;
mod plugin_view;
mod properties_view;
mod recent_view;
mod repohistory_view;
mod settings_view;
mod space_actions;
mod table_actions;
mod table_view;
mod ui;
mod update;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use atelyx_core::credentials;
use atelyx_core::global_config::{self, LastActive};
use atelyx_core::layout::{
    self, apply_layout_op, normalize, op_passes_shape_check, AppUiState, DetachedWindow,
    LayoutOp, WindowBounds, UI_STATE_SCHEMA,
};
use atelyx_core::markdown::{LinkForm, MarkdownDocument};
use atelyx_core::plugin_runtime::{PluginEvent, PluginRuntime, PluginStatus, SurfaceKind};
use atelyx_core::vault;
use atelyx_core::vault_refs;
use iced::widget::{space, text_editor};
use iced::window::{self, Level, Mode, Position, Settings};
use iced::{keyboard, mouse, Element, Event, Point, Size, Subscription, Task, Theme};

use crate::table_view as tv;
use markdown_edit::HitTarget;
use properties_view::{PropsMsg, PropsPanel};
use ui::notify::{Level as NoticeLevel, Notification, Progress as NoticeProgress};

use layout_view::{
    enumerate_dividers, panel_tab_ids_in, PopupOpen, PopupState, RenameState, RenameTarget, UiMetrics,
};

/// 撕裂窗口建窗参数：bounds 恢复位置 + 条目 OS 选项（不进任务栏）；无系统边框。
fn panel_settings(w: &DetachedWindow) -> Settings {
    let mut settings = Settings {
        size: Size::new(w.bounds.width.max(200.0) as f32, w.bounds.height.max(160.0) as f32),
        min_size: Some(PANEL_MIN_SIZE),
        position: Position::Specific(Point::new(w.bounds.x as f32, w.bounds.y as f32)),
        exit_on_close_request: false,
        decorations: false,
        icon: ui::window_icon(),
        ..Settings::default()
    };
    #[cfg(windows)]
    {
        settings.platform_specific.skip_taskbar = w.options.skip_taskbar;
    }
    settings
}

/// 撕裂窗口最小尺寸（OS 缩放下限）。
const PANEL_MIN_SIZE: Size = Size::new(320.0, 240.0);

/// note 视图模式：只读渲染 / 预览编辑 / 源码。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteMode {
    Read,
    Edit,
    Source,
}

/// 剥离开头的 frontmatter 块（阅读/编辑面只处理正文）；缺闭合定界符时按无 frontmatter 处理，全量正文。
fn strip_frontmatter(text: &str) -> &str {
    match atelyx_core::frontmatter::block_end(text) {
        Some(end) => &text[end..],
        None => text,
    }
}

/// 展平文件树中的全部文件（树序；(rel, 含扩展名文件名)）。
pub(crate) fn collect_md_files(tree: &[vault::FileTreeNode], out: &mut Vec<(String, String)>) {
    for node in tree {
        if node.is_dir {
            collect_md_files(&node.children, out);
        } else {
            out.push((node.path.clone(), node.name.clone()));
        }
    }
}

/// 历史署名（本地桌面端：用户名即设备名）。
pub(crate) fn history_author() -> atelyx_core::history::HistoryAuthor {
    let user = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "本地".into());
    atelyx_core::history::HistoryAuthor { id: format!("desktop:{user}"), name: user.clone(), device: user }
}

/// markdown 链接 href 编码：空格 / 括号 / 百分号百分号化，其余（含中文）原样。
fn encode_md_href(file: &str) -> String {
    let mut out = String::with_capacity(file.len());
    for ch in file.chars() {
        match ch {
            ' ' => out.push_str("%20"),
            '(' => out.push_str("%28"),
            ')' => out.push_str("%29"),
            '%' => out.push_str("%25"),
            other => out.push(other),
        }
    }
    out
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

/// 主窗口默认大小（逻辑像素）。
const DEFAULT_WINDOW_SIZE: Size = Size::new(1440.0, 900.0);
/// 主窗口最小大小（逻辑像素）。
const MIN_WINDOW_SIZE: Size = Size::new(1100.0, 760.0);
/// 防抖落盘周期：期间多次模型变更合并为一次写盘（拖宽/拖拽等连续操作不逐帧写）。
const PERSIST_TICK: std::time::Duration = std::time::Duration::from_millis(800);

/// 自动保存防抖：最近一次内容变更后停止输入该时长才写回（尾沿 debounce，单定时器链续排）。
pub(crate) const AUTOSAVE_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(500);

/// 应用数据目录（bundle identifier = com.atelyx.desktop）；ui-state.json 与 global.json 同放此处。
fn app_data_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    let base = std::env::var("APPDATA").ok().map(PathBuf::from);
    #[cfg(not(windows))]
    let base = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|_| std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".local/share")))
        .ok();
    // 拼接写在 cfg 之外：紧跟 cfg 块的 `.map(...)` 会被同一条 cfg 一并裁掉，Windows 上会静默
    // 退回未加标识符的裸目录（应用级文件与插件根随之漂移）
    base.map(|d| d.join("com.atelyx.desktop"))
}

/// ui-state.json 落盘路径（应用级 UI 状态）。
fn ui_state_path() -> Option<PathBuf> {
    app_data_dir().map(|d| d.join("ui-state.json"))
}

/// global.json 落盘路径（应用级全局配置：最近仓库列表）。
fn global_config_path() -> Option<PathBuf> {
    app_data_dir().map(|d| d.join("global.json"))
}

/// 更新包下载目录（应用数据目录下 updates；boot 即创建，失败留给下载侧如实报错；
/// 应用数据目录不可用（极少见）回落系统临时目录）。
fn update_download_dir() -> PathBuf {
    let dir = app_data_dir()
        .map(|d| d.join("updates"))
        .unwrap_or_else(|| std::env::temp_dir().join("atelyx-updates"));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// 启动读全局配置；读错误（原文损坏且备份失败等）回传文案供通知。
fn load_global_boot() -> (global_config::GlobalConfig, Option<String>) {
    let Some(path) = global_config_path() else {
        return (global_config::GlobalConfig::default(), None);
    };
    match global_config::read_global_config(&path) {
        Ok((config, backup)) => (
            config,
            backup.map(|name| format!("全局配置损坏，已留档为 {name}；最近仓库列表已重置")),
        ),
        Err(e) => (global_config::GlobalConfig::default(), Some(e)),
    }
}

/// 当前 unix 秒（最近仓库的打开时间戳）。
pub(crate) fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 命令候选键形态：Ctrl/Cmd + 单字符（撤销/重做/剪贴板类），或命名键（任意修饰，
/// 覆盖 NoMod 清空类与其余注册表键位）。全局订阅不持 App 状态，是否命中与消费
/// 由处理器按录制态与作用域焦点自守卫。
fn command_key_candidate(key: &keyboard::Key, mods: keyboard::Modifiers) -> bool {
    match key {
        keyboard::Key::Character(c) => c.len() == 1 && (mods.control() || mods.logo()),
        keyboard::Key::Named(_) => true,
        keyboard::Key::Unidentified => false,
    }
}

/// 注册表命令 → 表格消息（Note 域命令由编辑面部件内的绑定表消费，不产表格消息）。
fn table_msg_of(id: commands::CommandId) -> Option<tv::TableMsg> {
    match id {
        commands::CommandId::TableUndo => Some(tv::TableMsg::Undo),
        commands::CommandId::TableRedo => Some(tv::TableMsg::Redo),
        commands::CommandId::TableCopy => Some(tv::TableMsg::Copy),
        commands::CommandId::TableCut => Some(tv::TableMsg::Cut),
        commands::CommandId::TablePaste => Some(tv::TableMsg::Paste),
        commands::CommandId::TableClear => Some(tv::TableMsg::ClearSelection),
        _ => None,
    }
}

/// 从磁盘恢复布局状态：缺失/损坏/schema 不符一律回落默认态。
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

/// 仓库级配置与元数据回执（config / 提示词标记 / 文件夹颜色）。
pub(crate) type VaultMetaResult =
    Result<(vault::VaultConfig, Vec<String>, HashMap<String, String>), String>;

/// 表格可见图片批量加载回执：相对路径 + 文件指纹 + 字节或错误。
pub(crate) type ImageLoadBatch = Vec<(String, (u128, u64), Result<Vec<u8>, String>)>;

#[derive(Debug, Clone)]
enum Message {
    WindowOpened(window::Id),
    WindowCloseRequested(window::Id),
    WindowClosed(window::Id),
    PickVault,
    VaultPicked(Result<PathBuf, String>),
    /// 从最近仓库列表进入指定仓库（脏拦截统一 flush 放行）。
    /// 启动自动进入 = from_boot；运行中切换在 gate 保存后关闭当前打开的文件。
    SelectVault { path: PathBuf, from_boot: bool },
    ToggleDir(String),
    /// gen = 发起任务时的仓库世代；回调落地前与当前世代比对，过期即丢弃。
    ChildrenLoaded(String, u64, Result<Vec<vault::DirChild>, String>),
    OpenFile(String),
    FileLoaded(u64, Result<(String, String), String>),
    Edited(text_editor::Action),
    Save,
    /// 携带保存发起时冻结的内容与保存动机：落地后与编辑器现值比对，区分保存完成与在途新编辑。
    Saved(u64, String, SaveReason, Result<(), String>),
    /// 自动保存防抖定时器到期（单定时器链，见 chain_autosave_timer）。
    AutosaveTick,
    /// 切换 note 视图模式（只读 / 预览编辑 / 源码）。
    NoteMode(NoteMode),
    /// 只读档正文双击：进预览编辑档并把光标落到双击点。
    NoteEnterEdit,
    /// 笔记面板内按压上报（PressRelay 转播；编辑档外点退出据此区分面板内外）。
    NotePressInside,
    /// 编辑面事件（部件只上报，正文/光标/选区真相在应用层）。
    Editor(markdown_edit::EditorEvent),
    /// 属性面板消息（frontmatter 类型化编辑）。
    Props(PropsMsg),
    /// 反链扫描落地（gen 守卫 + 笔记身份守卫：慢扫描回执晚于切文件时丢弃，防串台）。
    BacklinksLoaded(u64, String, Result<Vec<vault::BacklinkRow>, String>),
    /// 标签词汇表落地（gen 守卫）。
    TagVocabLoaded(u64, Result<Vec<vault::TagRow>, String>),
    /// 全仓库笔记清单落地（双链候选数据源；gen 守卫）。
    NoteFilesLoaded(u64, Result<Vec<(String, String)>, String>),
    /// 方向键（先反链候选导航，无候选时调整分割把手）。
    NavKey(bool, bool),
    /// 编辑面 Enter（双链候选开启时 = 选中候选）。
    EnterKey,
    /// 双链候选选中（候选项序号）。
    WikiPick(usize),
    /// 搜索面板查询词输入。
    SearchInput(String),
    /// 历史版本列表落地（gen 守卫 + kind/文件身份守卫：版本快照随文件绑定，串台会跨文件回滚）。
    HistoryLoaded(u64, atelyx_core::history::HistoryKind, String, Vec<atelyx_core::history::HistoryVersion>),
    /// 历史记录弹层（打开 / 展开预览 / 回滚二段确认）。
    History(history_browser::HistoryBrowserMsg),
    /// 画布回滚写盘回执（gen 守卫；失败提示不阻塞）。
    CanvasHistoryRestored(u64, String, Result<(), String>),
    /// 重建内部链接（设置-编辑器：二段确认 / 队列执行 / 回执）。
    RebuildLinks(settings_view::editor::RebuildLinksMsg),
    /// 历史记录/回滚落盘完成 → 重读版本列表。
    HistoryRecorded(u64, Option<String>),
    /// 最近打开条目失效剔除（确认缺失的 (vaultKey, file) 对）。
    RecentPruned(Vec<(String, String)>),
    /// 仓库历史聚合落地（gen 守卫）。
    RepoHistoryLoaded(u64, Result<Vec<repohistory_view::RepoHistoryGroup>, String>),
    /// 仓库历史面板刷新（失效重取）。
    RepoHistoryRefresh,
    /// AI 对话面板消息（会话 / run 事件 / 编排）。
    Chat(chat_actions::ChatMsg),
    /// 无操作（后台任务回执无界面变化时的统一落点）。
    Noop,
    /// 日历视图消息（日程增删改 / 月份导航 / 装载回执）。
    Calendar(calendar_actions::CalendarMsg),
    /// 打开链接（外链 / 仓库路径 / wiki）。
    OpenLink(String, LinkForm),
    /// 笔记图片字节落地（gen 守卫 + src；失败进失败集回落占位，不重试）。
    ImageLoaded(u64, String, Result<Vec<u8>, String>),
    /// 文件面板排序方式切换（落仓库 config 的 fileExplorerSort）。
    SetSort(String),
    /// 创建同目录副本（文件字节复制 / 文件夹递归复制）。
    DuplicateEntry(String, bool),
    /// 提示词标记注册/注销切换（.atelyx/prompt-notes.json）。
    PromptToggle(String),
    /// 设置/清除文件夹图标颜色（None = 恢复默认）。
    FolderColorSet(String, Option<String>),
    /// 仓库级配置与元数据（config / 提示词标记 / 文件夹颜色）落地（gen 守卫）。
    VaultMetaLoaded(u64, VaultMetaResult),
    /// 提示词标记清单落地（读或写回后的最新清单；gen 守卫）。
    PromptNotesLoaded(u64, Result<Vec<String>, String>),
    /// Agent 配置清单落地（agents.json 读或写后重读；gen 守卫）。
    AgentsLoaded(u64, Result<Vec<vault::AgentConfig>, String>),
    /// 文件夹颜色落盘完成（重读后的清单；gen 守卫）。
    FolderColorsLoaded(u64, Result<HashMap<String, String>, String>),
    /// 仓库级配置落盘回执（失败提示，不回滚界面选择）。
    ConfigSaved(u64, Result<(), String>),
    /// 文件行按压（拖拽候选；由行 mouse_area 上报，超阈值转正拖拽会话）。
    FileRowPress { window: window::Id, rel: String, is_dir: bool },
    /// 拖拽悬停进入可落点（文件夹行 / 仓库行；无会话时忽略）。
    FileDragEnter { rel: String, is_dir: bool },
    /// 拖拽悬停离开可落点。
    FileDragExit { rel: String },
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
    /// × = 关文件留标签（kind = "note" / "table"，只关对应面）。
    PanelTabFileClose(String),
    DetachedTabFileClose(String),
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
    /// 表格视图消息（交互 / 命令 / 保存链回执）。
    Table(table_view::TableMsg),
    /// 表格可见图片批量加载落地（gen 守卫）。
    TableImagesLoaded(u64, ImageLoadBatch),
    /// 键盘修饰态（Ctrl+滚轮缩放 / Shift+滚轮横滚判定源）。
    ModsChanged(keyboard::Modifiers),
    /// 标题栏空白区按下：拖拽窗口 / 双击最大化。
    TitleBarPress(window::Id),
    /// 边缘命中带按下：进入 OS 原生缩放循环（最大化态由 OS 拒绝）。
    WindowResizeStart(window::Id, window::Direction),
    WindowMinimize(window::Id),
    WindowMaximizeToggle(window::Id),
    /// 全屏切换（查询当前模式 → 反转）。
    FullscreenToggle(window::Id),
    ApplyFullscreen(window::Id, iced::window::Mode),
    /// Esc：先关弹层，再取消行内重命名。
    Escape(window::Id),
    /// 命令候选键（注册表键位 + Ctrl+S/A 兜底的统一入口）。
    KeyCommand { key: keyboard::Key, mods: keyboard::Modifiers },
    /// 设置-快捷键：命令行进入录制态。
    ShortcutRecord(commands::CommandId),
    /// 设置-快捷键：清除该命令的键位覆盖（恢复默认表）。
    ShortcutReset(commands::CommandId),
    /// 打开 / 关闭设置整页；设置左栏切 tab；折叠钮切换。
    SettingsToggle,
    SettingsTab(String),
    SettingsNavCollapse,
    /// AI 设置页消息（子分区 / 供应商 / 模型服务 / Agent / 搜索）。
    Ai(settings_view::ai::AiMsg),
    /// 设置-插件页消息（安装 / 启停 / 卸载 / 详情）。
    Plugins(settings_view::plugins::PluginsMsg),
    /// 插件运行时事件（单一通道进 update）。
    PluginEvent(Box<PluginEvent>),
    /// 插件界面节点事件（点击 / 变更 / 提交）。
    PluginView {
        plugin_id: String,
        surface_id: String,
        node_id: String,
        event: atelyx_core::plugin_runtime::ViewEvent,
    },
    /// 插件菜单 / 下拉节点被按下：用光标锚点打开弹层。
    PluginList { window: window::Id, entries: Vec<plugin_actions::PopupEntry> },
    /// 插件多行输入的编辑器动作。
    PluginTextEdit {
        plugin_id: String,
        surface_id: String,
        node_id: String,
        action: text_editor::Action,
    },
    /// 插件请求接管聚焦窗口（壳应用后经运行时回填插件）。
    PluginTakeoverAsk {
        request_id: u64,
        plugin_id: String,
        scope: atelyx_core::plugin_runtime::TakeoverScope,
        surface_id: String,
    },
    /// 插件请求打开独立窗口。
    PluginWindowOpen {
        request_id: u64,
        plugin_id: String,
        params: atelyx_core::plugin_runtime::WindowOpenParams,
    },
    /// 插件请求关闭自己的一个窗口。
    PluginWindowClose { request_id: u64, plugin_id: String, window_key: String },
    /// 全接管标题栏的拖动热区被按下（宿主代执行整窗拖动）。
    PluginWindowDrag(window::Id),
    /// 退出接管（常驻退出入口 / Esc）。
    PluginExitTakeover(window::Id),
    /// 窗口获得焦点（接管请求作用域 = 最近聚焦窗口）。
    WindowFocused(window::Id),
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
    /// 仓库行：在系统文件管理器中打开指定仓库根。
    RevealVaultAt(String),
    /// 最近仓库行：从列表移除入口（进弹层确认 pane）。
    RemoveVaultAsk(String),
    /// 从列表移除确认（只移出列表，不影响磁盘文件与激活态）。
    RemoveVaultConfirm(String),
    /// 取消从列表移除。
    RemoveVaultCancel,
    /// 全局配置落盘回执（失败提示，不回滚内存态）。
    GlobalSaved(Result<(), String>),
    /// 启动期全局配置读错误提示。
    StartupNotice(String),
    /// 文件操作落地（gen 守卫 + 结果通知 + 刷新子项 + 打开文件跟随）。
    OpDone(u64, String, Result<String, String>, FollowUp),
    /// 展开全部/收起全部文件夹（懒加载树递归展开）。
    ExpandAllToggle,
    /// 递归展开过程中的子项落地（累计入展开集并链式下发）。
    ExpandChildrenLoaded(String, u64, Result<Vec<vault::DirChild>, String>),
    /// 行内输入行被按下（重命名失焦提交的「点在输入行内」标志）。
    RenamePressInside,
    /// 分割把手键盘调整（上次拖动过的把手；horizontal + 正向）。
    DividerAdjust(bool, bool),
    /// 保存失败徽标的关闭。
    SaveErrorDismiss,
    /// 未实现功能的占位通知。
    NotImplemented(&'static str),
    /// 协作空间消息（账号 / 空间切换 / 空间保存回执）。
    Space(SpaceMsg),
    /// 文件面板云钮：打开协作空间菜单（未登录先弹登录层）。
    SpaceMenuOpen,
    /// 登录层切换注册 / 登录模式（弹层保持打开）。
    SpaceLoginToggle(bool),
    /// 最近空间行：从列表移除（仅移出列表，不影响服务端数据与成员资格）。
    RemoveSpaceAsk { server_url: String, space_id: String },
    /// 设置-通用：界面字号草稿输入（数字过滤；合法档位实时生效）。
    FontSizeInput(String),
    /// 设置-通用：字号提交（非法草稿回退显示当前生效值）。
    FontSizeCommit,
    /// 设置-通用：自动恢复上次打开文件开关。
    AutoRestoreToggle,
    /// 设置-通用：自动检查更新开关（启动静默检查的开关源）。
    AutoUpdateToggle,
    /// 设置-通用：开机自启开关（写系统启动项；失败提示并回读真实状态）。
    AutostartToggle,
    /// 设置-编辑器：宽松换行开关。
    SoftWrapToggle,
    /// 设置-编辑器：页面内标题开关。
    InPageTitleToggle,
    /// 设置-编辑器：正文行宽草稿输入（数字过滤）。
    ContentWidthInput(String),
    /// 设置-编辑器：正文行宽提交（0 = 不限；越界非法值忽略）。
    ContentWidthCommit,
    /// 设置-文件：排除文件夹草稿输入（逗号分隔）。
    ExcludeFoldersInput(String),
    /// 设置-文件：排除文件夹提交（空 = 删除该键）。
    ExcludeFoldersCommit,
    /// 设置-文件：附件文件夹草稿输入。
    AttachmentFolderInput(String),
    /// 设置-文件：附件文件夹提交（空 = 删除该键）。
    AttachmentFolderCommit,
    /// 设置-工作区：启动场景选择（None = 跟随当前，删除该键）。
    EntryScenePick(Option<String>),
    /// 更新流程（检查 / 下载 / 安装状态机）。
    Update(update::UpdateMsg),
}

/// 文件操作成功后对打开中笔记的跟随语义。
#[derive(Debug, Clone)]
pub enum FollowUp {
    None,
    /// 改名/移动：打开中笔记按前缀跟随；`rewritten` = 引用同步代写的笔记（打开中需重读）。
    Renamed { from: String, to: String, rewritten: Vec<String> },
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

/// 行内重命名输入挂焦点：输入行随本帧视图重建挂载，操作在重建后的树上执行。
fn rename_focus_task() -> Task<Message> {
    iced::widget::operation::focus(iced::widget::Id::new(ui::controls::INLINE_RENAME_INPUT))
}

/// 目录相对路径 + 条目名拼完整相对路径（dir = "" 即根）。
fn full_rel(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

/// 同目录取不重名目标名：候选已占用（排除 `exclude_rel` 自身，如 case-only 改名）时按
/// `-2`、`-3` 递增。`ext` = 带前导点的扩展名（None/空 = 目录或无扩展名）。
fn dedupe_sibling(
    root: &Path,
    dir: &str,
    name: &str,
    ext: Option<&str>,
    exclude_rel: &str,
) -> Result<String, String> {
    let with_ext = |stem: &str| match ext {
        Some(e) if !e.is_empty() => format!("{stem}{e}"),
        _ => stem.to_string(),
    };
    let mut candidate = with_ext(name);
    let mut seq = 2;
    loop {
        let rel = full_rel(dir, &candidate);
        if rel == exclude_rel {
            return Ok(candidate);
        }
        if !vault::safe_join(root, &rel, false)?.exists() {
            return Ok(candidate);
        }
        candidate = with_ext(&format!("{name}-{seq}"));
        seq += 1;
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
#[derive(Debug, Clone, PartialEq)]
pub enum DraftKind {
    Note,
    Folder,
    Table,
    /// 插件领域文档（扩展名与展示标签取自插件注册）。
    Doc { extension: String, label: String },
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

/// 保存动机：决定完成回执的处理（提示 / 清脏 / 放行待续动作）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveReason {
    /// Ctrl+S：完成提示「已保存」。
    Manual,
    /// 停止输入 500ms 防抖自动写回。
    AutoSave,
    /// 脏时放行动作前的收尾保存：落地且期间无新输入后放行 flush_pending（gate 挂起）。
    Flush,
}

/// flush 放行后继续的动作（挂起输入已落盘才可执行，否则动作会丢失未保存内容或在途写打到旧路径）。
#[derive(Debug, Clone)]
enum PendingAfterFlush {
    OpenFile(String),
    PickVault,
    /// 从最近仓库列表进入指定仓库（from_boot 区分启动进入与运行中切换）。
    SelectVault { path: PathBuf, from_boot: bool },
    CloseMain(window::Id),
    /// 关文件留标签（kind = "note" / "table"，只关对应面）。
    CloseFileTab(String),
    /// 仓库内笔记链接（已解码；能进 flush 的只有 Path/Wiki/Create 形态，外链直接打开）。
    OpenLink(String),
    Delete { rel: String, is_dir: bool },
    /// 文件树拖拽移动（挂起输入先落盘，防代写被未落盘内容回写覆盖）。
    MoveEntry { from: String, is_dir: bool, to_dir: String },
    RenameFile(RenameState),
    /// 从空间列表进入指定协作空间（会话恢复可能弹登录层）。
    SelectSpace { server_url: String, space_id: String, name: String },
    /// 安装就绪的更新包（挂起输入先落盘，安装器替换文件不丢未保存内容）。
    InstallUpdate,
}

/// 仓库容器身份：本地文件夹仓库 or 协作空间（同一时刻互斥激活）。
/// 本地分派 vault；空间分派 space_actions（内容真源在服务端，`vault` 会话保持空置）。
#[derive(Clone, Default)]
pub enum RepoKind {
    #[default]
    Local,
    Space(space_actions::SpaceSession),
}

/// 协作弹层表单草稿（登录 / 创建空间 / 邀请码加入共用一套草稿位）。
#[derive(Clone, Debug, Default)]
pub struct SpaceForm {
    pub server: String,
    pub username: String,
    pub password: String,
    pub device: String,
    /// 创建空间：空间名。
    pub name: String,
    /// 加入空间：邀请码。
    pub code: String,
    /// 登录层当前模式（true = 注册）。
    pub register: bool,
}

/// 协作弹层表单字段。
#[derive(Debug, Clone, Copy)]
pub enum SpaceField {
    Server,
    Username,
    Password,
    Device,
    Name,
    Code,
    /// 邀请码表单：有效时长（小时原文，空 = 服务端缺省）。
    InviteHours,
    /// 邀请码表单：次数上限（原文，空 = 不限）。
    InviteUses,
}

/// 协作空间消息：账号 / 空间切换 / 空间保存回执 / 实时事件。
/// 内容面回执尽量复用既有消息（FileLoaded / OpDone / VaultMetaLoaded 等，gen 守卫同源）。
#[derive(Debug, Clone)]
pub enum SpaceMsg {
    LoginField(SpaceField, String),
    /// 提交登录 / 注册（register = 注册模式）。
    LoginSubmit { register: bool },
    LoginDone {
        server_url: String,
        result: Result<atelyx_core::space_client::AuthInfo, atelyx_core::space_client::SpaceError>,
    },
    SpacesLoaded {
        server_url: String,
        result: Result<Vec<atelyx_core::space_client::SpaceSummary>, atelyx_core::space_client::SpaceError>,
        window: window::Id,
    },
    /// 选择要进入的空间（文件面板空间行 / 创建加入成功后）。
    SelectSpace { server_url: String, space_id: String, name: String, from_boot: bool },
    /// 会话恢复回执：Ok = 进入空间；Err = 弹登录层（启动自动进入则静默停留）。
    RestoreDone {
        server_url: String,
        space_id: String,
        name: String,
        from_boot: bool,
        result: Result<
            (Arc<atelyx_core::space_client::SpaceClient>, Vec<atelyx_core::space_client::SpaceSummary>),
            atelyx_core::space_client::SpaceError,
        >,
    },
    CreateSpace,
    CreateSpaceDone {
        result: Result<atelyx_core::space_client::SpaceSummary, atelyx_core::space_client::SpaceError>,
    },
    JoinSpace,
    JoinSpaceDone {
        result: Result<atelyx_core::space_client::AcceptAck, atelyx_core::space_client::SpaceError>,
    },
    /// 空间整树落地：切片填充 children / note_files / file_count。
    TreeLoaded {
        gen: u64,
        result: Result<Vec<atelyx_core::space_client::TreeNode>, atelyx_core::space_client::SpaceError>,
    },
    /// 空间笔记保存回执（rel = 发起时文件；frozen = 发起时冻结的正文，Ok 携带服务端合流后的最终全文）。
    NoteSaved {
        gen: u64,
        rel: String,
        frozen: String,
        reason: SaveReason,
        result: Result<String, String>,
    },
    /// 空间表格补丁回执（new_file = 服务端标题漂移后的实际路径）。
    TablePatchDone {
        gen: u64,
        file: String,
        new_file: String,
        reason: tv::SaveReason,
        result: Result<String, String>,
        frozen: atelyx_core::vault::TableFile,
    },
    /// 文件夹确认删除（needsConfirm 二段式的 force 步）。
    ForceDeleteFolder { gen: u64, rel: String },
    HistoryRecorded { err: Option<String> },
    /// 设备会话列表落地（设置页「多人协作」）。
    DevicesLoaded {
        result: Result<Vec<atelyx_core::space_client::DeviceEntry>, atelyx_core::space_client::SpaceError>,
    },
    /// 吊销指定设备会话（回执经 DeviceRevoked）。
    RevokeDevice(String),
    /// 吊销设备会话回执（吊销后重拉列表）。
    DeviceRevoked { session_id: String, result: Result<(), atelyx_core::space_client::SpaceError> },
    /// 退出登录：清密钥链令牌、服务器清单与最近空间，摘掉激活空间身份。
    Logout,
    /// 帧泵订阅建立：命令入口回填（此后协作命令经它上行；身份回验防过期泵串会话）。
    PumpReady {
        server_url: String,
        space_id: String,
        cmds: tokio::sync::mpsc::UnboundedSender<atelyx_core::collab::SpaceCmd>,
    },
    /// 帧泵事件（连接 / 名册 / 协作帧 / 断线重连；身份随行供会话回验）。
    Event {
        server_url: String,
        space_id: String,
        event: Box<atelyx_core::collab::SpaceEvent>,
    },
    /// 他端结构变更的树刷新兜底 tick（合并窗口结束后的最终一次刷新）。
    FsTreeRefresh,
    /// 空间成员名册落地（协作房间视图与成员弹层共用缓存；gen 防跨空间串台）。
    MembersLoaded {
        gen: u64,
        result: Result<Vec<atelyx_core::space_client::MemberInfo>, atelyx_core::space_client::SpaceError>,
    },
    /// 成员行内二段确认进入 / 取消（转让或移除）。
    MemberAsk(String, space_actions::MemberConfirm),
    MemberAskCancel,
    MemberTransfer(String),
    MemberRemove(String),
    /// 成员操作回执（成功后重拉名册；转让另刷空间列表对齐自身角色；gen 防跨空间串台）。
    MemberOpDone {
        gen: u64,
        op: space_actions::MemberConfirm,
        result: Result<(), atelyx_core::space_client::SpaceError>,
    },
    /// 邀请码列表落地（owner 专属端点；gen 防跨空间串台）。
    InvitesLoaded {
        gen: u64,
        result: Result<Vec<atelyx_core::space_client::InviteInfo>, atelyx_core::space_client::SpaceError>,
    },
    /// 邀请码表单：角色选择（选后回根面板）。
    InviteRolePick(atelyx_core::space_client::SpaceRole),
    /// 按表单草稿生成邀请码。
    CreateInvite,
    InviteCreated {
        gen: u64,
        result: Result<atelyx_core::space_client::InviteInfo, atelyx_core::space_client::SpaceError>,
    },
    /// 撤销邀请码二段确认进入 / 取消（code）。
    InviteRevokeAsk(String),
    InviteRevokeCancel,
    RevokeInvite(String),
    InviteRevoked {
        gen: u64,
        code: String,
        result: Result<(), atelyx_core::space_client::SpaceError>,
    },
    /// 复制邀请码（按钮转「已复制」）。
    InviteCopy(String),
    /// 协作房间视图 → 设置页「多人协作」分区（SettingsTab + SettingsToggle 组合）。
    OpenCollabSettings,
}

pub struct App {
    /// 布局状态真源（core 模型；磁盘格式由 schema 版本标识）。
    pub ui: AppUiState,
    vault: Arc<vault::VaultState>,
    /// 打开的表格会话（数据 + 保存基线 + 视图态；None = 未打开）。
    pub table: Option<table_view::TableSession>,
    /// flush 放行后待续的动作（笔记与表格任一脏时挂起，全部落盘后执行）。
    pub(crate) flush_pending: Option<PendingAfterFlush>,
    /// 键盘修饰态（Ctrl+滚轮缩放判定源）。
    pub mods: keyboard::Modifiers,
    /// 历史面板当前作用的文件类型（载入笔记/表格时置位；读/回滚按它分派）。
    pub history_kind: atelyx_core::history::HistoryKind,
    /// 表格弹层内联输入草稿（改名 / 插列 / 添加字段共用；弹层关闭清零）。
    pub table_popup_draft: String,
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
    pub(crate) popup: Option<(window::Id, PopupOpen)>,
    /// 弹层面板导航（两 pane 菜单）。
    pub popup_pane: usize,
    /// 设置输入的待提交消息（逐键武装；Enter/外点收口，见 UiMsg::InputSubmit / CommitPendingInput）。
    arm_commit: Option<Message>,
    /// 设置整页开启态与左栏 tab。
    pub settings_open: bool,
    pub settings_tab: String,
    /// 快捷键录制中的命令（None = 未录制；录制态吃掉命令候选键，
    /// Esc / 切设置分区 / 关弹层取消）。
    pub shortcut_recording: Option<commands::CommandId>,
    /// 插件宿主运行时（进程内单例；多窗口共享）。
    pub plugins: Arc<PluginRuntime>,
    /// 设置-插件页状态（列表缓存 / 详情 / 卸载确认 / 安装在途）。
    pub plugins_state: settings_view::plugins::PluginsState,
    /// 已注册贡献投影（运行时事件后刷新；菜单、标签与面板渲染都读它）。
    pub plugin_contributions: Vec<atelyx_core::plugin_runtime::Contribution>,
    /// 当前呈现布局里已挂的插件面（布局变化时重算并差额通知插件）。
    pub plugin_surfaces: HashSet<(String, String)>,
    /// 已挂载的插件设置面（注册即挂载，贡献变化时差额通知插件）。
    pub plugin_settings_surfaces: HashSet<(String, String)>,
    /// 已挂面的最新界面树投影（键 = 插件 id + 面 id）。
    pub plugin_trees: HashMap<(String, String), atelyx_core::plugin_runtime::ViewNode>,
    /// 单行 / 数字输入的宿主草稿（键 = 插件 id + 面 id + 节点 id）。
    pub plugin_drafts: HashMap<plugin_actions::InputKey, plugin_actions::InputDraft>,
    /// 多行输入的本机编辑器状态（内容与光标归宿主所有）。
    pub plugin_editors: HashMap<plugin_actions::InputKey, plugin_actions::EditorState>,
    /// 画布节点的最新绘图指令清单投影（键 = 插件 id + 面 id + 节点 id；随树投影刷新）。
    pub plugin_canvases:
        HashMap<plugin_actions::InputKey, std::sync::Arc<Vec<atelyx_core::plugin_runtime::DrawCommand>>>,
    /// 各窗口的接管态（键 = OS 窗口 id；布局真源不动，退出即恢复）。
    pub plugin_takeovers: HashMap<window::Id, plugin_actions::TakeoverActive>,
    /// 插件窗口记账（键 = OS 窗口 id；随窗口关闭 / 插件停用收尾）。
    pub plugin_windows: HashMap<window::Id, plugin_actions::PluginWindowActive>,
    /// 打开中的插件文档实例（单实例；切换文件即换实例）。
    pub doc_open: Option<plugin_actions::DocOpenInstance>,
    /// 全部已安装插件注册的领域文档类型投影（随贡献刷新；文件树图标与新建菜单读它）。
    pub plugin_doc_types: Vec<atelyx_core::plugin_runtime::DocTypeInfo>,
    /// 最近聚焦的窗口（插件接管请求的作用域）。
    pub focused_window: Option<window::Id>,
    /// 市场索引缓存落盘路径（应用数据目录不可用时为 None：不缓存，市场仅内存展示）。
    pub market_cache_path: Option<PathBuf>,
    /// 应用级全局配置（最近仓库列表；落盘 global.json）。
    pub global: global_config::GlobalConfig,
    /// 设置-通用：界面字号草稿（None = 显示当前生效值）。
    pub font_size_draft: Option<String>,
    /// 设置-通用：开机自启系统态缓存（打开设置页 / 切换后刷新；None = 未读）。
    pub autostart_enabled: Option<bool>,
    /// 更新流程状态机（检查 / 下载 / 安装）。
    pub update: update::UpdateState,
    /// 在途下载的取消标志（None = 无在途下载）。
    update_cancel: Option<Arc<AtomicBool>>,
    /// 更新包下载目录（boot 定位并创建）。
    update_download_dir: PathBuf,
    /// 下载进度暂存（下载回调线程只写；ProgressTick 读走刷入状态与常驻通知）。
    update_progress: Arc<Mutex<(u64, u64)>>,
    /// 当前检查是否手动发起（Checked 回执据此决定提示方式）。
    update_manual_check: bool,
    /// 最近一次发起的切仓目标：flush 回放时只有与它一致才执行，防快速点选落到非最后目标。
    vault_switch_target: Option<PathBuf>,
    /// 从列表移除的确认态（目标仓库根；弹层确认 pane 数据源）。
    pub remove_vault_confirm: Option<String>,
    /// 当前仓库根（打开仓库后与 vault.root() 一致；仅用于展示与 files 视图判空）。
    pub vault_root: Option<PathBuf>,
    /// 仓库容器身份（本地 / 空间；内容面按它分派）。
    pub repo_kind: RepoKind,
    /// 空间整树缓存（进入空间后拉取；children / note_files / file_count 共同数据源）。
    pub space_tree: Option<Vec<atelyx_core::space_client::TreeNode>>,
    /// 服务端空间列表（登录后拉取；文件面板空间行展示）。
    pub space_list: Vec<atelyx_core::space_client::SpaceSummary>,
    /// 协作弹层表单草稿（登录 / 创建 / 加入）。
    pub space_form: SpaceForm,
    /// 协作请求在途（弹层提交按钮禁用）。
    pub space_busy: bool,
    /// 登录前挂起的目标空间（登录成功自动续接）。
    space_pending: Option<(String, String, String)>,
    /// 会话内令牌兜底（密钥链不可用时仅本次会话有效，不落盘）。
    session_token: Option<(String, String)>,
    /// 当前打开笔记的「上次确认落盘基线」（空间模式三方合流的祖先；None = 尚未落盘）。
    pub space_note_flushed: Option<String>,
    /// 空间笔记保存在途标志（回执即清）：空间保存无写队列可依赖，并发两笔会乱序落地覆盖服务端。
    space_note_save_inflight: bool,
    /// 在途保存期间收到 Flush 请求：回执后补一笔串行收尾（关窗/切文件的脏拦截依赖它）。
    space_note_save_flush_queued: bool,
    /// 各空间独立的文件树展开集（会话内记忆；键 = 服务器/空间 id）。
    expanded_by_space: HashMap<String, Vec<String>>,
    /// 设备会话列表（设置页「多人协作」；打开该页时拉取）。
    pub space_devices: Vec<atelyx_core::space_client::DeviceEntry>,
    /// 会话令牌（WS hello 用；登录/恢复会话时置位，登出/离开清空）。
    space_token: Option<String>,
    /// 协作帧泵命令入口（订阅建立后回填；None = 泵未就绪）。
    pub space_cmds: Option<tokio::sync::mpsc::UnboundedSender<atelyx_core::collab::SpaceCmd>>,
    /// 在线成员名册（Peers / Presence 事件维护）。
    pub space_peers: Vec<atelyx_core::collab::PeerInfo>,
    /// 帧泵连接态（Ready 置位；断开 / 服务端拒绝 / 摘泵清零）。
    pub space_connected: bool,
    /// 空间成员名册缓存（协作房间视图与成员弹层共用；进入/离开空间清空）。
    pub space_members: Option<Vec<atelyx_core::space_client::MemberInfo>>,
    pub space_members_loading: bool,
    /// 名册加载失败（弹层与视图内红字；操作失败走通知，不入此字段）。
    pub space_members_error: Option<String>,
    /// 成员行内二段确认（目标 userId + 动作）。
    pub space_member_confirm: Option<(String, space_actions::MemberConfirm)>,
    /// 邀请码列表缓存（owner 专属端点；进入/离开空间清空）。
    pub space_invites: Option<Vec<atelyx_core::space_client::InviteInfo>>,
    pub space_invites_loading: bool,
    pub space_invites_error: Option<String>,
    /// 邀请码表单草稿（角色 + 可空数字字段原文，空 = 服务端缺省）。
    pub space_invite_form: space_actions::InviteForm,
    /// 最近一次生成的邀请码（撤销后清除；跨弹层开关保留）。
    pub space_invite_result: Option<atelyx_core::space_client::InviteInfo>,
    /// 复制反馈（点击复制后按钮转「已复制」；重新生成或重开弹层复位）。
    pub space_invite_copied: bool,
    /// 撤销邀请码的二段确认（code）。
    pub space_invite_confirm: Option<String>,
    /// 他端结构变更的树刷新节流（上次刷新时刻 + 合并窗口内是否已排兜底 tick）。
    space_tree_refresh_at: Option<std::time::Instant>,
    fs_tree_refresh_pending: bool,
    /// 笔记协作文档绑定（空间模式且打开笔记时）。
    pub note_collab: Option<atelyx_core::note_collab::NoteCollab>,
    /// 协作绑定当前挂的文件（None = 未挂）。
    pub note_collab_file: Option<String>,
    /// 远端成员光标（awareness 换算为 note_text 字节偏移；编辑面渲染）。
    pub remote_cursors: Vec<RemoteCursorView>,
    /// 本端帧泵 peerId（hello-ack 分配）：自声落地帧与名册自条目据此过滤。
    pub space_own_peer_id: Option<String>,
    /// 远端光标过期剔除的节流时刻（周期 tick 搭载刷新，避免每 100ms 全文换算）。
    space_cursor_prune_at: Option<std::time::Instant>,
    /// 泵被服务端拒绝后的停闸标志：订阅据此不再重生泵（重连无意义，防无退避风暴）。
    /// 容器切换时复位；重进空间重新放行。
    pub(crate) space_pump_denied: bool,
    /// 仓库文件总数（打开仓库后异步统计；统计在途为 None）。
    pub file_count: Option<usize>,
    /// 主窗口（渲染激活布局树）；None = 首帧在途。
    pub main_id: Option<window::Id>,
    /// 撕裂窗口 OS 注册：iced 窗口 id → 模型窗口 id（建窗前预注册，见 reconcile）。
    detached_os: HashMap<window::Id, String>,
    /// OS 窗口当前可见态（调和比对基准；建窗即视为可见）。
    os_shown: HashMap<window::Id, bool>,
    /// files 视图懒加载子项缓存（key = 目录相对路径，根为 ""；运行态不落盘）。
    pub children: HashMap<String, Vec<vault::DirChild>>,
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
    /// 属性面板（frontmatter 类型化渲染与槽位编辑）。
    pub props: PropsPanel,
    pub dirty: bool,
    /// 上次保存失败原因（None = 无；保存成功或再次编辑即清除）。
    pub save_error: Option<String>,
    /// 自动保存期限（最近一次内容变更 + 500ms；保存落地或到期触发时清空）。
    autosave_deadline: Option<Instant>,
    /// 自动保存定时器在途标志（单定时器链：到期检查期限，被推迟则续排剩余时长）。
    autosave_timer: bool,
    /// note 视图模式（缺省只读）。
    pub note_mode: NoteMode,
    /// 笔记面板内按压标志（NotePressInside 同批置位，全局 Press 消费）。
    note_press_inside: bool,
    /// 进编辑档待落光标（只读档双击进入时置位，首个编辑面事件消化）。
    note_edit_caret_pending: bool,
    /// 只读档的解析产物：进入只读档或文件载入时重建，避免每帧重解析。
    pub preview_doc: Option<MarkdownDocument>,
    /// 笔记图片句柄缓存（key = src 原文；随文件载入清空，在途与失败集合防重复发起）。
    pub note_images: markdown_view::ImageHandles,
    note_images_inflight: HashSet<String>,
    note_images_failed: HashSet<String>,
    /// ensure_note_images 收集的载入任务（update 收尾统一 chain，调用点不必逐个穿 Task）。
    image_loads: Option<Task<Message>>,
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
    /// 反链（当前笔记；文件载入与改名/移动后刷新）。
    pub backlinks: Vec<vault::BacklinkRow>,
    /// 全仓库标签词汇表（tags 候选；打开仓库与载入笔记后刷新）。
    pub tag_vocab: Vec<vault::TagRow>,
    /// 全仓库文件清单（双链候选与搜索面板数据源；树序，全部文件类型）。
    pub note_files: Vec<(String, String)>,
    /// 双链候选开启态（None = 关闭；from = 触发符之后的正文起点）。
    pub wiki_picker: Option<WikiPicker>,
    /// 最近一次 Esc 关闭的双链触发位置（同位置不再自动重开，直到出现新触发符）。
    wiki_dismissed_from: Option<usize>,
    /// 双链候选选中后吞掉随后的编辑面 Newline（Enter 键全局订阅与部件事件双发，见 wiki_pick）。
    wiki_swallow_newline: bool,
    /// 搜索面板查询词。
    pub search_query: String,
    /// 历史弹层/上下文的版本列表（打开弹层与每次保存后刷新；旧→新序）。
    pub history: Vec<atelyx_core::history::HistoryVersion>,
    /// 列表加载世代（回滚守卫：容器/仓库切换后列表过期）。
    pub history_gen: u64,
    /// 弹层版本列表加载中。
    pub history_loading: bool,
    /// 弹层展开预览的版本 seq（互斥；None = 全收起）。
    pub history_expanded: Option<u64>,
    /// 弹层回滚二段确认的版本 seq。
    pub history_confirm: Option<u64>,
    /// 回滚已发起、等待保存链稳定落盘的一次性标志（成功回执消费并提示，失败/切容器清除）。
    pub(crate) history_restore_pending: bool,
    /// 仓库历史面板缓存（None = 未加载/已失效；Err = 加载失败待手动刷新）。
    pub repo_history: Option<Result<Vec<repohistory_view::RepoHistoryGroup>, String>>,
    /// 仓库历史聚合在途。
    pub repo_history_loading: bool,
    /// 日历视图会话态（日程清单 / 月导航 / 活动密度缓存）。
    pub calendar: calendar_actions::CalendarState,
    /// 最近打开条目存在性校验的上次发起时刻（视图展示期节流）。
    recent_checked_at: Option<Instant>,
    /// 设置-编辑器：重建内部链接会话态。
    pub rebuild_links: settings_view::editor::RebuildLinksState,
    /// 文件面板排序（仓库 config 的 fileExplorerSort；缺省编辑时间从新到旧）。
    pub file_sort: String,
    /// 仓库级文件面板配置（打开仓库后随仓库配置读入；设置页展示与提交基线）。
    pub exclude_folders: Vec<String>,
    pub attachment_folder: Option<String>,
    /// 启动时切换的目标场景（仓库 config 的 entrySceneId；None = 跟随当前）。
    pub entry_scene_id: Option<String>,
    /// 仓库级配置基线（打开容器后随元数据读入；设置-AI 的展示与提交基线）。
    pub repo_config: vault::VaultConfig,
    /// Agent 配置清单（agents.json；打开 AI 设置页时读入）。
    pub agents: Vec<vault::AgentConfig>,
    /// 设置-AI 分区的会话内编辑状态（容器切换时整体清空）。
    pub ai: settings_view::ai::AiState,
    /// AI 设置子分区（供应商 | 模型服务 | Agent | 联网搜索）。
    /// 设置-AI 的子分区 tab 与会话内编辑状态（容器切换时整体清空）。
    pub ai_sub_tab: String,
    /// AI 对话面板状态（会话投影 / run 订阅 / 输入草稿）。
    pub chat: chat_actions::ChatState,
    /// 设置页文本项草稿（正文行宽 / 排除文件夹 / 附件文件夹；None = 显示当前值）。
    pub content_width_draft: Option<String>,
    pub exclude_folders_draft: Option<String>,
    pub attachment_folder_draft: Option<String>,
    /// 提示词标记清单（.atelyx/prompt-notes.json；会话内存态）。
    pub prompt_notes: Vec<String>,
    /// 文件夹图标颜色（.atelyx/folder-colors.json；会话内存态）。
    pub folder_colors: HashMap<String, String>,
    /// 文件树拖拽候选（行按压自证命中；超阈值转正会话，未转正释放 = 行为原样）。
    file_press: Option<FilePress>,
    /// 文件树拖拽会话（笔记/文件夹移动）。
    pub file_drag: Option<FileDrag>,
    /// 拖拽悬停中的落点（文件夹相对路径；None = 不在可落点上）。
    pub file_drag_target: Option<String>,
}

/// 编辑面渲染用的远端光标视图（字节偏移，UTF-16 已换算）。
#[derive(Debug, Clone)]
pub struct RemoteCursorView {
    pub name: String,
    pub color: String,
    pub anchor: usize,
    pub head: usize,
}

/// 文件树拖拽按下候选。
struct FilePress {
    window: window::Id,
    rel: String,
    is_dir: bool,
}

/// 文件树拖拽会话（ghost 跟随光标；释放落入文件夹行即移动）。
pub struct FileDrag {
    pub from: String,
    pub is_dir: bool,
    /// ghost 文案（末段名）。
    pub label: String,
    /// 全局光标（源窗口位置 + 本地坐标）。
    pub current: Point,
    pub cursor_window: window::Id,
}

/// 双链候选会话：`from` = 触发符（`[[` / `【【`）之后的正文偏移，查询词随编辑推进。
#[derive(Clone, Debug)]
pub struct WikiPicker {
    pub from: usize,
    pub query: String,
    pub active: usize,
}

impl App {
    fn boot() -> (Self, Task<Message>) {
        let (main_os, open) = window::open(Settings {
            size: DEFAULT_WINDOW_SIZE,
            min_size: Some(MIN_WINDOW_SIZE),
            exit_on_close_request: false,
            decorations: false,
            icon: ui::window_icon(),
            ..Settings::default()
        });
        ui::icon::init();
        // 字体加载完成前首帧以系统回退字体渲染，随后自动替换
        let fonts = iced::font::load(ui::FONT_REGULAR)
            .chain(iced::font::load(ui::FONT_MEDIUM))
            .chain(iced::font::load(ui::FONT_SEMIBOLD))
            .chain(iced::font::load(ui::FONT_DISPLAY))
            .discard();
        // 插件运行时：根 = 应用数据目录下 `plugins/`（数据目录不可用时回落系统临时目录，功能降级不崩溃）
        let plugins_root = app_data_dir()
            .map(|d| d.join("plugins"))
            .unwrap_or_else(|| std::env::temp_dir().join("atelyx-plugins"));
        let plugins = PluginRuntime::new(plugins_root);
        let (global, global_notice) = load_global_boot();
        // 搜索提供方上岗者随配置恢复（插件未声明契约时调用侧自动回落内置）
        plugins.set_search_provider(global.search_provider.clone());
        // 市场索引缓存落应用数据目录根（与 plugins/ 平级；目录不可用 = 不缓存）
        let market_cache_path = app_data_dir().map(|d| d.join("market-cache.json"));
        // 界面基准字号随配置（缺字段走默认档；越界钳回合法区间）
        ui::palette::set_ui_base(global.font_size.clamp(12, 20));
        // 启动自动进入：最后激活身份优先，缺省回落最近仓库首位（路径失效则停留未激活态）
        let auto_vault: Option<PathBuf> = match &global.last_active {
            Some(LastActive::Vault { root }) => Some(PathBuf::from(root)),
            Some(LastActive::Space { .. }) => None,
            None => global.recent_vaults.first().map(|v| PathBuf::from(&v.root)),
        };
        let mut boot = open
            .map(Message::WindowOpened)
            .chain(window::position(main_os).map(move |p| Message::PositionKnown(main_os, p)))
            .chain(fonts);
        if let Some(text) = global_notice {
            boot = boot.chain(Task::done(Message::StartupNotice(text)));
        }
        if let Some(root) = auto_vault.clone() {
            boot = boot.chain(Task::done(Message::SelectVault { path: root, from_boot: true }));
        }
        if let Some(LastActive::Space { server_url, space_id }) = &global.last_active {
            let name = global
                .recent_spaces
                .iter()
                .find(|s| s.server_url == *server_url && s.space_id == *space_id)
                .map(|s| s.name.clone())
                .unwrap_or_else(|| space_id.clone());
            boot = boot.chain(Task::done(Message::Space(SpaceMsg::SelectSpace {
                server_url: server_url.clone(),
                space_id: space_id.clone(),
                name,
                from_boot: true,
            })));
        }
        // 启动静默检查更新（autoUpdate 开启时；debug 构建跳过，避免开发期噪音）
        if !cfg!(debug_assertions)
            && update::UpdateState::Idle.can_auto_check(global.auto_update)
        {
            boot = boot.chain(Task::done(Message::Update(update::UpdateMsg::Check {
                manual: false,
            })));
        }
        let mut app = Self {
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
                arm_commit: None,
                settings_open: false,
                settings_tab: "general".into(),
                shortcut_recording: None,
                plugins: plugins.clone(),
                plugins_state: settings_view::plugins::PluginsState::default(),
                plugin_contributions: Vec::new(),
                plugin_surfaces: HashSet::new(),
                plugin_settings_surfaces: HashSet::new(),
                plugin_trees: HashMap::new(),
                plugin_drafts: HashMap::new(),
                plugin_editors: HashMap::new(),
                plugin_canvases: HashMap::new(),
                plugin_takeovers: HashMap::new(),
                plugin_windows: HashMap::new(),
                doc_open: None,
                plugin_doc_types: Vec::new(),
                focused_window: None,
                market_cache_path,
                ai_sub_tab: "providers".into(),
                chat: chat_actions::ChatState::new(),
                repo_config: vault::VaultConfig::default(),
                agents: Vec::new(),
                ai: settings_view::ai::AiState::default(),
                global,
                font_size_draft: None,
                autostart_enabled: None,
                update: update::UpdateState::Idle,
                update_cancel: None,
                update_download_dir: update_download_dir(),
                update_progress: Arc::new(Mutex::new((0, 0))),
                update_manual_check: false,
                vault_switch_target: None,
                remove_vault_confirm: None,
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
                props: PropsPanel::default(),
                dirty: false,
                save_error: None,
                autosave_deadline: None,
                autosave_timer: false,
                note_mode: NoteMode::Read,
                note_press_inside: false,
                note_edit_caret_pending: false,
                preview_doc: None,
                note_images: markdown_view::ImageHandles::new(),
                note_images_inflight: HashSet::new(),
                note_images_failed: HashSet::new(),
                image_loads: None,
                file_sort: "mtime-desc".to_string(),
                exclude_folders: Vec::new(),
                attachment_folder: None,
                entry_scene_id: None,
                content_width_draft: None,
                exclude_folders_draft: None,
                attachment_folder_draft: None,
                prompt_notes: Vec::new(),
                folder_colors: HashMap::new(),
                file_press: None,
                file_drag: None,
                file_drag_target: None,
                persist_dirty: false,
                main_size: DEFAULT_WINDOW_SIZE,
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
                backlinks: Vec::new(),
                tag_vocab: Vec::new(),
                note_files: Vec::new(),
                wiki_picker: None,
                wiki_dismissed_from: None,
                wiki_swallow_newline: false,
                search_query: String::new(),
                history: Vec::new(),
                history_gen: 0,
                history_restore_pending: false,
                history_loading: false,
                history_expanded: None,
                history_confirm: None,
                repo_history: None,
                repo_history_loading: false,
                calendar: calendar_actions::CalendarState::default(),
                recent_checked_at: None,
                rebuild_links: settings_view::editor::RebuildLinksState::Idle,
                history_kind: atelyx_core::history::HistoryKind::Note,
                table: None,
                flush_pending: None,
                mods: keyboard::Modifiers::default(),
                table_popup_draft: String::new(),
                repo_kind: RepoKind::Local,
                space_tree: None,
                space_list: Vec::new(),
                space_form: SpaceForm::default(),
                space_busy: false,
                space_pending: None,
                session_token: None,
                space_note_flushed: None,
                space_note_save_inflight: false,
                space_note_save_flush_queued: false,
                expanded_by_space: HashMap::new(),
                space_devices: Vec::new(),
                space_token: None,
                space_cmds: None,
                space_peers: Vec::new(),
                space_connected: false,
                space_members: None,
                space_members_loading: false,
                space_members_error: None,
                space_member_confirm: None,
                space_invites: None,
                space_invites_loading: false,
                space_invites_error: None,
                space_invite_form: space_actions::InviteForm::default(),
                space_invite_result: None,
                space_invite_copied: false,
                space_invite_confirm: None,
                space_own_peer_id: None,
                space_cursor_prune_at: None,
                space_pump_denied: false,
                space_tree_refresh_at: None,
                fs_tree_refresh_pending: false,
                note_collab: None,
                note_collab_file: None,
                remote_cursors: Vec::new(),
        };
        // 启动自动进入的仓库：把持久化的展开集预置为该仓库的记忆（enter_vault 据此恢复子目录展开）；
        // 空集不种子（全新安装 / 上次全收起），走无记忆的根行保底
        if let Some(root) = &auto_vault {
            if !app.ui.file_explorer_expanded.is_empty() {
                app.expanded_by_vault
                    .insert(root.clone(), app.ui.file_explorer_expanded.clone());
            }
        }
        // 插件运行时：扫描已安装清单并启动已启用插件；布局里现存的插件面就绪即通知
        app.plugins.scan();
        app.plugins.start_enabled();
        plugin_actions::sync_surfaces(&mut app);
        boot = boot.chain(plugin_actions::refresh_projection(&mut app));
        (app, boot)
    }

    /// 追加一条通知（右下角；容量上限防堆积，溢出优先淘汰最旧的非常驻条）。
    pub(crate) fn notify(&mut self, level: NoticeLevel, text: impl Into<String>) {
        self.notify_seq += 1;
        self.notifications.push(Notification::new(self.notify_seq, level, text));
        if self.notifications.len() > 5 {
            let evict = self
                .notifications
                .iter()
                .position(|n| !n.persistent)
                .unwrap_or(0);
            self.notifications.remove(evict);
        }
    }

    /// 置换更新流程的常驻通知（同一时刻至多一条：先移除旧常驻条再置入）。
    /// 动作表以分配到的通知 id 合成（[跳过] / [稍后] 等关闭动作需要它）。
    fn set_update_notice(
        &mut self,
        level: NoticeLevel,
        text: String,
        progress: Option<NoticeProgress>,
        actions: impl FnOnce(u64) -> Vec<(String, Message)>,
    ) {
        self.dismiss_update_notice();
        self.notify_seq += 1;
        let id = self.notify_seq;
        let mut n = Notification::new(id, level, text);
        n.persistent = true;
        n.progress = progress;
        n.actions = actions(id);
        self.notifications.push(n);
    }

    /// 移除更新流程的常驻通知（置入新常驻条前同样先走这里）。
    fn dismiss_update_notice(&mut self) {
        self.notifications.retain(|n| !n.persistent);
    }

    /// 世代内的光标位置（按下事件不带坐标，命中判定用最近位置）。
    pub(crate) fn cursor_for(&self, window: window::Id) -> Point {
        self.cursors.get(&window).copied().unwrap_or(Point::ORIGIN)
    }

    /// 编辑面正文（frontmatter 之后的部分）：编辑面全部坐标相对正文。
    fn edit_body(&self) -> &str {
        let n = atelyx_core::frontmatter::block_end(&self.note_text).unwrap_or(0);
        &self.note_text[n..]
    }

    /// 用编辑面新正文替换正文（保留 frontmatter 前缀）并置脏、排自动保存。
    fn set_edit_body(&mut self, body: String) {
        let n = atelyx_core::frontmatter::block_end(&self.note_text).unwrap_or(0);
        let mut next = String::with_capacity(n + body.len());
        next.push_str(&self.note_text[..n]);
        next.push_str(&body);
        self.note_text = next;
        self.dirty = true;
        self.autosave_deadline = Some(Instant::now() + AUTOSAVE_DEBOUNCE);
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        let task = self.handle(message);
        // 图片载入任务由 ensure_note_images 收集、此处统一链出（调用点无需穿 Task）
        let task = match self.image_loads.take() {
            Some(loads) => task.chain(loads),
            None => task,
        };
        self.chain_autosave_timer(task)
    }

    /// 内容变更后补排自动保存定时器（单定时器链）：期限被输入推迟时按剩余时长续排，否则到期触发保存。
    fn chain_autosave_timer(&mut self, task: Task<Message>) -> Task<Message> {
        if self.autosave_deadline.is_some() && !self.autosave_timer {
            self.autosave_timer = true;
            let delay = self
                .autosave_deadline
                .unwrap_or_else(Instant::now)
                .saturating_duration_since(Instant::now());
            return task.chain(Task::perform(
                async move { tokio::time::sleep(delay).await },
                |_| Message::AutosaveTick,
            ));
        }
        task
    }

    fn handle(&mut self, message: Message) -> Task<Message> {
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
                    // 关窗 = 退出：挂起输入先落盘（保存失败则留在应用内重试）
                    return self.gate(PendingAfterFlush::CloseMain(id));
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
                    // 主窗关闭即退出：显式收尾插件运行时（停全部插件并结束托管进程树）
                    self.plugins.shutdown();
                    iced::exit()
                } else if self.plugin_windows.contains_key(&id) {
                    plugin_actions::on_plugin_window_closed(self, id)
                } else {
                    self.detached_os.remove(&id);
                    self.os_shown.remove(&id);
                    // 被接管窗口的接管条目随窗口消失（防悬挂：插件重启不再向死面重发 attached）
                    let _ = plugin_actions::exit_takeover(self, id);
                    if self.focused_window == Some(id) {
                        self.focused_window = None;
                    }
                    // 光标记录随窗口消失；该窗在途悬停提示立即清除
                    // （cursors 缺失会让收起判定恒 false，提示与心跳订阅悬挂）
                    self.cursors.remove(&id);
                    if self.tooltip.as_ref().is_some_and(|(w, _, _, _, _, _)| *w == id) {
                        self.tooltip = None;
                    }
                    Task::none()
                }
            }
            Message::PickVault => self.gate(PendingAfterFlush::PickVault),
            Message::VaultPicked(Ok(path)) => {
                // 系统目录选择器选定：记为最近切仓目标，过 gate（选择器打开期间的输入先落盘）
                self.vault_switch_target = Some(path.clone());
                self.gate(PendingAfterFlush::SelectVault { path, from_boot: false })
            }
            Message::VaultPicked(Err(e)) => {
                self.notify(NoticeLevel::Error, e);
                Task::none()
            }
            Message::SelectVault { path, from_boot } => {
                // 记录目标：flush 回放时据此丢弃被更晚点选取代的切换
                self.vault_switch_target = Some(path.clone());
                self.gate(PendingAfterFlush::SelectVault { path, from_boot })
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
            Message::OpenFile(rel) => self.gate(PendingAfterFlush::OpenFile(rel)),
            Message::FileLoaded(gen, Ok((rel, text))) => {
                if gen != self.epoch() || self.ui.last_note_file.as_deref() != Some(rel.as_str()) {
                    return Task::none();
                }
                // 内容载入成功即登记最近打开（本地与空间模式同链回执）
                recent_view::record(self, &rel, recent_view::RecentKind::Note);
                self.note_text = text.clone();
                // 空间模式：载入成功即磁盘基线（三方合流的祖先）
                self.space_note_flushed = matches!(self.repo_kind, RepoKind::Space(_)).then(|| text.clone());
                self.source_editor = text_editor::Content::with_text(&self.note_text);
                self.edit.reset(0);
                self.dirty = false;
                self.reset_props();
                self.backlinks.clear();
                self.history.clear();
                // 图片缓存随文件清空：同 src 不同仓库指向不同文件，过期句柄不得复用
                self.note_images.clear();
                self.note_images_inflight.clear();
                self.note_images_failed.clear();
                self.refresh_preview();
                self.refresh_edit();
                self.notify(NoticeLevel::Success, "已打开");
                // 空间模式：笔记接入协作文档（基线通告 + 握手 + 身份帧）
                if matches!(self.repo_kind, RepoKind::Space(_)) {
                    space_actions::collab_bind_note(self);
                }
                self.refresh_backlinks().chain(self.reload_history())
            }
            Message::FileLoaded(_, Err(e)) => {
                // 载入失败必须清空编辑器：last_note_file 已指向新文件，残留旧内容
                // 会让 Ctrl+S 把旧内容写进新路径；空间合流基线与协作绑定同样不得残留
                self.note_text.clear();
                self.source_editor = text_editor::Content::new();
                self.edit.reset(0);
                self.edit_doc = None;
                self.edit_meta.clear();
                self.dirty = false;
                self.preview_doc = None;
                self.reset_props();
                self.space_note_flushed = None;
                self.note_collab = None;
                self.note_collab_file = None;
                self.remote_cursors.clear();
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
                        self.autosave_deadline = Some(Instant::now() + AUTOSAVE_DEBOUNCE);
                        self.refresh_edit();
                        // Source 档同样接入协作：差分合入协作文档并上行（否则本端按键对端不可见，
                        // 且远端合入后本端继续输入会以旧全文覆盖 note_text 回退远端内容）
                        space_actions::collab_after_edit(self);
                    }
                }
                Task::none()
            }
            Message::Save => {
                // Ctrl+S：单元格编辑先提交，笔记与表格各自手动保存（无打开内容 = 提示）
                table_actions::commit_edit(self);
                let mut task = Task::none();
                if self.ui.last_note_file.is_some() {
                    task = task.chain(self.submit_save(SaveReason::Manual));
                }
                if self.table.is_some() {
                    task = task
                        .chain(table_actions::submit_table_save(self, table_view::SaveReason::Manual));
                }
                if self.ui.last_note_file.is_none() && self.table.is_none() {
                    self.notify(NoticeLevel::Warning, "没有打开的文件");
                }
                task
            }
            Message::AutosaveTick => {
                self.autosave_timer = false;
                // 期限已到才触发；未到 = 输入推迟了期限，chain_autosave_timer 会按剩余时长续排
                if self.autosave_deadline.is_some_and(|at| at <= Instant::now()) {
                    self.autosave_deadline = None;
                    let mut task = Task::none();
                    if self.dirty && self.ui.last_note_file.is_some() {
                        task = task.chain(self.submit_save(SaveReason::AutoSave));
                    }
                    if self.table.as_ref().is_some_and(|t| t.dirty) {
                        task = task
                            .chain(table_actions::submit_table_save(self, table_view::SaveReason::AutoSave));
                    }
                    return task;
                }
                Task::none()
            }
            Message::Saved(gen, saved, reason, Ok(())) => {
                if gen != self.epoch() {
                    return Task::none();
                }
                self.save_error = None;
                let proceed = match reason {
                    SaveReason::Flush => {
                        // 保存发起时冻结的内容与当前一致才算落盘；期间又有输入则再收一轮
                        if self.note_text == saved {
                            self.dirty = false;
                            self.settle_history_restore(true);
                            self.after_any_flush()
                        } else {
                            self.submit_save(SaveReason::Flush)
                        }
                    }
                    SaveReason::Manual => {
                        if self.note_text == saved {
                            self.dirty = false;
                            self.settle_history_restore(true);
                            self.notify(NoticeLevel::Success, "已保存");
                        } else {
                            self.notify(NoticeLevel::Info, "保存完成，期间有新修改");
                        }
                        Task::none()
                    }
                    SaveReason::AutoSave => {
                        if self.note_text == saved {
                            self.dirty = false;
                            self.settle_history_restore(true);
                        }
                        Task::none()
                    }
                };
                // 落盘即记历史版本（edit；内容 no-op 由记录侧跳过），随后刷新版本列表
                proceed.chain(self.record_note_history(saved))
            }
            Message::Saved(_, _, reason, Err(e)) => {
                self.settle_history_restore(false);
                // flush 被保存失败打断：动作不放行（放行会丢弃未落盘内容），挂起输入保留待重试
                if matches!(reason, SaveReason::Flush) {
                    self.notify(NoticeLevel::Error, format!("保存失败，操作未执行：{e}"));
                } else {
                    self.notify(NoticeLevel::Error, format!("保存失败：{e}"));
                }
                self.save_error = Some(e);
                Task::none()
            }
            Message::NoteMode(mode) => {
                match mode {
                    NoteMode::Read => self.enter_read_mode(),
                    NoteMode::Edit => {
                        self.note_edit_caret_pending = false;
                        self.enter_edit_mode();
                    }
                    NoteMode::Source => {
                        self.note_mode = NoteMode::Source;
                        self.preview_doc = None;
                        self.edit_doc = None;
                        self.edit_meta.clear();
                        self.edit.focused = false;
                        self.note_edit_caret_pending = false;
                        self.source_editor = text_editor::Content::with_text(&self.note_text);
                    }
                }
                Task::none()
            }
            Message::NoteEnterEdit => {
                self.enter_edit_mode();
                // 光标落点交给编辑面在释放事件消化（同批按压由 PressRelay 上报，不误判外点退出）
                self.note_edit_caret_pending = true;
                Task::none()
            }
            Message::NotePressInside => {
                self.note_press_inside = true;
                Task::none()
            }
            Message::Editor(event) => self.on_editor(event),
            Message::Props(m) => self.on_props(m),
            Message::BacklinksLoaded(gen, rel, Ok(rows)) => {
                // 身份守卫：慢扫描回执晚于切文件时丢弃，防串台显示他篇反链
                if gen == self.epoch() && self.ui.last_note_file.as_deref() == Some(rel.as_str()) {
                    self.backlinks = rows;
                }
                Task::none()
            }
            // 反链尽力而为：扫描失败保持上次结果，不打断编辑
            Message::BacklinksLoaded(_, _, Err(_)) | Message::TagVocabLoaded(_, Err(_)) | Message::NoteFilesLoaded(_, Err(_)) => Task::none(),
            Message::TagVocabLoaded(gen, Ok(rows)) => {
                if gen == self.epoch() {
                    self.tag_vocab = rows;
                }
                Task::none()
            }
            Message::NoteFilesLoaded(gen, Ok(files)) => {
                if gen == self.epoch() {
                    self.note_files = files;
                }
                Task::none()
            }
            Message::NavKey(horizontal, positive) => {
                if self.wiki_picker.is_some() {
                    let n = self.wiki_candidates().len();
                    if n > 0 {
                        if let Some(p) = self.wiki_picker.as_mut() {
                            p.active = if positive { (p.active + 1) % n } else { (p.active + n - 1) % n };
                        }
                    }
                    Task::none()
                } else if horizontal
                    && self.table.as_ref().is_some_and(|t| t.view.focused && t.view.editing.is_none())
                {
                    let dir = if positive { table_view::NavDir::Right } else { table_view::NavDir::Left };
                    table_actions::on_msg(self, table_view::TableMsg::NavKey(dir))
                } else if !horizontal
                    && self.table.as_ref().is_some_and(|t| t.view.focused && t.view.editing.is_none())
                {
                    let dir = if positive { table_view::NavDir::Down } else { table_view::NavDir::Up };
                    table_actions::on_msg(self, table_view::TableMsg::NavKey(dir))
                } else {
                    self.handle(Message::DividerAdjust(horizontal, positive))
                }
            }
            Message::EnterKey => {
                if self.wiki_picker.is_some() {
                    let active = self.wiki_picker.as_ref().map(|p| p.active).unwrap_or(0);
                    let picked = self.wiki_candidates().get(active).copied().unwrap_or(0);
                    self.wiki_pick(picked);
                    Task::none()
                } else if self
                    .table
                    .as_ref()
                    .is_some_and(|t| t.view.focused && t.view.editing.is_some())
                {
                    // 编辑态 Enter 由编辑器 key_binding 转发为 CommitEnter，此处兜底不重放
                    Task::none()
                } else if self
                    .table
                    .as_ref()
                    .is_some_and(|t| t.view.focused && t.view.editing.is_none())
                {
                    // 选中态 Enter = 下移一行
                    table_actions::on_msg(self, table_view::TableMsg::NavKey(table_view::NavDir::Down))
                } else {
                    Task::none()
                }
            }
            Message::WikiPick(i) => {
                self.wiki_pick(i);
                Task::none()
            }
            Message::SearchInput(s) => {
                self.search_query = s;
                Task::none()
            }
            Message::HistoryLoaded(gen, kind, rel, versions) => {
                // 身份守卫：版本快照随 kind+文件绑定（弹层显式指定，或当前上下文同源），
                // 串台会让回滚跨文件写盘
                let popup_matches = matches!(
                    &self.popup,
                    Some((_, open)) if matches!(
                        &open.state,
                        PopupState::HistoryBrowser { kind: k, file } if *k == kind && file == &rel
                    )
                );
                let ctx_matches = match kind {
                    atelyx_core::history::HistoryKind::Note => {
                        self.history_kind == atelyx_core::history::HistoryKind::Note
                            && self.ui.last_note_file.as_deref() == Some(rel.as_str())
                    }
                    atelyx_core::history::HistoryKind::Table => {
                        self.history_kind == atelyx_core::history::HistoryKind::Table
                            && self.ui.last_table_file.as_deref() == Some(rel.as_str())
                    }
                    atelyx_core::history::HistoryKind::Canvas => false,
                };
                if gen == self.epoch() && (popup_matches || ctx_matches) {
                    self.history = versions;
                    self.history_gen = gen;
                    self.history_loading = false;
                }
                Task::none()
            }
            Message::History(m) => history_browser::on_msg(self, m),
            Message::CanvasHistoryRestored(gen, rel, result) => {
                if gen != self.epoch() {
                    return Task::none();
                }
                match result {
                    Err(e) => {
                        self.notify(NoticeLevel::Error, format!("画布回滚失败：{e}"));
                        Task::none()
                    }
                    Ok(()) => {
                        self.notify(NoticeLevel::Success, "已回滚");
                        // 记 restore 版本（尽力而为；回执经 HistoryRecorded 刷新弹层列表）
                        self.record_canvas_restore_history(rel)
                    }
                }
            }
            Message::RebuildLinks(m) => settings_view::editor::on_msg(self, m),
            Message::HistoryRecorded(gen, err) => {
                if let Some(e) = err {
                    self.notify(NoticeLevel::Error, format!("历史版本记录失败：{e}"));
                }
                // 侧文件有新版本：面板聚合缓存失效，下个 tick 重取（展示中）
                self.repo_history = None;
                // 日历活动密度随记版失效（版本与 mtime 计数已变）
                calendar_actions::invalidate_activity(self);
                if gen == self.epoch() {
                    self.reload_history()
                } else {
                    Task::none()
                }
            }
            Message::RecentPruned(gone) => {
                if !gone.is_empty() {
                    recent_view::prune(self, &gone);
                }
                Task::none()
            }
            Message::RepoHistoryLoaded(gen, result) => repohistory_view::on_loaded(self, gen, result),
            Message::RepoHistoryRefresh => repohistory_view::refresh(self),
            Message::Calendar(m) => calendar_actions::on_msg(self, m),
            Message::OpenLink(href, form) => self.open_link(&href, form),
            Message::ImageLoaded(gen, key, result) => {
                self.note_images_inflight.remove(&key);
                if gen == self.epoch() {
                    match result {
                        Ok(bytes) => {
                            self.note_images.insert(key, iced::widget::image::Handle::from_bytes(bytes));
                        }
                        // 失败进失败集：占位回落且本次会话不重试（避免滚动反复发起）
                        Err(_) => {
                            self.note_images_failed.insert(key);
                        }
                    }
                }
                Task::none()
            }
            Message::SetSort(key) => {
                self.file_sort = key.clone();
                if matches!(self.repo_kind, RepoKind::Space(_)) {
                    return space_actions::space_set_sort(self, &key);
                }
                let Some(root) = self.vault.root().ok() else { return Task::none() };
                let patch = serde_json::json!({ "fileExplorerSort": key });
                let gen = self.epoch();
                Task::perform(
                    async move { vault::patch_vault_config(&root, &patch).map(|_| ()) },
                    move |result| Message::ConfigSaved(gen, result),
                )
            }
            Message::ConfigSaved(gen, Err(e)) => {
                if gen != self.epoch() {
                    return Task::none();
                }
                self.notify(NoticeLevel::Error, format!("保存配置失败：{e}"));
                Task::none()
            }
            Message::ConfigSaved(gen, Ok(())) => {
                if gen != self.epoch() {
                    return Task::none();
                }
                // 供应商删除挂起的密钥链清理：配置落盘成功才执行（失败保留原状）
                if let Some((scope, id)) = self.ai.provider_key_delete_pending.take() {
                    if let Err(e) = credentials::delete_provider_key(&scope, &id) {
                        self.notify(NoticeLevel::Warning, format!("密钥链清理失败：{e}"));
                    }
                }
                Task::none()
            }
            Message::DuplicateEntry(rel, is_dir) => {
                self.dismiss_popup();
                self.duplicate_entry(&rel, is_dir)
            }
            Message::PromptToggle(rel) => {
                self.dismiss_popup();
                self.toggle_prompt_note(&rel)
            }
            Message::PromptNotesLoaded(gen, result) => {
                if gen == self.epoch() {
                    match result {
                        Ok(notes) => self.prompt_notes = notes,
                        Err(e) => self.notify(NoticeLevel::Error, format!("保存提示词标记失败：{e}")),
                    }
                }
                Task::none()
            }
            Message::FolderColorSet(rel, color) => {
                self.dismiss_popup();
                self.set_folder_color(&rel, color)
            }
            Message::FolderColorsLoaded(gen, result) => {
                if gen == self.epoch() {
                    match result {
                        Ok(colors) => self.folder_colors = colors,
                        Err(e) => self.notify(NoticeLevel::Error, format!("保存文件夹颜色失败：{e}")),
                    }
                }
                Task::none()
            }
            Message::VaultMetaLoaded(gen, Ok((config, prompts, colors))) => {
                if gen != self.epoch() {
                    return Task::none();
                }
                // 配置基线先整份留存（设置-AI 的展示与提交基线），字段抽取沿用既有会话态
                self.repo_config = config.clone();
                self.arm_commit = None;
                self.file_sort = config
                    .file_explorer_sort
                    .unwrap_or_else(|| "mtime-desc".to_string());
                self.exclude_folders = config.exclude_folders.unwrap_or_default();
                self.attachment_folder = config.attachment_folder;
                self.entry_scene_id = config.entry_scene_id;
                self.prompt_notes = prompts;
                self.folder_colors = colors;
                // Agent 名册随容器装载（会话的 agentId / 提示词 / 工具集不依赖打开设置页）
                let mut task = settings_view::ai::load_agents(self);
                // 会话真源装载（进仓 / 切仓 / 进空间；同源幂等）：空间后端是阻塞 HTTP，
                // 装载落在自建线程上并回执；本地同步完成
                task = task.chain(match &self.repo_kind {
                    RepoKind::Space(_) => chat_actions::load_space_sessions(self),
                    RepoKind::Local => {
                        match self.vault_root.clone() {
                            Some(root) => chat_actions::load_vault(self, &root),
                            None => chat_actions::detach_vault(self),
                        }
                        Task::none()
                    }
                });
                task
            }
            Message::VaultMetaLoaded(_, Err(_)) => Task::none(),
            Message::Chat(msg) => chat_actions::update(self, msg),
            Message::Noop => Task::none(),
            Message::FileRowPress { window, rel, is_dir } => {
                // 拖拽会话 / 弹层 / 重命名输入期间不接纳新候选
                if self.file_drag.is_none() && self.popup.is_none() && self.rename.is_none() {
                    self.file_press = Some(FilePress { window, rel, is_dir });
                }
                Task::none()
            }
            Message::FileDragEnter { rel, is_dir } => {
                if let Some(d) = &self.file_drag {
                    if is_dir {
                        // 拒绝落进自身、自身子树与自身祖先（均构成原地 no-op）
                        let into_self = d.is_dir
                            && (rel == d.from
                                || rel.starts_with(&format!("{}/", d.from))
                                || d.from.starts_with(&format!("{rel}/")));
                        if !into_self {
                            self.file_drag_target = Some(rel);
                        }
                    }
                }
                Task::none()
            }
            Message::FileDragExit { rel } => {
                if self.file_drag_target.as_deref() == Some(rel.as_str()) {
                    self.file_drag_target = None;
                }
                Task::none()
            }
            Message::ToggleTask(item_from) => {
                if let Some((next, at)) = markdown_edit::toggle_task(self.edit_body(), item_from) {
                    self.edit.snapshot(&self.note_text);
                    self.set_edit_body(next);
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
            // × = 关文件留标签：摘下对应视图的文件回到未打开态（tab 不动）
            Message::PanelTabFileClose(kind) | Message::DetachedTabFileClose(kind) => {
                self.gate(PendingAfterFlush::CloseFileTab(kind))
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
                // 插件窗口：声明 hide_on_blur 的隐藏（浮窗形态）
                if self.plugin_windows.contains_key(&os_id) {
                    return plugin_actions::on_plugin_window_blur(self, os_id);
                }
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
                rename_focus_task()
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
                        // 改名/移动一律过 gate：除「目标 = 打开中笔记」外，链接到该笔记的打开中笔记
                        // 也会被引用同步代写，挂起输入先落盘（gate 无脏时零成本直通）
                        self.gate(PendingAfterFlush::RenameFile(r))
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
                rename_focus_task()
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
                // 主页面板懒加载搭载：历史面板缓存失效时聚合，最近打开条目按节流校验存在性，日历按需装载
                let lazy = repohistory_view::tick(self)
                    .chain(recent_view::tick(self))
                    .chain(calendar_actions::tick(self))
                    .chain(space_actions::members_tick(self));
                if !self.persist_dirty {
                    return lazy;
                }
                self.persist_dirty = false;
                let Some(path) = ui_state_path() else { return lazy };
                match serde_json::to_string(&self.ui) {
                    Ok(json) => Task::perform(
                        async move { vault::atomic_write(&path, &json) },
                        Message::PersistDone,
                    )
                    .chain(lazy),
                    Err(e) => {
                        self.notify(NoticeLevel::Error, format!("布局序列化失败：{e}"));
                        lazy
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
                chat_actions::on_ui_tick(self);
                self.notifications.retain(|n| !n.expired());
                // 表格图片长按检查（拖拽排序激活）
                table_actions::check_long_press(self);
                // 远端光标过期剔除节流（1s）：对端崩溃/断网无离场帧，靠 awareness 超时惰性剔除
                if self.note_collab.is_some()
                    && self
                        .space_cursor_prune_at
                        .is_none_or(|at| at.elapsed() >= std::time::Duration::from_secs(1))
                {
                    self.space_cursor_prune_at = Some(Instant::now());
                    space_actions::refresh_remote_cursors(self);
                }
                // awareness 保活重发（core 内 15s 限频，连接期间搭载）：空闲端身份帧
                // 否则被对端 30s 超时剔除——后加入者与对端互见都依赖周期重发
                if self.space_connected {
                    space_actions::awareness_keepalive(self);
                }
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
                // 下载进度刷新搭载同一心跳：Downloading 时派发 ProgressTick 读暂存刷入
                if matches!(self.update, update::UpdateState::Downloading { .. }) {
                    return Task::done(Message::Update(update::UpdateMsg::ProgressTick));
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
                // 焦点窗口跟踪（接管请求作用域 = 最近按压的窗口）
                self.focused_window = Some(id);
                // 笔记面板内按压标志：同批 widget 消息（PressRelay）先行置位，此处一次性消费
                let note_press_inside = std::mem::take(&mut self.note_press_inside);
                // 对话输入面外点失焦：同批输入部件按压（Focus 事件）先行置位
                let chat_press_inside = std::mem::take(&mut self.chat.press_inside);
                if self.chat.input.focused && !chat_press_inside {
                    self.chat.input.focused = false;
                }
                // 表格焦点：同批表格面按压（画布或菜单，widget 消息先行）已置 press_seen = 保持聚焦；
                // 否则本次按压在表格面之外 = 失焦（键盘分派随之让位）
                if let Some(s) = &mut self.table {
                    if s.view.press_seen {
                        s.view.press_seen = false;
                    } else {
                        s.view.focused = false;
                    }
                }
                // 弹层打开期间按压交由捕获层处理（外点关闭），不触发拖拽手势
                if self.popup.as_ref().is_some_and(|(w, _)| *w == id) {
                    return Task::none();
                }
                // 笔记编辑档外点退出：按压未落在笔记面板内 = 回只读档（面板内含工具条 / 属性 / 反链；
                // 弹层打开期间由上方早退守卫豁免，源码档不参与）
                if self.note_mode == NoteMode::Edit && !note_press_inside {
                    self.enter_read_mode();
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
                // 日历日程标题编辑失焦提交：按压不在输入行内 = 提交（Enter 同路径；编辑会话至多一个）
                if self.calendar.edit.is_some() {
                    if self.calendar.press_inside {
                        self.calendar.press_inside = false;
                    } else {
                        return Task::perform(async {}, |_| {
                            Message::Calendar(calendar_actions::CalendarMsg::EditCommit)
                        });
                    }
                }
                // 表格编辑器失焦提交：按压不在编辑器浮层内 = 提交（同批 widget 消息已先置 editor_inside）
                if self.table.as_ref().is_some_and(|t| t.view.editing.is_some()) {
                    if self.table.as_ref().is_some_and(|t| t.view.editor_inside) {
                        if let Some(s) = &mut self.table {
                            s.view.editor_inside = false;
                        }
                    } else {
                        table_actions::commit_edit(self);
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
                if let Some(fp) = self.file_press.take() {
                    let moved = self.press.as_ref().is_some_and(|(w, p)| {
                        *w == fp.window && ((pos.x - p.x).abs() > 5.0 || (pos.y - p.y).abs() > 5.0)
                    });
                    // 按住候选期间弹层/重命名被打开 = 释放左键之外的交互，放弃起拖
                    if moved && self.popup.is_none() && self.rename.is_none() {
                        let label = fp.rel.rsplit('/').next().unwrap_or(&fp.rel).to_string();
                        let current = self
                            .os_positions
                            .get(&id)
                            .map(|w| Point::new(w.x + pos.x, w.y + pos.y))
                            .unwrap_or(Point::new(0.0, 0.0));
                        self.file_drag = Some(FileDrag {
                            from: fp.rel,
                            is_dir: fp.is_dir,
                            label,
                            current,
                            cursor_window: id,
                        });
                        self.file_drag_target = None;
                        return Task::none();
                    }
                    self.file_press = Some(fp);
                }
                let moved = self
                    .tab_press
                    .as_ref()
                    .zip(self.press.as_ref())
                    .is_some_and(|(tp, (w, p))| {
                        *w == tp.window && ((pos.x - p.x).abs() > 5.0 || (pos.y - p.y).abs() > 5.0)
                    });
                if moved {
                    if let Some(tp) = self.tab_press.take() {
                        return self.start_drag_from_press(tp);
                    }
                }
                if self.file_drag.is_some() {
                    if let Some(d) = self.file_drag.as_mut() {
                        d.cursor_window = id;
                        if let Some(wpos) = self.os_positions.get(&id) {
                            d.current = Point::new(wpos.x + pos.x, wpos.y + pos.y);
                        }
                    }
                    return Task::none();
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
                self.file_press = None;
                // 表格手势悬挂清理：松手在画布外时画布收不到 Release（按压会话作废，
                // 防下一次无关移动触发幽灵灯箱/翻页/框选；画布内松手已在同批先行消费）
                if let Some(s) = self.table.as_mut() {
                    s.view.press = None;
                    s.view.swipe = None;
                    s.view.img_drag = None;
                    s.view.reorder = None;
                    s.view.col_drag = None;
                    s.view.row_drag = None;
                }
                if let Some(d) = self.file_drag.take() {
                    return self.resolve_file_drop(d);
                }
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
            Message::Update(msg) => self.on_update(msg),
            Message::Table(msg) => {
                // 数据版本变化 = 表格编辑：与笔记同口径推迟自动保存期限
                let version_before = self.table.as_ref().map(|t| t.version);
                let task = table_actions::on_msg(self, msg);
                if self.table.as_ref().is_some_and(|t| t.dirty)
                    && self.table.as_ref().map(|t| t.version) != version_before
                {
                    self.autosave_deadline = Some(Instant::now() + AUTOSAVE_DEBOUNCE);
                }
                // 滚动/载入后补拉可见区图片句柄（无待加载 = 无任务）
                task.chain(table_actions::ensure_image_loads(self).unwrap_or_default())
            }
            Message::TableImagesLoaded(gen, results) => {
                if gen != self.epoch() {
                    return Task::none();
                }
                Task::batch(results.into_iter().map(|(rel, fp, result)| {
                    self.handle(Message::Table(table_view::TableMsg::ImageLoaded { rel, fp, result }))
                }))
            }
            Message::ModsChanged(m) => {
                self.mods = m;
                Task::none()
            }
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
            Message::WindowResizeStart(id, dir) => window::drag_resize(id, dir),
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
                // 录制态：Esc 仅取消录制，不触发其余 Esc 语义
                if self.shortcut_recording.take().is_some() {
                    return Task::none();
                }
                // Esc 作用域 = 事件来源窗口：先取消文件拖拽，再关双链候选，再关该窗口弹层，
                // 再退出接管（接管面内的弹层先于接管退出），再取消属性面板编辑态与行内重命名
                if self.file_drag.take().is_some() {
                    self.file_drag_target = None;
                    self.file_press = None;
                    return Task::none();
                }
                if self.wiki_picker.is_some() {
                    self.close_wiki_picker(true);
                    return Task::none();
                }
                if self.popup.as_ref().is_some_and(|(w, _)| *w == window) {
                    self.dismiss_popup();
                } else if self.plugin_takeovers.contains_key(&window) {
                    // 接管窗口：弹层已关（上一分支），Esc = 退出接管（退出入口常驻，Esc 是后备通道）
                    return plugin_actions::exit_takeover(self, window);
                } else if self.main_id == Some(window)
                    && (self.props.editing.is_some() || self.props.adding.is_some())
                {
                    return self.on_props(PropsMsg::CancelSlot);
                } else if self.calendar.edit.take().is_some() {
                    // 日历日程标题编辑 Esc = 取消（草稿丢弃）
                } else if self.main_id == Some(window) && self.rename.take().is_some() {
                    // 行内重命名 Esc = 取消
                } else if layout_view::window_shows_table(self, window)
                    && self.table.as_ref().is_some_and(|t| t.view.lightbox.is_some())
                {
                    // 灯箱 Esc = 关闭预览
                    if let Some(s) = &mut self.table {
                        s.view.lightbox = None;
                    }
                    return Task::none();
                } else if layout_view::window_shows_table(self, window)
                    && self.table.as_ref().is_some_and(|t| t.view.focused && t.view.editing.is_some())
                {
                    // 编辑态 Esc = 取消并清选中
                    return table_actions::on_msg(self, table_view::TableMsg::CancelEdit);
                } else if layout_view::window_shows_table(self, window)
                    && self
                        .table
                        .as_ref()
                        .is_some_and(|t| t.view.focused && t.view.selection.is_some())
                {
                    // 选中态 Esc = 清选中
                    if let Some(s) = &mut self.table {
                        s.view.selection = None;
                    }
                }
                Task::none()
            }
            Message::SettingsToggle => {
                self.settings_open = !self.settings_open;
                self.dismiss_popup();
                // 打开设置页刷新开机自启缓存（用户可能在系统侧改过启动项）
                if self.settings_open {
                    self.autostart_enabled = Some(autostart::enabled());
                }
                Task::none()
            }
            Message::SettingsTab(tab) => {
                // 切分区即取消快捷键录制（录制行不在新分区，防录制态悬挂吃键）
                self.shortcut_recording = None;
                self.settings_tab = tab.clone();
                // 协作页打开即拉设备会话列表（有登录身份才有内容）
                if tab == "collab" && self.global.last_identity.is_some() {
                    return space_actions::fetch_devices(self);
                }
                // 通用页打开即刷新开机自启缓存
                if tab == "general" {
                    self.autostart_enabled = Some(autostart::enabled());
                }
                // AI 页打开即拉 Agent 配置清单（并兜底选中首项）
                if tab == "ai" {
                    settings_view::ai::ensure_selection(self);
                    return settings_view::ai::load_agents(self);
                }
                // 插件页打开即刷新已装列表
                if tab == "plugins" {
                    return settings_view::plugins::update(self, settings_view::plugins::PluginsMsg::Refresh);
                }
                Task::none()
            }
            Message::KeyCommand { key, mods } => self.on_key_command(key, mods),
            Message::ShortcutRecord(id) => {
                self.shortcut_recording = Some(id);
                Task::none()
            }
            Message::ShortcutReset(id) => {
                self.global.command_shortcuts.remove(id.key());
                self.save_global_now();
                Task::none()
            }
            Message::Ai(msg) => settings_view::ai::update(self, msg),
            Message::Plugins(msg) => settings_view::plugins::update(self, msg),
            Message::PluginEvent(event) => {
                // 插件（重启 / 重新启用后）就绪：已挂的面重新通知，插件据此重建界面；
                // 停用 / 失败 / 熔断：该插件的窗口随会话收尾关闭（重启退避中不动作）
                let mut task = Task::none();
                if let PluginEvent::StatusChanged { id, status } = event.as_ref() {
                    if *status == PluginStatus::Running {
                        for (plugin_id, surface_id) in self.plugin_surfaces.iter() {
                            if plugin_id == id {
                                self.plugins
                                    .notify_surface_attached(plugin_id, surface_id, SurfaceKind::Panel);
                            }
                        }
                        // 插件窗口 / 接管面 / 打开中的文档实例随重启重新就绪
                        plugin_actions::resync_surfaces_on_running(self, id);
                    } else if matches!(
                        status,
                        PluginStatus::Stopped | PluginStatus::Failed(_) | PluginStatus::CircuitBroken
                    ) {
                        let to_close: Vec<window::Id> = self
                            .plugin_windows
                            .iter()
                            .filter(|(_, w)| w.plugin_id == *id)
                            .map(|(os, _)| *os)
                            .collect();
                        for os in to_close {
                            task = task.chain(window::close(os));
                        }
                    }
                }
                settings_view::plugins::on_event(self, *event);
                // 运行时事件（界面提交 / 贡献变化 / 状态变化）后重投影：树与贡献都取最新快照
                task.chain(plugin_actions::refresh_projection(self))
            }
            Message::PluginView { plugin_id, surface_id, node_id, event } => {
                plugin_actions::on_view_event(self, plugin_id, surface_id, node_id, event)
            }
            Message::PluginList { window, entries } => {
                let point = self.cursor_for(window);
                Task::done(Message::PopupOpen(
                    window,
                    PopupOpen {
                        point: Point::new(point.x, point.y + 16.0),
                        flip_y: Some(point.y - 12.0),
                        state: PopupState::PluginList { entries },
                        min_width: 0.0,
                    },
                ))
            }
            Message::PluginTextEdit { plugin_id, surface_id, node_id, action } => {
                plugin_actions::on_text_edit(self, plugin_id, surface_id, node_id, action)
            }
            Message::PluginTakeoverAsk { request_id, plugin_id, scope, surface_id } => {
                plugin_actions::on_takeover_request(self, request_id, plugin_id, scope, surface_id)
            }
            Message::PluginWindowOpen { request_id, plugin_id, params } => {
                plugin_actions::on_window_open(self, request_id, plugin_id, params)
            }
            Message::PluginWindowClose { request_id, plugin_id, window_key } => {
                plugin_actions::on_window_close(self, request_id, plugin_id, window_key)
            }
            Message::PluginWindowDrag(id) => {
                // 拖动热区只在工作区窗口的接管态生效（普通面板里的 dragRegion 不拖宿主窗口）
                if self.plugin_takeovers.contains_key(&id) {
                    window::drag(id)
                } else {
                    Task::none()
                }
            }
            Message::PluginExitTakeover(id) => plugin_actions::exit_takeover(self, id),
            Message::WindowFocused(id) => {
                self.focused_window = Some(id);
                Task::none()
            }
            Message::AgentsLoaded(gen, result) => {
                if gen == self.epoch() {
                    match result {
                        Ok(agents) => {
                            // 内置 Agent 缺失即补：本地写盘（种子补齐落 agents.json），
                            // 空间仅内存补齐（种子不是用户操作，不改团队共享清单）
                            let disk_len = agents.len();
                            let seeded = atelyx_core::vault::ensure_builtin_agents(agents);
                            // 以「种子是否新增条目」判定（比较磁盘名单而非内存——内存可能只是还没跟上
                            // 最近一次保存的回执），避免每次改名都追加一次相同内容的写盘
                            let added = seeded.len() > disk_len;
                            self.agents = seeded;
                            settings_view::ai::ensure_selection(self);
                            // 本地缺失写盘；空间不动团队共享清单（种子仅在内存生效）。
                            // 先更新内存再写：写后回执名单齐备，种子不再新增，不会二次保存
                            if added {
                                if let crate::RepoKind::Local = self.repo_kind {
                                    let agents = self.agents.clone();
                                    return settings_view::ai::save_agents_pub(self, agents);
                                }
                            }
                        }
                        Err(e) => self.notify(NoticeLevel::Error, format!("Agent 配置读取失败：{e}")),
                    }
                }
                Task::none()
            }
            Message::FontSizeInput(v) => {
                let digits: String = v.chars().filter(|c| c.is_ascii_digit()).take(2).collect();
                // 合法档位实时生效并落盘；非法值不动当前生效字号
                if let Ok(size) = digits.parse::<u32>() {
                    if (12..=20).contains(&size) && size != self.global.font_size {
                        self.global.font_size = size;
                        ui::palette::set_ui_base(size);
                        // 编辑面块度量以行高换算落点，字号变更须随基字号重建
                        self.refresh_edit();
                        if let Some(t) = self.table.as_mut() {
                            t.rebuild_layout();
                        }
                        self.save_global_now();
                    }
                }
                self.font_size_draft = Some(digits);
                Task::none()
            }
            Message::FontSizeCommit => {
                // 清草稿 = 回退显示当前生效值
                self.font_size_draft = None;
                Task::none()
            }
            Message::AutoRestoreToggle => {
                self.global.auto_restore = !self.global.auto_restore;
                self.save_global_now();
                Task::none()
            }
            Message::AutoUpdateToggle => {
                self.global.auto_update = !self.global.auto_update;
                self.save_global_now();
                Task::none()
            }
            Message::AutostartToggle => {
                let target = !self.autostart_enabled.unwrap_or_else(autostart::enabled);
                if let Err(e) = autostart::set_enabled(target) {
                    self.notify(NoticeLevel::Error, format!("设置开机自启失败：{e}"));
                }
                // 回读系统真实状态：写失败时开关不虚报
                self.autostart_enabled = Some(autostart::enabled());
                Task::none()
            }
            Message::SoftWrapToggle => {
                self.global.soft_wrap = !self.global.soft_wrap;
                self.save_global_now();
                Task::none()
            }
            Message::InPageTitleToggle => {
                self.global.in_page_title = !self.global.in_page_title;
                self.save_global_now();
                Task::none()
            }
            Message::ContentWidthInput(v) => {
                let digits: String = v.chars().filter(|c| c.is_ascii_digit()).take(5).collect();
                self.content_width_draft = Some(digits);
                Task::none()
            }
            Message::ContentWidthCommit => {
                if let Some(w) = self.content_width_draft.take().and_then(|d| d.parse::<u32>().ok()) {
                    if w <= 5000 && w != self.global.content_width {
                        self.global.content_width = w;
                        self.save_global_now();
                    }
                }
                Task::none()
            }
            Message::ExcludeFoldersInput(v) => {
                self.exclude_folders_draft = Some(v);
                Task::none()
            }
            Message::ExcludeFoldersCommit => {
                if let Some(draft) = self.exclude_folders_draft.take() {
                    let folders: Vec<String> = draft
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect();
                    if folders != self.exclude_folders {
                        self.exclude_folders = folders.clone();
                        // 空清单删键（缺省 = 无排除，保持 config 干净）
                        let value = if folders.is_empty() {
                            serde_json::Value::Null
                        } else {
                            serde_json::json!(folders)
                        };
                        return self.patch_repo_config(serde_json::json!({ "excludeFolders": value }));
                    }
                }
                Task::none()
            }
            Message::AttachmentFolderInput(v) => {
                self.attachment_folder_draft = Some(v);
                Task::none()
            }
            Message::AttachmentFolderCommit => {
                let folder = self
                    .attachment_folder_draft
                    .take()
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                let value = (!folder.is_empty()).then_some(folder);
                if value != self.attachment_folder {
                    self.attachment_folder = value.clone();
                    // 空值发 null = 删键（缺省 = 仓库根目录）
                    return self
                        .patch_repo_config(serde_json::json!({ "attachmentFolder": value }));
                }
                Task::none()
            }
            Message::EntryScenePick(id) => {
                self.dismiss_popup();
                if id != self.entry_scene_id {
                    self.entry_scene_id = id.clone();
                    // None 发 null = 删键（缺省 = 保持上次界面）
                    return self.patch_repo_config(serde_json::json!({ "entrySceneId": id }));
                }
                Task::none()
            }
            Message::PopupOpen(window, open) => {
                // 拖拽会话中不接受弹层（行右键等路径；释放结算后才恢复）
                if self.file_drag.is_some() {
                    return Task::none();
                }
                // 成员 / 邀请码弹层：打开即重置交互态并拉取数据（重开 = 强制刷新）
                let fetch = match &open.state {
                    PopupState::SpaceMembers => {
                        self.space_member_confirm = None;
                        Some(space_actions::fetch_members(self))
                    }
                    PopupState::SpaceInvite => {
                        self.space_invite_confirm = None;
                        self.space_invite_copied = false;
                        Some(space_actions::fetch_invites(self))
                    }
                    _ => None,
                };
                self.popup = Some((window, open));
                self.popup_pane = 0;
                // 表格弹层内联输入以空草稿回退显示当前值
                self.table_popup_draft.clear();
                fetch.unwrap_or_else(Task::none)
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
                rename_focus_task()
            }
            Message::RenameStartFile(rel) => {
                self.dismiss_popup();
                // 笔记重命名初值去 .md 后缀（标题即文件名约定）
                let name = rel.rsplit('/').next().unwrap_or(&rel).to_string();
                let name = name.strip_suffix(".md").map(|s| s.to_string()).unwrap_or(name);
                self.rename = Some(RenameState { target: RenameTarget::File(rel), value: name });
                rename_focus_task()
            }
            Message::RenameStartFolder(dir) => {
                self.dismiss_popup();
                let name = dir.rsplit('/').next().unwrap_or(&dir).to_string();
                self.rename = Some(RenameState { target: RenameTarget::Folder(dir), value: name });
                rename_focus_task()
            }
            Message::DeleteAsk(rel) => {
                // 文件删除 = 直接进确认 pane（无标题行确认态）
                self.popup_pane = 1;
                self.delete_confirm = Some((rel, false, None));
                Task::none()
            }
            Message::FolderDeleteAsk(rel) => {
                // 文件夹删除 = 非强制删除先行：空目录直接删；非空返回条目数进确认 pane
                if matches!(self.repo_kind, RepoKind::Space(_)) {
                    return space_actions::space_delete_entry(self, &rel, true, false);
                }
                let Some(root) = self.vault.root().ok() else { return Task::none() };
                let gen = self.epoch();
                let rel_task = rel.clone();
                Task::perform(
                    async move { vault::delete_folder(&root, &rel_task, false).map(|r| (r.needs_confirm, r.item_count)) },
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
                // 删除命中打开中的笔记/表格：挂起输入先落盘（防未保存内容被静默清空）
                let hits = |open: Option<&str>| {
                    open.is_some_and(|f| {
                        if is_dir {
                            f == rel || f.starts_with(&format!("{rel}/"))
                        } else {
                            f == rel
                        }
                    })
                };
                let hits_open = hits(self.ui.last_note_file.as_deref())
                    || hits(self.ui.last_table_file.as_deref());
                if hits_open {
                    self.gate(PendingAfterFlush::Delete { rel, is_dir })
                } else {
                    self.dismiss_popup();
                    self.delete_confirm = None;
                    self.delete_entry(&rel, is_dir)
                }
            }
            Message::RevealVaultAt(root) => {
                let path = PathBuf::from(root);
                #[cfg(windows)]
                {
                    let _ = std::process::Command::new("explorer").arg(&path).spawn();
                }
                #[cfg(not(windows))]
                {
                    let _ = std::process::Command::new("xdg-open").arg(&path).spawn();
                }
                Task::none()
            }
            Message::RemoveVaultAsk(root) => {
                // 进弹层确认 pane（与删除确认同形）：条目留在当前菜单内二段确认
                self.remove_vault_confirm = Some(root);
                self.popup_pane = 1;
                Task::none()
            }
            Message::RemoveVaultConfirm(root) => {
                self.dismiss_popup();
                self.remove_vault_confirm = None;
                self.global.remove_vault(&root);
                self.save_global()
            }
            Message::RemoveVaultCancel => {
                self.remove_vault_confirm = None;
                self.popup_pane = 0;
                Task::none()
            }
            Message::GlobalSaved(Err(e)) => {
                self.notify(NoticeLevel::Error, format!("最近仓库列表保存失败：{e}"));
                Task::none()
            }
            Message::GlobalSaved(Ok(())) => Task::none(),
            Message::StartupNotice(text) => {
                self.notify(NoticeLevel::Error, text);
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
                // 引用同步代写了打开中的笔记（rewritten 键 = 落盘后路径，与跟随后的
                // last_note_file 比对）：磁盘正文已变，重读对齐——改名/移动的 gate 已先
                // flush，此刻无未落盘输入可丢
                let mut reload_note: Option<String> = None;
                if let FollowUp::Renamed { rewritten, .. } = &follow {
                    if let Some(f) = self.ui.last_note_file.clone() {
                        if rewritten.contains(&f) {
                            reload_note = Some(f);
                        }
                    }
                }
                // 目录改名/移动：children 缓存按前缀跟随（展开集已在 apply_follow 换路，
                // 缓存不跟随会让移动后的已展开目录显示「展开但空」）
                if let FollowUp::Renamed { from, to, .. } = &follow {
                    let prefix = format!("{from}/");
                    self.children = self
                        .children
                        .drain()
                        .map(|(k, v)| {
                            if k == *from {
                                (to.clone(), v)
                            } else if let Some(rest) = k.strip_prefix(&prefix) {
                                (format!("{to}/{rest}"), v)
                            } else {
                                (k, v)
                            }
                        })
                        .collect();
                }
                // 空间模式 children 切片自整树缓存：结构变更后必须先拉新树，
                // 否则切片复用旧树（新建文件不显示）；Renamed 的源/目标目录已含在整树内
                let mut refresh = if matches!(self.repo_kind, RepoKind::Space(_)) {
                    space_actions::refresh_space_tree(self)
                } else {
                    self.load_children(&dir)
                };
                if !matches!(self.repo_kind, RepoKind::Space(_)) {
                    if let FollowUp::Renamed { from, .. } = &follow {
                        let from_dir = parent_of(from);
                        if from_dir != dir {
                            refresh = refresh.chain(self.load_children(&from_dir));
                        }
                    }
                }
                let mut next = refresh
                    .chain(self.refresh_note_files())
                    .chain(self.refresh_tag_vocab())
                    .chain(self.refresh_backlinks());
                if matches!(follow, FollowUp::Renamed { .. }) {
                    next = next.chain(self.load_vault_meta());
                }
                if let Some(rel) = reload_note {
                    next = next.chain(self.load_note(&rel));
                }
                next
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
                for child in entries {
                    let (rel, is_dir) = (child.rel, child.is_dir);
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
            Message::Space(msg) => match msg {
                // 空间切换走脏拦截（登录恢复在途可能有未落盘输入）；启动自动进入直通
                SpaceMsg::SelectSpace { server_url, space_id, name, from_boot } => {
                    if from_boot {
                        space_actions::handle(
                            self,
                            SpaceMsg::SelectSpace { server_url, space_id, name, from_boot: true },
                        )
                    } else {
                        self.gate(PendingAfterFlush::SelectSpace {
                            server_url,
                            space_id,
                            name,
                        })
                    }
                }
                other => space_actions::handle(self, other),
            },
            Message::SpaceMenuOpen => {
                let Some(window) = self.main_id else { return Task::none() };
                let cursor = self.cursor_for(window);
                space_actions::open_space_menu(self, window, cursor)
            }
            Message::SpaceLoginToggle(register) => {
                self.space_form.register = register;
                if let Some(window) = self.popup.as_ref().map(|(w, _)| *w) {
                    space_actions::open_login_popup(self, window)
                } else {
                    Task::none()
                }
            }
            Message::RemoveSpaceAsk { server_url, space_id } => {
                self.dismiss_popup();
                // 只影响本机最近记录：服务端账号空间始终随列表展示，移除无效时明示
                let in_recents = self
                    .global
                    .recent_spaces
                    .iter()
                    .any(|s| s.server_url == server_url && s.space_id == space_id);
                self.global.remove_space(&server_url, &space_id);
                if !in_recents {
                    self.notify(NoticeLevel::Info, "该空间来自服务器账号，列表展示不受影响");
                }
                self.save_global()
            }
        }
    }

    /// 应用一个布局操作：形状校验 + 变更 + 调度落盘 + 撕裂窗口调和
    /// （iced 单状态树天然全窗口可见，无广播层）。布局操作全部伴随弹层收起。
    pub(crate) fn apply(&mut self, op: LayoutOp) -> Task<Message> {
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
        // 布局变化可能增减插件面：差额通知插件（attached / detached）；面集合确实变了才重投影
        // （改分割比例这类高频布局操作不触发重投影）
        let surfaces_changed = plugin_actions::sync_surfaces(self);
        let reconcile = self.reconcile();
        let task = match surfaces_changed {
            true => reconcile.chain(plugin_actions::refresh_projection(self)),
            false => reconcile,
        };
        (task, result)
    }

    /// 收起当前弹层（菜单动作完成后的统一收尾）。
    pub(crate) fn dismiss_popup(&mut self) {
        self.popup = None;
        self.popup_pane = 0;
        // 弹层收起即取消快捷键录制（录制态不得跨弹层存续吃键）
        self.shortcut_recording = None;
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

    pub(crate) fn schedule_persist(&mut self) {
        self.persist_dirty = true;
    }

    /// 会话世代：core 在每次切换仓库时自增；在途任务的回调据此判定是否过期。
    pub(crate) fn epoch(&self) -> u64 {
        self.vault
            .generation
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// 笔记与表格任一脏。
    fn any_dirty(&self) -> bool {
        self.dirty || self.table.as_ref().is_some_and(|t| t.dirty)
    }

    /// 脏时先收尾挂起输入再放行动作；不脏直接执行。
    /// 笔记与表格任一脏即挂起 pending，全部落盘后执行（after_any_flush）。
    fn gate(&mut self, pending: PendingAfterFlush) -> Task<Message> {
        // 在途单元格草稿先提交（提交即置脏，走下面的 flush；无改动 = 零成本直通）：
        // 打开/关闭/切仓库/改名/删除都会替换或丢弃表格会话，草稿不得随会话被静默丢弃
        if self.table.as_ref().is_some_and(|t| t.view.editing.is_some()) {
            table_actions::commit_edit(self);
        }
        if self.any_dirty() {
            self.flush_pending = Some(pending);
            return self.flush_dirty_now();
        }
        self.proceed(pending)
    }

    /// 提交当前所有脏面的保存（flush 语义；全部落地后由回执侧放行 flush_pending）。
    fn flush_dirty_now(&mut self) -> Task<Message> {
        let mut task = Task::none();
        if self.dirty {
            task = task.chain(self.submit_save(SaveReason::Flush));
        }
        if self.table.as_ref().is_some_and(|t| t.dirty) {
            task = task.chain(table_actions::submit_table_save(self, table_view::SaveReason::Flush));
        }
        task
    }

    /// 任一保存回执后的放行判定：全部干净且有待续动作 = 执行。
    pub(crate) fn after_any_flush(&mut self) -> Task<Message> {
        if self.flush_pending.is_some() && !self.any_dirty() {
            let pending = self.flush_pending.take().expect("flush_pending 已判存在");
            return self.proceed(pending);
        }
        Task::none()
    }

    /// flush 放行后的动作执行体（gate 保证挂起输入已落盘）。
    fn proceed(&mut self, pending: PendingAfterFlush) -> Task<Message> {
        match pending {
            PendingAfterFlush::OpenFile(rel) => self.open_file(&rel),
            PendingAfterFlush::PickVault => self.start_pick_vault(),
            PendingAfterFlush::SelectVault { path, from_boot } => {
                // 已被更晚的点选取代：丢弃挂起的这次切换（避免落到非最后点击的仓库）
                if self.vault_switch_target.as_ref() != Some(&path) {
                    return Task::none();
                }
                self.enter_vault(path, from_boot)
            }
            PendingAfterFlush::CloseMain(id) => {
                // 退出前取消在途下载（`.part` 保留为完整前缀，下次启动续传）
                if let Some(flag) = &self.update_cancel {
                    flag.store(true, Relaxed);
                }
                // 会话真源的最后一次落盘：写链防抖未到的变更不落盘即随进程丢失
                chat_actions::flush_sessions().chain(window::close(id))
            }
            PendingAfterFlush::CloseFileTab(kind) => self.close_file_tab(&kind),
            PendingAfterFlush::OpenLink(href) => self.open_link_inner(href),
            PendingAfterFlush::Delete { rel, is_dir } => {
                self.dismiss_popup();
                self.delete_confirm = None;
                self.delete_entry(&rel, is_dir)
            }
            PendingAfterFlush::RenameFile(r) => self.commit_file_rename(r),
            PendingAfterFlush::MoveEntry { from, is_dir, to_dir } => self.move_entry(&from, is_dir, &to_dir),
            PendingAfterFlush::SelectSpace { server_url, space_id, name } => {
                // 脏拦截已过：直接进入（不得再经 gate，否则空脏判定重复放行成环）
                space_actions::handle(
                    self,
                    SpaceMsg::SelectSpace { server_url, space_id, name, from_boot: false },
                )
            }
            PendingAfterFlush::InstallUpdate => self.install_ready_update(),
        }
    }

    /// 提交一次笔记写盘：仓库根与内容在发起时冻结，写盘经 core 串行队列按提交序执行。
    /// 空间模式分派服务端（回读合流 + PUT + 服务端历史）。
    fn submit_save(&mut self, reason: SaveReason) -> Task<Message> {
        if matches!(self.repo_kind, RepoKind::Space(_)) {
            return space_actions::space_submit_save(self, reason);
        }
        let Some(rel) = self.ui.last_note_file.clone() else {
            self.notify(NoticeLevel::Warning, "没有打开的文件");
            return Task::none();
        };
        let Ok(root) = self.vault.root() else {
            self.notify(NoticeLevel::Warning, "仓库未打开");
            return Task::none();
        };
        let gen = self.epoch();
        let content = self.note_text.clone();
        let saved = content.clone();
        let queue = self.vault.writes.clone();
        Task::perform(
            async move {
                let path = vault::safe_join(&root, &rel, false)?;
                queue.write(path, content).await
            },
            move |result| Message::Saved(gen, saved, reason, result),
        )
    }

    /// 打开文件（脏拦截已由 gate 处理）：按扩展名分流（.atb → 表格、插件注册的领域扩展名 → 插件文档面），
    /// 复位只读并清掉上一文件的解析产物。
    fn open_file(&mut self, rel: &str) -> Task<Message> {
        if rel.ends_with(".atb") {
            return table_actions::open_table(self, rel);
        }
        if let Some(extension) = rel.rsplit('.').next() {
            if self
                .plugin_doc_types
                .iter()
                .any(|info| info.registration.extension.eq_ignore_ascii_case(extension))
            {
                return plugin_actions::open_doc(self, rel);
            }
        }
        self.history_kind = atelyx_core::history::HistoryKind::Note;
        self.ui.last_note_file = Some(rel.to_string());
        // 打开文件复位只读：笔记视图按文件重新挂载，档位不跨文件延续
        self.note_mode = NoteMode::Read;
        // 清掉上一文件的解析产物：载入在途期间两档都不留旧内容
        self.preview_doc = None;
        self.edit_doc = None;
        self.edit_meta.clear();
        self.schedule_persist();
        self.load_note(rel)
    }

    /// 系统文件夹选择器（切仓库入口，脏拦截已由 gate 处理）。
    fn start_pick_vault(&mut self) -> Task<Message> {
        Task::perform(
            async {
                match rfd::AsyncFileDialog::new().pick_folder().await {
                    Some(handle) => Ok(handle.path().to_path_buf()),
                    None => Err("未选择文件夹".into()),
                }
            },
            Message::VaultPicked,
        )
    }

    /// 进入仓库：成功则清空旧仓库会话态、登记最近列表并落盘 global.json；失败提示并保持当前态。
    /// 来自协作空间时先摘掉空间身份（会话态清空与展开集记忆由 leave_space_identity 承担）。
    /// 启动自动进入（from_boot）按「自动恢复」设置载入上次打开的文件；
    /// 运行中切换在 gate 保存后关闭当前打开的文件，进入新仓库为未打开态。
    fn enter_vault(&mut self, path: PathBuf, from_boot: bool) -> Task<Message> {
        if matches!(self.repo_kind, RepoKind::Space(_)) {
            space_actions::leave_space_identity(self);
        }
        // 展开集按仓库独立：切走时记忆当前仓库的集合，切回时恢复
        if let Some(old) = self.vault_root.clone() {
            self.expanded_by_vault.insert(old, self.ui.file_explorer_expanded.clone());
        }
        // 排除文件夹随仓库配置快照进会话（树过滤读取会话副本；改动需重开仓库生效）
        let exclude = vault::read_vault_config(&path)
            .map(|c| c.exclude_folders.unwrap_or_default())
            .unwrap_or_default();
        if let Err(e) = self.vault.set(path.clone(), exclude) {
            self.vault_switch_target = None;
            self.notify(NoticeLevel::Error, e);
            return Task::none();
        }
        let root = self.vault.root().ok();
        self.vault_root = root.clone();
        // 登记最近仓库：root 用归一化后的仓库根，与列表项、切仓比对同源
        if let Some(root) = &root {
            let name = vault::vault_display_name(root);
            self.global.bump_vault(&root.to_string_lossy(), &name, now_secs());
        }
        self.vault_switch_target = None;
        self.notify(NoticeLevel::Success, "仓库已打开");
        // 运行中切换：当前打开的笔记与表格一并关闭（保存已由 gate 完成），
        // 「上次打开」记录随之清除，进入新仓库为未打开态
        if !from_boot {
            self.ui.last_note_file = None;
            self.ui.last_table_file = None;
            // 打开中的插件文档实例随切仓库关闭（旧容器路径失效，实例面撤下；投影任务丢弃，后续刷新兜底）
            let _ = plugin_actions::close_doc(self);
            self.note_mode = NoteMode::Read;
            self.note_edit_caret_pending = false;
            self.note_press_inside = false;
            self.history.clear();
        }
        self.children.clear();
        self.note_text.clear();
        self.source_editor = text_editor::Content::new();
        self.edit.reset(0);
        self.edit_doc = None;
        self.edit_meta.clear();
        self.preview_doc = None;
        self.dirty = false;
        self.save_error = None;
        self.reset_props();
        self.file_count = None;
        self.expand_all = false;
        self.expand_pending = 0;
        self.rename_notice = None;
        // 表格会话随仓库切换清空（载入在途不留旧数据）
        self.table = None;
        // 仓库级配置会话态随切仓清空（VaultMetaLoaded 落地后回填）
        self.exclude_folders.clear();
        self.attachment_folder = None;
        self.entry_scene_id = None;
        self.exclude_folders_draft = None;
        self.attachment_folder_draft = None;
        // AI 配置基线与编辑态一并清空：残留会把上一仓库的供应商清单写进新仓库
        self.repo_config = vault::VaultConfig::default();
        self.agents.clear();
        self.ai = settings_view::ai::AiState::default();
        // 历史弹层交互态与重建链接会话态随切仓清空
        self.history_loading = false;
        self.history_expanded = None;
        self.history_confirm = None;
        // 仓库历史面板缓存随切仓失效（进面板后重取）
        self.repo_history = None;
        self.repo_history_loading = false;
        // 日历会话态随切仓重置（日程 / 选中天 / 活动密度一并重取）
        self.calendar = calendar_actions::CalendarState::default();
        self.rebuild_links = settings_view::editor::RebuildLinksState::Idle;
        // 展开集按仓库独立：切走时记忆当前仓库的集合，切回时恢复；全新进入（无记忆）保底展开根行
        self.ui.file_explorer_expanded = match self.expanded_by_vault.get(&path) {
            Some(set) => set.clone(),
            None => vec![String::new()],
        };
        // 容器已换：未声明保活的插件重载、声明的收到 vault:switch（旧仓库上下文此刻起失效）
        if let Some(root) = &root {
            self.plugins.on_vault_switch(
                atelyx_core::plugin_runtime::VaultSwitchPayload::Local {
                    root: root.to_string_lossy().into_owned(),
                },
            );
        }
        self.schedule_persist();
        let mut task = Task::none();
        // 展开集内的目录逐层补齐子项缓存（children 按目录缓存，进仓库后为空；根也在集合内）
        for dir in self.ui.file_explorer_expanded.clone() {
            task = task.chain(self.load_children(&dir));
        }
        task = task.chain(self.count_files());
        task = task.chain(self.refresh_tag_vocab()).chain(self.refresh_note_files());
        task = task.chain(self.load_vault_meta());
        task = task.chain(self.save_global());
        // 启动自动进入且「自动恢复」开启：恢复上次打开的文件；运行中切换不恢复（文件已随切仓关闭）
        if from_boot && self.global.auto_restore {
            if let Some(rel) = self.ui.last_note_file.clone() {
                task = task.chain(self.load_note(&rel));
            }
            if let Some(rel) = self.ui.last_table_file.clone() {
                task = task.chain(table_actions::load_table(self, &rel));
            }
        }
        task
    }

    /// 落盘 global.json（小文件整写；失败提示，不回滚内存态）。
    fn save_global(&self) -> Task<Message> {
        let Some(path) = global_config_path() else {
            return Task::none();
        };
        let config = self.global.clone();
        Task::perform(
            async move { global_config::write_global_config(&path, &config) },
            Message::GlobalSaved,
        )
    }

    /// 同步落盘 global.json（设置项单值写：串行于 UI 线程，连续输入不会产生写回序错乱）。
    pub(crate) fn save_global_now(&mut self) {
        let Some(path) = global_config_path() else { return };
        if let Err(e) = global_config::write_global_config(&path, &self.global) {
            self.notify(NoticeLevel::Error, format!("保存全局配置失败：{e}"));
        }
    }

    /// 仓库级配置补丁落盘（本地 = patch_vault_config；空间 = 服务端 config.json 读改写）。
    fn patch_repo_config(&mut self, patch: serde_json::Value) -> Task<Message> {
        if matches!(self.repo_kind, RepoKind::Space(_)) {
            return space_actions::space_patch_config(self, patch);
        }
        let Some(root) = self.vault.root().ok() else { return Task::none() };
        let gen = self.epoch();
        Task::perform(
            async move { vault::patch_vault_config(&root, &patch).map(|_| ()) },
            move |result| Message::ConfigSaved(gen, result),
        )
    }

    /// 关闭打开的文件（×：摘下对应视图的文件、标签保留回未打开态；脏拦截已由 gate 处理）。
    fn close_file_tab(&mut self, kind: &str) -> Task<Message> {
        self.dismiss_popup();
        // 插件文档视图：关闭当前文档实例（标签保留回未打开态）
        if crate::plugin_actions::parse_view_kind(kind).is_some() {
            let is_doc_view = self
                .doc_open
                .as_ref()
                .is_some_and(|d| plugin_actions::doc_view_kind(&d.plugin_id, &d.contribution_id) == kind);
            return if is_doc_view {
                plugin_actions::close_doc(self)
            } else {
                Task::none()
            };
        }
        match kind {
            "table" => {
                self.ui.last_table_file = None;
                self.table = None;
                // 与笔记关闭同规：历史面板不再有对应文件可查，清空防列表冻结在旧数据
                self.history.clear();
                // 空间模式回收在场（名册不再滞留「正在编辑该表格」）
                if matches!(self.repo_kind, RepoKind::Space(_)) {
                    space_actions::send_presence(self);
                }
            }
            _ => {
                // 空间模式先摘协作绑定（离场帧 + presence 回收）再清编辑态
                if matches!(self.repo_kind, RepoKind::Space(_)) {
                    space_actions::collab_unbind_note(self);
                }
                self.ui.last_note_file = None;
                self.note_text.clear();
                self.source_editor = text_editor::Content::new();
                self.edit.reset(0);
                self.edit_doc = None;
                self.edit_meta.clear();
                self.preview_doc = None;
                self.dirty = false;
                self.save_error = None;
                self.reset_props();
                self.history.clear();
            }
        }
        self.schedule_persist();
        Task::none()
    }

    fn active_tree(&self) -> Option<layout::LayoutNode> {
        Some(layout::active_layout(&self.ui).tree)
    }

    // ===== 反链 / 标签词汇表 / 双链候选 =====

    /// 反链扫描：指纹增量刷新 wiki 索引后查询当前笔记（spawn_blocking，回执 gen 守卫）。
    fn refresh_backlinks(&self) -> Task<Message> {
        if matches!(self.repo_kind, RepoKind::Space(_)) {
            return space_actions::refresh_space_backlinks(self);
        }
        let Some(rel) = self.ui.last_note_file.clone() else { return Task::none() };
        let gen = self.epoch();
        let state = self.vault.clone();
        let title = rel.rsplit('/').next().unwrap_or(&rel).trim_end_matches(".md").to_string();
        let rel_for_cb = rel.clone();
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    let (root, exclude, _) = state.session_snapshot()?;
                    let mut guard = state.wiki.lock().map_err(|e| e.to_string())?;
                    let index = guard.get_or_insert_with(Default::default);
                    vault::refresh_wiki_index(&root, &exclude, index)?;
                    Ok(vault::query_wiki_backlinks(index, &title, &rel))
                })
                .await
                .map_err(|e| e.to_string())?
            },
            move |result| Message::BacklinksLoaded(gen, rel_for_cb, result),
        )
    }

    /// 标签词汇表扫描（spawn_blocking；候选按 count 降序）。
    fn refresh_tag_vocab(&self) -> Task<Message> {
        if matches!(self.repo_kind, RepoKind::Space(_)) {
            return space_actions::refresh_space_tags(self);
        }
        let gen = self.epoch();
        let state = self.vault.clone();
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    let (root, exclude, _) = state.session_snapshot()?;
                    let mut guard = state.tags.lock().map_err(|e| e.to_string())?;
                    let index = guard.get_or_insert_with(Default::default);
                    vault::refresh_tag_index(&root, &exclude, index)?;
                    Ok(vault::aggregate_tag_counts(index))
                })
                .await
                .map_err(|e| e.to_string())?
            },
            move |result| Message::TagVocabLoaded(gen, result),
        )
    }

    /// 全仓库笔记清单（双链候选数据源；树序）。空间模式由整树回执填充，此处直通。
    fn refresh_note_files(&self) -> Task<Message> {
        if matches!(self.repo_kind, RepoKind::Space(_)) {
            return Task::none();
        }
        let gen = self.epoch();
        let state = self.vault.clone();
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    let (root, exclude, _) = state.session_snapshot()?;
                    let tree = vault::list_vault_tree(&root, &exclude)?;
                    let mut files = Vec::new();
                    collect_md_files(&tree, &mut files);
                    Ok(files)
                })
                .await
                .map_err(|e| e.to_string())?
            },
            move |result| Message::NoteFilesLoaded(gen, result),
        )
    }

    /// 双链候选的过滤序号（.md 笔记按文件名子串过滤、树序，上限 50）。
    fn wiki_candidates(&self) -> Vec<usize> {
        let Some(p) = &self.wiki_picker else { return Vec::new() };
        let q = p.query.trim().to_lowercase();
        self.note_files
            .iter()
            .enumerate()
            .filter(|(_, (rel, name))| {
                rel.ends_with(".md") && (q.is_empty() || name.to_lowercase().contains(&q))
            })
            .map(|(i, _)| i)
            .take(50)
            .collect()
    }

    /// 输入 / 删除后的双链候选状态推进：光标处于未闭合 `[[` / `【【` 片段时开启或更新。
    fn sync_wiki_picker(&mut self) {
        // 先算清决策（片段位置与查询词），再落到状态（避免借用交叠）
        enum Sync {
            Close,
            Open(usize, String),
        }
        let decision = {
                let body = self.edit_body();
                let caret = self.edit.caret.min(body.len());
                let ascii = body[..caret].rfind("[[").map(|i| (i, 2usize));
                let wide = body[..caret].rfind("【【").map(|i| (i, "【【".len()));
                let found = match (ascii, wide) {
                    (Some(a), Some(b)) if a.0 >= b.0 => Some(a),
                    (Some(_), Some(b)) => Some(b),
                    (a, b) => a.or(b),
                };
                match found {
                    None => Sync::Close,
                    Some((from, tlen)) => {
                        let q_start = from + tlen;
                        if caret < q_start {
                            Sync::Close
                        } else {
                            let query = &body[q_start..caret];
                            if query.contains(['[', ']', '【', '】', '\n']) {
                                Sync::Close
                            } else {
                                Sync::Open(from, query.to_string())
                            }
                        }
                    }
                }
            };
        match decision {
            Sync::Close => self.close_wiki_picker(false),
            Sync::Open(from, query) => match &mut self.wiki_picker {
                Some(p) if p.from == from => {
                    if p.query != query {
                        p.query = query;
                        p.active = 0;
                    }
                }
                _ => {
                    if self.wiki_dismissed_from != Some(from) {
                        self.wiki_dismissed_from = None;
                        self.wiki_picker = Some(WikiPicker { from, query, active: 0 });
                    }
                }
            },
        }
    }

    /// 关闭双链候选；`dismiss` = Esc 主动关闭（同触发位置不再自动重开）。
    fn close_wiki_picker(&mut self, dismiss: bool) {
        if let Some(p) = self.wiki_picker.take() {
            if dismiss {
                self.wiki_dismissed_from = Some(p.from);
            }
        }
    }

    /// 光标移出候选片段时收起（光标移入编辑面的导航路径调用）。
    fn close_wiki_picker_if_outside(&mut self) {
        if let Some(p) = &self.wiki_picker {
            let caret = self.edit.caret.min(self.edit_body().len());
            if caret < p.from {
                self.close_wiki_picker(false);
            }
        }
    }

    /// 选中双链候选：片段替换为 `[标题](路径)` markdown 链接。
    fn wiki_pick(&mut self, candidate_index: usize) {
        let Some(picker) = self.wiki_picker.clone() else { return };
        let candidates = self.wiki_candidates();
        let Some(file_index) = candidates.get(candidate_index).copied() else { return };
        let Some((file, name)) = self.note_files.get(file_index).cloned() else { return };
        let body = self.edit_body().to_string();
        let caret = self.edit.caret.min(body.len());
        if caret < picker.from {
            self.wiki_picker = None;
            return;
        }
        let label = name.trim_end_matches(".md").replace(['[', ']'], "");
        let insert = format!("[{}]({})", label, encode_md_href(&file));
        let mut next = String::with_capacity(body.len() - (caret - picker.from) + insert.len());
        next.push_str(&body[..picker.from]);
        next.push_str(&insert);
        next.push_str(&body[caret..]);
        self.edit.snapshot(&self.note_text);
        self.set_edit_body(next);
        self.edit.caret = picker.from + insert.len();
        self.edit.anchor = self.edit.caret;
        self.wiki_picker = None;
        self.wiki_dismissed_from = None;
        // 全局 Enter 订阅与编辑面 Newline 事件对同一次按键双发：本路径先落则吞掉随后的 Newline
        self.wiki_swallow_newline = true;
        self.after_edit();
    }

    // ===== 历史侧车 =====

    /// 读取当前上下文（笔记/表格）的历史版本（尽力而为，读失败 = 空列表）。
    fn reload_history(&self) -> Task<Message> {
        let (kind, rel) = match self.history_kind {
            atelyx_core::history::HistoryKind::Note => {
                (atelyx_core::history::HistoryKind::Note, self.ui.last_note_file.clone())
            }
            atelyx_core::history::HistoryKind::Table => {
                (atelyx_core::history::HistoryKind::Table, self.ui.last_table_file.clone())
            }
            // 画布视图未接线：无打开上下文可读（弹层按显式 kind+file 经 load_history_for 读取）
            atelyx_core::history::HistoryKind::Canvas => (atelyx_core::history::HistoryKind::Canvas, None),
        };
        match rel {
            Some(rel) => self.load_history_for(kind, &rel),
            None => Task::none(),
        }
    }

    /// 按 kind+file 读取历史版本（本地 spawn_blocking / 空间模式走服务端端点）；
    /// 回执经 HistoryLoaded 落地（gen + kind/文件身份守卫）。
    pub(crate) fn load_history_for(&self, kind: atelyx_core::history::HistoryKind, rel: &str) -> Task<Message> {
        if matches!(self.repo_kind, RepoKind::Space(_)) {
            return space_actions::space_history_task(self, kind, rel);
        }
        let gen = self.epoch();
        let state = self.vault.clone();
        let rel = rel.to_string();
        let rel_for_cb = rel.clone();
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    let (root, _, _) = state.session_snapshot()?;
                    Ok::<_, String>(atelyx_core::history::load_history(&root, kind, &rel))
                })
                .await
                .map_err(|e| e.to_string())
                .and_then(|r| r)
                .unwrap_or_default()
            },
            move |versions| Message::HistoryLoaded(gen, kind, rel_for_cb, versions),
        )
    }

    /// 记录一次保存为历史版本（edit；60s 窗口内同作者连续编辑合并为一版）。
    /// 历史为尽力而为侧车：写失败静默丢弃（不打扰正文保存链），与读取侧空列表降级同口径。
    fn record_note_history(&self, content: String) -> Task<Message> {
        let Some(rel) = self.ui.last_note_file.clone() else { return Task::none() };
        let gen = self.epoch();
        let state = self.vault.clone();
        let author = history_author();
        Task::perform(
            async move {
                let _ = tokio::task::spawn_blocking(move || {
                    let (root, _, _) = state.session_snapshot()?;
                    atelyx_core::history::record_version(
                        &root,
                        atelyx_core::history::HistoryKind::Note,
                        &rel,
                        &atelyx_core::history::RecordOpts {
                            content,
                            action: atelyx_core::history::HistoryAction::Edit,
                            author,
                            coalesce_edit_ms: Some(60_000),
                            max_versions: 0,
                            note: None,
                        },
                    )
                })
                .await;
                Message::HistoryRecorded(gen, None)
            },
            |msg| msg,
        )
    }

    /// 回滚到指定版本：全文快照写回走各保存链（画布走 canvas_history_restore 快照直写）。
    pub(crate) fn history_restore(
        &mut self,
        kind: atelyx_core::history::HistoryKind,
        version: atelyx_core::history::HistoryVersion,
    ) -> Task<Message> {
        match kind {
            atelyx_core::history::HistoryKind::Table => {
                // 表格：快照 = TableFile JSON；解析失败静默放弃（历史尽力而为）
                let Ok(table) = serde_json::from_str::<atelyx_core::vault::TableFile>(&version.content)
                else {
                    self.notify(NoticeLevel::Error, "历史版本无法解析");
                    return Task::none();
                };
                if table_actions::apply_history_snapshot(self, table) {
                    self.autosave_deadline = Some(Instant::now() + AUTOSAVE_DEBOUNCE);
                }
                let Some(rel) = self.ui.last_table_file.clone() else { return Task::none() };
                if matches!(self.repo_kind, RepoKind::Space(_)) {
                    return space_actions::space_record_table_restore(self, rel, version.content);
                }
                let gen = self.epoch();
                let state = self.vault.clone();
                let author = history_author();
                let content = version.content.clone();
                Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            let (root, _, _) = state.session_snapshot()?;
                            atelyx_core::history::record_version(
                                &root,
                                atelyx_core::history::HistoryKind::Table,
                                &rel,
                                &atelyx_core::history::RecordOpts {
                                    content,
                                    action: atelyx_core::history::HistoryAction::Restore,
                                    author,
                                    coalesce_edit_ms: None,
                                    max_versions: 0,
                                    note: None,
                                },
                            )
                        })
                        .await
                        .map_err(|e| e.to_string())
                        .and_then(|r| r)
                    },
                    move |r: Result<(), String>| Message::HistoryRecorded(gen, r.err()),
                )
            }
            atelyx_core::history::HistoryKind::Note => self.note_history_restore(version),
            // 画布回滚走 canvas_history_restore（快照直写仓库，无会话链）
            atelyx_core::history::HistoryKind::Canvas => Task::none(),
        }
    }

    /// 画布回滚：快照校验后写回仓库真源（本地经写盘队列批量作业，空间模式先写服务端再记历史）。
    pub(crate) fn canvas_history_restore(
        &mut self,
        rel: String,
        version: atelyx_core::history::HistoryVersion,
    ) -> Task<Message> {
        if let Err(e) = atelyx_core::history::validate_snapshot(
            atelyx_core::history::HistoryKind::Canvas,
            &version.content,
        ) {
            self.notify(NoticeLevel::Error, e);
            return Task::none();
        }
        if matches!(self.repo_kind, RepoKind::Space(_)) {
            return space_actions::space_restore_canvas(self, rel, version.content);
        }
        let gen = self.epoch();
        // 根在入队前快照：作业排队期间切仓不改变执行目标（闭包内不重读会话）
        let root = self.vault.root();
        let queue = self.vault.writes.clone();
        let content = version.content.clone();
        Task::perform(
            async move {
                let rel_for_cb = rel.clone();
                let r = queue.bulk(move || {
                    atelyx_core::history::restore_snapshot(
                        &root?,
                        atelyx_core::history::HistoryKind::Canvas,
                        &rel,
                        &content,
                    )
                })
                .await;
                Message::CanvasHistoryRestored(gen, rel_for_cb, r)
            },
            |m| m,
        )
    }

    /// 画布回滚落盘后的 restore 版本记录（尽力而为）。
    fn record_canvas_restore_history(&self, rel: String) -> Task<Message> {
        // 快照即回滚写入的原文：从磁盘事实取内容（队列写已完成，读到的是回滚后状态）
        let gen = self.epoch();
        let state = self.vault.clone();
        let author = history_author();
        Task::perform(
            async move {
                let r = tokio::task::spawn_blocking(move || {
                    let (root, _, _) = state.session_snapshot()?;
                    let path = atelyx_core::vault::safe_join(&root, &rel, false)?;
                    let content = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
                    atelyx_core::history::record_version(
                        &root,
                        atelyx_core::history::HistoryKind::Canvas,
                        &rel,
                        &atelyx_core::history::RecordOpts {
                            content,
                            action: atelyx_core::history::HistoryAction::Restore,
                            author,
                            coalesce_edit_ms: None,
                            max_versions: 0,
                            note: None,
                        },
                    )
                })
                .await
                .map_err(|e| e.to_string())
                .and_then(|r| r);
                Message::HistoryRecorded(gen, r.err())
            },
            |m| m,
        )
    }

    /// 笔记回滚：全文快照替换正文（撤销可回退），保存链写回并记 restore 版本。
    fn note_history_restore(&mut self, version: atelyx_core::history::HistoryVersion) -> Task<Message> {
        self.edit.snapshot(&self.note_text);
        self.note_text = version.content.clone();
        // 源码档正文真源 = note_text 派生的编辑器内容：程序化替换须同步重建
        if self.note_mode == NoteMode::Source {
            self.source_editor = text_editor::Content::with_text(&self.note_text);
        }
        self.dirty = true;
        self.save_error = None;
        self.autosave_deadline = Some(Instant::now() + AUTOSAVE_DEBOUNCE);
        self.refresh_preview();
        self.refresh_edit();
        self.refresh_props();
        let Some(rel) = self.ui.last_note_file.clone() else { return Task::none() };
        if matches!(self.repo_kind, RepoKind::Space(_)) {
            return space_actions::space_record_note_restore(self, rel, version.content);
        }
        let gen = self.epoch();
        let state = self.vault.clone();
        let author = history_author();
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    let (root, _, _) = state.session_snapshot()?;
                    atelyx_core::history::record_version(
                        &root,
                        atelyx_core::history::HistoryKind::Note,
                        &rel,
                        &atelyx_core::history::RecordOpts {
                            content: version.content,
                            action: atelyx_core::history::HistoryAction::Restore,
                            author,
                            coalesce_edit_ms: None,
                            max_versions: 0,
                            note: None,
                        },
                    )
                })
                .await
                .map_err(|e| e.to_string())
                .and_then(|r| r)
            },
            move |r: Result<(), String>| Message::HistoryRecorded(gen, r.err()),
        )
    }

    /// 消费回滚落盘回执（一次性）：成功出「已回滚」，失败仅清标志（失败详情由保存链提示）。
    pub(crate) fn settle_history_restore(&mut self, ok: bool) {
        if !self.history_restore_pending {
            return;
        }
        self.history_restore_pending = false;
        if ok {
            self.notify(NoticeLevel::Success, "已回滚");
        }
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
            ui::UiMsg::InputEdit { edit, arm } => {
                self.arm_commit = Some(*arm);
                self.update(*edit)
            }
            ui::UiMsg::InputSubmit { commit } => {
                self.arm_commit = None;
                self.update(*commit)
            }
            ui::UiMsg::CommitPendingInput => match self.arm_commit.take() {
                Some(m) => {
                    self.arm_commit = None;
                    self.update(m)
                }
                None => Task::none(),
            },
            ui::UiMsg::PopupClose { window } => {
                if self.popup.as_ref().is_some_and(|(w, _)| *w == window) {
                    self.popup = None;
                    self.popup_pane = 0;
                    self.delete_confirm = None;
                    self.remove_vault_confirm = None;
                    self.table_popup_draft.clear();
                    // 弹层收起即取消快捷键录制（与 dismiss_popup 同口径）
                    self.shortcut_recording = None;
                }
                Task::none()
            }
            ui::UiMsg::NotifyDismiss(id) => {
                self.notifications.retain(|n| n.id != id);
                Task::none()
            }
        }
    }

    /// 更新流程消息处理（状态迁移语义在 update::UpdateState；此处只做任务发起与通知编排）。
    fn on_update(&mut self, msg: update::UpdateMsg) -> Task<Message> {
        use update::UpdateMsg;
        match msg {
            UpdateMsg::Check { manual } => {
                if !self.update.start_check() {
                    return Task::none();
                }
                self.update_manual_check = manual;
                Task::perform(atelyx_core::updater::latest_release(), |result| {
                    Message::Update(UpdateMsg::Checked(result))
                })
            }
            UpdateMsg::Checked(result) => {
                let manual = self.update_manual_check;
                match result {
                    Ok(release) => match self.update.finish_check(env!("CARGO_PKG_VERSION"), release) {
                        Ok(()) => {
                            if let update::UpdateState::Available { version, .. } = &self.update {
                                let version = version.clone();
                                self.set_update_notice(
                                    NoticeLevel::Info,
                                    format!("发现新版本 {version}"),
                                    None,
                                    |id| {
                                        vec![
                                            ("下载".into(), Message::Update(UpdateMsg::Download)),
                                            ("跳过".into(), Message::Ui(ui::UiMsg::NotifyDismiss(id))),
                                        ]
                                    },
                                );
                            } else if manual {
                                self.notify(NoticeLevel::Info, "已是最新版本");
                            }
                        }
                        // 有新版本但本机平台无安装包：手动检查转开发布页，静默检查仅提示
                        Err(page) => {
                            if manual {
                                return self.open_link(&page, LinkForm::External);
                            }
                            self.notify(NoticeLevel::Info, "发现新版本，请到发布页下载");
                        }
                    },
                    Err(e) => {
                        self.update.fail_check();
                        if manual {
                            self.notify(NoticeLevel::Error, e);
                        }
                    }
                }
                Task::none()
            }
            UpdateMsg::Download => {
                // 地址与摘要只在 Available 态持有，先取出再迁移状态
                let Some((version, url, sha256)) = (match &self.update {
                    update::UpdateState::Available { version, download_url, sha256 } => {
                        Some((version.clone(), download_url.clone(), sha256.clone()))
                    }
                    _ => None,
                }) else {
                    return Task::none();
                };
                if self.update.start_download().is_none() {
                    return Task::none();
                }
                self.dismiss_update_notice();
                let cancel = update::cancel_flag();
                self.update_cancel = Some(Arc::clone(&cancel));
                *self.update_progress.lock().expect("进度暂存锁失效") = (0, 0);
                self.set_update_notice(
                    NoticeLevel::Info,
                    format!("正在下载 {version}"),
                    Some(NoticeProgress { received: 0, total: 0 }),
                    |_| vec![("取消".into(), Message::Update(UpdateMsg::Cancel))],
                );
                let dir = self.update_download_dir.clone();
                let progress = Arc::clone(&self.update_progress);
                Task::perform(
                    async move {
                        // 进度回调在任务线程执行，只写暂存不碰 App；心跳派发的 ProgressTick 读走
                        atelyx_core::updater::download(
                            &dir,
                            &url,
                            sha256.as_deref(),
                            cancel,
                            Arc::new(move |received, total| {
                                *progress.lock().expect("进度暂存锁失效") = (received, total);
                            }),
                        )
                        .await
                    },
                    |result| Message::Update(UpdateMsg::Downloaded(result)),
                )
            }
            UpdateMsg::ProgressTick => {
                if !matches!(self.update, update::UpdateState::Downloading { .. }) {
                    return Task::none();
                }
                let (received, total) = *self.update_progress.lock().expect("进度暂存锁失效");
                self.update.set_progress(received, total);
                let progress = Some(NoticeProgress { received, total });
                for n in &mut self.notifications {
                    if n.persistent {
                        n.progress = progress;
                    }
                }
                Task::none()
            }
            UpdateMsg::Downloaded(result) => {
                // 取消后迟到的回执：状态已离开 Downloading，静默丢弃
                if !matches!(self.update, update::UpdateState::Downloading { .. }) {
                    self.update_cancel = None;
                    return Task::none();
                }
                self.update.finish_download(result.clone());
                self.update_cancel = None;
                self.dismiss_update_notice();
                match result {
                    Ok(_) => self.set_update_notice(
                        NoticeLevel::Success,
                        "新版本已就绪".into(),
                        None,
                        |id| {
                            vec![
                                ("立即安装".into(), Message::Update(UpdateMsg::Install)),
                                ("稍后".into(), Message::Ui(ui::UiMsg::NotifyDismiss(id))),
                            ]
                        },
                    ),
                    Err(e) => self.notify(NoticeLevel::Error, format!("下载失败：{e}")),
                }
                Task::none()
            }
            UpdateMsg::Cancel => {
                let was_downloading = matches!(self.update, update::UpdateState::Downloading { .. });
                if let Some(flag) = &self.update_cancel {
                    flag.store(true, Relaxed);
                }
                self.update_cancel = None;
                self.update.cancel();
                if was_downloading {
                    self.dismiss_update_notice();
                }
                Task::none()
            }
            UpdateMsg::Install => self.gate(PendingAfterFlush::InstallUpdate),
        }
    }

    /// 安装就绪的更新包（flush 放行后执行）：拉起安装器成功即退出进程让出 exe 占用；
    /// 失败提示且应用继续运行。
    fn install_ready_update(&mut self) -> Task<Message> {
        let update::UpdateState::Ready { path, .. } = &self.update else {
            return Task::none();
        };
        match update::launch_installer(path) {
            Ok(()) => iced::exit(),
            Err(e) => {
                self.notify(NoticeLevel::Error, e);
                Task::none()
            }
        }
    }

    /// 文件操作成功后对「打开中的笔记」的跟随：改名换前缀、删除清空；目录改名/移动同步展开集。
    fn apply_follow(&mut self, follow: &FollowUp) {
        match follow {
            FollowUp::None => {}
            FollowUp::Renamed { from, to, .. } => {
                // 展开集路径跟随（目录改名/移动；文件路径不会成为展开键，前缀判空转无害）
                let prefix = format!("{from}/");
                for d in &mut self.ui.file_explorer_expanded {
                    if d == from {
                        *d = to.clone();
                    } else if let Some(rest) = d.strip_prefix(&prefix) {
                        *d = format!("{to}/{rest}");
                    }
                }
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
                if let Some(f) = &self.ui.last_table_file {
                    let next = if f == from {
                        Some(to.clone())
                    } else if f.starts_with(&format!("{from}/")) {
                        Some(f.replacen(from, to, 1))
                    } else {
                        None
                    };
                    if let Some(n) = next {
                        if let Some(s) = &mut self.table {
                            if s.file == *f || s.file.starts_with(&format!("{from}/")) {
                                s.file = n.clone();
                                // 表格内标题与文件名保持一致（新表以文件名起稿，改名随之同步）
                                let stem = n
                                    .rsplit('/')
                                    .next()
                                    .unwrap_or(n.as_str())
                                    .strip_suffix(".atb")
                                    .unwrap_or(n.as_str());
                                if s.data.title != stem {
                                    s.data.title = stem.to_string();
                                    s.touch();
                                    self.autosave_deadline = Some(Instant::now() + AUTOSAVE_DEBOUNCE);
                                }
                            }
                        }
                        self.ui.last_table_file = Some(n);
                        self.schedule_persist();
                    }
                }
                space_actions::collab_follow_rename(self, from, to);
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
                    // 空间模式先摘协作绑定（离场帧 + presence 回收）再清编辑态
                    if matches!(self.repo_kind, RepoKind::Space(_)) {
                        space_actions::collab_unbind_note(self);
                    }
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
                let hit_table = self.ui.last_table_file.as_deref().is_some_and(|f| {
                    if *is_dir {
                        f == rel || f.starts_with(&format!("{rel}/"))
                    } else {
                        f == rel
                    }
                });
                if hit_table {
                    self.ui.last_table_file = None;
                    self.table = None;
                    if matches!(self.repo_kind, RepoKind::Space(_)) {
                        space_actions::send_presence(self);
                    }
                    self.schedule_persist();
                }
                // 打开中的插件文档被删 / 所在目录被删：实例随磁盘事实关闭
                let hit_doc = self.doc_open.as_ref().is_some_and(|d| {
                    if *is_dir {
                        d.path == *rel || d.path.starts_with(&format!("{rel}/"))
                    } else {
                        d.path == *rel
                    }
                });
                if hit_doc {
                    let _ = plugin_actions::close_doc(self);
                }
            }
        }
    }

    /// 仓库文件总数（排除隐藏/排除夹的树扫描；打开仓库后一次）。
    /// 空间模式由整树回执填充（count_tree_files），此处直通。
    fn count_files(&self) -> Task<Message> {
        if matches!(self.repo_kind, RepoKind::Space(_)) {
            return Task::none();
        }
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
    /// 文件操作落地（gen 守卫 + 结果通知 + 刷新子项 + 打开文件跟随）。
    /// 空间模式表单草稿提交分派 space_actions（服务端建条目）。
    fn commit_file_rename(&mut self, r: RenameState) -> Task<Message> {
        if matches!(self.repo_kind, RepoKind::Space(_)) {
            if let RenameTarget::Draft { dir, kind } = &r.target {
                let name = vault::sanitize_filename(r.value.trim());
                if name.is_empty() {
                    return Task::none();
                }
                return space_actions::space_create_entry(self, dir, kind.clone(), &name);
            }
            return space_actions::space_commit_rename(self, r);
        }
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
                                let base = match &kind {
                                    DraftKind::Note => format!("{name}.md"),
                                    DraftKind::Folder => name.clone(),
                                    DraftKind::Table => format!("{name}.atb"),
                                    DraftKind::Doc { extension, .. } => format!("{name}.{extension}"),
                                };
                                let mut candidate = base.clone();
                                let mut seq = 2;
                                loop {
                                    let path = vault::safe_join(&root, &full_rel(&dir, &candidate), false)?;
                                    if !path.exists() {
                                        break;
                                    }
                                    candidate = match &kind {
                                        DraftKind::Note => format!("{name}-{seq}.md"),
                                        DraftKind::Folder => format!("{name}-{seq}"),
                                        DraftKind::Table => format!("{name}-{seq}.atb"),
                                        DraftKind::Doc { extension, .. } => format!("{name}-{seq}.{extension}"),
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
                                    DraftKind::Table => {
                                        // 初始 = 一个「名称」文本字段（空白表无字段难用）
                                        let table = atelyx_core::table_ops::new_table_file(&name);
                                        let path = vault::safe_join(&root, &rel, true)?;
                                        vault::write_table_file(&path, &table).map(|_| rel)
                                    }
                                    // 领域文档初始为空文件：骨架内容随插件打开时自定（协作空间同构，通用文件对待）
                                    DraftKind::Doc { .. } => {
                                        let path = vault::safe_join(&root, &rel, true)?;
                                        vault::atomic_write(&path, "").map(|_| rel)
                                    }
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
                let Ok(exclude) = self.vault.exclude_folders() else { return Task::none() };
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
                        // 请求名与原名落点全等 = 纯 no-op（case-only 改名字符串不等，
                        // 走下方事务由 same_physical_file 豁免真正 rename 生效）
                        let full = full_rel(&parent_for_task, &target);
                        if full == old_for_task {
                            return Ok((old_for_task.clone(), Vec::new()));
                        }
                        let result = vault_refs::rename_note_synced(&root, &exclude, &old_for_task, &full)?;
                        Ok((full, result.rewritten))
                    },
                    move |result| match result {
                        Ok((full, rewritten)) => Message::OpDone(
                            gen,
                            parent,
                            Ok(full.clone()),
                            FollowUp::Renamed { from: old, to: full, rewritten },
                        ),
                        Err(e) => Message::OpDone(gen, parent, Err(e), FollowUp::None),
                    },
                )
            }
            RenameTarget::Folder(old_dir) => {
                let name = vault::sanitize_filename(r.value.trim());
                if name.is_empty() {
                    return Task::none();
                }
                let parent = parent_of(&old_dir);
                let new_for_task = match old_dir.rfind('/') {
                    Some(i) => format!("{}/{name}", &old_dir[..i]),
                    None => name,
                };
                let old_for_task = old_dir.clone();
                let Ok(exclude) = self.vault.exclude_folders() else { return Task::none() };
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
                            return Ok((target, Vec::new()));
                        }
                        let result = vault_refs::rename_folder_synced(&root, &exclude, &old_for_task, &target)?;
                        Ok((target, result.rewritten))
                    },
                    move |result| match result {
                        Ok((full, rewritten)) => Message::OpDone(
                            gen,
                            parent,
                            Ok(full.clone()),
                            FollowUp::Renamed { from: old_dir, to: full, rewritten },
                        ),
                        Err(e) => Message::OpDone(gen, parent, Err(e), FollowUp::None),
                    },
                )
            }
            RenameTarget::Layout(_) | RenameTarget::Scene(_) => Task::none(),
        }
    }

    /// 删除文件/文件夹（不可恢复；菜单内已就地确认）。Ok 携带目标相对路径。
    /// 表格文件随删图片附件目录（按 tableId 划分；读不到 id 跳过，不拦截删除本身）。
    /// 空间模式：文件直删，文件夹直接落删（needsConfirm 的探测在菜单删除入口，不在此处）。
    fn delete_entry(&mut self, rel: &str, is_dir: bool) -> Task<Message> {
        if matches!(self.repo_kind, RepoKind::Space(_)) {
            // 本入口即「用户已确认」（菜单内二段确认已过）：文件夹直接落删，不再探测
            return space_actions::space_delete_entry(self, rel, is_dir, true);
        }
        let Some(root) = self.vault.root().ok() else { return Task::none() };
        let gen = self.epoch();
        let parent = parent_of(rel);
        let rel_for_task = rel.to_string();
        let rel_for_follow = rel.to_string();
        Task::perform(
            async move {
                // 删除前读取：附件目录按表 id 定位（文件夹递归收集其下 .atb），文件删掉后无从读取；
                // 收集在后台线程执行（大文件夹递归解析不阻塞 UI）
                let rel = rel_for_task.clone();
                let root_for_scan = root.clone();
                let attachment_dirs = tokio::task::spawn_blocking(move || vault::table_attachment_dirs(&root_for_scan, &rel, is_dir))
                    .await
                    .unwrap_or_default();
                let result = if is_dir {
                    vault::delete_folder(&root, &rel_for_task, true).map(|_| rel_for_task)
                } else {
                    vault::delete_vault_file(&root, &rel_for_task).map(|_| rel_for_task)
                };
                // 图片附件目录随表私有：删除后尽力清理（残留不拦截删除本身）
                if result.is_ok() {
                    vault::purge_table_attachments(attachment_dirs);
                }
                result
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
    /// 空间模式 = 整树切片（树未就绪先拉树）。
    fn load_children(&mut self, rel: &str) -> Task<Message> {
        if matches!(self.repo_kind, RepoKind::Space(_)) {
            return space_actions::space_load_children(self, rel);
        }
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
            async move { vault::read_dir_entries(&root, &rel, &exclude) },
            move |result| Message::ChildrenLoaded(rel_for_cb, gen, result),
        )
    }

    /// 递归展开用的目录读取（带世代守卫：在途任务不跨仓库落地）。
    fn load_children_for_expand(&mut self, rel: &str) -> Task<Message> {
        if matches!(self.repo_kind, RepoKind::Space(_)) {
            return space_actions::space_load_children_for_expand(self, rel);
        }
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
            async move { vault::read_dir_entries(&root, &rel, &exclude) },
            move |result| Message::ExpandChildrenLoaded(rel_for_cb, gen, result),
        )
    }

    /// 进入只读档：清编辑态解析产物并重建只读渲染。
    fn enter_read_mode(&mut self) {
        self.note_mode = NoteMode::Read;
        self.edit_doc = None;
        self.edit_meta.clear();
        self.edit.focused = false;
        self.note_edit_caret_pending = false;
        self.refresh_preview();
    }

    /// 进入预览编辑档：不揭示任何块、不持光标，首帧全篇按块渲染。
    fn enter_edit_mode(&mut self) {
        self.note_mode = NoteMode::Edit;
        self.preview_doc = None;
        self.edit.active = None;
        self.edit.preedit = None;
        self.edit.focused = false;
        self.refresh_edit();
    }

    /// 只读档的解析产物：进入只读档或文件载入时重建（渲染面不逐帧解析）。
    pub(crate) fn refresh_preview(&mut self) {
        if self.note_mode != NoteMode::Read {
            self.preview_doc = None;
            return;
        }
        let body = strip_frontmatter(&self.note_text).to_string();
        let doc = markdown_view::parse_document(&body);
        self.ensure_note_images(&doc);
        self.preview_doc = Some(doc);
    }

    /// 编辑档解析产物与块元数据重建（frontmatter 剥离：编辑面只处理正文，坐标相对正文）。
    pub(crate) fn refresh_edit(&mut self) {
        if self.note_mode != NoteMode::Edit {
            self.edit_doc = None;
            self.edit_meta.clear();
            return;
        }
        let body = strip_frontmatter(&self.note_text);
        let doc = markdown_view::parse_document(body);
        self.edit_meta = markdown_view::block_meta(&doc.blocks);
        // 空文档（无块）也要有光标宿主：补一个空块
        if self.edit_meta.is_empty() {
            self.edit_meta.push(markdown_view::empty_block_meta());
        }
        self.ensure_note_images(&doc);
        self.edit_doc = Some(doc);
    }

    /// 文档内块级图片的按需载入：缺句柄且未在途/未失败的 src 发起读盘（仓库相对路径）或
    /// 公网策略下载（http/https）任务，任务经 update 收尾统一链出。失败不重试（失败集）。
    fn ensure_note_images(&mut self, doc: &MarkdownDocument) {
        let srcs = markdown_view::collect_image_srcs(&doc.source, &doc.blocks);
        if srcs.is_empty() {
            return;
        }
        let gen = self.epoch();
        let root = self.vault.root().ok();
        let mut tasks = Vec::new();
        for src in srcs {
            if self.note_images.contains_key(&src)
                || self.note_images_failed.contains(&src)
                || !self.note_images_inflight.insert(src.clone())
            {
                continue;
            }
            let task = match (&root, src.starts_with("http://") || src.starts_with("https://")) {
                // 空间模式：仓库内路径走服务端字节读取
                (_, true) => {
                    let url = src.clone();
                    Task::perform(
                        async move { atelyx_core::remote_image::fetch_external_image(&url).await },
                        move |r| Message::ImageLoaded(gen, src.clone(), r),
                    )
                }
                (_, false) if matches!(self.repo_kind, RepoKind::Space(_)) => {
                    space_actions::space_load_image(self, gen, src.clone())
                }
                (Some(root), false) => {
                    let root = root.clone();
                    let path = src.clone();
                    Task::perform(
                        async move { vault::read_file_bytes(&root, &path) },
                        move |r| Message::ImageLoaded(gen, src.clone(), r),
                    )
                }
                // 无仓库时仓库内图片无落点：保持占位
                (None, false) => {
                    self.note_images_inflight.remove(&src);
                    continue;
                }
            };
            tasks.push(task);
        }
        if !tasks.is_empty() {
            let batch = Task::batch(tasks);
            self.image_loads = Some(match self.image_loads.take() {
                Some(prev) => prev.chain(batch),
                None => batch,
            });
        }
    }

    /// 读取仓库元数据（仓库配置 / 提示词标记 / 文件夹颜色；缺省回落不阻塞）。
    fn load_vault_meta(&self) -> Task<Message> {
        if matches!(self.repo_kind, RepoKind::Space(_)) {
            return space_actions::load_space_meta(self);
        }
        let Some(root) = self.vault.root().ok() else { return Task::none() };
        let gen = self.epoch();
        Task::perform(
            async move {
                let config = vault::read_vault_config(&root)?;
                let prompts = vault::read_prompt_notes_file(&root).unwrap_or_default();
                let colors = vault::read_folder_colors_file(&root).unwrap_or_default();
                Ok((config, prompts, colors))
            },
            move |result| Message::VaultMetaLoaded(gen, result),
        )
    }

    /// 提示词标记注册/注销（读改写原子落盘，回执重读对齐）。
    fn toggle_prompt_note(&mut self, rel: &str) -> Task<Message> {
        if matches!(self.repo_kind, RepoKind::Space(_)) {
            return space_actions::space_toggle_prompt_note(self, rel);
        }
        let Some(root) = self.vault.root().ok() else { return Task::none() };
        let rel = rel.to_string();
        let gen = self.epoch();
        Task::perform(
            async move {
                let mut notes = vault::read_prompt_notes_file(&root)?;
                if let Some(i) = notes.iter().position(|n| n == &rel) {
                    notes.remove(i);
                } else {
                    notes.push(rel.clone());
                }
                vault::write_prompt_notes_file(&root, &notes)?;
                vault::read_prompt_notes_file(&root)
            },
            move |result| Message::PromptNotesLoaded(gen, result),
        )
    }

    /// 设置/清除文件夹图标颜色（None = 恢复默认即删键；回执重读对齐）。
    fn set_folder_color(&mut self, rel: &str, color: Option<String>) -> Task<Message> {
        if matches!(self.repo_kind, RepoKind::Space(_)) {
            return space_actions::space_set_folder_color(self, rel, color);
        }
        let Some(root) = self.vault.root().ok() else { return Task::none() };
        let rel = rel.to_string();
        let gen = self.epoch();
        Task::perform(
            async move {
                let mut colors = vault::read_folder_colors_file(&root)?;
                match color {
                    Some(c) => colors.insert(rel, c),
                    None => colors.remove(&rel),
                };
                vault::write_folder_colors_file(&root, &colors)?;
                vault::read_folder_colors_file(&root)
            },
            move |result| Message::FolderColorsLoaded(gen, result),
        )
    }

    /// 创建同目录副本（文件字节复制 + .atlx/.atb 重生成 id；文件夹递归复制含 id 重生成）。
    /// 目标名同名自动加序号，实际落点经 OpDone 回传。空间模式 = copy 端点 + id 重建。
    fn duplicate_entry(&mut self, rel: &str, is_dir: bool) -> Task<Message> {
        if matches!(self.repo_kind, RepoKind::Space(_)) {
            return space_actions::space_duplicate_entry(self, rel, is_dir);
        }
        let Some(root) = self.vault.root().ok() else { return Task::none() };
        let gen = self.epoch();
        let parent = parent_of(rel);
        let name = rel.rsplit('/').next().unwrap_or(rel).to_string();
        let rel = rel.to_string();
        let is_dir_for_task = is_dir;
        let parent_for_task = parent.clone();
        Task::perform(
            async move {
                let parent = parent_for_task;
                if is_dir_for_task {
                    // 副本场景源名必须视为已占用（exclude 传空串），否则首个候选 = 源名自复制
                    let target = dedupe_sibling(&root, &parent, &name, None, "")?;
                    vault::copy_folder(&root, &rel, &full_rel(&parent, &target))?;
                    Ok(full_rel(&parent, &target))
                } else {
                    let (stem, ext) = match name.rfind('.') {
                        Some(0) | None => (name.clone(), String::new()),
                        Some(i) => (name[..i].to_string(), name[i..].to_string()),
                    };
                    let target = dedupe_sibling(&root, &parent, &stem, Some(&ext), "")?;
                    let new_rel = full_rel(&parent, &target);
                    let src = vault::safe_join(&root, &rel, false)?;
                    let dst = vault::safe_join(&root, &new_rel, true)?;
                    std::fs::copy(&src, &dst).map_err(|e| format!("复制文件失败：{e}"))?;
                    vault::regenerate_file_id(&dst)?;
                    Ok(new_rel)
                }
            },
            move |result| Message::OpDone(gen, parent, result, FollowUp::None),
        )
    }

    /// 拖拽释放落点 → 移动（挂起输入先落盘：引用同步会代写打开中的笔记，防回写覆盖）。
    fn resolve_file_drop(&mut self, d: FileDrag) -> Task<Message> {
        let Some(target_dir) = self.file_drag_target.take() else { return Task::none() };
        let from_dir = parent_of(&d.from);
        if target_dir == from_dir {
            return Task::none();
        }
        self.gate(PendingAfterFlush::MoveEntry {
            from: d.from,
            is_dir: d.is_dir,
            to_dir: target_dir,
        })
    }

    /// 移动文件/文件夹到目标目录（保持文件名，同名自动加序号；引用同步事务）。
    /// 空间模式 = 服务端 rename（重名 409 提示）。
    fn move_entry(&mut self, from: &str, is_dir: bool, to_dir: &str) -> Task<Message> {
        if matches!(self.repo_kind, RepoKind::Space(_)) {
            let _ = is_dir;
            return space_actions::space_move_entry(self, from, to_dir);
        }
        let Some(root) = self.vault.root().ok() else { return Task::none() };
        let Ok(exclude) = self.vault.exclude_folders() else { return Task::none() };
        let gen = self.epoch();
        let name = from.rsplit('/').next().unwrap_or(from).to_string();
        let from = from.to_string();
        let from_for_task = from.clone();
        let to_dir = to_dir.to_string();
        let to_dir_for_task = to_dir.clone();
        let is_dir_for_task = is_dir;
        Task::perform(
            async move {
                let to_dir = to_dir_for_task;
                let from = from_for_task;
                let target = if is_dir_for_task {
                    dedupe_sibling(&root, &to_dir, &name, None, &from)?
                } else {
                    let (stem, ext) = match name.rfind('.') {
                        Some(0) | None => (name.clone(), String::new()),
                        Some(i) => (name[..i].to_string(), name[i..].to_string()),
                    };
                    dedupe_sibling(&root, &to_dir, &stem, Some(&ext), &from)?
                };
                let new_rel = full_rel(&to_dir, &target);
                if new_rel == from {
                    return Ok((new_rel, Vec::new()));
                }
                let rewritten = if is_dir_for_task {
                    vault_refs::rename_folder_synced(&root, &exclude, &from, &new_rel)?.rewritten
                } else {
                    vault_refs::rename_note_synced(&root, &exclude, &from, &new_rel)?.rewritten
                };
                Ok((new_rel, rewritten))
            },
            move |result| match result {
                Ok((full, rewritten)) => Message::OpDone(
                    gen,
                    to_dir,
                    Ok(full.clone()),
                    FollowUp::Renamed { from, to: full, rewritten },
                ),
                Err(e) => Message::OpDone(gen, to_dir, Err(e), FollowUp::None),
            },
        )
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
    /// 命中目标 → 正文偏移（编辑面坐标相对正文）。行内换算多由编辑面就地完成，
    /// 这里只处理源码排版换算、行边界吸附与文首 / 文末。
    fn resolve_hit(&self, target: HitTarget) -> usize {
        let len = self.edit_body().len();
        match target {
            HitTarget::Offset(offset) => offset.min(len),
            HitTarget::RowEdge { from, to, upper } => if upper { from } else { to }.min(len),
            HitTarget::SourceText { from, to, source, point, width } => markdown_edit::offset_in_block(
                self.edit_body(),
                from,
                to,
                &source,
                width,
                point,
                &self.edit_opts,
            ),
            HitTarget::CodeBody { content, size, point, width } => {
                let code = &self.edit_body()[content.0.min(len)..content.1.min(len)];
                let code = code.trim_end_matches('\n');
                content.0 + markdown_edit::code_offset(code, size, width, point)
            }
            HitTarget::GapLine { offset } => offset.min(len),
            HitTarget::Start => self.edit_meta.first().map_or(0, |m| m.from),
            HitTarget::End => len,
        }
    }

    /// 活动块 = 光标所在块。光标落在空白区（文首 / 块间 / 文末的空白行与文末虚拟行）
    /// 时不揭示任何块，键盘与光标由编辑面托管；其余文外位置回到最近块保住键盘宿主。
    fn sync_active(&mut self) {
        let len = self.edit_body().len();
        let caret = self.edit.caret.min(len);
        self.close_wiki_picker_if_outside();
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
        self.block_at(self.edit.anchor.min(self.edit_body().len()))
    }

    /// 一次编辑落地后的收尾。
    fn after_edit(&mut self) {
        self.refresh_edit();
        self.sync_active();
        self.autosave_deadline = Some(Instant::now() + AUTOSAVE_DEBOUNCE);
        self.sync_wiki_picker();
        // 空间模式：差分合入协作文档并上行 Update 帧（不置脏、不排保存）
        space_actions::collab_after_edit(self);
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
        let body = self.edit_body().to_string();
        self.edit.snapshot(&self.note_text);
        let mut next = String::with_capacity(body.len());
        next.push_str(&body[..a]);
        next.push_str(&body[b..]);
        self.set_edit_body(next);
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
        let body = self.edit_body().to_string();
        self.edit.snapshot(&self.note_text);
        let (a, b) = self.edit.selection();
        let mut next = String::with_capacity(body.len() + text.len());
        next.push_str(&body[..a]);
        next.push_str(&text);
        next.push_str(&body[b..]);
        self.set_edit_body(next);
        self.edit.caret = a + text.len();
        self.edit.anchor = self.edit.caret;
        self.after_edit();
    }

    /// 退格：先删选区；否则空列表项整段标记；再否则删前一个字符。
    fn edit_backspace(&mut self) {
        if self.delete_selection() {
            return;
        }
        let body = self.edit_body().to_string();
        if self.is_in_list() {
            if let Some((next, caret)) =
                markdown_edit::list_backspace_edit(&body, self.edit.caret)
            {
                self.edit.snapshot(&self.note_text);
                self.set_edit_body(next);
                self.edit.caret = caret;
                self.edit.anchor = caret;
                self.after_edit();
                return;
            }
        }
        let caret = self.edit.caret.min(body.len());
        if caret == 0 {
            return;
        }
        let prev = prev_boundary(&body, caret);
        self.edit.snapshot(&self.note_text);
        let mut next = String::with_capacity(body.len());
        next.push_str(&body[..prev]);
        next.push_str(&body[caret..]);
        self.set_edit_body(next);
        self.edit.caret = prev;
        self.edit.anchor = prev;
        self.after_edit();
    }

    /// 删除：先删选区；否则删后一个字符。
    fn edit_delete(&mut self) {
        if self.delete_selection() {
            return;
        }
        let body = self.edit_body().to_string();
        let caret = self.edit.caret.min(body.len());
        if caret >= body.len() {
            return;
        }
        let after = next_boundary(&body, caret);
        self.edit.snapshot(&self.note_text);
        let mut next = String::with_capacity(body.len());
        next.push_str(&body[..caret]);
        next.push_str(&body[after..]);
        self.set_edit_body(next);
        self.after_edit();
    }

    /// 回车：列表块内延续项标记，否则插入换行。
    fn edit_newline(&mut self) {
        if self.delete_selection() {
            return;
        }
        let body = self.edit_body().to_string();
        if self.is_in_list() {
            if let Some((next, caret)) =
                markdown_edit::list_enter_edit(&body, self.edit.caret)
            {
                self.edit.snapshot(&self.note_text);
                self.set_edit_body(next);
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
        let body = self.edit_body().to_string();
        let edit = if outdent {
            match markdown_edit::outdent_edit(&body, from, to) {
                Some(edit) => edit,
                None => return,
            }
        } else {
            markdown_edit::indent_edit(&body, from, to)
        };
        self.edit.snapshot(&self.note_text);
        self.set_edit_body(edit.text);
        self.edit.anchor = edit.from;
        self.edit.caret = edit.to;
        self.after_edit();
    }

    /// 光标移动（上下越界时跨块）。
    fn edit_move(&mut self, motion: markdown_edit::Motion, extend: bool) {
        use markdown_edit::Motion;
        let body = self.edit_body();
        let len = body.len();
        let caret = self.edit.caret.min(len);
        let target = match motion {
            Motion::Left => prev_boundary(body, caret),
            Motion::Right => next_boundary(body, caret),
            Motion::Home => line_start_of(body, caret),
            Motion::End => line_end_of(body, caret),
            Motion::Up => {
                self.edit_move_line(false, extend);
                return;
            }
            Motion::Down => {
                self.edit_move_line(true, extend);
                return;
            }
        };
        if !extend {
            self.edit.anchor = target;
        }
        self.edit.caret = target;
        self.sync_active();
    }

    /// 上下移动逐行走：上一行 / 下一行行首（空行与文末虚拟行同样是行；越界不动）。
    /// 行内同列的上下移动由行自己的源码面承接，走到行的边界才落到相邻行。
    fn edit_move_line(&mut self, forward: bool, extend: bool) {
        let body = self.edit_body();
        let len = body.len();
        let caret = self.edit.caret.min(len);
        let line_start = line_start_of(body, caret);
        let target = if forward {
            match body[line_start..].find('\n') {
                Some(i) => line_start + i + 1,
                None => return,
            }
        } else if line_start == 0 {
            return;
        } else {
            line_start_of(body, line_start - 1)
        };
        let target = target.min(len);
        if !extend {
            self.edit.anchor = target;
        }
        self.edit.caret = target;
        self.sync_active();
    }

    fn edit_undo(&mut self) {
        if let Some((text, caret, anchor)) = self.edit.undo(&self.note_text) {
            // 撤销栈存全文（含 frontmatter）：直接回填，正文坐标不变。
            // 属性槽编辑态随内容失效（槽序号可能越界），一律取消
            self.props.editing = None;
            self.props.adding = None;
            self.props.draft.clear();
            self.note_text = text;
            self.dirty = true;
            let body_len = self.edit_body().len();
            self.edit.caret = caret.min(body_len);
            self.edit.anchor = anchor.min(body_len);
            self.refresh_props();
            self.after_edit();
        }
    }

    fn edit_redo(&mut self) {
        if let Some((text, caret, anchor)) = self.edit.redo(&self.note_text) {
            self.props.editing = None;
            self.props.adding = None;
            self.props.draft.clear();
            self.note_text = text;
            self.dirty = true;
            let body_len = self.edit_body().len();
            self.edit.caret = caret.min(body_len);
            self.edit.anchor = anchor.min(body_len);
            self.refresh_props();
            self.after_edit();
        }
    }

    /// 命令候选键分派：录制态先吃按键；注册表按序先到先得——Note 域让位给
    /// 编辑面部件内的绑定表（部件持焦点即已消费本键，未持焦点则放行后命者），
    /// Table 域在表格持焦点时转发既有 TableMsg。注册表外的 Ctrl+S（保存）与
    /// Ctrl+A（表格全选）保持硬编码，下游按焦点自守卫。
    fn on_key_command(&mut self, key: keyboard::Key, mods: keyboard::Modifiers) -> Task<Message> {
        if self.shortcut_recording.is_some() {
            return self.on_shortcut_key(key, mods);
        }
        let resolved = commands::resolve(&self.global.command_shortcuts);
        for (id, binding) in &resolved {
            if !commands::matches(binding, mods, &key) {
                continue;
            }
            match id.scope() {
                commands::Scope::Note => {
                    if self.edit.focused {
                        return Task::none();
                    }
                }
                commands::Scope::Table => {
                    if self.table.as_ref().is_some_and(|t| t.view.focused) {
                        if let Some(msg) = table_msg_of(*id) {
                            return self.handle(Message::Table(msg));
                        }
                    }
                }
            }
        }
        // 重做别名：redo 未被重定义时 mod+shift+z 沿用（编辑面聚焦走部件内注入，表格直接分派）
        if (mods.control() || mods.logo())
            && mods.shift()
            && !mods.alt()
            && matches!(&key, keyboard::Key::Character(c) if c.to_lowercase() == "z")
        {
            if self.edit.focused {
                return Task::none();
            }
            if self.table.as_ref().is_some_and(|t| t.view.focused)
                && commands::redo_alias_active(&self.global.command_shortcuts, commands::Scope::Table)
            {
                return self.handle(Message::Table(tv::TableMsg::Redo));
            }
        }
        // 退格 = 清空选区的既有别名（清空键位保持默认 delete 且表格持焦点时沿用）
        if mods.is_empty()
            && matches!(key, keyboard::Key::Named(keyboard::key::Named::Backspace))
            && resolved
                .iter()
                .find(|(c, _)| *c == commands::CommandId::TableClear)
                .is_some_and(|(_, b)| b == "delete")
            && self.table.as_ref().is_some_and(|t| t.view.focused)
        {
            return self.handle(Message::Table(tv::TableMsg::ClearSelection));
        }
        if (mods.control() || mods.logo()) && !mods.alt() {
            if let keyboard::Key::Character(c) = &key {
                return match c.to_lowercase().as_str() {
                    "s" => self.handle(Message::Save),
                    "a" => self.handle(Message::Table(tv::TableMsg::SelectAll)),
                    _ => Task::none(),
                };
            }
        }
        Task::none()
    }

    /// 录制态按键：捕获 → 校验 → 写覆盖并落盘收尾。校验失败提示后保持录制态；
    /// 纯修饰键 / 未注册键名忽略（继续等待）；Esc 取消由 Escape 处理器先行。
    fn on_shortcut_key(&mut self, key: keyboard::Key, mods: keyboard::Modifiers) -> Task<Message> {
        let Some(id) = self.shortcut_recording else { return Task::none() };
        let Some(binding) = commands::capture(mods, &key) else {
            return Task::none();
        };
        if let Err(reason) = commands::validate(id, &binding) {
            self.notify(NoticeLevel::Warning, reason);
            return Task::none();
        }
        // 首个通过校验的组合键即写入覆盖并退出录制态
        self.global
            .command_shortcuts
            .insert(id.key().to_string(), binding);
        self.shortcut_recording = None;
        self.save_global_now();
        Task::none()
    }

    /// 编辑面事件处理。
    fn on_editor(&mut self, event: markdown_edit::EditorEvent) -> Task<Message> {
        use markdown_edit::EditorEvent as E;
        // 进编辑档待落光标在首个编辑面事件消化（部件侧据此定位光标，见 EditSurface 释放路径）
        self.note_edit_caret_pending = false;
        match event {
            E::Hit { target, extend } => {
                let offset = self.resolve_hit(target);
                if !extend {
                    self.edit.anchor = offset;
                }
                self.edit.caret = offset.min(self.edit_body().len());
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
                let offset = offset.min(self.edit_body().len());
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
                // 双链候选开启时 Enter 归候选选中（编辑面无条件上报 Newline）；
                // 选中路径已先行时吞掉本次 Newline，防换行混入候选替换区间
                if self.wiki_picker.is_some() || self.wiki_swallow_newline {
                    self.wiki_swallow_newline = false;
                    return Task::none();
                }
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
                self.edit.caret = self.edit_body().len();
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
        match form {
            LinkForm::External => {
                open_external(&decoded);
                Task::none()
            }
            LinkForm::Path | LinkForm::Wiki | LinkForm::Create => {
                self.gate(PendingAfterFlush::OpenLink(decoded))
            }
            LinkForm::Plain => Task::none(),
        }
    }

    /// 打开仓库内笔记链接（flush 放行后执行；落点以磁盘事实为准）。
    fn open_link_inner(&mut self, decoded: String) -> Task<Message> {
        let path = decoded.split('#').next().unwrap_or("").trim().to_string();
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
        if matches!(self.repo_kind, RepoKind::Space(_)) {
            return space_actions::space_load_note(self, rel);
        }
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
                        format!("Atelyx — {}", layout_view::view_label(self, &t.view))
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
        } else if self.plugin_windows.contains_key(&id) {
            layout_view::plugin_window_view(self, id)
        } else {
            space().into()
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        // 心跳仅在有待过期通知、提醒条在途、悬停提示在途（距离收起判定）、编辑面聚焦、
        // 远端光标在册（过期剔除节流搭载）、表格图片长按候选或快捷键页展示中（冲突警示两态闪烁）时挂载
        let need_tick = !self.notifications.is_empty()
            || self.rename_notice.is_some()
            || self.tooltip.is_some()
            || self.edit.focused
            || !self.remote_cursors.is_empty()
            || !self.chat.renaming.is_empty()
            || (self.settings_open && self.settings_tab == "shortcuts")
            || self.table.as_ref().is_some_and(|t| {
                t.view.press.as_ref().is_some_and(|p| p.img.is_some() && !p.active)
            });
        Subscription::batch([
            iced::time::every(PERSIST_TICK).map(|_| Message::PersistTick),
            if need_tick {
                iced::time::every(std::time::Duration::from_millis(100)).map(|_| Message::UiTick)
            } else {
                Subscription::none()
            },
            // 方向键：双链候选开启时导航候选，否则调整分割把手（语义在 NavKey 处理器分派）
            // 表格专属键在弹层打开时也照发，由 on_msg 处理器按弹层态自守卫
            iced::event::listen_with(|event, status, window| match event {
                Event::Keyboard(keyboard::Event::ModifiersChanged(m)) => {
                    Some(Message::ModsChanged(m))
                }
                Event::Keyboard(keyboard::Event::KeyPressed {
                    key: keyboard::Key::Named(keyboard::key::Named::ArrowLeft),
                    modifiers,
                    ..
                }) if modifiers.is_empty() => Some(Message::NavKey(true, false)),
                Event::Keyboard(keyboard::Event::KeyPressed {
                    key: keyboard::Key::Named(keyboard::key::Named::ArrowRight),
                    modifiers,
                    ..
                }) if modifiers.is_empty() => Some(Message::NavKey(true, true)),
                Event::Keyboard(keyboard::Event::KeyPressed {
                    key: keyboard::Key::Named(keyboard::key::Named::ArrowUp),
                    modifiers,
                    ..
                }) if modifiers.is_empty() => Some(Message::NavKey(false, false)),
                Event::Keyboard(keyboard::Event::KeyPressed {
                    key: keyboard::Key::Named(keyboard::key::Named::ArrowDown),
                    modifiers,
                    ..
                }) if modifiers.is_empty() => Some(Message::NavKey(false, true)),
                // 已被组件捕获的 Enter（文本编辑器发送 / 表格编辑提交等）不再全局派发
                Event::Keyboard(keyboard::Event::KeyPressed {
                    key: keyboard::Key::Named(keyboard::key::Named::Enter),
                    modifiers,
                    ..
                }) if modifiers.is_empty() && status == iced::event::Status::Ignored => {
                    Some(Message::EnterKey)
                }
                Event::Keyboard(keyboard::Event::KeyPressed {
                    key: keyboard::Key::Named(keyboard::key::Named::Home),
                    modifiers,
                    ..
                }) if modifiers.is_empty() => {
                    Some(Message::Table(table_view::TableMsg::NavKey(table_view::NavDir::Home)))
                }
                Event::Keyboard(keyboard::Event::KeyPressed {
                    key: keyboard::Key::Named(keyboard::key::Named::End),
                    modifiers,
                    ..
                }) if modifiers.is_empty() => {
                    Some(Message::Table(table_view::TableMsg::NavKey(table_view::NavDir::End)))
                }
                Event::Keyboard(keyboard::Event::KeyPressed {
                    key: keyboard::Key::Character(_),
                    modifiers,
                    text,
                    ..
                }) if modifiers.is_empty() => {
                    // 打印字符（无修饰）：选区态覆盖输入（编辑态由编辑器部件接管，处理器自守卫）
                    text.and_then(|t| t.chars().next())
                        .filter(|ch| !ch.is_control())
                        .map(|ch| Message::Table(table_view::TableMsg::CharKey(ch)))
                }
                Event::Keyboard(keyboard::Event::KeyPressed {
                    key: keyboard::Key::Named(keyboard::key::Named::Tab),
                    modifiers,
                    ..
                }) => Some(Message::Props(PropsMsg::CommitMove(if modifiers.shift() { -1 } else { 1 }))),
                Event::Keyboard(keyboard::Event::KeyPressed {
                    key: keyboard::Key::Named(keyboard::key::Named::Escape),
                    ..
                }) => Some(Message::Escape(window)),
                // 命令候选键（撤销/重做/剪贴板/清空 + Ctrl+S/A 兜底）：置于具名键臂之后，
                // 不抢占 NavKey / CharKey / Tab / Escape 既有路径
                Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. })
                    if command_key_candidate(&key, modifiers) =>
                {
                    Some(Message::KeyCommand { key, mods: modifiers })
                }
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
                Event::Window(window::Event::Focused) => Some(Message::WindowFocused(window)),
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
                Event::Window(window::Event::FileDropped(path)) => {
                    // 外部文件拖入：路由到对话面板（是否纳用由编排按当前布局判定）
                    Some(Message::Chat(chat_actions::ChatMsg::FileDropped { window, path }))
                }
                _ => None,
            }),
            self.space_pump_subscription(),
            self.plugin_events_subscription(),
            self.plugin_host_requests_subscription(),
            chat_actions::run_events_subscription(),
            chat_actions::control_subscription(),
        ])
    }

    /// 插件运行时事件订阅：单一广播通道接进 update（同一运行时不重建流）。
    fn plugin_events_subscription(&self) -> Subscription<Message> {
        Subscription::run_with(
            settings_view::plugins::RuntimeSpec(self.plugins.clone()),
            settings_view::plugins::runtime_stream,
        )
    }

    /// 壳侧服务请求订阅：对话框请求走无界通道（不丢帧），与事件广播分属两条流。
    fn plugin_host_requests_subscription(&self) -> Subscription<Message> {
        Subscription::run_with(
            settings_view::plugins::RuntimeSpec(self.plugins.clone()),
            settings_view::plugins::host_requests_stream,
        )
    }

    /// 协作帧泵订阅：空间会话激活期间保持一条 WS 长连接（心跳 / 断线重连在泵内建）。
    /// 泵实例经 spec 固定身份（重连不发新 spec）；命令入口由 PumpReady 消息回填，
    /// 事件流转发为 Event 消息。
    fn space_pump_subscription(&self) -> Subscription<Message> {
        if self.space_pump_denied {
            // 服务端已拒绝（令牌失效 / 被移出）：重连无意义，停闸防订阅重算重生泵
            return Subscription::none();
        }
        let crate::RepoKind::Space(session) = &self.repo_kind else {
            return Subscription::none();
        };
        let Some(token) = self.space_token.clone() else {
            return Subscription::none();
        };
        let nickname = self
            .global
            .last_identity
            .as_ref()
            .map(|i| i.display_name.clone())
            .unwrap_or_else(|| "用户".into());
        let color = self
            .global
            .collab_color
            .clone()
            .unwrap_or_else(|| space_actions::DEFAULT_COLLAB_COLOR.into());
        // 泵身份 = 连接五元组（哈希作为订阅标识；换空间/换号自动换泵）
        let spec = serde_json::json!([
            session.server_url,
            session.space_id,
            token,
            nickname,
            color,
            space_actions::DEFAULT_DEVICE_NAME,
        ])
        .to_string();
        Subscription::run_with(spec, |spec: &String| space_pump_stream(spec.as_str()))
    }
}

/// 帧泵订阅流构建器（`run_with` 经闭包适配调用）：建泵 → 回填命令入口 → 转发事件。
fn space_pump_stream(
    spec: &str,
) -> impl iced::futures::Stream<Item = Message> + use<> {
    let spec = spec.to_owned();
    iced::stream::channel(64, async move |mut output| {
        // spec = [url, spaceId, token, nickname, color, deviceName]
        let Ok(parts) = serde_json::from_str::<Vec<String>>(&spec) else {
            return;
        };
        if parts.len() < 6 {
            return;
        }
        let hello = atelyx_core::collab::HelloFrame {
            space_id: parts[1].clone(),
            token: parts[2].clone(),
            nickname: parts[3].clone(),
            color: parts[4].clone(),
            device_name: parts[5].clone(),
            version: None,
        };
        let (cmds, mut events) = atelyx_core::collab::spawn_pump(&parts[0], hello);
        let _ = iced::futures::SinkExt::send(
            &mut output,
            Message::Space(SpaceMsg::PumpReady {
                server_url: parts[0].clone(),
                space_id: parts[1].clone(),
                cmds,
            }),
        )
        .await;
        while let Some(event) = events.recv().await {
            let _ = iced::futures::SinkExt::send(
                &mut output,
                Message::Space(SpaceMsg::Event {
                    server_url: parts[0].clone(),
                    space_id: parts[1].clone(),
                    event: Box::new(event),
                }),
            )
            .await;
        }
    })
}
