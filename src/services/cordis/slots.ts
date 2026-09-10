/**
 * slots 运行时注册表：具名槽的贡献收集与解析（纯代数见 utils/cordis/slots）。
 *
 * 槽命名约定（渲染/扩展位置常量）：view/<kind>、node/<type>、edge/<type>、tableview/<kind>、
 * titlebar/<region>、toolbar/<region>、panelhead/<region>、contextmenu/<target>、
 * settings/<block>、statusbar/<region>、command/<id>。
 * 随应用分发的视图/节点/边 = 对应默认组合成员挂载时注册的 single 槽贡献；注册经 ctx.effect 随 fiber 撤销
 * （disposePluginSlots 仅供测试/兜底，正常卸载走 effect 清理）。
 */
import type { ComponentType, ReactNode } from "react";
import { VIEW_LABELS } from "@/constants/views";
import type { SlotCardinality, SlotContribution, SlotScope } from "@/utils/cordis/slots";
import { pickSlotWinner, sortSlotList } from "@/utils/cordis/slots";

/** 视图槽载荷（view/<kind>）：label + component/render 至少其一（render 优先，重型视图承载宿主面板 id）。 */
export interface ViewSlotPayload {
  label: string;
  component?: ComponentType;
  render?: (hostId: string) => ReactNode;
}

/** 视图贡献（ViewHost 分派用：slot 胜出贡献的转换形态；render 优先，重型视图承载宿主面板 id）。 */
export interface ViewContribution {
  kind: string;
  label: string;
  component?: ComponentType;
  /** 按宿主面板/撕裂窗口 id 渲染（重型视图用；普通插件面板不提供）。 */
  render?: (hostId: string) => ReactNode;
  pluginId: string;
}

/** 槽贡献（视图载荷特化）。 */
export type ViewSlotContribution = SlotContribution<ViewSlotPayload>;

const contributions = new Map<string, SlotContribution>();

const listeners = new Set<() => void>();
function notify(): void {
  for (const listener of listeners) listener();
}

