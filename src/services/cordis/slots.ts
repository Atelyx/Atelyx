/**
 * slots 运行时注册表：具名槽的贡献收集与解析（纯代数见 utils/cordis/slots）。
 *
 * 可注册的槽位以 constants/slots.ts 的声明表为宿主侧唯一清单：固定具名槽（titlebar/toolbar/
 * panelhead/statusbar/settings）与右键菜单目标（contextmenu/<target>）未声明即注册失败并附近似槽名
 * 提示；开放 kind 槽（view/node/edge/tableview/empty/inspector）按前缀放行。
 * 插件可经 ctx.slots.declare 声明自有槽位（先到先得 + 宿主保护，运行时声明注册表在本模块）；
 * 对宿主槽与插件声明槽的贡献注册路径相同，仅声明校验来源不同。
 * 随应用分发的视图/节点/边 = 对应默认组合成员挂载时注册的 single 槽贡献；注册经 ctx.effect 随 fiber 撤销。
 */
import type { ComponentType, ReactNode } from "react";
import { VIEW_LABELS } from "@/constants/views";
import { findSlotDeclaration, slotPayloadShape, SLOT_DECLARATIONS } from "@/constants/slots";
import type { SlotDeclaration } from "@/constants/slots";
import type { SlotCardinality, SlotContribution, SlotDecorator } from "@/utils/cordis/slots";
import { pickSlotWinner, sortSlotDecorators, sortSlotList } from "@/utils/cordis/slots";
import type { PluginSlotChain, SlotConflictRow } from "@/types/plugin";

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
  const pinned = pinnedWinner(slot);
  if (pinned) return pinned;
  const list = [...contributions.values()].filter((c) => c.slot === slot);
  return pickSlotWinner(list);
}

// ===== single 槽胜者用户覆盖（设置 → 插件槽位冲突裁决注入；默认关闭 = 纯 priority 决胜） =====

/** 覆盖源（槽 → 钉住的贡献 id）：由 pluginStore 接线时注入（读应用级 ui-state）。
 *  services 层不直接依赖 store，与 access.ts 注入点同模式；测试可直设后置空。 */
type SlotWinnerOverrideSource = (slot: string) => string | null;
let slotWinnerOverrideSource: SlotWinnerOverrideSource | null = null;

/** 注入/清除胜者覆盖源（null = 关闭覆盖，回退 priority 决胜）。 */
export function setSlotWinnerOverrideSource(source: SlotWinnerOverrideSource | null): void {
  slotWinnerOverrideSource = source;
}

/** 钉住的胜者：覆盖源返回的贡献 id 仍在注册且同槽才生效（被钉者卸载后自动回退 priority）。 */
function pinnedWinner(slot: string): SlotContribution | undefined {
  const pinnedId = slotWinnerOverrideSource?.(slot);
  if (!pinnedId) return undefined;
  const contrib = contributions.get(pinnedId);
  return contrib && contrib.slot === slot ? contrib : undefined;
}

/** list 槽解析：全部贡献按 priority 降序。 */
export function listSlot(slot: string): SlotContribution[] {
  return sortSlotList([...contributions.values()].filter((c) => c.slot === slot));
}

/** 全部已注册槽名（视图菜单/调试用）。 */
export function registeredSlots(): string[] {
  return [...new Set([...contributions.values()].map((c) => c.slot))];
}

/** 全部槽贡献快照（按插件聚合/治理展示用；随 fiber 撤销的贡献自然不在列）。 */
export function listAllContributions(): SlotContribution[] {
  return [...contributions.values()];
}

/** 全部槽装饰器快照（按插件聚合/治理展示用）。 */
export function listAllDecorators(): SlotDecorator[] {
  return [...decorators.values()];
}

/** 载荷展示标签：取 payload 的字符串 label（component 等函数载荷不展示）；无 = undefined。
 *  审计槽位面与治理展示共用同一提取规则。 */
export function payloadLabel(payload: unknown): string | undefined {
  const label = (payload as { label?: unknown } | null)?.label;
  return typeof label === "string" ? label : undefined;
}

