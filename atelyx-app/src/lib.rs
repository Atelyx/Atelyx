//! Atelyx 原生壳：daemon 多窗口 + 布局模型（场景/布局/面板树）+ 各视图实现。
//!
//! 布局状态真源 = atelyx-core::layout：落盘 ui-state.json（磁盘格式由 schema
//! 版本标识，本壳只读不改格式）；本壳负责窗口副作用（OS 窗口、落盘防抖、输入路由）。
//!
//! 本 crate 同时是装配根：它认识全部功能模块（`pack`），并把它们的视图交给布局层。
//! 进程入口在 `main.rs`，只调 [`run`]。

mod autostart;
mod chat_actions;
mod app_icon;
mod color_picker_actions;
mod commands;
mod history_browser;
mod clipboard;
mod layout_view;
mod pack;
mod plugin_actions;
mod doc_session;
mod panel_header;
mod plugin_theme;
mod plugin_frame;
mod plugin_overlay;
mod plugin_paint;
mod plugin_view;
mod recent_files;
mod registry;
mod repo_history;
mod settings_view;
mod space_actions;
mod doc_presence;
mod table_panel;
mod tray;
pub use atelyx_ui as ui;
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
use atelyx_core::markdown::LinkForm;
use atelyx_core::plugin_runtime::{
    EditorSelection, FileChangeKind, PluginEvent, PluginRuntime, PluginStatus, SurfaceKind,
};
use atelyx_core::vault;
use atelyx_core::vault_refs;
use iced::widget::space;
use iced::window::{self, Level, Mode, Position, Settings};
use iced::{keyboard, mouse, Element, Event, Point, Size, Subscription, Task, Theme};

use ui::notify::{Level as NoticeLevel, Notification, Progress as NoticeProgress};

use layout_view::{
    enumerate_dividers, panel_tab_ids_in, PopupOpen, PopupState, RenameState, RenameTarget, UiMetrics,
};
use plugin_actions::OpenMode;

