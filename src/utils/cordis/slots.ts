/**
 * slots 代数纯核心（cardinality × priority + decorate）。
 *
 * 槽 = 具名渲染/扩展位置（如 "view/canvas"、"toolbar/note/right"、"settings/theme"）。
 * 贡献按 (cardinality, priority) 解析：
 * - single：最高 priority 胜出（同优先级后注册者胜，last-wins 覆盖语义）；
 * - list：按 priority 降序全量提供（UI 区域多贡献有序）。
 * 装饰器（decorate）按 priority 降序成链，外层 = 高 priority，包裹槽的渲染内容。
 * 纯函数无依赖，可直测。
 */
import type { ComponentType, ReactNode } from "react";

export type SlotCardinality = "single" | "list";

/** 单个槽贡献（payload 由注册方自定义，如视图载荷）。 */
export interface SlotContribution<TPayload = unknown> {
  /** 贡献 id（注册方保证唯一；如 `<pluginId>:<slotId>`）。 */
  id: string;
  pluginId: string;
  /** 槽名（如 "view/canvas"）。 */
  slot: string;
  cardinality: SlotCardinality;
  /** 优先级（higher wins）；同优先级 single 槽后注册者胜。 */
  priority: number;
  payload: TPayload;
}

/** 槽装饰器（ctx.slots.decorate 注册：包裹某槽渲染内容的 React 组件）。
 *  wrapper 必须渲染 children——吞掉 children 即「吞掉宿主 UI」，宿主校验后剔除该装饰器。 */
export interface SlotDecorator {
  /** 装饰器 id（注册方保证唯一；如 `<pluginId>:decorate:<slot>`）。 */
  id: string;
  pluginId: string;
  /** 槽名（与贡献同一命名空间）。 */
  slot: string;
  /** 优先级（higher wins）；外层 = 高 priority。 */
  priority: number;
  /** 包裹组件（必须渲染 children）。 */
  wrapper: ComponentType<{ children?: ReactNode }>;
}

/**
 * single 槽胜出解析：最高 priority 胜；同优先级取数组靠后（后注册者，last-wins）。
 * 空贡献返回 undefined。
 */
export function pickSlotWinner<TPayload>(contribs: readonly SlotContribution<TPayload>[]): SlotContribution<TPayload> | undefined {
  if (contribs.length === 0) return undefined;
  return contribs.reduce((best, c) => (c.priority >= best.priority ? c : best));
}

/** list 槽排序：priority 降序（同优先级保持注册序）。 */
export function sortSlotList<TPayload>(contribs: readonly SlotContribution<TPayload>[]): SlotContribution<TPayload>[] {
  return [...contribs].sort((a, b) => b.priority - a.priority);
}

/** 装饰器链排序：priority 降序（同优先级保持注册序；外层 = 高 priority）。 */
export function sortSlotDecorators(decorators: readonly SlotDecorator[]): SlotDecorator[] {
  return [...decorators].sort((a, b) => b.priority - a.priority);
}
