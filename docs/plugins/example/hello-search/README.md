# 示例搜索面板（示例插件）

第三方搜索面板示例插件，展示注册自己的视图 kind 与内置搜索并列。

- `atelyx.json` — 清单（`type: panel`，仅 UI 平面，入口 `mainUi`）
- `ui.js` — 主线程 UI 入口（`registerPanel` 注册自己的视图 kind + facade 仓库访问方法）

## 使用

1. 把本目录复制到你的 GitHub 仓库。
2. （可选）修改 `atelyx.json` 的 `id`/`name`/`author`。
3. 推送到 GitHub 仓库（`atelyx.json` 位于仓库根），给仓库打 `atelyx-plugin` topic。

安装并在设置 → 插件启用后，「添加视图」菜单里出现「示例搜索」——与内置「搜索」并列，
用户自选、可同时打开在不同面板；停用/卸载本插件后该项随之消失，内置搜索始终可用。

## 机制要点

- **kind 全局唯一**：内置视图与插件面板在同一视图贡献注册表里注册；重复注册、或占用内置
  保留 kind（如 `canvas`/`search`）都会抛错（会中断该插件脚本的后续注册），插件须用反向
  域名命名自己的 kind。
- **数据经 facade 通用方法**：`listFiles` / `openCanvasFile` / `openNote` / `openTable`
  ——与内置搜索面板同一输入面（宿主经 provider 注入，见 `services/plugins/ui.ts` 的
  `setPluginVaultAccess`），任何面板插件都可调用。

完整文档见 [插件开发指南](../../README.md)。
