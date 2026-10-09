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

/// 撕裂窗口的启动底色（主题 `--bg-primary` 的浅/深基底）：窗口创建先于页面渲染，
/// 与 `tauri.conf.json > backgroundColor`、`index.html` 首帧底色取同一组值才不闪异色
/// （一致性由 lib.rs 契约测试把守）。
#[cfg(desktop)]
const DARK_BG: Color = Color(19, 20, 24, 255);
#[cfg(desktop)]
const LIGHT_BG: Color = Color(234, 233, 227, 255);

/// 当前生效主题的原生启动底色（主窗口 setup 与撕裂窗口建窗共用）。
#[cfg(desktop)]
pub(crate) fn startup_background(app: &AppHandle) -> Color {
    match crate::commands::global::startup_theme_scheme(app, system_dark(app)) {
        crate::commands::global::ThemeScheme::Light => LIGHT_BG,
        crate::commands::global::ThemeScheme::Dark => DARK_BG,
    }
}

/// 系统是否深色：窗口未显式设主题时 `theme()` 即系统设置，与页面 `prefers-color-scheme` 同源。
/// 取不到窗口（极端时序）按深色——两套基底里深色是缺省。
#[cfg(desktop)]
fn system_dark(app: &AppHandle) -> bool {
    app.get_webview_window("main")
        .and_then(|win| win.theme().ok())
        .map(|theme| theme == tauri::Theme::Dark)
        .unwrap_or(true)
}

/// 创建撕裂面板窗口（label = `panel-<id>`；内部函数，供布局迷你窗口管理器
/// 撕裂建新窗/恢复调和/直撕与显窗动作调用）。已存在（恢复防重）时直接返回 true。
/// `visible` 为模型侧期望可见性，与 UI 驻留标志（托盘）取交集——驻留期间建窗恒隐藏，
/// 显示由 show_all_windows 统一补。
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
    visible: bool,
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
            // 启动背景色 = 当前主题底色（与主窗口同源），防新建窗口白闪/异色闪
            .background_color(startup_background(app));
        // UI 驻留托盘（静默自启/主窗口已驻留）期间建窗不可见，显示由 show_all_windows
        // 统一补；标志未托管（不可能在驻留语义外建窗的极端时序）按可见处理
        let hidden = app
            .try_state::<crate::tray::UiHidden>()
            .map(|flag| flag.get())
            .unwrap_or(false);
        let builder = builder.visible(!hidden && visible);
        let win = builder.build();
        match win {
            Ok(win) => {
                // 按条目声明的窗口选项应用 OS 属性（置顶 / 不进任务栏；失焦收起与
                // 关闭即藏由窗口事件层按模型 options 判定）
                apply_window_os_options(app, &label);
                // 窗口事件钩子：Moved/Resized → 布局迷你窗口管理器权威 bounds（拖拽命中/落点解析）
                win.on_window_event(window_event_handler(app, label_owned.clone()));
                // 种子化初始 bounds：新窗未触发 Moved/Resized 前拖拽解析读不到（主窗口同，见 setup）
                crate::layout::seed_window_bounds(app, &label_owned);
                // 驻留托盘期间建出的隐藏窗口不抢焦点
                if !hidden && visible {
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

/// 按模型条目的窗口选项重应用 OS 属性（置顶 / 不进任务栏）到已建 OS 窗口。
/// 建窗路径（create_panel_window_internal）与选项收敛路径（toggle 后的既有窗口）共用；
/// 窗口不存在 = no-op。失焦收起与关闭即藏由窗口事件层实时读模型，不经此处。
pub(crate) fn apply_window_os_options(app: &AppHandle, label: &str) {
    let options = app
        .state::<crate::layout::LayoutState>()
        .inner
        .lock()
        .ok()
        .and_then(|inner| {
            let id = label.strip_prefix(PANEL_LABEL_PREFIX)?;
            inner
                .ui
                .detached_windows
                .iter()
                .find(|w| w.id == id)
                .map(|w| w.options)
        });
    if let Some(options) = options {
        #[cfg(desktop)]
        if let Some(win) = app.get_webview_window(label) {
            let _ = win.set_always_on_top(options.always_on_top);
            let _ = win.set_skip_taskbar(options.skip_taskbar);
        }
        #[cfg(not(desktop))]
        let _ = options;
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

// ===== 屏幕与显示器几何 =====

/// 显示器几何信息（物理像素 + 缩放；多屏虚拟桌面坐标系，原点 = 主显示器左上角）。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorInfo {
    /// 会话内稳定的序号 id（显示器热插拔后重排，不跨会话持久）。
    id: String,
    /// 系统显示器名（拿不到时为空串）。
    name: String,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    /// 工作区（扣除任务栏等系统保留区域），同一坐标系。
    work_x: i32,
    work_y: i32,
    work_width: u32,
    work_height: u32,
    scale_factor: f64,
}

/// 列出全部显示器的几何（插件多窗口跨屏定位的查询面）；移动端单屏语义返回空列表。
#[tauri::command]
pub fn list_monitors(app: AppHandle) -> Vec<MonitorInfo> {
    monitors_impl(app)
}

#[cfg(desktop)]
fn monitors_impl(app: AppHandle) -> Vec<MonitorInfo> {
    let monitors = app.available_monitors().unwrap_or_default();
    monitors
        .iter()
        .enumerate()
        .map(|(i, m)| MonitorInfo {
            id: format!("monitor-{i}"),
            name: m.name().map(String::as_str).unwrap_or_default().to_string(),
            x: m.position().x,
            y: m.position().y,
            width: m.size().width,
            height: m.size().height,
            work_x: m.work_area().position.x,
            work_y: m.work_area().position.y,
            work_width: m.work_area().size.width,
            work_height: m.work_area().size.height,
            scale_factor: m.scale_factor(),
        })
        .collect()
}

#[cfg(not(desktop))]
fn monitors_impl(_app: AppHandle) -> Vec<MonitorInfo> {
    Vec::new()
}