/** 运行时声明的占用者插件 id（精确 key 或最长前缀命中；无 = null）。 */
function runtimeDeclarerOf(slot: string): string | null {
  const exact = [...runtimeDeclarations.values()].find((r) => !r.decl.prefix && r.decl.key === slot);
  if (exact) return exact.pluginId;
  const prefixed = [...runtimeDeclarations.values()]
    .filter((r) => r.decl.prefix === true && slot.startsWith(`${r.decl.key}/`) && slot.length > r.decl.key.length + 1)
    .sort((a, b) => b.decl.key.length - a.decl.key.length)[0];
  return prefixed?.pluginId ?? null;
}

/** 槽位声明方：静态声明表命中 = 宿主；否则运行时声明插件 id（无声明 = 宿主兜底）。 */
export function slotDeclarer(slot: string): string {
  return findSlotDeclaration(slot) ? "宿主" : (runtimeDeclarerOf(slot) ?? "宿主");
}

/** 槽位修改链（归属可见）：声明方 + 全部贡献/装饰者，贡献/装饰各自按 priority 降序。 */
export function slotChain(slot: string): PluginSlotChain {
  return {
    slot,
    declarer: slotDeclarer(slot),
    contributors: sortSlotList([...contributions.values()].filter((c) => c.slot === slot)).map((c) => ({
      id: c.id,
      pluginId: c.pluginId,
      priority: c.priority,
      label: payloadLabel(c.payload),
    })),
    decorators: listDecorators(slot).map((d) => ({ id: d.id, pluginId: d.pluginId, priority: d.priority })),
  };
}

/** single 槽冲突清单（设置 → 插件冲突裁决）：声明基数为 single 且贡献 ≥2 才出。
 *  winner = 钉住命中否则 priority 胜出（与 resolveSlot 同口径）；pins 由调用方从应用级 ui-state 传入。 */
export function slotConflictRows(pins: Record<string, string>): SlotConflictRow[] {
  const bySlot = new Map<string, SlotContribution[]>();
  for (const c of contributions.values()) {
    const arr = bySlot.get(c.slot);
    if (arr) arr.push(c);
    else bySlot.set(c.slot, [c]);
  }
  const rows: SlotConflictRow[] = [];
  for (const [slot, contribs] of bySlot) {
    const decl = findSlotDeclarationRuntime(slot);
    if (!decl || decl.cardinality !== "single" || contribs.length < 2) continue;
    // 槽名是插件任意字符串，读钉住须 hasOwn（`pins[slot]` 会命中原型链，如 "constructor"）。
    const pinnedId = Object.hasOwn(pins, slot) ? pins[slot] : null;
    const pinnedActive = pinnedId !== null && contribs.some((c) => c.id === pinnedId);
    rows.push({
      slot,
      declarer: slotDeclarer(slot),
      contributors: sortSlotList(contribs).map((c) => ({
        id: c.id,
        pluginId: c.pluginId,
        priority: c.priority,
        label: payloadLabel(c.payload),
      })),
      pinnedId,
      winnerId: pinnedActive ? pinnedId : (pickSlotWinner(contribs)?.id ?? null),
    });
  }
  return rows.sort((a, b) => (a.slot < b.slot ? -1 : 1));
}

/** 全部已注册的视图 kind（槽名剥 `view/` 前缀；视图菜单/选择器用）。 */
export function viewKinds(): string[] {
  const prefix = "view/";
  return registeredSlots()
    .filter((s) => s.startsWith(prefix))
    .map((s) => s.slice(prefix.length));
}

// ===== 插件自声明槽位（ctx.slots.declare：运行时声明，先到先得 + 宿主保护） =====

/** 运行时槽位声明条目（含占用者；声明经 fiber 撤销，冲突指名占用者用）。 */
interface RuntimeDeclaration {
  decl: SlotDeclaration;
  pluginId: string;
}

/** 运行时声明注册表（key → 声明；静态声明表是宿主专属，插件的扩展点全部走这里）。 */
const runtimeDeclarations = new Map<string, RuntimeDeclaration>();

/** 两段声明覆盖集是否相交：精确 key 覆盖 {key}，前缀 key 覆盖 {key/<非空>}。
 *  相交即冲突——声明方不得吞并他人（含宿主）已占的槽位。 */
function declarationSetsOverlap(aKey: string, aPrefix: boolean, bKey: string, bPrefix: boolean): boolean {
  if (!aPrefix && !bPrefix) return aKey === bKey;
  if (!aPrefix && bPrefix) return aKey.startsWith(`${bKey}/`);
  if (aPrefix && !bPrefix) return bKey.startsWith(`${aKey}/`);
  return aKey === bKey || aKey.startsWith(`${bKey}/`) || bKey.startsWith(`${aKey}/`);
}

