# 你好工具（Python 示例插件）

Python 运行时的最小可用 Atelyx 插件示例：AI 工具 + 命令 + 事件订阅。

- `atelyx.json` — 清单（`runtime: "python"`，`main: "main.py"`）
- `main.py` — 逻辑入口：顶层直接用注入的全局 `bridge`（无需 import）

## 使用

1. 把本目录复制到你的 GitHub 仓库。
2. （可选）修改 `atelyx.json` 的 `id`/`name`/`author`。
3. 推送到 GitHub 仓库（`atelyx.json` 位于仓库根），给仓库打 `atelyx-plugin` topic。

安装前确保本机有 Python（`python`/`python3` 在 PATH 中）。安装后在 Atelyx
设置 → 插件 → 启用，然后在设置 → Agent → 勾选 `hello` 工具即可被模型调用。

> 桥 API 与 JS 版完全一致；`ctx.stream` 流式、`registerCapability` 定义能力等同理可用。
> 完整文档见 [插件开发指南](../../README.md) 与 [桥协议](../../bridge-protocol.md)。
