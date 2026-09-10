/**
 * 宿主侧 UI 贡献注册表：插件主线程平面（设置项/应用页/命令/主题设置项）。
 *
 * 注册经 ctx.slots 触达（slotsApi.ts，随插件 fiber 生命周期撤销）；消费者经 pluginStore 读取。
 * 本表为「键值型」主线程平面：设置 key / 应用页 id / 命令 globalId / 主题设置 key 天然唯一，
 * 同 key 后注册覆盖（last-wins）；需要优先级竞争的位置一律走 slots。
 * 节点/边/表格视图的读取 getter 亦收拢于此（读 slots 注册表，single 胜出），供 pluginStore 统一读取。
 */
import type { ComponentType } from "react";
import { resolveNodeSlot, resolveEdgeSlot, resolveTableViewSlot, nodeKinds, edgeKinds, tableViewKinds } from "./slots";

/** 插件设置项注册（设置页左侧 tab）。 */
export interface PluginSettingRegistration {
  pluginId: string;
  key: string;
  label: string;
  component: ComponentType;
}
/** 插件应用级页面注册（应用页面/模式；路由接管接入处）。 */
export interface PluginAppPageRegistration {
  pluginId: string;
  id: string;
  label: string;
  component: ComponentType;
}
/** 插件画布节点注册（CanvasView nodeTypes 合并接入处）。 */
export interface PluginNodeRegistration {
  pluginId: string;
  type: string;
  component: ComponentType;
}
/** 插件画布边注册（CanvasView edgeTypes 合并接入处；single 槽胜出，可高 priority 替换内置）。 */
export interface PluginEdgeRegistration {
  pluginId: string;
  type: string;
  component: ComponentType;
}
/** 插件主线程命令注册。 */
export interface PluginCommandRegistration {
  pluginId: string;
  id: string;
  label: string;
  run: () => unknown;
  /** 快捷键（如 "mod+k"；可选；主线程统一监听匹配后执行）。 */
  shortcut?: string;
}

/** 插件命令贡献（管理 UI「运行命令」入口：全局 id = `<pluginId>:<命令 id>`）。 */
export interface PluginCommandContribution {
  globalId: string;
  pluginId: string;
  id: string;
  label: string;
}
/** 插件表格视图注册（表格编辑器内视图：工具条视图列表合并 + 内容区分派，见 TableEditor）。 */
export interface PluginTableViewRegistration {
  pluginId: string;
  kind: string;
  label: string;
  component: ComponentType;
}

/** 主题插件设置项组件 props（主题页设置区：绑定该插件条目的设置值字典）。 */
export interface ThemeSettingComponentProps {
  /** 该插件条目的设置值字典（含预置键 colorMode/accentColor 与插件自定义键）。 */
  value: Record<string, unknown>;
  /** 写设置项（value = undefined 删除键恢复默认；落盘 global.json）。 */
  onChange: (key: string, value: unknown) => void;
}

/** 主题插件设置项注册（主题页设置区：激活该插件时渲染其设置区块；键 pluginId 内唯一）。 */
export interface ThemeSettingRegistration {
  pluginId: string;
  key: string;
  label: string;
  component: ComponentType<ThemeSettingComponentProps>;
}

const settings = new Map<string, PluginSettingRegistration>(); // `${pluginId}:${key}` → 注册
const appPages = new Map<string, PluginAppPageRegistration>(); // id → 注册
const commands = new Map<string, PluginCommandRegistration>(); // `${pluginId}:${id}` → 注册
const themeSettings = new Map<string, ThemeSettingRegistration>(); // `${pluginId}:${key}` → 注册

const listeners = new Set<() => void>();
function notify(): void {
  for (const listener of listeners) listener();
}

