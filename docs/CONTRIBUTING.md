# 贡献准则

欢迎任何形式的贡献：Bug、需求、文档、修复、新功能。

## 提 Issue

- **Bug**：复现步骤、期望行为与实际行为、系统与版本；附截图或日志更好。
- **需求**：你正在做什么、为什么现有能力不够、期望怎么解决。来自真实创作场景的问题最有价值。

## 本地开发

前置要求：Rust stable（版本由 `rust-toolchain.toml` 锁定），Linux 另需 X11 / Wayland 开发库。打包另需 Windows 上的 NSIS 与 Linux 上的 `appimagetool`。

```bash
git clone https://github.com/Atelyx/Atelyx.git
cd Atelyx
bash patches/apply.sh    # 一次即可：物化本地依赖补丁的源码目录
cargo run -p atelyx-app  # 启动桌面应用
```

常用命令：

```bash
cargo test --workspace                              # 工作区测试
cargo test --manifest-path atelyx-server/Cargo.toml # 服务端测试
bash scripts/package.sh                             # 构建并产出安装包
```

## 代码约定

### 分层职责

仓库是 Cargo workspace，依赖方向单向：壳层依赖 `atelyx-core`，`atelyx-core` 无 GUI 依赖。

| crate | 路径 | 职责 |
| --- | --- | --- |
| 应用壳 | `atelyx-app/` | iced 窗口与布局、视图、组件库与主题 |
| 核心 | `atelyx-core/` | UI 无关：仓库存储、Markdown 与 frontmatter 解析、会话与布局模型、插件宿主运行时、进程托管 |
| 协议 | `atelyx-plugin-protocol/` | 插件契约：握手、JSON-RPC 信封、ViewNode 界面树、manifest |
| SDK | `atelyx-plugin-sdk/` | 插件侧封装：连接泵、能力面代理、界面构建辅助 |
| 服务端 | `atelyx-server/` | 协作服务端（独立 Cargo 工程，不进 workspace） |

### 风格

- Rust 2021 edition；沿用仓库既有排版（不整文件跑格式化工具，避免噪音 diff）。
- 命名：类型 / 枚举 `PascalCase`，函数 / 变量 `snake_case`，常量 `UPPER_SNAKE_CASE`。
- 注释只写代码表达不了的契约（行为约定、失败与异常、时序与竞态、所有权与副作用），不写控制流复述。
- 新增 crate 或跨 crate 能力前先确认有无现成实现。

## 提交与 PR

- 分支式开发，一个 PR 只做一件事，写明改动动机与验证方式。
- 提交前自查 diff（`git status` / `git diff`），确认无调试残留、无多余文件。
- commit 描述用中文、一句话点题（如 `feat: 支持 xxx`），改动与描述一一对应。
- 合并前确保工作区与服务端测试全绿。
