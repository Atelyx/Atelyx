//! 布局迷你窗口管理器的窗口事件与 OS 生命周期层：权威 bounds 注册表由窗口事件
//! （Moved/Resized/DPI 变化）驱动、撕裂窗口模型 bounds 同步、撕裂窗口 OS 建/关窗调和。
//! bounds 单一写者：OS 窗口事件 → 注册表 → 撕裂窗口模型（持久化位置）。

use std::collections::HashSet;
use std::sync::Mutex;

use tauri::{AppHandle, Manager};

use crate::layout::LayoutState;
use crate::layout_model::{AppUiState, WindowBounds};

/// 撕裂窗口 label 前缀（与前端 PANEL_LABEL_PREFIX 对齐）。
pub const PANEL_LABEL_PREFIX: &str = "panel-";

/// 窗口事件钩子（Moved/Resized → 权威 bounds 注册表 + 撕裂窗口模型 bounds 同步）。
/// 在 setup 为主窗口注册，在 create_panel_window_internal 为撕裂窗口注册。
pub fn window_event_handler(app: &AppHandle, label: String) -> impl Fn(&tauri::WindowEvent) + Send + 'static {
    let app = app.clone();
    move |event: &tauri::WindowEvent| {
        let win = match app.get_webview_window(&label) {
            Some(w) => w,
            None => return,
        };
        let sf = win.scale_factor().unwrap_or(1.0);
        match event {
            tauri::WindowEvent::Moved(pos) => {
                // 位置由 Moved 更新，尺寸维持
                write_bounds(&app, &label, Some(pos.x as f64 / sf), Some(pos.y as f64 / sf), None, None, sf);
            }
            tauri::WindowEvent::Resized(size) => {
                // 尺寸由 Resized 更新，位置维持
                write_bounds(&app, &label, None, None, Some(size.width as f64 / sf), Some(size.height as f64 / sf), sf);
            }
            tauri::WindowEvent::ScaleFactorChanged { scale_factor, new_inner_size, .. } => {
                // DPI 变化：更新 scale + 内尺寸
                let sf = *scale_factor;
                write_bounds(&app, &label, None, None, Some(new_inner_size.width as f64 / sf), Some(new_inner_size.height as f64 / sf), sf);
            }
            _ => {}
        }
    }
}

/// 写入权威 bounds 注册表（含 scale）+ 同步撕裂窗口模型 bounds（logical px；None = 维持原值）。
/// 拖拽命中/落点解析只读注册表；模型 bounds 为持久化恢复位置（不广播）。
/// bounds 单一写者：OS 窗口事件（Moved/Resized/DPI）与显式种子是注册表唯一写入来源，
/// 前端 op（TearOff）只提供初始位置，模型最终以窗口事件为准。
fn write_bounds(
    app: &AppHandle,
    label: &str,
    x: Option<f64>,
    y: Option<f64>,
    w: Option<f64>,
    h: Option<f64>,
    scale: f64,
) {
    let state = app.state::<LayoutState>();
    let Ok(mut inner) = state.inner.lock() else {
        eprintln!("[layout] 布局状态锁已损坏，放弃写入窗口 bounds");
        return;
    };
    let entry = inner
        .window_bounds
        .entry(label.to_string())
        .or_insert_with(|| WindowBounds { x: 0.0, y: 0.0, width: 0.0, height: 0.0, scale: 1.0 });
    if let Some(x) = x {
        entry.x = x;
    }
    if let Some(y) = y {
        entry.y = y;
    }
    if let Some(w) = w {
        entry.width = w;
    }
    if let Some(h) = h {
        entry.height = h;
    }
    entry.scale = scale;
    let entry_clone = entry.clone();
    // 仅撕裂窗口模型 bounds 实际写回才置 dirty：主窗口移动/缩放不进模型，不触发无谓落盘
    if sync_detached_bounds(&mut inner.ui, label, &entry_clone) {
        inner.dirty = true;
    }
}

/// 种子化窗口权威 bounds（启动/建窗/恢复调和时调用）。
/// `on_window_event` 只在窗口移动/缩放后触发——从未动过的窗口（如启动后未移动的主窗口）
/// 不会注册 bounds，拖拽落点解析会把它当成「窗口外」误判为撕裂。显式种子保证注册表恒完整。
pub fn seed_window_bounds(app: &AppHandle, label: &str) {
    let Some(win) = app.get_webview_window(label) else { return };
    let Ok(sf) = win.scale_factor() else { return };
    let x = win.outer_position().ok().map(|p| p.x as f64 / sf);
    let y = win.outer_position().ok().map(|p| p.y as f64 / sf);
    let w = win.outer_size().ok().map(|s| s.width as f64 / sf);
    let h = win.outer_size().ok().map(|s| s.height as f64 / sf);
    write_bounds(app, label, x, y, w, h, sf);
}

/// 同步撕裂窗口模型 bounds（label 非撕裂窗口或条目不存在 = false；保留 x/y 或 w/h 已更新的一侧）。
fn sync_detached_bounds(ui: &mut AppUiState, label: &str, b: &WindowBounds) -> bool {
    let id = match label.strip_prefix(PANEL_LABEL_PREFIX) {
        Some(id) => id.to_string(),
        None => return false,
    };
    for w in &mut ui.detached_windows {
        if w.id == id {
            w.bounds = b.clone();
            return true;
        }
    }
    false
}

