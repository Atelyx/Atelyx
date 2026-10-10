//! 布局迷你窗口管理器的命令面 + 状态 + 操作应用中枢：布局模型（场景 + 布局列表 + 撕裂窗口）的唯一
//! 真相在本模块族，任何窗口的前端只发命令与接收广播渲染自身切片（webview 间无法共享内存，一致性
//! 只能靠「一个权威 + 广播」）。纯模型见 `layout_model`，持久化/广播见 `layout_persist`，窗口事件见 `layout_window`。

use std::sync::Mutex;

use tauri::{AppHandle, Manager, State};

use crate::layout_drag::{DragHit, DragSession, DragStartPayload};
use crate::layout_model::{
    active_layout, active_scene, active_scene_mut, apply_tab_group_detached,
    apply_tab_group_panel, close_panel_op, collect_tabs, create_blank_tree, create_tab,
    find_panel, find_tab_in_detached, find_tab_in_tree, group_activate_tab, group_add_tab,
    group_move_tab, group_of, group_of_detached, group_remove_tab, group_set_tab_locked,
    group_set_tab_view, map_detached, map_panel, next_layout_name, next_scene_name,
    prune_empty_windows, regenerate_ids, set_active_tree, set_layout_sizes_op, sizes_valid_for,
    split_children_count, split_panel_op, tear_off_from_detached, tear_off_from_panel,
    AppUiState, DetachedWindow, LayoutOp, LayoutOpResult, Scene, UiStatePatch, MAX_RECENT_FILES,
    DEFAULT_SCENE_ID, HOME_LAYOUT_ID, WorkspaceLayout,
};
use crate::layout_persist::{broadcast_layout, persist_now, schedule_persist};
use crate::layout_window::{reconcile_panel_windows, title_of_tabs};

// 命令面与窗口生命周期对外再导出：lib.rs 注册命令、windows.rs 复用窗口钩子/常量，
// 前端契约类型亦经 `layout::` 路径引用（保持既有路径不变）。
// tauri 命令宏会生成同名的 `__cmd__X`/`__tauri_command_name_X` 宏并随函数再导出，
// generate_handler 按函数路径解析它们——故这里必须一并再导出，否则 lib.rs 注册会断。
pub use crate::layout_drag::{
    __cmd__drag_end, __cmd__drag_hit, __cmd__drag_update, __tauri_command_name_drag_end,
    __tauri_command_name_drag_hit, __tauri_command_name_drag_update, drag_end, drag_hit,
    drag_update,
};
pub use crate::layout_model::{WindowBounds, WindowOptions};
pub use crate::layout_persist::load_from_disk;
// 窗口事件钩子是桌面语义（移动端单窗口无 Moved/Resized 事件源），随实现一同分档
#[cfg(desktop)]
pub use crate::layout_window::{seed_window_bounds, window_event_handler, PANEL_LABEL_PREFIX};
#[cfg(not(desktop))]
pub use crate::layout_window::{seed_window_bounds, PANEL_LABEL_PREFIX};

// ===== 托管状态 =====

/// 布局迷你窗口管理器的运行时状态（进程内唯一权威）。
pub struct LayoutState {
    pub(crate) inner: Mutex<LayoutInner>,
}

pub(crate) struct LayoutInner {
    pub ui: AppUiState,
    /// 持久化世代号（防抖合并：仅最新一代真正落盘）。
    pub persist_gen: u64,
    /// 是否有待落盘的改动。
    pub dirty: bool,
    /// 本模块是否已从磁盘加载（bootstrap 前不落盘，防默认态覆盖真实磁盘状态）。
    pub loaded: bool,
    /// 权威窗口 bounds 注册表（窗口事件驱动 + 启动/建窗/拖拽种子化，logical px；
    /// label → bounds；scale 内嵌于 WindowBounds）。
    pub window_bounds: std::collections::HashMap<String, WindowBounds>,
    /// 活跃跨窗口拖拽会话（None = 无拖拽）。
    pub drag: Option<DragSession>,
    /// 各窗口最近上报的 DOM 命中（drag-end 落点解析用）。
    pub drag_hits: std::collections::HashMap<String, DragHit>,
    /// 拖拽移动世代号（Rust 看门狗合并：仅最新一代到期真正结束）。
    pub drag_move_gen: u64,
    /// 拖拽结束解析中（防 pointerup/轮询/看门狗并发重复解析）。
    pub drag_resolving: bool,
    /// 解析窗口内到达的新 begin（拖拽收尾完成后接续为新会话；载荷 + 首帧坐标）。
    /// 多次 begin 后到覆盖前到——收尾只接续最新的那次手势。
    pub pending_start: Option<(DragStartPayload, f64, f64)>,
    /// 驻留托盘前的各撕裂窗口 hidden 备份（Some = 正处驻留；纯内存态，不落盘）：
    /// hide_to_tray 整体置 hidden=true（镜像与 OS 实况一致，插件的显隐判定不失真），
    /// 恢复时按备份还原——唤起类隐藏窗口不随驻留补显。
    pub residence_backup: Option<std::collections::HashMap<String, bool>>,
    /// 预热备用撕裂窗口（模型外；见 `PrewarmWindow`）。
    pub prewarm: Option<PrewarmWindow>,
}

/// 拖拽期间预建的备用撕裂窗口（隐藏、且不在布局模型里）：撕裂落点直接复用它的 id 作为条目 id，
/// 窗口随即被认领进模型；未用上就留作下次（再次拖拽起手不再付一次建窗停顿）。
#[derive(Clone, Debug)]
pub(crate) struct PrewarmWindow {
    /// 预分配的窗口 id（label = `panel-<id>`）。
    pub id: String,
    /// 建窗是否已落地。建窗在途（false）时落点不认领，回退现场建窗——不赌一个可能还没建出来的窗口。
    pub ready: bool,
}

impl LayoutState {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(LayoutInner {
                ui: AppUiState::default(),
                persist_gen: 0,
                dirty: false,
                loaded: false,
                window_bounds: std::collections::HashMap::new(),
                drag: None,
                drag_hits: std::collections::HashMap::new(),
                drag_move_gen: 0,
                drag_resolving: false,
                pending_start: None,
                residence_backup: None,
                prewarm: None,
            }),
        }
    }
}

/// 预热备用窗口的 label（模型外的隐藏窗口）；无 = None。
/// 托盘显隐与退出收尾按窗口枚举时必须放过它：它没有布局条目、也没有内容可落盘。
/// 桌面专属：预热机制与托盘均只在桌面存在。
#[cfg(desktop)]
pub(crate) fn prewarm_window_label(app: &AppHandle) -> Option<String> {
    let state = app.try_state::<LayoutState>()?;
    let inner = state.inner.lock().ok()?;
    inner
        .prewarm
        .as_ref()
        .map(|p| format!("{}{}", crate::layout_window::PANEL_LABEL_PREFIX, p.id))
}

// ===== 布局操作应用（唯一变更入口）=====

/// 视图是否已被占用（树 + 撕裂窗口合计；自身当前视图除外）。
fn view_occupied(ui: &AppUiState, view: &str, except_tab_id: Option<&str>) -> bool {
    let tree = active_layout(ui).tree;
    let mut tabs = Vec::new();
    collect_tabs(&tree, &mut tabs);
    let in_tree = tabs.iter().any(|t| t.view == view && except_tab_id != Some(t.id.as_str()));
    if in_tree {
        return true;
    }
    ui.detached_windows.iter().flat_map(|w| &w.tabs).any(|t| t.view == view && except_tab_id != Some(t.id.as_str()))
}

