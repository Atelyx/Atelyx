/**
 * 宿主侧 UI 贡献注册表：插件主线程平面（设置项/应用页/节点/边/命令/表格视图/主题设置项/通用扩展点）。
 *
 * 注册经 ctx.slots 触达（slotsApi.ts，随插件 fiber 生命周期撤销）；消费者经 pluginStore 读取。
 * 视图贡献（view/<kind>）不在此表——走 slots 注册表（services/cordis/slots.ts）。
 */
import type { ComponentType } from "react";

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
/** 插件画布边注册（CanvasView edgeTypes 合并接入处；与节点同语义、last-wins 覆盖）。 */
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

/** 通用扩展点注册（主线程平面）：point → 条目。 */
export interface PluginUiContribution {
  pluginId: string;
  point: string;
  id?: string;
  payload: unknown;
}

const settings = new Map<string, PluginSettingRegistration>(); // `${pluginId}:${key}` → 注册
const appPages = new Map<string, PluginAppPageRegistration>(); // id → 注册
const nodes = new Map<string, PluginNodeRegistration>(); // type → 注册
const edges = new Map<string, PluginEdgeRegistration>(); // type → 注册
const commands = new Map<string, PluginCommandRegistration>(); // `${pluginId}:${id}` → 注册
const tableViews = new Map<string, PluginTableViewRegistration>(); // kind → 注册
const themeSettings = new Map<string, ThemeSettingRegistration>(); // `${pluginId}:${key}` → 注册
const uiContributions = new Map<string, PluginUiContribution>(); // `${point}:${pluginId}${id ? ":"+id : ""}` → 注册

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
export function getPluginNode(type: string): PluginNodeRegistration | undefined {
  return nodes.get(type);
}
export function getPluginNodes(): PluginNodeRegistration[] {
  return [...nodes.values()];
}
export function getPluginEdge(type: string): PluginEdgeRegistration | undefined {
  return edges.get(type);
}
export function getPluginEdges(): PluginEdgeRegistration[] {
  return [...edges.values()];
}
export function getPluginCommands(): PluginCommandRegistration[] {
  return [...commands.values()];
}
export function getPluginTableView(kind: string): PluginTableViewRegistration | undefined {
  return tableViews.get(kind);
}
export function getPluginTableViews(): PluginTableViewRegistration[] {
  return [...tableViews.values()];
}
/** 某主题插件的设置项注册（主题页设置区渲染用；空 = 该插件无自定义设置项）。 */
export function getPluginThemeSettings(pluginId: string): ThemeSettingRegistration[] {
  return [...themeSettings.values()].filter((s) => s.pluginId === pluginId);
}

/** 某扩展点的全部主线程注册条目（宿主/其他插件消费自定义扩展点用）。 */
export function listUiContributions(point: string): PluginUiContribution[] {
  return [...uiContributions.values()].filter((c) => c.point === point);
}

// ===== 注册（经 ctx.slots 调用，pluginId 由调用方上下文解析） =====

/** 注册插件表格视图（表格编辑器视图列表；按 pluginId 溯源，卸载随插件撤销）。 */
export function registerPluginTableView(
  pluginId: string,
  kind: string,
  label: string,
  component: ComponentType,
): void {
  tableViews.set(kind, { pluginId, kind, label, component });
  notify();
}

/** 撤销某插件在主线程平面的全部贡献（停用/卸载/重载时调用；按 pluginId 溯源）。 */
export function unregisterPluginUi(pluginId: string): void {
  let changed = false;
  for (const [k, v] of settings) if (v.pluginId === pluginId) changed = settings.delete(k) || changed;
  for (const [k, v] of appPages) if (v.pluginId === pluginId) changed = appPages.delete(k) || changed;
  for (const [k, v] of nodes) if (v.pluginId === pluginId) changed = nodes.delete(k) || changed;
  for (const [k, v] of edges) if (v.pluginId === pluginId) changed = edges.delete(k) || changed;
  for (const [k, v] of commands) if (v.pluginId === pluginId) changed = commands.delete(k) || changed;
  for (const [k, v] of tableViews) if (v.pluginId === pluginId) changed = tableViews.delete(k) || changed;
  for (const [k, v] of themeSettings) if (v.pluginId === pluginId) changed = themeSettings.delete(k) || changed;
  for (const [k, v] of uiContributions) if (v.pluginId === pluginId) changed = uiContributions.delete(k) || changed;
  if (changed) notify();
}
