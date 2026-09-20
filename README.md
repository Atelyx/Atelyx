<div align="center" style="padding:6px 0 10px">

<a href="docs/README_EN.md" style="display:inline-block;padding:4px 16px;border:1px solid #d8d8dc;border-radius:999px;color:#6b6b70;font-size:13px;line-height:1.6;text-decoration:none">English version →</a>

</div>

<div align="center" style="background:#17171a;border:1px solid #2a2a2e;border-radius:16px;padding:48px 24px 40px;margin:0 0 32px">

<img src="src-tauri/icons/icon.png" alt="Atelyx" width="92">

<h1 style="color:#E5E0D5;font-weight:700;letter-spacing:3px;margin:16px 0 10px">ATELYX</h1>

<p style="color:#D4AF37;font-size:17px;font-weight:600;margin:0 0 14px">把对话、笔记、表格、文件放进同一张可拓展的工作台——AI 辅助，多人实时协作</p>

<p style="color:#8b8b8b;max-width:660px;margin:0 auto 30px;font-size:15px;line-height:1.8">
Atelyx 是一款以人为本的可拓展桌面工作台：对话、笔记、表格、文件在同一工作台；局域网多人实时协作。应用本体是极薄的内核，对话、笔记、表格、画布、搜索、日历等功能由随应用分发的插件实现，可停用、可被第三方插件替换——目标是探索 AI 时代协作与工作的新范式。
</p>

<span style="background:#D4AF37;color:#1C1C1E;border-radius:999px;padding:4px 18px;font-size:13px;font-weight:700;margin:0 4px">Windows</span>
<span style="border:1px solid #5a5a5e;color:#9a9a9e;border-radius:999px;padding:3px 17px;font-size:13px;font-weight:600;margin:0 4px">Linux · Wayland</span>
<span style="border:1px solid #5a5a5e;color:#9a9a9e;border-radius:999px;padding:3px 17px;font-size:13px;font-weight:600;margin:0 4px">Apache-2.0</span>

</div>

## 设计理念

平时工作需要在多个工具之间来回切换：写作用一个软件、表格用一个软件、沟通再切到聊天工具，每次切换都会打断思路。Atelyx 把常用场景放进同一张工作台：

- **一个工作台装下常用工作** — 对话、笔记、表格、文件、搜索在同一工作台并排打开；标签可停靠，面板可拆成独立窗口，布局自由组合。内置功能本身由插件实现，可停用、可替换，工作台形态不预设。
- **AI 辅助随手可用** — AI 内嵌在对话、笔记、表格、文件与搜索等场景里，边写边用，不必切到单独的 AI 工具；工作由人主导，AI 提供辅助。
- **素材可复用** — 搜索结果、提炼的段落、粘贴的素材会沉淀为可复用资产，可随时接入任意对话。
- **局域网协作** — 在自建服务端上开一个协作空间：成员实时互见，同编一篇笔记、同改一块画布、同看一张表格；局域网内开箱即用，内容真源在服务端。
- **文件即仓库** — 个人仓库没有数据库：画布、笔记、附件都是本地普通文件，可积累、可备份、可 Git 同步，也可被外部编辑器打开并实时同步回来；两类仓库在文件面板并排使用。

## 安装

从 [GitHub Releases](https://github.com/Atelyx/Atelyx/releases) 下载对应平台安装包：

| 平台 | 安装包 |
| --- | --- |
| Windows 10/11（x64） | `.msi` / `.exe` |
| Linux（Wayland 原生，兼容 X11） | 源码构建 |

前置要求：Node.js 18+、pnpm、Rust（stable）、Tauri 2 系统依赖（见 [Tauri 官方文档](https://v2.tauri.app/start/prerequisites/)）。从源码构建见下文「开发命令」。

## 开发命令

```bash
pnpm install         # 安装前端依赖
pnpm run tauri:dev   # 启动开发（自动开 Vite + Tauri 窗口）
pnpm run tauri:build # 打包
pnpm run check       # 完整门禁：类型检查 + ESLint + 前端测试 + cargo test
```

## 插件开发

Atelyx 由内核与插件组成：内核负责窗口、布局与插件加载，功能以插件形态提供。随应用分发的基础插件与第三方插件同构——同一挂载机制、无特权，可停用、可替换默认实现。插件内核基于 Cordis 基座（可逆 effect + typed events），与 [deepseek-harness](https://github.com/deepseek-ai/deepseek-harness) 同源；两者定位不同——deepseek-harness 是面向 Agent 的运行时，Atelyx 是面向人的桌面工作台。

插件是一个 Git 仓库：在 `package.json` 的 `atelyx` 块里声明入口与能力面，推送到 GitHub 并打上 `atelyx-plugin` 主题，即被插件市场自动收录。开发指南见 [插件开发文档](docs/plugins/README.md)。

## 参与贡献

- 提 Bug / 需求：[Issues](https://github.com/Atelyx/Atelyx/issues)
- 提交 Pull Request：请先阅读 [贡献准则](docs/CONTRIBUTING.md)

## License

[Apache-2.0](LICENSE)
