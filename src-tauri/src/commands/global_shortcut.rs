//! 全局快捷键的命令面：`ctx.shortcuts` 的 OS 层后端（桌面专属）。
//! 注册收进 Rust 侧统一登记（应用级资源：OS 热键进程内唯一，不应随注册窗口销毁失效——
//! 插件在每个窗口都有独立内核实例，直接经插件 JS 通道注册会让归属取决于加载时序）。
//! 信任模型与 `ctx.shell` 一致：不构成防插件边界，占用/释放只在插件之间做归属仲裁；格式合法性与按键语义由 OS 层校验。

// 登记表条目与其消费方均为桌面专属，导入同步门控（移动端两表恒空）
#[cfg(desktop)]
use std::collections::HashMap;
#[cfg(desktop)]
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};
#[cfg(desktop)]
use tauri::{Emitter, Manager};

/// 触发事件名（Rust → 主窗口；载荷 [ShortcutTriggerPayload]）。
#[cfg(desktop)]
const TRIGGERED_EVENT: &str = "plugin-shortcut-triggered";

/// 窗口直控目标（快捷键触发时 Rust 直接切换的撕裂窗口）。
///
/// 不做平台门控：命令签名双端一致（移动端载荷原样收下、忽略），门控会让该参数在
/// 移动端从签名消失、安卓目标因缺类型编译失败。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WindowToggleSpec {
    pub view: String,
    pub options: crate::layout_model::WindowOptions,
}

/// 触发事件载荷（字段 camelCase 与前端对齐）。
#[cfg(desktop)]
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ShortcutTriggerPayload {
    accelerator: String,
    plugin_id: String,
    /// 窗口直控目标（Some = 触发由 Rust 直接切换承载 `view` 的撕裂窗口，不经主窗口 JS——
    /// 主窗口驻留托盘时热键照常生效；None = 转发主窗口由插件回调处理）。
    #[serde(skip_serializing_if = "Option::is_none")]
    window_toggle: Option<WindowToggleSpec>,
}

/// 登记表：归属表（快捷键 → 插件 id）与触发回查表（OS 快捷键 id → 归属）。
/// 两表同锁同更新；移动端无 OS 层，两表恒空（注册恒拒，注销/释放幂等成功）。
#[derive(Default)]
pub struct GlobalShortcutState {
    #[cfg(desktop)]
    inner: Mutex<ShortcutOwners>,
}

#[cfg(desktop)]
#[derive(Default)]
struct ShortcutOwners {
    /// 快捷键原始注册串 → 插件 id（占用仲裁与随插件整体释放）。
    by_accelerator: HashMap<String, String>,
    /// OS 快捷键 id → 触发转发归属（回查用：解析后的 Display 形态会改写修饰键顺序，
    /// 必须把注册时的原始串原样带回前端）。
    by_id: HashMap<u32, ShortcutTriggerPayload>,
}

/// 注册全局快捷键（归属插件 id 仅用于占用仲裁与随插件整体释放）。
///
/// 已被**其他**插件占用 → 拒绝；同插件重复注册 → 幂等成功（插件重跑 apply、多窗口内核
/// 各发一次注册是常态）；快捷键格式非法或 OS 层注册失败（被系统/其他应用占用）→ 可读错误。
#[tauri::command(async)]
pub fn plugin_shortcut_register(
    app: AppHandle,
    state: State<'_, GlobalShortcutState>,
    accelerator: String,
    plugin_id: String,
    window_toggle: Option<WindowToggleSpec>,
) -> Result<(), String> {
    #[cfg(desktop)]
    {
        desktop_register(app, &state, &accelerator, &plugin_id, window_toggle)
    }
    #[cfg(not(desktop))]
    {
        let _ = (&app, &state, &accelerator, &plugin_id, &window_toggle);
        Err("当前平台不支持全局快捷键".into())
    }
}

/// 注销单个全局快捷键（仅归属插件可注销；未注册 = 幂等成功）。
#[tauri::command(async)]
pub fn plugin_shortcut_unregister(
    app: AppHandle,
    state: State<'_, GlobalShortcutState>,
    accelerator: String,
    plugin_id: String,
) -> Result<(), String> {
    #[cfg(desktop)]
    {
        desktop_unregister(app, &state, &accelerator, &plugin_id)
    }
    #[cfg(not(desktop))]
    {
        let _ = (&app, &state, &accelerator, &plugin_id);
        // 移动端无 OS 层，不存在任何登记可注销：幂等成功。返回错误会让宿主每次加载/停用
        // 插件的清理收尾（本地登记为空也照常调用释放，跨窗口口径）逐插件误报失败。
        Ok(())
    }
}

