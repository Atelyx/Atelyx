/**
 * 内置「极光」皮肤（默认主题插件的一个深色条目）的配色变量。
 * 键名 = `styles/index.css` 的主题变量契约（省略 `--` 前缀亦可）；由 useAppearance 内联写到
 * :root 覆盖同名变量，未列出的变量沿用内置深色基底。底色半透明，让 `--app-backdrop`
 * 的页面极光光晕从面板下透出形成玻璃感。
 */
export const AURORA_THEME_ID = "aurora";

export const AURORA_THEME_NAME = "极光";

export const AURORA_THEME_VARIABLES: Record<string, string> = {
  /* 背景色阶：半透明档，让 body 的极光氛围底透上来（相邻档仍靠位移逐级抬亮）；
     正文所在的面叠在 html 实底与氛围底之上，透明度控制在不影响正文对比的范围内 */
  "--bg-sunken": "rgba(6, 8, 13, 0.52)",
  "--bg-primary": "rgba(10, 14, 21, 0.55)",
  "--bg-secondary": "rgba(18, 22, 31, 0.45)",
  "--bg-tertiary": "rgba(26, 31, 42, 0.5)",
  "--bg-card": "rgba(18, 22, 31, 0.45)",
  "--bg-overlay": "rgba(20, 26, 36, 0.88)",
  "--surface": "rgba(18, 22, 31, 0.45)",
  "--scrim": "rgba(2, 4, 8, 0.62)",
  /* html 实底：半透明面之下的最后一块底色 */
  "--app-base": "#070A11",

  /* 文本 */
  "--text-primary": "#E7EAF0",
  "--text-secondary": "#A6ADBB",
  "--text-muted": "#7B8394",

  /* 边与交互态：hover 用抬白而非实色，叠在半透明面上才有「浮起」观感 */
  "--border-subtle": "#1B2130",
  "--border": "#262D3C",
  "--border-strong": "#353D4E",
  "--hover": "rgba(255, 255, 255, 0.05)",

  /* 表单与滚动条 */
  "--input-bg": "rgba(255, 255, 255, 0.04)",
  "--input-border": "#353D4E",
  "--input-placeholder": "#7B8394",
  "--scrollbar-thumb": "#262D3C",
  "--scrollbar-thumb-hover": "#353D4E",

  /* 强调：极光青（青绿相）。渐变与辉光都从 --accent 派生——用户改强调色后整族一起跟随，
     不会留下与强调色色相错配的皮肤原色；深底上提亮一档同时抬高 --accent-fg 的对比 */
  "--accent": "#3FB4A4",
  "--accent-hover": "#4FC7B7",
  "--accent-fg": "#04231F",
  "--accent-soft": "rgba(63, 180, 164, 0.14)",
  "--focus-ring": "0 0 0 2px rgba(63, 180, 164, 0.34)",
  "--accent-grad": "linear-gradient(135deg, var(--accent), color-mix(in srgb, var(--accent) 78%, white))",
  "--accent-glow": "0 0 22px color-mix(in srgb, var(--accent) 26%, transparent)",

  /* 状态语义色（作正文用须 ≥4.5:1） */
  "--success": "#5FBF8B",
  "--warning": "#D9A94F",
  "--danger": "#E8796C",
  "--danger-fill": "#C8564A",
  "--info": "#6FA8DC",

  /* 标题栏窗口控制按钮 */
  "--titlebar-icon": "#A6ADBB",
  "--titlebar-hover": "rgba(255, 255, 255, 0.07)",
  "--titlebar-close-hover": "#E81123",

  /* Markdown 与代码：代码块保持近实底，正文嵌在玻璃面上仍要读得清 */
  "--highlight-bg": "rgba(255, 255, 255, 0.07)",
  "--highlight-text": "#E7EAF0",
  "--highlight-code": "color-mix(in srgb, var(--accent) 88%, white)",
  "--link-internal": "color-mix(in srgb, var(--accent) 88%, white)",
  "--callout-bg": "rgba(255, 255, 255, 0.05)",
  "--code-bg": "#04060A",
  "--table-border": "#262D3C",
  "--table-header-bg": "rgba(255, 255, 255, 0.04)",

  /* 圆角：整条刻度较默认深色放大一档（行 6→8、卡片 10→12、弹窗 14→16） */
  "--radius-xs": "5px",
  "--radius-sm": "8px",
  "--radius-bubble": "10px",
  "--radius-md": "12px",
  "--radius-lg": "16px",

  /* 字体：Sora（显示）/ Manrope（界面）/ JetBrains Mono（等宽），中文回退系统 CJK */
  "--font-sans":
    '"Manrope", "Noto Sans SC", "Source Han Sans SC", "PingFang SC", "Microsoft YaHei", system-ui, sans-serif',
  "--font-display": '"Sora", var(--font-sans)',
  "--font-mono": '"JetBrains Mono", ui-monospace, SFMono-Regular, Menlo, Consolas, monospace',

  /* 页面氛围底（铺在 body 上）：青/蓝两团极光光晕，面板半透明后从底下透出 */
  "--app-backdrop":
    "radial-gradient(900px 620px at 16% -12%, rgba(63, 180, 164, 0.20), transparent 62%), radial-gradient(880px 680px at 104% 14%, rgba(62, 147, 181, 0.17), transparent 62%)",

  /* 浮层玻璃：仅给漂浮在内容之上的面（菜单/下拉/通知）模糊，面板自身靠半透明透光 */
  "--glass-filter": "blur(16px) saturate(140%)",
};
