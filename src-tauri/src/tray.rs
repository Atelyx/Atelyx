//! 系统托盘（桌面）：托盘图标/菜单、全窗口驻留显隐、完全退出协调。
//!
//! 驻留模型：UI 驻留标志（`UiHidden`）= 主窗口与撕裂窗口当前是否整体隐藏在托盘。
//! 撕裂窗口建窗时读该标志决定可见性——静默自启或已驻留期间的启动恢复调和建窗不弹出，
//! 显示动作由 `show_all_windows` 统一补。主窗口点 X = `hide_to_tray`（进程不退出）；
//! 完全退出唯一入口 = 托盘菜单「退出」：广播退出请求 → 各 WebView 落盘收尾后经
//! `exit_flush_done` 回报 → 收齐全部回报真正退出；看门狗超时强制退出兜底
//! （WebView 卡死或未完成启动时回报永远不会到齐）。

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use tauri::{AppHandle, Manager};

/// 托盘退出请求事件（广播给全部 WebView；各窗口收尾后经 exit_flush_done 回报）。
pub const TRAY_EXIT_EVENT: &str = "atelyx:tray-exit-requested";

/// 看门狗等待窗口：自广播退出请求起，超过该时长仍有窗口未回报即强制退出。
const EXIT_WATCHDOG: Duration = Duration::from_secs(8);

/// UI 驻留标志：true = 全部窗口隐藏在托盘（撕裂窗口建窗应不可见）。
pub struct UiHidden(AtomicBool);

impl UiHidden {
    pub fn new(hidden: bool) -> Self {
        Self(AtomicBool::new(hidden))
    }

    pub fn get(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    pub fn set(&self, hidden: bool) {
        self.0.store(hidden, Ordering::Relaxed);
    }
}

/// 完全退出协调：待回报窗口标签集合；None = 无退出进行中（或已收齐）。
#[derive(Default)]
pub struct ExitWait(Mutex<Option<HashSet<String>>>);

impl ExitWait {
    /// 登记待回报窗口集合；已有退出进行中时返回 false（调用方不再重复广播/起看门狗）。
    fn begin(&self, labels: HashSet<String>) -> bool {
        let mut guard = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_some() {
            return false;
        }
        *guard = Some(labels);
        true
    }

    /// 移除一个已回报窗口；返回是否全部回报完毕（调用方此时真正退出）。
    /// 集合清空即复位为 None，防看门狗把「已正常收齐」误判为超时。
    fn settle(&self, label: &str) -> bool {
        let mut guard = self.0.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_mut() {
            Some(pending) => {
                pending.remove(label);
                if pending.is_empty() {
                    *guard = None;
                    true
                } else {
                    false
                }
            }
            None => false,
        }
    }

    fn in_progress(&self) -> bool {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }
}

/// 显示全部窗口并聚焦主窗口（托盘打开 / 手动二次启动共用）；
/// 同时解除驻留标志，此后撕裂窗口照常可见建窗。
/// 先按驻留备份还原模型 hidden（唤起类隐藏窗口不随驻留补显），再照常跳过模型隐藏窗口。
pub fn show_all_windows(app: &AppHandle) {
    if let Some(flag) = app.try_state::<UiHidden>() {
        flag.set(false);
    }
    crate::layout::residence_set(app, false);
    let model_hidden: HashSet<String> = app
        .try_state::<crate::layout::LayoutState>()
        .map(|state| {
            state
                .inner
                .lock()
                .map(|inner| {
                    inner
                        .ui
                        .detached_windows
                        .iter()
                        .filter(|w| w.hidden)
                        .map(|w| format!("{}{}", crate::layout::PANEL_LABEL_PREFIX, w.id))
                        .collect()
                })
                .unwrap_or_default()
        })
        .unwrap_or_default();
    for (_, win) in app.webview_windows() {
        if model_hidden.contains(win.label()) {
            continue;
        }
        let _ = win.unminimize();
        let _ = win.show();
    }
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.set_focus();
    }
}

