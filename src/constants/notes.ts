/**
 * 笔记属性常用键名建议（属性区「添加属性」键名输入时下拉提示）：
 * 覆盖内置徽章键（tags/aliases/cssclasses）与常见元数据键；纯静态常量，前缀过滤用。
 */

/** 徽章类预置键（唯一事实来源）：类型锁定为「标签」、值恒为数组、渲染为 `#` 徽章
 * （frontmatter.ts 的 BADGE_KEYS 由此派生，勿另起一份）。 */
export const BADGE_PROPERTY_KEYS: string[] = ["tags", "aliases", "cssclasses"];

/** 笔记正文行宽上限（px）：应用级显示偏好，0 = 不限制（随面板宽度铺满，仅保留内边距）。 */
export const NOTE_LINE_WIDTH_DEFAULT = 780;
/** 行宽可设区间（越界值在设置项里钳制，防手输极端值把正文压成一条）。 */
export const NOTE_LINE_WIDTH_MIN = 320;
export const NOTE_LINE_WIDTH_MAX = 2400;

export const COMMON_PROPERTY_KEYS: string[] = [
  ...BADGE_PROPERTY_KEYS,
  "status",
  "priority",
  "type",
  "author",
  "source",
  "url",
  "created",
  "updated",
  "summary",
];
