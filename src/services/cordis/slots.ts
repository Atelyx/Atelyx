/**
 * slots 运行时注册表：具名槽的贡献收集与解析（纯代数见 utils/cordis/slots）。
 *
 * 可注册的槽位以 constants/slots.ts 的声明表为唯一清单：固定具名槽（titlebar/toolbar/
 * panelhead/statusbar/settings）与右键菜单目标（contextmenu/<target>）未声明即注册失败并附近似槽名
 * 提示；开放 kind 槽（view/node/edge/tableview/empty/inspector）按前缀放行。
 * 随应用分发的视图/节点/边 = 对应默认组合成员挂载时注册的 single 槽贡献；注册经 ctx.effect 随 fiber 撤销。
 */
import type { ComponentType, ReactNode } from "react";
import { VIEW_LABELS } from "@/constants/views";
import { findSlotDeclaration, slotPayloadShape, suggestSlotNames } from "@/constants/slots";
import type { SlotDeclaration } from "@/constants/slots";
import type { SlotCardinality, SlotContribution, SlotDecorator } from "@/utils/cordis/slots";
import { pickSlotWinner, sortSlotDecorators, sortSlotList } from "@/utils/cordis/slots";

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
/** 槽装饰器（ctx.slots.decorate 注册；与贡献分开收集，宿主渲染时按链包裹）。 */
const decorators = new Map<string, SlotDecorator>();

/** 每槽装饰器纪元号：任何注册/撤销单调递增。宿主机据此识别「装饰器被重载/重挂」（含同 id 重注册），
 *  复位其本地剔除记录——仅凭注册集合签名在 React 批处理合并中间渲染时会漏判，纪元号不受此影响。 */
const decoratorEpochs = new Map<string, number>();
function bumpDecoratorEpoch(slot: string): void {
  decoratorEpochs.set(slot, (decoratorEpochs.get(slot) ?? 0) + 1);
}

/** 某槽装饰器纪元号（宿主机识别注册变化用；无变化 = 0）。 */
export function decoratorEpochOf(slot: string): number {
  return decoratorEpochs.get(slot) ?? 0;
}

const globalListeners = new Set<(slot: string) => void>();
function notify(slot: string): void {
  for (const listener of globalListeners) listener(slot);
}

/** 订阅槽注册/装饰变化（回调收到变化槽名；视图菜单与 pluginStore 的按槽修订据此刷新）。返回退订函数。 */
export function onSlotChange(listener: (slot: string) => void): () => void {
  globalListeners.add(listener);
  return () => {
    globalListeners.delete(listener);
  };
}

/** 注册槽贡献（id 全局唯一，重复即拒绝）。 */
export function registerSlot(contrib: SlotContribution): void {
  if (contributions.has(contrib.id)) throw new Error(`槽贡献 ${contrib.id} 已注册`);
  contributions.set(contrib.id, contrib);
  notify(contrib.slot);
}

/** 按 id 撤销单个槽贡献。 */
export function unregisterSlot(id: string): void {
  const prev = contributions.get(id);
  if (prev && contributions.delete(id)) notify(prev.slot);
}

/** 注册槽装饰器（id 全局唯一，重复即拒绝）。 */
export function registerSlotDecorator(dec: SlotDecorator): void {
  if (decorators.has(dec.id)) throw new Error(`槽装饰器 ${dec.id} 已注册`);
  decorators.set(dec.id, dec);
  bumpDecoratorEpoch(dec.slot);
  notify(dec.slot);
}

/** 按 id 撤销单个槽装饰器。 */
export function unregisterSlotDecorator(id: string): void {
  const prev = decorators.get(id);
  if (prev && decorators.delete(id)) {
    bumpDecoratorEpoch(prev.slot);
    notify(prev.slot);
  }
}

