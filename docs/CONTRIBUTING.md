# 贡献准则

欢迎任何形式的贡献：提 Bug、提需求、改进文档、修复问题、实现功能。

## 提 Issue

- **Bug**：说明复现步骤、期望行为与实际行为、操作系统与版本号；附上截图或日志更好。
- **需求**：说明你正在做什么、为什么现有能力不够、期望怎么解决。从真实创作场景出发的问题最有价值。

## 本地开发

前置要求：Node.js 18+、pnpm、Rust（stable）、Tauri 2 系统依赖。

```bash
git clone https://github.com/Atelyx/Atelyx.git
cd Atelyx
pnpm install
pnpm run tauri:dev    # 启动开发（自动开 Vite + Tauri 窗口）
```

常用命令：

```bash
pnpm run check       # 类型检查 + ESLint（提交前必须通过）
pnpm run format      # Prettier 格式化
pnpm run tauri:build # 打包
```

## 代码约定

### 分层职责

前端按层组织，跨层只走 store 或 props，组件不直接调 service：

| 层 | 路径 | 职责 |
| --- | --- | --- |
| 页面 | `src/pages/` | 组合组件与 store |
| 组件 | `src/components/` | 纯 UI，props 与回调通信 |
| 状态 | `src/stores/` | 运行时状态 + 调 service 持久化 |
| 服务 | `src/services/` | 所有外部 I/O 的唯一出口（invoke / AI / 搜索 / keychain） |
| 类型 | `src/types/` | 类型契约 |
| Rust 命令 | `src-tauri/src/commands/` | Tauri 命令边界 |
| Rust 模块 | `src-tauri/src/{vault,watcher}.rs` | 仓库文件读写、文件监听 |

主要目录：

- `components/canvas/` 画布节点与面板，`components/table/` 多维表格，`components/editor/` 笔记编辑器，`components/layout/` 工作区布局，`components/common/` 通用组件
- `hooks/` 复用交互逻辑，`utils/` 纯函数工具，`constants/` 常量与 schema，`styles/` 主题变量

### 风格

- TypeScript 严格模式，禁止 `any`、禁止 `@ts-ignore`（类型定义文件除外）。
- 命名：组件 `PascalCase`，工具 / service `camelCase`，常量 `UPPER_SNAKE_CASE`，数据字段 camelCase。
- 图标统一用 lucide-react 线性图标。
- 新增节点组件放 `components/canvas/nodes/`，新增类型放 `src/types/` 并在 barrel export，新增 Rust 命令在 `lib.rs` 注册。


## 提交与 PR

- 分支式开发，一个 PR 只做一件事，说明改动动机与验证方式。
- 提交前自行审查 diff（`git diff` / `git status`），确认无调试残留、无多余文件。
- commit 描述用中文、一句话简短点题（如 `feat: 支持 xxx` / `fix: 修复 xxx`），内容与描述一一对应。
- 合并前确保 `pnpm run check` 通过。