/// 应用一个布局操作并返回结果。模型变更后由调用方负责广播 + 调度落盘。
pub(crate) fn apply_layout_op(ui: &mut AppUiState, op: &LayoutOp) -> LayoutOpResult {
    let mut result = LayoutOpResult::default();
    match op {
        LayoutOp::AddView { panel_id, view } => {
            let tree = active_layout(ui).tree;
            if let Some(p) = find_panel(&tree, panel_id) {
                if let Some(existing) = p.iter().find(|t| t.view == *view) {
                    // 组内已有 = 激活
                    let tid = existing.id.clone();
                    let tree = map_panel(&tree, panel_id, &|p| apply_tab_group_panel(p, group_activate_tab(&group_of(p), &tid)));
                    set_active_tree(ui, tree);
                } else if !view_occupied(ui, view, None) {
                    let tab = create_tab(view);
                    let tree = map_panel(&tree, panel_id, &|p| apply_tab_group_panel(p, Some(group_add_tab(&group_of(p), tab.clone(), None))));
                    set_active_tree(ui, tree);
                }
            }
        }
        LayoutOp::SetActive { panel_id, tab_id } => {
            let tree = active_layout(ui).tree;
            let tree = map_panel(&tree, panel_id, &|p| apply_tab_group_panel(p, group_activate_tab(&group_of(p), tab_id)));
            set_active_tree(ui, tree);
        }
        LayoutOp::CloseTab { panel_id, tab_id } => {
            let tree = active_layout(ui).tree;
            if let Some(p) = find_panel(&tree, panel_id) {
                if p.iter().any(|t| t.id == *tab_id && !t.locked) {
                    let tree = map_panel(&tree, panel_id, &|p| apply_tab_group_panel(p, group_remove_tab(&group_of(p), tab_id)));
                    set_active_tree(ui, tree);
                }
            }
        }
        LayoutOp::SetLocked { panel_id, tab_id, locked } => {
            let tree = active_layout(ui).tree;
            let tree = map_panel(&tree, panel_id, &|p| apply_tab_group_panel(p, Some(group_set_tab_locked(&group_of(p), tab_id, *locked))));
            set_active_tree(ui, tree);
        }
        LayoutOp::SetTabView { panel_id, tab_id, view } => {
            let tree = active_layout(ui).tree;
            if let Some(p) = find_panel(&tree, panel_id) {
                if let Some(tab) = p.iter().find(|t| t.id == *tab_id) {
                    let currently = tab.view.clone();
                    let locked = tab.locked;
                    if !locked && (currently == *view || !view_occupied(ui, view, Some(tab_id))) {
                        let tree = map_panel(&tree, panel_id, &|p| apply_tab_group_panel(p, Some(group_set_tab_view(&group_of(p), tab_id, view))));
                        set_active_tree(ui, tree);
                    }
                }
            }
        }
        LayoutOp::MoveTabWithin { panel_id, tab_id, to_index } => {
            let tree = active_layout(ui).tree;
            let tree = map_panel(&tree, panel_id, &|p| apply_tab_group_panel(p, group_move_tab(&group_of(p), tab_id, *to_index)));
            set_active_tree(ui, tree);
        }
        LayoutOp::MoveTabBetween { from_panel_id, to_panel_id, tab_id, index } => {
            let tree = active_layout(ui).tree;
            if let Some((src, tab)) = find_tab_in_tree(&tree, tab_id) {
                if src == *from_panel_id {
                    let tree = map_panel(&tree, from_panel_id, &|p| apply_tab_group_panel(p, group_remove_tab(&group_of(p), tab_id)));
                    let tree = map_panel(&tree, to_panel_id, &|p| apply_tab_group_panel(p, Some(group_add_tab(&group_of(p), tab.clone(), *index))));
                    set_active_tree(ui, tree);
                }
            }
        }
        LayoutOp::SplitPanel { panel_id, direction, position } => {
            let pos = position.as_deref().unwrap_or("after");
            let tree = active_layout(ui).tree;
            let (new_tree, new_id) = split_panel_op(&tree, panel_id, direction, pos);
            set_active_tree(ui, new_tree);
            result.split_panel_id = Some(new_id);
        }
        LayoutOp::ClosePanel { panel_id } => {
            let tree = active_layout(ui).tree;
            if let Some(new_tree) = close_panel_op(&tree, panel_id) {
                set_active_tree(ui, new_tree);
            }
        }
        LayoutOp::TearOff { panel_id, tab_id, bounds } => {
            result.detached_window = tear_off_from_panel(ui, panel_id, tab_id, bounds, None);
        }
        LayoutOp::TearOffFromDetached { window_id, tab_id, bounds } => {
            result.detached_window = tear_off_from_detached(ui, window_id, tab_id, bounds, None);
        }
        LayoutOp::DockIntoPanel { panel_id, tab_id, index } => {
            if let Some((src, tab)) = find_tab_in_detached(&ui.detached_windows, tab_id) {
                let tree = active_layout(ui).tree;
                let tree = map_panel(&tree, panel_id, &|p| apply_tab_group_panel(p, Some(group_add_tab(&group_of(p), tab.clone(), *index))));
                set_active_tree(ui, tree);
                ui.detached_windows = prune_empty_windows(map_detached(&ui.detached_windows, &src, &|w| apply_tab_group_detached(w, group_remove_tab(&group_of_detached(w), tab_id))));
            }
        }
        LayoutOp::DockIntoDetached { window_id, tab_id, index } => {
            // 目标撕裂窗口必须存在（防停靠进幽灵窗口：OS 窗口尚在但条目已移除——否则标签
            // 从源移除后无处可去，直接丢失）。解析层已先按窗外处理，此处再兜底一次。
            if !ui.detached_windows.iter().any(|w| w.id == *window_id) {
                return result;
            }
            // 同窗口 = 组内排序
            let same = ui.detached_windows.iter().find(|w| w.id == *window_id);
            if let Some(w) = same {
                if w.tabs.iter().any(|t| t.id == *tab_id) {
                    let len = w.tabs.len();
                    ui.detached_windows = map_detached(&ui.detached_windows, window_id, &|w| apply_tab_group_detached(w, group_move_tab(&group_of_detached(w), tab_id, index.unwrap_or(len))));
                    return result;
                }
            }
            // 来源 = 树面板
            if let Some((src, tab)) = find_tab_in_tree(&active_layout(ui).tree, tab_id) {
                let tree = active_layout(ui).tree;
                let tree = map_panel(&tree, &src, &|p| apply_tab_group_panel(p, group_remove_tab(&group_of(p), tab_id)));
                set_active_tree(ui, tree);
                ui.detached_windows = map_detached(&ui.detached_windows, window_id, &|w| apply_tab_group_detached(w, Some(group_add_tab(&group_of_detached(w), tab.clone(), *index))));
                return result;
            }
            // 来源 = 另一撕裂窗口
            if let Some((src, tab)) = find_tab_in_detached(&ui.detached_windows, tab_id) {
                let next = map_detached(&ui.detached_windows, &src, &|w| apply_tab_group_detached(w, group_remove_tab(&group_of_detached(w), tab_id)));
                let next = prune_empty_windows(next);
                ui.detached_windows = map_detached(&next, window_id, &|w| apply_tab_group_detached(w, Some(group_add_tab(&group_of_detached(w), tab.clone(), *index))));
            }
        }
        LayoutOp::DetachedAddView { window_id, view } => {
            if !view_occupied(ui, view, None) {
                let tab = create_tab(view);
                ui.detached_windows = map_detached(&ui.detached_windows, window_id, &|w| apply_tab_group_detached(w, Some(group_add_tab(&group_of_detached(w), tab.clone(), None))));
            }
        }
        LayoutOp::DetachedSetActive { window_id, tab_id } => {
            ui.detached_windows = map_detached(&ui.detached_windows, window_id, &|w| apply_tab_group_detached(w, group_activate_tab(&group_of_detached(w), tab_id)));
        }
        LayoutOp::DetachedCloseTab { window_id, tab_id } => {
            let win = ui.detached_windows.iter().find(|w| w.id == *window_id);
            if let Some(w) = win {
                if w.tabs.iter().any(|t| t.id == *tab_id && !t.locked) {
                    ui.detached_windows = prune_empty_windows(map_detached(&ui.detached_windows, window_id, &|w| apply_tab_group_detached(w, group_remove_tab(&group_of_detached(w), tab_id))));
                }
            }
        }
        LayoutOp::DetachedSetLocked { window_id, tab_id, locked } => {
            ui.detached_windows = map_detached(&ui.detached_windows, window_id, &|w| apply_tab_group_detached(w, Some(group_set_tab_locked(&group_of_detached(w), tab_id, *locked))));
        }
        LayoutOp::DetachedSetTabView { window_id, tab_id, view } => {
            let win = ui.detached_windows.iter().find(|w| w.id == *window_id);
            if let Some(w) = win {
                if let Some(tab) = w.tabs.iter().find(|t| t.id == *tab_id) {
                    if !tab.locked && (tab.view == *view || !view_occupied(ui, view, Some(tab_id))) {
                        ui.detached_windows = map_detached(&ui.detached_windows, window_id, &|w| apply_tab_group_detached(w, Some(group_set_tab_view(&group_of_detached(w), tab_id, view))));
                    }
                }
            }
        }
        LayoutOp::DetachedMoveTab { window_id, tab_id, to_index } => {
            ui.detached_windows = map_detached(&ui.detached_windows, window_id, &|w| apply_tab_group_detached(w, group_move_tab(&group_of_detached(w), tab_id, *to_index)));
        }
        LayoutOp::RemoveDetachedWindow { window_id } => {
            ui.detached_windows.retain(|w| w.id != *window_id);
        }
        LayoutOp::CreateDetachedWindow { view, bounds, restore_on_launch } => {
            // 视图全局唯一（与 AddView 同口径）：被占用 = 忽略（不建窗不建条目）
            if !view_occupied(ui, view, None) {
                let tab = create_tab(view);
                let win = DetachedWindow {
                    id: nanoid::nanoid!(),
                    tabs: vec![tab.clone()],
                    active_tab_id: Some(tab.id.clone()),
                    bounds: bounds.clone(),
                    hidden: false,
                    restore_on_launch: restore_on_launch.unwrap_or(true),
                    options: WindowOptions::default(),
                    pinned: false,
                };
                ui.detached_windows.push(win.clone());
                result.detached_window = Some(win);
            }
        }
        // 聚焦 = 纯 OS 动作（命令层在调和后执行），模型不动
        LayoutOp::FocusDetachedWindow { .. } => {}
        LayoutOp::HideDetachedWindow { window_id } => {
            if let Some(w) = ui.detached_windows.iter_mut().find(|w| w.id == *window_id) {
                w.hidden = true;
            }
        }
        LayoutOp::ShowDetachedWindow { window_id } => {
            if let Some(w) = ui.detached_windows.iter_mut().find(|w| w.id == *window_id) {
                w.hidden = false;
            }
        }
        LayoutOp::ToggleDetachedWindow { view, bounds, options } => {
            // 按视图定位条目（一个视图至多一个 toggle 窗口）：有 = 显隐翻转 + 选项随本次声明收敛；
            // 无 = 以给定边界与选项新建（视图全局唯一，被占用 = 忽略）。bounds 仅新建时使用。
            if let Some(w) = ui
                .detached_windows
                .iter_mut()
                .find(|w| w.tabs.iter().any(|t| t.view == *view))
            {
                w.options = *options;
                w.hidden = !w.hidden;
                result.detached_window = Some(w.clone());
            } else if !view_occupied(ui, view, None) {
                let tab = create_tab(view);
                let win = DetachedWindow {
                    id: nanoid::nanoid!(),
                    tabs: vec![tab.clone()],
                    active_tab_id: Some(tab.id.clone()),
                    bounds: bounds.clone(),
                    hidden: false,
                    restore_on_launch: false,
                    options: *options,
                    pinned: false,
                };
                ui.detached_windows.push(win.clone());
                result.detached_window = Some(win);
            }
        }
        LayoutOp::SetDetachedWindowPinned { window_id, pinned } => {
            if let Some(w) = ui
                .detached_windows
                .iter_mut()
                .find(|w| w.id == *window_id && w.options.hide_on_blur)
            {
                w.pinned = *pinned;
            }
        }
        LayoutOp::SetLayoutSizes { split_id, sizes } => {
            let tree = active_layout(ui).tree;
            // 形状校验（长度 = children 数、有限非负、和 > 0）：坏值一旦落进模型就会被广播并持久化，
            // 前端随后按它布局可能越界/塌陷，且坏状态每次启动都被读回。不合法 = 忽略本次操作。
            let valid = split_children_count(&tree, split_id)
                .is_some_and(|n| sizes_valid_for(sizes, n));
            if valid {
                let tree = set_layout_sizes_op(&tree, split_id, sizes);
                set_active_tree(ui, tree);
            }
        }
        LayoutOp::AddLayout => {
            let new_id = {
                let scene = active_scene_mut(ui);
                let names: Vec<String> = scene.layouts.iter().map(|l| l.name.clone()).collect();
                let blank = WorkspaceLayout { id: nanoid::nanoid!(), name: next_layout_name(&names), tree: create_blank_tree() };
                let new_id = blank.id.clone();
                scene.layouts.push(blank);
                scene.active_layout_id = Some(new_id.clone());
                new_id
            };
            ui.active_layout_id = Some(new_id);
            ui.focused_panel_id = None;
        }
        LayoutOp::RenameLayout { id, name } => {
            if id == HOME_LAYOUT_ID {
                return result;
            }
            let trimmed = name.trim();
            if trimmed.is_empty() {
                return result;
            }
            let scene = active_scene_mut(ui);
            for l in &mut scene.layouts {
                if l.id == *id {
                    l.name = trimmed.to_string();
                    break;
                }
            }
        }
        LayoutOp::DeleteLayout { id } => {
            if id == HOME_LAYOUT_ID {
                return result;
            }
            let fallback: Option<String> = {
                let scene = active_scene_mut(ui);
                if scene.layouts.len() <= 1 {
                    return result;
                }
                scene.layouts.retain(|l| l.id != *id);
                // 场景记忆指向被删布局 → 回落场景内第一个（记忆与顶层激活是两个独立态：
                // normalize 允许记忆 ≠ 顶层激活，二者须各自修复，不能只看顶层）
                if scene.active_layout_id.as_deref() == Some(id) {
                    let first = scene.layouts.first().map(|l| l.id.clone());
                    scene.active_layout_id = first.clone();
                    first
                } else {
                    scene.active_layout_id.clone()
                }
            };
            // 顶层激活指向被删布局 → 跟随场景记忆（上一步已保证记忆指向存活布局）
            if ui.active_layout_id.as_deref() == Some(id) {
                ui.active_layout_id = fallback;
                ui.focused_panel_id = None;
            }
        }
        LayoutOp::ActivateLayout { id } => {
            if id == HOME_LAYOUT_ID {
                // 主页是激活场景的专属槽位：激活它并记入场景记忆（切回本场景时恢复）
                active_scene_mut(ui).active_layout_id = Some(id.clone());
                ui.active_layout_id = Some(id.clone());
                ui.focused_panel_id = None;
                return result;
            }
            if !active_scene(ui).layouts.iter().any(|l| l.id == *id) {
                return result;
            }
            active_scene_mut(ui).active_layout_id = Some(id.clone());
            ui.active_layout_id = Some(id.clone());
            ui.focused_panel_id = None;
        }
        LayoutOp::MoveLayout { from_index, to_index } => {
            // 合成序 = tab 条可见序：0 = 固定主页，场景内布局从 1 起。
            // 主页不可拖动（from 0），其他布局不可拖到主页之前（to 0）。
            if *from_index == 0 || *to_index == 0 || from_index == to_index {
                return result;
            }
            let scene = active_scene_mut(ui);
            let len = scene.layouts.len();
            let from = *from_index - 1;
            let to = *to_index - 1;
            if from >= len || to >= len {
                return result;
            }
            let moved = scene.layouts.remove(from);
            scene.layouts.insert(to, moved);
        }
        LayoutOp::AddScene => {
            let names: Vec<String> = ui.scenes.iter().map(|s| s.name.clone()).collect();
            let source = active_scene(ui);
            // 复制源场景（布局与专属主页均复制：布局 id 与树内节点/标签 id 重新生成防跨场景串扰，
            // 主页 id 恒为 HOME_LAYOUT_ID），激活记忆指向记忆布局的复制件
            let active_index = source
                .layouts
                .iter()
                .position(|l| Some(&l.id) == source.active_layout_id.as_ref());
            let layouts: Vec<WorkspaceLayout> = source
                .layouts
                .iter()
                .map(|l| WorkspaceLayout { id: nanoid::nanoid!(), name: l.name.clone(), tree: regenerate_ids(&l.tree) })
                .collect();
            let copy = Scene {
                id: nanoid::nanoid!(),
                name: next_scene_name(&names),
                home_layout: WorkspaceLayout {
                    id: HOME_LAYOUT_ID.to_string(),
                    name: source.home_layout.name.clone(),
                    tree: regenerate_ids(&source.home_layout.tree),
                },
                active_layout_id: match source.active_layout_id.as_deref() {
                    Some(HOME_LAYOUT_ID) => Some(HOME_LAYOUT_ID.to_string()),
                    _ => active_index.and_then(|i| layouts.get(i)).map(|l| l.id.clone()),
                },
                layouts,
            };
            ui.active_layout_id = copy.active_layout_id.clone();
            ui.scenes.push(copy);
            ui.active_scene_id = ui.scenes.last().map(|s| s.id.clone());
            ui.focused_panel_id = None;
        }
        LayoutOp::RenameScene { id, name } => {
            if id == DEFAULT_SCENE_ID {
                return result;
            }
            let trimmed = name.trim();
            if trimmed.is_empty() {
                return result;
            }
            for s in &mut ui.scenes {
                if s.id == *id {
                    s.name = trimmed.to_string();
                    break;
                }
            }
        }
        LayoutOp::DeleteScene { id } => {
            if id == DEFAULT_SCENE_ID {
                return result;
            }
            let removing_active = ui.active_scene_id.as_deref() == Some(id);
            ui.scenes.retain(|s| s.id != *id);
            if removing_active {
                // 删除激活场景 → 回到默认场景（默认场景不可删，恒在首位）及其记忆布局
                ui.active_scene_id = Some(ui.scenes[0].id.clone());
                ui.active_layout_id = ui.scenes[0].active_layout_id.clone();
                ui.focused_panel_id = None;
            }
        }
        LayoutOp::ActivateScene { id } => {
            let Some(scene) = ui.scenes.iter_mut().find(|s| s.id == *id) else {
                return result;
            };
            // 恢复目标场景记忆的激活布局（normalize 保证记忆恒有效）
            ui.active_layout_id = scene.active_layout_id.clone();
            ui.active_scene_id = Some(id.clone());
            ui.focused_panel_id = None;
        }
    }
    result
}

