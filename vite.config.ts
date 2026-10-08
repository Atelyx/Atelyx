import { defineConfig, type Plugin } from "vite";
import react from "@vitejs/plugin-react";
import path from "node:path";
import { BOOT_SPLASH_ID, bootSplashMarkup, bootSplashStyle } from "./src/constants/panelSkeleton";

// Tauri 期望的固定端口与入口，不要随意改动
const TAURI_DEV_PORT = 1420;

/**
 * 首帧骨架注入（dev 与打包同源）：WebView 内核启动到 React 挂载之间没有任何页面 CSS/JS 可用，
 * 骨架只能以原文形式随 index.html 下发——由 `constants/panelSkeleton` 单源生成，
 * 挂在 body 首位（fixed 覆盖），App 首个提交时移除。
 */
function bootSplashPlugin(): Plugin {
  return {
    name: "atelyx-boot-splash",
    transformIndexHtml() {
      return [
        { tag: "style", children: bootSplashStyle(), injectTo: "head" },
        {
          tag: "div",
          attrs: { id: BOOT_SPLASH_ID, "aria-hidden": "true" },
          children: bootSplashMarkup(),
          injectTo: "body-prepend",
        },
      ];
    },
  };
}

// https://vitejs.dev/config/
export default defineConfig({
  plugins: [react(), bootSplashPlugin()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "src"),
      // vendored Cordis 依赖映射（源码见 vendor/，说明见 vendor/README.md）
      "@atelyx/cordis": path.resolve(__dirname, "vendor/cordis/src/index.ts"),
      "@atelyx/cosmokit": path.resolve(__dirname, "vendor/cosmokit/src/index.ts"),
    },
  },
  // Tauri 要求固定 dev 端口，且只通过 iframe 访问
  clearScreen: false,
  server: {
    port: TAURI_DEV_PORT,
    strictPort: true,
    host: "127.0.0.1",
    watch: {
      // 监听 Tauri 配置变化，触发热重载
      ignored: ["**/src-tauri/target/**"],
    },
  },
  envPrefix: ["VITE_", "TAURI_"],
  build: {
    target: "es2022",
    minify: "esbuild",
    sourcemap: false,
  },
});
