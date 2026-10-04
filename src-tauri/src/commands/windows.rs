//! 撕裂面板窗口管理（多窗口面板体系）。
//!
//! 窗口创建/回收由布局迷你窗口管理器持有权威：撕裂建新窗与启动恢复经
//! `layout_window::reconcile_panel_windows` 调本模块内部函数；前端不再直接建窗。
//! url 同主入口，前端按 label 分流渲染单面板。

#[cfg(desktop)]
use tauri::{window::Color, WebviewUrl, WebviewWindowBuilder};
use tauri::{AppHandle, Manager};

#[cfg(desktop)]
use crate::layout::window_event_handler;
use crate::layout::{PANEL_LABEL_PREFIX, WindowBounds};

/// 撕裂窗口加载地址：统一 `WebviewUrl::App("index.html")`（受信任协议，与主窗口同机制）。
#[cfg(desktop)]
fn panel_url() -> WebviewUrl {
    WebviewUrl::App("index.html".into())
}

/// 撕裂窗口的启动底色（深色主题 `--bg-primary`）：窗口创建先于页面渲染，与主窗口
/// `tauri.conf.json > backgroundColor` 同值才不闪异色（一致性由 lib.rs 契约测试把守）。
#[cfg(desktop)]
const STARTUP_BG: Color = Color(19, 20, 24, 255);

/// 创建撕裂面板窗口（label = `panel-<id>`；内部函数，供布局迷你窗口管理器
/// 撕裂建新窗/恢复调和调用）。已存在（恢复防重）时直接返回 true。
/// 同步命令/事件处理器里调用会死锁（wry#583），故本函数只在 async 上下文
/// （拖拽落点/恢复调和命令）调用——build() 在 async runtime 线程执行，经投递
/// 主事件循环在正常派发点创建窗口。
/// 撕裂窗口是桌面能力：移动端单窗口无调用点，此处仅保留可编译实现。
#[cfg_attr(not(desktop), allow(unused_variables))]
pub(crate) fn create_panel_window_internal(
    app: &AppHandle,
    label: &str,
    title: &str,
    bounds: &WindowBounds,
) -> bool {
    if app.get_webview_window(label).is_some() {
        return true;
    }
    #[cfg(desktop)]
    {
        let label_owned = label.to_string();
        let builder = WebviewWindowBuilder::new(app, label, panel_url())
            .title(title.to_string())
            .inner_size(bounds.width, bounds.height)
            .position(bounds.x, bounds.y)
            // 与主窗口一致的自定义标题栏（decorations: false + 前端 TitleBarControls）
            .decorations(false)
            .resizable(true)
            .min_inner_size(320.0, 240.0)
            // 启动背景色 = 主窗口 tauri.conf.json 的 backgroundColor，防新建窗口白闪
            .background_color(STARTUP_BG);
        // UI 驻留托盘（静默自启/主窗口已驻留）期间建窗不可见，显示由 show_all_windows
        // 统一补；标志未托管（不可能在驻留语义外建窗的极端时序）按可见处理
        let hidden = app
            .try_state::<crate::tray::UiHidden>()
            .map(|flag| flag.get())
            .unwrap_or(false);
        let builder = builder.visible(!hidden);
        let win = builder.build();
        match win {
            Ok(win) => {
                // 窗口事件钩子：Moved/Resized → 布局迷你窗口管理器权威 bounds（拖拽命中/落点解析）
                win.on_window_event(window_event_handler(app, label_owned.clone()));
                // 种子化初始 bounds：新窗未触发 Moved/Resized 前拖拽解析读不到（主窗口同，见 setup）
                crate::layout::seed_window_bounds(app, &label_owned);
                // 驻留托盘期间建出的隐藏窗口不抢焦点
                if !hidden {
                    let _ = win.set_focus();
                }
            }
            Err(e) => {
                // if cfg! 而非 #[cfg]：e 语法上被引用，release 不触发 unused_variables 告警（恒假分支被消除）
                if cfg!(debug_assertions) {
                    eprintln!("[panel:{label}] build 失败: {e}");
                }
            }
        }
    }
    true
}

/// 关闭撕裂面板窗口（按 id；窗口不存在时静默跳过）。由布局迷你窗口管理器在
/// 条目被移除（拖空/关空）时调用；关闭触发 JS onCloseRequested（flush 托管视图后销毁）。
pub(crate) fn close_panel_window_internal(app: &AppHandle, window_id: &str) {
    let label_full = format!("{PANEL_LABEL_PREFIX}{window_id}");
    if let Some(win) = app.get_webview_window(&label_full) {
        let _ = win.close();
    }
}

/// 鼠标左键当前是否按下（跨窗口拖拽释放检测）。
///
/// 标签拖出窗口后，webview 可能收不到窗口外的 pointerup（窗口外指针事件不可靠），
/// 拖拽会话无法结束、drop 指示器残留——前端在拖拽活跃期间轮询本命令，物理检测左键松开即终止会话；
/// Rust 侧拖拽看门狗收尾前也调用它（`layout_drag`）避免把「按住暂停」当成释放。
/// 仅 Windows 支持（GetAsyncKeyState），其他平台返回 None = 无探测能力，
/// 调用方按「已空闲够久即收尾」处理（拿不到按键状态，只能按计时判定）。
#[tauri::command]
pub fn is_mouse_left_down() -> Option<bool> {
    #[cfg(target_os = "windows")]
    {
        // VK_LBUTTON = 0x01；返回 SHORT 最高位 = 按键处于按下状态（负值即按下）
        let state = unsafe { winapi::um::winuser::GetAsyncKeyState(0x01) };
        return Some(state < 0);
    }
    #[cfg(not(target_os = "windows"))]
    {
        None
    }
}