/// 隐藏全部窗口驻留托盘（主窗口关闭守卫收尾调用）。
/// 模型 hidden 随 OS 实况整体置真并广播（镜像不失真，插件的显隐判定在驻留期间有效）；
/// 驻留前的 hidden 备份在内存里，托盘恢复时还原。
fn hide_all_windows(app: &AppHandle) {
    crate::layout::residence_set(app, true);
    if let Some(flag) = app.try_state::<UiHidden>() {
        flag.set(true);
    }
    for (_, win) in app.webview_windows() {
        let _ = win.hide();
    }
}

/// 完全退出唯一入口：登记待回报窗口并广播退出请求，随后起看门狗。
/// 重复调用（退出进行中）为 no-op，不重置看门狗。
#[cfg(desktop)]
pub fn begin_exit(app: &AppHandle) {
    use tauri::Emitter;

    let labels: HashSet<String> = app.webview_windows().keys().cloned().collect();
    // 无窗口可收尾（全部已销毁）直接退出，不等看门狗
    if labels.is_empty() {
        app.exit(0);
        return;
    }
    let Some(wait) = app.try_state::<ExitWait>() else {
        return;
    };
    if !wait.begin(labels) {
        return;
    }
    let _ = app.emit(TRAY_EXIT_EVENT, ());
    let handle = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(EXIT_WATCHDOG);
        // 收齐回报时 settle 已把集合复位为 None；仍是 Some = 有窗口未回报，强制退出
        if handle.state::<ExitWait>().in_progress() {
            eprintln!("[tray] 退出收尾超时，强制退出");
            handle.exit(0);
        }
    });
}

/// 创建托盘（setup 调用一次）：左键点击显示全部窗口；右键菜单 打开 Atelyx / 退出。
#[cfg(desktop)]
pub fn create_tray(app: &AppHandle) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    let open_item = MenuItem::with_id(app, "open", "打开 Atelyx", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit_item = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open_item, &separator, &quit_item])?;

    TrayIconBuilder::with_id("atelyx-tray")
        .icon(app.default_window_icon().expect("应用图标未配置").clone())
        .tooltip("Atelyx")
        .menu(&menu)
        // 左键点击留给「显示窗口」，菜单只在右键弹出（Linux 托盘激活事件不一定派发，
        // 菜单始终可用作兜底入口）
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => show_all_windows(app),
            "quit" => begin_exit(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_all_windows(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

// ===== 命令 =====

/// 主窗口关闭守卫收尾：全部窗口隐藏驻留托盘，进程保持运行。
#[tauri::command]
pub fn hide_to_tray(app: AppHandle) {
    hide_all_windows(&app);
}

/// 窗口退出收尾回报：收齐全部窗口回报后真正退出进程。
/// 未在待回报集合中的窗口（重复回报或集合已清空）为 no-op。
#[tauri::command]
pub fn exit_flush_done(app: AppHandle, window: tauri::Window) {
    let Some(wait) = app.try_state::<ExitWait>() else {
        return;
    };
    if wait.settle(window.label()) {
        app.exit(0);
    }
}

// ===== 单元测试 =====

#[cfg(test)]
mod tests {
    use super::*;

    /// 回报登记簿：begin 后逐窗口 settle，收齐最后一个回报才判定完成并复位；
    /// 退出进行中重复 begin 被拒；settle 未知/重复标签不影响判定。
    #[test]
    fn exit_wait_settles_only_after_all_labels_reported() {
        let wait = ExitWait::default();
        // 未 begin：settle 一律 false
        assert!(!wait.settle("main"));
        // begin 两窗口：退出进行中，重复 begin 被拒
        assert!(wait.begin(HashSet::from(["main".into(), "panel-a".into()])));
        assert!(!wait.begin(HashSet::from(["main".into()])));
        // 部分回报未完成；重复回报同标签不推进
        assert!(!wait.settle("panel-a"));
        assert!(!wait.settle("panel-a"));
        assert!(!wait.settle("panel-ghost"));
        assert!(wait.in_progress());
        // 最后一个窗口回报：完成并复位（看门狗据此不误判超时）
        assert!(wait.settle("main"));
        assert!(!wait.in_progress());
    }
}
