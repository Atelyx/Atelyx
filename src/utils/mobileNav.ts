/**
 * 移动端导航顺序（底部横栏取前若干个，其余进完整列表）。
 *
 * 顺序是应用级偏好（global.json 的 `mobileNavOrder`），存的是视图 key 数组——
 * 插件启停会改变可用视图集合，故排序只在「当前可用视图」上生效，缺失/新增的视图按
 * 保存序在前、其余保持**入参顺序**（= 组合行顺序，见 pluginViewKinds）追加，
 * 保证任何磁盘状态下都能排出稳定结果。
 */

/** 底部导航栏直接显示的视图数上限（超出部分只从「更多」进）。 */
export const MOBILE_NAV_BAR_SIZE = 5;

/** 缺省顺序（用户未自定义时）：常用视图在前，其余按入参顺序追加。 */
export const MOBILE_NAV_DEFAULT_ORDER: readonly string[] = [
  "recent",
  "note",
  "files",
  "aichat",
  "table",
];

/** 按保存顺序排列当前可用视图；savedOrder 为空 = 用缺省顺序。
 *  保存序来自用户可手改的 global.json，故逐项去重（重复 key 会让列表出现重复项与重复 React key）。 */
export function orderMobileViews(kinds: string[], savedOrder: readonly string[]): string[] {
  const available = new Set(kinds);
  const base: string[] = [];
  for (const k of savedOrder.length > 0 ? savedOrder : MOBILE_NAV_DEFAULT_ORDER) {
    if (!available.has(k) || base.includes(k)) continue;
    base.push(k);
  }
  return [...base, ...kinds.filter((k) => !base.includes(k))];
}

/** 相邻两项互换（越界原样返回）；设置页「上移/下移」用。 */
export function swapInOrder(order: readonly string[], from: number, to: number): string[] {
  if (from === to || from < 0 || to < 0 || from >= order.length || to >= order.length) {
    return [...order];
  }
  const next = [...order];
  const tmp = next[from];
  next[from] = next[to];
  next[to] = tmp;
  return next;
}
