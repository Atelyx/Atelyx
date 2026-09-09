/**
 * slots 运行时注册表：具名槽的贡献收集与解析（纯代数见 utils/cordis/slots）。
 *
 * 视图槽命名 `view/<kind>`（M1 仅视图；标题栏/工具条/菜单等槽位 M3 全树化时扩展前缀）。
 * 内置视图 = 第一方插件挂载时注册的 single 槽贡献；停用/卸载经 disposePluginSlots 撤销。
 */
import type { ComponentType, ReactNode } from "react";
import { VIEW_LABELS } from "@/constants/views";
import type { SlotContribution } from "@/utils/cordis/slots";
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
  /** 按宿主面板/撕裂窗口 id 渲染（内置重型视图用；第三方面板不提供）。 */
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

/** 注册视图槽贡献（slot = `view/<kind>`）；返回撤销函数。 */
export function registerViewSlot(
  kind: string,
  pluginId: string,
  payload: ViewSlotPayload,
  opts?: { cardinality?: "single" | "list"; scope?: "root" | "vault"; priority?: number; id?: string },
): () => void {
  const id = opts?.id ?? `${pluginId}:view/${kind}`;
  registerSlot({
    id,
    pluginId,
    slot: `view/${kind}`,
    cardinality: opts?.cardinality ?? "single",
    scope: opts?.scope ?? "root",
    priority: opts?.priority ?? 0,
    payload,
  });
  return () => unregisterSlot(id);
}

/** 解析某视图 kind 的胜出贡献（ViewHost 分派用；无贡献 = undefined）。 */
export function resolveViewKind(kind: string): ViewSlotContribution | undefined {
  return resolveSlot(`view/${kind}`) as ViewSlotContribution | undefined;
}

/** 视图显示名（视图槽标签 → 内置视图标签 → 原样兜底，不崩溃）。 */
export function pluginViewLabel(view: string): string {
  return resolveViewKind(view)?.payload.label ?? (VIEW_LABELS as Record<string, string>)[view] ?? view;
}
