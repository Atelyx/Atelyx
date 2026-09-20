import { defineConfig } from "vitest/config";
import path from "node:path";

// 测试环境的 `@` 别名需与 vite.config.ts 一致（vitest.config 存在时不会合并 vite.config 的 resolve）。
export default defineConfig({
  test: {
    environment: "node",
    // .test.tsx = 组件测试（文件内 @vitest-environment jsdom 逐文件声明 DOM 环境）
    include: ["src/**/*.test.ts", "src/**/*.test.tsx"],
    pool: "forks",
  },
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "src"),
      "@atelyx/cordis": path.resolve(__dirname, "vendor/cordis/src/index.ts"),
      "@atelyx/cosmokit": path.resolve(__dirname, "vendor/cosmokit/src/index.ts"),
    },
  },
});