// ===== 命令 =====

/// 布局状态全量快照（bootstrap）：主窗口/撕裂窗口初始化渲染用。
/// 返回归一化后的完整 AppUiState（含非布局字段，前端据此初始化自身镜像）；
/// 锁被 poison（曾持锁 panic）时返回 Err，由调用方进入错误态而非继续用可疑模型渲染。
#[tauri::command]
pub fn layout_bootstrap(state: State<'_, LayoutState>) -> Result<AppUiState, String> {
    let inner = state.inner.lock().map_err(|e| e.to_string())?;
    Ok(inner.ui.clone())
}

/// 尺寸补丁是否可应用（用于在变更前挡掉坏值：坏值既不该进模型，也不该触发落盘/广播）。
/// 其余操作恒为 true（各自在校验分支内决定是否真正改动）。
pub(crate) fn op_passes_shape_check(ui: &AppUiState, op: &LayoutOp) -> bool {
    match op {
        LayoutOp::SetLayoutSizes { split_id, sizes } => split_children_count(&active_layout(ui).tree, split_id)
            .is_some_and(|n| sizes_valid_for(sizes, n)),
        _ => true,
    }
}

/// 应用一个布局操作：校验 + 变更 + 广播 + 调度落盘。返回操作结果（splitPanel/tearOff 用）。
/// 布局模型变更后同步窗口生命周期：reconcile 自锁读当前模型补建缺失/回收幽灵。
#[tauri::command]
pub async fn layout_op(app: AppHandle, op: LayoutOp) -> Result<LayoutOpResult, String> {
    let state = app.state::<LayoutState>();
    let mut inner = state.inner.lock().map_err(|e| e.to_string())?;
    if !inner.loaded {
        return Ok(LayoutOpResult::default());
    }
    // 形状不合法 = 什么也没发生：不置 dirty、不落盘、不广播（否则坏值会被持久化并被各窗口读回）
    if !op_passes_shape_check(&inner.ui, &op) {
        return Ok(LayoutOpResult::default());
    }
    let result = apply_layout_op(&mut inner.ui, &op);
    inner.dirty = true;
    let ui = inner.ui.clone();
    drop(inner);
    reconcile_panel_windows(&app);
    apply_window_action(&app, &op, &ui, &result);
    schedule_persist(&app, &state);
    broadcast_layout(&app, &ui);
    Ok(result)
}

/// 按视图 toggle 的默认边界（首次热键唤起建窗用；此后以窗口事件回写的模型 bounds 为准）：
/// 主显示器右侧贴边、垂直偏上，逻辑 px。
#[cfg(desktop)]
fn default_summon_bounds(app: &AppHandle) -> WindowBounds {
    const W: f64 = 440.0;
    const H: f64 = 680.0;
    if let Ok(Some(monitor)) = app.primary_monitor() {
        let sf = monitor.scale_factor() as f64;
        let lw = monitor.size().width as f64 / sf;
        let lh = monitor.size().height as f64 / sf;
        return WindowBounds {
            x: (lw - W - 24.0).max(12.0),
            y: ((lh - H) * 0.18).max(12.0),
            width: W,
            height: H,
            scale: sf,
        };
    }
    WindowBounds { x: 60.0, y: 60.0, width: W, height: H, scale: 1.0 }
}

/// 热键直控（Rust 快捷键触发线程投递 async runtime 执行，不经主窗口 JS——主窗口驻留
/// 托盘时照常生效）：与 `layout_op` 命令同一套「模型变更 → 调和 → 窗口动作 → 落盘 → 广播」
/// 序列。选项随 toggle 收敛到条目（创建方的配置变更由此生效）；新建时的边界由宿主按
/// 主显示器推导（创建后以窗口事件回写为准）。
#[cfg(desktop)]
pub fn toggle_window_by_view(
    app: &AppHandle,
    view: &str,
    options: WindowOptions,
) -> Result<(), String> {
    let state = app.state::<LayoutState>();
    // 默认边界在锁外推导：primary_monitor() 阻塞等待主事件循环应答，持锁调用会与主线程
    // 的窗口事件（write_bounds 等需同一把锁）互等死锁——「让主事件循环回头的动作一律
    // 放锁后」纪律的另一种形态。边界仅新建分支使用（apply 内判定条目是否已存在），
    // 既有条目的 toggle 忽略它。
    let bounds = default_summon_bounds(app);
    let op = LayoutOp::ToggleDetachedWindow { view: view.to_string(), bounds, options };
    let mut inner = state.inner.lock().map_err(|e| e.to_string())?;
    if !inner.loaded {
        return Ok(());
    }
    if !op_passes_shape_check(&inner.ui, &op) {
        return Ok(());
    }
    let result = apply_layout_op(&mut inner.ui, &op);
    inner.dirty = true;
    let ui = inner.ui.clone();
    drop(inner);
    reconcile_panel_windows(&app);
    apply_window_action(app, &op, &ui, &result);
    schedule_persist(app, &state);
    broadcast_layout(app, &ui);
    Ok(())
}