/// 按插件整体注销（插件停用/卸载的宿主收口；未持有任何快捷键 = 幂等成功）。
#[tauri::command(async)]
pub fn plugin_shortcut_release_plugin(
    app: AppHandle,
    state: State<'_, GlobalShortcutState>,
    plugin_id: String,
) -> Result<(), String> {
    #[cfg(desktop)]
    {
        desktop_release_plugin(app, &state, &plugin_id)
    }
    #[cfg(not(desktop))]
    {
        let _ = (&app, &state, &plugin_id);
        // 移动端无 OS 层，无登记可释放：幂等成功（口径同注销）。
        Ok(())
    }
}

/// 单条全局快捷键登记信息（设置页展示用：归属插件 + 是否窗口直控）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShortcutRegistrationInfo {
    pub accelerator: String,
    pub plugin_id: String,
    /// true = 窗口切换热键（触发由 Rust 直控窗口显隐，无 JS 回调）。
    pub window_toggle: bool,
}

/// 列出当前登记的全部全局快捷键（OS 层登记表是应用级真源，跨窗口一致；移动端恒空）。
#[tauri::command(async)]
pub fn plugin_shortcut_list(state: State<'_, GlobalShortcutState>) -> Result<Vec<ShortcutRegistrationInfo>, String> {
    #[cfg(desktop)]
    {
        let owners = state
            .inner
            .lock()
            .map_err(|_| "快捷键登记表锁定失败".to_string())?;
        Ok(owners
            .by_id
            .values()
            .map(|payload| ShortcutRegistrationInfo {
                accelerator: payload.accelerator.clone(),
                plugin_id: payload.plugin_id.clone(),
                window_toggle: payload.window_toggle.is_some(),
            })
            .collect())
    }
    #[cfg(not(desktop))]
    {
        let _ = &state;
        Ok(Vec::new())
    }
}

#[cfg(desktop)]
fn desktop_register(
    app: AppHandle<tauri::Wry>,
    state: &State<'_, GlobalShortcutState>,
    accelerator: &str,
    plugin_id: &str,
    window_toggle: Option<WindowToggleSpec>,
) -> Result<(), String> {
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

    let parsed: Shortcut = accelerator
        .parse()
        .map_err(|e| format!("快捷键格式无效（{accelerator}）：{e}"))?;
    // OS 快捷键 id 与修饰键书写顺序无关（"Shift+Alt+E" 与 "Alt+Shift+E" 同 id）：
    // 占用仲裁必须按 id 而非原始串，否则不同书写的二次注册会覆写并回滚掉既有登记
    let id = parsed.id();

    let mut owners = state
        .inner
        .lock()
        .map_err(|_| "快捷键登记表锁定失败".to_string())?;
    if let Some(existing) = owners.by_id.get(&id) {
        if existing.plugin_id == plugin_id {
            // 同插件幂等：直控目标变更（设置切换后重注册）原地更新载荷，OS 注册不动；
            // accelerator 保留首次注册串——OS 快捷键 id 与修饰键书写顺序无关，重注册串
            // 可能只是同 id 的另一种书写，改写会让 by_accelerator（仅首注册写入）与
            // by_id 载荷分叉，release 清理时漏掉旧串残留
            if existing.window_toggle != window_toggle {
                let updated = ShortcutTriggerPayload {
                    accelerator: existing.accelerator.clone(),
                    plugin_id: plugin_id.to_string(),
                    window_toggle: window_toggle.clone(),
                };
                owners.by_id.insert(id, updated);
            }
            return Ok(());
        }
        return Err(format!(
            "快捷键 {accelerator} 已被插件 {} 注册",
            existing.plugin_id
        ));
    }

    // 登记先行、注册失败即回滚：OS 注册成功到登记完成之间按键不丢触发
    let owner = ShortcutTriggerPayload {
        accelerator: accelerator.to_string(),
        plugin_id: plugin_id.to_string(),
        window_toggle: window_toggle.clone(),
    };
    owners.by_accelerator.insert(accelerator.to_string(), plugin_id.to_string());
    owners.by_id.insert(id, owner.clone());

    let result = app.global_shortcut().on_shortcut(accelerator, move |app, shortcut, event| {
        if event.state != ShortcutState::Pressed {
            return;
        }
        // 按触发时的快捷键 id 查回原始注册串与归属（见 by_id 注释）
        let owner = app
            .state::<GlobalShortcutState>()
            .inner
            .lock()
            .ok()
            .and_then(|owners| owners.by_id.get(&shortcut.id()).cloned());
        let Some(owner) = owner else { return };
        match owner.window_toggle.clone() {
            // 窗口直控：Rust 侧直接按视图切换撕裂窗口（建窗须经 async runtime，见
            // create_panel_window_internal 的 wry#583 注记），不经主窗口 JS——
            // 主窗口驻留托盘时热键照常生效
            Some(spec) => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    let _ = crate::layout::toggle_window_by_view(&app, &spec.view, spec.options);
                });
            }
            // 转发目标固定主窗口：其内核持有全部已激活插件的回调，与注册窗口的生灭无关；
            // 主窗口不存在（应用退出中）则事件自然丢弃
            None => {
                let _ = app.emit_to("main", TRIGGERED_EVENT, owner);
            }
        }
    });
    if let Err(e) = result {
        owners.by_accelerator.remove(accelerator);
        owners.by_id.remove(&id);
        return Err(format!("全局快捷键注册失败（{accelerator}）：{e}"));
    }
    Ok(())
}

