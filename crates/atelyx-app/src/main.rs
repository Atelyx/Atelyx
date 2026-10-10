//! Atelyx 原生壳：daemon 多窗口 + 布局模型（场景/布局/面板树）+ files/note 视图。
//!
//! 布局状态真源 = atelyx-core::layout（与老壳共用同一 ui-state.json 磁盘格式，
//! 本壳只读不改格式）；本壳负责窗口副作用（OS 窗口、落盘防抖、输入路由）。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod layout_view;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use atelyx_core::layout::{
    self, apply_layout_op, normalize, op_passes_shape_check, AppUiState, DetachedWindow,
    LayoutOp, WindowBounds, UI_STATE_SCHEMA,
};
use atelyx_core::vault;
use iced::widget::{space, text_editor};
use iced::window::{self, Level, Mode, Position, Settings};
use iced::{keyboard, mouse, Element, Event, Point, Size, Subscription, Task, Theme};

use layout_view::{enumerate_dividers, RenameState, RenameTarget, UiMetrics};

/// 撕裂窗口建窗参数：bounds 恢复位置 + 条目 OS 选项（不进任务栏）。
fn panel_settings(w: &DetachedWindow) -> Settings {
    let mut settings = Settings {
        size: Size::new(w.bounds.width.max(200.0) as f32, w.bounds.height.max(160.0) as f32),
        position: Position::Specific(Point::new(w.bounds.x as f32, w.bounds.y as f32)),
        exit_on_close_request: false,
        ..Settings::default()
    };
    #[cfg(windows)]
    {
        settings.platform_specific.skip_taskbar = w.options.skip_taskbar;
    }
    settings
}

fn main() -> iced::Result {
    iced::daemon(App::boot, App::update, App::view)
        .title(App::title)
        .theme(|app: &App, _id| app.theme.clone())
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
    PanelTab(String, String),
    PanelTabClose(String, String),
    PanelTabLock(String, String),
    PanelSplit(String, String),
    PanelClose(String),
    PanelTearOff(String, String),
    /// 撕出边界已按主窗口位置解析（主窗口位置 + 级联偏移）。
    TearOffAt(String, String, Option<Point>),
    DetachedTab(String, String),
    DetachedTabClose(String, String),
    DetachedTabLock(String, String),
    DetachedDockBack(String),
    DetachedClose(String),
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
    LayoutShift(String, bool),
    SceneActivate(String),
    SceneNew,
    SceneRenameStart(String),
    SceneRenameInput(String),
    SceneDelete(String),
    PersistTick,
    PersistDone(Result<(), String>),
    WindowResized(window::Id, Size),
    DividerPressed(window::Id),
    DividerMove(window::Id, Point),
    DividerEnd,
    ToggleTheme,
}

pub struct App {
    /// 布局状态真源（core 模型；与老壳共用磁盘格式）。
    pub ui: AppUiState,
    vault: Arc<vault::VaultState>,
    pub theme: Theme,
    pub status: String,
    /// 当前仓库根（打开仓库后与 vault.root() 一致；仅用于展示与 files 视图判空）。
    pub vault_root: Option<PathBuf>,
    /// 主窗口（渲染激活布局树）；None = 首帧在途。
    main_id: Option<window::Id>,
    /// 撕裂窗口 OS 注册：iced 窗口 id → 模型窗口 id（建窗前预注册，见 reconcile）。
    detached_os: HashMap<window::Id, String>,
    /// OS 窗口当前可见态（调和比对基准；建窗即视为可见）。
    os_shown: HashMap<window::Id, bool>,
    /// files 视图懒加载子项缓存（key = 目录相对路径，根为 ""；运行态不落盘）。
    pub children: HashMap<String, Vec<(String, bool)>>,
    /// note 视图编辑器内容与脏标记（视图实例全局唯一）。
    pub editor: text_editor::Content,
    pub dirty: bool,
    /// 防抖落盘：有未写盘的模型变更。
    persist_dirty: bool,
    /// 主窗口内容区尺寸（logical；Resized 驱动，把手命中几何用）。
    main_size: Size,
    /// 主窗口最近光标位置（按下事件不带坐标，命中判定用最近位置）。
    cursor: Point,
    divider: Option<DividerDrag>,
    pub rename: Option<RenameState>,
}