/// 将撕裂窗口置为隐藏（模型 hidden 置真 + 广播 + 调度落盘 + OS 隐藏）；
/// 非 `panel-` 窗口或模型已隐藏 = no-op。失焦收起与关闭键隐藏共用。
#[cfg(desktop)]
pub(crate) fn hide_detached_window(app: &AppHandle, label: &str) {
    let Some(id) = label.strip_prefix(crate::layout_window::PANEL_LABEL_PREFIX) else {
        return;
    };
    let state = app.state::<LayoutState>();
    let Ok(mut inner) = state.inner.lock() else {
        return;
    };
    let Some(w) = inner.ui.detached_windows.iter_mut().find(|w| w.id == id) else {
        return;
    };
    if w.hidden {
        return;
    }
    w.hidden = true;
    let ui = inner.ui.clone();
    inner.dirty = true;
    drop(inner);
    if let Some(win) = app.get_webview_window(label) {
        let _ = win.hide();
    }
    broadcast_layout(app, &ui);
    schedule_persist(app, &state);
}

/// 失焦自动收起（`options.hide_on_blur` 声明的窗口；图钉豁免）。
/// 失焦带宽限期复核：头部拖动区启动原生拖拽在 Windows 上会瞬时失焦，立即收会打断拖动——
/// 延迟后复核焦点，已回到本窗口（拖拽结束回焦/用户点回）则不收。
/// 桌面专属（焦点语义仅桌面有）。
#[cfg(desktop)]
pub fn hide_window_on_blur(app: &AppHandle, label: &str) {
    let Some(id) = label.strip_prefix(crate::layout_window::PANEL_LABEL_PREFIX) else {
        return;
    };
    let state = app.state::<LayoutState>();
    let Ok(inner) = state.inner.lock() else {
        return;
    };
    let should_hide = inner
        .ui
        .detached_windows
        .iter()
        .find(|w| w.id == id)
        .is_some_and(|w| w.options.hide_on_blur && !w.pinned && !w.hidden);
    drop(inner);
    if !should_hide {
        return;
    }
    let app = app.clone();
    let label = label.to_string();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(220));
        let Some(win) = app.get_webview_window(&label) else {
            return;
        };
        // 拖动循环期间 is_focused 报告实现不一：以「仍持有焦点」为豁免判据，
        // 焦点确实离开（点了其他应用/窗口）才收。图钉豁免在 set 前按最新模型复算。
        if win.is_focused().unwrap_or(false) {
            return;
        }
        let state = app.state::<LayoutState>();
        let exempt = state
            .inner
            .lock()
            .ok()
            .and_then(|inner| {
                inner
                    .ui
                    .detached_windows
                    .iter()
                    .find(|w| w.id == label.trim_start_matches(crate::layout_window::PANEL_LABEL_PREFIX))
                    .map(|w| w.pinned || !w.options.hide_on_blur)
            })
            .unwrap_or(true);
        if exempt {
            return;
        }
        hide_detached_window(&app, &label);
    });
}

/// 关闭请求是否按「隐藏不销毁」处理（`options.close_hides` 声明；label → 模型条目查表）。
#[cfg(desktop)]
pub fn window_close_hides(app: &AppHandle, label: &str) -> bool {
    let Some(id) = label.strip_prefix(crate::layout_window::PANEL_LABEL_PREFIX) else {
        return false;
    };
    app.state::<LayoutState>()
        .inner
        .lock()
        .ok()
        .and_then(|inner| {
            inner
                .ui
                .detached_windows
                .iter()
                .find(|w| w.id == id)
                .map(|w| w.options.close_hides)
        })
        .unwrap_or(false)
}

/// 布局操作触达的 OS 窗口动作（免标签直撕建窗 / 聚焦 / 显隐）：模型变更与调和后执行。
/// 建窗走 `create_panel_window_internal`（其内部与 UI 驻留标志取交集）。驻留期间的显式
/// 唤起（show/直撕）照常把目标窗口带到眼前——那是用户的直接意图，且该窗口已随
/// `residence_forget` 从驻留备份除名，托盘恢复不会把它翻回隐藏；驻留期间新建的窗口
/// 会因驻留标志建为隐藏，同样补 show + 前置。仅 focus 不做驻留豁免（聚焦一个隐藏
/// 窗口无意义，可见性归 show 管）。
fn apply_window_action(app: &AppHandle, op: &LayoutOp, ui: &AppUiState, result: &LayoutOpResult) {
    let residence = app
        .try_state::<crate::tray::UiHidden>()
        .map(|flag| flag.get())
        .unwrap_or(false);
    match op {
        LayoutOp::CreateDetachedWindow { .. } => {
            if let Some(w) = &result.detached_window {
                let label = format!("{PANEL_LABEL_PREFIX}{}", w.id);
                crate::commands::windows::create_panel_window_internal(
                    app,
                    &label,
                    &title_of_tabs(&w.tabs, &w.active_tab_id),
                    &w.bounds,
                    true,
                );
                if residence {
                    residence_forget(app, &w.id);
                    if let Some(win) = app.get_webview_window(&label) {
                        // unminimize 桌面专属 API（移动端单窗口无最小化语义）
                        #[cfg(desktop)]
                        {
                            let _ = win.unminimize();
                        }
                        let _ = win.show();
                        let _ = win.set_focus();
                    }
                }
            }
        }
        LayoutOp::FocusDetachedWindow { window_id } => {
            if !residence {
                if let Some(win) = app.get_webview_window(&format!("{PANEL_LABEL_PREFIX}{window_id}")) {
                    // 最小化窗口 set_focus 不会还原，先 unminimize（与托盘补显同口径）；
                    // unminimize 桌面专属 API（移动端单窗口无最小化语义）
                    #[cfg(desktop)]
                    {
                        let _ = win.unminimize();
                    }
                    let _ = win.set_focus();
                }
            }
        }
        LayoutOp::HideDetachedWindow { window_id } => {
            if let Some(win) = app.get_webview_window(&format!("{PANEL_LABEL_PREFIX}{window_id}")) {
                let _ = win.hide();
            }
        }
        LayoutOp::ShowDetachedWindow { window_id } => {
            residence_forget(app, window_id);
            let label = format!("{PANEL_LABEL_PREFIX}{window_id}");
            match app.get_webview_window(&label) {
                Some(win) => {
                    // unminimize 桌面专属 API（移动端单窗口无最小化语义）
                    #[cfg(desktop)]
                    {
                        let _ = win.unminimize();
                    }
                    let _ = win.show();
                    let _ = win.set_focus();
                }
                // 不参与启动恢复的窗口条目无 OS 窗口：显窗即补建。驻留期间建窗为隐藏
                // （与驻留标志取交集），显式唤起同样补 show + 前置
                None => {
                    if let Some(w) = ui.detached_windows.iter().find(|w| w.id == *window_id) {
                        crate::commands::windows::create_panel_window_internal(
                            app,
                            &label,
                            &title_of_tabs(&w.tabs, &w.active_tab_id),
                            &w.bounds,
                            true,
                        );
                        if let Some(win) = app.get_webview_window(&label) {
                            // unminimize 桌面专属 API（移动端单窗口无最小化语义）
                            #[cfg(desktop)]
                            {
                                let _ = win.unminimize();
                            }
                            let _ = win.show();
                            let _ = win.set_focus();
                        }
                    }
                }
            }
        }
        LayoutOp::ToggleDetachedWindow { .. } => {
            // 显隐翻转在 apply 阶段完成，此处按应用后的模型条目执行 OS 动作：
            // 未建窗先补建（驻留期间建窗会因驻留标志为隐藏，随后统一 show）；显示 = 前置 + 聚焦，
            // 且 residence_forget 从驻留备份除名（托盘恢复不把它翻回隐藏）；隐藏 = 仅隐藏。
            if let Some(w) = &result.detached_window {
                let label = format!("{PANEL_LABEL_PREFIX}{}", w.id);
                let entry = ui.detached_windows.iter().find(|e| e.id == w.id);
                let show = entry.is_some_and(|e| !e.hidden);
                if app.get_webview_window(&label).is_none() {
                    if let Some(e) = entry {
                        crate::commands::windows::create_panel_window_internal(
                            app,
                            &label,
                            &title_of_tabs(&e.tabs, &e.active_tab_id),
                            &e.bounds,
                            true,
                        );
                    }
                }
                if let Some(win) = app.get_webview_window(&label) {
                    // 既有 OS 窗口的选项收敛：本次声明可能变更了置顶/任务栏属性，
                    // 建窗时应用的旧值在此对齐模型（补建路径由建窗函数自行应用）
                    crate::commands::windows::apply_window_os_options(app, &label);
                    if show {
                        residence_forget(app, &w.id);
                        // unminimize 桌面专属 API（移动端单窗口无最小化语义）
                        #[cfg(desktop)]
                        {
                            let _ = win.unminimize();
                        }
                        let _ = win.show();
                        let _ = win.set_focus();
                    } else {
                        let _ = win.hide();
                    }
                }
            }
        }
        // 图钉只改模型（失焦收起判定读模型），无 OS 动作
        LayoutOp::SetDetachedWindowPinned { .. } => {}
        _ => {}
    }
}

/// 非布局字段补丁（前端 JS 拥有这些字段，合并进模型并调度落盘）。
#[tauri::command]
pub async fn ui_state_patch(app: AppHandle, patch: UiStatePatch) -> Result<(), String> {
    let state = app.state::<LayoutState>();
    let mut inner = state.inner.lock().map_err(|e| e.to_string())?;
    if !inner.loaded {
        return Ok(());
    }
    if let Some(v) = patch.file_explorer_expanded {
        inner.ui.file_explorer_expanded = v;
    }
    if let Some(v) = patch.last_canvas_file {
        inner.ui.last_canvas_file = v;
    }
    if let Some(v) = patch.last_note_file {
        inner.ui.last_note_file = v;
    }
    if let Some(v) = patch.last_table_file {
        inner.ui.last_table_file = v;
    }
    if let Some(v) = patch.focused_panel_id {
        inner.ui.focused_panel_id = v;
    }
    if let Some(v) = patch.recent_files {
        inner.ui.recent_files = v.into_iter().take(MAX_RECENT_FILES).collect();
    }
    if let Some(v) = patch.slot_winner_overrides {
        inner.ui.slot_winner_overrides = v;
    }
    inner.dirty = true;
    drop(inner);
    schedule_persist(&app, &state);
    Ok(())
}

/// 立即落盘（应用退出/切页面前 flush 用，防 debounce 窗口内丢状态）。
#[tauri::command]
pub async fn layout_flush(app: AppHandle) -> Result<(), String> {
    persist_now(&app)
}

