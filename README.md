<div align="center" style="padding:6px 0 10px">

<a href="docs/README_EN.md" style="display:inline-block;padding:4px 16px;border:1px solid #d8d8dc;border-radius:999px;color:#6b6b70;font-size:13px;line-height:1.6;text-decoration:none">English version →</a>

</div>

<div align="center" style="background:#17171a;border:1px solid #2a2a2e;border-radius:16px;padding:48px 24px 40px;margin:0 0 32px">

<img src="src-tauri/icons/icon.png" alt="Atelyx" width="92">

<h1 style="color:#E5E0D5;font-weight:700;letter-spacing:3px;margin:16px 0 10px">ATELYX</h1>

<p style="color:#D4AF37;font-size:17px;font-weight:600;margin:0 0 14px">把对话、笔记、表格、文件放进同一张可拓展的工作台——AI 辅助，多人实时协作</p>

<p style="color:#8b8b8b;max-width:660px;margin:0 auto 30px;font-size:15px;line-height:1.8">
Atelyx 是一款以人为本的可拓展桌面工作台：对话、笔记、表格、文件在同一工作台；自建服务端即可开启多人实时协作。应用本体是极薄的内核，对话、笔记、表格、画布、搜索、日历等功能由随应用分发的插件实现，可停用、可被第三方插件替换——目标是探索 AI 时代协作与工作的新范式。
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
- **协作空间** — 在自建服务端上开一个协作空间：成员实时互见，同编一篇笔记、同改一块画布、同看一张表格；内容真源在服务端，局域网与公网均可部署。
- **文件即仓库** — 个人仓库没有数据库：画布、笔记、附件都是本地普通文件，可备份、可 Git 同步；与协作空间在文件面板并排使用。

## 功能总览

内核极薄，随应用分发的功能全部由插件提供：

- **AI 对话** — 接入 OpenAI 兼容模型，自定义供应商与密钥；对话可分支成画布节点，连线表达数据流。
- **笔记** — Markdown 编辑，支持表格、数学公式、双链、标签、脚注等，多人实时协作编辑。
- **表格** — 结构化数据表，支持图片与多人实时协作。
- **画布** — 空间画布：对话分支、笔记、素材成为节点，连线表达引用与产出关系。
- **文件** — 仓库文件树：个人仓库与协作空间并排使用，文件面板就地切换。
- **搜索** — 网络搜索与仓库内全文检索，结果可沉淀为素材接入对话。
- **无限拓展** — 内置功能只是插件的起点：应用内市场一键安装第三方插件，插件与内置功能同构、无特权，可随时停用或替换默认实现——工作台的能力上限由插件决定。

## 安装

从 [GitHub Releases](https://github.com/Atelyx/Atelyx/releases) 下载对应平台安装包：

| 平台 | 安装包 |
| --- | --- |
| Windows 10/11（x64） | `.exe` 安装包（支持应用内自动更新） |
| Linux（Wayland 原生，兼容 X11） | 源码构建 |

前置要求：Node.js 20.19+、pnpm 10、Rust（stable）、Tauri 2 系统依赖（见 [Tauri 官方文档](https://v2.tauri.app/start/prerequisites/)）。从源码构建见下文「开发命令」。

## 开发命令

```bash
pnpm install         # 安装前端依赖
pnpm run tauri:dev   # 启动开发（自动开 Vite + Tauri 窗口）
pnpm run tauri:build # 打包
pnpm run check       # 完整门禁：类型检查 + ESLint + 前端测试 + cargo test
```

## 自建协作服务端

协作空间的内容真源在服务端（`collab-relay/`，Rust 单进程），一台 Linux 机器或 NAS 即可部署：

```bash
# 方式一：Docker Compose（数据落在 ./data，备份 = 拷贝该目录）
cd collab-relay && docker compose up -d

# 方式二：Linux + systemd（自动构建并注册服务，数据目录 /var/lib/atelyx）
sudo bash collab-relay/install.sh
# 自定义端口 / 数据目录：sudo bash collab-relay/install.sh 13000 /mnt/nas/atelyx-data
```

- 服务端口默认 `11224`；设 `TLS_CERT` + `TLS_KEY` 即启用 HTTPS/WSS，更多配置见 `collab-relay/docker-compose.yml` 与 `install.sh` 头注释。
- 部署后浏览器打开 `http://<服务器>:11224` 进入管理台：注册账号（首个注册账号为管理员）、创建空间并邀请成员；客户端在应用内填服务器地址登录即可协作。

## 插件开发

Atelyx 由内核与插件组成：内核负责窗口、布局与插件加载，功能以插件形态提供。随应用分发的基础插件与第三方插件同构——同一挂载机制、无特权，可停用、可替换默认实现。插件内核基于 Cordis 基座（可逆 effect + typed events），与 [deepseek-harness](https://github.com/deepseek-ai/deepseek-harness) 同源；两者定位不同——deepseek-harness 是面向 Agent 的运行时，Atelyx 是面向人的桌面工作台。

插件是一个 Git 仓库：在 `package.json` 的 `atelyx` 块里声明入口与能力面，推送到 GitHub 并打上 `atelyx-plugin` 主题，即被插件市场自动收录。开发指南见 [插件开发文档](docs/plugins/README.md)。

## 参与贡献

- 提 Bug / 需求：[Issues](https://github.com/Atelyx/Atelyx/issues)
- 提交 Pull Request：请先阅读 [贡献准则](docs/CONTRIBUTING.md)

## License

[Apache-2.0](LICENSE)