/// 撕裂窗口建窗参数：bounds 恢复位置 + 条目 OS 选项（不进任务栏）；无系统边框。
fn panel_settings(w: &DetachedWindow) -> Settings {
    // 非 Windows 不动 platform_specific：mut 只在该平台分支下需要
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut settings = Settings {
        size: Size::new(w.bounds.width.max(200.0) as f32, w.bounds.height.max(160.0) as f32),
        min_size: Some(PANEL_MIN_SIZE),
        position: Position::Specific(Point::new(w.bounds.x as f32, w.bounds.y as f32)),
        exit_on_close_request: false,
        decorations: false,
        icon: app_icon::window_icon(),
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

/// 剥离开头的 frontmatter 块（属性面板与正文处理只处理正文）；缺闭合定界符时按无 frontmatter 处理，全量正文。
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

/// 历史署名身份：已登录用登录身份的显示名（与协作成员名同源），未登录回落本机用户名（用户名即设备名）。
/// id 随身份走——切换账号/空间即换身份，连续编辑合并只发生在同一身份内。
pub(crate) fn history_author(app: &App) -> atelyx_core::history::HistoryAuthor {
    if let Some(id) = app.global.last_identity.as_ref().filter(|i| !i.display_name.trim().is_empty()) {
        return atelyx_core::history::HistoryAuthor {
            id: format!("space:{}@{}", id.username, id.server_url),
            name: id.display_name.clone(),
            device: crate::space_actions::DEFAULT_DEVICE_NAME.to_string(),
        };
    }
    let user = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "本地".into());
    atelyx_core::history::HistoryAuthor { id: format!("desktop:{user}"), name: user.clone(), device: user }
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

/// 启动 daemon：建主窗口、装配功能模块、跑 iced 事件循环。
pub fn run() -> iced::Result {
    // 字体族清单必须在建渲染器之前备好：default_font 只在渲染器创建时读一次
    ui::font::init();
    iced::daemon(App::boot, App::update, App::view)
        .title(App::title)
        .theme(|app: &App, _id| app.theme.clone())
        .default_font(iced::Font {
            family: ui::family(),
            ..iced::Font::default()
        })
        .subscription(App::subscription)
        .run()
}

/// 主窗口默认大小（逻辑像素）。
const DEFAULT_WINDOW_SIZE: Size = Size::new(1440.0, 900.0);
/// 主窗口最小大小（逻辑像素）。
const MIN_WINDOW_SIZE: Size = Size::new(1100.0, 760.0);

/// 双击判定窗口：同一目标两次按压的间隔上限（标题栏空白最大化与布局 tab 改名共用）。
const DOUBLE_CLICK_MS: u128 = 350;

/// 防抖落盘周期：期间多次模型变更合并为一次写盘（拖宽/拖拽等连续操作不逐帧写）。
const PERSIST_TICK: std::time::Duration = std::time::Duration::from_millis(800);

/// 帧拍的调度粒度（125 Hz，覆盖协议允许的最高帧率）：具体节拍由各画布与整面帧声明的 fps 决定，
/// 此值只决定检查密度——比它更慢会让 120 fps 的声明退化成按检查密度发帧。
const FRAME_TICK: std::time::Duration = std::time::Duration::from_millis(8);

/// 自动保存防抖：最近一次内容变更后停止输入该时长才写回（尾沿 debounce，单定时器链续排）。
pub(crate) const AUTOSAVE_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(500);

/// 应用数据目录（bundle identifier = com.atelyx.desktop）；ui-state.json 与 global.json 同放此处。
fn app_data_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    let base = std::env::var("APPDATA").ok().map(PathBuf::from);
    #[cfg(not(windows))]
    let base = std::env::var("XDG_DATA_HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".local/share")));
    // 拼接写在 cfg 之外：紧跟 cfg 块的 `.map(...)` 会被同一条 cfg 一并裁掉，Windows 上会静默
    // 退回未加标识符的裸目录（应用级文件与插件根随之漂移）
    base.map(|d| d.join("com.atelyx.desktop"))
}

/// ui-state.json 落盘路径（应用级 UI 状态）。
fn ui_state_path() -> Option<PathBuf> {
    app_data_dir().map(|d| d.join("ui-state.json"))
}

/// 发行包内的预置插件目录：Windows 安装目录下 `preset-plugins/`，Linux 安装布局在
/// `<exe>/../lib/atelyx/preset-plugins`。取第一个存在的。
fn bundled_presets_dir() -> Option<PathBuf> {
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    [exe_dir.join("preset-plugins"), exe_dir.join("../lib/atelyx/preset-plugins")]
        .into_iter()
        .find(|dir| dir.is_dir())
}

/// 当前全局编辑偏好（`editor.prefs` 回执与偏好变更事件共用同一份）。
pub(crate) fn editor_prefs(app: &App) -> atelyx_core::plugin_runtime::EditorPrefs {
    atelyx_core::plugin_runtime::EditorPrefs {
        soft_wrap: app.global.soft_wrap,
        in_page_title: app.global.in_page_title,
        content_width: app.global.content_width,
    }
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
/// 宿主的键盘派生消息（整面帧持有键盘时一律不处理）。
/// 只列**无命令修饰**的键盘来源项——带 Ctrl / Alt / Super 的组合键归宿主（`KeyCommand` / `CopyKey`
/// 因此不在此列），鼠标、窗口与定时消息也不受插件焦点影响。
fn is_host_keyboard_message(message: &Message) -> bool {
    match message {
        Message::NavKey(..) | Message::Escape(_) => true,
        _ => false,
    }
}

/// 整面帧收键盘时的宿主键盘派生消息：帧声明 `keys` 即收全部按键（含命令组合），宿主不做键盘动作。
fn is_frame_keyboard_message(message: &Message) -> bool {
    match message {
        Message::KeyCommand { .. } | Message::CopyKey { .. } => true,
        other => is_host_keyboard_message(other),
    }
}

fn command_key_candidate(key: &keyboard::Key, mods: keyboard::Modifiers) -> bool {
    match key {
        keyboard::Key::Character(c) => c.len() == 1 && (mods.control() || mods.logo()),
        keyboard::Key::Named(_) => true,
        keyboard::Key::Unidentified => false,
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

/// 一条插件声明的面板头部：哪个插件声明的、声明的内容是什么。
/// 头部状态由插件喂数据、宿主按当前主题渲染（状态胶囊 / 错误 / 胶囊 / 右侧文本 / 菜单项）。
pub(crate) struct PanelHeaderEntry {
    pub plugin_id: String,
    pub state: atelyx_core::plugin_runtime::PanelHeaderState,
}

/// 在途的面板落盘请求：令牌 + 目标面板 + 归属插件 + 用途。
pub(crate) struct PendingPanelFlush {
    pub token: u64,
    pub target: String,
    pub plugin_id: String,
    pub purpose: PanelFlushPurpose,
}

/// 落盘请求的用途：脏拦截放行动作 / 回滚提示等落盘回执再给。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PanelFlushPurpose {
    Gate,
    RestoreNotice,
}

/// 历史回滚待结算：文件、快照、是否空间仓库与发起时的世代（跨世代不回写）。
pub(crate) struct PendingTableRestore {
    pub rel: String,
    pub content: String,
    pub space: bool,
    pub gen: u64,
}

/// 画布回滚待结算：与表格同构——先交回快照给插件，落盘收敛后再记 restore 版本。
pub(crate) struct PendingCanvasRestore {
    pub rel: String,
    pub content: String,
    pub space: bool,
    pub gen: u64,
}

/// 面板落盘请求的等待上限：插件无响应时撤回请求并提示，不让脏拦截永久挂起。
const PANEL_FLUSH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// 界面图片落地的两种形态：原图字节（渲染层按需解码），或宿主已按上限解码好的 RGBA
/// 缩略图（避免大图整幅上传，并按 EXIF 方向摆正）。
#[derive(Clone, Debug)]
pub(crate) enum PluginImage {
    Bytes(Vec<u8>),
    Thumb(atelyx_core::image_thumb::Thumb),
}

#[derive(Debug, Clone)]
enum Message {
    WindowOpened(window::Id),
    WindowCloseRequested(window::Id),
    WindowClosed(window::Id),
    VaultPicked(Result<PathBuf, String>),
    /// 从最近仓库列表进入指定仓库（脏拦截统一 flush 放行）。
    /// 启动自动进入 = from_boot；运行中切换在 gate 保存后关闭当前打开的文件。
    SelectVault { path: PathBuf, from_boot: bool },
    /// gen = 发起任务时的仓库世代；回调落地前与当前世代比对，过期即丢弃。
    ChildrenLoaded(String, u64, Result<Vec<vault::DirChild>, String>),
    Save,
    /// 携带保存发起时冻结的内容与保存动机：落地后与当前正文比对，区分保存完成与在途新编辑。
    Saved(u64, String, String, SaveReason, Result<(), String>),
    /// 自动保存防抖定时器到期（单定时器链，见 chain_autosave_timer）。
    AutosaveTick,
    /// Ctrl/Cmd+C 未被输入部件捕获时回落到命令路由。
    CopyKey { key: keyboard::Key, mods: keyboard::Modifiers },
    /// 标签词汇表落地（gen 守卫）。
    TagVocabLoaded(u64, Result<Vec<vault::TagRow>, String>),
    /// 全仓库文件清单落地（`vault.files` 数据源；gen 守卫）。
    NoteFilesLoaded(u64, Result<Vec<(String, String)>, String>),
    /// 方向键（无可分派的输入态时调整分割把手）。
    NavKey(bool, bool),
    /// 历史版本列表落地（gen 守卫 + kind/文件身份守卫：版本快照随文件绑定，串台会跨文件回滚）。
    HistoryLoaded(u64, atelyx_core::history::HistoryKind, String, Vec<atelyx_core::history::HistoryVersion>),
    /// 历史记录弹层（打开 / 展开预览 / 回滚二段确认）。
    History(history_browser::HistoryBrowserMsg),
    /// 画布回滚写盘回执（gen 守卫；失败提示不阻塞）。
    /// 重建内部链接（设置-编辑器：二段确认 / 队列执行 / 回执）。
    RebuildLinks(settings_view::editor::RebuildLinksMsg),
    /// 历史记录/回滚落盘完成 → 重读版本列表。
    HistoryRecorded(u64, Option<String>),
    /// 最近打开条目失效剔除（确认缺失的 (vaultKey, file) 对）。
    RecentPruned(Vec<(String, String)>),
    /// 仓库历史聚合落地（容器世代 + 聚合世代双重守卫）。
    RepoHistoryLoaded(u64, u64, Result<repo_history::RepoHistory, String>),
    /// AI 对话面板消息（会话 / run 事件 / 编排）。
    Chat(chat_actions::ChatMsg),
    /// 无操作（后台任务回执无界面变化时的统一落点）。
    Noop,
    /// 打开链接（外链 / 仓库路径 / wiki）。
    OpenLink(String, LinkForm),
    /// 面板落盘请求超时（令牌）：撤回在途请求并提示。
    PanelFlushTimeout(u64),
    /// 历史回滚交回插件后延迟一拍请求落盘：等插件把新的脏头部送到宿主。
    TableRestoreFlush,
    CanvasRestoreFlush,
    /// 模态插件浮层期间点了被它盖住的面板头部：转成「按在浮层外」交给插件收起。
    PluginPopupDismiss { window: window::Id },
    /// 打开面板头部的「视图」下拉（宿主按贡献槽列出可切换的视图）。
    PanelViewMenu { window: window::Id, target: String },
    /// 打开面板头部的「⋯」菜单（条目来自插件声明）。
    PanelItemsMenu { window: window::Id, target: String },
    /// 用户点了头部菜单的某一项：按条目 id 回发声明它的插件。
    PanelHeaderAction { target: String, item: String },
    /// 面板内容槽切换视图（`None` = 该面板自己的默认视图）。
    PanelViewSelect { target: String, view: Option<String> },
    /// 图片落地（gen 守卫 + 内容键；失败按退避重试，用尽次数回落占位）。
    ImageLoaded(u64, String, Result<PluginImage, String>),
    /// 一次失败重试到点（gen 守卫）：重扫插件界面里仍缺句柄的来源。
    ImageRetryDue(u64),
    /// 取色器（表格字色/底色、文件夹图标色、协作展示色、插件取色）。
    ColorPicker(color_picker_actions::ColorPickerMsg),
    /// 协作展示色落盘（远端光标 / 名单取色）。
    CollabColorSet(String),
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


    /// 复制代码块正文到系统剪贴板。
    PanelTab(String, String),
    /// 空面板右键添加视图。
    PanelAddView { panel_id: String, view: String },
    /// 右键切换标签视图。
    SetTabView { panel_id: String, tab_id: String, view: String },
    /// 右键切换撕裂窗口标签视图。
    DetachedSetTabView { window_id: String, tab_id: String, view: String },
    /// 关闭文件（×：摘下文件、标签保留回未打开态）。
    /// × = 关文件留标签（kind = "note" / "table" / "canvas"，只关对应面）。
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
    Ui(ui::UiMsg<Message>),
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
    /// 托盘选择（插件条目回发 `host:tray.click`，宿主条目由壳处理）。
    Tray(tray::TrayEvent),
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
    /// 插件请求运行时改一个已打开窗口的属性（尺寸 / 置顶 / 标题）。
    PluginWindowUpdate {
        request_id: u64,
        plugin_id: String,
        params: atelyx_core::plugin_runtime::WindowUpdateParams,
    },
    /// 插件请求重新显示一个因失焦被隐藏的窗口。
    PluginWindowShow { request_id: u64, plugin_id: String, window_key: String },
    /// 插件请求最小化自己的一个窗口。
    PluginWindowMinimize { request_id: u64, plugin_id: String, window_key: String },
    /// 插件请求切换自己打开窗口的最大化态。
    PluginWindowToggleMaximize { request_id: u64, plugin_id: String, window_key: String },
    /// 插件请求拖动承载某面的窗口（自绘标题栏）。
    PluginWindowBeginDrag { request_id: u64, plugin_id: String, surface_id: String },
    /// 插件发起一次文件拖拽（ghost、落点与自动展开都在壳）。
    PluginDragStart {
        request_id: u64,
        plugin_id: String,
        params: atelyx_core::plugin_runtime::DragStartParams,
    },
    /// 插件面上报落点候选。
    PluginDragHover {
        request_id: u64,
        params: atelyx_core::plugin_runtime::DragHoverParams,
    },
    /// 插件结束拖拽会话。
    PluginDragEnd {
        request_id: u64,
        params: atelyx_core::plugin_runtime::DragEndParams,
    },
    /// 插件请求弹出一个宿主菜单（`plugin_id` 记下发起来源，关闭时按它回传选中项）。
    PluginMenuOpen {
        request_id: u64,
        plugin_id: String,
        params: atelyx_core::plugin_runtime::MenuOpenParams,
    },
    /// 插件请求打开文件夹选择器作为仓库（工具条「打开文件夹」入口）。
    PluginPickVault { request_id: u64 },

    /// 插件请求打开宿主取色器。
    PluginPickColor {
        request_id: u64,
        params: atelyx_core::plugin_runtime::PickColorParams,
    },
    /// 宿主菜单关闭：回传选中项给发起插件。
    PluginMenuClosed { selected: Option<String> },
    /// 插件菜单搜索输入。
    PluginMenuQuery(String),
    /// 整面帧上报自己的窗口坐标原点（布局变化时发）：窗口级浮层按它平移面本地坐标。
    PluginSurfaceOrigin {
        window: window::Id,
        plugin_id: String,
        surface_id: String,
        origin: Point,
    },
    /// 整面帧的焦点请求 / 释放。
    PluginFrameFocus {
        window: window::Id,
        plugin_id: String,
        surface_id: String,
        focused: bool,
    },
    /// 帧节拍：给声明了帧率的整面帧发一条帧通知（只在有动画面时订阅）。
    PluginFrameTick,
    /// 一拍帧通知已写完（复位在飞标记）。
    PluginFramePumped,
    /// 退出接管（常驻退出入口 / Esc）。
    PluginExitTakeover(window::Id),
    /// 窗口获得焦点（接管请求作用域 = 最近聚焦窗口）。
    WindowFocused(window::Id),
    /// 打开弹层（几何规格 + 内容形态由 layout_view 定义）。
    PopupOpen(window::Id, PopupOpen),
    /// 仓库文件总数（打开仓库后异步统计；gen 守卫）。
    FileCount(u64, Result<usize, String>),
    /// 全局配置落盘回执（失败提示，不回滚内存态）。
    GlobalSaved(Result<(), String>),
    /// 启动期全局配置读错误提示。
    StartupNotice(String),
    /// 文件操作落地（gen 守卫 + 结果通知 + 刷新子项 + 打开文件跟随）。
    OpDone(u64, String, Result<String, String>, FollowUp),

    /// 行内输入行被按下（重命名失焦提交的「点在输入行内」标志）。
    RenamePressInside,
    /// 分割把手键盘调整（上次拖动过的把手；horizontal + 正向）。
    DividerAdjust(bool, bool),
    /// 保存失败徽标的关闭。
    SaveErrorDismiss,
    /// 协作空间消息（账号 / 空间切换 / 空间保存回执）。
    Space(SpaceMsg),
    /// 文件面板云钮：打开协作空间菜单（未登录先弹登录层）。
    SpaceMenuOpen,
    /// 登录层切换注册 / 登录模式（弹层保持打开）。
    SpaceLoginToggle(bool),
    /// 设置-通用：界面字号草稿输入（数字过滤；合法档位实时生效）。
    FontSizeInput(String),
    /// 设置-通用：字号提交（非法草稿回退显示当前生效值）。
    FontSizeCommit,
    /// 字体选择弹层：过滤草稿（逐键收窄族清单）。
    FontPickerQuery(String),
    /// 设置-通用：界面字体族选定（None = 跟随系统）；立即生效并落盘。
    InterfaceFontSet(Option<String>),
    /// 设置-通用：自动恢复上次打开文件开关。
    AutoRestoreToggle,
    /// 设置-通用：自动检查更新开关（启动与运行期轮询的开关源）。
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

/// 文件树拖拽悬停区：进出配对凭它区分同目标的相邻落点区（三处根落点区都指向仓库根）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileDropKey {
    /// 文件树行（值为相对路径）。
    Row(String),
    /// 行的侧缝（值为该行相对路径 + 左右侧；缝隙指向仓库根）。
    Gutter { rel: String, right: bool },
    /// 激活仓库行。
    Vault,
    /// 激活协作空间行。
    Space,
    /// 列表下方空白落点区。
    Bottom,
}

/// 文件操作成功后对打开中笔记的跟随语义。
#[derive(Debug, Clone)]
pub enum FollowUp {
    None,
    /// 新建文件 / 文件夹 / 副本：落点供 `vault:file.changed` 广播。
    Created { rel: String },
    /// 改名/移动：打开中笔记按前缀跟随；`rewritten` = 引用同步代写的笔记（打开中需重读）；
    /// `announce` = 成功出提示（移动要回执，改名由行内输入自证）。
    Renamed { from: String, to: String, rewritten: Vec<String>, announce: bool },
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

/// 行内重命名输入挂焦点并全选：输入行随本帧视图重建挂载，操作在重建后的树上执行；
/// 进入即整体替换，避免在原名末尾追加。
fn rename_focus_task() -> Task<Message> {
    let id = iced::widget::Id::new(ui::controls::INLINE_RENAME_INPUT);
    iced::widget::operation::focus(id.clone()).chain(iced::widget::operation::select_all(id))
}

/// 在系统文件管理器中定位路径：目录 = 打开该目录，文件 = 打开所在目录并选中该项。
/// Linux 无统一选中语义，一律打开所在目录。
fn reveal_in_file_manager(path: &Path, is_dir: bool) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut cmd = std::process::Command::new("explorer");
        if is_dir {
            cmd.arg(path);
        } else {
            // explorer 自行解析命令行：`/select,<路径>` 必须作为一条原始参数传入
            cmd.raw_arg(format!("/select,\"{}\"", path.display()));
        }
        let _ = cmd.spawn();
    }
    #[cfg(not(windows))]
    {
        let target = if is_dir { path } else { path.parent().unwrap_or(path) };
        let _ = std::process::Command::new("xdg-open").arg(target).spawn();
    }
}

/// 拖拽落点拒绝原因（驱动 ghost 文案）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropReject {
    /// 目标不是目录（文件行）。
    NotADir,
    /// 目标就是源目录本身：落下去是原地不动。
    SameDir,
    /// 目标是自身 / 自身子树 / 自身祖先。
    SelfTree,
}

/// 落点被拒的可读原因（与壳内拖拽 ghost 的提示同一套文案）。
fn reject_text(why: DropReject) -> &'static str {
    match why {
        DropReject::NotADir => "只能放进文件夹",
        DropReject::SameDir => "已在此目录",
        DropReject::SelfTree => "不能放进自身",
    }
}

/// 拖拽落点判定：`target`（相对路径，`""` = 仓库根）能否接收当前拖拽会话。
/// 目录行与根落点区可收；文件行不接受拖入，自身 / 自身子树 / 自身祖先 / 源目录本身
/// 落下去都是原地 no-op。
fn file_drop_check(
    from: &str,
    from_is_dir: bool,
    target: &str,
    target_is_dir: bool,
) -> Result<(), DropReject> {
    if !target_is_dir {
        return Err(DropReject::NotADir);
    }
    if parent_of(from) == target {
        return Err(DropReject::SameDir);
    }
    if target == from
        || (from_is_dir
            && (target.starts_with(&format!("{from}/")) || from.starts_with(&format!("{target}/"))))
    {
        return Err(DropReject::SelfTree);
    }
    Ok(())
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
    /// 画布（.atlx，初始空画布）。
    Canvas,
    /// 插件领域文档（扩展名与展示标签取自插件注册）。
    Doc { extension: String, label: String },
}

impl DraftKind {
    /// 该类条目的扩展名（文件夹为 None）。
    fn extension(&self) -> Option<&str> {
        match self {
            DraftKind::Note => Some("md"),
            DraftKind::Table => Some("atb"),
            DraftKind::Canvas => Some("atlx"),
            DraftKind::Doc { extension, .. } => Some(extension.as_str()),
            DraftKind::Folder => None,
        }
    }
}

/// 草稿提交名净化：剥掉手打的同名后缀（新建表格输入「表.atb」不落成「表.atb.atb」）。
fn draft_name(kind: &DraftKind, input: &str) -> Option<String> {
    vault::draft_file_name(input, kind.extension())
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

/// 容器移出最近列表的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContainerRemoval {
    /// 从最近清单里删掉了。
    Removed,
    /// 本就不在最近清单（账号空间随时可再取回），列表展示不受影响。
    NotListed,
}

/// flush 放行后继续的动作（挂起输入已落盘才可执行，否则动作会丢失未保存内容或在途写打到旧路径）。
#[derive(Debug, Clone)]
enum PendingAfterFlush {
    /// 打开仓库内文件；`mode` = 打开意图（恢复只挂已有视图，不新建标签）。
    OpenFile { path: String, mode: OpenMode },
    PickVault,
    /// 从最近仓库列表进入指定仓库（from_boot 区分启动进入与运行中切换）。
    SelectVault { path: PathBuf, from_boot: bool },
    CloseMain(window::Id),
    /// 关文件留标签（kind = "note" / "table" / "canvas"，只关对应面）。
    CloseFileTab(String),
    /// 仓库内笔记链接（已解码；能进 flush 的只有 Path/Wiki/Create 形态，外链直接打开）。
    OpenLink(String),
    /// 文件树拖拽移动（挂起输入先落盘，防代写被未落盘内容回写覆盖）。
    MoveEntry { from: String, is_dir: bool, to_dir: String },
    RenameFile(RenameState),
    /// 插件发起的仓库删除（挂起输入先落盘，与文件树删除同一口径）。
    PluginVaultRemove { request_id: u64, path: String },
    /// 插件发起的仓库改名/移动（引用同步代写链接文件，挂起输入先落盘防代写被回写覆盖）。
    PluginVaultRename { request_id: u64, from: String, to: String },
    /// 从空间列表进入指定协作空间（会话恢复可能弹登录层）。
    SelectSpace { server_url: String, space_id: String, name: String },
    /// 安装就绪的更新包（挂起输入先落盘，安装器替换文件不丢未保存内容）。
    InstallUpdate,
}

/// 启动自动恢复要打开的文件：笔记 / 表格 / 画布各按上次路径发起一次，全部用
/// [`OpenMode::Restore`]（只把文件挂到已有视图，不新建标签、不改布局）。
/// 运行中切换容器（`from_boot == false`）不恢复；「自动恢复」关闭时不产生请求。
pub(crate) fn restore_open_requests(
    ui: &AppUiState,
    from_boot: bool,
    auto_restore: bool,
) -> Vec<(String, OpenMode)> {
    if !from_boot || !auto_restore {
        return Vec::new();
    }
    [
        ui.last_note_file.clone(),
        ui.last_table_file.clone(),
        ui.last_canvas_file.clone(),
    ]
    .into_iter()
    .flatten()
    .map(|rel| (rel, OpenMode::Restore))
    .collect()
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
/// 内容面回执尽量复用既有消息（OpDone / VaultMetaLoaded 等，gen 守卫同源）。
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
    /// 空间文档保存回执（rel = 发起时文件；frozen = 发起时冻结的正文，Ok 携带服务端合流后的最终全文）。
    DocSaved {
        gen: u64,
        rel: String,
        frozen: String,
        reason: SaveReason,
        result: Result<String, String>,
    },
    /// 文件夹确认删除（needsConfirm 二段式的 force 步）。
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

impl ui::UiHost for Message {
    fn ui(msg: ui::UiMsg<Self>) -> Self {
        Message::Ui(msg)
    }
}

/// 图片载入失败记账：失败次数与下次可重试时刻。
///
/// 载入失败的常见原因是「内容还没就绪」——插件刚提交缩略图、异步写完的文件尚未落盘、外链
/// 短暂不可达。一次失败即本会话放弃会让占位永久空白，故退避重试（1s / 2s，共三次尝试），
/// 用尽才放弃；放弃后的记项仍在表里，重开文档或切仓库清表时才重来。
#[derive(Clone, Copy, Debug)]
pub(crate) struct ImageRetry {
    attempts: u32,
    retry_at: Instant,
}

impl ImageRetry {
    /// 尝试次数上限（含首次）：用尽即本会话放弃，占位回落空白。
    const MAX_ATTEMPTS: u32 = 3;

    /// 第 `attempts` 次失败后的退避：1s、2s。上限与 [`ImageRetry::MAX_ATTEMPTS`] 对齐，
    /// 指数只在这里出现一次，不再各自写一遍。
    fn backoff(attempts: u32) -> std::time::Duration {
        let shift = attempts.saturating_sub(1).min(Self::MAX_ATTEMPTS - 1);
        std::time::Duration::from_millis(1000u64 << shift)
    }

    fn exhausted(&self) -> bool {
        self.attempts >= Self::MAX_ATTEMPTS
    }
}

/// 滚轮行增量折算成像素的系数（插件帧输入与自绘视图同口径）。
pub(crate) const WHEEL_LINE_PX: f32 = 40.0;

/// 插件面图片缓存键 → 已解码图柄。
pub type ImageHandles = HashMap<String, iced::widget::image::Handle>;

/// 图片字节的固有像素尺寸（解码失败 = 0×0，调用方按「尺寸未知」处理）。
pub(crate) fn image_dimensions(bytes: &[u8]) -> (u32, u32) {
    image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()
        .and_then(|r| r.into_dimensions().ok())
        .unwrap_or((0, 0))
}

pub struct App {
    /// 布局状态真源（core 模型；磁盘格式由 schema 版本标识）。
    pub ui: AppUiState,
    /// 唯一注册表（视图 / 服务 / 文档类型；选择器、标签与设置页只读它）。
    /// 启动装配期与投影刷新时整体重建，运行期只读。
    pub registry: crate::registry::Registry,
    vault: Arc<vault::VaultState>,
    /// 本轮 flush 是否还需画布插件确认：插件脏标记异步到达，打开画布时一律先落盘再放行动作。
    pub(crate) canvas_flush_pending: bool,
    /// flush 放行后待续的动作（笔记 / 表格 / 画布任一脏时挂起，全部落盘后执行）。
    pub(crate) flush_pending: Option<PendingAfterFlush>,
    /// 插件声明的面板头部状态（按面板视图 id）：宿主按它渲染那一行工具条。
    pub(crate) panel_headers: std::collections::HashMap<String, PanelHeaderEntry>,
    /// 面板内容槽的当前视图 id（`None` = 该面板自己的默认视图）。
    pub(crate) panel_view: std::collections::HashMap<String, Option<String>>,
    /// 在途的面板落盘请求（等插件回 `panel.flushDone` 才结算并放行挂起动作）。
    pub(crate) pending_panel_flush: Option<PendingPanelFlush>,
    /// 历史回滚待结算：内容已交回插件，等它的落盘回执后才记 restore 版本并报成功。
    pub(crate) pending_table_restore: Option<PendingTableRestore>,
    pub(crate) pending_canvas_restore: Option<PendingCanvasRestore>,
    /// 画布视图是否展示在前台（后台标签页不算）；变化时才广播给插件。
    pub(crate) canvas_displayed: bool,
    /// 当前展开的模态插件浮层（插件 id + 面 id）：其覆盖区的点击不再落到宿主控件。
    pub(crate) plugin_modal: Option<(String, String)>,
    panel_flush_seq: u64,
    /// 键盘修饰态（Ctrl+滚轮缩放判定源）。
    pub mods: keyboard::Modifiers,
    /// 历史面板当前作用的文件类型（载入笔记/表格/画布时置位；读/回滚按它分派）。
    pub history_kind: atelyx_core::history::HistoryKind,
    pub theme: Theme,
    /// 应用内通知（右下角；自动消失）。
    pub(crate) notifications: Vec<Notification<Message>>,
    notify_seq: u64,
    /// 悬停提示（图标按钮；显现/收起由心跳按光标-锚点距离判定；
    /// 事件 enter 仅负责启动——批量事件中 exit 会被捕获状态吞掉，不可靠）。
    pub tooltip: Option<(window::Id, String, Instant, bool, Option<Instant>, Point)>,
    /// 触发钮半边长（提示锚定其下缘；菜单项 = 半行高）。
    pub tooltip_half: Option<f32>,
    /// 重名自动加序号的提醒条（文案 + 出现时刻；3s 自动消失）。
    pub rename_notice: Option<layout_view::RenameNotice>,
    /// 设置左栏折叠态。
    pub settings_collapsed: bool,
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
    /// 系统托盘（插件条目 + 宿主条目；点击经订阅回到 update）。
    pub tray: tray::Service,
    /// 设置-插件页状态（列表缓存 / 详情 / 卸载确认 / 安装在途）。
    pub plugins_state: settings_view::plugins::PluginsState,
    /// 已注册贡献投影（运行时事件后刷新；菜单、标签与面板渲染都读它）。
    pub plugin_contributions: Vec<atelyx_core::plugin_runtime::Contribution>,
    /// 当前呈现布局里已挂的插件面（布局变化时重算并差额通知插件）。
    pub plugin_surfaces: HashSet<(String, String)>,
    /// 已挂载的插件设置面（注册即挂载，贡献变化时差额通知插件）。
    pub plugin_settings_surfaces: HashSet<(String, String)>,
    /// 已挂载的插件表格视图面（表格面板内当前选中的视图；换视图 / 关表格即差额通知插件）。
    pub plugin_table_surfaces: HashSet<(String, String)>,
    /// 最近一次广播 `editor:open.changed` 时的打开文件集合（身份差分用）。
    pub open_files_event: Vec<String>,
    /// 各面的最新整面帧（键 = 插件 id + 面 id）。
    pub plugin_frames:
        HashMap<(String, String), std::sync::Arc<atelyx_core::plugin_runtime::SurfaceFrame>>,
    /// 整面帧的键盘焦点（全局单槽：OS 只给聚焦窗口发键盘，窗口失焦即清除）。
    pub plugin_frame_focus: Option<plugin_actions::FrameFocus>,
    /// 帧拍是否还在飞（同一时刻只推一拍，慢插件不积压帧任务）。
    pub plugin_frame_pumping: bool,
    /// 各窗口的接管态（键 = OS 窗口 id；布局真源不动，退出即恢复）。
    pub plugin_takeovers: HashMap<window::Id, plugin_actions::TakeoverActive>,
    /// 插件窗口记账（键 = OS 窗口 id；随窗口关闭 / 插件停用收尾）。
    pub plugin_windows: HashMap<window::Id, plugin_actions::PluginWindowActive>,
    /// 打开中的插件文档实例（单实例；切换文件即换实例）。
    pub doc_open: Option<plugin_actions::DocOpenInstance>,
    /// 在途文档读盘的归属守卫（身份 + 序号）：失效的读盘结果不得回填到已切换的会话。
    doc_open_guard: doc_session::DocOpenGuard,
    /// 最近聚焦的窗口（插件接管请求的作用域）。
    pub focused_window: Option<window::Id>,
    /// 市场索引缓存落盘路径（应用数据目录不可用时为 None：不缓存，市场仅内存展示）。
    pub market_cache_path: Option<PathBuf>,
    /// 应用级全局配置（最近仓库列表；落盘 global.json）。
    pub global: global_config::GlobalConfig,
    /// 设置-通用：界面字号草稿（None = 显示当前生效值）。
    pub font_size_draft: Option<String>,
    /// 字体选择弹层的过滤草稿（打开弹层时清空）。
    pub font_picker_query: String,
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
    /// 在途检查标志：定时轮询与手动入口共用，避免并发发起。
    update_check_in_flight: bool,
    /// 本次运行已跳过的版本（自动检查不再提示；手动检查不受限）。
    update_skipped: Option<String>,
    /// 本次运行已提示过「无安装包」的版本（自动检查同一版本只提示一次）。
    update_no_asset_notice: Option<String>,
    /// 更新流程常驻通知的 id（通知 × 据此识别为跳过）。
    update_notice_id: Option<u64>,
    /// 最近一次发起的切仓目标：flush 回放时只有与它一致才执行，防快速点选落到非最后目标。
    vault_switch_target: Option<PathBuf>,
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
    pub space_doc_flushed: Option<String>,
    /// 空间笔记保存在途标志（回执即清）：空间保存无写队列可依赖，并发两笔会乱序落地覆盖服务端。
    space_doc_save_inflight: bool,
    /// 在途保存期间收到 Flush 请求：回执后补一笔串行收尾（关窗/切文件的脏拦截依赖它）。
    space_doc_save_flush_queued: bool,
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
    /// 文本文档协作文档绑定（空间模式且打开文档时）。
    pub doc_collab: Option<atelyx_core::note_collab::NoteCollab>,
    /// 协作绑定当前挂的文件（None = 未挂）。
    pub doc_collab_file: Option<String>,
    /// 文档在场通道（本端光标/选区上行与远端光标条目）。
    pub doc_presence: Option<doc_presence::DocPresenceChannel>,
    /// 远端成员光标（在场通道状态换算为 doc_text 字节偏移；供 `doc:presence` 广播）。
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
    /// note 正文真相（与磁盘逐字节一致；插件笔记面与画布 / 表格的文本数据源）。
    pub doc_text: String,
    pub doc_dirty: bool,
    /// 上次保存失败原因（None = 无；保存成功或再次编辑即清除）。
    pub doc_save_error: Option<String>,
    /// 自动保存期限（最近一次内容变更 + 500ms；保存落地或到期触发时清空）。
    autosave_deadline: Option<Instant>,
    /// 自动保存定时器在途标志（单定时器链：到期检查期限，被推迟则续排剩余时长）。
    autosave_timer: bool,
    /// 文档正文由插件面承载（壳内不再渲染文档视图；正文仍在本 `doc_text`）。
    pub doc_active: bool,
    /// 插件文档面上报的本端选区（全文字节偏移；`None` = 无光标）。
    pub doc_selection: Option<(usize, usize)>,
    /// 插件最后提交并被宿主接受的正文（三方合并的共同祖先；远端合入不回写它）。
    /// `None` = 与 [`Self::doc_text`] 相同：稳定态不另存一份全文。
    pub doc_base: Option<String>,
    /// 文档插件尚未就绪时挂起的打开请求（启动自动恢复 / 用户抢先点开）；
    /// 连同「打开意图」一起挂起，就绪后按原样重试。
    pending_doc_open: Option<(String, OpenMode)>,
    /// 笔记 / 画布 / 插件图片句柄缓存（key = src 原文；切仓库清空，在途与失败集合防重复发起）。
    pub note_images: ImageHandles,
    /// 图片像素尺寸（key 同 `note_images`）：自绘贴片按 contain / cover 摆放要用原图长宽。
    pub note_image_sizes: HashMap<String, (u32, u32)>,

    /// 图片句柄代数：句柄入表或整表清空即自增，自绘贴片据此作废已缓存的几何。
    pub note_image_epoch: u64,
    note_images_inflight: HashSet<String>,
    note_images_failed: HashMap<String, ImageRetry>,
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
    /// 上次布局 tab 按下（tab id + 时刻；双击进入改名判定）。
    layout_tab_last_press: Option<(String, Instant)>,
    /// 标题栏布局条横向滚动偏移（拖拽命中几何与渲染对齐）。
    titlebar_tab_scroll_x: f32,
    /// 各面板标签条横向滚动偏移（同上；key = 面板 id）。
    panel_tab_scroll_x: HashMap<String, f32>,
    /// 全仓库标签词汇表（tags 候选；打开仓库与载入笔记后刷新）。
    pub tag_vocab: Vec<vault::TagRow>,
    /// 全仓库文件清单（`vault.files` 的数据源；树序，全部文件类型）。
    pub note_files: Vec<(String, String)>,
    /// 历史弹层/上下文的版本列表（打开弹层与每次保存后刷新；旧→新序）。
    pub history: Vec<atelyx_core::history::HistoryVersion>,
    /// 列表加载世代（回滚守卫：容器/仓库切换后列表过期）。
    pub history_gen: u64,
    /// 弹层版本列表加载中。
    pub history_loading: bool,
    /// 弹层展开预览的版本 seq（互斥；None = 全收起）。
    pub history_expanded: Option<u64>,
    /// 展开版本的预览内容缓存（按 seq 绑定；展开时算一次，避免每帧重算 diff）。
    pub history_preview: Option<history_browser::HistoryPreview>,
    /// 弹层回滚二段确认的版本 seq。
    pub history_confirm: Option<u64>,
    /// 回滚已发起、等待保存链稳定落盘的一次性标志（成功回执消费并提示，失败/切容器清除）。
    pub(crate) history_restore_pending: bool,
    /// 仓库历史聚合缓存（None = 未加载/已失效；Err = 加载失败待重试）。`history.*` 查询共用这份。
    pub repo_history: Option<Result<repo_history::RepoHistory, String>>,
    /// 仓库历史聚合在途。
    pub repo_history_loading: bool,
    /// 聚合缓存世代：每次丢弃缓存 +1，在途扫描按它作废（不把失效前的聚合写回）。
    pub repo_history_gen: u64,
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
    /// 拖拽悬停中的落点（目标目录相对路径，`""` = 仓库根；None = 不在可落点上）。
    pub file_drag_target: Option<String>,
    /// 插件发起的拖拽会话归属（插件 id + 面标识）：落点状态按它回推给发起方。
    pub file_drag_surface: Option<(String, String)>,
    /// 插件菜单的归属插件与条目（宿主弹层渲染，选中后回传）。
    pub plugin_menu: Option<(String, atelyx_core::plugin_runtime::MenuOpenParams)>,
    /// 插件菜单搜索输入（菜单声明 `search` 时使用；每次打开清空）。
    pub plugin_menu_query: String,
    /// 窗口级浮层的面原点（窗口 / 插件 / 面 → 面左上角窗口坐标），由整面帧布局变化时上报。
    pub plugin_surface_origins: HashMap<(window::Id, String, String), Point>,
}

/// `doc:presence` 广播用的远端光标视图（字节偏移，UTF-16 已换算）。选区缺省 = 对端在场但
/// 光标不在文档内（未打开笔记 / 插件未上报选区），此时只参与协作者胶囊，不画光标。
#[derive(Debug, Clone)]
pub struct RemoteCursorView {
    pub name: String,
    pub color: String,
    pub anchor: Option<usize>,
    pub head: Option<usize>,
}

/// 文件树拖拽按下候选。
struct FilePress {
    window: window::Id,
    rel: String,
    is_dir: bool,
}

/// 文件树拖拽会话（ghost 跟随光标；释放落入文件夹行 / 根落点区即移动）。
pub struct FileDrag {
    pub from: String,
    pub is_dir: bool,
    /// ghost 文案（末段名）。
    pub label: String,
    /// 全局光标（源窗口位置 + 本地坐标）。
    pub current: Point,
    pub cursor_window: window::Id,
    /// 悬停中的不可落点（目标相对路径 + 拒绝原因）：只驱动提示，不参与释放判定。
    pub reject: Option<(String, DropReject)>,
    /// 悬停折叠目录的自动展开挂起（目录相对路径 + 进入时刻；由 UI 心跳到点展开）。
    pub spring: Option<(String, Instant)>,
    /// 当前悬停区（进出配对：只有同一区的离开才清除落点与提示）。
    pub hover: Option<FileDropKey>,
}

impl FileDrag {
    /// 该行当前是否为被拒落点（返回原因）。
    pub fn reject_at(&self, rel: &str) -> Option<DropReject> {
        self.reject.as_ref().filter(|(r, _)| r == rel).map(|(_, why)| *why)
    }
}

impl App {
    fn boot() -> (Self, Task<Message>) {
        let (main_os, open) = window::open(Settings {
            size: DEFAULT_WINDOW_SIZE,
            min_size: Some(MIN_WINDOW_SIZE),
            exit_on_close_request: false,
            decorations: false,
            icon: app_icon::window_icon(),
            ..Settings::default()
        });
        ui::icon::init();
        // 插件运行时：根 = 应用数据目录下 `plugins/`（数据目录不可用时回落系统临时目录，功能降级不崩溃）。
        // 宿主版本取应用版本：插件 `minHostVersion` 校验与握手自报都以产品版本为准（core 包版本不参与）。
        let plugins_root = app_data_dir()
            .map(|d| d.join("plugins"))
            .unwrap_or_else(|| std::env::temp_dir().join("atelyx-plugins"));
        let plugins = PluginRuntime::with_host_version(plugins_root, env!("CARGO_PKG_VERSION"));
        let (global, global_notice) = load_global_boot();
        // 市场索引缓存落应用数据目录根（与 plugins/ 平级；目录不可用 = 不缓存）
        let market_cache_path = app_data_dir().map(|d| d.join("market-cache.json"));
        // 界面基准字号随配置（缺字段走默认档；越界钳回合法区间）
        ui::palette::set_ui_base(global.font_size.clamp(12, 20));
        // 界面字体族随配置（未安装或空值 = 跟随系统 UI 字体）
        ui::set_family(global.font_family.as_deref());
        // 启动自动进入：最后激活身份优先，缺省回落最近仓库首位（路径失效则停留未激活态）
        let auto_vault: Option<PathBuf> = match &global.last_active {
            Some(LastActive::Vault { root }) => Some(PathBuf::from(root)),
            Some(LastActive::Space { .. }) => None,
            None => global.recent_vaults.first().map(|v| PathBuf::from(&v.root)),
        };
        let mut boot = open
            .map(Message::WindowOpened)
            .chain(window::position(main_os).map(move |p| Message::PositionKnown(main_os, p)));
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
        // 启动静默检查更新（开关与开发构建口径由更新处理器统一判定）
        if global.auto_update {
            boot = boot.chain(Task::done(Message::Update(update::UpdateMsg::Check {
                manual: false,
            })));
        }
        let mut app = Self {
                ui: load_ui_state(),
                registry: crate::registry::Registry::default(),
                vault: Arc::new(vault::VaultState::default()),
                theme: ui::palette::app_theme(),
                notifications: Vec::new(),
                notify_seq: 0,
                tooltip: None,
                tooltip_half: None,
                rename_notice: None,
                settings_collapsed: false,
                cursors: HashMap::new(),
                popup: None,
                popup_pane: 0,
                arm_commit: None,
                settings_open: false,
                settings_tab: "general".into(),
                shortcut_recording: None,
                plugins: plugins.clone(),
                tray: tray::Service::new(),
                plugins_state: settings_view::plugins::PluginsState::default(),
                plugin_contributions: Vec::new(),
                plugin_surfaces: HashSet::new(),
                plugin_settings_surfaces: HashSet::new(),
                plugin_table_surfaces: HashSet::new(),
                open_files_event: Vec::new(),
                plugin_frames: HashMap::new(),
                plugin_frame_focus: None,
                plugin_frame_pumping: false,
                plugin_takeovers: HashMap::new(),
                plugin_windows: HashMap::new(),
                doc_open: None,
                doc_open_guard: doc_session::DocOpenGuard::default(),
                focused_window: None,
                market_cache_path,
                ai_sub_tab: "providers".into(),
                chat: chat_actions::ChatState::default(),
                repo_config: vault::VaultConfig::default(),
                agents: Vec::new(),
                ai: settings_view::ai::AiState::default(),
                global,
                font_size_draft: None,
                font_picker_query: String::new(),
                autostart_enabled: None,
                update: update::UpdateState::Idle,
                update_cancel: None,
                update_download_dir: update_download_dir(),
                update_progress: Arc::new(Mutex::new((0, 0))),
                update_manual_check: false,
                update_check_in_flight: false,
                update_skipped: None,
                update_no_asset_notice: None,
                update_notice_id: None,
                vault_switch_target: None,
                vault_root: None,
                file_count: None,
                main_id: None,
                detached_os: HashMap::new(),
                os_shown: HashMap::new(),
                children: HashMap::new(),
                doc_text: String::new(),
                doc_dirty: false,
                doc_save_error: None,
                autosave_deadline: None,
                autosave_timer: false,
                doc_active: false,
                doc_selection: None,
                doc_base: None,
                pending_doc_open: None,
                note_images: ImageHandles::new(),
                note_image_sizes: HashMap::new(),
                note_image_epoch: 0,


                note_images_inflight: HashSet::new(),
                note_images_failed: HashMap::new(),
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
                file_drag_surface: None,
                plugin_menu: None,
                plugin_menu_query: String::new(),
                plugin_surface_origins: HashMap::new(),
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
                layout_tab_last_press: None,
                titlebar_tab_scroll_x: 0.0,
                panel_tab_scroll_x: HashMap::new(),
                tag_vocab: Vec::new(),
                note_files: Vec::new(),
                history: Vec::new(),
                history_gen: 0,
                history_restore_pending: false,
                history_loading: false,
                history_expanded: None,
                history_preview: None,
                history_confirm: None,
                repo_history: None,
                repo_history_loading: false,
                repo_history_gen: 0,
                recent_checked_at: None,
                rebuild_links: settings_view::editor::RebuildLinksState::Idle,
                history_kind: atelyx_core::history::HistoryKind::Note,
                canvas_flush_pending: false,
                flush_pending: None,
                panel_headers: std::collections::HashMap::new(),
                panel_view: std::collections::HashMap::new(),
                pending_panel_flush: None,
                pending_table_restore: None,
            pending_canvas_restore: None,
                canvas_displayed: false,
                plugin_modal: None,
                panel_flush_seq: 0,
                mods: keyboard::Modifiers::default(),
                repo_kind: RepoKind::Local,
                space_tree: None,
                space_list: Vec::new(),
                space_form: SpaceForm::default(),
                space_busy: false,
                space_pending: None,
                session_token: None,
                space_doc_flushed: None,
                space_doc_save_inflight: false,
                space_doc_save_flush_queued: false,
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
                doc_collab: None,
                doc_collab_file: None,
                doc_presence: None,
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
        // 随包预置插件：发行包内的预置目录补进用户插件根（首装启用；版本更新或内容变化即替换，
        // 用户卸过的不装回）。这里只补内容，启动交给随后的 scan + start_enabled
        if let Some(data) = app_data_dir() {
            if let Some(bundled) = bundled_presets_dir() {
                match app.plugins.sync_presets(&bundled, &data.join("preset-plugins.json")) {
                    Ok(report) => {
                        for (id, reason) in report.failed {
                            app.notify(NoticeLevel::Warning, format!("预置插件 {id} 同步失败：{reason}"));
                        }
                    }
                    Err(e) => app.notify(NoticeLevel::Warning, format!("预置插件同步失败：{e}")),
                }
            }
        }
        // 插件运行时：扫描已安装清单并启动已启用插件；布局里现存的插件面就绪即通知
        app.plugins.scan();
        app.plugins.start_enabled();
        // 内置插件的停用态要落到能力总线：停用插件的内置服务不进目录（外部候选可接手）
        for plugin in app.ui.disabled_plugins.clone() {
            app.plugins.set_builtin_owner_enabled(&plugin, false);
        }
        plugin_actions::sync_surfaces(&mut app);
        plugin_actions::sync_canvas_display(&mut app);
        plugin_actions::rebuild_registry(&mut app);
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
    /// 动作表以分配到的通知 id 合成（[稍后] 等关闭动作需要它）。
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
        self.update_notice_id = Some(id);
    }

    /// 移除更新流程的常驻通知（置入新常驻条前同样先走这里）。
    fn dismiss_update_notice(&mut self) {
        self.notifications.retain(|n| !n.persistent);
        self.update_notice_id = None;
    }

    /// 世代内的光标位置（按下事件不带坐标，命中判定用最近位置）。
    pub(crate) fn cursor_for(&self, window: window::Id) -> Point {
        self.cursors.get(&window).copied().unwrap_or(Point::ORIGIN)
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        let task = self.handle(message);
        // 表格视图面挂载差分：换视图 / 开合 / 切换表格后差额通知插件（面集变了才重投影）
        let task = if plugin_actions::sync_table_surfaces(self) {
            task.chain(plugin_actions::refresh_projection(self))
        } else {
            task
        };
        self.notify_open_files();
        self.chain_autosave_timer(task)
    }

    /// 打开文件集合变化（打开 / 切换 / 关闭笔记 / 表格 / 画布）即广播 `editor:open.changed`。
    fn notify_open_files(&mut self) {
        let current = space_actions::open_file_paths(self);
        if current == self.open_files_event {
            return;
        }
        self.open_files_event = current;
        self.plugins.on_open_files_changed();
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
        // 整面帧声明 keys 时连命令组合也归它：宿主的键盘派生消息与快捷键动作同样让位
        if plugin_actions::frame_takes_keyboard(self) && is_frame_keyboard_message(&message) {
            return Task::none();
        }
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
                plugin_actions::release_plugin_focus(self, id);
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
                    let takeover_task = plugin_actions::exit_takeover(self, id);
                    if self.focused_window == Some(id) {
                        self.focused_window = None;
                    }
                    // 光标记录随窗口消失；该窗在途悬停提示立即清除
                    // （cursors 缺失会让收起判定恒 false，提示与心跳订阅悬挂）
                    self.cursors.remove(&id);
                    self.plugin_surface_origins.retain(|(w, _, _), _| *w != id);
                    if self.tooltip.as_ref().is_some_and(|(w, _, _, _, _, _)| *w == id) {
                        self.tooltip = None;
                    }
                    takeover_task
                }
            }
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
            Message::Save => {
                // Ctrl+S：文档 / 画布各自手动保存（无打开内容 = 提示）
                let mut task = Task::none();
                if self.ui.last_note_file.is_some() {
                    task = task.chain(self.submit_save(SaveReason::Manual));
                }
                // 表格与画布内容都在插件：手动保存走面板 flush（插件侧会先提交在途草稿）
                let panel_open = self.ui.last_table_file.is_some() || self.ui.last_canvas_file.is_some();
                if panel_open {
                    task = task.chain(self.request_panel_flush(PanelFlushPurpose::Gate));
                }
                if self.ui.last_note_file.is_none() && !panel_open {
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
                    if self.doc_dirty && self.ui.last_note_file.is_some() {
                        task = task.chain(self.submit_save(SaveReason::AutoSave));
                    }
                    return task;
                }
                Task::none()
            }
            Message::Saved(gen, rel, saved, reason, Ok(())) => {
                if gen != self.epoch() {
                    return Task::none();
                }
                self.plugins.on_file_changed(&rel, FileChangeKind::Updated, None);
                self.doc_save_error = None;
                let proceed = match reason {
                    SaveReason::Flush => {
                        // 保存发起时冻结的内容与当前一致才算落盘；期间又有输入则再收一轮
                        if self.doc_text == saved {
                            self.doc_dirty = false;
                            self.settle_history_restore(true);
                            self.after_any_flush()
                        } else {
                            self.submit_save(SaveReason::Flush)
                        }
                    }
                    SaveReason::Manual => {
                        if self.doc_text == saved {
                            self.doc_dirty = false;
                            self.settle_history_restore(true);
                            self.notify(NoticeLevel::Success, "已保存");
                        } else {
                            self.notify(NoticeLevel::Info, "保存完成，期间有新修改");
                        }
                        Task::none()
                    }
                    SaveReason::AutoSave => {
                        if self.doc_text == saved {
                            self.doc_dirty = false;
                            self.settle_history_restore(true);
                        }
                        Task::none()
                    }
                };
                // 落盘即记历史版本（edit；内容 no-op 由记录侧跳过），随后刷新版本列表
                if self.ui.last_note_file.as_deref() == Some(rel.as_str()) {
                    crate::doc_session::publish_state(self, None);
                }
                proceed.chain(self.record_doc_history(saved))
            }
            Message::Saved(_, rel, _, reason, Err(e)) => {
                self.settle_history_restore(false);
                // flush 被保存失败打断：动作不放行（放行会丢弃未落盘内容），挂起输入保留待重试
                if matches!(reason, SaveReason::Flush) {
                    self.notify(NoticeLevel::Error, format!("保存失败，操作未执行：{e}"));
                } else {
                    self.notify(NoticeLevel::Error, format!("保存失败：{e}"));
                }
                self.doc_save_error = Some(e);
                if self.ui.last_note_file.as_deref() == Some(rel.as_str()) {
                    crate::doc_session::publish_state(self, None);
                }
                Task::none()
            }
            Message::CopyKey { key, mods } => self.on_key_command(key, mods),
            Message::TagVocabLoaded(gen, Ok(rows)) => {
                if gen == self.epoch() {
                    self.tag_vocab = rows;
                }
                Task::none()
            }
            Message::TagVocabLoaded(_, Err(_)) | Message::NoteFilesLoaded(_, Err(_)) => Task::none(),
            Message::NoteFilesLoaded(gen, Ok(files)) => {
                if gen == self.epoch() {
                    self.note_files = files;
                    self.plugins.on_files_changed();
                }
                Task::none()
            }
            Message::NavKey(horizontal, positive) => {
                self.handle(Message::DividerAdjust(horizontal, positive))
            }
            Message::HistoryLoaded(gen, kind, rel, mut versions) => {
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
                    atelyx_core::history::HistoryKind::Canvas => {
                        self.history_kind == atelyx_core::history::HistoryKind::Canvas
                            && self.ui.last_canvas_file.as_deref() == Some(rel.as_str())
                    }
                };
                if gen == self.epoch() && (popup_matches || ctx_matches) {
                    // 无摘要的版本（协作空间早前记录、回滚）在加载时按 kind 补规模描述：
                    // 列表逐帧渲染，摘要回落若留在渲染路径会每帧整解析快照
                    for v in versions.iter_mut() {
                        if v.summary.as_deref().unwrap_or_default().is_empty() {
                            v.summary = Some(history_browser::scope_line(kind, &v.content));
                        }
                    }
                    self.history = versions;
                    self.history_gen = gen;
                    self.history_loading = false;
                    // 保存后刷新列表：展开态的预览可能已随版本变化失效，按当前展开项重算；
                    // 展开项已不在新列表（剪枝）时一并收起，避免留孤儿展开态占预览高度
                    self.history_preview = self
                        .history_expanded
                        .and_then(|seq| history_browser::build_preview(self, seq));
                    if self.history_preview.is_none() {
                        self.history_expanded = None;
                    }
                }
                Task::none()
            }
            Message::History(m) => history_browser::on_msg(self, m),
            Message::RebuildLinks(m) => settings_view::editor::on_msg(self, m),
            Message::HistoryRecorded(gen, err) => {
                if let Some(e) = err {
                    self.notify(NoticeLevel::Error, format!("历史版本记录失败：{e}"));
                }
                // 侧文件有新版本：聚合缓存失效并通知订阅了历史分组的插件重取
                repo_history::invalidate(self);
                self.plugins.on_history_changed();
                if gen == self.epoch() {
                    self.reload_history()
                } else {
                    Task::none()
                }
            }
            Message::RecentPruned(gone) => {
                if !gone.is_empty() {
                    recent_files::prune(self, &gone);
                }
                Task::none()
            }
            Message::RepoHistoryLoaded(epoch, gen, result) => {
                repo_history::on_loaded(self, epoch, gen, result)
            }
            Message::OpenLink(href, form) => self.open_link(&href, form),
            Message::PanelFlushTimeout(token) => self.settle_panel_flush_timeout(token),
            Message::CanvasRestoreFlush => {
                if self.pending_canvas_restore.is_none() {
                    return Task::none();
                }
                // 已有别的 flush 在途：等它结算后再强制请画布落盘，不能把回滚判失败
                if self.pending_panel_flush.is_some() {
                    return Task::perform(
                        async move {
                            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                        },
                        |()| Message::CanvasRestoreFlush,
                    );
                }
                let task = self.request_canvas_flush(PanelFlushPurpose::RestoreNotice);
                if self.pending_panel_flush.is_none() {
                    // 画布插件没把回滚后的脏状态送到：不记版本、不报成功
                    self.pending_canvas_restore = None;
                    self.notify(NoticeLevel::Error, "回滚未落盘".to_string());
                }
                task
            }
            Message::TableRestoreFlush => {
                if self.pending_table_restore.is_none() {
                    return Task::none();
                }
                // 已有别的 flush 在途：等它结算后再强制请表格落盘，不能把回滚判失败
                if self.pending_panel_flush.is_some() {
                    return Task::perform(
                        async move {
                            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                        },
                        |()| Message::TableRestoreFlush,
                    );
                }
                let task = self.request_table_flush(PanelFlushPurpose::RestoreNotice);
                if self.pending_panel_flush.is_none() {
                    // 表格插件没把回滚后的脏状态送到：不记版本、不报成功
                    self.pending_table_restore = None;
                    self.notify(NoticeLevel::Error, "回滚未落盘".to_string());
                }
                task
            }
            Message::PluginPopupDismiss { window } => {
                let Some((plugin_id, surface_id)) = self.plugin_modal.clone() else {
                    return Task::none();
                };
                let point = self.cursors.get(&window).copied().unwrap_or(Point::ORIGIN);
                self.plugins.notify_frame_input(
                    &plugin_id,
                    &surface_id,
                    &atelyx_core::plugin_runtime::FrameInput::Outside {
                        x: point.x,
                        y: point.y,
                    },
                );
                Task::none()
            }
            Message::PanelViewMenu { window, target } => {
                let point = self.cursors.get(&window).copied().unwrap_or(Point::ORIGIN);
                self.popup = Some((
                    window,
                    crate::layout_view::PopupOpen {
                        point,
                        flip_y: None,
                        state: crate::layout_view::PopupState::PanelViewMenu { target },
                        min_width: 0.0,
                    },
                ));
                Task::none()
            }
            Message::PanelItemsMenu { window, target } => {
                let point = self.cursors.get(&window).copied().unwrap_or(Point::ORIGIN);
                self.popup = Some((
                    window,
                    crate::layout_view::PopupOpen {
                        point,
                        flip_y: None,
                        state: crate::layout_view::PopupState::PanelItemsMenu { target },
                        min_width: 0.0,
                    },
                ));
                Task::none()
            }
            Message::PanelHeaderAction { target, item } => {
                self.dismiss_popup();
                let Some(plugin_id) = self
                    .panel_headers
                    .get(&target)
                    .map(|entry| entry.plugin_id.clone())
                else {
                    return Task::none();
                };
                self.plugins.notify_plugin_event(
                    &plugin_id,
                    atelyx_core::plugin_runtime::event_name::PANEL_HEADER_ACTION,
                    &atelyx_core::plugin_runtime::PanelHeaderActionParams { target, item },
                );
                Task::none()
            }
            Message::PanelViewSelect { target, view } => {
                self.panel_view.insert(target, view);
                self.dismiss_popup();
                Task::none()
            }
            Message::ImageLoaded(gen, key, result) => {
                self.note_images_inflight.remove(&key);
                if gen != self.epoch() {
                    return Task::none();
                }
                match result {
                    Ok(image) => {
                        self.note_images_failed.remove(&key);
                        // 画布与图片贴片的 contain / cover 需要固有像素尺寸，装载点顺手记下
                        let (handle, size) = match image {
                            PluginImage::Bytes(bytes) => {
                                let size = image_dimensions(&bytes);
                                (iced::widget::image::Handle::from_bytes(bytes), size)
                            }
                            PluginImage::Thumb(thumb) => (
                                iced::widget::image::Handle::from_rgba(
                                    thumb.width,
                                    thumb.height,
                                    thumb.rgba,
                                ),
                                thumb.source,
                            ),
                        };
                        if size.0 > 0 && size.1 > 0 {
                            self.note_image_sizes.insert(key.clone(), size);
                        }
                        self.note_images.insert(key, handle);
                        self.note_image_epoch = self.note_image_epoch.wrapping_add(1);
                        Task::none()
                    }
                    // 失败按退避重排一次重试：内容可能只是还没就绪（插件刚落盘的缩略图、
                    // 异步写完的附件）。用尽次数后回落占位并停手，避免滚动反复发起。
                    Err(_) => {
                        let retry = match self.note_images_failed.get(&key) {
                            Some(previous) => ImageRetry {
                                attempts: previous.attempts + 1,
                                retry_at: Instant::now() + ImageRetry::backoff(previous.attempts + 1),
                            },
                            None => ImageRetry {
                                attempts: 1,
                                retry_at: Instant::now() + ImageRetry::backoff(1),
                            },
                        };
                        let wait = retry.retry_at.saturating_duration_since(Instant::now());
                        let exhausted = retry.exhausted();
                        self.note_images_failed.insert(key, retry);
                        if exhausted {
                            Task::none()
                        } else {
                            Task::perform(async move { tokio::time::sleep(wait).await }, move |_| {
                                Message::ImageRetryDue(gen)
                            })
                        }
                    }
                }
            }
            Message::ImageRetryDue(gen) => {
                if gen == self.epoch() {
                    crate::plugin_actions::ensure_images(self)
                } else {
                    Task::none()
                }
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
            Message::PromptNotesLoaded(gen, result) => {
                if gen == self.epoch() {
                    match result {
                        Ok(notes) => self.prompt_notes = notes,
                        Err(e) => self.notify(NoticeLevel::Error, format!("保存提示词标记失败：{e}")),
                    }
                }
                Task::none()
            }
            Message::ColorPicker(msg) => color_picker_actions::update(self, msg),
            Message::FolderColorsLoaded(gen, result) => {
                if gen == self.epoch() {
                    match result {
                        Ok(colors) => self.folder_colors = colors,
                        Err(e) => {
                            self.notify(NoticeLevel::Error, format!("保存文件夹颜色失败：{e}"))
                        }
                    }
                }
                Task::none()
            }
            Message::CollabColorSet(color) => {
                self.global.collab_color = Some(color);
                // 换色后立即刷新文档 / 画布在场身份，对端不必等到下一次选区事件
                space_actions::collab_after_caret(self);
                self.save_global_now();
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
            Message::Chat(msg) => {
                chat_actions::update(self, msg)
            }
            Message::Noop => Task::none(),
            Message::PluginDragStart { request_id, plugin_id, params } => {
                self.start_plugin_drag(request_id, plugin_id, params)
            }
            Message::PluginDragHover { request_id, params } => {
                self.plugin_drag_hover(request_id, params)
            }
            Message::PluginDragEnd { request_id, params } => {
                self.end_plugin_drag(request_id, params)
            }
            Message::PluginMenuOpen { request_id, plugin_id, params } => {
                self.open_plugin_menu(request_id, plugin_id, params)
            }
            Message::PluginMenuClosed { selected } => self.close_plugin_menu(selected),
            Message::PluginMenuQuery(query) => {
                self.plugin_menu_query = query;
                Task::none()
            }
            Message::PluginSurfaceOrigin { window, plugin_id, surface_id, origin } => {
                // 一个面同时只在一个窗口里：换窗口时清掉旧窗的原点，避免后续在旧窗重画浮层
                self.plugin_surface_origins
                    .retain(|(_, p, s), _| !(p == &plugin_id && s == &surface_id));
                self.plugin_surface_origins.insert((window, plugin_id, surface_id), origin);
                Task::none()
            }
            Message::PluginPickColor { request_id, params } => {
                self.plugin_pick_color(request_id, params)
            }
            Message::PluginPickVault { request_id } => self.plugin_pick_vault(request_id),
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
                // 双击改名：同 tab 两次按下间隔在判定窗口内（标题栏空白双击同口径）；
                // 这次按压随后会作为全局 Press 到达，由 begin_layout_rename 置行内按压吞掉
                if self.drag.is_none() {
                    let now = Instant::now();
                    let double = self
                        .layout_tab_last_press
                        .take_if(|(id, t)| {
                            id == &layout_id && now.duration_since(*t).as_millis() < DOUBLE_CLICK_MS
                        })
                        .is_some();
                    if double {
                        self.layout_tab_last_press = None;
                        return self.begin_layout_rename(layout_id, true);
                    }
                    self.layout_tab_last_press = Some((layout_id.clone(), now));
                }
                // 该 tab 改名在途：按压（含输入框上的拖选）不得转成拖标签手势
                if self
                    .rename
                    .as_ref()
                    .is_some_and(|r| r.target == RenameTarget::Layout(layout_id.clone()))
                {
                    return Task::none();
                }
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
                plugin_actions::release_plugin_focus(self, os_id);
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
            Message::LayoutNew => {
                let task = self.apply(LayoutOp::AddLayout);
                // 新建即命名：生成「布局 N」后直接进入编辑，名字全选可整体替换
                let Some(id) = self.ui.active_layout_id.clone() else { return task };
                if id == layout::HOME_LAYOUT_ID {
                    return task;
                }
                let Some(name) = self
                    .active_scene()
                    .and_then(|s| s.layouts.iter().find(|l| l.id == id))
                    .map(|l| l.name.clone())
                else {
                    return task;
                };
                self.rename = Some(self.rename_state(RenameTarget::Layout(id), name));
                task.chain(rename_focus_task())
            }
            Message::LayoutRenameStart(id) => self.begin_layout_rename(id, false),
            Message::LayoutRenameInput(value) => {
                if let Some(r) = &mut self.rename {
                    r.value = value;
                }
                Task::none()
            }
            Message::LayoutRenameCommit => {
                let Some(r) = self.rename.take() else { return Task::none() };
                match r.target {
                    RenameTarget::Layout(id) => {
                        // 目标在编辑期间被删 = 静默退出（不能误报成空名）
                        let Some(current_name) = self
                            .active_scene()
                            .and_then(|s| s.layouts.iter().find(|l| l.id == id))
                            .map(|l| l.name.clone())
                        else {
                            return Task::none();
                        };
                        let Some(final_name) = self
                            .active_scene()
                            .and_then(|s| layout::renamed_layout_name(s, &id, &r.value))
                        else {
                            self.notify(NoticeLevel::Warning, "布局名称不能为空");
                            return Task::none();
                        };
                        // 名称未变 = 只退出编辑，不写盘
                        if current_name == final_name {
                            return Task::none();
                        }
                        if final_name != r.value.trim() {
                            self.notify(
                                NoticeLevel::Warning,
                                format!("已有同名布局，已重命名为「{final_name}」"),
                            );
                        }
                        self.apply(LayoutOp::RenameLayout { id, name: final_name })
                    }
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
                self.rename = Some(self.rename_state(RenameTarget::Scene(id), name));
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
                // 主页面板懒加载搭载：最近打开条目按节流校验存在性
                let lazy = recent_files::tick(self)
                    .chain(space_actions::members_tick(self))
                    // 绘制期同步取走的帧不走通知：投影可能落后，心跳里补一次
                    // （图片载入与键位声明读的是投影缓存）
                    .chain(if self.plugins.take_observed_pending() {
                        plugin_actions::refresh_frames(self)
                    } else {
                        Task::none()
                    });
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
                self.notifications.retain(|n| !n.expired());
                // 拖拽悬停折叠目录到点自动展开（展开后该行不再是候选，避免反复触发）
                let spring = self.spring_open_dir();
                // 远端光标过期剔除节流（1s）：对端崩溃/断网无离场帧，靠通道超时惰性剔除
                if self.doc_presence.is_some()
                    && self
                        .space_cursor_prune_at
                        .is_none_or(|at| at.elapsed() >= std::time::Duration::from_secs(1))
                {
                    self.space_cursor_prune_at = Some(Instant::now());
                    space_actions::refresh_remote_cursors(self);
                }
                // 在场保活重发（通道内 15s 限频，连接期间搭载）：空闲端身份帧
                // 否则被对端 30s 超时剔除——后加入者与对端互见都依赖周期重发
                if self.space_connected {
                    space_actions::doc_presence_keepalive(self);
                }
                // 限频压下的选区变化（拖选连续位移）由本拍补发最新一份：断线期产出即丢弃，
                // 重连时的强制重发以最新状态兜底
                space_actions::doc_presence_flush(self);
                // 重名提醒条 3s 过期
                if self
                    .rename_notice
                    .as_ref()
                    .is_some_and(|n| n.at.elapsed().as_millis() >= 3000)
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
                    return spring.chain(Task::done(Message::Update(update::UpdateMsg::ProgressTick)));
                }
                spring
            }
            Message::WindowResized(id, size) => {
                if self.main_id == Some(id) {
                    self.main_size = size;
                } else if let Some(entry) = self.plugin_windows.get_mut(&id) {
                    // 插件窗口：记账里的尺寸随 OS 调整更新（`window.update` 只改一维时取它）
                    entry.size = size;
                } else {
                    // 撕裂窗口尺寸回写模型 bounds（OS 窗口事件 = bounds 唯一写者）
                    self.os_sizes.insert(id, size);
                    if let Some(model_id) = self.detached_os.get(&id) {
                        if let Some(w) =
                            self.ui.detached_windows.iter_mut().find(|w| w.id == *model_id)
                        {
                            w.bounds.width = size.width as f64;
                            w.bounds.height = size.height as f64;
                            self.schedule_persist();
                        }
                    }
                }
                // 拖动窗口边缘时插件帧在绘制期被取走、不再发通知：这一拍把投影补上
                // （事件本身已经触发重建，补投影不多花一次）
                plugin_actions::refresh_frames(self)
            }
            Message::Press(id) => {
                // 焦点窗口跟踪（接管请求作用域 = 最近按压的窗口）
                self.focused_window = Some(id);
                let cursor = self.cursor_for(id);
                // 对话输入面外点失焦：同批输入部件按压（Focus 事件）先行置位
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
                // 行内重命名失焦提交：按压不在输入行内 = 提交（Enter 同路径）；
                // 判定窗口 = 输入框所在窗口（标题就地改名可在撕裂窗口）。同步提交：
                // 同一按压随后可能触发新建/切标签，延迟提交会取到它们改写后的会话
                let rename_window = self.rename.as_ref().map(|r| r.window);
                if rename_window.is_some() {
                    if self.rename_press_inside {
                        self.rename_press_inside = false;
                    } else if rename_window == Some(id) {
                        return self.update(Message::LayoutRenameCommit);
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
                    if let Some(zone) = zones
                        .iter()
                        .find(|z| z.panel_id.is_some() && z.rect.contains(cursor))
                    {
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
                            reject: None,
                            spring: None,
                            hover: None,
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
                        let is_layout_drag = self
                            .drag
                            .as_ref()
                            .is_some_and(|d| d.from_panel.is_none() && d.from_detached.is_none());
                        if let Some(d) = self.drag.as_mut() {
                            d.current = global;
                            d.cursor_window = id;
                        }
                        // 布局 tab 拖拽只在标题栏条带内落位（插入位标由视图按同一算法渲染），
                        // 面板 / 撕裂窗口的落点指示不参与
                        if is_layout_drag {
                            self.drag_hover = None;
                        } else {
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
                if let Some(d) = self.file_drag.take() {
                    return self.resolve_file_drop(d);
                }
                if self.divider.take().is_some() {
                    // 拖动期间的帧在绘制期被取走、不再发通知：收手时把投影补上
                    return plugin_actions::refresh_frames(self);
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
            Message::ModsChanged(m) => {
                self.mods = m;
                Task::none()
            }
            Message::TitleBarPress(id) => {
                // 双击（判定窗口内同窗口两次空白按下）= 最大化切换；否则进入窗口拖拽
                let now = Instant::now();
                let double = self
                    .last_titlebar_press
                    .take_if(|(w, t)| {
                        *w == id && now.duration_since(*t).as_millis() < DOUBLE_CLICK_MS
                    })
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
                // Esc 作用域 = 事件来源窗口：先取消文件拖拽，再关该窗口弹层，
                // 再退出接管（接管面内的弹层先于接管退出），再取消属性面板编辑态与行内重命名
                if self.file_drag.is_some() {
                    self.cancel_file_drag();
                    self.file_press = None;
                    return Task::none();
                }
                if self.popup.as_ref().is_some_and(|(w, _)| *w == window) {
                    self.dismiss_popup();
                } else if self.plugin_takeovers.contains_key(&window) {
                    // 接管窗口：弹层已关（上一分支），Esc = 退出接管（退出入口常驻，Esc 是后备通道）
                    return plugin_actions::exit_takeover(self, window);
                } else if self.rename.as_ref().is_some_and(|r| r.window == window) {
                    // 行内重命名 Esc = 取消
                    self.rename = None;
                } else if layout_view::window_shows_canvas(self, window) {
                    self.plugins.notify_plugin_event(
                        crate::plugin_actions::CANVAS_PLUGIN,
                        atelyx_core::plugin_runtime::event_name::CANVAS_COMMAND,
                        &atelyx_core::plugin_runtime::CanvasCommandParams {
                            command: "escape".to_string(),
                        },
                    );
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
            Message::Tray(event) => match event {
                tray::TrayEvent::PluginClick { plugin_id, item_id } => {
                    self.plugins.notify_tray_click(&plugin_id, &item_id);
                    Task::none()
                }
                tray::TrayEvent::ShowMain => match self.main_id {
                    Some(id) => window::minimize(id, false).chain(window::gain_focus(id)),
                    None => Task::none(),
                },
                // 退出与关主窗同一条路径：先放行在途保存再收尾退出
                tray::TrayEvent::Quit => match self.main_id {
                    Some(id) => self.update(Message::WindowCloseRequested(id)),
                    None => iced::exit(),
                },
                tray::TrayEvent::Unavailable(reason) => {
                    self.notify(NoticeLevel::Warning, format!("系统托盘：{reason}"));
                    Task::none()
                }
            },
            Message::PluginEvent(event) => {
                // 插件（重启 / 重新启用后）就绪：已挂的面重新通知，插件据此重建界面；
                // 停用 / 失败 / 熔断：该插件的窗口随会话收尾关闭（重启退避中不动作）
                let mut task = Task::none();
                if let PluginEvent::AiChatEvent { params, .. } = event.as_ref() {
                    // 流式对话事件只更新对应会话 / 卡片，不进注册表重建
                    let task = chat_actions::on_ai_chat_event(self, params.clone());
                    settings_view::plugins::on_event(self, *event);
                    return task;
                }
                if let PluginEvent::ViewSubmitted { id, surface_id } = event.as_ref() {
                    // 帧事件只换对应面的投影；surface_id 缺省 = 帧集合整体清空（停用 / 收尾）
                    let task = match surface_id.as_deref() {
                        Some(surface_id) => {
                            plugin_actions::refresh_surface_frame(self, id, surface_id)
                        }
                        None => plugin_actions::refresh_frames(self),
                    };
                    settings_view::plugins::on_event(self, *event);
                    return task;
                }
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
                        if let Some((path, mode)) = self.pending_doc_open.clone() {
                            if self.pending_doc_plugin(&path).as_deref() == Some(id.as_str()) {
                                self.pending_doc_open = None;
                                task = task.chain(
                                    self.gate(PendingAfterFlush::OpenFile { path, mode }),
                                );
                            }
                        }
                    } else if matches!(
                        status,
                        PluginStatus::Stopped | PluginStatus::Failed(_)
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
                        // 停下的插件不会再发 drag.end：会话留在宿主手里会挡住之后的拖拽
                        if self.file_drag_surface.as_ref().is_some_and(|(owner, _)| owner == id) {
                            self.cancel_file_drag();
                        }
                        if let Some((path, _)) = self.pending_doc_open.clone() {
                            if self.pending_doc_plugin(&path).as_deref() == Some(id.as_str()) {
                                self.pending_doc_open = None;
                                self.notify(
                                    NoticeLevel::Error,
                                    "文档插件未运行，无法打开文档".to_string(),
                                );
                            }
                        }
                        // 核心异常退出 / 被停用都没有插件侧收尾：替在途 run 补发失败终态
                        // （正常停用同样收掉在途 run，避免面板永久停在「回复中」）
                        if *id == atelyx_core::plugin_runtime::AI_CORE_PLUGIN {
                            let sessions = chat_actions::in_flight_sessions(self);
                            if !sessions.is_empty() {
                                self.plugins.settle_ai_core_runs(
                                    &sessions,
                                    "AI 核心已停止，本轮回复中断",
                                );
                            }
                        }
                    }
                }
                if let PluginEvent::DefaultLayoutDeclared { id, .. } = event.as_ref() {
                    task = task.chain(plugin_actions::apply_declared_default_layout(self, id));
                }
                settings_view::plugins::on_event(self, *event);
                task.chain(plugin_actions::refresh_projection(self))
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
            Message::PluginWindowUpdate { request_id, plugin_id, params } => {
                plugin_actions::on_window_update(self, request_id, plugin_id, params)
            }
            Message::PluginWindowShow { request_id, plugin_id, window_key } => {
                plugin_actions::on_window_show(self, request_id, plugin_id, window_key)
            }
            Message::PluginWindowMinimize { request_id, plugin_id, window_key } => {
                plugin_actions::on_window_minimize(self, request_id, plugin_id, window_key)
            }
            Message::PluginWindowToggleMaximize { request_id, plugin_id, window_key } => {
                plugin_actions::on_window_toggle_maximize(self, request_id, plugin_id, window_key)
            }
            Message::PluginWindowBeginDrag { request_id, plugin_id, surface_id } => {
                plugin_actions::on_window_begin_drag(self, request_id, plugin_id, surface_id)
            }
            Message::PluginFrameFocus { window, plugin_id, surface_id, focused } => {
                plugin_actions::on_frame_focus(self, window, plugin_id, surface_id, focused);
                Task::none()
            }
            Message::PluginFrameTick => {
                // 帧拍只推通知、不等插件回执；写出插件 stdin 交给线程池——管道写满时阻塞的是任务线程，
                // 不能是 UI 线程。上一拍还在写就不再起新任务：否则慢插件会让帧排成一队、顺序也不保证
                if self.plugin_frame_pumping {
                    return Task::none();
                }
                self.plugin_frame_pumping = true;
                let plugins = self.plugins.clone();
                Task::perform(
                    async move { plugins.pump_frame_ticks(std::time::Instant::now()) },
                    |_| Message::PluginFramePumped,
                )
            }
            Message::PluginFramePumped => {
                self.plugin_frame_pumping = false;
                Task::none()
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
            Message::FontPickerQuery(query) => {
                self.font_picker_query = query;
                Task::none()
            }
            Message::InterfaceFontSet(name) => {
                self.global.font_family = name.clone();
                ui::set_family(name.as_deref());
                self.dismiss_popup();
                self.save_global_now();
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
                // 打开开关立即查一次，不必等下个轮询周期
                if self.global.auto_update && !cfg!(debug_assertions) {
                    return Task::done(Message::Update(update::UpdateMsg::Check { manual: false }));
                }
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
                self.plugins.on_editor_prefs_changed(editor_prefs(self));
                Task::none()
            }
            Message::InPageTitleToggle => {
                self.global.in_page_title = !self.global.in_page_title;
                self.save_global_now();
                self.plugins.on_editor_prefs_changed(editor_prefs(self));
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
                        self.plugins.on_editor_prefs_changed(editor_prefs(self));
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
                self.font_picker_query.clear();
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
                        self.rename_notice = Some(layout_view::RenameNotice {
                            text: format!("「{old_name}」已存在，已重命名为「{new_name}」"),
                            at: Instant::now(),
                        });
                        *to = final_rel;
                    }
                }
                if let FollowUp::Renamed { to, announce, .. } = &follow {
                    if *announce {
                        let dir = parent_of(to);
                        self.notify(
                            NoticeLevel::Success,
                            if dir.is_empty() {
                                "已移到根目录".to_string()
                            } else {
                                let name = dir.rsplit('/').next().unwrap_or(&dir);
                                format!("已移到「{name}」")
                            },
                        );
                    }
                } else {
                    self.notify(NoticeLevel::Success, "完成");
                }
                // 宿主发起的文件操作成功后广播（改名按去重后的实际落点；原地改名不重复告知）
                match &follow {
                    FollowUp::Created { rel } => {
                        self.plugins.on_file_changed(rel, FileChangeKind::Created, None);
                    }
                    FollowUp::Renamed { from, to, .. } if from != to => {
                        self.plugins.on_file_changed(
                            to,
                            FileChangeKind::Renamed,
                            Some(from.as_str()),
                        );
                    }
                    FollowUp::Deleted { rel, .. } => {
                        self.plugins.on_file_changed(rel, FileChangeKind::Deleted, None);
                    }
                    _ => {}
                }
                self.apply_follow(&follow);
                // 引用同步代写了打开中的文档（rewritten 键 = 落盘后路径，与跟随后的
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
                    self.follow_children_cache(from, to);
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
                    .chain(self.refresh_tag_vocab());
                if matches!(follow, FollowUp::Renamed { .. }) {
                    next = next.chain(self.load_vault_meta());
                }
                if let Some(rel) = reload_note {
                    next = next.chain(self.open_file(&rel, OpenMode::Reveal));
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
            Message::RenamePressInside => {
                self.rename_press_inside = true;
                Task::none()
            }
            Message::SettingsNavCollapse => {
                self.settings_collapsed = !self.settings_collapsed;
                Task::none()
            }
            Message::SaveErrorDismiss => {
                self.doc_save_error = None;
                Task::none()
            }
            Message::DividerAdjust(horizontal, positive) => {
                // 键盘调整上次拖动过的把手（±2%，双侧钳 12%）；
                // 门控：对话输入聚焦 / 行内重命名在途时方向键属文本操作，不调把手
                if self.rename.is_some()
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
        plugin_actions::sync_canvas_display(self);
        let reconcile = self.reconcile();
        let task = match surfaces_changed {
            true => reconcile.chain(plugin_actions::refresh_projection(self)),
            false => reconcile,
        };
        (task, result)
    }

    /// 收起当前弹层（菜单动作完成后的统一收尾）。
    pub(crate) fn dismiss_popup(&mut self) {
        // 插件取色器被收起（Esc / 点外点 / 打开别的弹层）= 取消：结果缺省，别让插件一直等
        if let Some((_, open)) = self.popup.take() {
            if let layout_view::PopupState::ColorPicker {
                target: crate::color_picker_actions::ColorTarget::Plugin { request_id, .. },
                ..
            } = open.state
            {
                crate::color_picker_actions::reply_plugin_color(self, request_id, None);
            }
        }
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
        // （titlebar_tab_insert 返回主窗口本地坐标下的插入位，光标须换算回本地）
        if d.from_panel.is_none() && d.from_detached.is_none() {
            if let Some(main_id) = self.main_id {
                if let Some(wpos) = self.os_positions.get(&main_id) {
                    let local = Point::new(d.current.x - wpos.x, d.current.y - wpos.y);
                    if let Some(insert) = layout_view::titlebar_tab_insert(self, local) {
                        return Task::done(Message::LayoutDropped(d.layout_from, insert));
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

    /// 画布快捷键上下文：画布已打开且在焦点窗口展示、无弹层 / 行内改名 / 拖拽在途。
    fn canvas_key_ready(&self) -> bool {
        if self.ui.last_canvas_file.is_none() || self.popup.is_some() || self.rename.is_some() {
            return false;
        }
        self.focused_window
            .is_some_and(|window| layout_view::window_shows_canvas(self, window))
    }

    /// 把画布作用域命令转给画布插件执行（快捷键可自定义，绑定仍归宿主）。
    fn send_canvas_command(&self, command: &str) {
        self.plugins.notify_plugin_event(
            plugin_actions::CANVAS_PLUGIN,
            atelyx_core::plugin_runtime::event_name::CANVAS_COMMAND,
            &atelyx_core::plugin_runtime::CanvasCommandParams { command: command.to_string() },
        );
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

    /// 文档 / 画布 / 仍在运行的面板插件任一脏（停用插件的残留头部不再拦动作）。
    fn any_dirty(&self) -> bool {
        self.doc_dirty
            || self.canvas_flush_pending
            || self.panel_headers.values().any(|entry| {
                entry.state.dirty && self.plugin_is_running(&entry.plugin_id)
            })
    }

    /// 插件是否在运行（面板头部的脏只认运行中的插件）。
    fn plugin_is_running(&self, plugin_id: &str) -> bool {
        self.plugins
            .list()
            .iter()
            .any(|plugin| plugin.id == plugin_id && plugin.status == PluginStatus::Running)
    }

    /// 下一个要落盘的面板（按目标名排序，避免哈希序抖动；跳过已停插件）。
    fn next_dirty_panel(&self) -> Option<(String, String)> {
        let mut dirty: Vec<(String, String)> = self
            .panel_headers
            .iter()
            .filter(|(_, entry)| entry.state.dirty && self.plugin_is_running(&entry.plugin_id))
            .map(|(target, entry)| (target.clone(), entry.plugin_id.clone()))
            .collect();
        dirty.sort();
        dirty.into_iter().next()
    }

    /// 脏时先收尾挂起输入再放行动作；不脏直接执行。
    /// 文档 / 画布任一脏即挂起 pending，全部落盘后执行（after_any_flush）。
    fn gate(&mut self, pending: PendingAfterFlush) -> Task<Message> {
        // 在途的插件结构变更将被更晚的 pending 取代：先回填可读错误，插件调用不悬挂
        if let Some(
            PendingAfterFlush::PluginVaultRemove { request_id, .. }
            | PendingAfterFlush::PluginVaultRename { request_id, .. },
        ) = &self.flush_pending
        {
            self.plugins
                .resolve_shell_call(*request_id, Err("被更晚的宿主操作取代，请重试".into()));
        }
        // 画布脏标记由插件异步上报，不能只靠它判定：打开着画布就先请插件确认一次
        if self.ui.last_canvas_file.is_some()
            && self.plugin_is_running(plugin_actions::CANVAS_PLUGIN)
        {
            self.canvas_flush_pending = true;
        }
        if self.any_dirty() || self.canvas_flush_pending {
            self.flush_pending = Some(pending);
            return self.flush_dirty_now();
        }
        self.proceed(pending)
    }

    /// 提交当前所有脏面的保存（flush 语义；全部落地后由回执侧放行 flush_pending）。
    fn flush_dirty_now(&mut self) -> Task<Message> {
        let mut task = Task::none();
        task = task.chain(self.request_panel_flush(PanelFlushPurpose::Gate));
        if self.pending_panel_flush.is_none() && self.canvas_flush_pending {
            task = task.chain(self.request_canvas_flush(PanelFlushPurpose::Gate));
        }
        if self.doc_dirty {
            task = task.chain(self.submit_save(SaveReason::Flush));
        }
        task
    }

    /// 请声明了脏头部的插件立即落盘（一次只发一个在途请求，回执按令牌 + 归属结算）。
    fn request_panel_flush(&mut self, purpose: PanelFlushPurpose) -> Task<Message> {
        if self.pending_panel_flush.is_some() {
            return Task::none();
        }
        let Some((target, plugin_id)) = self.next_dirty_panel() else {
            return Task::none();
        };
        self.send_panel_flush(target, plugin_id, purpose)
    }

    /// 回滚后强制请表格插件落盘：不依赖脏头部（回滚可能已被自动保存提前落盘）。
    fn request_table_flush(&mut self, purpose: PanelFlushPurpose) -> Task<Message> {
        if self.pending_panel_flush.is_some() {
            return Task::none();
        }
        self.send_panel_flush(
            "table".to_string(),
            plugin_actions::TABLES_PLUGIN.to_string(),
            purpose,
        )
    }

    /// 回滚后强制请画布插件落盘：不依赖脏头部（回滚可能已被自动保存提前落盘）。
    fn request_canvas_flush(&mut self, purpose: PanelFlushPurpose) -> Task<Message> {
        if self.pending_panel_flush.is_some() {
            return Task::none();
        }
        self.send_panel_flush(
            "canvas".to_string(),
            plugin_actions::CANVAS_PLUGIN.to_string(),
            purpose,
        )
    }

    fn send_panel_flush(
        &mut self,
        target: String,
        plugin_id: String,
        purpose: PanelFlushPurpose,
    ) -> Task<Message> {
        self.panel_flush_seq = self.panel_flush_seq.wrapping_add(1);
        let token = self.panel_flush_seq;
        self.pending_panel_flush = Some(PendingPanelFlush {
            token,
            target: target.clone(),
            plugin_id: plugin_id.clone(),
            purpose,
        });
        self.plugins.notify_plugin_event(
            &plugin_id,
            atelyx_core::plugin_runtime::event_name::PANEL_FLUSH,
            &atelyx_core::plugin_runtime::PanelFlushParams { target, token },
        );
        // 插件无响应时不能让脏拦截永久挂起：超时撤回请求并提示（动作不执行）
        Task::perform(
            async move {
                tokio::time::sleep(PANEL_FLUSH_TIMEOUT).await;
                token
            },
            Message::PanelFlushTimeout,
        )
    }

    /// 插件回报落盘完成：令牌与发起方都对上才结算；成功只清这个插件的脏。
    /// 还有别的脏面板就继续下一个；全部干净后放行挂起动作。
    pub(crate) fn settle_panel_flush(
        &mut self,
        plugin_id: &str,
        params: atelyx_core::plugin_runtime::PanelFlushDoneParams,
    ) -> Task<Message> {
        let Some(pending) = self.pending_panel_flush.as_ref() else {
            return Task::none();
        };
        let target_matches = params
            .target
            .as_deref()
            .is_none_or(|target| target == pending.target);
        if pending.token != params.token || pending.plugin_id != plugin_id || !target_matches {
            return Task::none();
        }
        let purpose = pending.purpose;
        let target = pending.target.clone();
        self.pending_panel_flush = None;
        if target == "canvas" {
            self.canvas_flush_pending = false;
        }
        if !params.ok {
            let reason = params.error.unwrap_or_else(|| "未知原因".to_string());
            self.notify(NoticeLevel::Error, format!("保存失败，操作未执行：{reason}"));
            return Task::none();
        }
        if let Some(entry) = self.panel_headers.get_mut(&target) {
            if entry.plugin_id == plugin_id {
                entry.state.dirty = false;
            }
        }
        let mut history = Task::none();
        if purpose == PanelFlushPurpose::RestoreNotice {
            if target == "table" {
            if let Some(restore) = self.pending_table_restore.take() {
                if restore.gen != self.epoch() {
                    self.notify(NoticeLevel::Warning, "已切换仓库，回滚取消".to_string());
                    return self.after_any_flush();
                }
                // 回滚期间又切了文件：快照已不属于当前打开的表，不能记成功
                if self.ui.last_table_file.as_deref() != Some(restore.rel.as_str()) {
                    self.notify(NoticeLevel::Warning, "表格已切换，回滚取消".to_string());
                    return self.after_any_flush();
                }
                self.notify(NoticeLevel::Success, "已回滚".to_string());
                history = if restore.space {
                    space_actions::space_record_table_restore(self, restore.rel, restore.content)
                } else {
                    let gen = self.epoch();
                    let state = self.vault.clone();
                    let author = history_author(self);
                    Task::perform(
                        async move {
                            tokio::task::spawn_blocking(move || {
                                let (root, _, _) = state.session_snapshot()?;
                                atelyx_core::history::record_version(
                                    &root,
                                    atelyx_core::history::HistoryKind::Table,
                                    &restore.rel,
                                    &atelyx_core::history::RecordOpts {
                                        content: restore.content,
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
                };
            }
            } else if target == "canvas" {
                if let Some(restore) = self.pending_canvas_restore.take() {
                    if restore.gen != self.epoch() {
                        self.notify(NoticeLevel::Warning, "已切换仓库，回滚取消".to_string());
                        return self.after_any_flush();
                    }
                    // 回滚期间又切了文件：快照已不属于当前打开的画布，不能记成功
                    if self.ui.last_canvas_file.as_deref() != Some(restore.rel.as_str()) {
                        self.notify(NoticeLevel::Warning, "画布已切换，回滚取消".to_string());
                        return self.after_any_flush();
                    }
                    self.notify(NoticeLevel::Success, "已回滚".to_string());
                    history = if restore.space {
                        space_actions::space_record_canvas_restore(self, restore.rel, restore.content)
                    } else {
                        let gen = self.epoch();
                        let state = self.vault.clone();
                        let author = history_author(self);
                        Task::perform(
                            async move {
                                tokio::task::spawn_blocking(move || {
                                    let (root, _, _) = state.session_snapshot()?;
                                    atelyx_core::history::record_version(
                                        &root,
                                        atelyx_core::history::HistoryKind::Canvas,
                                        &restore.rel,
                                        &atelyx_core::history::RecordOpts {
                                            content: restore.content,
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
                    };
                }
            }
        }
        let next = if let Some((target, _)) = self.next_dirty_panel() {
            let purpose = if (self.pending_table_restore.is_some() && target == "table")
                || (self.pending_canvas_restore.is_some() && target == "canvas")
            {
                PanelFlushPurpose::RestoreNotice
            } else {
                PanelFlushPurpose::Gate
            };
            self.request_panel_flush(purpose)
        } else if self.pending_table_restore.is_some() {
            // 回滚可能已被自动保存提前落盘：脏面清空后仍要请表格插件确认一次
            let task = self.request_table_flush(PanelFlushPurpose::RestoreNotice);
            if self.pending_panel_flush.is_some() {
                task
            } else {
                self.after_any_flush()
            }
        } else if self.canvas_flush_pending {
            self.request_canvas_flush(PanelFlushPurpose::Gate)
        } else if self.pending_canvas_restore.is_some() {
            let task = self.request_canvas_flush(PanelFlushPurpose::RestoreNotice);
            if self.pending_panel_flush.is_some() {
                task
            } else {
                self.after_any_flush()
            }
        } else {
            self.after_any_flush()
        };
        history.chain(next)
    }

    /// 落盘请求超时：撤回在途请求与挂起动作并提示（用户可重试，不由后台反复重发）。
    pub(crate) fn settle_panel_flush_timeout(&mut self, token: u64) -> Task<Message> {
        if self.pending_panel_flush.as_ref().map(|pending| pending.token) != Some(token) {
            return Task::none();
        }
        self.pending_panel_flush = None;
        self.canvas_flush_pending = false;
        // 挂起动作一并放弃：保留它会让下一次放行在插件仍无响应时反复触发同一超时
        self.flush_pending = None;
        let restore = self.pending_table_restore.take().is_some();
        self.notify(
            NoticeLevel::Error,
            if restore { "回滚未落盘".to_string() } else { "保存超时，操作未执行".to_string() },
        );
        Task::none()
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
            PendingAfterFlush::OpenFile { path, mode } => self.open_file(&path, mode),
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
            PendingAfterFlush::RenameFile(r) => self.commit_file_rename(r),
            PendingAfterFlush::MoveEntry { from, is_dir, to_dir } => self.move_entry(&from, is_dir, &to_dir),
            PendingAfterFlush::PluginVaultRemove { request_id, path } => {
                // 脏拦截已过：直接执行（不得再经 gate，否则空脏判定重复放行成环）
                settings_view::plugins::execute_vault_remove(self, request_id, path)
            }
            PendingAfterFlush::PluginVaultRename { request_id, from, to } => {
                settings_view::plugins::execute_vault_rename(self, request_id, from, to)
            }
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

    /// 插件文档面承载一篇文档：正文真源仍在本 `doc_text`，编辑态由插件自持。
    /// 协作空间同时建立 CRDT 文档（基线以载入正文为准）。
    pub(crate) fn attach_doc_session(&mut self, rel: &str, text: String) {
        let path_changed = self.ui.last_note_file.as_deref() != Some(rel);
        self.ui.last_note_file = Some(rel.to_string());
        self.doc_active = true;
        self.doc_selection = None;
        self.doc_base = None;
        self.doc_text = text.clone();
        // 换篇后可重试之前失败的图片（句柄按 src 缓存，仍可复用）
        self.note_images_inflight.clear();
        self.note_images_failed.clear();
        self.history_kind = atelyx_core::history::HistoryKind::Note;
        self.history.clear();
        self.doc_dirty = false;
        self.doc_save_error = None;
        self.autosave_deadline = None;
        self.autosave_timer = false;
        match self.repo_kind.clone() {
            RepoKind::Local => {
                self.space_doc_flushed = None;
                self.doc_collab = None;
                self.doc_collab_file = None;
                self.doc_presence = None;
                self.remote_cursors.clear();
            }
            RepoKind::Space(_) => {
                self.space_doc_flushed = Some(text);
                space_actions::collab_doc_loaded(self, rel);
            }
        }
        crate::doc_session::publish_presence(self);
        // 启动恢复挂回的仍是磁盘上的同一路径：不新增写盘。
        if path_changed {
            self.schedule_persist();
        }
    }

    /// 插件文档面提交一次编辑：以 `doc_base`（插件当前持有的全文）为祖先三方合并，
    /// 返回需要回填给插件的新正文（无 = None）。只有合并结果真的变了正文才置脏并排自动保存。
    pub(crate) fn apply_doc_edit(
        &mut self,
        text: String,
        selection: Option<(usize, usize)>,
    ) -> Option<String> {
        // 旧正文先移出，避免每笔编辑都为比对再复制一份全文
        let before = std::mem::take(&mut self.doc_text);
        let (merged, base, correction) = merge_doc_edit(self.doc_base.take(), &before, text);
        self.doc_base = base;
        let changed = merged != before;
        self.doc_text = merged;
        if changed {
            self.doc_dirty = true;
            self.doc_save_error = None;
            self.autosave_deadline = Some(Instant::now() + AUTOSAVE_DEBOUNCE);
            space_actions::collab_after_edit(self);
        }
        // 选区偏移相对插件提交的全文；正文被合并改写时坐标系已错位，宁可丢弃。
        self.doc_selection = if correction.is_some() { None } else { selection };
        space_actions::collab_after_caret(self);
        correction
    }

    /// 关闭文档会话（换文件 / 关标签 / 切仓库）：摘协作文档与在场通道。
    pub(crate) fn close_doc_session(&mut self) {
        if !self.doc_active {
            return;
        }
        self.doc_active = false;
        self.doc_selection = None;
        self.ui.last_note_file = None;
        self.doc_dirty = false;
        self.doc_save_error = None;
        self.autosave_deadline = None;
        self.autosave_timer = false;
        if matches!(self.repo_kind, RepoKind::Space(_)) {
            space_actions::collab_unbind_doc(self);
        }
        self.doc_collab = None;
        self.doc_collab_file = None;
        self.doc_presence = None;
        self.remote_cursors.clear();
        self.space_doc_flushed = None;
        crate::doc_session::publish_presence(self);
        self.schedule_persist();
    }

    /// 提交一次文档写盘：仓库根与内容在发起时冻结，写盘经 core 串行队列按提交序执行。
    /// 空间模式分派服务端（回读合流 + PUT + 服务端历史）。
    pub(crate) fn submit_save(&mut self, reason: SaveReason) -> Task<Message> {
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
        let content = self.doc_text.clone();
        let saved = content.clone();
        let queue = self.vault.writes.clone();
        let rel_for_done = rel.clone();
        Task::perform(
            async move {
                let path = vault::safe_join(&root, &rel, false)?;
                queue.write(path, content).await
            },
            move |result| Message::Saved(gen, rel_for_done, saved, reason, result),
        )
    }

    /// 打开 .atb：登记打开的文件、面板内容切到表格插件网格视图、广播打开文件集合。
    /// 表格数据、编辑与保存链都在插件；宿主只负责视图显示（`mode`）、最近打开与历史列表。
    fn open_table(&mut self, rel: &str, mode: OpenMode) -> Task<Message> {
        self.ui.last_table_file = Some(rel.to_string());
        self.history_kind = atelyx_core::history::HistoryKind::Table;
        // 打开 / 切换表格即广播：第三方表格视图按当前路径重取
        self.plugins.on_table_changed(Some(rel));
        if mode.persists() {
            self.schedule_persist();
        }
        if mode.records_recent() {
            crate::recent_files::record(self, rel, atelyx_core::plugin_runtime::RecentKind::Table);
        }
        // 插件按打开文件集合事件重读内容：切表 / 首次打开都靠这条
        self.notify_open_files();
        // 空间名册的「正在编辑」取打开文件集合：打开表格同样要上行
        if matches!(self.repo_kind, RepoKind::Space(_)) {
            crate::space_actions::send_presence(self);
        }
        let task = if mode.reveals() {
            plugin_actions::reveal_view(self, "table")
        } else {
            Task::none()
        };
        task.chain(self.reload_history())
    }

    /// 打开 .atlx：登记打开的文件、面板内容切到画布插件视图、广播打开文件集合。
    /// 画布数据、交互与保存链都在插件；宿主只负责视图显示（`mode`）、最近打开与历史列表。
    fn open_canvas(&mut self, rel: &str, mode: OpenMode) -> Task<Message> {
        self.ui.last_canvas_file = Some(rel.to_string());
        self.history_kind = atelyx_core::history::HistoryKind::Canvas;
        if mode.persists() {
            self.schedule_persist();
        }
        if mode.records_recent() {
            crate::recent_files::record(self, rel, atelyx_core::plugin_runtime::RecentKind::Canvas);
        }
        // 插件按打开文件集合事件重读内容：切画布 / 首次打开都靠这条
        self.notify_open_files();
        if matches!(self.repo_kind, RepoKind::Space(_)) {
            crate::space_actions::send_presence(self);
        }
        let task = if mode.reveals() {
            plugin_actions::reveal_view(self, atelyx_core::layout::CANVAS_VIEW)
        } else {
            Task::none()
        };
        task.chain(self.reload_history())
    }

    /// 打开文件（脏拦截已由 gate 处理）：按扩展名分流（.atlx → 画布、.atb → 表格、
    /// 插件注册的领域扩展名 → 插件文档面），复位只读并清掉上一文件的解析产物。
    /// `mode` = 打开意图：Reveal 确保视图标签存在并激活；Background 只挂已有视图；
    /// Restore 只把文件挂到已有视图，不新建 / 不激活标签、不落盘。
    pub(crate) fn open_file(&mut self, rel: &str, mode: OpenMode) -> Task<Message> {
        if rel.ends_with(".atlx") {
            return self.open_canvas(rel, mode);
        }
        if rel.ends_with(".atb") {
            return self.open_table(rel, mode);
        }
        if let Some(extension) = rel.rsplit('.').next() {
            if self
                .registry
                .doc_types()
                .iter()
                .any(|info| info.registration.extension.eq_ignore_ascii_case(extension))
            {
                return plugin_actions::open_doc(self, rel, mode);
            }
        }
        if rel.ends_with(".md") {
            // 文档插件可能仍在后台装载（启动自动恢复先于它就绪）：挂起一次，就绪后按原意图重试。
            self.pending_doc_open = Some((rel.to_string(), mode));
            // 启动恢复是静默恢复：不为「插件启动中」再提示一次。
            if mode.records_recent() {
                self.notify(
                    NoticeLevel::Info,
                    "文档插件正在启动，稍后打开".to_string(),
                );
            }
            return Task::none();
        }
        self.notify(
            NoticeLevel::Warning,
            format!("没有可打开该文件的视图：{rel}"),
        );
        Task::none()
    }

    /// 挂起中的文档打开请求归属哪个插件（按扩展名查文档类型注册）。
    fn pending_doc_plugin(&self, path: &str) -> Option<String> {
        let extension = path.rsplit('.').next()?;
        self.registry
            .doc_types()
            .iter()
            .find(|info| info.registration.extension.eq_ignore_ascii_case(extension))
            .map(|info| info.plugin_id.clone())
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
        // 运行中切换：当前打开的笔记 / 表格 / 画布一并关闭（保存已由 gate 完成），
        // 「上次打开」记录随之清除，进入新仓库为未打开态
        if !from_boot {
            self.ui.last_note_file = None;
            self.ui.last_table_file = None;
            self.plugins.on_table_changed(None);
            self.ui.last_canvas_file = None;
            // 打开中的插件文档实例随切仓库关闭（旧容器路径失效，实例面撤下；投影任务丢弃，后续刷新兜底）
            let _ = plugin_actions::close_doc(self);
            self.doc_active = false;
            self.doc_selection = None;
            self.pending_doc_open = None;
            self.history.clear();
        }
        self.children.clear();
        self.doc_text.clear();
        self.doc_dirty = false;
        self.doc_save_error = None;
        self.file_count = None;
        self.rename_notice = None;
        // 图片句柄按仓库相对路径缓存：换仓库后同一路径指向别的文件，整表作废
        self.note_images.clear();
        self.note_image_sizes.clear();
        self.note_images_inflight.clear();
        self.note_images_failed.clear();
        self.note_image_epoch = self.note_image_epoch.wrapping_add(1);
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
        self.history_preview = None;
        self.history_confirm = None;
        // 聚合缓存随切仓失效（在途扫描按世代作废，插件重挂后重取）
        repo_history::invalidate(self);
        self.repo_history_loading = false;
        // 等聚合落地的查询随切仓作废：逐个回绝，插件调用不会悬着
        settings_view::plugins::fail_pending_history(self, "仓库已切换");
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
        // 启动自动进入且「自动恢复」开启：把上次打开的文件挂回已有视图；运行中切换不恢复（文件已随切仓关闭）
        for (rel, mode) in restore_open_requests(&self.ui, from_boot, self.global.auto_restore) {
            // 笔记由插件文档面承载：走同一套打开路由（插件未就绪时按恢复意图挂起）
            task = task.chain(self.open_file(&rel, mode));
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
        // 画布是整面插件视图：先于插件文档分支处理，否则会被「非文档插件视图」早退吞掉
        if layout_view::is_canvas_view(kind) {
            self.ui.last_canvas_file = None;
            self.schedule_persist();
            self.notify_open_files();
            self.history.clear();
            if matches!(self.repo_kind, RepoKind::Space(_)) {
                space_actions::send_presence(self);
            }
            return Task::none();
        }
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
                self.panel_view.insert("table".to_string(), None);
                self.plugins.on_table_changed(None);
                self.schedule_persist();
                // 与笔记关闭同规：历史面板不再有对应文件可查，清空防列表冻结在旧数据
                self.history.clear();
                self.notify_open_files();
                // 空间模式回收在场（名册不再滞留「正在编辑该表格」）
                if matches!(self.repo_kind, RepoKind::Space(_)) {
                    space_actions::send_presence(self);
                }
            }
            _ => {
                // 空间模式先摘协作绑定（离场帧 + presence 回收）再清编辑态
                if matches!(self.repo_kind, RepoKind::Space(_)) {
                    space_actions::collab_unbind_doc(self);
                }
                self.ui.last_note_file = None;
                self.doc_text.clear();
                self.doc_dirty = false;
                self.doc_save_error = None;
                        self.history.clear();
            }
        }
        self.schedule_persist();
        Task::none()
    }

    fn active_tree(&self) -> Option<layout::LayoutNode> {
        Some(layout::active_layout(&self.ui).tree)
    }

    // ===== 标签词汇表 / 全仓库文件清单 =====

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

    /// 全仓库文件清单（`vault.files` 数据源；树序）。空间模式由整树回执填充，此处直通。
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

    // ===== 历史侧车 =====

    /// 读取当前上下文（笔记 / 表格 / 画布）的历史版本（尽力而为，读失败 = 空列表）。
    fn reload_history(&self) -> Task<Message> {
        let (kind, rel) = match self.history_kind {
            atelyx_core::history::HistoryKind::Note => {
                (atelyx_core::history::HistoryKind::Note, self.ui.last_note_file.clone())
            }
            atelyx_core::history::HistoryKind::Table => {
                (atelyx_core::history::HistoryKind::Table, self.ui.last_table_file.clone())
            }
            atelyx_core::history::HistoryKind::Canvas => {
                (atelyx_core::history::HistoryKind::Canvas, self.ui.last_canvas_file.clone())
            }
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
    fn record_doc_history(&self, content: String) -> Task<Message> {
        let Some(rel) = self.ui.last_note_file.clone() else { return Task::none() };
        let gen = self.epoch();
        let state = self.vault.clone();
        let author = history_author(self);
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

    fn doc_history_restore(&mut self, version: atelyx_core::history::HistoryVersion) -> Task<Message> {
        self.doc_text = version.content.clone();
        self.doc_dirty = true;
        self.doc_save_error = None;
        self.autosave_deadline = Some(Instant::now() + AUTOSAVE_DEBOUNCE);
        if self.doc_active {
            // 回滚换掉整篇正文：插件面必须同步看到新文；祖先与正文相同，用 None 表示。
            self.doc_base = None;
            self.doc_selection = None;
            doc_session::publish_state(self, Some(self.doc_text.clone()));
        }
        let Some(rel) = self.ui.last_note_file.clone() else { return Task::none() };
        if matches!(self.repo_kind, RepoKind::Space(_)) {
            return space_actions::space_record_doc_restore(self, rel, version.content);
        }
        let gen = self.epoch();
        let state = self.vault.clone();
        let author = history_author(self);
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

    /// 回滚到指定版本：全文快照写回走各保存链（画布走 canvas_history_restore 快照直写）。
    pub(crate) fn history_restore(
        &mut self,
        kind: atelyx_core::history::HistoryKind,
        version: atelyx_core::history::HistoryVersion,
    ) -> Task<Message> {
        match kind {
            atelyx_core::history::HistoryKind::Table => {
                // 表格：快照 = TableFile JSON；解析失败静默放弃（历史尽力而为）
                if serde_json::from_str::<atelyx_core::vault::TableFile>(&version.content).is_err() {
                    self.notify(NoticeLevel::Error, "历史版本无法解析");
                    return Task::none();
                }
                let Some(rel) = self.ui.last_table_file.clone() else {
                    self.notify(NoticeLevel::Warning, "表格未打开，无法回滚");
                    return Task::none();
                };
                // 内容与保存链都在插件：插件不在跑就无从交回快照，不能谎报成功
                if !self
                    .plugins
                    .list()
                    .iter()
                    .any(|p| p.id == plugin_actions::TABLES_PLUGIN && p.status == PluginStatus::Running)
                {
                    self.notify(NoticeLevel::Warning, "表格插件未运行，无法回滚");
                    return Task::none();
                }
                self.plugins.notify_plugin_event(
                    plugin_actions::TABLES_PLUGIN,
                    atelyx_core::plugin_runtime::event_name::TABLE_RESTORE,
                    &atelyx_core::plugin_runtime::TableContentParams {
                        path: Some(rel.clone()),
                        content: version.content.clone(),
                    },
                );
                // 回滚内容在插件里：先交回快照，等它落盘成功再记 restore 版本并报成功
                self.pending_table_restore = Some(PendingTableRestore {
                    rel,
                    content: version.content,
                    space: matches!(self.repo_kind, RepoKind::Space(_)),
                    gen: self.epoch(),
                });
                Task::perform(
                    async move {
                        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                    },
                    |()| Message::TableRestoreFlush,
                )
            }
            atelyx_core::history::HistoryKind::Note => self.doc_history_restore(version),
            atelyx_core::history::HistoryKind::Canvas => {
                let Some(rel) = self.ui.last_canvas_file.clone() else {
                    self.notify(NoticeLevel::Warning, "画布未打开，无法回滚");
                    return Task::none();
                };
                self.canvas_history_restore(rel, version)
            }
        }
    }

    /// 画布回滚：快照校验后交回插件，等它落盘收敛再记 restore 版本（与表格同一条链）。
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
        if self.ui.last_canvas_file.as_deref() != Some(rel.as_str()) {
            self.notify(NoticeLevel::Warning, "画布未打开，无法回滚");
            return Task::none();
        }
        // 内容与保存链都在插件：插件不在跑就无从交回快照，不能谎报成功
        if !self
            .plugins
            .list()
            .iter()
            .any(|p| p.id == plugin_actions::CANVAS_PLUGIN && p.status == PluginStatus::Running)
        {
            self.notify(NoticeLevel::Warning, "画布插件未运行，无法回滚");
            return Task::none();
        }
        self.plugins.notify_plugin_event(
            plugin_actions::CANVAS_PLUGIN,
            atelyx_core::plugin_runtime::event_name::CANVAS_RESTORE,
            &atelyx_core::plugin_runtime::CanvasContentParams {
                path: Some(rel.clone()),
                content: version.content.clone(),
            },
        );
        self.pending_canvas_restore = Some(PendingCanvasRestore {
            rel,
            content: version.content,
            space: matches!(self.repo_kind, RepoKind::Space(_)),
            gen: self.epoch(),
        });
        Task::perform(
            async move {
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            },
            |()| Message::CanvasRestoreFlush,
        )
    }

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
    fn on_ui(&mut self, msg: ui::UiMsg<Message>) -> Task<Message> {
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
                // 点外点 / Esc 关掉的插件菜单也要回传「未选中」，否则发起插件等不到收尾
                let had_plugin_menu = self.plugin_menu.is_some();
                if self.popup.as_ref().is_some_and(|(w, _)| *w == window) {
                    // 走统一收尾：插件取色器在这里以「取消」回执，插件不会悬挂
                    self.dismiss_popup();
                }
                match had_plugin_menu {
                    true => self.close_plugin_menu(None),
                    false => Task::none(),
                }
            }
            ui::UiMsg::NotifyDismiss(id) => {
                // 更新通知的 × 等同「跳过」：本次运行不再自动提示该版本
                if self.update_notice_id == Some(id) {
                    let skipped = match &self.update {
                        update::UpdateState::Available { version, .. } => Some(version.clone()),
                        _ => None,
                    };
                    if let Some(version) = skipped {
                        self.update_skipped = Some(version);
                        self.update = update::UpdateState::Idle;
                    }
                    self.update_notice_id = None;
                }
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
                // 在途检查升级为手动来源：回执按手动口径反馈，不重复发请求
                if self.update_check_in_flight {
                    if manual {
                        self.update_manual_check = true;
                        self.update.start_check();
                    }
                    return Task::none();
                }
                let allowed = if manual {
                    self.update.start_check()
                } else {
                    !cfg!(debug_assertions) && self.update.can_auto_check(self.global.auto_update)
                };
                if !allowed {
                    return Task::none();
                }
                self.update_manual_check = manual;
                self.update_check_in_flight = true;
                Task::perform(atelyx_core::updater::latest_release(), |result| {
                    Message::Update(UpdateMsg::Checked(result))
                })
            }
            UpdateMsg::Checked(result) => {
                let manual = std::mem::take(&mut self.update_manual_check);
                self.update_check_in_flight = false;
                // 检查期间关掉了开关：自动结果不再打扰
                if !manual && !self.global.auto_update {
                    return Task::none();
                }
                match result {
                    Ok(release) => {
                        match self.update.apply_check(
                            env!("CARGO_PKG_VERSION"),
                            self.update_skipped.as_deref(),
                            release,
                            manual,
                        ) {
                            Some(update::CheckOutcome::Available { version }) => {
                                let skip = version.clone();
                                self.set_update_notice(
                                    NoticeLevel::Info,
                                    format!("发现新版本 {version}"),
                                    None,
                                    move |_| {
                                        vec![
                                            ("下载".into(), Message::Update(UpdateMsg::Download)),
                                            ("跳过".into(), Message::Update(UpdateMsg::Skip { version: skip })),
                                        ]
                                    },
                                );
                            }
                            // 有新版本但本机平台无安装包：手动检查转开发布页，静默检查同一版本只提示一次
                            Some(update::CheckOutcome::NoAsset { version, page }) => {
                                let reported =
                                    self.update_no_asset_notice.as_deref() == Some(version.as_str());
                                self.update_no_asset_notice = Some(version);
                                if manual {
                                    return self.open_link(&page, LinkForm::External);
                                }
                                if !reported {
                                    self.notify(NoticeLevel::Info, "发现新版本，请到发布页下载");
                                }
                            }
                            Some(update::CheckOutcome::UpToDate) if manual => {
                                self.notify(NoticeLevel::Info, "已是最新版本");
                            }
                            Some(update::CheckOutcome::UpToDate) | None => {}
                        }
                    }
                    Err(e) => {
                        self.update.fail_check();
                        if manual {
                            self.notify(NoticeLevel::Error, e);
                        }
                    }
                }
                Task::none()
            }
            UpdateMsg::Skip { version } => {
                // 只处理当前可下载版本；被更高版本替换后的迟到消息直接丢弃
                let current = match &self.update {
                    update::UpdateState::Available { version, .. } => Some(version.clone()),
                    _ => None,
                };
                if current.as_deref() == Some(version.as_str()) {
                    self.update_skipped = Some(version);
                    self.update = update::UpdateState::Idle;
                    self.dismiss_update_notice();
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
            FollowUp::Created { .. } => {}
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
                        if let Some(doc) = &mut self.doc_open {
                            if doc.path == *from || doc.path.starts_with(&format!("{from}/")) {
                                doc.path = n.clone();
                            }
                        }
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
                        self.ui.last_table_file = Some(n.clone());
                        self.plugins.on_table_changed(Some(&n));
                        self.schedule_persist();
                    }
                }
                if let Some(f) = &self.ui.last_canvas_file {
                    let next = if f == from {
                        Some(to.clone())
                    } else if f.starts_with(&format!("{from}/")) {
                        Some(f.replacen(from, to, 1))
                    } else {
                        None
                    };
                    if let Some(n) = next {
                        self.ui.last_canvas_file = Some(n);
                        self.schedule_persist();
                        // 插件按打开文件集合更新会话路径，不重发会继续按旧路径保存
                        self.plugins.on_open_files_changed();
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
                        space_actions::collab_unbind_doc(self);
                    }
                    self.ui.last_note_file = None;
                    self.doc_text.clear();
                    self.doc_dirty = false;
                    self.doc_save_error = None;
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
                    self.plugins.on_table_changed(None);
                    self.schedule_persist();
                    if matches!(self.repo_kind, RepoKind::Space(_)) {
                        space_actions::send_presence(self);
                    }
                }
                let hit_canvas = self.ui.last_canvas_file.as_deref().is_some_and(|f| {
                    if *is_dir {
                        f == rel || f.starts_with(&format!("{rel}/"))
                    } else {
                        f == rel
                    }
                });
                if hit_canvas {
                    self.ui.last_canvas_file = None;
                                            self.notify_open_files();
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

    /// 目录改名/移动后的文件树子目录缓存按前缀换路（精确命中与 `from/` 前缀两种）。
    fn follow_children_cache(&mut self, from: &str, to: &str) {
        let prefix = format!("{from}/");
        self.children = self
            .children
            .drain()
            .map(|(k, v)| {
                if k == from {
                    (to.to_string(), v)
                } else if let Some(rest) = k.strip_prefix(&prefix) {
                    (format!("{to}/{rest}"), v)
                } else {
                    (k, v)
                }
            })
            .collect();
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

    /// 文件树 / 布局 / 场景的改名状态：输入框渲染在主窗口。
    fn rename_state(&self, target: RenameTarget, value: String) -> RenameState {
        RenameState {
            target,
            value,
            window: self.main_id.unwrap_or_else(iced::window::Id::unique),
        }
    }

    /// 进入布局改名会话：`from_press` = 由同一按压触发（tab 双击），
    /// 该按压随后的全局 Press 须按行内按压吞掉，否则会话刚建立就被失焦提交。
    fn begin_layout_rename(&mut self, id: String, from_press: bool) -> Task<Message> {
        if id == layout::HOME_LAYOUT_ID {
            return Task::none();
        }
        self.dismiss_popup();
        // 同一布局已在改名 = 双击落在输入框里，保留草稿不重置
        if self.rename.as_ref().is_some_and(|r| r.target == RenameTarget::Layout(id.clone())) {
            return Task::none();
        }
        // 另起改名：弹层内按压不触发失焦提交，先结算在途草稿，避免静默丢弃
        let commit = if self.rename.is_some() {
            self.update(Message::LayoutRenameCommit)
        } else {
            Task::none()
        };
        let Some(name) = self
            .active_scene()
            .and_then(|s| s.layouts.iter().find(|l| l.id == id))
            .map(|l| l.name.clone())
        else {
            return commit;
        };
        if from_press {
            self.rename_press_inside = true;
        }
        self.rename = Some(self.rename_state(RenameTarget::Layout(id), name));
        // 焦点先于结算任务：结算可能含等待落盘的 gate，不应延后输入聚焦
        rename_focus_task().chain(commit)
    }

    /// 文件树重命名/新建草稿提交：按目标种类分派到 core 的写入口；
    /// 新建与改名对同名做 `-2`/`-3` 加序号去重（以磁盘事实为准，改名排除自身），
    /// 文件操作落地（gen 守卫 + 结果通知 + 刷新子项 + 打开文件跟随）。
    /// 空间模式表单草稿提交分派 space_actions（服务端建条目）。
    fn commit_file_rename(&mut self, r: RenameState) -> Task<Message> {
        if matches!(self.repo_kind, RepoKind::Space(_)) {
            if let RenameTarget::Draft { dir, kind } = &r.target {
                let Some(name) = draft_name(kind, &r.value) else { return Task::none() };
                return space_actions::space_create_entry(self, dir, kind.clone(), &name);
            }
            return space_actions::space_commit_rename(self, r);
        }
        let Some(root) = self.vault.root().ok() else { return Task::none() };
        let gen = self.epoch();
        match r.target {
            RenameTarget::Draft { dir, kind } => {
                let Some(name) = draft_name(&kind, &r.value) else { return Task::none() };
                        // 新条目落 dir 内部：完成后刷新 dir 本身的子项缓存
                        let parent = dir.clone();
                        Task::perform(
                            async move {
                                // 同名加序号：name、name-2、name-3 …（存在性以磁盘为准，目录同名同样让位）
                                let base = match &kind {
                                    DraftKind::Note => format!("{name}.md"),
                                    DraftKind::Folder => name.clone(),
                                    DraftKind::Table => format!("{name}.atb"),
                                    DraftKind::Canvas => format!("{name}.atlx"),
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
                                        DraftKind::Canvas => format!("{name}-{seq}.atlx"),
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
                                    DraftKind::Canvas => {
                                        // 初始 = 空画布（标题随文件名起稿）
                                        let now = now_secs() as i64;
                                        let canvas = atelyx_core::vault::CanvasFile {
                                            schema: vault::CANVAS_SCHEMA.into(),
                                            id: uuid::Uuid::new_v4().to_string(),
                                            title: name,
                                            nodes: vec![],
                                            edges: vec![],
                                            created_at: now,
                                            updated_at: now,
                                        };
                                        let path = vault::safe_join(&root, &rel, true)?;
                                        vault::write_canvas_file(&path, &canvas).map(|_| rel)
                                    }
                                    // 领域文档初始为空文件：骨架内容随插件打开时自定（协作空间同构，通用文件对待）
                                    DraftKind::Doc { .. } => {
                                        let path = vault::safe_join(&root, &rel, true)?;
                                        vault::atomic_write(&path, "").map(|_| rel)
                                    }
                                }
                            },
                            move |result| {
                                let follow = match &result {
                                    Ok(rel) => FollowUp::Created { rel: rel.clone() },
                                    Err(_) => FollowUp::None,
                                };
                                Message::OpDone(gen, parent, result, follow)
                            },
                        )
            }
            RenameTarget::File(old) => {
                // 输入框只给主名：扩展名恒取原名（重命名不改类型），手打的同名后缀先剥掉
                let old_name = old.rsplit('/').next().unwrap_or(&old);
                let Some(new_name) = vault::rename_file_name(&r.value, old_name) else { return Task::none() };
                let parent = parent_of(&old);
                let new = full_rel(&parent, &new_name);
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
                            FollowUp::Renamed { from: old, to: full, rewritten, announce: false },
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
                            FollowUp::Renamed { from: old_dir, to: full, rewritten, announce: false },
                        ),
                        Err(e) => Message::OpDone(gen, parent, Err(e), FollowUp::None),
                    },
                )
            }
            RenameTarget::Layout(_) | RenameTarget::Scene(_) => Task::none(),
        }
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

    /// 该内容键的图片是否可以发起载入：未失败，或失败已过退避且仍有重试余量。
    pub(crate) fn can_load_image(&self, key: &str) -> bool {
        match self.note_images_failed.get(key) {
            None => true,
            Some(retry) => !retry.exhausted() && Instant::now() >= retry.retry_at,
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

    /// 写文件树排序偏好（本地落仓库配置，空间走服务端配置补丁）。
    pub(crate) fn save_file_sort(&mut self, sort: String) -> Task<Message> {
        if matches!(self.repo_kind, RepoKind::Space(_)) {
            return space_actions::space_set_sort(self, &sort);
        }
        let Some(root) = self.vault.root().ok() else { return Task::none() };
        let patch = serde_json::json!({ "fileExplorerSort": sort });
        let gen = self.epoch();
        Task::perform(
            async move { vault::patch_vault_config(&root, &patch).map(|_| ()) },
            move |result| Message::ConfigSaved(gen, result),
        )
    }

    /// 写整份目录颜色表（本地落侧文件，空间走服务端配置补丁）。
    fn save_folder_colors(&mut self) -> Task<Message> {
        if matches!(self.repo_kind, RepoKind::Space(_)) {
            let colors: serde_json::Map<String, serde_json::Value> = self
                .folder_colors
                .iter()
                .map(|(path, color)| (path.clone(), serde_json::Value::String(color.clone())))
                .collect();
            return space_actions::space_patch_config(
                self,
                serde_json::json!({ "folderColors": colors }),
            );
        }
        let Some(root) = self.vault.root().ok() else { return Task::none() };
        let colors: HashMap<String, String> = self.folder_colors.clone();
        let gen = self.epoch();
        let writes = self.vault.writes.clone();
        Task::perform(
            async move {
                writes
                    .bulk(move || {
                        vault::write_folder_colors_file(&root, &colors)?;
                        Ok(colors)
                    })
                    .await
            },
            move |result| Message::FolderColorsLoaded(gen, result),
        )
    }

    /// 写整份提示词标记表（本地落侧文件，空间走服务端配置补丁）。
    fn save_prompt_notes(&mut self) -> Task<Message> {
        if matches!(self.repo_kind, RepoKind::Space(_)) {
            let notes: Vec<serde_json::Value> = self
                .prompt_notes
                .iter()
                .map(|path| serde_json::Value::String(path.clone()))
                .collect();
            return space_actions::space_patch_config(
                self,
                serde_json::json!({ "promptNotes": notes }),
            );
        }
        let Some(root) = self.vault.root().ok() else { return Task::none() };
        let notes = self.prompt_notes.clone();
        let gen = self.epoch();
        Task::perform(
            async move {
                vault::write_prompt_notes_file(&root, &notes)?;
                vault::read_prompt_notes_file(&root)
            },
            move |result| Message::PromptNotesLoaded(gen, result),
        )
    }

    /// 插件请求创建副本（`shell.duplicate`）：与本地菜单项走同一条复制链，
    /// 结果只回填给插件（宿主自己的文件面照常靠 `vault:file.changed` 与插件重取对齐）。
    pub(crate) fn duplicate_entry_reply(
        &mut self,
        request_id: u64,
        rel: &str,
        is_dir: bool,
    ) -> Task<Message> {
        if matches!(self.repo_kind, RepoKind::Space(_)) {
            return space_actions::space_duplicate_reply(self, request_id, rel, is_dir);
        }
        let Some(root) = self.vault.root().ok() else {
            self.plugins.resolve_shell_call(request_id, Err("未打开仓库".to_string()));
            return Task::none();
        };
        let gen = self.epoch();
        let rel = rel.to_string();
        Task::perform(
            async move {
                let parent = parent_of(&rel);
                let name = rel.rsplit('/').next().unwrap_or(rel.as_str()).to_string();
                let created = if is_dir {
                    // 副本场景源名必须视为已占用（exclude 传空串），否则首个候选 = 源名自复制
                    let target = dedupe_sibling(&root, &parent, &name, None, "")?;
                    vault::copy_folder(&root, &rel, &full_rel(&parent, &target))?;
                    full_rel(&parent, &target)
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
                    new_rel
                };
                Ok(created)
            },
            move |result| {
                Message::Plugins(settings_view::plugins::PluginsMsg::ShellDuplicateDone {
                    request_id,
                    gen,
                    result,
                })
            }
        )
    }

    /// 文件树结构刷新（工具栏刷新钮）：磁盘无监听，外部改动只能靠这里与切仓库感知。
    /// 本地 = 目录缓存整体重读（按当前展开集）+ 文件计数与笔记名册重取；
    /// 空间 = 整树重拉（服务端真源，切片缓存由回执重建）。
    fn refresh_file_tree(&mut self) -> Task<Message> {
        if matches!(self.repo_kind, RepoKind::Space(_)) {
            return space_actions::refresh_space_tree(self);
        }
        if self.vault_root.is_none() {
            return Task::none();
        }
        let expanded = self.ui.file_explorer_expanded.clone();
        // 展开集外的目录缓存先丢弃（收起状态看不到，避免留下过期切片）；
        // 可见目录保留旧值到重读回执，刷新期间不闪空
        self.children.retain(|dir, _| expanded.contains(dir));
        let mut task = Task::none();
        for dir in expanded {
            task = task.chain(self.load_children(&dir));
        }
        task.chain(self.count_files()).chain(self.refresh_note_files())
    }

    /// 拖拽悬停折叠目录到点自动展开：让「移进折叠目录的下一层」不必先松手再展开。
    /// 展开后该行不再是候选（`spring` 清空），落点保持为该目录。
    fn spring_open_dir(&mut self) -> Task<Message> {
        const SPRING: std::time::Duration = std::time::Duration::from_millis(600);
        let Some(d) = self.file_drag.as_mut() else { return Task::none() };
        let Some((dir, at)) = d.spring.clone() else { return Task::none() };
        if at.elapsed() < SPRING {
            return Task::none();
        }
        d.spring = None;
        if self.ui.file_explorer_expanded.iter().any(|e| e == &dir) {
            return Task::none();
        }
        self.ui.file_explorer_expanded.push(dir.clone());
        self.schedule_persist();
        // 发起拖拽的插件要跟着展开（它自己画树）：推一次「落点已不再是折叠候选」
        self.publish_drag_state();
        if self.children.contains_key(&dir) {
            Task::none()
        } else {
            self.load_children(&dir)
        }
    }

    /// 重读文件树（工具条刷新入口；磁盘无监听，外部改动只能靠刷新感知）。
    pub(crate) fn refresh_file_tree_task(&mut self) -> Task<Message> {
        self.refresh_file_tree()
    }

    /// 补齐尚未缓存的目录子项：引用候选走的是这份缓存，
    /// 新展开的目录不读一次就进不了候选。
    pub(crate) fn ensure_children_loaded(&mut self, dirs: &[String]) -> Task<Message> {
        let mut task = Task::none();
        for dir in dirs {
            if !self.children.contains_key(dir) {
                task = task.chain(self.load_children(dir));
            }
        }
        task
    }

    /// 插件请求打开文件夹选择器把某个文件夹作为仓库（工具条「打开文件夹」入口）。
    pub(crate) fn plugin_pick_vault(&mut self, request_id: u64) -> Task<Message> {
        self.plugins.resolve_shell_call(request_id, Ok(serde_json::json!({})));
        self.gate(PendingAfterFlush::PickVault)
    }

    /// 插件请求新建一个仓库条目：复用「新建草稿」那条链（同名自动加序号、按类型起空内容、
    /// 走同一写盘出口），只是名字由插件给出而不是就地输入。
    pub(crate) fn plugin_create_entry(
        &mut self,
        request_id: u64,
        params: atelyx_core::plugin_runtime::CreateEntryParams,
    ) -> Task<Message> {
        // 目录为空串 = 仓库根（插件在根上新建）
        let dir = if params.dir.is_empty() {
            String::new()
        } else {
            match vault::validate_plugin_path(&params.dir) {
                Ok(dir) => dir,
                Err(e) => {
                    self.plugins.resolve_shell_call(request_id, Err(e));
                    return Task::none();
                }
            }
        };
        let kind = match params.kind.as_str() {
            "note" => DraftKind::Note,
            "table" => DraftKind::Table,
            "canvas" => DraftKind::Canvas,
            "folder" => DraftKind::Folder,
            extension => DraftKind::Doc {
                extension: extension.to_string(),
                label: extension.to_string(),
            },
        };
        let Some(name) = draft_name(&kind, &params.name) else {
            self.plugins
                .resolve_shell_call(request_id, Err("条目名不能为空".to_string()));
            return Task::none();
        };
        self.plugins
            .resolve_shell_call(request_id, Ok(serde_json::json!({})));
        let target = RenameTarget::Draft { dir, kind };
        let mut state = self.rename_state(target, name);
        state.value = params.name.clone();
        self.rename = Some(state);
        let task = self.update(Message::LayoutRenameCommit);
        task
    }

    /// 插件请求切换容器：按清单里的 id 分派到进入仓库 / 进入空间两条既有链路。
    pub(crate) fn plugin_switch_container(
        &mut self,
        request_id: u64,
        params: atelyx_core::plugin_runtime::SwitchContainerParams,
    ) -> Task<Message> {
        self.plugins.resolve_shell_call(request_id, Ok(serde_json::json!({})));
        if let Some(root) = params.id.strip_prefix("local:") {
            return self.update(Message::SelectVault {
                path: PathBuf::from(root),
                from_boot: false,
            });
        }
        let Some(rest) = params.id.strip_prefix("space:") else { return Task::none() };
        let Some((server_url, space_id)) = rest.split_once('#') else { return Task::none() };
        let name = self
            .global
            .recent_spaces
            .iter()
            .find(|space| space.server_url == server_url && space.space_id == space_id)
            .map(|space| space.name.clone())
            .or_else(|| {
                self.space_list
                    .iter()
                    .find(|space| space.space_id == space_id)
                    .map(|space| space.name.clone())
            })
            .unwrap_or_else(|| space_id.to_string());
        self.update(Message::Space(crate::SpaceMsg::SelectSpace {
            server_url: server_url.to_string(),
            space_id: space_id.to_string(),
            name,
            from_boot: false,
        }))
    }

    /// 插件请求把容器移出最近列表：只改 global.json 的最近清单，磁盘内容与服务端数据不动。
    /// 激活中的容器拒绝（切走再移除）；来自账号的空间不进最近清单，只提示不影响列表。
    pub(crate) fn plugin_remove_container(
        &mut self,
        request_id: u64,
        params: atelyx_core::plugin_runtime::RemoveContainerParams,
    ) -> Task<Message> {
        let result = self.remove_container_by_id(&params.id);
        let reply = match &result {
            Ok(_) => Ok(serde_json::json!({})),
            Err(e) => Err(e.clone()),
        };
        self.plugins.resolve_shell_call(request_id, reply);
        match result {
            Ok(ContainerRemoval::Removed) => {
                self.save_global_now();
                Task::none()
            }
            Ok(ContainerRemoval::NotListed) => {
                self.notify(NoticeLevel::Info, "该空间来自服务器账号，列表展示不受影响");
                Task::none()
            }
            Err(e) => {
                self.notify(NoticeLevel::Error, e);
                Task::none()
            }
        }
    }

    /// 按容器 id 移出最近列表；失败给可读原因（激活中 / 未知 id / 不是容器）。
    fn remove_container_by_id(&mut self, id: &str) -> Result<ContainerRemoval, String> {
        if let Some(root) = id.strip_prefix("local:") {
            let active = self.vault_root.as_ref().is_some_and(|r| r.to_string_lossy() == root);
            if active {
                return Err("当前仓库已激活，请先切换到其他容器再移除".to_string());
            }
            if !self.global.recent_vaults.iter().any(|v| v.root == root) {
                return Err("该仓库不在最近列表".to_string());
            }
            self.global.remove_vault(root);
            return Ok(ContainerRemoval::Removed);
        }
        let Some(rest) = id.strip_prefix("space:") else {
            return Err(format!("未知的容器标识：{id}"));
        };
        let Some((server_url, space_id)) = rest.split_once('#') else {
            return Err(format!("未知的容器标识：{id}"));
        };
        let active = matches!(
            &self.repo_kind,
            RepoKind::Space(session)
                if session.server_url == server_url && session.space_id == space_id
        );
        if active {
            return Err("当前空间已激活，请先切换到其他容器再移除".to_string());
        }
        if !self
            .global
            .recent_spaces
            .iter()
            .any(|space| space.server_url == server_url && space.space_id == space_id)
        {
            return Ok(ContainerRemoval::NotListed);
        }
        self.global.remove_space(server_url, space_id);
        Ok(ContainerRemoval::Removed)
    }

    /// 插件请求取色器：宿主弹自己的取色器，用户选定的颜色经回执交回插件。
    /// 回执只在提交 / 取消时给一次（取色器走值通道），开弹层不算已应答。
    pub(crate) fn plugin_pick_color(
        &mut self,
        request_id: u64,
        params: atelyx_core::plugin_runtime::PickColorParams,
    ) -> Task<Message> {
        let window = self.main_id.unwrap_or_else(iced::window::Id::unique);
        Task::done(Message::ColorPicker(crate::color_picker_actions::ColorPickerMsg::OpenPlugin {
            window,
            request_id,
            initial: params.initial,
        }))
    }

    /// 插件请求弹出宿主菜单：条目来自插件，渲染 / 定位 / 外点关闭沿用宿主弹层。
    fn open_plugin_menu(
        &mut self,
        request_id: u64,
        plugin_id: String,
        params: atelyx_core::plugin_runtime::MenuOpenParams,
    ) -> Task<Message> {
        let window = self.main_id.unwrap_or_else(iced::window::Id::unique);
        let point = self.cursor_for(window);
        self.plugin_menu = Some((plugin_id, params));
        self.popup = Some((
            window,
            layout_view::PopupOpen {
                point,
                flip_y: None,
                state: layout_view::PopupState::PluginMenu,
                min_width: 0.0,
            },
        ));
        self.popup_pane = 0;
        self.plugin_menu_query.clear();
        self.plugins.resolve_ui_call(request_id, Ok(()));
        Task::none()
    }

    /// 宿主菜单关闭：把选中项交回发起插件（点外点 / Esc = 未选中）。
    fn close_plugin_menu(&mut self, selected: Option<String>) -> Task<Message> {
        let Some((plugin_id, _)) = self.plugin_menu.take() else { return Task::none() };
        self.plugin_menu_query.clear();
        self.dismiss_popup();
        self.plugins.notify_menu_closed(&plugin_id, selected);
        Task::none()
    }

    /// 插件发起一次文件拖拽：ghost、落点判定与自动展开沿用壳内同一套会话。
    /// `surface_id` 记下来，落点状态按面回推给发起插件（驱动行高亮与拒绝提示）。
    fn start_plugin_drag(
        &mut self,
        request_id: u64,
        plugin_id: String,
        params: atelyx_core::plugin_runtime::DragStartParams,
    ) -> Task<Message> {
        let surface_owner = (plugin_id, params.surface_id.clone());
        if self.file_drag.is_some() {
            self.plugins
                .resolve_ui_call(request_id, Err("已有拖拽会话进行中".to_string()));
            return Task::none();
        }
        let rel = match vault::validate_plugin_path(&params.item.path) {
            Ok(rel) if !rel.is_empty() => rel,
            Ok(_) => {
                self.plugins
                    .resolve_ui_call(request_id, Err("仓库根不能拖动".to_string()));
                return Task::none();
            }
            Err(e) => {
                self.plugins.resolve_ui_call(request_id, Err(e));
                return Task::none();
            }
        };
        let label = params
            .item
            .label
            .filter(|label| !label.trim().is_empty())
            .unwrap_or_else(|| rel.rsplit('/').next().unwrap_or(&rel).to_string());
        let window = self.main_id.unwrap_or_else(iced::window::Id::unique);
        let point = self.cursor_for(window);
        let current = self
            .os_positions
            .get(&window)
            .map(|w| Point::new(w.x + point.x, w.y + point.y))
            .unwrap_or(point);
        self.file_drag = Some(FileDrag {
            from: rel,
            is_dir: params.item.is_dir,
            label,
            current,
            cursor_window: window,
            reject: None,
            spring: None,
            hover: None,
        });
        self.file_drag_target = None;
        self.file_drag_surface = Some(surface_owner.clone());
        self.plugins.resolve_ui_call(request_id, Ok(()));
        Task::none()
    }

    /// 插件面上报落点候选：能不能落由宿主按同一套规则复核（与壳内行悬停同一条判定）。
    fn plugin_drag_hover(
        &mut self,
        request_id: u64,
        params: atelyx_core::plugin_runtime::DragHoverParams,
    ) -> Task<Message> {
        let Some(d) = self.file_drag.as_ref() else {
            self.plugins.resolve_ui_call(request_id, Ok(()));
            return Task::none();
        };
        let Some(target) = params.target.clone() else {
            self.clear_plugin_drag_hover();
            self.plugins.resolve_ui_call(request_id, Ok(()));
            return Task::none();
        };
        let target = match vault::validate_plugin_path(&target) {
            Ok(target) => target,
            Err(e) => {
                self.plugins.resolve_ui_call(request_id, Err(e));
                return Task::none();
            }
        };
        let verdict = file_drop_check(&d.from, d.is_dir, &target, params.is_dir);
        // 折叠目录悬停 = 自动展开候选（与壳内行悬停同一条：离开或换目标即作废）
        let collapsed = !target.is_empty()
            && !self.ui.file_explorer_expanded.iter().any(|e| e == &target);
        if let Some(d) = self.file_drag.as_mut() {
            match verdict {
                Ok(()) => {
                    self.file_drag_target = Some(target.clone());
                    d.reject = None;
                    d.spring = collapsed.then(|| (target, Instant::now()));
                }
                Err(why) => {
                    self.file_drag_target = None;
                    d.reject = Some((target, why));
                    d.spring = None;
                }
            }
        }
        self.plugins.resolve_ui_call(request_id, Ok(()));
        self.publish_drag_state();
        Task::none()
    }

    /// 插件结束拖拽（`cancelled` = 不落位）：与壳内释放走同一条落位链。
    fn end_plugin_drag(
        &mut self,
        request_id: u64,
        params: atelyx_core::plugin_runtime::DragEndParams,
    ) -> Task<Message> {
        self.plugins.resolve_ui_call(request_id, Ok(()));
        self.file_drag_surface = None;
        let Some(d) = self.file_drag.take() else { return Task::none() };
        if params.cancelled {
            self.file_drag_target = None;
            return Task::none();
        }
        self.resolve_file_drop(d)
    }

    /// 清除插件面的落点状态（指针离开行）。
    fn clear_plugin_drag_hover(&mut self) {
        if let Some(d) = self.file_drag.as_mut() {
            d.hover = None;
            d.reject = None;
            d.spring = None;
        }
        self.file_drag_target = None;
        self.publish_drag_state();
    }

    /// 取消当前文件拖拽会话（Esc、发起面消失、发起插件停下）：先推「无落点」让插件清高亮，再收会话。
    pub(crate) fn cancel_file_drag(&mut self) {
        self.file_drag = None;
        self.file_drag_target = None;
        self.publish_drag_state();
        self.file_drag_surface = None;
    }

    /// 把当前落点状态推给发起拖拽的插件（驱动行高亮、拒绝色与等待展开提示）。
    pub(crate) fn publish_drag_state(&self) {
        let Some((plugin_id, _)) = self.file_drag_surface.clone() else { return };
        let (target, reject, springing) = match self.file_drag.as_ref() {
            None => (None, None, false),
            Some(d) => (
                self.file_drag_target.clone(),
                d.reject.as_ref().map(|(_, why)| reject_text(*why).to_string()),
                d.spring.is_some(),
            ),
        };
        self.plugins.notify_plugin_event(
            &plugin_id,
            "drag:state",
            &atelyx_core::plugin_runtime::DragStateParams { target, reject, springing },
        );
    }

    /// 拖拽释放落点 → 移动（挂起输入先落盘：引用同步会代写打开中的笔记，防回写覆盖）。
    fn resolve_file_drop(&mut self, d: FileDrag) -> Task<Message> {
        let Some(target_dir) = self.file_drag_target.take() else { return Task::none() };
        // 释放前按同一判据复核：视图重建滞后可能让落点停在不可放的位置
        if file_drop_check(&d.from, d.is_dir, &target_dir, true).is_err() {
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
                    FollowUp::Renamed { from, to: full, rewritten, announce: true },
                ),
                Err(e) => Message::OpDone(gen, to_dir, Err(e), FollowUp::None),
            },
        )
    }

    /// 插件视角的当前选区：偏移相对正文（frontmatter 之后）。未打开文档回空选区。
    pub(crate) fn editor_selection(&self) -> EditorSelection {
        let path = self.ui.last_note_file.clone();
        if self.doc_active {
            let base = atelyx_core::frontmatter::block_end(&self.doc_text).unwrap_or(0);
            let body = &self.doc_text[base..];
            let (anchor, head) = self.doc_selection.unwrap_or((base, base));
            let anchor = crate::doc_session::clamp_boundary(&self.doc_text, anchor);
            let head = crate::doc_session::clamp_boundary(&self.doc_text, head);
            let from = anchor.min(head).saturating_sub(base).min(body.len());
            let to = anchor.max(head).saturating_sub(base).min(body.len());
            return EditorSelection { path, text: body[from..to].to_string(), from, to };
        }
        EditorSelection::empty(path)
    }

    /// 插件替换当前选区（空串 = 删除选区），走与插件编辑同一出口：三方合并、置脏与自动保存期限一致。
    pub(crate) fn editor_replace_selection(&mut self, text: String) -> Result<(), String> {
        if self.ui.last_note_file.is_none() {
            return Err("未打开文档".to_string());
        }
        if self.doc_active {
            if !crate::doc_session::can_write(self) {
                return Err("该空间为只读身份，无法编辑文档".into());
            }
            let (anchor, head) = self
                .doc_selection
                .ok_or_else(|| "文档没有选区".to_string())?;
            let anchor = crate::doc_session::clamp_boundary(&self.doc_text, anchor);
            let head = crate::doc_session::clamp_boundary(&self.doc_text, head);
            let from = anchor.min(head);
            let to = anchor.max(head);
            let mut next = String::with_capacity(self.doc_text.len() + text.len());
            next.push_str(&self.doc_text[..from]);
            next.push_str(&text);
            next.push_str(&self.doc_text[to..]);
            let caret = from + text.len();
            let published = self
                .apply_doc_edit(next, Some((caret, caret)))
                .unwrap_or_else(|| self.doc_text.clone());
            crate::doc_session::publish_state(self, Some(published));
            return Ok(());
        }
        Err("文档不在编辑状态".to_string())
    }

    /// 命令候选键分派：录制态先吃按键；注册表按序先到先得，画布域按画布焦点自守卫。
    /// 注册表外的 Ctrl+S（保存）保持硬编码。
    fn on_key_command(&mut self, key: keyboard::Key, mods: keyboard::Modifiers) -> Task<Message> {
        if self.shortcut_recording.is_some() {
            return self.on_shortcut_key(key, mods);
        }
        let resolved = commands::resolve(&self.global.command_shortcuts);
        for (id, binding) in &resolved {
            if !commands::matches(binding, mods, &key) {
                continue;
            }
            if matches!(id.scope(), commands::Scope::Canvas) && self.canvas_key_ready() {
                let command = match id {
                    commands::CommandId::CanvasUndo => "undo",
                    commands::CommandId::CanvasRedo => "redo",
                    commands::CommandId::CanvasCopy => "copy",
                    commands::CommandId::CanvasPaste => "paste",
                    commands::CommandId::CanvasSelectAll => "select-all",
                    commands::CommandId::CanvasClear => "delete",
                };
                self.send_canvas_command(command);
                return Task::none();
            }
        }
        // 重做别名：redo 未被重定义时 mod+shift+z 沿用（画布直接分派）
        if (mods.control() || mods.logo())
            && mods.shift()
            && !mods.alt()
            && matches!(&key, keyboard::Key::Character(c) if c.to_lowercase() == "z")
            && self.canvas_key_ready()
            && commands::redo_alias_active(&self.global.command_shortcuts)
        {
            self.send_canvas_command("redo");
            return Task::none();
        }
        // 退格 = 删除选中的既有别名（删除键位保持默认 delete 时沿用）
        let clear_binding = |id: commands::CommandId| {
            resolved
                .iter()
                .find(|(c, _)| *c == id)
                .map(|(_, b)| b.clone())
        };
        if mods.is_empty()
            && matches!(key, keyboard::Key::Named(keyboard::key::Named::Backspace))
            && clear_binding(commands::CommandId::CanvasClear).as_deref() == Some("delete")
            && self.canvas_key_ready()
        {
            self.send_canvas_command("delete");
            return Task::none();
        }
        if (mods.control() || mods.logo()) && !mods.alt() {
            if let keyboard::Key::Character(c) = &key {
                if c.eq_ignore_ascii_case("s") {
                    return self.handle(Message::Save);
                }
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
                self.notify(NoticeLevel::Success, "打开链接");
                self.open_file(&rel, OpenMode::Reveal)
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

    fn title(&self, id: window::Id) -> String {
        if self.main_id == Some(id) {
            return "Atelyx".into();
        }
        // 插件窗口标题 = 插件在 window.open 声明的 title
        if let Some(entry) = self.plugin_windows.get(&id) {
            return entry.title.clone();
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
        // 心跳仅在有待过期通知、提醒条在途、悬停提示在途（距离收起判定）、
        // 文件拖拽悬停折叠目录（自动展开判定）、笔记在场有待发状态或有对端
        // （补发 / 保活 / 远端剔除搭载）、快捷键页展示中（冲突警示两态闪烁）时挂载
        let need_tick = !self.notifications.is_empty()
            || self.rename_notice.is_some()
            || self.tooltip.is_some()
            // 文件拖拽悬停折叠目录：自动展开挂起需心跳到点判定
            || self.file_drag.as_ref().is_some_and(|d| d.spring.is_some())
            // 笔记在场：有待发状态或有对端时需要周期拍；单人无对端不挂拍，避免空闲被 10Hz 重绘
            || self
                .doc_presence
                .as_ref()
                .is_some_and(|ch| ch.has_pending() || ch.has_peers())
            || !self.chat.renaming.is_empty()
            || (self.settings_open && self.settings_tab == "shortcuts");
        Subscription::batch([
            iced::time::every(PERSIST_TICK).map(|_| Message::PersistTick),
            // 运行期轮询：开着自动检查且非开发构建时按固定间隔静默检查
            if self.global.auto_update && !cfg!(debug_assertions) {
                iced::time::every(update::POLL_INTERVAL)
                    .map(|_| Message::Update(update::UpdateMsg::Check { manual: false }))
            } else {
                Subscription::none()
            },
            if need_tick {
                iced::time::every(std::time::Duration::from_millis(100)).map(|_| Message::UiTick)
            } else {
                Subscription::none()
            },
            // 帧拍：只有存在声明了帧率的画布或整面帧时才挂（静态面不产生周期唤醒）
            if self.plugins.has_frame_ticks() {
                iced::time::every(FRAME_TICK).map(|_| Message::PluginFrameTick)
            } else {
                Subscription::none()
            },
            // 方向键落点调整分割把手（语义在 NavKey 处理器分派）。
            // 已被控件捕获的左右键（文本输入 / 编辑器光标）不再全局派发：输入框里的光标
            // 移动不得连带调布局。上下键照发，由 NavKey 处理器按输入态门控。
            iced::event::listen_with(|event, status, window| match event {
                Event::Keyboard(keyboard::Event::ModifiersChanged(m)) => {
                    Some(Message::ModsChanged(m))
                }
                Event::Keyboard(keyboard::Event::KeyPressed {
                    key: keyboard::Key::Named(keyboard::key::Named::ArrowLeft),
                    modifiers,
                    ..
                }) if modifiers.is_empty() && status == iced::event::Status::Ignored => {
                    Some(Message::NavKey(true, false))
                }
                Event::Keyboard(keyboard::Event::KeyPressed {
                    key: keyboard::Key::Named(keyboard::key::Named::ArrowRight),
                    modifiers,
                    ..
                }) if modifiers.is_empty() && status == iced::event::Status::Ignored => {
                    Some(Message::NavKey(true, true))
                }
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
                Event::Keyboard(keyboard::Event::KeyPressed {
                    key: keyboard::Key::Named(keyboard::key::Named::Escape),
                    ..
                }) => Some(Message::Escape(window)),
                // 正文选区复制：输入部件已捕获的 Ctrl/Cmd+C 走其原生复制；未命中笔记 /
                // 对话选区时在处理器里回落命令路由（画布复制等）
                Event::Keyboard(keyboard::Event::KeyPressed {
                    key: keyboard::Key::Character(c),
                    modifiers,
                    ..
                }) if c.eq_ignore_ascii_case("c")
                    && (modifiers.control() || modifiers.logo())
                    && status == iced::event::Status::Ignored =>
                {
                    Some(Message::CopyKey { key: keyboard::Key::Character(c), mods: modifiers })
                }
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
                    // 外部文件拖入：路由到画布对话卡（面板由插件自理）
                    Some(Message::Chat(chat_actions::ChatMsg::FileDropped(path)))
                }
                _ => None,
            }),
            self.space_pump_subscription(),
            self.plugin_events_subscription(),
            self.plugin_host_requests_subscription(),
            Subscription::run_with(self.tray.spec(), tray::events_stream).map(Message::Tray),
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
            platform: Some(space_actions::client_platform().to_string()),
            version: Some(space_actions::CLIENT_VERSION.to_string()),
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

/// 一次插件编辑的三方合并结果：返回（合并正文, 下一笔的祖先, 需要回填的校正）。
/// 祖先为 None 表示它与当前正文相同，不必再存一份全文。
fn merge_doc_edit(
    base: Option<String>,
    before: &str,
    text: String,
) -> (String, Option<String>, Option<String>) {
    match base {
        Some(base) if base != before => {
            let merged = atelyx_core::merge3::merge3(
                &base,
                &text,
                before,
                atelyx_core::merge3::ConflictSide::Ours,
            );
            // 插件手里仍是它提交的旧文：只有发生校正时才需要把它留作下一笔的祖先。
            // 先推进到合并结果会让未及应用校正的下一笔以过期祖先合并，把远端内容改回去。
            let correction = (merged != text).then(|| merged.clone());
            let next_base = if correction.is_some() { Some(text) } else { None };
            (merged, next_base, correction)
        }
        // 没有远端改动：插件提交的全文即最新，祖先与正文相同
        _ => (text, None, None),
    }
}

#[cfg(test)]
mod doc_edit_tests {
    use super::merge_doc_edit;

    #[test]
    fn stable_edit_stores_no_duplicate_base() {
        let (merged, base, correction) =
            merge_doc_edit(None, "old", "old + new".to_string());
        assert_eq!(merged, "old + new");
        assert!(base.is_none(), "祖先与正文相同，不另存全文");
        assert!(correction.is_none());
    }

    #[test]
    fn stale_base_equal_to_before_needs_no_merge() {
        let (merged, base, correction) =
            merge_doc_edit(Some("same".into()), "same", "same + new".to_string());
        assert_eq!(merged, "same + new");
        assert!(base.is_none());
        assert!(correction.is_none());
    }

    #[test]
    fn remote_change_keeps_plugin_text_as_next_ancestor() {
        let base = Some("line1\nline2\nline3\n".to_string());
        let before = "remote1\nline2\nline3\n";
        let text = "line1\nline2\nplugin3\n".to_string();
        let expected_base = text.clone();
        let (merged, next_base, correction) = merge_doc_edit(base, before, text);
        assert_eq!(merged, "remote1\nline2\nplugin3\n");
        assert_eq!(next_base.as_deref(), Some(expected_base.as_str()));
        assert_eq!(correction.as_deref(), Some(merged.as_str()));
    }

    #[test]
    fn plugin_applying_remote_change_needs_no_correction() {
        let base = Some("line1\nline2\nline3\n".to_string());
        let before = "remote1\nline2\nline3\n";
        let text = "remote1\nline2\nplugin3\n".to_string();
        let (merged, next_base, correction) = merge_doc_edit(base, before, text);
        assert_eq!(merged, "remote1\nline2\nplugin3\n");
        assert!(next_base.is_none(), "合并结果即插件正文时不必保留祖先");
        assert!(correction.is_none());
    }
}

#[cfg(test)]
mod file_drop_tests {
    use super::{file_drop_check, DropReject};

    #[test]
    fn accepts_other_folders_and_root() {
        // 文件 → 别的文件夹 / 根
        assert_eq!(file_drop_check("a.md", false, "docs", true), Ok(()));
        assert_eq!(file_drop_check("docs/a.md", false, "", true), Ok(()));
        // 文件夹 → 别的文件夹 / 根
        assert_eq!(file_drop_check("docs", true, "notes", true), Ok(()));
        assert_eq!(file_drop_check("docs/a", true, "", true), Ok(()));
        // 名称前缀相同但不是子树：仍可放
        assert_eq!(file_drop_check("docs", true, "docs2", true), Ok(()));
    }

    #[test]
    fn rejects_file_rows() {
        assert_eq!(
            file_drop_check("a.md", false, "b.md", false),
            Err(DropReject::NotADir)
        );
        assert_eq!(
            file_drop_check("docs", true, "docs/b.md", false),
            Err(DropReject::NotADir)
        );
    }

    #[test]
    fn rejects_self_subtree_and_ancestor() {
        assert_eq!(
            file_drop_check("docs", true, "docs", true),
            Err(DropReject::SelfTree)
        );
        assert_eq!(
            file_drop_check("docs", true, "docs/sub", true),
            Err(DropReject::SelfTree)
        );
        assert_eq!(
            file_drop_check("docs/sub", true, "docs", true),
            Err(DropReject::SameDir)
        );
    }

    #[test]
    fn rejects_source_dir_itself() {
        assert_eq!(
            file_drop_check("a.md", false, "", true),
            Err(DropReject::SameDir)
        );
        assert_eq!(
            file_drop_check("docs/a.md", false, "docs", true),
            Err(DropReject::SameDir)
        );
        assert_eq!(
            file_drop_check("docs", true, "", true),
            Err(DropReject::SameDir)
        );
    }
}

#[cfg(test)]
mod restore_open_tests {
    use super::restore_open_requests;
    use crate::plugin_actions::{reveal_op, OpenMode};
    use atelyx_core::layout::{self, AppUiState, LayoutOp, NOTES_VIEW};

    fn default_ui() -> AppUiState {
        let mut ui = AppUiState::default();
        layout::normalize(&mut ui);
        ui
    }

    fn first_panel_id(ui: &AppUiState) -> String {
        let active = layout::active_layout(ui);
        let mut panels = Vec::new();
        layout::collect_panels(&active.tree, &mut panels);
        panels[0].node_id().to_string()
    }

    fn tab_count(ui: &AppUiState) -> usize {
        let active = layout::active_layout(ui);
        let mut tabs = Vec::new();
        layout::collect_tabs(&active.tree, &mut tabs);
        tabs.len()
    }

    fn layouts_with(ui: &AppUiState, view: &str) -> usize {
        let mut count = ui
            .scenes
            .iter()
            .flat_map(|scene| scene.layouts.iter())
            .filter(|layout| {
                let mut tabs = Vec::new();
                layout::collect_tabs(&layout.tree, &mut tabs);
                tabs.iter().any(|tab| tab.view == view)
            })
            .count();
        let active_is_home = match ui.active_layout_id.as_deref() {
            None | Some(layout::HOME_LAYOUT_ID) => true,
            _ => false,
        };
        if active_is_home {
            let active = layout::active_layout(ui);
            let mut tabs = Vec::new();
            layout::collect_tabs(&active.tree, &mut tabs);
            if tabs.iter().any(|tab| tab.view == view) {
                count += 1;
            }
        }
        count
    }

    fn add_view(ui: &mut AppUiState, view: &str) {
        let panel_id = first_panel_id(ui);
        layout::apply_layout_op(
            ui,
            &LayoutOp::AddView { panel_id, view: view.to_string() },
        );
    }

    #[test]
    fn restore_requests_open_files_without_touching_layout() {
        let mut ui = default_ui();
        ui.last_note_file = Some("a.md".into());
        ui.last_table_file = Some("b.atb".into());
        ui.last_canvas_file = Some("c.atlx".into());
        let before = tab_count(&ui);
        let requests = restore_open_requests(&ui, true, true);
        assert_eq!(
            requests,
            vec![
                ("a.md".to_string(), OpenMode::Restore),
                ("b.atb".to_string(), OpenMode::Restore),
                ("c.atlx".to_string(), OpenMode::Restore),
            ]
        );
        // 只有打开请求：主页没有笔记标签，也不因恢复新增
        assert_eq!(tab_count(&ui), before);
        assert!(!OpenMode::Restore.reveals(), "恢复不新建 / 不激活标签");
        assert!(!OpenMode::Restore.persists(), "恢复不落盘");
        assert!(!OpenMode::Restore.records_recent(), "恢复不重排最近打开");
        // 运行中切换容器、关闭「自动恢复」都不产生请求
        assert!(restore_open_requests(&ui, false, true).is_empty());
        assert!(restore_open_requests(&ui, true, false).is_empty());
    }

    #[test]
    fn restore_keeps_existing_tab_without_layout_change() {
        let mut ui = default_ui();
        add_view(&mut ui, NOTES_VIEW);
        let before = tab_count(&ui);
        let _ = restore_open_requests(&ui, true, true);
        assert_eq!(tab_count(&ui), before);
        // 用户主动打开才会激活已有标签；恢复路径不调用这条
        assert!(matches!(
            reveal_op(&ui, NOTES_VIEW),
            Some(LayoutOp::SetActive { .. })
        ));
    }

    #[test]
    fn user_open_adds_tab_only_when_view_missing() {
        let ui = default_ui();
        assert!(matches!(
            reveal_op(&ui, NOTES_VIEW),
            Some(LayoutOp::AddView { .. })
        ));
        assert!(OpenMode::Reveal.reveals());
        assert!(!OpenMode::Background.reveals(), "后台打开不动布局");
        assert!(OpenMode::Background.persists() && OpenMode::Background.records_recent());
    }

    #[test]
    fn view_in_multiple_layouts_is_activated_without_duplicate_tab() {
        let mut ui = default_ui();
        // 默认「笔记」「表格」两套布局都带笔记视图（主页不带）
        assert_eq!(layouts_with(&ui, NOTES_VIEW), 2);
        add_view(&mut ui, NOTES_VIEW);
        assert_eq!(layouts_with(&ui, NOTES_VIEW), 3);
        assert!(
            matches!(reveal_op(&ui, NOTES_VIEW), Some(LayoutOp::SetActive { .. })),
            "多布局同视图只激活一个标签，不重复新建"
        );
        // 同一视图存在于多个布局，恢复仍只产生一次打开请求
        ui.last_note_file = Some("a.md".into());
        assert_eq!(restore_open_requests(&ui, true, true).len(), 1);
    }
}
