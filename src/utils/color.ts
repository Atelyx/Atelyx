/**
 * 强调色工具：自定义强调色（仓库级）由 App 层动态覆盖 CSS 变量时，
 * `--accent-hover`（暗化）与 `--accent-fg`（对比前景）无法直接由用户输入推导，
 * 统一在此计算（输入 hex，输出 hex；非法输入回退默认金色）。
 */

/** 品牌黄铜金默认强调色（与 styles/index.css 的 --accent 同源）。 */
export const DEFAULT_ACCENT = "#e0a94e";
/** 自定义强调色的 hover 暗化系数（内置金的 hover 由主题变量各自定义，不走此处）。 */
const DARKEN_FACTOR = 0.86;

/** 前景阈值：相对亮度高于此值时底上用深色文字（默认金用深字），否则白字。 */
const FOREGROUND_LUMINANCE_THRESHOLD = 0.4;

/** 解析 `#rrggbb` 为 [r, g, b]（0-255）；非法输入返回品牌金。 */
function parseHex(hex: string): [number, number, number] {
  if (!/^#[0-9a-fA-F]{6}$/.test(hex)) return [224, 169, 78];
  return [
    parseInt(hex.slice(1, 3), 16),
    parseInt(hex.slice(3, 5), 16),
    parseInt(hex.slice(5, 7), 16),
  ];
}

function toHex(r: number, g: number, b: number): string {
  return `#${[r, g, b].map((v) => Math.round(v).toString(16).padStart(2, "0")).join("")}`;
}

/** 暗化强调色（hover 态用，系数 0.86 对齐内置金的 hover 档）。 */
export function darkenHex(hex: string): string {
  const [r, g, b] = parseHex(hex);
  return toHex(r * DARKEN_FACTOR, g * DARKEN_FACTOR, b * DARKEN_FACTOR);
}

/** 向白色混合（深色主题的 hover 需比本色更亮，与浅色主题的 darkenHex 方向相反）。 */
export function lightenHex(hex: string, amount = 0.28): string {
  const [r, g, b] = parseHex(hex);
  const mix = (v: number) => v + (255 - v) * amount;
  return toHex(mix(r), mix(g), mix(b));
}

/** 强调色的半透明变体（`--accent-soft` / `--focus-ring` 等）：必须跟随强调色，
 *  否则用户换成非金色后，选中底/焦点环仍是内置金，色相不匹配。 */
export function withAlpha(hex: string, alpha: number): string {
  const [r, g, b] = parseHex(hex);
  return `rgba(${r}, ${g}, ${b}, ${alpha})`;
}

/**
 * 强调色底上的前景文字色：按 WCAG 相对亮度判定——亮度高（如金色）用深色文字保证对比度，
 * 亮度低用白字。
 */
export function foregroundFor(hex: string): string {
  const [r, g, b] = parseHex(hex).map((v) => v / 255);
  const linear = (c: number) =>
    c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  const luminance = 0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b);
  return luminance > FOREGROUND_LUMINANCE_THRESHOLD ? "#1c1c1e" : "#ffffff";
}