/** 槽位声明参数（ctx.slots.declare；key 未被宿主或他插件占用即可声明）。 */
export interface SlotDeclareOptions {
  /** 槽名或前缀名（如 "toolbar/timeline/play"；prefix=true 时写 "toolbar/timeline"）。 */
  key: string;
  /** 前缀放行（`<key>/<任意非空>` 均合法，仿宿主开放 kind 槽语义）。 */
  prefix?: boolean;
  cardinality: SlotCardinality;
  /** 必需载荷字段（贡献方须提供；应含 "component" 才能被 host(slot) 渲染）。 */
  required: readonly string[];
  /** 可选载荷字段（与 required 不重叠）。 */
  optional?: readonly string[];
  /** 渲染位置（缺省 = 插件声明）。 */
  scope?: string;
  /** 用途说明（仅当比 scope 多出信息时才写）。 */
  summary?: string;
  /** 是否可被装饰（缺省可装饰）。 */
  decoratable?: boolean;
}

/** 声明 key 形态校验：非空、无首尾斜杠与连续斜杠（槽名 = 路径段序列，host 分派依赖该形态）。 */
function validateDeclareKey(key: string): string {
  if (typeof key !== "string" || key.length === 0) throw new Error("槽位声明需要非空 key");
  if (key.startsWith("/") || key.endsWith("/") || key.includes("//")) {
    throw new Error(`槽位 key「${key}」不能以斜杠开头/结尾或含连续斜杠`);
  }
  return key;
}

/** 字段名集合校验：全字符串非空、去重、不与另一清单重叠（空集合法——可选清单可缺省）。 */
function validateDeclareFields(key: string, label: string, fields: readonly string[], other: ReadonlySet<string>): string[] {
  const seen = new Set<string>();
  for (const f of fields) {
    if (typeof f !== "string" || f.length === 0) {
      throw new Error(`槽位「${key}」的${label}字段名须为非空字符串`);
    }
    if (seen.has(f)) throw new Error(`槽位「${key}」的${label}字段重复：${f}`);
    if (other.has(f)) throw new Error(`槽位「${key}」的${label}字段与另一清单重叠：${f}`);
    seen.add(f);
  }
  return [...seen];
}

/**
 * 声明插件自有的槽位（先到先得）：未被宿主或他插件占用即可声明；宿主已声明的 key 与开放前缀受保护。
 * 返回撤销函数（停用插件经 fiber 撤销声明；对声明槽的贡献与声明各自独立撤销）。
 */
export function declareSlot(opts: SlotDeclareOptions, pluginId: string): () => void {
  const key = validateDeclareKey(opts.key);
  const prefix = opts.prefix === true;
  if (opts.cardinality !== "single" && opts.cardinality !== "list") {
    throw new Error(`槽位「${key}」的基数须为 single 或 list`);
  }
  if (!Array.isArray(opts.required) || opts.required.length === 0) {
    throw new Error(`槽位「${key}」的声明需要至少一个必需字段`);
  }
  const optional = validateDeclareFields(key, "可选", opts.optional ?? [], new Set());
  const required = validateDeclareFields(key, "必需", opts.required, new Set(optional));
  // 宿主保护：覆盖集与宿主声明（含开放前缀）相交即拒绝——插件不得吞并宿主座位或侵入开放前缀。
  for (const host of SLOT_DECLARATIONS) {
    if (declarationSetsOverlap(key, prefix, host.key, host.prefix === true)) {
      throw new Error(`槽位「${key}」与宿主槽位「${host.key}」重叠，宿主槽位受保护`);
    }
  }
  // 先到先得：与既有插件声明相交即拒绝并指名占用者（开发者据此改名或协商）。
  for (const [, owner] of runtimeDeclarations) {
    if (declarationSetsOverlap(key, prefix, owner.decl.key, owner.decl.prefix === true)) {
      throw new Error(`槽位「${key}」已被插件 ${owner.pluginId} 声明`);
    }
  }
  const decl: SlotDeclaration = Object.freeze({
    key,
    ...(prefix ? { prefix: true } : {}),
    cardinality: opts.cardinality,
    required: Object.freeze(required),
    ...(optional.length > 0 ? { optional: Object.freeze(optional) } : {}),
    scope: opts.scope ?? `插件声明：${pluginId}`,
    ...(opts.summary ? { summary: opts.summary } : {}),
    ...(opts.decoratable === false ? { decoratable: false } : {}),
  });
  runtimeDeclarations.set(key, { decl, pluginId });
  return () => undeclareSlot(key);
}

