# `atelyx.json` 清单格式

清单是插件的唯一契约，放在插件根目录（git 仓库根，安装时以此为插件根）。字段带格式版本号
`schemaVersion`：未来新增字段不会破坏旧版 Atelyx（未知字段/未知类型被安全跳过）。

```jsonc
{
  "schemaVersion": 1,             // 必须：清单格式版本（当前 = 1）
  "id": "com.example.hello",      // 必须：反向域名式稳定标识，发布后不可变（至少两段，小写字母/数字/中划线）
  "name": "你好工具",               // 必须：显示名
  "version": "1.0.0",             // 必须：语义化版本
  "type": "tool",                 // 必须：主分类（见下表）
  "types": ["tool", "command"],   // 可选：附加分类（一个插件可多类）
  "scope": "app",                 // 可选：app=个人工具（本机，默认）| vault=随仓库共享
  "atelyxVersionMin": "0.3.7",    // 可选：兼容的宿主版本下限
  // "atelyxVersionMax": "0.5.0", // 可选：兼容的宿主版本上限（不含）
  "platforms": ["windows-x64"],   // 可选：目标平台，缺省全平台
  "main": "plugin.js",            // worker 平面入口（tool/background/command 逻辑；纯 theme 插件可省略）
  "mainUi": "ui.js",              // 可选：主线程 UI 入口（panel/tableview/setting/app/node；缺省用 main）
  "uses": ["vault:read", "ai:tool"], // 可选：声明的命令/能力清单（市场展示 + 敏感能力门槛）
  "permissions": { "vault:read": "读取笔记正文以分析" }, // 可选：能力 → 一句理由（安装/详情展示）
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

## 能力声明 `uses`（重要）

`uses` 列出插件会使用的命令/能力，安装与详情页会原样展示，帮助用户判断插件要做什么。
**敏感能力必须声明才能使用**（未声明即运行时拒绝）：

- `keychain:read` — 读取 API Key
- `shell` — 执行外部程序
- `vault:delete` — 删除仓库文件

其余能力（`vault:read`/`vault:write`/`vault:rename`/`ai:chat`/`ai:tool`/`search:web`/`web:fetch`/
`clipboard`/`window:manage`/`settings:read`/`settings:write`/`state:persist`/`events:subscribe`/`table:read`）
非敏感，声明后用于展示与审计；建议如实声明插件实际用到的能力。

## 宿主兼容

- `atelyxVersionMin`/`atelyxVersionMax`：不匹配的插件在安装时会被拒绝（并回滚清理）。
- `platforms`：`windows-x64` / `linux-x64`，缺省全平台。

## 作用域

- `app`（默认）：个人工具，装在 App 数据目录，跨仓库可用，不随仓库同步。
- `vault`：随仓库共享，装在仓库 `.atelyx/plugins/`，适合团队共用的插件；安装时会有「代码随仓库扩散」提示。

## 声明式皮肤（`theme`）

`theme` 类型插件只需在清单里声明 CSS 变量覆盖，无需任何代码。变量作用于 `:root`
（浅色）与暗色（`dark` 覆盖）。多个主题插件按 id 排序叠加，后者覆盖前者。