#[cfg(desktop)]
fn desktop_unregister(
    app: AppHandle<tauri::Wry>,
    state: &State<'_, GlobalShortcutState>,
    accelerator: &str,
    plugin_id: &str,
) -> Result<(), String> {
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};

    let mut owners = state
        .inner
        .lock()
        .map_err(|_| "快捷键登记表锁定失败".to_string())?;
    match owners.by_accelerator.get(accelerator) {
        Some(owner) if owner != plugin_id => {
            return Err(format!("快捷键 {accelerator} 由插件 {owner} 注册，无权注销"));
        }
        Some(_) => {}
        None => return Ok(()),
    }
    // 先出册再注销 OS 层：注销期间到达的触发查不到回查表，自然丢弃（快捷键已在注销中）；
    // OS 注销失败则把登记原样放回（归属与 OS 注册保持一致，不留「登记已丢、OS 仍注册」的死键）
    let restored = parsed_id_of(accelerator).and_then(|id| owners.by_id.get(&id).cloned());
    let parsed = accelerator.parse::<Shortcut>();
    if let Ok(parsed) = parsed {
        owners.by_id.remove(&parsed.id());
    }
    owners.by_accelerator.remove(accelerator);
    drop(owners);
    if let Err(e) = app.global_shortcut().unregister(accelerator) {
        if let Ok(mut owners) = state.inner.lock() {
            owners.by_accelerator.insert(accelerator.to_string(), plugin_id.to_string());
            if let Ok(parsed) = accelerator.parse::<Shortcut>() {
                owners.by_id.insert(
                    parsed.id(),
                    restored.unwrap_or_else(|| ShortcutTriggerPayload {
                        accelerator: accelerator.to_string(),
                        plugin_id: plugin_id.to_string(),
                        window_toggle: None,
                    }),
                );
            }
        }
        return Err(format!("全局快捷键注销失败（{accelerator}）：{e}"));
    }
    Ok(())
}

/// 快捷键原始串 → OS id（解析失败 = None；调用方按无 id 处理）。
#[cfg(desktop)]
fn parsed_id_of(accelerator: &str) -> Option<u32> {
    accelerator.parse::<tauri_plugin_global_shortcut::Shortcut>().ok().map(|s| s.id())
}

#[cfg(desktop)]
fn desktop_release_plugin(
    app: AppHandle<tauri::Wry>,
    state: &State<'_, GlobalShortcutState>,
    plugin_id: &str,
) -> Result<(), String> {
    use tauri_plugin_global_shortcut::GlobalShortcutExt;

    // 以 by_id（含完整载荷）为唯一事实来源收集本插件条目，两表同轮清理
    let owned: Vec<(u32, ShortcutTriggerPayload)> = {
        let mut owners = state
            .inner
            .lock()
            .map_err(|_| "快捷键登记表锁定失败".to_string())?;
        let owned: Vec<(u32, ShortcutTriggerPayload)> = owners
            .by_id
            .iter()
            .filter(|(_, payload)| payload.plugin_id == plugin_id)
            .map(|(id, payload)| (*id, payload.clone()))
            .collect();
        for (id, payload) in &owned {
            owners.by_id.remove(id);
            owners.by_accelerator.remove(&payload.accelerator);
        }
        owned
    };
    if owned.is_empty() {
        return Ok(());
    }
    let global = app.global_shortcut();
    let mut failures: Vec<String> = Vec::new();
    for (id, payload) in &owned {
        if let Err(e) = global.unregister(payload.accelerator.as_str()) {
            // OS 注销失败：登记放回（同 desktop_unregister，不留归属与 OS 注册不一致的死键）
            if let Ok(mut owners) = state.inner.lock() {
                owners.by_accelerator.insert(payload.accelerator.clone(), plugin_id.to_string());
                owners.by_id.insert(*id, payload.clone());
            }
            failures.push(format!("{}：{}", payload.accelerator, e));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!("部分快捷键注销失败：{}", failures.join("；")))
    }
}
