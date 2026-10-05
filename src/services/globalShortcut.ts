/**
 * 全局快捷键的 service 层：`ctx.shortcuts` 的 OS 层后端调用（Rust 命令见
 * `src-tauri/src/commands/global_shortcut.rs`）。
 *
 * 注册/注销/按插件释放三面命令 + 触发事件订阅。触发事件只投递主窗口（Rust 侧固定转发
 * 目标），载荷携带**原始注册串**——前端一切键控都按注册时的原串，不做归一化。
 */
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { UnlistenFn } from "@tauri-apps/api/event";
import type { WindowOptions } from "@/types";

/** 注册全局快捷键（同键已被其他插件占用即失败；同插件重复注册幂等）。 */
export function registerGlobalShortcut(accelerator: string, pluginId: string): Promise<void> {
  return invoke("plugin_shortcut_register", { accelerator, pluginId });
}

/** 注册窗口切换热键（Rust 直控：触发时按 `windowToggle.options` 声明切换承载 `view` 的
 *  撕裂窗口，不经主窗口 JS，主窗口驻留托盘时照常生效；同键已被其他插件占用即失败；
 *  同插件重复注册幂等，声明变更原地更新）。 */
export function registerWindowToggleShortcut(
  accelerator: string,
  pluginId: string,
  windowToggle: { view: string; options: WindowOptions },
): Promise<void> {
  return invoke("plugin_shortcut_register", { accelerator, pluginId, windowToggle });
}

/** 注销单个全局快捷键（仅归属插件可注销；未注册 = no-op）。 */
export function unregisterGlobalShortcut(accelerator: string, pluginId: string): Promise<void> {
  return invoke("plugin_shortcut_unregister", { accelerator, pluginId });
}

/** 按插件整体注销（插件停用/卸载的宿主收口；未持有任何快捷键 = no-op）。 */
export function releasePluginGlobalShortcuts(pluginId: string): Promise<void> {
  return invoke("plugin_shortcut_release_plugin", { pluginId });
}

/** 单条全局快捷键登记（OS 层登记表现查结果）。 */
export interface GlobalShortcutRegistration {
  accelerator: string;
  pluginId: string;
  /** true = 窗口切换热键（触发由 Rust 直控窗口显隐，无 JS 回调）。 */
  windowToggle: boolean;
}

/** 列出当前登记的全部全局快捷键（OS 层真源，跨窗口一致；移动端恒空）。 */
export function listGlobalShortcuts(): Promise<GlobalShortcutRegistration[]> {
  return invoke("plugin_shortcut_list");
}

/** 订阅全局快捷键触发（只会在主窗口收到回调；accelerator = 原始注册串）。 */
export function onGlobalShortcutTriggered(
  handler: (accelerator: string, pluginId: string) => void,
): Promise<UnlistenFn> {
  return listen<{ accelerator: string; pluginId: string }>(
    "plugin-shortcut-triggered",
    (e) => handler(e.payload.accelerator, e.payload.pluginId),
  );
}
