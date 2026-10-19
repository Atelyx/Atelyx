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

笔记、表格、沟通分散在多个软件里，每次切换都打断思路。Atelyx 把这些场景收进同一张工作台：

- **一个工作台装下常用工作** — 对话、笔记、表格、文件、搜索并排打开；标签可停靠，面板可拆成独立窗口。
- **AI 辅助随手可用** — AI 内嵌在对话、笔记、表格、文件与搜索中，边写边用；工作由人主导。
- **素材可复用** — 搜索结果、摘录与粘贴的素材沉淀为资产，可接入任意对话。
- **协作空间** — 自建服务端开一个空间：成员实时互见，同编一篇笔记、同改一块画布、同看一张表格；内容真源在服务端，局域网与公网均可部署。
- **文件即仓库** — 个人仓库无数据库：笔记、表格、附件都是本地普通文件，可备份、可 Git 同步，与协作空间并排使用。

## 技术形态

纯 Rust 原生单体应用，GUI 采用 [iced](https://iced.rs)（Elm 架构）。仓库根是一个 Cargo workspace：

| 目录 | 职责 |
| --- | --- |
| `atelyx-app/` | iced 桌面壳：窗口与布局、编辑器与各功能视图、主题与组件库 |
| `atelyx-core/` | UI 无关核心：仓库存储、Markdown 与 frontmatter 解析、会话与布局模型、插件宿主运行时、进程托管 |
| `atelyx-plugin-protocol/` | 插件协议契约：握手、JSON-RPC 信封、ViewNode 界面树、manifest |
| `atelyx-plugin-sdk/` | 插件侧 SDK：连接泵、能力面代理、界面构建辅助 |
| `atelyx-server/` | 协作服务端（独立 Cargo 工程，不进 workspace） |

依赖方向：`atelyx-app` → `atelyx-core` → `atelyx-plugin-protocol`；`atelyx-plugin-sdk` → `atelyx-plugin-protocol`。`atelyx-core` 无 GUI 依赖；`atelyx-server` 不被任何 crate 依赖，客户端只经 HTTP/WS 访问。

## 插件开发

插件是独立进程：宿主启动它，经 stdin/stdout 上的 JSON-RPC 与它通信；界面由插件提交声明式控件树或绘图指令，宿主负责渲染。语言不限，官方提供 Rust SDK。

- 上手、构建打包、本地调试与上架：见 [插件开发文档](docs/plugin-development.md)。
- 逐字段 / 逐方法的契约：[插件 API 参考](docs/plugin-api.md)（由协议源码生成，改协议后重跑 `cargo run -p xtask -- docs`）。
- 契约真源 `atelyx-plugin-protocol/`，插件侧封装 `atelyx-plugin-sdk/`。

## 平台

Windows 10/11（x64）+ Linux（Wayland 原生，兼容 X11）。

## 开发命令

```bash
bash patches/apply.sh    # 新克隆后执行一次：物化本地依赖补丁的源码目录
cargo run -p atelyx-app  # 启动桌面应用
cargo test --workspace   # 工作区测试
cargo run -p xtask -- docs  # 重新生成插件 API 参考（协议改动后）
cargo test --manifest-path atelyx-server/Cargo.toml  # 服务端测试
bash scripts/package.sh  # 构建 release 并产出当前平台安装包到 target/dist/
```

前置要求：Rust stable（版本由 `rust-toolchain.toml` 锁定），Linux 另需 X11 / Wayland 开发库。打包另需 Windows 上的 NSIS 与 Linux 上的 `appimagetool`。

## 自建协作服务端

`atelyx-server/` 是协作空间的内容真源（Rust 单进程），一台 Linux 机器或 NAS 即可部署：

```bash
# 方式一：Docker Compose（数据落在 ./data，备份 = 拷贝该目录）
cd atelyx-server && docker compose up -d

# 方式二：Linux + systemd（自动构建并注册服务，数据目录 /var/lib/atelyx）
sudo bash atelyx-server/install.sh
# 自定义端口 / 数据目录：sudo bash atelyx-server/install.sh 13000 /mnt/nas/atelyx-data
```

- 默认端口 `11224`；设 `TLS_CERT` + `TLS_KEY` 即启用 HTTPS/WSS，其余配置见 `atelyx-server/docker-compose.yml` 与 `install.sh`。
- 部署后打开 `http://<服务器>:11224` 进入管理台：注册账号（首个为管理员）、创建空间、邀请成员。

## 参与贡献

- 提 Bug / 需求：[Issues](https://github.com/Atelyx/Atelyx/issues)
- 提交 Pull Request：请先阅读 [贡献准则](docs/CONTRIBUTING.md)

## License

[Apache-2.0](LICENSE)