impl App {
    fn boot() -> (Self, Task<Message>) {
        let (_, open) = window::open(Settings {
            size: WINDOW_SIZE,
            exit_on_close_request: false,
            ..Settings::default()
        });
        (
            Self {
                ui: load_ui_state(),
                vault: Arc::new(vault::VaultState::default()),
                theme: Theme::Dark,
                status: "打开一个仓库开始".into(),
                vault_root: None,
                main_id: None,
                detached_os: HashMap::new(),
                os_shown: HashMap::new(),
                children: HashMap::new(),
                editor: text_editor::Content::new(),
                dirty: false,
                persist_dirty: false,
                main_size: WINDOW_SIZE,
                cursor: Point::ORIGIN,
                divider: None,
                rename: None,
            },
            open.map(Message::WindowOpened),
        )
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
                        self.status = "有未保存改动，先 Ctrl+S 保存".into();
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
                    Task::none()
                }
            }
            Message::PickVault => {
                if self.dirty {
                    self.status = "有未保存改动，先 Ctrl+S 保存".into();
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
                if let Err(e) = self.vault.set(path.clone(), Vec::new()) {
                    self.status = e;
                    return Task::none();
                }
                self.vault_root = self.vault.root().ok();
                self.status = "仓库已打开".into();
                self.children.clear();
                self.editor = text_editor::Content::new();
                self.dirty = false;
                // last_note_file 跨仓库沿用（相对路径语义）；仓库就绪即恢复其内容
                let mut task = self.load_children("");
                if let Some(rel) = self.ui.last_note_file.clone() {
                    task = task.chain(self.load_note(&rel));
                }
                task
            }
            Message::VaultPicked(Err(e)) => {
                self.status = e;
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
                self.status = format!("读取目录失败：{e}");
                Task::none()
            }
            Message::OpenFile(rel) => {
                if self.dirty {
                    self.status = "有未保存改动，先 Ctrl+S 保存".into();
                    return Task::none();
                }
                self.ui.last_note_file = Some(rel.clone());
                self.schedule_persist();
                self.load_note(&rel)
            }
            Message::FileLoaded(gen, Ok((rel, text))) => {
                if gen != self.epoch() || self.ui.last_note_file.as_deref() != Some(rel.as_str()) {
                    return Task::none();
                }
                self.editor = text_editor::Content::with_text(&text);
                self.dirty = false;
                self.status = "已打开".into();
                Task::none()
            }
            Message::FileLoaded(_, Err(e)) => {
                // 载入失败必须清空编辑器：last_note_file 已指向新文件，残留旧内容
                // 会让 Ctrl+S 把旧内容写进新路径
                self.editor = text_editor::Content::new();
                self.dirty = false;
                self.status = e;
                Task::none()
            }
            Message::Edited(action) => {
                // 动作须全部执行（widget 只发布不应用）；置脏只看内容是否实际变化
                let may_edit = action.is_edit();
                let before = may_edit.then(|| self.editor.text());
                self.editor.perform(action);
                if let Some(before) = before {
                    if self.editor.text() != before {
                        self.dirty = true;
                    }
                }
                Task::none()
            }
            Message::Save => {
                let Some(rel) = self.ui.last_note_file.clone() else {
                    self.status = "没有打开的文件".into();
                    return Task::none();
                };
                // 仓库根在发起时冻结：写盘目标固定为打开该文件时的仓库。
                let Ok(root) = self.vault.root() else {
                    self.status = "仓库未打开".into();
                    return Task::none();
                };
                let gen = self.epoch();
                let content = self.editor.text();
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
                // 保存发起时冻结的内容与当前一致才算保存完成；在途期间的新编辑保持脏标记
                if self.editor.text() == saved {
                    self.dirty = false;
                    self.status = "已保存".into();
                } else {
                    self.status = "保存完成，期间有新修改".into();
                }
                Task::none()
            }
            Message::Saved(_, _, Err(e)) => {
                self.status = format!("保存失败：{e}");
                Task::none()
            }
            Message::PanelTab(panel_id, tab_id) => {
                self.apply(LayoutOp::SetActive { panel_id, tab_id })
            }
            Message::PanelTabClose(panel_id, tab_id) => {
                self.apply(LayoutOp::CloseTab { panel_id, tab_id })
            }
            Message::PanelTabLock(panel_id, tab_id) => {
                let locked = self
                    .active_tree()
                    .and_then(|tree| layout::find_tab_in_tree(&tree, &tab_id))
                    .is_some_and(|(_, t)| t.locked);
                self.apply(LayoutOp::SetLocked { panel_id, tab_id, locked: !locked })
            }
            Message::PanelSplit(panel_id, direction) => self.apply(LayoutOp::SplitPanel {
                panel_id,
                direction,
                position: Some("after".into()),
            }),
            Message::PanelClose(panel_id) => self.apply(LayoutOp::ClosePanel { panel_id }),
            Message::PanelTearOff(panel_id, tab_id) => {
                // 新窗边界以主窗口位置级联（老壳按主显示器推导，此处取主窗位置 + 偏移）
                let Some(main) = self.main_id else { return Task::none() };
                window::position(main)
                    .map(move |pos| Message::TearOffAt(panel_id.clone(), tab_id.clone(), pos))
            }
            Message::TearOffAt(panel_id, tab_id, pos) => {
                let bounds = WindowBounds {
                    x: pos.map_or(60.0, |p| (p.x as f64) + 40.0),
                    y: pos.map_or(60.0, |p| (p.y as f64) + 40.0),
                    width: 420.0,
                    height: 560.0,
                    scale: 0.0,
                };
                self.apply(LayoutOp::TearOff { panel_id, tab_id, bounds })
            }
            Message::DetachedTab(window_id, tab_id) => {
                self.apply(LayoutOp::DetachedSetActive { window_id, tab_id })
            }
            Message::DetachedTabClose(window_id, tab_id) => {
                self.apply(LayoutOp::DetachedCloseTab { window_id, tab_id })
            }
            Message::DetachedTabLock(window_id, tab_id) => {
                let locked = self
                    .ui
                    .detached_windows
                    .iter()
                    .find(|w| w.id == window_id)
                    .and_then(|w| w.tabs.iter().find(|t| t.id == tab_id))
                    .is_some_and(|t| t.locked);
                self.apply(LayoutOp::DetachedSetLocked { window_id, tab_id, locked: !locked })
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
                    self.status = "当前布局无面板，先分割出面板".into();
                    return Task::none();
                };
                self.apply(LayoutOp::DockIntoPanel { panel_id, tab_id, index: None })
            }
            Message::DetachedClose(window_id) => {
                self.apply(LayoutOp::RemoveDetachedWindow { window_id })
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
            Message::WindowMoved(id, pos) => {
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
            Message::LayoutActivate(id) => self.apply(LayoutOp::ActivateLayout { id }),
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
                }
            }
            Message::LayoutDelete(id) => self.apply(LayoutOp::DeleteLayout { id }),
            Message::LayoutShift(id, left) => {
                let Some(scene) = self.active_scene() else { return Task::none() };
                let Some(i) = scene.layouts.iter().position(|l| l.id == id) else {
                    return Task::none();
                };
                let (from, to) = if left { (i + 1, i) } else { (i + 1, i + 2) };
                self.apply(LayoutOp::MoveLayout { from_index: from, to_index: to })
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
            Message::SceneDelete(id) => self.apply(LayoutOp::DeleteScene { id }),
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
                        self.status = format!("布局序列化失败：{e}");
                        Task::none()
                    }
                }
            }
            Message::PersistDone(Ok(())) => Task::none(),
            Message::PersistDone(Err(e)) => {
                // 写盘失败回到脏态，下个周期重试
                self.persist_dirty = true;
                self.status = format!("布局落盘失败：{e}");
                Task::none()
            }
            Message::WindowResized(id, size) => {
                if self.main_id == Some(id) {
                    self.main_size = size;
                    return Task::none();
                }
                // 撕裂窗口尺寸回写模型 bounds（OS 窗口事件 = bounds 唯一写者）
                let Some(model_id) = self.detached_os.get(&id) else { return Task::none() };
                if let Some(w) = self.ui.detached_windows.iter_mut().find(|w| w.id == *model_id) {
                    w.bounds.width = size.width as f64;
                    w.bounds.height = size.height as f64;
                    self.schedule_persist();
                }
                Task::none()
            }
            Message::DividerPressed(id) => {
                if self.main_id != Some(id) || self.divider.is_some() {
                    return Task::none();
                }
                let tree = layout::active_layout(&self.ui).tree;
                for d in enumerate_dividers(&tree, self.content_area()) {
                    if d.rect.contains(self.cursor) {
                        self.divider = Some(DividerDrag {
                            split_id: d.split_id,
                            gap: d.gap,
                            horizontal: d.horizontal,
                            drag_start: if d.horizontal { self.cursor.x } else { self.cursor.y },
                            split_len: d.split_len,
                            sizes: d.sizes,
                        });
                        break;
                    }
                }
                Task::none()
            }
            Message::DividerMove(id, pos) => {
                let (Some(id_main), Some(d)) = (self.main_id, &mut self.divider) else {
                    if self.main_id == Some(id) {
                        self.cursor = pos;
                    }
                    return Task::none();
                };
                if id != id_main {
                    return Task::none();
                }
                self.cursor = pos;
                let axis = if d.horizontal { pos.x } else { pos.y };
                let sum: f64 = d.sizes.iter().sum();
                let dfrac = ((axis - d.drag_start) as f64 / d.split_len as f64) * sum;
                let s0 = d.sizes[d.gap] + dfrac;
                let s1 = d.sizes[d.gap + 1] - dfrac;
                if s0 < 0.02 || s1 < 0.02 {
                    return Task::none();
                }
                let split_id = d.split_id.clone();
                let mut sizes = d.sizes.clone();
                sizes[d.gap] = s0;
                sizes[d.gap + 1] = s1;
                self.apply(LayoutOp::SetLayoutSizes { split_id, sizes })
            }
            Message::DividerEnd => {
                self.divider = None;
                Task::none()
            }
            Message::ToggleTheme => {
                self.theme = match self.theme {
                    Theme::Dark => Theme::Light,
                    _ => Theme::Dark,
                };
                Task::none()
            }
        }
    }

    /// 应用一个布局操作：形状校验 + 变更 + 调度落盘 + 撕裂窗口调和
    /// （iced 单状态树天然全窗口可见，无广播层）。
    fn apply(&mut self, op: LayoutOp) -> Task<Message> {
        if !op_passes_shape_check(&self.ui, &op) {
            return Task::none();
        }
        let _result = apply_layout_op(&mut self.ui, &op);
        self.schedule_persist();
        self.reconcile()
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

    /// 主窗口分割树几何区域（头部三行以下的内容区，logical px）。
    fn content_area(&self) -> iced::Rectangle {
        iced::Rectangle {
            x: 0.0,
            y: UiMetrics::HEADER_H,
            width: self.main_size.width,
            height: (self.main_size.height - UiMetrics::HEADER_H).max(0.0),
        }
    }

    /// 读目录子项（core 的统一过滤语义：隐藏项 / 排除夹 / .tmp 副产物）。
    fn load_children(&mut self, rel: &str) -> Task<Message> {
        let Ok(root) = self.vault.root() else {
            return Task::none();
        };
        let Ok(exclude) = self.vault.exclude_folders() else {
            self.status = "仓库配置读取失败".into();
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
            layout_view::main_view(self)
        } else if let Some(model_id) = self.detached_os.get(&id) {
            layout_view::detached_view(self, model_id)
        } else {
            space().into()
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            iced::time::every(PERSIST_TICK).map(|_| Message::PersistTick),
            iced::event::listen_with(|event, _status, window| match event {
                Event::Keyboard(keyboard::Event::KeyPressed {
                    key: keyboard::Key::Character(c),
                    modifiers,
                    ..
                }) if c == "s" && modifiers.control() => Some(Message::Save),
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
                    Some(Message::DividerPressed(window))
                }
                Event::Mouse(mouse::Event::CursorMoved { position }) => {
                    Some(Message::DividerMove(window, position))
                }
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                    Some(Message::DividerEnd)
                }
                _ => None,
            }),
        ])
    }
}
