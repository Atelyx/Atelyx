# 插件包清单（`package.json`）

清单是插件的唯一契约，放在插件根目录（git 仓库根，安装时以此为插件根）。npm 标准字段 +
嵌套 `atelyx` 块；未知字段/类型被安全跳过，只对结构性问题（缺字段、类型错误、未知主分类）报错。

```jsonc
{
  "name": "com.example.hello",     // 必须：反向域名式稳定标识（发布后不可变；至少两段，小写字母/数字/中划线）
  "version": "1.0.0",              // 必须：语义化版本
  "main": "index.ts",              // 必须：入口（相对插件根目录；.js/.ts/.tsx；纯 theme 插件可省略）
  "dependencies": { "nanoid": "^5.0.0" }, // 可选：运行时依赖（需同时提交 package-lock.json，见「依赖与打包」）
  "description": "详细描述（markdown）",
  "author": "作者",
  "license": "MIT",
  "keywords": ["效率"],
  "atelyx": {
    "name": "你好工具",              // 显示名（缺省 = name）
    "type": "tool",                 // 必须：主分类（见下表）
    "bundle": true,                 // 可选：显式要求宿主打包（入口拆成多文件但无依赖时；声明了 dependencies 即自动打包）
    "types": ["tool", "command"],   // 可选：附加分类
    "scope": "app",                 // 可选：app=个人工具（本机，默认）| vault=随仓库共享
    "tagline": "一句话简介",
    "atelyxVersionMin": "0.5.0",    // 可选：兼容的宿主版本下限
    // "atelyxVersionMax": "0.6.0", // 可选：兼容的宿主版本上限（不含）
    "platforms": ["windows-x64"],   // 可选：目标平台，缺省全平台
    "declares": ["table"],          // 可选：披露将访问的 Atelyx 服务（管理页「声明 vs 实际」审计对照的声明侧）
    "declaredDirs": ["~/Projects/my-app"], // 可选：披露并请求访问的仓库外目录（绝对路径或以 ~/ 开头；逐目录经用户批准后可用，见「外部目录访问」）
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
- TS 入口发布源码即可，宿主内置转译器加载时转译；**未打包**的入口须自包含（无运行时 import）。
- 声明了依赖或开启了打包的插件由宿主打成自包含产物（见下节），此时入口可以拆成多文件、按常规写法
  `import` 依赖与相邻模块。
- 纯 theme 插件（`type: "theme"` 且无其他代码类型）可省略 `main`——主题是声明式皮肤。

## 依赖与打包

插件可以只声明标准 npm 依赖，由宿主在**安装/更新时**解析、取件、校验并打进一个自包含产物；
运行时只求值产物——用户机器不需要 Node/pnpm，启动时也不需要联网。

```jsonc
{
  "main": "src/index.ts",
  "dependencies": { "nanoid": "^5.0.0" },
  "atelyx": { "type": "tool" }
}
```

- 声明 `dependencies` 时**必须同时提交 `package-lock.json`**（npm 7+ 生成）：宿主只照锁文件取件、
  不做版本区间解析——锁把每个包钉到具体版本与字节摘要，装出来的依赖可复现。
- 只支持 registry 的 tarball 依赖；`git` / `file` / `link` 来源一律拒绝。只取 `dependencies`：
  `devDependencies` / `peerDependencies` / `optionalDependencies` 不参与。
- 入口拆成多文件但不想声明依赖时，用 `atelyx.bundle: true` 显式开启打包（相对 `import` 会被内联）。
- 产物写在插件目录的 `.atelyx-dist/entry.js`，由宿主生成与维护：**不要手改，也不要提交**它
  （连同 `node_modules` 一起加进忽略，本地目录来源的实时引用同样会被写入产物）。
- 取下来的依赖按内容摘要缓存在本机，重复安装与回退不再下载。

产物跑在 WebView 里，所以只有**浏览器可用**的 npm 包能用：依赖 Node 内置模块（`fs`、
`child_process` 等）、原生扩展（`.node`）、或无法静态解析的 `require(变量)` 的包会在安装阶段失败
并指认到来源文件。这类需求改用 `ctx.native.invoke`（原始命令逃生舱）或 `ctx.shell.exec`（执行外部程序）。

`react` 与 `react/jsx-runtime` 不需要（也不应）声明为依赖：宿主已提供全局 React，打包时会自动
接到宿主那一份上，避免同一个界面里出现两份 React。`react-dom` 不在接管范围内，需要就自行声明。

## 执行外部程序（`ctx.shell`）

`ctx.shell.exec(opts, handlers?)` 执行并等进程结束（不传 `handlers` 聚合输出返回，传则流式回调）；
`ctx.shell.spawn(opts, handlers?)` 启动后立即返回句柄 `{ pid, cancel() }`——托管长驻服务（本机模型
服务、sidecar 等）用后者：`cancel()` 结束该进程及其全部子孙（含 `sh -c`/`cmd.exe /C` 包出来的实际
服务进程，不会只杀掉包装进程）。程序只能是 `sh`（Unix，配 `-c`）或 `cmd.exe`（Windows，配 `/C`），
参数全开即等价任意命令执行。

插件启动的进程**按插件记账**：插件停用、卸载、更新、回退以及宿主重载插件时都会结束它们——
长驻服务不该活过插件运行时（启动还没返回时被停用也一样会被结束）。插件因此不必自己登记清理，
但仍应把停止入口（设置页的按钮等）做给用户；`cancel()` 之后再停用不会有副作用（已结束的句柄为
no-op）。注意「进程随运行时结束」也意味着服务不能跨重载存活：要复用已在跑的服务，请自查端口/
接口后再决定是否启动（插件被重载时上一轮的 `{ pid, cancel }` 句柄已随作用域失效）。

## 披露 `declares` 与完全自由模型

**完全自由模型：无运行时拒绝**——插件与宿主在同一 realm 内运行，`declares` 是**披露不是约束**：
它只用于管理页与审计对照展示，少报/漏报不会拦住任何调用，安全责任落在用户知情（安装警告）。
宿主服务里的敏感项（如 `shell`/`clipboard`）在管理页以「敏感」高亮。

## 外部目录访问（`declaredDirs` + `ctx.fs`）

仓库内文件走 `ctx.vault`（相对仓库根的路径）。仓库**外**目录需要显式声明并经用户批准：

- **声明**：`atelyx.declaredDirs` 列绝对路径或以 `~/` 开头（`~` 由宿主解析为用户主目录；相对路径无基准，安装时拒绝）。
- **批准**：管理页插件详情「外部目录访问」区逐目录批准；只能批准清单里声明过的目录。批准结果持久化（跨更新保留、卸载即消失、跨窗口一致）。
- **访问**：批准后插件经 `ctx.fs` 读写该目录——方法面与 `ctx.vault` 镜像（`readFile`/`writeFile`/`listDir`/`createFolder`/`renameFile`/`moveFile`/`deleteFile`/`deleteDir`），入参为绝对路径，须落在已批准目录内（含子目录，符号链接越出即拒）。插件代码不传插件 id——宿主按调用方自动绑定。
- **撤销**：详情页撤销后立即失效（每次调用实时校验，无缓存窗口）。
- 模型工具（AI 文件工具）走 `ctx.vault`，不会触达 `ctx.fs` 的授权目录。

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
