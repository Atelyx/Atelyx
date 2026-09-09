/**
 * 主线程 UI 平面：UI 类插件（panel/setting/appPage/node/edge/command）的主线程加载与注册收集。
 *
 * UI 代码必须跑在主线程（渲染 React、触达 DOM），无法进 worker。加载方式：把插件入口作为
 * blob script 注入页面（CSP script-src 已含 blob:），代码包在 IIFE 里、只暴露按插件 id 生成的
 * `bridge` facade（`window.__atelyxPlugin__.forPlugin(id)`）与同一 React 实例。
 *
 * 信任边界（完全自由模型）：主线程插件与 App 同上下文、理论上可触达 window/invoke——
 * 这是既定边界；插件应只经 facade 注册贡献。每插件独立加载，单个插件脚本报错只影响自身。
 */
import React, { createElement, type ComponentType, type ReactNode } from "react";
import { VIEW_LABELS } from "@/constants/views";
import { VIEW_KINDS } from "@/types";
import type { CanvasFileRow, FileTreeNode, PluginTableSnapshot, VaultAccess } from "@/types";

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
/** 插件主线程命令注册（与 worker 平面命令同语义，直接持有 run）。 */
export interface PluginCommandRegistration {
  pluginId: string;
  id: string;
  label: string;
  run: () => unknown;
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

/** 通用扩展点注册（主线程平面）：point → 条目；现有 register* 是它的特化（类型化便捷入口）。 */
export interface PluginUiContribution {
  pluginId: string;
  point: string;
  id?: string;
  payload: unknown;
}

/**
 * 视图贡献（统一注册表）：所有插件视图同表注册，kind 全局唯一（重复即拒绝）；
 * kind 缺失/来源卸载时对应视图回退空面板占位。pluginId 标注来源（溯源/按插件撤销）。
 *
 * `render` 为可选承载：内置重型视图（画布/表格）需要 hostId（聚焦门控）——
 * 用 render(hostId) 而非 component（component 契约无 props，第三方面板不受影响）。
 * render 与 component 至少其一（render 优先）。
 */
export interface ViewContribution {
  kind: string;
  label: string;
  component?: ComponentType;
  /** 可选：按宿主面板/撕裂窗口 id 渲染（内置重型视图用；第三方面板不提供）。 */
  render?: (hostId: string) => ReactNode;
  pluginId: string;
}

const viewContributions = new Map<string, ViewContribution>(); // kind → 贡献（同表，pluginId 溯源）
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

/** 某视图的贡献（内置 + 插件面板统一查找；ViewHost 分派 / 视图菜单元数据用）。 */
export function getViewContribution(kind: string): ViewContribution | undefined {
  return viewContributions.get(kind);
}

/** 某扩展点的全部主线程注册条目（宿主/其他插件消费自定义扩展点用）。 */
export function listUiContributions(point: string): PluginUiContribution[] {
  return [...uiContributions.values()].filter((c) => c.point === point);
}

/** 视图显示名（内建 VIEW_LABELS → 统一注册表（插件面板）→ 原样兜底，不崩溃）。 */
export function pluginViewLabel(view: string): string {
  return (VIEW_LABELS as Record<string, string>)[view] ?? viewContributions.get(view)?.label ?? view;
}

/** 面板视图候选（视图选择器/切换菜单合并用）：内建 + 注册表里的其他贡献（内置 kind 已含在 VIEW_KINDS，不重复）。 */
export function pluginViewKinds(): string[] {
  const builtin = new Set<string>(VIEW_KINDS);
  const contributed = [...viewContributions.values()].filter((c) => !builtin.has(c.kind)).map((c) => c.kind);
  return [...(VIEW_KINDS as string[]), ...contributed];
}

// ===== 注册（经 facade 调用，pluginId 由闭包捕获） =====

/** 内置插件 id 集合（pluginStore 注入：插件行 sourceKind=builtin 的 id；用于封闭 ViewKind 的防劫持放行）。 */
let builtinPluginIds: ReadonlySet<string> = new Set();

/** 注入/复位内置插件 id 集合（pluginStore 加载插件行后调用；null 复位供测试）。 */
export function setBuiltinPluginIds(ids: ReadonlySet<string> | null): void {
  builtinPluginIds = ids ?? new Set();
}

/** 注册视图贡献（统一注册表核心）：kind 全局唯一，重复注册即拒绝。
 *  VIEW_KINDS 封闭枚举 kind 为平台保留命名空间：仅内置插件（随 App 分发的种子条目）可注册，
 *  第三方插件不得占用（防冒名劫持标签/死注册/污染视图菜单）。 */
function registerViewContribution(
  kind: string,
  label: string,
  component: ComponentType | undefined,
  pluginId: string,
  render?: (hostId: string) => ReactNode,
): void {
  if (typeof kind !== "string" || kind.length === 0) {
    throw new Error("视图贡献需要非空 kind");
  }
  if (!component && !render) {
    throw new Error("视图贡献需要 component 或 render");
  }
  if (
    ((VIEW_KINDS as readonly string[]).includes(kind) || kind === "empty") &&
    !builtinPluginIds.has(pluginId)
  ) {
    throw new Error(`视图 kind ${kind} 为内置视图保留，插件需用反向域名命名自己的 kind`);
  }
  if (viewContributions.has(kind)) {
    throw new Error(`视图 kind ${kind} 已被注册，kind 全局唯一`);
  }
  viewContributions.set(kind, { kind, label, component, render, pluginId });
  notify();
}

/** 注册内置插件视图贡献（内置插件 = 随 App 分发的种子条目；可注册 VIEW_KINDS 内 kind，停用即撤销）。
 *  `render` 可选：内置重型视图（画布/表格）经它接收宿主面板/撕裂窗口 id；`component` 与 `render`
 *  至少其一（render 优先）。 */
export function registerBuiltinView(
  pluginId: string,
  contrib: {
    kind: string;
    label: string;
    component?: ComponentType;
    render?: (hostId: string) => ReactNode;
  },
): void {
  registerViewContribution(contrib.kind, contrib.label, contrib.component, pluginId, contrib.render);
}

function registerPanel(pluginId: string, kind: string, label: string, component: ComponentType): void {
  registerViewContribution(kind, label, component, pluginId);
}
function registerSetting(pluginId: string, key: string, label: string, component: ComponentType): void {
  const globalKey = `${pluginId}:${key}`;
  settings.set(globalKey, { pluginId, key: globalKey, label, component });
  notify();
}
function registerAppPage(pluginId: string, id: string, label: string, component: ComponentType): void {
  appPages.set(id, { pluginId, id, label, component });
  notify();
}
function registerNode(pluginId: string, type: string, component: ComponentType): void {
  nodes.set(type, { pluginId, type, component });
  notify();
}
function registerEdge(pluginId: string, type: string, component: ComponentType): void {
  edges.set(type, { pluginId, type, component });
  notify();
}
function registerCommand(pluginId: string, id: string, label: string, run: () => unknown): void {
  commands.set(`${pluginId}:${id}`, { pluginId, id, label, run });
  notify();
}
function registerTableView(
  pluginId: string,
  kind: string,
  label: string,
  component: ComponentType,
): void {
  tableViews.set(kind, { pluginId, kind, label, component });
  notify();
}
function registerThemeSetting(
  pluginId: string,
  key: string,
  label: string,
  component: ComponentType<ThemeSettingComponentProps>,
): void {
  themeSettings.set(`${pluginId}:${key}`, { pluginId, key, label, component });
  notify();
}

/** 通用扩展点注册（payload 直接持有引用——主线程同域，无需序列化）。 */
function registerContribution(pluginId: string, point: string, id: string | undefined, payload: unknown): void {
  const key = `${point}:${pluginId}${id ? `:${id}` : ""}`;
  uiContributions.set(key, { pluginId, point, id, payload });
  notify();
}

/** 撤销某插件在主线程平面的全部贡献（卸载/停用/重载时调用；按 pluginId 溯源）。 */
export function unregisterPluginUi(pluginId: string): void {
  let changed = false;
  for (const [k, v] of viewContributions) {
    if (v.pluginId === pluginId) {
      changed = viewContributions.delete(k) || changed;
    }
  }
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

// ===== facade 全局 + 加载 =====

/** 表格数据访问 provider（由 pluginStore 接线注入，ui.ts 保持不 import store——分层：store 经此把
 *  当前表格数据暴露给主线程插件；null = 未接线）。 */
export interface PluginTableAccess {
  /** 订阅当前表格数据快照（立即推一次 + 变更推；返回退订函数）。 */
  subscribeSnapshot(cb: (snap: PluginTableSnapshot) => void): () => void;
  /** 选中表格行（与表格视图选中联动；null = 取消选中）。 */
  selectRow(rowId: string | null): void;
  /** 表格图片条目 → dataURL（`data:` 内嵌条目原样透传；读取失败 reject，调用方兜底）。 */
  resolveImage(entry: string): Promise<string>;
}

let tableAccess: PluginTableAccess | null = null;

/** 注入/复位表格数据访问（pluginStore.load 时接线；null 复位供测试）。 */
export function setPluginTableAccess(access: PluginTableAccess | null): void {
  tableAccess = access;
}

/** 插件侧仓库访问（facade 的 listFiles/open* 经它转发；pluginStore 接线注入，
 *  ui.ts 保持不 import store——分层：store 经此把仓库文件树与打开回调暴露给主线程插件）。 */
let vaultAccess: VaultAccess | null = null;

/** 注入/复位插件侧仓库访问（pluginStore.load 时接线；null 复位供测试）。 */
export function setPluginVaultAccess(access: VaultAccess | null): void {
  vaultAccess = access;
}

/** 读取插件侧仓库访问（测试/诊断用；未接线 = null）。 */
export function getPluginVaultAccess(): VaultAccess | null {
  return vaultAccess;
}

/** 主题插件设置项访问（facade 的 getThemeSettings/setThemeSetting 经它转发；pluginStore 接线注入
 *  settingsStore——ui.ts 保持不 import store，分层：store 经此把该插件条目的设置值暴露给主线程插件）。 */
export interface PluginThemeSettingsAccess {
  /** 读某主题插件条目的设置值字典（含预置键 colorMode/accentColor 与插件自定义键）。 */
  getSettings(pluginId: string): Record<string, unknown>;
  /** 写某主题插件条目的设置项（value = undefined 删除键恢复默认；落盘 global.json，失败仅记日志）。 */
  setSetting(pluginId: string, key: string, value: unknown): void;
}

let themeSettingsAccess: PluginThemeSettingsAccess | null = null;

/** 注入/复位主题设置项访问（pluginStore.load 时接线；null 复位供测试）。 */
export function setPluginThemeSettingsAccess(access: PluginThemeSettingsAccess | null): void {
  themeSettingsAccess = access;
}

/** 插件主线程 facade（插件代码经 `window.__atelyxPlugin__.forPlugin(id)` 取得）。 */
export interface PluginMainThreadFacade {
  React: typeof React;
  h: typeof createElement;
  registerPanel(opts: { kind: string; label: string; component: ComponentType }): void;
  registerSetting(opts: { key: string; label: string; component: ComponentType }): void;
  registerAppPage(opts: { id: string; label: string; component: ComponentType }): void;
  registerNode(opts: { type: string; component: ComponentType }): void;
  registerEdge(opts: { type: string; component: ComponentType }): void;
  registerCommand(opts: { id: string; label: string; run: () => unknown }): void;
  registerTableView(opts: { kind: string; label: string; component: ComponentType }): void;
  /** 注册主题设置项（主题页设置区渲染：激活该主题插件时展示其设置区块；props 绑定该插件条目的设置值字典）。 */
  registerThemeSetting(opts: {
    key: string;
    label: string;
    component: ComponentType<ThemeSettingComponentProps>;
  }): void;
  /** 读本插件条目的主题设置值字典（未接线返回空对象）。 */
  getThemeSettings(): Record<string, unknown>;
  /** 写本插件条目的主题设置项（value = undefined 删除键恢复默认；落盘 global.json，失败仅记日志）。 */
  setThemeSetting(key: string, value: unknown): void;
  /** 通用扩展点注册（point 为任意字符串；payload 直接持有引用，可含组件/函数）。 */
  registerContribution(opts: { point: string; id?: string; payload: unknown }): void;
  /** 订阅当前打开的表格的数据快照（tableStore 为应用级单例，撕裂窗口同源；立即推一次 + 变更推；返回退订函数）。 */
  subscribeTableData(cb: (snap: PluginTableSnapshot) => void): () => void;
  /** 选中表格行（与表格视图选中联动；null = 取消选中）。 */
  selectTableRow(rowId: string | null): void;
  /** 解析表格图片条目为 dataURL（`data:` 内嵌条目原样透传；失败 reject，调用方兜底）。 */
  resolveTableImage(entry: string): Promise<string>;
  /** 读取当前仓库文件树（未接线返回空数组；任何面板插件可用，含第三方搜索面板）。 */
  listFiles(): Promise<FileTreeNode[]>;
  /** 打开画布（.atlx/.canvas 行，与文件面板同一入口；未接线时 no-op）。 */
  openCanvasFile(row: CanvasFileRow): void;
  /** 打开笔记窗口（未接线时 no-op）。 */
  openNote(file: string, title: string): void;
  /** 打开表格窗口（未接线时 no-op）。 */
  openTable(file: string, title: string): void;
}

declare global {
  interface Window {
    /** 插件主线程平面入口（App 启动时经 exposePluginFacade 挂载）。 */
    __atelyxPlugin__?: { forPlugin(pluginId: string): PluginMainThreadFacade };
  }
}

/** 挂载全局 facade（幂等；App 启动时调用）。 */
export function exposePluginFacade(): void {
  if (window.__atelyxPlugin__) return;
  window.__atelyxPlugin__ = {
    forPlugin: (pluginId) => ({
      React,
      h: React.createElement,
      registerPanel: (o) => registerPanel(pluginId, o.kind, o.label, o.component),
      registerSetting: (o) => registerSetting(pluginId, o.key, o.label, o.component),
      registerAppPage: (o) => registerAppPage(pluginId, o.id, o.label, o.component),
      registerNode: (o) => registerNode(pluginId, o.type, o.component),
      registerEdge: (o) => registerEdge(pluginId, o.type, o.component),
      registerCommand: (o) => registerCommand(pluginId, o.id, o.label, o.run),
      registerTableView: (o) => registerTableView(pluginId, o.kind, o.label, o.component),
      registerThemeSetting: (o) => registerThemeSetting(pluginId, o.key, o.label, o.component),
      getThemeSettings: () => (themeSettingsAccess ? themeSettingsAccess.getSettings(pluginId) : {}),
      setThemeSetting: (key, value) => themeSettingsAccess?.setSetting(pluginId, key, value),
      registerContribution: (o) => registerContribution(pluginId, o.point, o.id, o.payload),
      subscribeTableData: (cb) => (tableAccess ? tableAccess.subscribeSnapshot(cb) : () => {}),
      selectTableRow: (rowId) => tableAccess?.selectRow(rowId),
      resolveTableImage: (entry) =>
        tableAccess ? tableAccess.resolveImage(entry) : Promise.reject(new Error("插件表格访问未就绪")),
      listFiles: () => (vaultAccess ? vaultAccess.listFiles() : Promise.resolve([])),
      openCanvasFile: (row) => vaultAccess?.openCanvasFile(row),
      openNote: (file, title) => vaultAccess?.openNote(file, title),
      openTable: (file, title) => vaultAccess?.openTable(file, title),
    }),
  };
}

/** 加载插件主线程入口（blob script 注入）；先撤销该插件旧贡献（重载防重复注册）。
 * 返回 Promise：脚本加载/执行错误 → reject（pluginStore 据此置 failed）——
 * 运行期错误经 window error 事件按 sourceURL 标记过滤，只认本插件脚本。 */
export function loadUiPlugin(pluginId: string, code: string): Promise<void> {
  unregisterPluginUi(pluginId);
  return new Promise((resolve, reject) => {
    // 注入 React（供 TSX 经 esbuild jsx-transform 转出的 React.createElement 引用）。
    const source = `(function(){\nvar bridge = window.__atelyxPlugin__.forPlugin(${JSON.stringify(pluginId)});\nvar React = bridge.React;\n${code}\n})();\n//# sourceURL=atelyx-plugin-${pluginId}`;
    const url = URL.createObjectURL(new Blob([source], { type: "text/javascript" }));
    const script = document.createElement("script");
    let settled = false;
    const done = (error?: string): void => {
      if (settled) return;
      settled = true;
      window.removeEventListener("error", onWindowError);
      URL.revokeObjectURL(url);
      script.remove();
      if (error) reject(new Error(error));
      else resolve();
    };
    // 运行期错误：ErrorEvent.filename 为 `# sourceURL` 标注名（或 blob URL）；只认本插件脚本。
    const onWindowError = (e: ErrorEvent): void => {
      const src = e.filename || "";
      if (src.includes("atelyx-plugin-") || src === url) {
        done(e.message || "插件 UI 执行出错");
      }
    };
    script.src = url;
    script.onload = () => done();
    script.onerror = () => done("插件 UI 入口加载失败");
    window.addEventListener("error", onWindowError);
    document.head.appendChild(script);
  });
}
