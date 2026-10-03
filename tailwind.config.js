/** @type {import('tailwindcss').Config} */
// 颜色一律走 styles/index.css 的主题变量（组件按需写 bg-[var(--bg-secondary)] 等任意值），
// 不在此维护调色板副本——避免两套色值各自漂移。
// 圆角整条 Tailwind 刻度都指向主题变量（不再有预设裸值）：xs 4 → sm 6 → md 10 → lg 14；
// xl / 2xl / 3xl 收敛到 lg（大容器、大图标）；bubble 是聊天气泡专用档（8px）。
// 字号同理指向 styles/index.css 的字阶变量（rem，随应用级「字体大小」设置缩放）。
// 既有档位取值与 Tailwind 默认 rem 刻度一致，故换用变量后无视觉差异：
// xs 12 = caption、sm 14 = body、base 16 = h2、xl 20 = h1；
// lg(18) / 2xl(24) 在字阶里无对应档，保留字面值。
// 另补micro / caption / ui / body / h2 / h1 / display 七个语义档，供新代码按用途取字号。
export default {
  content: ["./index.html", "./src/**/*.{ts,tsx}"],
  darkMode: "class",
  theme: {
    extend: {
      borderRadius: {
        xs: "var(--radius-xs)",
        sm: "var(--radius-sm)",
        DEFAULT: "var(--radius-xs)",
        md: "var(--radius-md)",
        bubble: "var(--radius-bubble)",
        lg: "var(--radius-lg)",
        xl: "var(--radius-lg)",
        "2xl": "var(--radius-lg)",
        "3xl": "var(--radius-lg)",
      },
      fontSize: {
        micro: ["var(--fs-micro)", "var(--lh-micro)"],
        caption: ["var(--fs-caption)", "var(--lh-caption)"],
        ui: ["var(--fs-ui)", "var(--lh-ui)"],
        body: ["var(--fs-body)", "var(--lh-body)"],
        h2: ["var(--fs-h2)", "var(--lh-h2)"],
        h1: ["var(--fs-h1)", "var(--lh-h1)"],
        display: ["var(--fs-display)", "var(--lh-display)"],
        xs: ["var(--fs-caption)", "var(--lh-caption)"],
        sm: ["var(--fs-body)", "var(--lh-body)"],
        base: ["var(--fs-h2)", "var(--lh-h2)"],
        lg: ["1.125rem", { lineHeight: "1.75rem" }],
        xl: ["var(--fs-h1)", "var(--lh-h1)"],
        "2xl": ["1.5rem", { lineHeight: "2rem" }],
      },
      // 动效同样走主题变量：裸 transition-colors（无 duration/ease 修饰）原先用Tailwind
      // 默认的 150ms + cubic-bezier(.4,0,.2,1)，这里改指 --dur-base / --ease，
      // 使全站状态变化节奏一致（悬停反馈比原先慢 30ms，是有意的统一）。
      // extend 为深度合并，Tailwind 原有的 duration-150 / ease-in 等档位全部保留。
      transitionDuration: {
        DEFAULT: "var(--dur-base)",
        0: "0s",
        fast: "var(--dur-fast)",
        base: "var(--dur-base)",
        slow: "var(--dur-slow)",
      },
      transitionTimingFunction: {
        DEFAULT: "var(--ease)",
        linear: "linear",
      },
    },
  },
  plugins: [],
};
