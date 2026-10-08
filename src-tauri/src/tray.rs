//! 系统托盘（桌面）：托盘图标/菜单（内置项 + 插件贡献项）、全窗口驻留显隐（驻留模型见
//! `UiHidden`）、完全退出协调（协议见 `begin_exit` / `exit_flush_done`）。主窗口点 X =
//! 驻留托盘不退出；完全退出唯一入口 = 托盘菜单「退出」。

use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

#[cfg(desktop)]
use std::time::Duration;

use tauri::{AppHandle, Manager};

/// 托盘退出请求事件（广播给全部 WebView；各窗口收尾后经 exit_flush_done 回报）。
#[cfg(desktop)]
pub const TRAY_EXIT_EVENT: &str = "atelyx:tray-exit-requested";

/// 插件托盘菜单项点击事件（payload = 叶子 key，定向发到注册来源窗口）。
#[cfg(desktop)]
pub const TRAY_PLUGIN_MENU_EVENT: &str = "atelyx:tray-plugin-menu";

/// 看门狗等待窗口：自广播退出请求起，超过该时长仍有窗口未回报即强制退出
/// （WebView 卡死或未完成启动时回报永远不会到齐，强制退出兜底）。
#[cfg(desktop)]
const EXIT_WATCHDOG: Duration = Duration::from_secs(8);

/// UI 驻留标志：true = 全部窗口隐藏在托盘（撕裂窗口建窗应不可见）。
pub struct UiHidden(AtomicBool);

impl UiHidden {
    #[cfg(desktop)]
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
    #[cfg(desktop)]
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

    #[cfg(desktop)]
    fn in_progress(&self) -> bool {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }
}

// ===== 插件托盘菜单 =====

/// 单插件托盘菜单节点数上限（含子菜单与分隔线）：防病态清单撑爆托盘菜单。
#[cfg(desktop)]
const MAX_MENU_NODES: usize = 64;

/// 插件托盘菜单树最大深度（顶层 = 1）。
#[cfg(desktop)]
const MAX_MENU_DEPTH: usize = 3;

/// 插件贡献的托盘菜单树节点（serde 形状 = 前端 `ctx.tray.setMenu` 的条目）。
/// `item` 叶子的 key 由前端拼好（`{插件id}:{路径id}`），点击按 key 原样回传；
/// 校验只认以插件 id 为前缀的 key，跨插件伪造与残留点击都在这里拦下。
#[derive(serde::Deserialize, Clone)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum TrayMenuNode {
    Item { key: String, label: String },
    Submenu { label: String, items: Vec<TrayMenuNode> },
    Separator,
}

/// 按插件 id 归册的菜单树 + 注册来源窗口（点击事件定向发回该窗口，
/// 多窗口重复注册时后到者覆盖——同一时刻只有一个窗口的桥持有 handler）。
/// `rebuild_lock` 串行化菜单重建：并发 setMenu 时菜单终态与注册表一致（后写入者完整呈现）。
#[cfg(desktop)]
#[derive(Default)]
pub struct PluginMenus {
    entries: Mutex<BTreeMap<String, PluginMenuEntry>>,
    rebuild_lock: Mutex<()>,
}

#[cfg(desktop)]
struct PluginMenuEntry {
    nodes: Vec<TrayMenuNode>,
    window: String,
}

#[cfg(desktop)]
impl PluginMenus {
    fn set(&self, plugin_id: String, entry: PluginMenuEntry) {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(plugin_id, entry);
    }

    fn remove(&self, plugin_id: &str) {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(plugin_id);
    }

    /// 叶子 key 的注册来源窗口（点击事件定向回传；key 不在册 = None）。
    fn window_of_leaf(&self, key: &str) -> Option<String> {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .find(|e| nodes_contain_leaf(&e.nodes, key))
            .map(|e| e.window.clone())
    }
}

