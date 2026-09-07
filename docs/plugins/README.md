# Atelyx 插件开发指南

Atelyx 是一个插件化平台：插件 = 一个 git 仓库（市场侧以 GitHub 为主），含一份 `atelyx.json` 清单 + 入口脚本 + 资源。
给仓库打上 `atelyx-plugin` topic，即可被市场自动收录，任何 Atelyx 用户都可在内置市场中找到并安装。

插件可扩展九类能力，按运行平面分两组：

## 插件类型

| 类型 | 说明 | 运行平面 |
| --- | --- | --- |
| `tool` | AI 工具/命令（模型可调用） | worker（隔离上下文） |
| `background` | 后台常驻服务（无界面，订阅事件） | worker（隔离上下文） |
| `command` | 全局命令（菜单/快捷键等执行入口） | worker（逻辑经桥 RPC） |
| `panel` | 工作区面板视图 | 主线程（React UI） |
| `tableview` | 表格编辑器内的多维表格视图 | 主线程（React UI） |
| `setting` | 设置页条目 | 主线程（React UI） |
| `app` | 应用级页面/模式（可全页接管） | 主线程（React UI） |
| `node` | 画布节点 | 主线程（React UI） |
| `theme` | UI 皮肤（CSS 变量，声明式，无需代码） | 声明式（仅清单） |

> 一个插件可同时属于多类：清单 `type` 为主分类，`types` 可列出附加分类；同时含 worker 逻辑与 UI 时，
> worker 逻辑写进 `main`，UI 入口写进 `mainUi`，两个平面都会加载。

## 两个运行平面

- **worker 平面**（`tool`/`background`/`command`）：入口 `main` 在独立的 Web Worker 中执行。
  与 App 隔离：没有 `window`、没有系统命令访问，只能通过 `bridge` 对象与 App 通信（注册工具/命令、读写自身状态、订阅事件）。单个插件崩溃不影响 App。
- **主线程平面**（`panel`/`tableview`/`setting`/`app`/`node`）：入口 `mainUi` 在主线程执行，可渲染 React 界面；
  未声明 `mainUi` 时，仅**无 worker 平面**（不属 tool/background/command）的插件才缺省用 `main` 作为 UI 入口。
  与 App 同上下文，经 `window.__atelyxPlugin__` 的 facade 注册贡献。

详见 [清单格式](manifest.md) 与 [桥 API](bridge-api.md)。

## 快速开始

1. 复制 `docs/plugins/example/hello-tool/` 到你自己的 GitHub 仓库（或从零按[清单格式](manifest.md)写）。
2. 开发工具/命令逻辑，用 `bridge.registerTool(...)` / `bridge.registerCommand(...)` 注册。
3. 推送到 GitHub 仓库（`atelyx.json` 必须位于仓库根；**仓库最新提交即发布版本**，无需打包）。
4. 给仓库打 `atelyx-plugin` topic。
5. 市场聚合每 6 小时刷新一次，之后即可在 Atelyx 的市场中搜到并安装（安装 = git clone 仓库源码）。

## 开发期安装（无需发布）

调试本地插件源码不需要发布：Atelyx 设置 → 插件页支持两种「取源码」安装：

- **从本地文件夹安装**：选择插件源码目录，App 以目录链接实时引用——改源码即时生效（无拷贝）。
- **从 Git 地址安装**：粘贴 git 仓库地址（GitHub / Gitee / 自建均可），App 克隆到插件目录，之后可一键更新（git pull）。

本地目录来源的插件在管理页显示「本地目录」徽标且无更新按钮（本身即最新）；Git 来源显示「Git」徽标。

[发布检查清单 →](publishing.md)
