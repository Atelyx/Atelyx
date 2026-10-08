/**
 * 插件托盘菜单桥：菜单树写入 Rust 注册表（与内置项平铺同层，插件之间加分隔线，子菜单随树），
 * 点击事件由 Rust 定向发回注册窗口后按叶子 key 分发到注册 handler。
 * 归属与生命周期由内核 ctx.effect 管理（随插件停用清除），本模块只管传输与分发。
 */
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

/** 插件托盘菜单树节点（ctx.tray.setMenu 的入参形状）。 */
export type TrayMenuInput =
  | { type: "item"; id: string; label: string; onActivate: () => void }
  | { type: "submenu"; id: string; label: string; items: TrayMenuInput[] }
  | { type: "separator" };

interface TrayMenuNodeWire {
  type: "item" | "submenu" | "separator";
  key?: string;
  label?: string;
  items?: TrayMenuNodeWire[];
}

/** Rust 点击回传事件（payload = 叶子 key，只发注册来源窗口）。 */
const TRAY_PLUGIN_MENU_EVENT = "atelyx:tray-plugin-menu";

let listening = false;
const handlers = new Map<string, () => void>();

async function ensureListener(): Promise<void> {
  if (listening) return;
  listening = true;
  await listen<string>(TRAY_PLUGIN_MENU_EVENT, (event) => {
    handlers.get(event.payload)?.();
  });
}

function leafKey(pluginId: string, path: string[]): string {
  return [pluginId, ...path].join(":");
}

function toWire(pluginId: string, items: TrayMenuInput[], path: string[]): TrayMenuNodeWire[] {
  return items.map((item) => {
    if (item.type === "separator") return { type: "separator" };
    if (item.type === "submenu") {
      return {
        type: "submenu",
        label: item.label,
        items: toWire(pluginId, item.items, [...path, item.id]),
      };
    }
    const key = leafKey(pluginId, [...path, item.id]);
    handlers.set(key, item.onActivate);
    return { type: "item", key, label: item.label };
  });
}

/** 写入/覆盖本插件的托盘菜单树；重复调用覆盖（旧叶子 handler 一并失效）。 */
export async function setPluginTrayMenu(pluginId: string, items: TrayMenuInput[]): Promise<void> {
  await ensureListener();
  // 先清旧叶子 handler 再登记新树：覆盖后旧 key 不再命中（同 key 覆盖除外）
  for (const key of [...handlers.keys()]) {
    if (key.startsWith(`${pluginId}:`)) handlers.delete(key);
  }
  const wire = toWire(pluginId, items, []);
  await invoke("tray_set_plugin_menu", { pluginId, items: wire });
}

/** 清除本插件的托盘菜单（幂等；key 前缀范围内的 handler 一并摘除）。 */
export async function clearPluginTrayMenu(pluginId: string): Promise<void> {
  for (const key of [...handlers.keys()]) {
    if (key.startsWith(`${pluginId}:`)) handlers.delete(key);
  }
  await invoke("tray_set_plugin_menu", { pluginId, items: null });
}