#[cfg(desktop)]
fn nodes_contain_leaf(nodes: &[TrayMenuNode], key: &str) -> bool {
    nodes.iter().any(|n| match n {
        TrayMenuNode::Item { key: k, .. } => k == key,
        TrayMenuNode::Submenu { items, .. } => nodes_contain_leaf(items, key),
        TrayMenuNode::Separator => false,
    })
}

/// 菜单树形状校验：节点数上限、深度上限、item 的 key 须以插件 id 为前缀且全树唯一、
/// label 非空。前端已做一层校验，这里是对抗性复核（命令入参不可信）。
#[cfg(desktop)]
fn validate_menu(plugin_id: &str, nodes: &[TrayMenuNode]) -> Result<(), String> {
    let mut keys = HashSet::new();
    validate_nodes(plugin_id, nodes, 1, &mut keys)
}

#[cfg(desktop)]
fn validate_nodes(
    plugin_id: &str,
    nodes: &[TrayMenuNode],
    depth: usize,
    keys: &mut HashSet<String>,
) -> Result<(), String> {
    if depth > MAX_MENU_DEPTH {
        return Err(format!("托盘菜单嵌套超过 {MAX_MENU_DEPTH} 层"));
    }
    if keys.len() + nodes.len() > MAX_MENU_NODES {
        return Err(format!("托盘菜单节点数超过 {MAX_MENU_NODES}"));
    }
    for node in nodes {
        match node {
            TrayMenuNode::Item { key, label } => {
                if !key.starts_with(&format!("{plugin_id}:")) {
                    return Err(format!("菜单项 key 必须以「{plugin_id}:」开头：{key}"));
                }
                if label.trim().is_empty() {
                    return Err(format!("菜单项文案不能为空：{key}"));
                }
                if !keys.insert(key.clone()) {
                    return Err(format!("菜单项 key 重复：{key}"));
                }
            }
            TrayMenuNode::Submenu { label, items } => {
                if label.trim().is_empty() {
                    return Err("子菜单文案不能为空".to_string());
                }
                validate_nodes(plugin_id, items, depth + 1, keys)?;
            }
            TrayMenuNode::Separator => {}
        }
    }
    Ok(())
}

