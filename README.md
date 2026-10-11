<div align="center" style="padding:6px 0 10px">

<img src="atelyx-app/assets/icons/icon.png" alt="Atelyx" width="92">

<h1 style="color:#E5E0D5;font-weight:700;letter-spacing:3px;margin:16px 0 10px">ATELYX</h1>

<p style="color:#D4AF37;font-size:17px;font-weight:600;margin:0 0 14px">把对话、笔记、表格、文件放进同一张可拓展的工作台——AI 辅助，多人实时协作</p>

<p style="color:#8b8b8b;max-width:660px;margin:0 auto 30px;font-size:15px;line-height:1.8">
Atelyx 是一款以人为本的可拓展桌面工作台：对话、笔记、表格、文件在同一工作台；自建服务端即可开启多人实时协作。应用本体是纯 Rust 原生单体，工作由人主导、AI 只做辅助。
</p>

<span style="background:#D4AF37;color:#1C1C1E;border-radius:999px;padding:4px 18px;font-size:13px;font-weight:700;margin:0 4px">Windows</span> <span style="border:1px solid #5a5a5e;color:#9a9a9e;border-radius:999px;padding:3px 17px;font-size:13px;font-weight:600;margin:0 4px">Linux · Wayland</span> <span style="border:1px solid #5a5a5e;color:#9a9a9e;border-radius:999px;padding:3px 17px;font-size:13px;font-weight:600;margin:0 4px">Apache-2.0</span>

</div>

## 设计理念

平时工作需要在多个工具之间来回切换：写作用一个软件、表格用一个软件、沟通再切到聊天工具，每次切换都会打断思路。Atelyx 把常用场景放进同一张工作台：

- **一个工作台装下常用工作** — 对话、笔记、表格、文件、搜索在同一工作台并排打开；标签可停靠，面板可拆成独立窗口，布局自由组合。
- **AI 辅助随手可用** — AI 内嵌在对话、笔记、表格、文件与搜索等场景里，边写边用，不必切到单独的 AI 工具；工作由人主导，AI 提供辅助。
- **素材可复用** — 搜索结果、提炼的段落、粘贴的素材会沉淀为可复用资产，可随时接入任意对话。
- **协作空间** — 在自建服务端上开一个协作空间：成员实时互见，同编一篇笔记、同改一块画布、同看一张表格；内容真源在服务端，局域网与公网均可部署。
- **文件即仓库** — 个人仓库没有数据库：笔记、表格、附件都是本地普通文件，可备份、可 Git 同步；与协作空间在文件面板并排使用。

## 技术形态

纯 Rust 原生单体应用：GUI 走 [iced](https://iced.rs)（Elm 架构）。仓库根是一个 Cargo workspace：

| 目录 | 职责 |
| --- | --- |
| `atelyx-app/` | iced 桌面壳：窗口与布局、编辑器与各功能视图、主题与组件库 |
| `atelyx-core/` | UI 无关核心：仓库存储、会话容器、Markdown 与 frontmatter 解析、协作帧泵、插件进程托管 |
| `atelyx-server/` | 协作服务端（独立进程，不进 workspace） |

依赖方向：两个 crate → `atelyx-core`；`atelyx-core` 不依赖任何 GUI crate。

## 平台

Windows 10/11（x64）+ Linux（Wayland 原生，兼容 X11）。

## 开发命令

```bash
cargo run -p atelyx-app     # 启动桌面应用
cargo test --workspace      # 工作区测试
cargo test --manifest-path atelyx-server/Cargo.toml   # 服务端测试
cargo build -p atelyx-app --release                   # 打包
```

前置要求：Rust（stable，仓库根 `rust-toolchain.toml` 锁定版本）。Linux 需要 iced/winit 的系统依赖（X11 或 Wayland 开发库）。

## 自建协作服务端

协作空间的内容真源在服务端（`atelyx-server/`，Rust 单进程），一台 Linux 机器或 NAS 即可部署：

```bash
# 方式一：Docker Compose（数据落在 ./data，备份 = 拷贝该目录）
cd atelyx-server && docker compose up -d

# 方式二：Linux + systemd（自动构建并注册服务，数据目录 /var/lib/atelyx）
sudo bash atelyx-server/install.sh
# 自定义端口 / 数据目录：sudo bash atelyx-server/install.sh 13000 /mnt/nas/atelyx-data
```

- 服务端口默认 `11224`；设 `TLS_CERT` + `TLS_KEY` 即启用 HTTPS/WSS，更多配置见 `atelyx-server/docker-compose.yml` 与 `install.sh` 头注释。
- 部署后浏览器打开 `http://<服务器>:11224` 进入管理台：注册账号（首个注册账号为管理员）、创建空间并邀请成员。

## 参与贡献

- 提 Bug / 需求：[Issues](https://github.com/Atelyx/Atelyx/issues)
- 提交 Pull Request：请先阅读 [贡献准则](docs/CONTRIBUTING.md)

## License

[Apache-2.0](LICENSE)