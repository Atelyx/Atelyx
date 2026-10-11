# 贡献准则

欢迎任何形式的贡献：提 Bug、提需求、改进文档、修复问题、实现功能。

## 提 Issue

- **Bug**：说明复现步骤、期望行为与实际行为、操作系统与版本号；附上截图或日志更好。
- **需求**：说明你正在做什么、为什么现有能力不够、期望怎么解决。从真实创作场景出发的问题最有价值。

## 本地开发

前置要求：Rust（stable，仓库根 `rust-toolchain.toml` 锁定版本）。Linux 还需 iced / winit 的系统依赖（X11 或 Wayland 开发库）。

```bash
git clone https://github.com/Atelyx/Atelyx.git
cd Atelyx
cargo run -p atelyx-app   # 启动开发
```

常用命令：

```bash
cargo test --workspace   # 工作区测试（提交前必须通过）
cargo build --release    # 打包
```

## 代码约定

### 分层职责

仓库是 Cargo workspace，依赖方向单向：壳层 crate 依赖 `atelyx-core`，`atelyx-core` 不依赖任何 GUI crate。

| crate | 路径 | 职责 |
| --- | --- | --- |
| 应用壳 | `atelyx-app/` | iced 窗口与布局、视图、组件库与主题 |
| 核心 | `atelyx-core/` | UI 无关：仓库存储、会话容器、Markdown 与 frontmatter 解析、协作帧泵、进程托管 |
| 服务端 | `atelyx-server/` | 协作服务端（独立进程，不进 workspace） |

### 风格

- Rust 2021 edition；沿用仓库既有排版（不整文件跑格式化工具，避免噪音 diff）。
- 命名：类型 / 枚举 `PascalCase`，函数 / 变量 `snake_case`，常量 `UPPER_SNAKE_CASE`。
- 注释只写代码表达不了的契约（行为约定、失败与异常、时序与竞态、所有权与副作用），不写控制流复述。
- 新增 crate 或跨 crate 能力前先确认已有实现能否复用，避免重复造轮子。

## 提交与 PR

- 分支式开发，一个 PR 只做一件事，说明改动动机与验证方式。
- 提交前自行审查 diff（`git diff` / `git status`），确认无调试残留、无多余文件。
- commit 描述用中文、一句话简短点题（如 `feat: 支持 xxx` / `fix: 修复 xxx`），内容与描述一一对应。
- 合并前确保 `cargo test --workspace` 通过。