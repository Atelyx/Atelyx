# `atelyx.json` 清单格式

清单是插件的唯一契约，放在插件根目录（git 仓库根，安装时以此为插件根）。字段带格式版本号
`schemaVersion`（当前 = 2）：未知字段/类型/能力名被安全跳过，只对结构性问题报错。

```jsonc
{
  "schemaVersion": 2,             // 必须：清单格式版本（当前 = 2）
  "id": "com.example.hello",      // 必须：反向域名式稳定标识，发布后不可变（至少两段，小写字母/数字/中划线）
  "name": "你好工具",               // 必须：显示名
  "version": "1.0.0",             // 必须：语义化版本
  "type": "tool",                 // 必须：主分类（见下表）
  "types": ["tool", "command"],   // 可选：附加分类（一个插件可多类）
  "scope": "app",                 // 可选：app=个人工具（本机，默认）| vault=随仓库共享
  "runtime": "js",                // 可选：逻辑运行平面语言（js|ts|python，缺省 js；UI 平面永远跑 JS）
  "main": "plugin.js",            // 必须：逻辑入口（js/ts 为脚本，python 为子进程入口；纯 theme 插件可省略）
  "mainUi": "ui.js",              // 可选：主线程 UI 入口（任何语言都可附 JS UI；无 UI 可省）
  "provides": ["com.example.hello.data"], // 可选：提供的能力命名空间（反向域名必含点；其他插件可经 bridge.call 调用）
  "requires": ["vault", "shell"], // 可选：依赖的能力命名空间（宿主或他插件提供；启动前校验，缺失拒绝启用）
  "declares": ["ai", "state", "event"], // 可选：披露将调用的能力命名空间（市场展示 + 管理页审计对照；无运行时门槛）
  "contributes": { "commands": [{ "id": "hi", "label": "你好" }] }, // 可选：静态贡献声明（纯元数据，展示/发现用）
  "atelyxVersionMin": "0.4.2",    // 可选：兼容的宿主版本下限
  // "atelyxVersionMax": "0.5.0", // 可选：兼容的宿主版本上限（不含）
  "platforms": ["windows-x64"],   // 可选：目标平台，缺省全平台
  "themes": [                     // 可选（type 含 theme 时）：声明式主题条目，无需代码
    { "id": "nord-light", "name": "Nord 浅色", "colorScheme": "light", "variables": { "accent": "#7c3aed" } }
  ],
  "themeOptions": { "accent": true }, // 可选：预置设置项声明（accent = 内核实现的强调色设置项）
  "tagline": "一句话简介",
  "description": "详细描述（markdown）",
  "author": "作者",
  "license": "MIT",
  "tags": ["效率"]
}
```

## 类型取值

| `type` | 说明 |
| --- | --- |
| `tool` | AI 工具/命令（模型可调用） |
| `background` | 后台常驻服务（无界面） |
| `command` | 全局命令（逻辑经桥 RPC） |
| `panel` | 工作区面板视图（主线程 UI） |
| `tableview` | 表格编辑器内的多维表格视图（主线程 UI） |
| `setting` | 设置页条目（主线程 UI） |
| `app` | 应用级页面/模式（主线程 UI，可全页接管） |
| `node` | 画布节点（主线程 UI） |
| `theme` | 主题插件（声明式主题条目 + 可选设置项，无需入口） |

## 多语言 `runtime`

逻辑平面支持三种语言，UI 平面永远在浏览器里跑 JS：

- `js`（缺省）：入口为主线程/Worker 直接执行的脚本。
- `ts`：入口 `.ts`/`.tsx` 源码即可用——宿主内置 esbuild-wasm 加载时转译，无需构建产物；
  仓库自带 `dist/` 预编译 JS 时跳过转译。**入口须自包含**（无 `import`/`export`——宿主只转译不打包；
  需要依赖请打成单文件或用 JS）。
- `python`：入口 `main.py`，宿主 spawn 解释器（`python`/`python3`，需安装并加入 PATH）经
  stdio 桥接入；插件顶层直接用注入的全局 `bridge`，无需 import。

## 能力命名空间 `provides` / `requires` / `declares` / `replace`

插件与宿主在同一能力注册表里对等提供能力：宿主命名空间不含点（`state`/`app`/`shell`，及
`ai`/`command`/`event` 三个糖方法面），插件命名空间必为反向域名（含点）。四个字段同一词汇表：
`provides` = 提供的能力，`requires` = 依赖的能力（**启动前校验**：缺失/成环的插件拒绝启用，
管理页显示原因），`declares` = 披露将调用的能力（市场展示 + 管理页「声明 vs 实际」审计对照），
`replace` = 显式替换意图：要替换的插件能力命名空间（**必须同时声明在 `requires` 里**，否则
清单校验拒绝）——冲突注册时按后注册者替换，未声明 `replace` 的冲突默认拒绝。

## 披露 `declares` 与完全自由模型

**完全自由模型：无运行时拒绝**——`declares` 是诚实披露，安全责任在用户知情（安装警告）与
稳定性隔离（进程/Worker 崩溃不影响 App）。宿主命名空间的敏感标记（如 `shell`）由宿主能力
注册表携带，安装与详情页以「敏感」高亮。

## 宿主兼容

- `atelyxVersionMin`/`atelyxVersionMax`：不匹配的插件在安装时会被拒绝（并回滚清理）。
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
  想做两套配色就声明两个条目）；`variables` 是语义 CSS 变量覆盖（如 `--bg-primary`/`--text-*`/`--border` 等，
  键可省略 `--` 前缀），**只覆盖想改的子集**，未覆盖的落回内置浅/深基础方案。与内置基底
  `light`/`dark` 重名的条目会被拒绝。
- `themeOptions.accent`：可选。声明后主题页为该插件提供内核预置的「强调色」设置项（值自动应用到
  `--accent` 系列变量，无需代码）。
- 自定义设置项：可选。插件带 `mainUi` 入口时，可经主线程 facade 的
  `registerThemeSetting({ key, label, component })` 注册设置区块（组件接收
  `{ value, onChange(key, value) }`，值持久化到该插件的 `themeSettings` 条目，切主题跟随），
  渲染在设置页「主题」tab 的该主题设置区。

主题插件与其它插件同一生命周期：安装后需在「已安装」列表启用；**平台至少保留一个启用的主题
插件**（停用/卸载最后一个会被拒绝）。
