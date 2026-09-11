# 插件包清单（`package.json`）

清单是插件的唯一契约，放在插件根目录（git 仓库根，安装时以此为插件根）。npm 标准字段 +
嵌套 `atelyx` 块；未知字段/类型被安全跳过，只对结构性问题（缺字段、类型错误、未知主分类）报错。

```jsonc
{
  "name": "com.example.hello",     // 必须：反向域名式稳定标识（发布后不可变；至少两段，小写字母/数字/中划线）
  "version": "1.0.0",              // 必须：语义化版本
  "main": "index.ts",              // 必须：入口（相对插件根目录；.js/.ts/.tsx；纯 theme 插件可省略）
  "description": "详细描述（markdown）",
  "author": "作者",
  "license": "MIT",
  "keywords": ["效率"],
  "atelyx": {
    "name": "你好工具",              // 显示名（缺省 = name）
    "type": "tool",                 // 必须：主分类（见下表）
    "types": ["tool", "command"],   // 可选：附加分类
    "scope": "app",                 // 可选：app=个人工具（本机，默认）| vault=随仓库共享
    "tagline": "一句话简介",
    "atelyxVersionMin": "0.5.0",    // 可选：兼容的宿主版本下限
    // "atelyxVersionMax": "0.6.0", // 可选：兼容的宿主版本上限（不含）
    "platforms": ["windows-x64"],   // 可选：目标平台，缺省全平台
    "declares": ["table"],          // 可选：披露将访问的 Atelyx 服务（管理页「声明 vs 实际」审计对照的声明侧）
    "permissions": { "table": "读取当前打开的表格数据" }, // 可选：服务名 → 一句理由（安装/详情展示）
    "hostApiVersion": 1,            // 可选：插件契约版本（与宿主 App 版本解耦）
    "themes": [                     // 可选（type 含 theme 时）：声明式主题条目，无需代码
      { "id": "nord-light", "name": "Nord 浅色", "colorScheme": "light", "variables": { "accent": "#7c3aed" } }
    ],
    "themeOptions": { "accent": true } // 可选：预置设置项声明（accent = 内核实现的强调色设置项）
  }
}
```

## 类型取值

| `type` | 说明 |
| --- | --- |
| `panel` | 工作区面板视图（`ctx.slots.registerView`） |
| `tableview` | 表格编辑器内的多维表格视图（`ctx.slots.registerTableView`） |
| `setting` | 设置页条目 |
| `app` | 应用级页面/模式（可全页接管） |
| `node` | 画布节点 |
| `theme` | 主题插件（声明式主题条目 + 可选设置项，无需入口） |
| `tool` | AI 工具（经 `ctx.ai.registerTool` 注册，模型可调用） |
| `command` | 全局命令（菜单/快捷键等执行入口） |
| `background` | 后台常驻服务（无界面，订阅事件） |

> 一个插件可同时属于多类：`type` 为主分类，`types` 列出附加分类。类型只做市场展示/过滤，
> 实际能力在 `apply` 运行时注册。

## 入口

- `main` 指向入口文件（`.js`/`.ts`/`.tsx`，其余扩展名在安装/读取时拒绝），默认导出 `apply(ctx, config)`。
- TS 入口发布源码即可，宿主内置转译器加载时转译；入口须自包含（无运行时 import）。
- 纯 theme 插件（`type: "theme"` 且无其他代码类型）可省略 `main`——主题是声明式皮肤。

## 披露 `declares` 与完全自由模型

**完全自由模型：无运行时拒绝**——插件与宿主在同一 realm 内运行，`declares` 是**披露不是约束**：
它只用于管理页与审计对照展示，少报/漏报不会拦住任何调用，安全责任落在用户知情（安装警告）。
宿主服务里的敏感项（如 `shell`/`clipboard`）在管理页以「敏感」高亮。

## 宿主兼容

- `atelyxVersionMin`/`atelyxVersionMax`：不匹配的插件在安装时会被拒绝（并回滚清理），启用时加载也会被拒绝。
- `hostApiVersion`：插件契约版本（`ctx` 服务面与 `ui` 注册面的语义版本，与宿主 App 版本解耦）。
  **缺省视为当前契约版本**；显式声明且与宿主不同时，安装与加载都会被拒绝并提示所需版本——
  这样破坏性契约变更会响亮失败，而不是静默坏掉。当前契约版本为 1。
- `platforms`：`windows-x64` / `linux-x64`，缺省全平台。

## 作用域

- `app`（默认）：个人工具，装在 App 数据目录，跨仓库可用，不随仓库同步。
- `vault`：随仓库共享，装在仓库 `.atelyx/plugins/`，适合团队共用的插件；安装时会有「代码随仓库扩散」提示。

## 声明式主题（`themes` + `themeOptions`）

`theme` 类型插件在清单里声明主题条目与可选设置项，无需代码。每个主题条目 = 一个
**基础配色方案 + 一组语义变量覆盖**：

```json
{
  "type": "theme",
  "themes": [
    { "id": "nord-light", "name": "Nord 浅色", "colorScheme": "light", "variables": { "--accent": "#7c3aed", "--bg-primary": "#f0f2f5" } },
    { "id": "nord-dark", "name": "Nord 深色", "colorScheme": "dark", "variables": { "--accent": "#a78bfa" } }
  ],
  "themeOptions": { "accent": true }
}
```

- `themes`：非空数组；`id` 插件内唯一；`colorScheme` 为 `light`/`dark`（固定基底，不跟随系统；
  想做两套配色就声明两个条目）；`variables` 是语义 CSS 变量覆盖（键可省略 `--` 前缀），
  **只覆盖想改的子集**，未覆盖的落回基础浅/深方案。与基础主题条目 `light`/`dark` 重名的条目会被丢弃
  （插件其余条目仍生效；全部重名则该插件不提供主题）。
- `themeOptions.accent`：可选。声明后主题页为该插件提供内核预置的「强调色」设置项（值自动应用到
  `--accent` 系列变量，无需代码）。

主题插件与其它插件同一生命周期：安装后需在「已安装」列表启用；**平台至少保留一个启用的主题
插件**（停用/卸载最后一个会被拒绝）。默认主题插件与第三方主题插件同一条目契约（同一 `themes`/
`themeOptions` 声明）。