/** 订阅 UI 注册变化（视图菜单/设置 tab 等据此刷新）；返回退订函数。 */
export function onPluginUiChange(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function getPluginSettings(): PluginSettingRegistration[] {
  return [...settings.values()];
}
export function getPluginSetting(key: string): PluginSettingRegistration | undefined {
  return settings.get(key);
}
export function getPluginAppPages(): PluginAppPageRegistration[] {
  return [...appPages.values()];
}
/** 画布节点槽胜出者（single 槽；CanvasView 合并 nodeTypes 用）。 */
export function getPluginNodes(): PluginNodeRegistration[] {
  return nodeKinds()
    .map((type) => ({ type, winner: resolveNodeSlot(type) }))
    .filter((x): x is { type: string; winner: NonNullable<ReturnType<typeof resolveNodeSlot>> } => !!x.winner)
    .map(({ type, winner }) => ({ pluginId: winner.pluginId, type, component: winner.payload.component }));
}
/** 画布边槽胜出者（single 槽；CanvasView 合并 edgeTypes 用）。 */
export function getPluginEdges(): PluginEdgeRegistration[] {
  return edgeKinds()
    .map((type) => ({ type, winner: resolveEdgeSlot(type) }))
    .filter((x): x is { type: string; winner: NonNullable<ReturnType<typeof resolveEdgeSlot>> } => !!x.winner)
    .map(({ type, winner }) => ({ pluginId: winner.pluginId, type, component: winner.payload.component }));
}
export function getPluginCommands(): PluginCommandRegistration[] {
  return [...commands.values()];
}
export function getPluginTableView(kind: string): PluginTableViewRegistration | undefined {
  const winner = resolveTableViewSlot(kind);
  if (!winner) return undefined;
  return { pluginId: winner.pluginId, kind, label: winner.payload.label, component: winner.payload.component };
}
export function getPluginTableViews(): PluginTableViewRegistration[] {
  return tableViewKinds()
    .map((kind) => ({ kind, winner: resolveTableViewSlot(kind) }))
    .filter((x): x is { kind: string; winner: NonNullable<ReturnType<typeof resolveTableViewSlot>> } => !!x.winner)
    .map(({ kind, winner }) => ({ pluginId: winner.pluginId, kind, label: winner.payload.label, component: winner.payload.component }));
}
/** 某主题插件的设置项注册（主题页设置区渲染用；空 = 该插件无自定义设置项）。 */
export function getPluginThemeSettings(pluginId: string): ThemeSettingRegistration[] {
  return [...themeSettings.values()].filter((s) => s.pluginId === pluginId);
}

// ===== 注册（经 ctx.slots 调用，pluginId 由调用方上下文解析） =====

/** 注册插件设置项（设置页 tab）；pluginId 溯源，返回精确撤销（删本项）。 */
export function registerPluginSetting(
  pluginId: string,
  key: string,
  label: string,
  component: ComponentType,
): () => void {
  const k = `${pluginId}:${key}`;
  settings.set(k, { pluginId, key, label, component });
  notify();
  return () => {
    if (settings.delete(k)) notify();
  };
}

/** 注册插件应用级页面（全页接管）；pluginId 溯源，返回精确撤销。 */
export function registerPluginAppPage(
  pluginId: string,
  id: string,
  label: string,
  component: ComponentType,
): () => void {
  appPages.set(id, { pluginId, id, label, component });
  notify();
  return () => {
    if (appPages.delete(id)) notify();
  };
}

/** 注册插件主线程命令（管理 UI「运行命令」入口）；pluginId 溯源，返回精确撤销。 */
export function registerPluginCommand(
  pluginId: string,
  id: string,
  label: string,
  run: () => unknown,
  shortcut?: string,
): () => void {
  const k = `${pluginId}:${id}`;
  commands.set(k, { pluginId, id, label, run, ...(shortcut ? { shortcut } : {}) });
  notify();
  return () => {
    if (commands.delete(k)) notify();
  };
}

/** 注册主题插件设置项（主题页设置区）；pluginId 溯源，返回精确撤销。 */
export function registerPluginThemeSetting(
  pluginId: string,
  key: string,
  label: string,
  component: ComponentType<ThemeSettingComponentProps>,
): () => void {
  const k = `${pluginId}:${key}`;
  themeSettings.set(k, { pluginId, key, label, component });
  notify();
  return () => {
    if (themeSettings.delete(k)) notify();
  };
}

/** 撤销某插件在主线程平面的全部键值贡献（测试用；正常卸载走各注册的 ctx.effect 撤销）。
 *  视图/节点/边/表格视图槽贡献不在此表（走 slots 注册表，见 disposePluginSlots）。 */
export function unregisterPluginUi(pluginId: string): void {
  let changed = false;
  for (const [k, v] of settings) if (v.pluginId === pluginId) changed = settings.delete(k) || changed;
  for (const [k, v] of appPages) if (v.pluginId === pluginId) changed = appPages.delete(k) || changed;
  for (const [k, v] of commands) if (v.pluginId === pluginId) changed = commands.delete(k) || changed;
  for (const [k, v] of themeSettings) if (v.pluginId === pluginId) changed = themeSettings.delete(k) || changed;
  if (changed) notify();
}