/// 驻留托盘的模型翻转（纯函数，`residence_set` 的模型部分）：进入驻留 = 备份各撕裂窗口
/// hidden 后整体置真（镜像与 OS 实况一致，插件的显隐判定在驻留期间不失真）；
/// 退出驻留 = 按备份还原（唤起类隐藏窗口不随驻留补显）。驻留期间新增/移除的条目
/// 不在备份内：新增条目维持现状（显式唤起的保持可见），已移除条目自然跳过。
fn residence_flip(
    ui: &mut AppUiState,
    backup: &mut Option<std::collections::HashMap<String, bool>>,
    to_hidden: bool,
) {
    match to_hidden {
        true => {
            let mut captured = std::collections::HashMap::new();
            for w in ui.detached_windows.iter_mut() {
                captured.insert(w.id.clone(), w.hidden);
                w.hidden = true;
            }
            *backup = Some(captured);
        }
        false => {
            if let Some(captured) = backup.take() {
                for w in ui.detached_windows.iter_mut() {
                    if let Some(hidden) = captured.get(&w.id) {
                        w.hidden = *hidden;
                    }
                }
            }
        }
    }
}

/// 驻留托盘进入/退出（tray 的 hide_all_windows / show_all_windows 调用）：
/// 翻转模型 hidden + 广播 + 调度落盘，让前端镜像在驻留期间与 OS 实况一致。
pub(crate) fn residence_set(app: &AppHandle, to_hidden: bool) {
    let state = app.state::<LayoutState>();
    let Ok(mut inner) = state.inner.lock() else {
        eprintln!("[layout] 布局状态锁已损坏，放弃驻留可见性翻转");
        return;
    };
    if !inner.loaded {
        return;
    }
    // 未驻留时退出翻转 = no-op（托盘点击在无驻留态下不产生无谓落盘与广播）
    if !to_hidden && inner.residence_backup.is_none() {
        return;
    }
    let mut backup = inner.residence_backup.take();
    residence_flip(&mut inner.ui, &mut backup, to_hidden);
    inner.residence_backup = backup;
    inner.dirty = true;
    let ui = inner.ui.clone();
    drop(inner);
    schedule_persist(app, &state);
    broadcast_layout(app, &ui);
}

/// 驻留期间插件显式唤起某窗口：其可见性以唤起为准，从驻留备份中除名
/// （托盘恢复时不再把它翻回隐藏）。
pub(crate) fn residence_forget(app: &AppHandle, window_id: &str) {
    let state = app.state::<LayoutState>();
    let Ok(mut inner) = state.inner.lock() else {
        return;
    };
    if let Some(backup) = inner.residence_backup.as_mut() {
        backup.remove(window_id);
    }
}

/// 布局调和（主窗口启动后调用）：种子化全部窗口 bounds + 补建持久化撕裂窗口的 OS 窗口。
#[tauri::command]
pub async fn layout_reconcile(app: AppHandle) -> Result<(), String> {
    let state = app.state::<LayoutState>();
    let changed = {
        let mut inner = state.inner.lock().map_err(|e| e.to_string())?;
        if !inner.loaded {
            return Ok(());
        }
        // 唤起类窗口（创建方声明不参与启动恢复）：启动时按「不存在」处理——标记隐藏 + 不补建
        // OS 窗口；出现由创建方显式触发（showDetachedWindow 补建并前置）。标记让前端镜像的
        // hidden 与「窗口是否在场」口径一致，插件的显隐判定不会把缺窗条目当成可见窗口。
        // 仅对无 OS 窗口的条目标记：插件可能已在本调和之前（如启动期 selectVault 内激活）
        // 显式 show 出了在场窗口，那类窗口的可见性以在场为准，不得误隐藏。
        let mut changed = false;
        for w in inner.ui.detached_windows.iter_mut() {
            if !w.restore_on_launch && !w.hidden {
                let label = format!("{PANEL_LABEL_PREFIX}{}", w.id);
                if app.get_webview_window(&label).is_none() {
                    w.hidden = true;
                    changed = true;
                }
            }
        }
        if changed {
            inner.dirty = true;
        }
        changed
    };
    // 种子化 bounds：主窗口启动后未移动过时 on_window_event 不触发，拖拽落点解析读不到
    for label in app.webview_windows().keys().cloned().collect::<Vec<_>>() {
        seed_window_bounds(&app, &label);
    }
    reconcile_panel_windows(&app);
    if changed {
        schedule_persist(&app, &state);
        let ui = {
            let inner = state.inner.lock().map_err(|e| e.to_string())?;
            inner.ui.clone()
        };
        broadcast_layout(&app, &ui);
    }
    Ok(())
}

/// 撕裂窗口关闭上报（用户关闭窗口/≡删除面板时调用）：移除模型条目，不关 OS 窗口
/// （窗口正由自身关闭流程销毁；JS onCloseRequested 已 flush 托管视图）。
#[tauri::command]
pub async fn panel_window_closed(app: AppHandle, window_id: String) -> Result<(), String> {
    let state = app.state::<LayoutState>();
    let mut inner = state.inner.lock().map_err(|e| e.to_string())?;
    if !inner.loaded {
        return Ok(());
    }
    let before = inner.ui.detached_windows.len();
    inner.ui.detached_windows.retain(|w| w.id != window_id);
    if inner.ui.detached_windows.len() == before {
        return Ok(());
    }
    inner.dirty = true;
    let ui = inner.ui.clone();
    drop(inner);
    schedule_persist(&app, &state);
    broadcast_layout(&app, &ui);
    Ok(())
}

