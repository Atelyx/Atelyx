/** @type {import('tailwindcss').Config} */
// 颜色一律走 styles/index.css 的主题变量（组件按需写 bg-[var(--bg-secondary)] 等任意值），
// 不在此维护调色板副本——避免两套色值各自漂移。
export default {
  content: ["./index.html", "./src/**/*.{ts,tsx}"],
  darkMode: "class",
  theme: { extend: {} },
  plugins: [],
};
