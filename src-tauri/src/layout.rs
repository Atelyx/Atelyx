//! 布局迷你窗口管理器：多窗口面板体系的唯一权威（撕裂窗口方案）。
//!
//! 布局模型（布局列表 + 激活布局 + 撕裂窗口）的唯一真相在本模块族。任何窗口的前端
//! 都不持有权威——只发命令（`layout_op`/`ui_state_patch`/拖拽）与接收广播
//! （`layout-broadcast`/`drag-session`）渲染自身切片。`app_data_dir/ui-state.json`
//! 由本模块族单一写者持久化（防双写竞争），磁盘 schema 与前端 `types/uiState.ts` 对齐
//! （`atelyx-ui-state/v1`），字段名/形状不变，无需迁移。
//!
//! 设计动机：webview 之间无法共享内存，跨窗口一致性只能靠「一个权威 + 广播」。
//! 布局操作全部在 `apply_layout_op` 校验（视图全局唯一、主页固定置顶、锁定语义、
//! 树形状）后应用，前端只负责渲染与像素级命中。跨窗口拖拽会话见 `layout_drag`。
//!
//! 本文件 = 命令面 + 布局状态 + 操作应用中枢；纯模型/树操作见 `layout_model`，
//! 持久化/广播见 `layout_persist`，窗口事件/OS 生命周期见 `layout_window`。

use std::sync::Mutex;

use tauri::{AppHandle, Manager, State};

use crate::layout_drag::{DragHit, DragSession};
use crate::layout_model::{
    active_layout, apply_tab_group_detached, apply_tab_group_panel, close_panel_op, collect_tabs,
    create_tab, find_panel, find_tab_in_detached, find_tab_in_tree, group_activate_tab,
    group_add_tab, group_move_tab, group_of, group_of_detached, group_remove_tab,
    group_set_tab_locked, group_set_tab_view, map_detached, map_panel, next_layout_name,
    prune_empty_windows, regenerate_ids, set_active_tree, set_layout_sizes_op, split_panel_op,
    tear_off_from_panel_op, AppUiState, DetachedWindow, LayoutOp, LayoutOpResult, UiStatePatch,
    MAX_RECENT_FILES, HOME_LAYOUT_ID, WorkspaceLayout,
};
use crate::layout_persist::{broadcast_layout, persist_now, schedule_persist};
use crate::layout_window::reconcile_panel_windows;