/** 撤销运行时槽位声明（幂等；key 不存在 = no-op）。 */
export function undeclareSlot(key: string): void {
  runtimeDeclarations.delete(key);
}

/** 合并解析槽声明：静态声明表优先，未命中再查运行时声明（精确 key 与前缀放行两种形态）。
 *  运行时声明不与宿主声明重叠（declareSlot 已拒绝），故静态命中即可短路。 */
export function findSlotDeclarationRuntime(slot: string): SlotDeclaration | undefined {
  const staticDecl = findSlotDeclaration(slot);
  if (staticDecl) return staticDecl;
  const exact = [...runtimeDeclarations.values()].find((r) => !r.decl.prefix && r.decl.key === slot);
  if (exact) return exact.decl;
  // 前缀匹配：多个运行时前缀互不重叠（先到先得保证），取最长命中（防御性确定）。
  return [...runtimeDeclarations.values()]
    .filter((r) => r.decl.prefix === true && slot.startsWith(`${r.decl.key}/`) && slot.length > r.decl.key.length + 1)
    .map((r) => r.decl)
    .sort((a, b) => b.key.length - a.key.length)[0];
}

/** 全部槽位声明（静态声明表 + 插件运行时声明）的冻结只读视图；`ctx.slots.list()` 据此暴露。 */
export function allSlotDeclarations(): readonly SlotDeclaration[] {
  return Object.freeze([...SLOT_DECLARATIONS, ...[...runtimeDeclarations.values()].map((r) => r.decl)]);
}

/** 近似槽名提示（静态 + 运行时声明合并，统一按公共前缀排序；注册失败时的可读原因用）。 */
export function suggestSlotNamesRuntime(slot: string, limit = 3): string[] {
  const head = slot.slice(0, Math.max(0, slot.indexOf("/")));
  const take = Math.max(0, Math.trunc(limit));
  // 静态候选（非前缀声明）与运行时 key 合并，公共前缀更长的排前——运行时槽名拼错也要能提示到。
  const candidates = new Set<string>([
    ...SLOT_DECLARATIONS.filter((d) => !d.prefix && d.key.startsWith(`${head}/`)).map((d) => d.key),
    ...[...runtimeDeclarations.keys()].filter((k) => k !== slot && k.startsWith(`${head}/`)),
  ]);
  return [...candidates]
    .sort((a, b) => commonPrefixLength(b, slot) - commonPrefixLength(a, slot))
    .slice(0, take);
}

/** 两串公共前缀长度（运行时槽名提示排序用）。 */
function commonPrefixLength(a: string, b: string): number {
  const max = Math.min(a.length, b.length);
  let i = 0;
  while (i < max && a[i] === b[i]) i += 1;
  return i;
}

/** 槽注册通用 opts（cardinality/priority/id）。 */
export interface SlotRegisterOptions {
  cardinality?: SlotCardinality;
  priority?: number;
  /** 覆盖默认贡献 id（缺省 `<pluginId>:<slot>`）。 */
  id?: string;
}

/** 槽位声明校验：未声明（宿主或插件声明均计）即抛错（附近似槽名提示），基数与载荷不符即抛错。
 *  抛错随插件 apply 传播 → 挂载器标记该行 failed + 可读原因（不静默丢失）。 */
function assertSlotDeclared(slot: string, cardinality: SlotCardinality, payload: unknown): void {
  const decl = findSlotDeclarationRuntime(slot);
  if (!decl) {
    const hints = suggestSlotNamesRuntime(slot);
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

/** 槽位装饰校验：未声明（宿主或插件声明均计）即抛错（附近似槽名提示），声明 `decoratable: false`
 *  即抛错（结构敏感位置不允许被包裹——包裹会破坏其渲染契约）。抛错随插件 apply 传播 → 行标 failed。 */
function assertSlotDecoratable(slot: string): void {
  const decl = findSlotDeclarationRuntime(slot);
  if (!decl) {
    const hints = suggestSlotNamesRuntime(slot);
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
