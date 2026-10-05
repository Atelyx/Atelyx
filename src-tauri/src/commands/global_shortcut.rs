//! 全局快捷键的命令面：`ctx.shortcuts` 的 OS 层后端（桌面专属）。
//!
//! 注册属**应用级资源**：OS 热键在进程内唯一，且不应随注册窗口销毁失效——插件在每个窗口
//! 都有独立内核实例，若直接经插件 JS 通道注册，「哪个窗口注册成功」取决于加载时序，注册
//! 窗口关闭还会把快捷键一起带走。故收进 Rust 侧统一登记：
//! - 登记 = 快捷键 → 插件 id 的归属表 + OS 层注册（同一快捷键已被其他插件占用即拒绝，
//!   同插件重复注册幂等成功）；
//! - 触发 = Rust 处理器统一接事件，按快捷键 id 查回**原始注册串**后转发主窗口
//!   （`plugin-shortcut-triggered`）——主窗口内核持有全部已激活插件的回调，转发目标
//!   与注册窗口的生灭无关；前端按原始串分发，不经归一化（解析后的 Display 形态会改写
//!   修饰键顺序）；
//! - 释放 = 按插件 id 整体注销（插件停用/卸载由宿主调用，见 services/cordis/pluginShortcuts.ts
//!   的登记表与 `stores/pluginStore.ts` 的停用收口）。
//!
//! 信任模型与 `ctx.shell` 一致：不构成防插件边界（占用/释放只在插件之间做归属仲裁），
//! 快捷键格式合法性与按键语义由 OS 层校验，非法格式以可读错误拒绝。

use std::collections::HashMap;
use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

/// 触发事件名（Rust → 主窗口；载荷 [ShortcutTriggerPayload]）。
const TRIGGERED_EVENT: &str = "plugin-shortcut-triggered";

/// 触发事件载荷（字段 camelCase 与前端对齐）。
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ShortcutTriggerPayload {
    accelerator: String,
    plugin_id: String,
}

/// 登记表：归属表（快捷键 → 插件 id）与触发回查表（OS 快捷键 id → 归属）。
/// 两表同锁同更新；移动端无 OS 层，两表恒空、命令恒拒。
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
) -> Result<(), String> {
    #[cfg(desktop)]
    {
        desktop_register(app, &state, &accelerator, &plugin_id)
    }
    #[cfg(not(desktop))]
    {
        let _ = (&app, &state, &accelerator, &plugin_id);
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
        Err("当前平台不支持全局快捷键".into())
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
        Err("当前平台不支持全局快捷键".into())
    }
}

#[cfg(desktop)]
fn desktop_register(
    app: AppHandle<tauri::Wry>,
    state: &State<'_, GlobalShortcutState>,
    accelerator: &str,
    plugin_id: &str,
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
        if let Some(owner) = owner {
            // 转发目标固定主窗口：其内核持有全部已激活插件的回调，与注册窗口的生灭无关；
            // 主窗口不存在（应用退出中）则事件自然丢弃
            let _ = app.emit_to("main", TRIGGERED_EVENT, owner);
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
    if let Ok(parsed) = accelerator.parse::<Shortcut>() {
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
                    ShortcutTriggerPayload {
                        accelerator: accelerator.to_string(),
                        plugin_id: plugin_id.to_string(),
                    },
                );
            }
        }
        return Err(format!("全局快捷键注销失败（{accelerator}）：{e}"));
    }
    Ok(())
}

#[cfg(desktop)]
fn desktop_release_plugin(
    app: AppHandle<tauri::Wry>,
    state: &State<'_, GlobalShortcutState>,
    plugin_id: &str,
) -> Result<(), String> {
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};

    let accelerators: Vec<String> = {
        let mut owners = state
            .inner
            .lock()
            .map_err(|_| "快捷键登记表锁定失败".to_string())?;
        let owned: Vec<String> = owners
            .by_accelerator
            .iter()
            .filter(|(_, owner)| owner.as_str() == plugin_id)
            .map(|(accelerator, _)| accelerator.clone())
            .collect();
        for accelerator in &owned {
            if let Ok(parsed) = accelerator.parse::<Shortcut>() {
                owners.by_id.remove(&parsed.id());
            }
            owners.by_accelerator.remove(accelerator);
        }
        owned
    };
    if accelerators.is_empty() {
        return Ok(());
    }
    let global = app.global_shortcut();
    let mut failures: Vec<String> = Vec::new();
    for accelerator in &accelerators {
        if let Err(e) = global.unregister(accelerator.as_str()) {
            // OS 注销失败：登记放回（同 desktop_unregister，不留归属与 OS 注册不一致的死键）
            if let Ok(mut owners) = state.inner.lock() {
                owners.by_accelerator.insert(accelerator.clone(), plugin_id.to_string());
                if let Ok(parsed) = accelerator.parse::<Shortcut>() {
                    owners.by_id.insert(
                        parsed.id(),
                        ShortcutTriggerPayload {
                            accelerator: accelerator.clone(),
                            plugin_id: plugin_id.to_string(),
                        },
                    );
                }
            }
            failures.push(format!("{accelerator}：{e}"));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!("部分快捷键注销失败：{}", failures.join("；")))
    }
}
