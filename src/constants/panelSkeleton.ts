/**
 * 面板骨架的形状与首帧（内联骨架 + 首帧主题缓存）：形状常量供 React 骨架与首帧骨架共用（一处一定义），
 * 首帧骨架由 vite 注入 index.html——WebView 内核装载到 React 挂载之间没有任何页面 CSS/JS 可用。
 */
/** 面板骨架的纵向档位（rem；与真 chrome 同档位：标题栏 h-9 = 2.25rem、标签条 h-7 = 1.75rem）。
 *  用 rem 不用 px：根字号是应用级「字体大小」设置的落点，px 不随它缩放会与真 chrome 错位。 */
export const PANEL_SKELETON_TITLEBAR_REM = 2.25;
export const PANEL_SKELETON_TABBAR_REM = 1.75;
/** 标签占位宽（px）与内容行宽（百分比）。 */
export const PANEL_SKELETON_TAB_PILLS: readonly number[] = [64, 46];
export const PANEL_SKELETON_CONTENT_ROWS: readonly number[] = [34, 76, 58, 68, 44];

/** 首帧骨架元素 id（App 首个提交时移除；仍在 = React 未挂载）。 */
export const BOOT_SPLASH_ID = "boot-splash";
/** 首帧骨架的动画名（首帧只能自带一份，与 React 骨架的扫光同为 1.6s 线性往返）。 */
const BOOT_SPLASH_SWEEP = "boot-splash-sweep";

/** 首帧主题缓存键（localStorage）：页面首帧同步取不到配置（要等 JS 与插件就绪），缓存上一次解析出的
 *  深浅供 `index.html` 内联脚本取用；写入方 `useAppearance`（主题解析完成时），语义 = 派生缓存，
 *  真相仍是配置与主题插件。键名在 `index.html` 里也出现一次（静态首帧只能写死），契约测试锁同值。 */
export const BOOT_SCHEME_KEY = "atelyx:bootScheme";

/** 写入首帧主题缓存（存储不可用时静默跳过——它只是首帧的近似，失败不该影响主题应用）。 */
export function cacheBootScheme(scheme: "light" | "dark"): void {
  try {
    localStorage.setItem(BOOT_SCHEME_KEY, scheme);
  } catch {
    /* 存储被禁用/配额满：首帧退回系统深浅 */
  }
}

/** 首帧配色 = index.css 主题基色（`:root` 浅色 / `:root.dark` 深色），契约测试锁同值。 */
const SPLASH_COLORS = {
  dark: { bg: "#131418", surface: "#1b1e23", line: "#2e333a", block: "#23272e", sweep: "rgba(255,255,255,0.055)" },
  light: { bg: "#eae9e3", surface: "#f5f4f0", line: "#d3d1c9", block: "#e4e3dc", sweep: "rgba(0,0,0,0.05)" },
} as const;

/**
 * 首帧骨架样式：只对撕裂窗口生效（`html.boot-panel`，角色由 index.html 头部脚本按窗口 label 判定）
 * ——主窗口启动期走加载屏，形状不同，铺面板骨架只会多一次突变。配色按 `.dark` 分支取深浅两套。
 * 扫光用中性色而非 `--accent`：强调色来自主题插件变量，首帧拿不到；窄条上的色差肉眼不可辨。
 */
export function bootSplashStyle(): string {
  const theme = (scheme: "dark" | "light", selector: string): string => {
    const c = SPLASH_COLORS[scheme];
    return [
      `${selector} #${BOOT_SPLASH_ID}{background:${c.bg}}`,
      `${selector} .bs-title{background:${c.surface};border-bottom:1px solid ${c.line}}`,
      `${selector} .bs-tabs{background:${c.surface};border-bottom:1px solid ${c.line}}`,
      `${selector} .bs-title i,${selector} .bs-tabs i,${selector} .bs-body i{background:${c.block}}`,
      `${selector} .bs-body i::after{background-image:linear-gradient(100deg,transparent 20%,${c.sweep} 50%,transparent 80%)}`,
    ].join("");
  };
  return [
    // box-sizing：首帧没有 Tailwind preflight（它随样式表下发），不显式声明则 border 会撑高 1px、
    // 与 React 骨架（preflight 下 border-box）错位
    `#${BOOT_SPLASH_ID},#${BOOT_SPLASH_ID} *{box-sizing:border-box}`,
    `#${BOOT_SPLASH_ID}{display:none;position:fixed;inset:0;z-index:999}`,
    `html.boot-panel #${BOOT_SPLASH_ID}{display:flex;flex-direction:column}`,
    `#${BOOT_SPLASH_ID} .bs-title{height:${PANEL_SKELETON_TITLEBAR_REM}rem;flex:none;display:flex;align-items:center;padding:0 8px}`,
    `#${BOOT_SPLASH_ID} .bs-title i{width:56px;height:10px;border-radius:4px}`,
    `#${BOOT_SPLASH_ID} .bs-tabs{height:${PANEL_SKELETON_TABBAR_REM}rem;flex:none;display:flex;align-items:center;gap:4px;padding:0 6px}`,
    `#${BOOT_SPLASH_ID} .bs-tabs i{height:14px;border-radius:4px}`,
    `#${BOOT_SPLASH_ID} .bs-body{flex:1 1 auto;display:flex;flex-direction:column;gap:10px;padding:16px 20px;overflow:hidden}`,
    `#${BOOT_SPLASH_ID} .bs-body i{position:relative;height:12px;border-radius:4px;overflow:hidden}`,
    `#${BOOT_SPLASH_ID} .bs-body i::after{content:"";position:absolute;inset:0;background-size:220% 100%;animation:${BOOT_SPLASH_SWEEP} 1.6s linear infinite}`,
    `@keyframes ${BOOT_SPLASH_SWEEP}{from{background-position:220% 0}to{background-position:-120% 0}}`,
    theme("light", "html.boot-panel:not(.dark)"),
    theme("dark", "html.boot-panel.dark"),
  ].join("");
}

/** 首帧骨架标记：形状与 React 骨架同源（标签占位宽 + 内容行宽两组常量）。 */
export function bootSplashMarkup(): string {
  const pills = PANEL_SKELETON_TAB_PILLS.map((w) => `<i style="width:${w}px"></i>`).join("");
  const rows = PANEL_SKELETON_CONTENT_ROWS.map((w) => `<i style="width:${w}%"></i>`).join("");
  return `<div class="bs-title"><i></i></div><div class="bs-tabs">${pills}</div><div class="bs-body">${rows}</div>`;
}
