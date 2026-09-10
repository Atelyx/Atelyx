# Atelyx 插件开发指南

Atelyx 是插件化平台：插件 = 一个 git 仓库（市场侧以 GitHub 为主），含一份 `package.json` 清单 + 入口源码。
给仓库打上 `atelyx-plugin` topic，即可被市场自动收录，任何 Atelyx 用户都可在应用内的市场中找到并安装。

插件语言支持 **JS / TypeScript**。TS 发布源码即可——宿主在加载时用内置转译器转成浏览器可执行代码，
无需构建产物；入口须**自包含**（单文件，无运行时 import/export 依赖；`import type` 为类型注解会
自动擦除）。

## 插件包结构

```
你的插件仓库/
├── package.json        # 清单（npm 标准字段 + atelyx 块，见 manifest.md）
├── index.ts            # 入口（默认导出 apply(ctx, config)；也可为 index.js）
└── README.md           # 建议附使用说明
```

## 入口契约

入口默认导出 `apply(ctx, config)`——`ctx` 是插件上下文，提供：

| 面 | 说明 |
| --- | --- |
| `ctx.<service>` | 类型化服务：平台服务 `state`/`app`/`shell`/`vault`/`dialog`/`clipboard`/`window`/`ai`/`collab` + 内核领域服务 `history`/`layout`/`uiState` + 插件提供的 `canvas`/`table`/`note`/`chat`（见 [ctx API](ctx-api.md)） |
| `ctx.events` | 领域事件总线：`ctx.events.on("canvas:changed", ...)` 订阅（`table:changed`/`vault:changed` 等，见 [ctx API](ctx-api.md)） |
| `ctx.slots` | 注册面：`registerView`/`registerTableView`/`registerNode`/`registerEdge`/`registerUi`/`registerMenu`/`registerSetting`/`registerAppPage`/`registerCommand`/`registerThemeSetting`（见 [ctx API](ctx-api.md)） |
| `ctx.ai` | AI 会话与工具：`ctx.ai.chat(...)` 直连模型；`ctx.ai.registerTool(...)` 贡献模型可调用的工具 |
| `ctx.effect` | 注册副作用（订阅/接线等），插件停用/卸载时自动撤销——**所有注册都应经它包裹** |

依赖声明用 apply 对象形式：`{ name, inject: ["table"], apply(ctx) { ... } }`——`inject` 声明的
服务缺失时插件不激活（管理页显示失败原因）。

## 插件类型

`package.json` 的 `atelyx.type` 只做市场展示/过滤（`panel`/`tableview`/`setting`/`app`/`node`/
`theme`/`tool`/`command`/`background`——`tool` = 经 `ctx.ai.registerTool` 贡献模型可调用的工具）；
实际能力全部在 `apply` 运行时注册（视图经 `ctx.slots`，逻辑经 `ctx` 服务/事件）。

## 快速开始

1. 按[清单格式](manifest.md)写 `package.json` + 入口源码。
2. 在 `apply` 里注册能力：`ctx.slots.registerView(...)`、`ctx.ai.registerTool(...)`、`ctx.events.on(...)`、调用 `ctx.<service>`。
3. 推送到 GitHub 仓库（`package.json` 位于仓库根；**仓库最新提交即发布版本**，无需打包）。
4. 给仓库打 `atelyx-plugin` topic。
5. 市场聚合每 6 小时刷新一次，之后即可在 Atelyx 的市场中搜到并安装（安装 = git clone 仓库源码）。

## 开发期安装（无需发布）

调试本地插件源码不需要发布：Atelyx 设置 → 插件页支持两种「取源码」安装：

- **从本地文件夹安装**：选择插件源码目录，App 以目录链接实时引用——改源码即时生效（无拷贝）。
- **从 Git 地址安装**：粘贴 git 仓库地址（GitHub / Gitee / 自建均可），App 克隆到插件目录，之后可一键更新（git pull）。
  地址需带协议头（`https://`/`http://`/`ssh://`/`git://` 或 `git@host:owner/repo`）；本机目录/NAS 路径请用上面的「从本地文件夹安装」（无需 git）。

本地目录来源的插件在管理页显示「本地目录」徽标且无更新按钮（本身即最新）；Git 来源显示「Git」徽标。

[发布检查清单 →](publishing.md)
