import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import path from "node:path";

// Tauri 期望的固定端口与入口，不要随意改动
const TAURI_DEV_PORT = 1420;

// https://vitejs.dev/config/
export default defineConfig({
  plugins: [react()],
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