// 命令面与窗口生命周期对外再导出：lib.rs 注册命令、windows.rs 复用窗口钩子/常量，
// 前端契约类型亦经 `layout::` 路径引用（保持既有路径不变）。
// tauri 命令宏会生成同名的 `__cmd__X`/`__tauri_command_name_X` 宏并随函数再导出，
// generate_handler 按函数路径解析它们——故这里必须一并再导出，否则 lib.rs 注册会断。
pub use crate::layout_drag::{
    __cmd__drag_end, __cmd__drag_hit, __cmd__drag_update, __tauri_command_name_drag_end,
    __tauri_command_name_drag_hit, __tauri_command_name_drag_update, drag_end, drag_hit,
    drag_update,
};
pub use crate::layout_model::WindowBounds;
pub use crate::layout_persist::load_from_disk;
pub use crate::layout_window::{seed_window_bounds, window_event_handler, PANEL_LABEL_PREFIX};

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
            }),
        }
    }
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
            let tree = active_layout(ui).tree;
            if let Some((new_tree, tab)) = tear_off_from_panel_op(&tree, panel_id, tab_id) {
                let win = DetachedWindow { id: nanoid::nanoid!(), tabs: vec![tab.clone()], active_tab_id: Some(tab.id.clone()), bounds: bounds.clone() };
                set_active_tree(ui, new_tree);
                ui.detached_windows.push(win.clone());
                result.detached_window = Some(win);
            }
        }
        LayoutOp::TearOffFromDetached { window_id, tab_id, bounds } => {
            if let Some((src, tab)) = find_tab_in_detached(&ui.detached_windows, tab_id) {
                if src == *window_id {
                    let win = DetachedWindow { id: nanoid::nanoid!(), tabs: vec![tab.clone()], active_tab_id: Some(tab.id.clone()), bounds: bounds.clone() };
                    let next = map_detached(&ui.detached_windows, window_id, &|w| apply_tab_group_detached(w, group_remove_tab(&group_of_detached(w), tab_id)));
                    let mut next = prune_empty_windows(next);
                    next.push(win.clone());
                    ui.detached_windows = next;
                    result.detached_window = Some(win);
                }
            }
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
        LayoutOp::SetLayoutSizes { split_id, sizes } => {
            let tree = active_layout(ui).tree;
            let tree = set_layout_sizes_op(&tree, split_id, sizes);
            set_active_tree(ui, tree);
        }
        LayoutOp::AddLayout => {
            let active = active_layout(ui);
            let names: Vec<String> = ui.workspace_layouts.iter().map(|l| l.name.clone()).collect();
            let copy = WorkspaceLayout { id: nanoid::nanoid!(), name: next_layout_name(&names), tree: regenerate_ids(&active.tree) };
            ui.workspace_layouts.push(copy);
            ui.active_layout_id = ui.workspace_layouts.last().map(|l| l.id.clone());
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
            for l in &mut ui.workspace_layouts {
                if l.id == *id {
                    l.name = trimmed.to_string();
                    break;
                }
            }
        }
        LayoutOp::DeleteLayout { id } => {
            if id == HOME_LAYOUT_ID || ui.workspace_layouts.len() <= 1 {
                return result;
            }
            let removing_active = ui.active_layout_id.as_deref() == Some(id);
            ui.workspace_layouts.retain(|l| l.id != *id);
            if removing_active {
                ui.active_layout_id = ui.workspace_layouts.first().map(|l| l.id.clone());
                ui.focused_panel_id = None;
            }
        }
        LayoutOp::ActivateLayout { id } => {
            if ui.workspace_layouts.iter().any(|l| l.id == *id) {
                ui.active_layout_id = Some(id.clone());
                ui.focused_panel_id = None;
            }
        }
        LayoutOp::MoveLayout { from_index, to_index } => {
            let len = ui.workspace_layouts.len();
            if from_index == to_index || *from_index >= len || *to_index >= len {
                return result;
            }
            // 主页固定置顶：禁止移动主页本身（index 0），也禁止把其他布局拖到主页之前（toIndex 0）
            let from_is_home = ui.workspace_layouts.get(*from_index).map(|l| l.id == HOME_LAYOUT_ID).unwrap_or(false);
            if from_is_home || *to_index == 0 {
                return result;
            }
            let moved = ui.workspace_layouts.remove(*from_index);
            ui.workspace_layouts.insert(*to_index, moved);
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

/// 应用一个布局操作：校验 + 变更 + 广播 + 调度落盘。返回操作结果（splitPanel/tearOff 用）。
/// 布局模型变更后同步窗口生命周期：reconcile 自锁读当前模型补建缺失/回收幽灵。
#[tauri::command]
pub async fn layout_op(app: AppHandle, op: LayoutOp) -> Result<LayoutOpResult, String> {
    let state = app.state::<LayoutState>();
    let mut inner = state.inner.lock().map_err(|e| e.to_string())?;
    if !inner.loaded {
        return Ok(LayoutOpResult::default());
    }
    let result = apply_layout_op(&mut inner.ui, &op);
    inner.dirty = true;
    let ui = inner.ui.clone();
    drop(inner);
    reconcile_panel_windows(&app);
    schedule_persist(&app, &state);
    broadcast_layout(&app, &ui);
    Ok(result)
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

/// 布局调和（主窗口启动后调用）：种子化全部窗口 bounds + 补建持久化撕裂窗口的 OS 窗口。
#[tauri::command]
pub async fn layout_reconcile(app: AppHandle) -> Result<(), String> {
    // 种子化 bounds：主窗口启动后未移动过时 on_window_event 不触发，拖拽落点解析读不到
    for label in app.webview_windows().keys().cloned().collect::<Vec<_>>() {
        seed_window_bounds(&app, &label);
    }
    reconcile_panel_windows(&app);
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
    use crate::layout_model::{LayoutNode, TabItem, UI_STATE_SCHEMA, WorkspaceLayout};

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

    fn ui_with(tree: LayoutNode) -> AppUiState {
        AppUiState {
            schema: UI_STATE_SCHEMA.into(),
            workspace_layouts: vec![WorkspaceLayout { id: "l1".into(), name: "L".into(), tree }],
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
    fn layout_add_rename_activate_delete() {
        let mut ui = ui_with(two_panels_horizontal());
        // AddLayout：复制树 + 命名去重「布局 N」+ 激活新布局
        let _ = apply_layout_op(&mut ui, &LayoutOp::AddLayout);
        let _ = apply_layout_op(&mut ui, &LayoutOp::AddLayout);
        assert_eq!(ui.workspace_layouts.len(), 3);
        assert_eq!(ui.workspace_layouts[1].name, "布局 1");
        assert_eq!(ui.workspace_layouts[2].name, "布局 2");
        assert_eq!(ui.active_layout_id.as_deref(), Some(ui.workspace_layouts[2].id.as_str()));
        // 复制树：视图种类一致但节点 id 全部重新生成（独立副本）
        let l0 = ui.workspace_layouts[0].clone();
        let l2 = ui.workspace_layouts[2].clone();
        let mut t0 = Vec::new();
        collect_tabs(&l0.tree, &mut t0);
        let mut t2 = Vec::new();
        collect_tabs(&l2.tree, &mut t2);
        let v0: Vec<&str> = t0.iter().map(|t| t.view.as_str()).collect();
        let v2: Vec<&str> = t2.iter().map(|t| t.view.as_str()).collect();
        assert_eq!(v0, v2);
        assert_ne!(l0.tree.node_id(), l2.tree.node_id());
        // RenameLayout：修剪空白；主页固定不可重命名；空名忽略
        let _ = apply_layout_op(&mut ui, &LayoutOp::RenameLayout { id: "l1".into(), name: "  主工作  ".into() });
        assert_eq!(ui.workspace_layouts[0].name, "主工作");
        let _ = apply_layout_op(&mut ui, &LayoutOp::RenameLayout { id: "l1".into(), name: "  ".into() });
        assert_eq!(ui.workspace_layouts[0].name, "主工作");
        // ActivateLayout：命中切换；缺失忽略
        let _ = apply_layout_op(&mut ui, &LayoutOp::ActivateLayout { id: "l1".into() });
        assert_eq!(ui.active_layout_id.as_deref(), Some("l1"));
        let _ = apply_layout_op(&mut ui, &LayoutOp::ActivateLayout { id: "nope".into() });
        assert_eq!(ui.active_layout_id.as_deref(), Some("l1"));
        // DeleteLayout：删除激活布局回退第一个；删到只剩一个后拒删
        let _ = apply_layout_op(&mut ui, &LayoutOp::DeleteLayout { id: "l1".into() });
        assert_eq!(ui.workspace_layouts.len(), 2);
        assert_eq!(ui.active_layout_id.as_deref(), Some(ui.workspace_layouts[0].id.as_str()));
        let id0 = ui.workspace_layouts[0].id.clone();
        let _ = apply_layout_op(&mut ui, &LayoutOp::DeleteLayout { id: id0 });
        assert_eq!(ui.workspace_layouts.len(), 1);
        let id0 = ui.workspace_layouts[0].id.clone();
        let _ = apply_layout_op(&mut ui, &LayoutOp::DeleteLayout { id: id0 });
        assert_eq!(ui.workspace_layouts.len(), 1); // 最后一个不可删
    }
}
