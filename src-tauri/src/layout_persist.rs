//! 布局迷你窗口管理器的持久化与广播层：`ui-state.json` 单一写者（防双写竞争）、
//! 防抖落盘世代合并、`layout-broadcast` 全量广播。事件名常量集中于此，
//! 拖拽广播（`drag-session`）也复用本模块的事件名，避免事件名散落各模块。

use std::path::PathBuf;
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager};

use crate::layout::LayoutState;
use crate::layout_model::{normalize, AppUiState, UI_STATE_SCHEMA};

/// 布局状态全量广播事件名（各窗口渲染自身切片）。
pub(crate) const LAYOUT_BROADCAST_EVENT: &str = "layout-broadcast";
/// 拖拽会话广播事件名（ghost + 各窗口命中计算驱动）。
pub(crate) const DRAG_SESSION_EVENT: &str = "drag-session";

fn ui_state_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("ui-state.json"))
}

/// 从磁盘加载并归一化布局状态（仅 setup 时调用一次）。
pub fn load_from_disk(app: &AppHandle, state: &LayoutState) {
    let path = match ui_state_path(app) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[layout] ui-state 路径解析失败: {e}");
            return;
        }
    };
    let mut ui = match path.exists() {
        false => AppUiState::default(),
        true => match std::fs::read_to_string(&path).ok().and_then(|json| serde_json::from_str::<AppUiState>(&json).ok()) {
            Some(s) => s,
            None => AppUiState::default(),
        },
    };
    // schema 校验：不符即拒绝（同 .atlx 私有格式保护，防外部工具/手改误写）
    if ui.schema != UI_STATE_SCHEMA {
        ui = AppUiState::default();
    }
    normalize(&mut ui);
    let Ok(mut inner) = state.inner.lock() else {
        eprintln!("[layout] 布局状态锁已损坏，放弃加载（保持默认布局）");
        return;
    };
    inner.ui = ui;
    inner.loaded = true;
    inner.dirty = false;
}

/// 调度防抖落盘：世代号合并——仅最新一代真正写盘（连续操作只落一次）。
pub(crate) fn schedule_persist(app: &AppHandle, state: &LayoutState) {
    let gen = {
        let Ok(mut inner) = state.inner.lock() else {
            eprintln!("[layout] 布局状态锁已损坏，放弃落盘调度");
            return;
        };
        inner.persist_gen += 1;
        inner.persist_gen
    };
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        persist_after_gen(&app, gen);
    });
}

/// 世代检查后落盘（仅最新一代执行；dirty 清除）。
fn persist_after_gen(app: &AppHandle, gen: u64) {
    let state = app.state::<LayoutState>();
    let Ok(mut inner) = state.inner.lock() else {
        eprintln!("[layout] 布局状态锁已损坏，放弃本次落盘");
        return;
    };
    if inner.persist_gen != gen || !inner.dirty {
        return;
    }
    inner.dirty = false;
    let ui = inner.ui.clone();
    drop(inner);
    persist_write(app, &ui);
}

/// 无条件立即落盘（flush 命令）；锁损坏时返回 Err —— 退出/切仓库前的 flush 不能把「没写」谎报成成功。
pub(crate) fn persist_now(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<LayoutState>();
    let mut inner = state.inner.lock().map_err(|e| e.to_string())?;
    inner.dirty = false;
    let ui = inner.ui.clone();
    drop(inner);
    persist_write(app, &ui);
    Ok(())
}

fn persist_write(app: &AppHandle, ui: &AppUiState) {
    let path = match ui_state_path(app) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[layout] ui-state 路径解析失败: {e}");
            return;
        }
    };
    let json = match serde_json::to_string_pretty(ui) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("[layout] ui-state 序列化失败: {e}");
            return;
        }
    };
    if let Err(e) = crate::vault::atomic_write(&path, &json) {
        eprintln!("[layout] ui-state 写盘失败: {e}");
    }
}

/// 广播完整布局状态（各窗口据此渲染自身切片；模型小，全量广播简单可靠）。
pub(crate) fn broadcast_layout(app: &AppHandle, ui: &AppUiState) {
    let _ = app.emit(LAYOUT_BROADCAST_EVENT, ui);
}