/** 订阅槽注册变化（pluginStore 据此刷新 uiRevision，驱动视图菜单/占位重渲染）；返回退订函数。 */
export function onSlotChange(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** 注册槽贡献（id 全局唯一，重复即拒绝）。 */
export function registerSlot(contrib: SlotContribution): void {
  if (contributions.has(contrib.id)) throw new Error(`槽贡献 ${contrib.id} 已注册`);
  contributions.set(contrib.id, contrib);
  notify();
}

/** 按 id 撤销单个槽贡献。 */
export function unregisterSlot(id: string): void {
  if (contributions.delete(id)) notify();
}

/** 撤销某插件的全部槽贡献（卸载/停用/重载时调用）。 */
export function disposePluginSlots(pluginId: string): void {
  let changed = false;
  for (const [id, c] of contributions) {
    if (c.pluginId === pluginId) {
      contributions.delete(id);
      changed = true;
    }
  }
  if (changed) notify();
}

/** single 槽解析：胜出贡献（无贡献 = undefined）。 */
export function resolveSlot(slot: string): SlotContribution | undefined {
  const list = [...contributions.values()].filter((c) => c.slot === slot);
  return pickSlotWinner(list);
}

/** list 槽解析：全部贡献按 priority 降序。 */
export function listSlot(slot: string): SlotContribution[] {
  return sortSlotList([...contributions.values()].filter((c) => c.slot === slot));
}

/** 全部已注册槽名（视图菜单/调试用）。 */
export function registeredSlots(): string[] {
  return [...new Set([...contributions.values()].map((c) => c.slot))];
}

/** 全部已注册的视图 kind（槽名剥 `view/` 前缀；视图菜单/选择器用）。 */
export function viewKinds(): string[] {
  const prefix = "view/";
  return registeredSlots()
    .filter((s) => s.startsWith(prefix))
    .map((s) => s.slice(prefix.length));
}

/** 槽注册通用 opts（cardinality/scope/priority/id）。 */
export interface SlotRegisterOptions {
  cardinality?: SlotCardinality;
  scope?: SlotScope;
  priority?: number;
  /** 覆盖默认贡献 id（缺省 `<pluginId>:<slot>`）。 */
  id?: string;
}

/** 注册任意槽贡献；返回撤销函数。默认 id = `<pluginId>:<slot>`。
 *  single 槽同插件重复注册（同 slot）按「重复 id 拒绝」（语义歧义）；list 槽（可多贡献）
 *  同插件重复注册自动加序去重（`<pluginId>:<slot>:N`）。 */
export function registerSlotContrib(
  slot: string,
  pluginId: string,
  payload: unknown,
  opts?: SlotRegisterOptions,
): () => void {
  const cardinality = opts?.cardinality ?? "single";
  const base = opts?.id ?? `${pluginId}:${slot}`;
  let id = base;
  // list 可多贡献：同插件同槽重复注册时 id 自动去重。
  if (cardinality === "list") {
    let n = 1;
    while (contributions.has(id)) id = `${base}:${n++}`;
  }
  registerSlot({
    id,
    pluginId,
    slot,
    cardinality,
    scope: opts?.scope ?? "root",
    priority: opts?.priority ?? 0,
    payload,
  });
  return () => unregisterSlot(id);
}

/** 注册视图槽贡献（slot = `view/<kind>`）；返回撤销函数。 */
export function registerViewSlot(
  kind: string,
  pluginId: string,
  payload: ViewSlotPayload,
  opts?: SlotRegisterOptions,
): () => void {
  return registerSlotContrib(`view/${kind}`, pluginId, payload, opts);
}

/** 注册画布节点槽贡献（slot = `node/<type>`；single 胜出，可被同 type 高 priority 替换）。 */
export function registerNodeSlot(type: string, pluginId: string, component: ComponentType, opts?: SlotRegisterOptions): () => void {
  return registerSlotContrib(`node/${type}`, pluginId, { component }, opts);
}

/** 注册画布边槽贡献（slot = `edge/<type>`；single 胜出）。 */
export function registerEdgeSlot(type: string, pluginId: string, component: ComponentType, opts?: SlotRegisterOptions): () => void {
  return registerSlotContrib(`edge/${type}`, pluginId, { component }, opts);
}

/** 注册表格视图槽贡献（slot = `tableview/<kind>`；single 胜出）。 */
export function registerTableViewSlot(kind: string, pluginId: string, payload: { label: string; component: ComponentType }, opts?: SlotRegisterOptions): () => void {
  return registerSlotContrib(`tableview/${kind}`, pluginId, payload, opts);
}

/** 解析某视图 kind 的胜出贡献（ViewHost 分派用；无贡献 = undefined）。 */
export function resolveViewKind(kind: string): ViewSlotContribution | undefined {
  return resolveSlot(`view/${kind}`) as ViewSlotContribution | undefined;
}

/** 解析画布节点胜出贡献（CanvasView nodeTypes 合并用；无贡献 = undefined）。 */
export function resolveNodeSlot(type: string): SlotContribution<{ component: ComponentType }> | undefined {
  return resolveSlot(`node/${type}`) as SlotContribution<{ component: ComponentType }> | undefined;
}

/** 解析画布边胜出贡献（CanvasView edgeTypes 合并用）。 */
export function resolveEdgeSlot(type: string): SlotContribution<{ component: ComponentType }> | undefined {
  return resolveSlot(`edge/${type}`) as SlotContribution<{ component: ComponentType }> | undefined;
}

/** 解析表格视图胜出贡献（TableEditor 视图分派用）。 */
export function resolveTableViewSlot(kind: string): SlotContribution<{ label: string; component: ComponentType }> | undefined {
  return resolveSlot(`tableview/${kind}`) as SlotContribution<{ label: string; component: ComponentType }> | undefined;
}

/** 视图显示名（视图槽标签 → 宿主视图标签 → 原样兜底，不崩溃）。 */
export function pluginViewLabel(view: string): string {
  return resolveViewKind(view)?.payload.label ?? (VIEW_LABELS as Record<string, string>)[view] ?? view;
}

/** 画布节点 type 集合（槽名剥 `node/` 前缀；CanvasView 内置节点/边合并兜底用）。 */
export function nodeKinds(): string[] {
  const prefix = "node/";
  return registeredSlots()
    .filter((s) => s.startsWith(prefix))
    .map((s) => s.slice(prefix.length));
}

/** 画布边 type 集合（槽名剥 `edge/` 前缀）。 */
export function edgeKinds(): string[] {
  const prefix = "edge/";
  return registeredSlots()
    .filter((s) => s.startsWith(prefix))
    .map((s) => s.slice(prefix.length));
}

/** 表格视图 kind 集合（槽名剥 `tableview/` 前缀）。 */
export function tableViewKinds(): string[] {
  const prefix = "tableview/";
  return registeredSlots()
    .filter((s) => s.startsWith(prefix))
    .map((s) => s.slice(prefix.length));
}

/** UI 区域槽载荷（titlebar/toolbar/panelhead/contextmenu/settings/statusbar 等具名槽位）。 */
export interface UiSlotPayload {
  /** 渲染组件（无 props 契约）。 */
  component: ComponentType;
}

/** UI 区域槽（titlebar/toolbar/panelhead/contextmenu/settings/statusbar 等）：默认 list（多贡献有序），可指定 single。 */
export function registerUiSlot(
  slot: string,
  pluginId: string,
  payload: UiSlotPayload,
  opts?: SlotRegisterOptions,
): () => void {
  return registerSlotContrib(slot, pluginId, payload, { cardinality: "list", ...opts });
}
