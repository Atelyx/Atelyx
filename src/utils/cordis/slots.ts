/**
 * slots 代数纯核心（cardinality × scope × priority）。
 *
 * 槽 = 具名渲染/扩展位置（如 "view/canvas"）；贡献按 (cardinality, scope, priority) 解析：
 * - single：最高 priority 胜出（同优先级后注册者胜，last-wins 覆盖语义）；
 * - list：按 priority 降序全量提供。
 * scope（root/vault）为挂载范围标记，M1 仅 root；纯函数无依赖，可直测。
 */
export type SlotCardinality = "single" | "list";

export type SlotScope = "root" | "vault";

/** 单个槽贡献（payload 由注册方自定义，如视图载荷）。 */
export interface SlotContribution<TPayload = unknown> {
  /** 贡献 id（注册方保证唯一；如 `<pluginId>:<slotId>`）。 */
  id: string;
  pluginId: string;
  /** 槽名（如 "view/canvas"）。 */
  slot: string;
  cardinality: SlotCardinality;
  scope: SlotScope;
  /** 优先级（higher wins）；同优先级 single 槽后注册者胜。 */
  priority: number;
  payload: TPayload;
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