/** 某槽的装饰器链（priority 降序；外层 = 高 priority）。 */
export function listDecorators(slot: string): SlotDecorator[] {
  return sortSlotDecorators([...decorators.values()].filter((d) => d.slot === slot));
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

/** 槽注册通用 opts（cardinality/priority/id）。 */
export interface SlotRegisterOptions {
  cardinality?: SlotCardinality;
  priority?: number;
  /** 覆盖默认贡献 id（缺省 `<pluginId>:<slot>`）。 */
  id?: string;
}

/** 槽位声明校验：未声明即抛错（附近似槽名提示），基数与载荷不符即抛错。
 *  抛错随插件 apply 传播 → 挂载器标记该行 failed + 可读原因（不静默丢失）。 */
function assertSlotDeclared(slot: string, cardinality: SlotCardinality, payload: unknown): void {
  const decl = findSlotDeclaration(slot);
  if (!decl) {
    const hints = suggestSlotNames(slot);
    const hint = hints.length > 0 ? `；是否想注册：${hints.join("、")}` : "（宿主未渲染该位置）";
    throw new Error(`未声明的槽位「${slot}」${hint}`);
  }
  if (decl.cardinality !== cardinality) {
    throw new Error(`槽位「${slot}」的基数是 ${decl.cardinality}，收到 ${cardinality}`);
  }
  assertSlotPayload(slot, decl, payload);
}

/** 载荷字段的值类型契约（按字段名约定）：槽位字段面小且稳定，无需在声明表逐槽展开。
 *  `undefined` 不在此判（缺字段由 required 校验负责），只拦「字段给了但类型不符」。 */
const PAYLOAD_FIELD_TYPES: Record<string, "string" | "function"> = {
  label: "string",
  component: "function",
  render: "function",
  onClick: "function",
};

/** 载荷字段契约校验：缺必需字段 / 带未知字段 / 值类型不符即抛错
 *  （治「字段名拼错静默渲染空白」与「回调传错形态运行时才炸」）。 */
function assertSlotPayload(slot: string, decl: SlotDeclaration, payload: unknown): void {
  if (typeof payload !== "object" || payload === null) {
    throw new Error(`槽位「${slot}」的载荷须为对象（应为 ${slotPayloadShape(decl)}）`);
  }
  const record = payload as Record<string, unknown>;
  // 缺字段只看自有属性（原型链上的同名成员不算提供，否则 Object.prototype 成员名会被误判为已给）。
  const hasOwn = (key: string): boolean => Object.prototype.hasOwnProperty.call(record, key);
  const missing = decl.required.filter((key) => !hasOwn(key) || record[key] === undefined);
  if (missing.length > 0) {
    throw new Error(`槽位「${slot}」的载荷缺少字段：${missing.join("、")}（应为 ${slotPayloadShape(decl)}）`);
  }
  const allowed = new Set<string>([...decl.required, ...(decl.optional ?? [])]);
  const unknown = Object.keys(record).filter((key) => !allowed.has(key));
  if (unknown.length > 0) {
    throw new Error(`槽位「${slot}」的载荷含未知字段：${unknown.join("、")}（应为 ${slotPayloadShape(decl)}）`);
  }
  const wrongType = Object.keys(record).filter((key) => {
    const expected = PAYLOAD_FIELD_TYPES[key];
    if (!expected || record[key] === undefined) return false;
    return expected === "string" ? typeof record[key] !== "string" : typeof record[key] !== "function";
  });
  if (wrongType.length > 0) {
    throw new Error(`槽位「${slot}」的载荷字段类型不符：${wrongType.join("、")}（应为 ${slotPayloadShape(decl)}）`);
  }
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
  // priority 非有限数字会让排序比较不可预期（NaN 恒 false 的静默乱序），直接拒绝。
  if (opts?.priority !== undefined && (typeof opts.priority !== "number" || Number.isNaN(opts.priority))) {
    throw new Error(`槽位「${slot}」的 priority 须为数字`);
  }
  assertSlotDeclared(slot, cardinality, payload);
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
    priority: opts?.priority ?? 0,
    payload,
  });
  return () => unregisterSlot(id);
}

/** 槽装饰器注册通用 opts（priority/id）。 */
export interface SlotDecoratorRegisterOptions {
  priority?: number;
  /** 覆盖默认装饰器 id（缺省 `<pluginId>:decorate:<slot>`）。 */
  id?: string;
}

/** 槽位装饰校验：未声明即抛错（附近似槽名提示），声明 `decoratable: false` 即抛错
 *  （结构敏感位置不允许被包裹——包裹会破坏其渲染契约）。抛错随插件 apply 传播 → 行标 failed。 */
function assertSlotDecoratable(slot: string): void {
  const decl = findSlotDeclaration(slot);
  if (!decl) {
    const hints = suggestSlotNames(slot);
    const hint = hints.length > 0 ? `；是否想注册：${hints.join("、")}` : "（宿主未渲染该位置）";
    throw new Error(`未声明的槽位「${slot}」${hint}`);
  }
  if (decl.decoratable === false) {
    throw new Error(`槽位「${slot}」不可被装饰`);
  }
}

/** 注册槽装饰器；返回撤销函数。默认 id = `<pluginId>:decorate:<slot>`；同插件同槽重复注册自动加序去重。 */
export function registerSlotDecoratorFor(
  slot: string,
  pluginId: string,
  wrapper: ComponentType<{ children?: ReactNode }>,
  opts?: SlotDecoratorRegisterOptions,
): () => void {
  if (opts?.priority !== undefined && (typeof opts.priority !== "number" || Number.isNaN(opts.priority))) {
    throw new Error(`槽位「${slot}」的 priority 须为数字`);
  }
  assertSlotDecoratable(slot);
  const base = opts?.id ?? `${pluginId}:decorate:${slot}`;
  let id = base;
  let n = 1;
  while (decorators.has(id)) id = `${base}:${n++}`;
  registerSlotDecorator({
    id,
    pluginId,
    slot,
    priority: opts?.priority ?? 0,
    wrapper,
  });
  return () => unregisterSlotDecorator(id);
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

/** UI 区域槽载荷（titlebar/toolbar/panelhead/settings/statusbar 等具名槽位）。 */
export interface UiSlotPayload {
  /** 渲染组件（无 props 契约）。 */
  component: ComponentType;
}

/** UI 区域槽（titlebar/toolbar/panelhead/settings/statusbar 等）：默认 list（多贡献有序），可指定 single
 *  （替换类槽位如 empty/<viewKind>）。右键菜单项不走此 API（载荷不同），见 slotsApi 的 registerMenu。 */
export function registerUiSlot(
  slot: string,
  pluginId: string,
  payload: UiSlotPayload,
  opts?: SlotRegisterOptions,
): () => void {
  // cardinality 缺省 list（undefined 显式传入也归 list）：single 槽须声明表基数为 single 且显式指定。
  return registerSlotContrib(slot, pluginId, payload, { ...opts, cardinality: opts?.cardinality ?? "list" });
}