// ===== 单元测试：布局操作应用 =====

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout_model::{
        create_home_layout, LayoutNode, TabItem, UI_STATE_SCHEMA, WorkspaceLayout,
    };

    /// 驻留翻转：进入 = 备份各窗口 hidden 并整体置真（镜像与 OS 实况一致，插件的显隐
    /// 判定在驻留期间不失真）；退出 = 按备份还原（唤起类隐藏窗口不随驻留补显）。
    /// 驻留期间新增的条目不在备份内（维持现状），已移除条目自然跳过。
    #[test]
    fn residence_flip_backs_up_and_restores() {
        let win = |id: &str, hidden: bool| DetachedWindow {
            id: id.into(),
            tabs: vec![],
            active_tab_id: None,
            bounds: WindowBounds { x: 0.0, y: 0.0, width: 100.0, height: 100.0, scale: 1.0 },
            hidden,
            restore_on_launch: true,
            options: WindowOptions::default(),
            pinned: false,
        };
        let mut ui = AppUiState {
            schema: UI_STATE_SCHEMA.into(),
            detached_windows: vec![win("w1", false), win("w2", true)],
            ..Default::default()
        };
        let mut backup: Option<std::collections::HashMap<String, bool>> = None;

        residence_flip(&mut ui, &mut backup, true);
        assert!(ui.detached_windows.iter().all(|w| w.hidden));
        let captured = backup.as_ref().unwrap();
        assert_eq!(captured.get("w1"), Some(&false));
        assert_eq!(captured.get("w2"), Some(&true));

        // 驻留期间新增（显式唤起的直撕窗口）与移除
        ui.detached_windows.push(win("w3", false));
        ui.detached_windows.retain(|w| w.id != "w2");

        residence_flip(&mut ui, &mut backup, false);
        // 备份还原：w1 回到可见；w2 已移除跳过；w3 不在备份、维持现状
        assert!(!ui.detached_windows.iter().find(|w| w.id == "w1").unwrap().hidden);
        assert!(ui.detached_windows.iter().all(|w| w.id != "w2"));
        assert!(!ui.detached_windows.iter().find(|w| w.id == "w3").unwrap().hidden);
        assert!(backup.is_none());
    }

    /// 布局状态锁的失败语义必须显式化：命令层 `map_err(...)?` 传播 Err，无返回值的内部路径
    /// 用 `let ... else { 记录并 return }`。`unwrap()` 会让一次持锁 panic 变成此后每次调用的 panic
    /// （布局与多窗口持久化在本进程剩余生命周期永久不可用），故此处静态锁死该形态。
    /// 只扫 `#[cfg(test)]` 之前的生产区（测试代码与本断言的字符串本身不受限）。
    #[test]
    fn no_unguarded_layout_lock() {
        fn production_part(src: &str) -> &str {
            src.split("#[cfg(test)]").next().unwrap_or(src)
        }
        for (name, src) in [
            ("layout.rs", include_str!("layout.rs")),
            ("layout_drag.rs", include_str!("layout_drag.rs")),
            ("layout_model.rs", include_str!("layout_model.rs")),
            ("layout_persist.rs", include_str!("layout_persist.rs")),
            ("layout_window.rs", include_str!("layout_window.rs")),
        ] {
            let prod = production_part(src);
            assert!(
                !prod.contains(".inner.lock().unwrap()") && !prod.contains(".inner.lock().expect("),
                "{name} 存在未处理的布局锁：poison 后必须返回 Err 或提前返回，不得 panic"
            );
        }
    }

    /// 回归守卫：调和体只能经 `ui_snapshot` 读模型。
    /// 该函数内部建窗，而建窗路径（`seed_window_bounds` → `write_bounds`）会在同线程二次取同一把
    /// 非可重入锁；函数自身一旦持有 guard 就自死锁（锁永不释放 → 新窗口 bootstrap 永久挂起、
    /// 主窗口冻结）。运行期无法单测（需 AppHandle），故静态锁死「读模型只走快照函数」这一形态。
    #[test]
    fn reconcile_reads_model_through_snapshot_only() {
        let src = include_str!("layout_window.rs");
        let prod = src.split("#[cfg(test)]").next().unwrap_or(src);
        let body = function_body(prod, "fn reconcile_windows_once")
            .expect("reconcile_windows_once 必须存在");
        assert!(
            !body.contains(".inner.lock()"),
            "调和体不得自行取布局锁（建窗回调 write_bounds 会二次取锁 → 自死锁），请经 ui_snapshot 读快照"
        );
        assert!(
            body.contains("ui_snapshot("),
            "调和体必须经 ui_snapshot 读模型快照"
        );
        // 串行化包装只做加锁与调用，不得在窗口动作期间持锁
        let wrapper = function_body(prod, "fn reconcile_panel_windows")
            .expect("reconcile_panel_windows 必须存在");
        assert!(
            !wrapper.contains(".inner.lock()"),
            "调和串行化包装不得取布局锁（窗口动作在调和体里执行）"
        );
        // 调和体只能从包装调用（绕过串行化 = 原竞态回归）；出现次数 = 定义 1 + 调用 1
        assert_eq!(
            prod.matches("reconcile_windows_once(").count(),
            2,
            "reconcile_windows_once 只允许定义处与被 reconcile_panel_windows 调用各一次"
        );
    }

    /// 按函数名取函数体源码（首个顶格 `}` 为止；按行切分，与行尾风格无关）。
    fn function_body<'a>(src: &'a str, signature: &str) -> Option<String> {
        let after = src.split(signature).nth(1)?;
        let lines: Vec<&str> = after.lines().take_while(|line| *line != "}").collect();
        Some(lines.join("\n"))
    }

    fn ui_with(tree: LayoutNode) -> AppUiState {
        AppUiState {
            schema: UI_STATE_SCHEMA.into(),
            scenes: vec![Scene {
                id: DEFAULT_SCENE_ID.into(),
                name: "默认".into(),
                home_layout: create_home_layout(),
                active_layout_id: Some("l1".into()),
                layouts: vec![WorkspaceLayout { id: "l1".into(), name: "L".into(), tree }],
            }],
            active_scene_id: Some(DEFAULT_SCENE_ID.into()),
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
    fn add_and_activate_view() {
        let mut ui = ui_with(two_panels_horizontal());
        let _ = apply_layout_op(&mut ui, &LayoutOp::AddView { panel_id: "p1".into(), view: "search".into() });
        let p1 = find_panel(&active_layout(&ui).tree, "p1").unwrap();
        assert!(p1.iter().any(|t| t.view == "search"));
        // 已占用视图不可重复添加
        let _ = apply_layout_op(&mut ui, &LayoutOp::AddView { panel_id: "p1".into(), view: "canvas".into() });
        let p1 = find_panel(&active_layout(&ui).tree, "p1").unwrap();
        assert_eq!(p1.iter().filter(|t| t.view == "canvas").count(), 0);
    }

    #[test]
    fn create_detached_window_direct_tear_off() {
        let mut ui = ui_with(two_panels_horizontal());
        let bounds = WindowBounds { x: 0.0, y: 0.0, width: 420.0, height: 560.0, scale: 0.0 };
        let r = apply_layout_op(
            &mut ui,
            &LayoutOp::CreateDetachedWindow { view: "search".into(), bounds: bounds.clone(), restore_on_launch: Some(false) },
        );
        // 免面板直撕：主窗口面板不动，撕裂窗口承载单视图
        assert!(r.detached_window.is_some());
        let w = &ui.detached_windows[0];
        assert_eq!(w.tabs.len(), 1);
        assert_eq!(w.tabs[0].view, "search");
        assert!(!w.hidden);
        assert!(!w.restore_on_launch);
        let p1 = find_panel(&active_layout(&ui).tree, "p1").unwrap();
        assert_eq!(p1.len(), 1);
        // 视图全局唯一：被占用 = 忽略（不建条目）
        let r2 = apply_layout_op(
            &mut ui,
            &LayoutOp::CreateDetachedWindow { view: "search".into(), bounds, restore_on_launch: None },
        );
        assert!(r2.detached_window.is_none());
        assert_eq!(ui.detached_windows.len(), 1);
    }

    #[test]
    fn hide_and_show_detached_window_toggle_model_flag() {
        let mut ui = ui_with(two_panels_horizontal());
        let tab_id = find_panel(&active_layout(&ui).tree, "p2").unwrap()[0].id.clone();
        let bounds = WindowBounds { x: 0.0, y: 0.0, width: 420.0, height: 560.0, scale: 0.0 };
        let _ = apply_layout_op(&mut ui, &LayoutOp::TearOff { panel_id: "p2".into(), tab_id: tab_id.clone(), bounds });
        // 默认参与启动恢复
        assert!(ui.detached_windows[0].restore_on_launch);
        let wid = ui.detached_windows[0].id.clone();
        let _ = apply_layout_op(&mut ui, &LayoutOp::HideDetachedWindow { window_id: wid.clone() });
        assert!(ui.detached_windows[0].hidden);
        let _ = apply_layout_op(&mut ui, &LayoutOp::ShowDetachedWindow { window_id: wid });
        assert!(!ui.detached_windows[0].hidden);
        // 条目不存在的显隐 = 忽略（不 panic 不新建）
        let _ = apply_layout_op(&mut ui, &LayoutOp::HideDetachedWindow { window_id: "ghost".into() });
        assert_eq!(ui.detached_windows.len(), 1);
    }

    #[test]
    fn tear_off_and_dock() {
        let mut ui = ui_with(two_panels_horizontal());
        let tab_id = find_panel(&active_layout(&ui).tree, "p2").unwrap()[0].id.clone();
        let bounds = WindowBounds { x: 0.0, y: 0.0, width: 420.0, height: 560.0, scale: 0.0 };
        let r = apply_layout_op(&mut ui, &LayoutOp::TearOff { panel_id: "p2".into(), tab_id: tab_id.clone(), bounds: bounds.clone() });
        assert!(r.detached_window.is_some());
        assert_eq!(ui.detached_windows.len(), 1);
        let p2 = find_panel(&active_layout(&ui).tree, "p2").unwrap();
        assert!(p2.is_empty());
        // 拖回主窗口
        let _ = apply_layout_op(&mut ui, &LayoutOp::DockIntoPanel { panel_id: "p2".into(), tab_id: tab_id.clone(), index: None });
        assert!(ui.detached_windows.is_empty());
        let p2 = find_panel(&active_layout(&ui).tree, "p2").unwrap();
        assert!(p2.iter().any(|t| t.id == tab_id));
    }

    #[test]
    fn locked_tab_cannot_close() {
        let mut ui = ui_with(panel("p1", &["files", "canvas"]));
        let tid = "t-canvas".to_string();
        let _ = apply_layout_op(&mut ui, &LayoutOp::SetLocked { panel_id: "p1".into(), tab_id: tid.clone(), locked: true });
        let _ = apply_layout_op(&mut ui, &LayoutOp::CloseTab { panel_id: "p1".into(), tab_id: tid.clone() });
        let p1 = find_panel(&active_layout(&ui).tree, "p1").unwrap();
        assert_eq!(p1.len(), 2);
        let _ = apply_layout_op(&mut ui, &LayoutOp::SetLocked { panel_id: "p1".into(), tab_id: tid.clone(), locked: false });
        let _ = apply_layout_op(&mut ui, &LayoutOp::CloseTab { panel_id: "p1".into(), tab_id: tid.clone() });
        let p1 = find_panel(&active_layout(&ui).tree, "p1").unwrap();
        assert_eq!(p1.len(), 1);
    }

    #[test]
    fn dock_into_phantom_window_does_not_lose_tab() {
        // 幽灵窗口（OS 窗口尚在但条目已移除）：停靠目标不存在时不得从树移除标签（防丢失）
        let mut ui = ui_with(two_panels_horizontal());
        let tab_id = find_panel(&active_layout(&ui).tree, "p2").unwrap()[0].id.clone();
        let _ = apply_layout_op(&mut ui, &LayoutOp::DockIntoDetached {
            window_id: "ghost".into(),
            tab_id: tab_id.clone(),
            index: None,
        });
        // 标签仍留在树面板（未丢失）
        let p2 = find_panel(&active_layout(&ui).tree, "p2").unwrap();
        assert!(p2.iter().any(|t| t.id == tab_id));
        assert!(ui.detached_windows.is_empty());
    }

    // ---- 布局操作应用（apply_layout_op）----

    #[test]
    fn set_tab_view_switch_locked_occupied() {
        let mut ui = ui_with(two_panels_horizontal());
        // 切换生效
        let _ = apply_layout_op(&mut ui, &LayoutOp::SetTabView { panel_id: "p1".into(), tab_id: "t-files".into(), view: "search".into() });
        let p1 = find_panel(&active_layout(&ui).tree, "p1").unwrap();
        assert!(p1.iter().any(|t| t.id == "t-files" && t.view == "search"));
        // 锁定标签拒切
        let _ = apply_layout_op(&mut ui, &LayoutOp::SetLocked { panel_id: "p1".into(), tab_id: "t-files".into(), locked: true });
        let _ = apply_layout_op(&mut ui, &LayoutOp::SetTabView { panel_id: "p1".into(), tab_id: "t-files".into(), view: "note".into() });
        let p1 = find_panel(&active_layout(&ui).tree, "p1").unwrap();
        assert!(p1.iter().any(|t| t.id == "t-files" && t.view == "search"));
        // 目标视图被其他位置占用 = 忽略（canvas 在 p2）
        let _ = apply_layout_op(&mut ui, &LayoutOp::AddView { panel_id: "p1".into(), view: "recent".into() });
        let recent_id = find_panel(&active_layout(&ui).tree, "p1").unwrap().iter().find(|t| t.view == "recent").unwrap().id.clone();
        let _ = apply_layout_op(&mut ui, &LayoutOp::SetTabView { panel_id: "p1".into(), tab_id: recent_id.clone(), view: "canvas".into() });
        let p1 = find_panel(&active_layout(&ui).tree, "p1").unwrap();
        assert!(p1.iter().any(|t| t.id == recent_id && t.view == "recent"));
    }

    #[test]
    fn move_tab_within_index() {
        let mut ui = ui_with(panel("p1", &["files", "canvas", "search", "note"]));
        // 前移：search(from 2) → 0
        let _ = apply_layout_op(&mut ui, &LayoutOp::MoveTabWithin { panel_id: "p1".into(), tab_id: "t-search".into(), to_index: 0 });
        let p1 = find_panel(&active_layout(&ui).tree, "p1").unwrap();
        let views: Vec<&str> = p1.iter().map(|t| t.view.as_str()).collect();
        assert_eq!(views, vec!["search", "files", "canvas", "note"]);
        // 后移：files(from 1) → 3（按移除后下标插入）
        let _ = apply_layout_op(&mut ui, &LayoutOp::MoveTabWithin { panel_id: "p1".into(), tab_id: "t-files".into(), to_index: 3 });
        let p1 = find_panel(&active_layout(&ui).tree, "p1").unwrap();
        let views: Vec<&str> = p1.iter().map(|t| t.view.as_str()).collect();
        assert_eq!(views, vec!["search", "canvas", "files", "note"]);
    }

    #[test]
    fn move_tab_between_index() {
        // 默认尾部
        let mut ui = ui_with(two_panels_horizontal());
        let _ = apply_layout_op(&mut ui, &LayoutOp::MoveTabBetween { from_panel_id: "p1".into(), to_panel_id: "p2".into(), tab_id: "t-files".into(), index: None });
        let p1 = find_panel(&active_layout(&ui).tree, "p1").unwrap();
        let p2 = find_panel(&active_layout(&ui).tree, "p2").unwrap();
        assert!(p1.is_empty());
        let views: Vec<&str> = p2.iter().map(|t| t.view.as_str()).collect();
        assert_eq!(views, vec!["canvas", "files"]);
        // 指定插入位
        let mut ui = ui_with(two_panels_horizontal());
        let _ = apply_layout_op(&mut ui, &LayoutOp::AddView { panel_id: "p1".into(), view: "search".into() });
        let search_id = find_panel(&active_layout(&ui).tree, "p1").unwrap().iter().find(|t| t.view == "search").unwrap().id.clone();
        let _ = apply_layout_op(&mut ui, &LayoutOp::MoveTabBetween { from_panel_id: "p1".into(), to_panel_id: "p2".into(), tab_id: search_id.clone(), index: Some(0) });
        let p2 = find_panel(&active_layout(&ui).tree, "p2").unwrap();
        let views: Vec<&str> = p2.iter().map(|t| t.view.as_str()).collect();
        assert_eq!(views, vec!["search", "canvas"]);
    }

    #[test]
    fn tear_off_from_detached_moves_tab_and_prunes_source() {
        let mut ui = ui_with(two_panels_horizontal());
        let b = WindowBounds { x: 0.0, y: 0.0, width: 420.0, height: 560.0, scale: 0.0 };
        let r = apply_layout_op(&mut ui, &LayoutOp::TearOff { panel_id: "p2".into(), tab_id: "t-canvas".into(), bounds: b.clone() });
        let w1 = r.detached_window.unwrap().id;
        let _ = apply_layout_op(&mut ui, &LayoutOp::DetachedAddView { window_id: w1.clone(), view: "note".into() });
        assert_eq!(ui.detached_windows.iter().find(|w| w.id == w1).unwrap().tabs.len(), 2);
        let note_id = ui.detached_windows.iter().find(|w| w.id == w1).unwrap().tabs.iter().find(|t| t.view == "note").unwrap().id.clone();
        // 再撕裂：标签移到新窗口条目，源窗口仍保留一标签
        let r2 = apply_layout_op(&mut ui, &LayoutOp::TearOffFromDetached { window_id: w1.clone(), tab_id: note_id.clone(), bounds: b.clone() });
        let w2 = r2.detached_window.unwrap().id;
        assert_ne!(w1, w2);
        let tabs1: Vec<&str> = ui.detached_windows.iter().find(|w| w.id == w1).unwrap().tabs.iter().map(|t| t.view.as_str()).collect();
        let tabs2: Vec<&str> = ui.detached_windows.iter().find(|w| w.id == w2).unwrap().tabs.iter().map(|t| t.view.as_str()).collect();
        assert_eq!(tabs1, vec!["canvas"]);
        assert_eq!(tabs2, vec!["note"]);
        // 源窗口拖空自动移除（最后标签再撕裂）
        let _ = apply_layout_op(&mut ui, &LayoutOp::TearOffFromDetached { window_id: w1.clone(), tab_id: "t-canvas".into(), bounds: b.clone() });
        assert!(!ui.detached_windows.iter().any(|w| w.id == w1));
        assert_eq!(ui.detached_windows.len(), 2); // w2 + 新建 w3
    }

    #[test]
    fn dock_into_detached_tree_and_detached_sources() {
        let mut ui = ui_with(two_panels_horizontal());
        let b = WindowBounds { x: 0.0, y: 0.0, width: 420.0, height: 560.0, scale: 0.0 };
        let r = apply_layout_op(&mut ui, &LayoutOp::TearOff { panel_id: "p2".into(), tab_id: "t-canvas".into(), bounds: b.clone() });
        let w1 = r.detached_window.unwrap().id;
        // 树来源停靠
        let _ = apply_layout_op(&mut ui, &LayoutOp::DockIntoDetached { window_id: w1.clone(), tab_id: "t-files".into(), index: None });
        let tabs: Vec<&str> = ui.detached_windows.iter().find(|w| w.id == w1).unwrap().tabs.iter().map(|t| t.view.as_str()).collect();
        assert_eq!(tabs, vec!["canvas", "files"]);
        let p1 = find_panel(&active_layout(&ui).tree, "p1").unwrap();
        assert!(p1.is_empty());
        // 另一撕裂窗口来源停靠：t-files 撕裂到 w2 → 停回 w1，w2 空条目自动移除
        let r2 = apply_layout_op(&mut ui, &LayoutOp::TearOffFromDetached { window_id: w1.clone(), tab_id: "t-files".into(), bounds: b.clone() });
        let w2 = r2.detached_window.unwrap().id;
        assert_eq!(ui.detached_windows.len(), 2);
        let _ = apply_layout_op(&mut ui, &LayoutOp::DockIntoDetached { window_id: w1.clone(), tab_id: "t-files".into(), index: None });
        assert!(!ui.detached_windows.iter().any(|w| w.id == w2));
        let tabs: Vec<&str> = ui.detached_windows.iter().find(|w| w.id == w1).unwrap().tabs.iter().map(|t| t.view.as_str()).collect();
        assert_eq!(tabs, vec!["canvas", "files"]);
        // 同窗口 = 组内排序
        let _ = apply_layout_op(&mut ui, &LayoutOp::DockIntoDetached { window_id: w1.clone(), tab_id: "t-files".into(), index: Some(0) });
        let tabs: Vec<&str> = ui.detached_windows.iter().find(|w| w.id == w1).unwrap().tabs.iter().map(|t| t.view.as_str()).collect();
        assert_eq!(tabs, vec!["files", "canvas"]);
    }

    #[test]
    fn toggle_window_by_view_and_pin() {
        let mut ui = ui_with(two_panels_horizontal());
        let b = WindowBounds { x: 0.0, y: 0.0, width: 440.0, height: 680.0, scale: 1.0 };
        let opts = WindowOptions {
            always_on_top: true,
            skip_taskbar: true,
            hide_on_blur: true,
            close_hides: true,
        };
        // 首次 toggle：以声明选项新建（不参与启动恢复、初始可见）
        let r = apply_layout_op(&mut ui, &LayoutOp::ToggleDetachedWindow { view: "com.test.summon".into(), bounds: b.clone(), options: opts });
        let w1 = r.detached_window.expect("首次 toggle 应建窗");
        assert!(w1.options.always_on_top && !w1.pinned && !w1.restore_on_launch && !w1.hidden);
        // 再次 toggle = 翻转为隐藏
        let _ = apply_layout_op(&mut ui, &LayoutOp::ToggleDetachedWindow { view: "com.test.summon".into(), bounds: b.clone(), options: opts });
        assert!(ui.detached_windows.iter().find(|w| w.id == w1.id).unwrap().hidden);
        // 第三次 toggle = 复原可见（不新建条目）
        let r = apply_layout_op(&mut ui, &LayoutOp::ToggleDetachedWindow { view: "com.test.summon".into(), bounds: b.clone(), options: opts });
        let w = ui.detached_windows.iter().find(|x| x.id == w1.id).unwrap();
        assert_eq!(r.detached_window.as_ref().unwrap().id, w1.id);
        assert!(!w.hidden);
        assert_eq!(ui.detached_windows.len(), 1);
        // 图钉只作用于声明了失焦收起的条目
        let _ = apply_layout_op(&mut ui, &LayoutOp::SetDetachedWindowPinned { window_id: w1.id.clone(), pinned: true });
        assert!(ui.detached_windows[0].pinned);
        // 视图已被 toggle 窗口占用：其他窗口占据同一视图 = 忽略（不建第二条目）
        let _ = apply_layout_op(&mut ui, &LayoutOp::CreateDetachedWindow { view: "com.test.summon".into(), bounds: b, restore_on_launch: Some(true) });
        assert_eq!(ui.detached_windows.len(), 1);
    }

    #[test]
    fn detached_close_lock_view_active_move_add() {
        let mut ui = ui_with(two_panels_horizontal());
        let b = WindowBounds { x: 0.0, y: 0.0, width: 420.0, height: 560.0, scale: 0.0 };
        let r = apply_layout_op(&mut ui, &LayoutOp::TearOff { panel_id: "p2".into(), tab_id: "t-canvas".into(), bounds: b.clone() });
        let w1 = r.detached_window.unwrap().id;
        let _ = apply_layout_op(&mut ui, &LayoutOp::DetachedAddView { window_id: w1.clone(), view: "note".into() });
        let note_id = ui.detached_windows.iter().find(|w| w.id == w1).unwrap().tabs.iter().find(|t| t.view == "note").unwrap().id.clone();
        // 关闭
        let _ = apply_layout_op(&mut ui, &LayoutOp::DetachedCloseTab { window_id: w1.clone(), tab_id: note_id.clone() });
        let w = ui.detached_windows.iter().find(|w| w.id == w1).unwrap();
        assert_eq!(w.tabs.len(), 1);
        // 锁定：拒关 + 拒切视图
        let _ = apply_layout_op(&mut ui, &LayoutOp::DetachedSetLocked { window_id: w1.clone(), tab_id: "t-canvas".into(), locked: true });
        let _ = apply_layout_op(&mut ui, &LayoutOp::DetachedCloseTab { window_id: w1.clone(), tab_id: "t-canvas".into() });
        let _ = apply_layout_op(&mut ui, &LayoutOp::DetachedSetTabView { window_id: w1.clone(), tab_id: "t-canvas".into(), view: "search".into() });
        let w = ui.detached_windows.iter().find(|w| w.id == w1).unwrap();
        assert!(w.tabs.iter().any(|t| t.id == "t-canvas" && t.view == "canvas"));
        // 解锁后切换视图生效；目标视图被树占用 = 忽略（files 在 p1）
        let _ = apply_layout_op(&mut ui, &LayoutOp::DetachedSetLocked { window_id: w1.clone(), tab_id: "t-canvas".into(), locked: false });
        let _ = apply_layout_op(&mut ui, &LayoutOp::DetachedSetTabView { window_id: w1.clone(), tab_id: "t-canvas".into(), view: "search".into() });
        let w = ui.detached_windows.iter().find(|w| w.id == w1).unwrap();
        assert!(w.tabs.iter().any(|t| t.id == "t-canvas" && t.view == "search"));
        let _ = apply_layout_op(&mut ui, &LayoutOp::DetachedSetTabView { window_id: w1.clone(), tab_id: "t-canvas".into(), view: "files".into() });
        let w = ui.detached_windows.iter().find(|w| w.id == w1).unwrap();
        assert!(w.tabs.iter().any(|t| t.id == "t-canvas" && t.view == "search"));
        // 已占用视图不可重复添加
        let _ = apply_layout_op(&mut ui, &LayoutOp::DetachedAddView { window_id: w1.clone(), view: "files".into() });
        let w = ui.detached_windows.iter().find(|w| w.id == w1).unwrap();
        assert_eq!(w.tabs.len(), 1);
        // 解锁 + 加标签 → 激活 + 组内排序
        let _ = apply_layout_op(&mut ui, &LayoutOp::DetachedSetLocked { window_id: w1.clone(), tab_id: "t-canvas".into(), locked: false });
        let _ = apply_layout_op(&mut ui, &LayoutOp::DetachedAddView { window_id: w1.clone(), view: "note".into() });
        let note_id = ui.detached_windows.iter().find(|w| w.id == w1).unwrap().tabs.iter().find(|t| t.view == "note").unwrap().id.clone();
        let _ = apply_layout_op(&mut ui, &LayoutOp::DetachedSetActive { window_id: w1.clone(), tab_id: "t-canvas".into() });
        let w = ui.detached_windows.iter().find(|w| w.id == w1).unwrap();
        assert_eq!(w.active_tab_id.as_deref(), Some("t-canvas"));
        let _ = apply_layout_op(&mut ui, &LayoutOp::DetachedMoveTab { window_id: w1.clone(), tab_id: note_id.clone(), to_index: 0 });
        let w = ui.detached_windows.iter().find(|w| w.id == w1).unwrap();
        let views: Vec<&str> = w.tabs.iter().map(|t| t.view.as_str()).collect();
        assert_eq!(views, vec!["note", "search"]);
    }

    #[test]
    fn set_layout_sizes_updates_ratio() {
        let mut ui = ui_with(two_panels_horizontal());
        let _ = apply_layout_op(&mut ui, &LayoutOp::SetLayoutSizes { split_id: "s1".into(), sizes: vec![30.0, 70.0] });
        let tree = active_layout(&ui).tree;
        match &tree {
            LayoutNode::Split { sizes, .. } => assert_eq!(sizes, &vec![30.0, 70.0]),
            _ => panic!("expected split root"),
        }
        // 未命中 split 不变
        let _ = apply_layout_op(&mut ui, &LayoutOp::SetLayoutSizes { split_id: "nope".into(), sizes: vec![50.0, 50.0] });
        let tree = active_layout(&ui).tree;
        match &tree {
            LayoutNode::Split { sizes, .. } => assert_eq!(sizes, &vec![30.0, 70.0]),
            _ => panic!("expected split root"),
        }
    }

    #[test]
    fn set_layout_sizes_rejects_deformed_patch() {
        // 坏值一律拒收：长度不符 / NaN / 负值 / 全零（和必须 > 0）。
        // 它们不该进模型、不该落盘、也不该被广播（前端按坏值布局会越界或塌陷，且坏状态每次启动都被读回）。
        let bad: Vec<(&str, Vec<f64>)> = vec![
            ("长度过短", vec![50.0]),
            ("长度过长", vec![10.0, 20.0, 70.0]),
            ("NaN", vec![f64::NAN, 50.0]),
            ("无穷", vec![f64::INFINITY, 50.0]),
            ("负值", vec![-5.0, 105.0]),
            ("全零", vec![0.0, 0.0]),
            ("空", vec![]),
        ];
        for (label, sizes) in bad {
            let mut ui = ui_with(two_panels_horizontal());
            let op = LayoutOp::SetLayoutSizes { split_id: "s1".into(), sizes };
            assert!(!op_passes_shape_check(&ui, &op), "{label} 应被形状校验拒绝");
            // 校验拒不通过时命令层提前返回，模型不得被改动
            if op_passes_shape_check(&ui, &op) {
                let _ = apply_layout_op(&mut ui, &op);
            }
            let tree = active_layout(&ui).tree;
            match &tree {
                LayoutNode::Split { sizes, .. } => assert_eq!(sizes, &vec![20.0, 80.0], "{label} 后模型应保持不变"),
                _ => panic!("expected split root"),
            }
        }
        // 形状合法（和不为 100，由前端归一化口径保证）仍被接受
        let mut ui = ui_with(two_panels_horizontal());
        let op = LayoutOp::SetLayoutSizes { split_id: "s1".into(), sizes: vec![33.3, 66.7] };
        assert!(op_passes_shape_check(&ui, &op));
        let _ = apply_layout_op(&mut ui, &op);
        match &active_layout(&ui).tree {
            LayoutNode::Split { sizes, .. } => assert_eq!(sizes, &vec![33.3, 66.7]),
            _ => panic!("expected split root"),
        }
    }

    #[test]
    fn sizes_valid_for_rules() {
        assert!(sizes_valid_for(&[50.0, 50.0], 2));
        assert!(sizes_valid_for(&[0.0, 100.0], 2));
        assert!(!sizes_valid_for(&[50.0, 50.0], 3));
        assert!(!sizes_valid_for(&[f64::NAN, 50.0], 2));
        assert!(!sizes_valid_for(&[-1.0, 101.0], 2));
        assert!(!sizes_valid_for(&[0.0, 0.0], 2));
        assert!(!sizes_valid_for(&[], 0));
    }

    #[test]
    fn layout_add_rename_activate_delete() {
        let mut ui = ui_with(two_panels_horizontal());
        fn layouts(ui: &AppUiState) -> &Vec<WorkspaceLayout> {
            &active_scene(ui).layouts
        }
        // AddLayout：全新空面板（不复制当前布局）+ 命名去重「布局 N」+ 激活新布局
        let _ = apply_layout_op(&mut ui, &LayoutOp::AddLayout);
        let _ = apply_layout_op(&mut ui, &LayoutOp::AddLayout);
        assert_eq!(layouts(&ui).len(), 3);
        assert_eq!(layouts(&ui)[1].name, "布局 1");
        assert_eq!(layouts(&ui)[2].name, "布局 2");
        assert_eq!(active_scene(&ui).active_layout_id.as_deref(), Some(layouts(&ui)[2].id.as_str()));
        for new_layout in [&layouts(&ui)[1], &layouts(&ui)[2]] {
            match &new_layout.tree {
                LayoutNode::Panel { tabs, active_tab_id, .. } => {
                    assert!(tabs.is_empty(), "新建布局 = 空面板占位");
                    assert_eq!(active_tab_id, &None);
                }
                _ => panic!("expected blank panel root"),
            }
        }
        // 源布局不受影响
        assert_eq!(layouts(&ui)[0].name, "L");
        match &layouts(&ui)[0].tree {
            LayoutNode::Split { children, .. } => assert_eq!(children.len(), 2),
            _ => panic!("expected split root"),
        }
        // RenameLayout：修剪空白；主页固定不可重命名；空名忽略
        let _ = apply_layout_op(&mut ui, &LayoutOp::RenameLayout { id: "l1".into(), name: "  主工作  ".into() });
        assert_eq!(layouts(&ui)[0].name, "主工作");
        let _ = apply_layout_op(&mut ui, &LayoutOp::RenameLayout { id: "l1".into(), name: "  ".into() });
        assert_eq!(layouts(&ui)[0].name, "主工作");
        // ActivateLayout：命中切换；缺失忽略
        let _ = apply_layout_op(&mut ui, &LayoutOp::ActivateLayout { id: "l1".into() });
        assert_eq!(ui.active_layout_id.as_deref(), Some("l1"));
        let _ = apply_layout_op(&mut ui, &LayoutOp::ActivateLayout { id: "nope".into() });
        assert_eq!(ui.active_layout_id.as_deref(), Some("l1"));
        // DeleteLayout：删除激活布局回退第一个；删到只剩一个后拒删
        let _ = apply_layout_op(&mut ui, &LayoutOp::DeleteLayout { id: "l1".into() });
        assert_eq!(layouts(&ui).len(), 2);
        assert_eq!(ui.active_layout_id.as_deref(), Some(layouts(&ui)[0].id.as_str()));
        let id0 = layouts(&ui)[0].id.clone();
        let _ = apply_layout_op(&mut ui, &LayoutOp::DeleteLayout { id: id0 });
        assert_eq!(layouts(&ui).len(), 1);
        let id0 = layouts(&ui)[0].id.clone();
        let _ = apply_layout_op(&mut ui, &LayoutOp::DeleteLayout { id: id0 });
        assert_eq!(layouts(&ui).len(), 1); // 最后一个不可删
    }

    #[test]
    fn delete_layout_repairs_scene_memory_independent_of_top_active() {
        fn layouts(ui: &AppUiState) -> &Vec<WorkspaceLayout> {
            &active_scene(ui).layouts
        }
        // 记忆与顶层激活是独立态（normalize 允许记忆 ≠ 顶层激活，如顶层失效回退主页而记忆有效）：
        // 删除「记忆指向但非顶层激活」的布局必须修复场景记忆，否则悬垂记忆经
        // ActivateScene 转成顶层激活失效 → active_layout 的不变量 expect 被击穿（锁毒化）
        let mut ui = ui_with(two_panels_horizontal());
        let _ = apply_layout_op(&mut ui, &LayoutOp::AddLayout); // [l1, 布局1]
        // 构造分歧态：顶层激活 = 主页，场景记忆 = 布局1
        let copy_id = layouts(&ui)[1].id.clone();
        let _ = apply_layout_op(&mut ui, &LayoutOp::ActivateLayout { id: copy_id.clone() });
        ui.active_layout_id = Some(HOME_LAYOUT_ID.into());
        // 删除记忆指向的布局（非顶层激活）
        let _ = apply_layout_op(&mut ui, &LayoutOp::DeleteLayout { id: copy_id });
        // 记忆修复为场景内第一个，顶层激活（主页）不受影响，不变量成立
        assert_eq!(active_scene(&ui).active_layout_id.as_deref(), Some("l1"));
        assert_eq!(ui.active_layout_id.as_deref(), Some(HOME_LAYOUT_ID));
        let _ = apply_layout_op(&mut ui, &LayoutOp::ActivateScene { id: DEFAULT_SCENE_ID.into() });
        assert_eq!(active_layout(&ui).id, "l1");
        // 顶层激活指向被删布局而记忆另有所指：顶层跟随记忆，不吞掉有效记忆
        let _ = apply_layout_op(&mut ui, &LayoutOp::AddLayout); // [l1, 布局1]
        ui.active_layout_id = Some("l1".into());
        ui.scenes[0].active_layout_id = Some(layouts(&ui)[1].id.clone());
        let _ = apply_layout_op(&mut ui, &LayoutOp::DeleteLayout { id: "l1".into() });
        assert_eq!(
            ui.active_layout_id.as_deref(),
            Some(layouts(&ui)[0].id.as_str()),
            "顶层激活应跟随场景记忆回落"
        );
        assert_eq!(active_scene(&ui).active_layout_id.as_deref(), Some(layouts(&ui)[0].id.as_str()));
    }
}
