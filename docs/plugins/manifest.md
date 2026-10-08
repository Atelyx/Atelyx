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
    "keepMountedOnVaultSwitch": true, // 可选：切仓库保活（默认关闭）——切仓库触发的插件重载跳过本插件（见「切仓库保活」）
    "compositionPatch": [           // 可选：组合接管声明（本插件接管列出的行，见「组合接管」）
      { "target": "builtin.chatcore", "priority": 10 }
    ],
    "types": ["tool", "command"],   // 可选：附加分类
    "scope": "app",                 // 可选：app=个人工具（本机，默认）| vault=随仓库共享
    "tagline": "一句话简介",
    "atelyxVersionMin": "0.5.0",    // 可选：兼容的宿主版本下限
    // "atelyxVersionMax": "0.6.0", // 可选：兼容的宿主版本上限（不含）
    "platforms": ["windows-x64"],   // 可选：目标平台（windows-x64 / linux-x64 / android），缺省全平台
    "hostApiVersion": 1,            // 可选：插件契约版本（与宿主 App 版本解耦）
    "themes": [                     // 可选（type 含 theme 时）：声明式主题条目，无需代码
      { "id": "nord-light", "name": "Nord 浅色", "colorScheme": "light", "variables": { "accent": "#7c3aed" } }
    ],
    "themeOptions": { "accent": true }, // 可选：预置设置项声明（accent = 内核实现的强调色设置项）
    "shortcuts": [                  // 可选：全局快捷键声明（设置 → 快捷键展示与改键的数据源）
      { "id": "toggle-window", "label": "显示我的窗口", "key": "CmdOrCtrl+Shift+E" }
    ]
  }
}
```

## 全局快捷键声明（`shortcuts`）

- `shortcuts`：可选数组；每项 `id`（插件内唯一，运行时注册按它引用）/ `label`（设置页展示的动作名）/
  `key`（默认热键，OS accelerator 格式，如 `CmdOrCtrl+Shift+E`）均为非空字符串。
- 声明后插件运行时经 `ctx.shortcuts.registerDeclared(id, handler)`（或 `registerDeclaredWindowToggle`）
  注册：实际生效键 = 用户覆盖（设置 → 快捷键）→ 声明默认键；设置页改键即时生效（宿主重注册）。
- 未声明也可直接用 `registerGlobal(accelerator, …)` 原始串注册（旧路径兼容），但不会进入设置页的
  可自定义列表，只在「运行时注册（未声明）」区只读展示。

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
- **安卓端不支持依赖打包**：声明了 `dependencies`（或 `atelyx.bundle: true`）的插件在安卓上会被
  拒绝安装并给出原因；要跨平台分发，请让插件保持纯 TS、无 npm 依赖（入口本身仍可在安卓上转译）。
- 取下来的依赖按内容摘要缓存在本机，重复安装与回退不再下载。

产物跑在 WebView 里，所以只有**浏览器可用**的 npm 包能用：依赖 Node 内置模块（`fs`、
`child_process` 等）、原生扩展（`.node`）、或无法静态解析的 `require(变量)` 的包会在安装阶段失败
并指认到来源文件。这类需求改用 `ctx.native.invoke`（原始命令逃生舱）或 `ctx.process.exec`（执行外部程序）。

`react` 与 `react/jsx-runtime` 不需要（也不应）声明为依赖：宿主已提供全局 React，打包时会自动
接到宿主那一份上，避免同一个界面里出现两份 React。`react-dom` 不在接管范围内，需要就自行声明。

## 执行外部程序（`ctx.process`）

`ctx.process.exec(opts, handlers?)` 执行并等进程结束（不传 `handlers` 聚合输出返回，传则流式回调）；
`ctx.process.spawn(opts, handlers?)` 启动后立即返回句柄 `{ pid, write(), endInput(), cancel() }`——托管长驻服务（本机模型服务、sidecar 等）用后者。

- 程序来源全部放行：裸名（`node`、`python`…）按 PATH 解析，路径形态按给定值使用；等价任意命令执行。
- `cwd` 指定工作目录；`env` 追加/覆盖宿主环境（不传则完整继承，本机服务通常需要）。
- `input` 传入时进程启动后写一次 stdin 并立即关闭（一次性喂数据）。input 无法送达（进程未读 stdin 就退出）时只记宿主诊断日志，不影响聚合结果——以进程自身的退出码与输出为准。
- spawn 句柄的 `write(data)` 可反复写入 stdin，`endInput()` 关闭 stdin（长驻 helper 的双向通信用）；`cancel()` 结束该进程及其全部子孙（含 `sh -c`/`cmd.exe /C` 包出的实际服务进程，不只杀包装进程）。
- 进程按插件记账：插件停用 / 卸载 / 更新 / 回退 / 重载与应用退出时一律结束——启动还没返回就被停用也一样。
- 不要用它甩出「应在应用关闭后继续跑」的进程；要复用已在跑的服务，自查端口/接口后再决定是否启动（重载后上一轮句柄已失效）。
- 仍应给用户做停止入口（如设置页按钮）；`cancel()` 后再停用无副作用（已结束的句柄为 no-op）。
- 托管进程需要活过切仓库时，在清单声明 `keepMountedOnVaultSwitch`（见「切仓库保活」）。

## 切仓库保活（`keepMountedOnVaultSwitch`）

默认关闭。声明后，切换仓库触发的插件全量重载会跳过本插件：运行时、UI 贡献与托管进程原地保留，
`apply` 不重跑——仓库根的感知走 `vault:switch` 事件，插件收到后自行刷新对当前仓库的引用。

- 声明的插件仍随**停用、卸载、更新、回退、安装与跨窗口重载**正常重建，托管进程照常收尾——保活只豁免切仓库这一种重载。
- 典型用户：托管长驻本机服务（本机模型服务等）的插件——服务启动成本高，且实例本身不随仓库变化。
- 插件数据的落点（`privateDir()` 私有目录与键值存储）本就跨仓库一致，保活不改变数据归属。

## 组合接管（`compositionPatch`）

**默认组合里的每一行都是一个可被接管的装配位置**。声明 `compositionPatch` 后，本插件接管列出的行：
那些行的位置改跑本插件的入口，而行的 id、位置、插件自持数据（`ctx.state`/`ctx.storage`）与审计归属
保持不变（`ctx` 服务读、事件订阅、槽位贡献都记在**行**的 id 上）。

- 每项 `target`（目标行 id，如 `builtin.chatcore`）必须是非空合法行 id，且不能是自身行；`priority`
  可选（数字，缺省 0）——多个插件声明接管同一行时按 priority 高者生效。**至多声明一项**：
  一个插件只接管一行（同一份代码按行各装一份会让槽位与能力重复注册、后者覆盖前者）。
- **接管方的自身行不再独立装配**：同一份 `apply` 只跑一次（在被接管行的位置）。停用本插件即接管失效，
  目标行回退它的默认实现（或另一个生效的提供者）。
- **用户层恒胜**：用户在「设置 → 插件 → 组合接管」可把某行钉住到指定实现或改回该行默认实现；钉住后
  插件声明不再对该行生效。该设置是应用级偏好。
- 目标行不存在的声明不会生效，并在上述面板中提示。
- 仅接管**一个应用内的装配位置**，不改变插件运行方式：接管方的代码与其它插件同样在 WebView 主上下文
  执行、同样经 fiber 撤销。

典型用法：把某个默认实现换成自己的版本（例如以自己的对话引擎接管对话核心行），或替换任一默认面板行。

## 能力自发现与完全自由模型

**完全自由模型：无运行时拒绝**——插件与宿主在同一 realm 内运行，服务调用无门槛，开发者**无需
在清单里声明将使用哪些服务**：能力面由宿主在运行时自动发现（审计记录实际访问的服务、调用与
事件订阅），管理页插件详情随之展示。宿主服务里的敏感项（如 `shell`/`clipboard`）以「敏感」
高亮，安全责任落在用户知情（安装警告）。

## 仓库外文件访问与私有目录（`ctx.fs`）

仓库内文件走 `ctx.vault`（相对仓库根的路径）。仓库**外**文件走 `ctx.fs`，按**绝对路径**读写：

- **作用域**：`ctx.fs` 无目录授权门槛，方法面与 `ctx.vault` 镜像（`readFile`/`writeFile`/`listDir`/`createFolder`/`renameFile`/`moveFile`/`deleteFile`/`deleteDir`）。
- 入参为绝对路径（须规范化，不接受 `..` 段与相对路径）；插件代码不传插件 id，宿主按调用方自动绑定。
- 插件经此能触达用户机器上任意文件，发布前想清楚是否值得。
- **审计**：`fs` 属敏感服务，调用按调用方插件记录方法与路径（去重后的调用形态，非逐次完整日志），管理页插件详情「能力面」可见。
- **二进制**：`writeFileBase64(path, base64Data)` 原子写字节（base64 进出，字节原样落盘，与编码无关）；`readFileDataUrl(path)` 读为 dataURL（mime 按扩展名推断，未知扩展名按 `application/octet-stream`）。
- 模型工具（AI 文件工具）走 `ctx.vault`，限仓库根内，结构性够不到 `ctx.fs`。

### 插件私有目录（`ctx.fs.privateDir()`）

`privateDir()` 返回本插件专属目录的绝对路径（不存在则创建）——它就是插件自己的
落点，`ctx.fs` 全部方法（含二进制读写）对它开放，同样不开放给其他插件。目录位于插件数据的
`data/files/` 下：随插件卸载一并清除、更新保留。适合存放插件生成的文件——例如生成图片后**当下**
把字节写入私有目录，跨重启可读；不要把生成产物放在外部程序的临时目录里再指望它常驻。

- `ctx.fs` 调用即生效，无需声明；每次调用会被宿主审计记录（见上）。
- 插件键值存储（`data/state.json` / `data/kv.json`）在私有目录之外，`ctx.fs` 摸不到它们。

## 宿主兼容

- `atelyxVersionMin`/`atelyxVersionMax`：不匹配的插件在安装时会被拒绝（并回滚清理），启用时加载也会被拒绝。
- `hostApiVersion`：插件契约版本（`ctx` 服务面与 `ui` 注册面的语义版本，与宿主 App 版本解耦）。
  **缺省视为当前契约版本**；显式声明且与宿主不同时，安装与加载都会被拒绝并提示所需版本——
  这样破坏性契约变更会响亮失败，而不是静默坏掉。当前契约版本为 1。
- `platforms`：`windows-x64` / `linux-x64` / `android`，缺省全平台。

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

- `themes`：非空数组；`id` 插件内唯一；`colorScheme` 为 `light`/`dark`（固定基底，不跟随系统；想做两套配色就声明两个条目）。
- `variables` 是语义 CSS 变量覆盖（键可省略 `--` 前缀），**只覆盖想改的子集**，未覆盖的落回基础浅/深方案；
  与基础条目 `light`/`dark` 重名的条目被丢弃（插件其余条目仍生效；全部重名则该插件不提供主题）。
- 除配色变量外还可覆盖五个呈现变量：`--accent-grad`（实心强调面填充，默认纯强调色）、
  `--accent-glow`（选中辉光，默认透明阴影）、`--app-backdrop`（`body` 页面氛围底，默认 none）、
  `--app-base`（`html` 层不透明底色，默认该主题 `--bg-primary` 实色）、`--glass-filter`（浮层背景模糊，默认 none）。
- 把 `--bg-*` 设为半透明即可让氛围底透上来形成玻璃面；`--accent-grad`/`--accent-glow` 宜从 `var(--accent)` 派生，
  用户改强调色时整族跟随。
- `themeOptions.accent`：可选。声明后主题页为该插件提供内核预置的「强调色」设置项（值自动应用到
  `--accent` 系列变量，无需代码）。

主题插件与其它插件同一生命周期：安装后需在「已安装」列表启用；**平台至少保留一个启用的主题
插件**（停用/卸载最后一个会被拒绝）。默认主题插件与第三方主题插件同一条目契约（同一 `themes`/
`themeOptions` 声明）。
