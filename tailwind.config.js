/** @type {import('tailwindcss').Config} */
// 颜色一律走 styles/index.css 的主题变量（组件按需写 bg-[var(--bg-secondary)] 等任意值），
// 不在此维护调色板副本——避免两套色值各自漂移。
// 圆角整条 Tailwind 刻度都指向主题变量（不再有预设裸值）：xs 4 → sm 6 → md 10 → lg 14；
// xl / 2xl / 3xl 收敛到 lg（大容器、大图标）；bubble 是聊天气泡专用档（8px）。
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
    },
  },
  plugins: [],
};
