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
  "requires": ["vault", "shell"], // 可选：依赖的能力命名空间（宿主或他插件提供；懒解析，缺失在调用时报错）
  "declares": ["ai", "state", "event"], // 可选：披露将调用的能力命名空间（市场展示 + 管理页审计对照；无运行时门槛）
  "contributes": { "commands": [{ "id": "hi", "label": "你好" }] }, // 可选：静态贡献声明（纯元数据，展示/发现用）
  "atelyxVersionMin": "0.4.2",    // 可选：兼容的宿主版本下限
  // "atelyxVersionMax": "0.5.0", // 可选：兼容的宿主版本上限（不含）
  "platforms": ["windows-x64"],   // 可选：目标平台，缺省全平台
  "theme": {                      // 可选（type 含 theme 时）：声明式皮肤，无需代码
    "variables": { "accent": "#7c3aed" },  // CSS 变量覆盖（键可省略 -- 前缀）
    "dark": { "bg": "#0b0b0d" }            // 可选：暗色模式额外覆盖
  },
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
| `theme` | UI 皮肤（声明式，无需入口） |

## 多语言 `runtime`

逻辑平面支持三种语言，UI 平面永远在浏览器里跑 JS：

- `js`（缺省）：入口为主线程/Worker 直接执行的脚本。
- `ts`：入口 `.ts`/`.tsx` 源码即可用——宿主内置 esbuild-wasm 加载时转译，无需构建产物；
  仓库自带 `dist/` 预编译 JS 时跳过转译。**入口须自包含**（无 `import`/`export`——宿主只转译不打包；
  需要依赖请打成单文件或用 JS）。
- `python`：入口 `main.py`，宿主 spawn 解释器（`python`/`python3`，需安装并加入 PATH）经
  stdio 桥接入；插件顶层直接用注入的全局 `bridge`，无需 import。

## 能力命名空间 `provides` / `requires` / `declares`

插件与宿主在同一能力注册表里对等提供能力：宿主命名空间不含点（`state`/`app`/`shell`，及
`ai`/`command`/`event` 三个糖方法面），插件命名空间必为反向域名（含点）。三个字段同一词汇表：
`provides` = 提供的能力，`requires` = 依赖的能力（懒解析：缺失在调用时报错，不阻塞激活），
`declares` = 披露将调用的能力（市场展示 + 管理页「声明 vs 实际」审计对照）。

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

## 声明式皮肤（`theme`）

`theme` 类型插件只需在清单里声明 CSS 变量覆盖，无需任何代码。变量作用于 `:root`
（浅色）与暗色（`dark` 覆盖）。多个主题插件按 id 排序叠加，后者覆盖前者。