/// 撕裂窗口标题（Rust 侧占位；窗口 boot 后由前端按激活标签更新）。
fn title_of_tabs_rust(tabs: &[crate::layout_model::TabItem], active_tab_id: &Option<String>) -> String {
    let active = tabs.iter().find(|t| Some(&t.id) == active_tab_id.as_ref()).or_else(|| tabs.first());
    active.map(|t| t.view.clone()).unwrap_or_else(|| "面板".to_string())
}

/// 读当前布局模型快照。锁只在本函数内持有：建窗路径（`seed_window_bounds` → `write_bounds`）
/// 会在同线程二次取同一把非可重入锁，调用方持锁期间触发建窗即自死锁。关窗与窗口事件是否同线程
/// 同步派发无保证，故「建窗/关窗/让主事件循环回头」的动作一律在放锁后执行。
fn ui_snapshot(state: &LayoutState) -> Option<AppUiState> {
    let inner = state.inner.lock().ok()?;
    Some(inner.ui.clone())
}

/// 调和串行化锁：布局模型锁在窗口动作期间已放锁，故「模型快照 + OS 窗口集合」的成对读取不再互斥——
/// 两次调和交叉时，一方会把另一方刚建好的窗口当幽灵关掉（其关闭上报又会删掉模型条目，标签丢失）。
/// 本锁与布局模型锁互不嵌套（调用方调本函数前都已放掉模型锁）、窗口动作期间不做 await，故无锁序环；
/// 等待者短暂阻塞换取「每个请求都跑完整一轮」，比标记 + 补跑协议更简单且不会漏唤醒。
static RECONCILE_LOCK: Mutex<()> = Mutex::new(());

/// 撕裂窗口 OS 窗口调和：补建缺失（模型有条目但无对应窗口） + 回收幽灵
/// （OS 窗口存在但模型条目已移除——如条目移除后 OS 窗口未销毁）。
/// 回收幽灵经 win.close() 触发其 JS onCloseRequested（flush 后销毁），
/// `panel_window_closed` 对已移除条目是 no-op、不广播 → 不会回环重触发。
///
/// 幽灵回收同时覆盖「条目移除后关 OS 窗」场景，调用方无需再按 before/after 快照 diff。
pub(crate) fn reconcile_panel_windows(app: &AppHandle) {
    // poison 恢复：上一次调和 panic 不该让窗口生命周期永久停摆（布局模型锁另有自己的 poison 处理）
    let _guard = RECONCILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    reconcile_windows_once(app);
}

/// 单轮调和（只在 `reconcile_panel_windows` 的串行化保护内执行）。
fn reconcile_windows_once(app: &AppHandle) {
    let state = app.state::<LayoutState>();
    let Some(ui) = ui_snapshot(&state) else {
        eprintln!("[layout] 布局状态锁已损坏，放弃窗口调和");
        return;
    };
    let existing: HashSet<String> = app.webview_windows().keys().cloned().collect();
    let wanted: HashSet<String> = ui
        .detached_windows
        .iter()
        .map(|w| format!("{PANEL_LABEL_PREFIX}{}", w.id))
        .collect();
    // 补建缺失
    for w in &ui.detached_windows {
        let label = format!("{PANEL_LABEL_PREFIX}{}", w.id);
        if !existing.contains(&label) {
            crate::commands::windows::create_panel_window_internal(
                app,
                &label,
                &title_of_tabs_rust(&w.tabs, &w.active_tab_id),
                &w.bounds,
            );
        }
    }
    // 回收幽灵（防落点解析把它当停靠目标——条目已移除时标签无处可去会丢失）
    for label in &existing {
        if label.starts_with(PANEL_LABEL_PREFIX) && !wanted.contains(label) {
            crate::commands::windows::close_panel_window_internal(app, label.trim_start_matches(PANEL_LABEL_PREFIX));
        }
    }
}

// ===== 单元测试 =====

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout_model::{AppUiState, DetachedWindow, TabItem, UI_STATE_SCHEMA};

    fn ui_with_detached(wid: &str) -> AppUiState {
        AppUiState {
            schema: UI_STATE_SCHEMA.into(),
            detached_windows: vec![DetachedWindow {
                id: wid.into(),
                tabs: vec![TabItem { id: "t".into(), view: "canvas".into(), locked: false }],
                active_tab_id: None,
                bounds: WindowBounds { x: 0.0, y: 0.0, width: 100.0, height: 100.0, scale: 0.0 },
            }],
            ..Default::default()
        }
    }

    /// 回归：write_bounds 仅当撕裂窗口模型 bounds 实际写回才置 dirty——
    /// 主窗口移动/缩放触发的事件不得造成无谓持久化（返回 false 即不置 dirty）。
    #[test]
    fn sync_detached_bounds_only_writes_for_panel_label() {
        let mut ui = ui_with_detached("w1");
        let b = WindowBounds { x: 10.0, y: 20.0, width: 300.0, height: 200.0, scale: 1.5 };
        // 主窗口 label → 不写模型、返回 false
        assert!(!sync_detached_bounds(&mut ui, "main", &b));
        // 撕裂窗口 label 命中条目 → 写回 + true
        assert!(sync_detached_bounds(&mut ui, "panel-w1", &b));
        assert_eq!(ui.detached_windows[0].bounds, b);
        // 撕裂窗口 label 但条目不存在（幽灵）→ false
        assert!(!sync_detached_bounds(&mut ui, "panel-ghost", &b));
    }
}