/// 显示全部窗口并聚焦主窗口（托盘打开 / 手动二次启动共用）；
/// 同时解除驻留标志，此后撕裂窗口照常可见建窗。
/// 先按驻留备份还原模型 hidden（唤起类隐藏窗口不随驻留补显），再照常跳过模型隐藏窗口。
/// 桌面专属：调用方（托盘/单实例二次启动）均为桌面侧。
#[cfg(desktop)]
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
    // 预热备用窗口是模型外的隐藏窗口，显示名单里没有它：驻留补显不得把它带出来
    let prewarm = crate::layout::prewarm_window_label(app);
    for (_, win) in app.webview_windows() {
        if model_hidden.contains(win.label()) || prewarm.as_deref() == Some(win.label()) {
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

    let mut labels: HashSet<String> = app.webview_windows().keys().cloned().collect();
    // 预热备用窗口（模型外）不参与退出收尾：它没有内容可落盘，等它回报只会拖住退出
    if let Some(prewarm) = crate::layout::prewarm_window_label(app) {
        labels.remove(&prewarm);
    }
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

/// 创建托盘（setup 调用一次）：左键点击显示全部窗口；右键菜单 = 内置项 + 插件贡献项。
#[cfg(desktop)]
pub fn create_tray(app: &AppHandle) -> tauri::Result<()> {
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    TrayIconBuilder::with_id("atelyx-tray")
        .icon(app.default_window_icon().expect("应用图标未配置").clone())
        .tooltip("Atelyx")
        .menu(&build_tray_menu(app)?)
        // 左键点击留给「显示窗口」，菜单只在右键弹出（Linux 托盘激活事件不一定派发，
        // 菜单始终可用作兜底入口）
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => show_all_windows(app),
            "quit" => begin_exit(app),
            key => {
                // 插件菜单项：注册表白名单内的 key 定向回传注册窗口（伪造/残留点击忽略）
                if let Some(window) = app.state::<PluginMenus>().window_of_leaf(key) {
                    use tauri::Emitter;
                    let _ = app.emit_to(window, TRAY_PLUGIN_MENU_EVENT, key);
                }
            }
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

/// 重建托盘右键菜单：内置项（打开 Atelyx / 退出）+ 各插件菜单树平铺同层（插件之间加分隔线）。
/// 插件树可含子菜单与分隔线；无插件贡献时即内置菜单。
#[cfg(desktop)]
fn build_tray_menu(app: &AppHandle) -> tauri::Result<tauri::menu::Menu<tauri::Wry>> {
    use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};

    let open_item = MenuItem::with_id(app, "open", "打开 Atelyx", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::new(app)?;
    menu.append(&open_item)?;
    menu.append(&PredefinedMenuItem::separator(app)?)?;

    let registry = app.state::<PluginMenus>();
    let trees: Vec<Vec<TrayMenuNode>> = {
        let guard = registry.entries.lock().unwrap_or_else(|e| e.into_inner());
        guard.values().map(|e| e.nodes.clone()).collect()
    };
    for (index, nodes) in trees.iter().enumerate() {
        if index > 0 {
            menu.append(&PredefinedMenuItem::separator(app)?)?;
        }
        for node in nodes {
            append_menu_node(app, &menu, node)?;
        }
    }

    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&quit_item)?;
    Ok(menu)
}

/// 把插件菜单树节点挂进容器（Menu 与 Submenu 的 append 同一签名，经本 trait 统一分派）。
#[cfg(desktop)]
trait AppendMenuItem {
    fn append_item(&self, item: &dyn tauri::menu::IsMenuItem<tauri::Wry>) -> tauri::Result<()>;
}

#[cfg(desktop)]
impl AppendMenuItem for tauri::menu::Menu<tauri::Wry> {
    fn append_item(&self, item: &dyn tauri::menu::IsMenuItem<tauri::Wry>) -> tauri::Result<()> {
        self.append(item)
    }
}

#[cfg(desktop)]
impl AppendMenuItem for tauri::menu::Submenu<tauri::Wry> {
    fn append_item(&self, item: &dyn tauri::menu::IsMenuItem<tauri::Wry>) -> tauri::Result<()> {
        self.append(item)
    }
}

#[cfg(desktop)]
fn append_menu_node(
    app: &AppHandle,
    container: &dyn AppendMenuItem,
    node: &TrayMenuNode,
) -> tauri::Result<()> {
    match node {
        TrayMenuNode::Item { key, label } => {
            let item = tauri::menu::MenuItem::with_id(app, key, label, true, None::<&str>)?;
            container.append_item(&item)
        }
        TrayMenuNode::Submenu { label, items } => {
            // 子菜单 id 不参与点击事件，加前缀避免与叶子 key 空间混淆
            let sub = tauri::menu::Submenu::with_id(app, format!("__sub:{label}"), label, true)?;
            for item in items {
                append_menu_node(app, &sub, item)?;
            }
            container.append_item(&sub)
        }
        TrayMenuNode::Separator => {
            container.append_item(&tauri::menu::PredefinedMenuItem::separator(app)?)
        }
    }
}

// ===== 命令 =====

/// 设置/清除某插件的托盘菜单树（items = None 即清除）；每次变更整表重建菜单。
/// 注册时记录来源窗口（tauri 注入的 `window` 参数），点击事件只发回该窗口——
/// 多窗口重复注册时后到者覆盖，同一时刻仅一个窗口的桥持有 handler，点击不会双跑。
#[tauri::command]
pub fn tray_set_plugin_menu(
    app: AppHandle,
    window: tauri::Window,
    plugin_id: String,
    items: Option<Vec<TrayMenuNode>>,
) -> Result<(), String> {
    #[cfg(desktop)]
    {
        if let Some(nodes) = &items {
            validate_menu(&plugin_id, nodes)?;
        }
        match items {
            Some(nodes) => app.state::<PluginMenus>().set(
                plugin_id.clone(),
                PluginMenuEntry {
                    nodes,
                    window: window.label().to_string(),
                },
            ),
            None => app.state::<PluginMenus>().remove(&plugin_id),
        }
        rebuild_tray_menu(&app)
    }
    #[cfg(not(desktop))]
    {
        let _ = (app, window, plugin_id, items);
        Err("当前平台无托盘".to_string())
    }
}

#[cfg(desktop)]
fn rebuild_tray_menu(app: &AppHandle) -> Result<(), String> {
    use tauri::tray::TrayIcon;
    // 重建串行化：并发 setMenu 的 set_menu 互相覆盖会让菜单终态与注册表漂移
    let registry = app.state::<PluginMenus>();
    let _serial = registry.rebuild_lock.lock().unwrap_or_else(|e| e.into_inner());
    let tray: TrayIcon = app
        .tray_by_id("atelyx-tray")
        .ok_or_else(|| "托盘未初始化".to_string())?;
    let menu = build_tray_menu(app).map_err(|e| e.to_string())?;
    tray.set_menu(Some(menu)).map_err(|e| e.to_string())
}

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

    /// 合法树：item key 带插件前缀、子菜单嵌套、分隔线。
    fn item(key: &str, label: &str) -> TrayMenuNode {
        TrayMenuNode::Item { key: key.into(), label: label.into() }
    }

    #[test]
    fn validate_menu_accepts_wellformed_tree() {
        let tree = vec![
            item("com.a:run", "执行"),
            TrayMenuNode::Submenu {
                label: "更多".into(),
                items: vec![TrayMenuNode::Separator, item("com.a:more:sync", "同步")],
            },
            TrayMenuNode::Separator,
        ];
        assert!(validate_menu("com.a", &tree).is_ok());
    }

    #[test]
    fn validate_menu_rejects_foreign_key_prefix() {
        assert!(validate_menu("com.a", &[item("com.b:run", "执行")])
            .unwrap_err()
            .contains("com.b:"));
    }

    #[test]
    fn validate_menu_rejects_duplicate_and_blank() {
        let dup = vec![item("com.a:x", "一"), item("com.a:x", "二")];
        assert!(validate_menu("com.a", &dup).unwrap_err().contains("重复"));
        let blank = vec![item("com.a:x", "  ")];
        assert!(validate_menu("com.a", &blank).unwrap_err().contains("不能为空"));
    }

    #[test]
    fn validate_menu_rejects_depth_and_size_overflow() {
        let deep = TrayMenuNode::Submenu {
            label: "一".into(),
            items: vec![TrayMenuNode::Submenu {
                label: "二".into(),
                items: vec![TrayMenuNode::Submenu {
                    label: "三".into(),
                    items: vec![item("com.a:deep", "超深")],
                }],
            }],
        };
        assert!(validate_menu("com.a", &[deep]).unwrap_err().contains("层"));
        let flood: Vec<TrayMenuNode> = (0..MAX_MENU_NODES + 1)
            .map(|i| item(&format!("com.a:k{i}"), "x"))
            .collect();
        assert!(validate_menu("com.a", &flood).unwrap_err().contains("节点数"));
    }

    #[test]
    fn window_of_leaf_finds_nested_items_only() {
        let mut registry = PluginMenus::default();
        registry.set(
            "com.a".into(),
            PluginMenuEntry {
                nodes: vec![TrayMenuNode::Submenu {
                    label: "组".into(),
                    items: vec![item("com.a:go", "走")],
                }],
                window: "main".into(),
            },
        );
        assert_eq!(registry.window_of_leaf("com.a:go").as_deref(), Some("main"));
        assert_eq!(registry.window_of_leaf("com.a:missing"), None);
        // 清除后叶 key 不再命中（残留点击不回传）
        registry.remove("com.a");
        assert_eq!(registry.window_of_leaf("com.a:go"), None);
    }
}
