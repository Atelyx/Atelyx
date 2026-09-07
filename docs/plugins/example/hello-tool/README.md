# 你好工具（示例插件）

最小可用的 Atelyx 插件示例：AI 工具 + 命令 + 事件订阅 + 状态持久化。

- `atelyx.json` — 清单（`type: tool`，另含 `command`/`background` 附加分类）
- `plugin.js` — worker 平面入口（`bridge.registerTool` / `registerCommand` / `on` / `stateRead` / `stateWrite`）

## 使用

1. 把本目录复制到你的 GitHub 仓库。
2. （可选）修改 `atelyx.json` 的 `id`/`name`/`author`。
3. 推送到 GitHub 仓库（`atelyx.json` 位于仓库根），给仓库打 `atelyx-plugin` topic。

安装后在 Atelyx 设置 → 插件 → 启用「你好工具」，然后在设置 → Agent → 勾选 `hello` 工具即可被模型调用。

完整文档见 [插件开发指南](../../README.md)。
