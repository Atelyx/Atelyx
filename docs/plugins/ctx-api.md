# ctx API（插件运行时）

插件的 `apply(ctx, config)` 拿到插件上下文 `ctx`。所有能力都经 `ctx` 触达——类型化服务、
事件总线、UI 注册与副作用管理。**无全局变量、无隐藏入口**；能力一律从 `ctx` 取，不从入口模块
导入宿主模块。`config` 目前没有宿主侧配置来源（恒为 `undefined`），插件不要依赖它。

## 副作用管理：`ctx.effect`

注册订阅/接线等副作用时，一律经 `ctx.effect(fn)` 包裹：`fn` 内注册的东西（或返回的撤销函数）
在插件停用/卸载时**自动撤销**——生命周期由框架持有，插件不泄漏。

```ts
ctx.effect(() => {
  const off = ctx.events.on("canvas:changed", (p) => { /* ... */ });
  return off; // 可选：返回撤销函数，插件停用时执行
});
```

## 类型化服务：`ctx.<service>.<method>()`

<!-- generated:ctx-api:services:begin -->
<!-- 本表由 ctx 类型契约自动生成，勿手改；改契约后运行 pnpm run ctx-api 重新生成。 -->

| 服务 | 方法 | 说明 |
| --- | --- | --- |
| `ctx.state` | `read(): Promise<unknown>` / `write(data: unknown): Promise<void>` | 插件自持状态服务（按调用方插件隔离；单 JSON 对象）。归属 id 由宿主按调用方上下文绑定，API 不暴露。 |
| `ctx.storage` | `get(key: string): Promise<unknown>` / `set(key: string, value: unknown): Promise<void>` / `delete(key: string): Promise<void>` / `keys(): Promise<string[]>` / `clear(): Promise<void>` | 插件键值存储服务（按调用方插件隔离，独立于 `ctx.state`；值须为 JSON 可序列化，整表落 `data/kv.json`）。 归属 id 由宿主按调用方上下文绑定，API 不暴露。 |
| `ctx.http` | `request(req: HttpRequestInput): Promise<HttpResponseResult>` | 通用 HTTP 请求服务（Rust 代理：CORS 绕行 + URL 协议白名单；20s 超时 + 1MB 响应上限）。 地址按调用方身份分策略——本服务是插件面（可信主体），走本机/局域网策略：回环/私网/ULA 放行， 云元数据/链路本地仍拒；模型工具抓取（`fetch_web`）走公网策略。IP 字面量与 DNS 解析结果逐 IP 同口径校验 + 重定向每跳复检，连接只建立到已校验地址。 |
| `ctx.notification` | `notify(input: NotificationInput): string` / `dismiss(id: string): void` | 应用内通知服务（右下角通知堆叠；`level` = info/success/warning/error，自动消失）。 |
| `ctx.app` | `version(): Promise<string>` / `platform(): Promise<string>` / `openPage(pageId: string): Promise<boolean>` | 宿主信息服务（版本 / 平台 / 打开插件应用页）。 |
| `ctx.shell` | `exec(opts: ShellExecOptions, handlers?: ShellStreamHandlers): Promise<ShellExecResult | undefined>` / `spawn(opts: ShellExecOptions, handlers?: ShellStreamHandlers): Promise<ShellProcessHandle>` | 外部程序执行服务（敏感：程序只放行 `sh`（Unix，配 `-c`）/ `cmd.exe`（Windows，配 `/C`），`args` 全开，等价任意命令执行）。 插件启动的进程按调用方记账：插件停用/卸载时由宿主统一结束，应用退出时也一并结束——长驻服务 不该活过插件本身，更不该活过应用（被强杀时靠 Windows 作业对象兜底，Unix 该路径不保证）。 |
| `ctx.vault` | `listFiles(): Promise<FileTreeNode[]>` / `readFile(file: string): Promise<string>` / `readFileWindow(file: string, opts?: { offset?: number; limit?: number }): Promise<ReadWindowResult>` / `listDir(dir?: string): Promise<ListDirResult>` / `glob(pattern: string, opts?: { path?: string }): Promise<GlobVaultResult>` / `grep(pattern: string, opts?: { path?: string; include?: string }): Promise<GrepVaultResult>` / `writeFile(file: string, content: string): Promise<{ ok: boolean; summary: string }>` / `editFile(file: string, edits: { oldText: string; newText: string }[]): Promise<{ ok: boolean; summary: string }>` / `appendFile(file: string, content: string): Promise<{ ok: boolean; summary: string }>` / `renameFile(oldPath: string, newName: string): Promise<{ ok: boolean; summary: string; actualPath: string }>` / `moveFile(oldPath: string, targetDir: string): Promise<{ ok: boolean; summary: string; actualPath: string }>` / `deleteFile(path: string): Promise<{ ok: boolean; summary: string }>` / `deleteDir(dir: string, force?: boolean): Promise<{ ok: boolean; summary: string; needsConfirm?: boolean; itemCount?: number }>` / `createFolder(dir: string): Promise<{ ok: boolean; summary: string; path: string }>` | 仓库文件读写服务（读写全开；写方法语义与 AI 文件工具一致；失败返回 { ok:false, summary } 不抛断）。 |
| `ctx.fs` | `readFile(path: string): Promise<string>` / `writeFile(path: string, content: string): Promise<{ ok: boolean; summary: string }>` / `listDir(path: string): Promise<ListDirResult>` / `createFolder(path: string): Promise<{ ok: boolean; summary: string; path: string }>` / `renameFile(path: string, newName: string): Promise<{ ok: boolean; summary: string; actualPath: string }>` / `moveFile(path: string, targetDir: string): Promise<{ ok: boolean; summary: string; actualPath: string }>` / `deleteFile(path: string): Promise<{ ok: boolean; summary: string }>` / `deleteDir(path: string, force?: boolean): Promise<{ ok: boolean; summary: string; needsConfirm?: boolean; itemCount?: number }>` | 仓库外授权目录文件读写服务（外部文件服务面）：方法面与 `vault` 镜像，但入参是绝对路径， 须落在该插件经用户批准的授权目录内（Rust 侧实时校验，撤销立即失效）。 插件代码不传插件 id——宿主按当前 fiber 绑定（与 state/storage 同机制）。 模型工具（AI 文件工具）走 `vault`，结构性够不到本面。 |
| `ctx.dialog` | `pickDirectory(): Promise<string | null>` / `pickFile(filters?: DialogFilters[]): Promise<string | null>` / `saveFile(opts?: { defaultPath?: string; filters?: DialogFilters[] }): Promise<string | null>` | 系统对话框服务（用户取消返回 null）。 |
| `ctx.clipboard` | `readText(): Promise<string>` / `writeText(text: string): Promise<void>` / `copyImage(dataUrl: string): Promise<void>` | 剪贴板服务（文本 + 图片 dataURL；敏感：可读写用户剪贴板）。 |
| `ctx.window` | `minimize(): Promise<void>` / `toggleMaximize(): Promise<void>` / `close(): Promise<void>` | 窗口控制服务（自定义标题栏窗口）。 |
| `ctx.ai` | `chat(req: ChatRequest, handlers?: ChatStreamHandlers): Promise<ChatResult | undefined>` / `listModels(): Promise<Array<{ providerId: string; providerName: string; modelId: string; label: string }>>` / `listAgents(): Promise<Array<{ id: string; name: string }>>` / `registerTool(opts: PluginToolOptions): () => void` | AI 会话服务：模型/Agent 列表 + 流式对话 + 插件工具贡献；`req.signal` 可中止流式（中止后按 `end` 收敛）。 |
| `ctx.collab` | `peers(): CollabPeer[]` / `setPresence(view: string | null, file: string | null): void` / `sendMessage(channel: string, payload: unknown, opts?: { to?: number }): boolean` / `myPeer(): CollabMyPeer` | 协作服务（读 peers + 上报 presence + 插件通用消息收发）。 |
| `ctx.canvas` | `snapshot(): PluginCanvasSnapshot` / `addNode(node: { type: string; position: { x: number; y: number }; data?: Record<string, unknown> }): string` / `updateNode(nodeId: string, patch: Record<string, unknown>): void` / `moveNode(nodeId: string, position: { x: number; y: number }): void` / `deleteNode(nodeId: string): void` / `addEdge(edge: { source: string; target: string; sourceHandle?: string; targetHandle?: string; directed?: boolean; linkMode?: string; }): string` / `deleteEdge(edgeId: string): void` / `selectNode(nodeId: string | null): void` | 画布数据服务（由随应用分发的画布插件提供，停用即不可用；写方法要求已打开可写画布）。 |
| `ctx.table` | `snapshot(): PluginTableSnapshot` / `updateCell(rowId: string, fieldId: string, value: CellValue | undefined): void` / `addRow(): void` / `removeRow(rowId: string): void` / `selectRow(rowId: string | null): void` / `resolveImage(entry: string): Promise<string>` | 表格数据服务（由随应用分发的表格插件提供，停用即不可用；写操作要求已接线）。 |
| `ctx.note` | `currentFile(): string | null` / `open(file: string, title: string): void` / `read(file?: string): Promise<string>` / `write(content: string): Promise<void>` / `save(): Promise<void>` | 笔记内容服务（由随应用分发的笔记插件提供，停用即不可用；读写走当前仓库上下文的编辑器链）。 写入 `.md` 时若该笔记正被编辑且有未落盘输入，按「磁盘与本地正文不同」转冲突条由用户决策，不静默覆盖任何一侧。 |
| `ctx.chat` | `sessions(): EditorChatSession[]` / `activeSession(): EditorChatSession | null` / `isStreaming(): boolean` / `openSession(id: string): void` / `startSession(): void` / `sendMessage(content: string): Promise<void>` / `stop(): void` / `deleteSession(id: string): void` | AI 会话服务（由随应用分发的 AI 对话插件提供，停用即不可用；会话历史 + 发起/停止会话）。 |
| `ctx.history` | `list(kind: HistoryKind, file: string): Promise<HistoryVersion[]>` / `rollback(kind: HistoryKind, file: string, seq: number): Promise<void>` / `repoHistory(): RepoHistoryResult | null` | 领域历史服务（笔记/画布/表格的版本历史读 + 回滚）。 |
| `ctx.layout` | `activeLayoutId(): string | null` / `layouts(): WorkspaceLayout[]` / `addView(panelId: string, view: string): Promise<LayoutOpResult>` / `op(op: LayoutOp): Promise<LayoutOpResult>` | 布局服务（读取布局镜像 + 发布布局操作；`op` 与 Rust `LayoutOp` 逐字段对齐，改布局一律经 `layout_op`，布局权威在 Rust）。 |
| `ctx.uiState` | `read(): AppUiState` | 应用级 UI 使用状态读服务（只读非布局字段 + 布局镜像）。 |
| `ctx.slots` | `registerView(opts: RegisterViewOptions): () => void` / `registerTableView(opts: RegisterTableViewOptions): () => void` / `registerNode(opts: RegisterNodeOptions): () => void` / `registerEdge(opts: RegisterEdgeOptions): () => void` / `registerSetting(opts: RegisterSettingOptions): () => void` / `registerAppPage(opts: RegisterAppPageOptions): () => void` / `registerCommand(opts: RegisterCommandOptions): () => void` / `registerThemeSetting(opts: RegisterThemeSettingOptions): () => void` / `registerUi(opts: RegisterUiOptions): () => void` / `registerMenu(opts: RegisterMenuOptions): () => void` / `decorate(opts: RegisterDecorateOptions): () => void` / `declare(opts: RegisterDeclareOptions): () => void` / `host(slot: string): () => ReactNode` / `list(): readonly SlotDeclaration[]` | 插件 UI 注册服务（视图/节点/边/表格视图/设置项/应用页/命令/主题设置项/具名槽位/右键菜单/装饰器）。 |
| `ctx.services` | `list(): ServiceInfo[]` / `get(name: K): Context[K] | undefined` | 服务注册表查询服务（ctx.services）：插件据此发现当前真实可用的服务面与提供者。 宿主内核提供平台服务（无 provider）；插件经 ctx.root.provide 提供的服务带提供者插件 id。 |
| `ctx.native` | `invoke(command: string, args?: Record<string, unknown>): Promise<unknown>` | 原始 Rust 命令逃生舱（ctx.native.invoke）：未封装的服务能力经此触达，调用进审计。 |
<!-- generated:ctx-api:services:end -->

依赖某个服务时用 apply 对象声明：`{ name, inject: ["table"], apply(ctx) { ... } }`——
服务缺失（如提供该服务的插件被停用）时插件不激活，管理页显示原因。

**可选依赖**：inject 值形如 `{ foo: { optional: true } }` 的条目不阻断激活——缺失时 apply
照常执行，插件在内经 `ctx.services.get("foo")` 判空降级（返回 `undefined`，不抛错）；
存在时返回该服务对象。**注意**：可选依赖不能直接 `ctx.foo` 属性访问（不在 inject 声明内
会抛错），一律经 `ctx.services.get("foo")` 读取。

**服务发现**：`ctx.services.list()` 返回当前全部已注册服务面（服务名 + 提供者插件 id；
宿主内核提供的平台服务无 provider 字段）。插件经 `ctx.provide`（自身 ctx）注册的服务
带提供者插件 id；服务存在与否随时反映真实运行状态（停用提供插件即消失）。

**原始命令逃生舱**：未封装成 `ctx` 服务的 Rust 命令经 `ctx.native.invoke(command, args)`
调用（敏感：等价原始命令执行）。调用形状（命令名 + 参数个数）进管理页审计，参数原文
不进审计。`ctx` 仍是推荐能力面，逃生舱只用于「宿主未开放且必须触达」的场景。

## 事件：`ctx.events.on` / `ctx.emit`

领域事件总线（订阅经 `ctx.effect` 包裹随插件撤销）：

<!-- generated:ctx-api:events:begin -->
<!-- 本表由 ctx 类型契约自动生成，勿手改；改契约后运行 pnpm run ctx-api 重新生成。 -->

| 事件 | 载荷 | 分派 | 说明 |
| --- | --- | --- | --- |
| `vault:switch` | `{ root: string; id: string }` | emit | 进仓/切仓完成广播（载荷 { root, id }）。 |
| `vault:clear` | — | emit | 离开仓库/回启动页：清空仓库上下文（插件据此丢弃 vault 级驻留态）。 |
| `canvas:changed` | `{ file: string | null }` | emit | 当前画布变更（轻量信号：只带 file，按需再调 ctx.canvas.snapshot()）。 |
| `table:changed` | `{ file: string | null }` | emit | 当前表格变更（轻量信号）。 |
| `collab:changed` | `{ peers: CollabPeer[] }` | emit | 协作在线用户变更。 |
| `collab:message` | `{ peerId: number; channel: string; payload: unknown }` | emit | 收到同房间其他成员经协作通道发来的插件消息（不含自己；payload 为发送方原样透传的 JSON）。 |
| `vault:changed` | — | emit | 仓库文件树变更。 |
| `note:before-save` | `{ file: string; content: string }` | serial | 笔记保存前钩子（serial：顺序执行；返回 { veto } 阻断本次保存，返回 { content } 改写落盘内容）。 |
| `ai:before-request` | `{ model: string; messages: LlmMessage[]; tools?: ToolSchema[]; }` | serial | AI 请求发出前钩子（serial：顺序执行；返回 { veto } 阻断本次请求，返回 { messages } 改写请求消息）。 |
| `note:opened` | `{ file: string | null }` | emit | 笔记打开/切换（file = null = 关闭当前笔记）。 |
| `note:changed` | `{ file: string | null }` | emit | 当前笔记内容变更（保存落盘后发出；按需再调 note 服务读内容）。 |
| `chat:started` | `{ sessionId: string }` | emit | AI 会话开始（发起请求）。 |
| `chat:message` | `{ sessionId: string; role: "user" | "assistant"; content: string }` | emit | AI 会话消息（角色 + 内容；assistant 消息在流式完成后发出，非逐 token）。 |
| `chat:finished` | `{ sessionId: string }` | emit | AI 会话结束（正常 / 中止 / 出错统一收敛）。 |
<!-- generated:ctx-api:events:end -->

```ts
ctx.effect(() =>
  ctx.events.on("table:changed", () => {
    const snap = ctx.table.snapshot();
    // 刷新视图
  }),
);
```

### serial 事件：veto / 改写（`note:before-save`、`ai:before-request`）

serial 事件是宿主管线的**拦截面**：监听器按注册顺序依次执行，返回对象与载荷合并后传给下一个监听器。
两类返回值：

- `{ veto: true }`：阻断管线——`note:before-save` 时本次保存不落盘（内容保留在挂起输入，不丢）；
  `ai:before-request` 时本次请求不发出（对话按错误收敛）。后续监听器不再执行。
- `{ content }` / `{ messages }`：改写管线——把改写后的值传给下一个监听器，最终落盘/发出。

```ts
// 保存前校验：含敏感词拒绝落盘（建议同时用 ctx.notification 说明原因，宿主不代发提示）
ctx.effect(() =>
  ctx.events.on("note:before-save", (p) => {
    if (p.content.includes("机密")) return { veto: true };
    return { content: normalize(p.content) }; // 改写落盘内容
  }),
);

// AI 请求上下文注入：每次对话请求（含工具轮次）自动附加当前文档内容
ctx.effect(() =>
  ctx.events.on("ai:before-request", (p) => {
    if (p.model === "gpt-4o") return; // 只读参考：model / tools
    return { messages: [...p.messages, { role: "system", text: "请用中文回答" }] };
  }),
);
```

- **作用域**：`note:before-save` 覆盖所有笔记写盘路径（自动保存 / 会话 flush / 关窗 flush /
  `ctx.note.write` / 历史回滚 / 冲突保留本地）；`ai:before-request` 覆盖面板、画布与 `ctx.ai.chat`
  的每次流式请求（工具轮次每轮一次），标题生成等一次性短任务不经此面。
- **异常隔离**：单个监听器抛错只记录并继续（保存/请求照常，按上一值前进），不会因插件 bug 中断宿主管线。
- 监听器返回值只改写声明中的字段（`content` / `messages`），载荷其余字段只读；`ai:before-request`
  载荷不含供应商密钥。

## UI 注册：`ctx.slots`

所有 `register*` 返回的撤销函数（或经 `ctx.effect` 包裹时）在插件停用/卸载时自动撤销。
`single` 槽（视图/节点/边/表格视图）按 `priority` 取胜出者（higher wins，缺省 0）——设更高
`priority` 即可**替换**随应用分发的同 kind 视图/节点/边（如 `registerView({ kind: "note", priority: 10 })` 替换默认笔记编辑器）。

### 工作区面板视图 `registerView`

```ts
ctx.slots.registerView({ kind: "com.example.panel", label: "我的面板", component: MyComponent });
ctx.slots.registerView({ kind: "note", label: "我的笔记", component: MyNote, priority: 10 }); // 替换默认笔记视图
```

- `kind`：插件内唯一（建议反向域名式）；注册后出现在工作区「添加面板」菜单（任意插件可注册任意 kind，`ViewKind` 已开放）。
- `component`：React 组件（无 props 契约）；宿主内 JSX 经转译引用 `React.createElement`（宿主已提供 React 全局）。
- `priority`：可选，替换同 kind 的默认实现时设高值。

### 画布节点 / 边 `registerNode` / `registerEdge`

```ts
ctx.slots.registerNode({ type: "com.example.card", component: MyNode });
ctx.slots.registerEdge({ type: "com.example.link", component: MyEdge });
```

- 注册后 CanvasView `nodeTypes`/`edgeTypes` 合并；同名 `type` 经 `priority` 胜负（可替换随应用分发的 `conversation`/`text` 等）。

### 表格视图 `registerTableView`

```ts
ctx.slots.registerTableView({ kind: "com.example.timeline", label: "时间线", component: TimelineView });
```

- 注册后出现在表格编辑器工具条「视图」下拉；与表格视图选中联动经 `ctx.table.selectRow`，
  数据经 `ctx.table.snapshot()` + `table:changed` 事件，图片条目经 `ctx.table.resolveImage(entry)`。

### 设置项 / 应用页 / 命令 / 主题设置项

```ts
ctx.slots.registerSetting({ key: "com.example", label: "我的设置", component: SettingsComp });
ctx.slots.registerAppPage({ id: "com.example.page", label: "我的应用页", component: PageComp });
ctx.slots.registerCommand({ id: "say", label: "打招呼", run: () => console.log("hi"), shortcut: "mod+k" });
ctx.slots.registerThemeSetting({ key: "accent", label: "强调色", component: AccentComp });
```

- `registerSetting` 注册的是**独立设置页**：并入设置页左侧栏（tab 以你传入的 `key` 命名，
  整页由你的组件渲染）；想在既有设置页里追加一节，用下面的设置区块槽。
  `key` 在插件内唯一；设置页左侧栏的 tab 以 `pluginId` + `key` 共同标识，不同插件的同名 `key`
  互不冲突（仍建议带自身命名空间前缀，便于用户辨认）。
- 命令出现在管理页「运行命令」；`shortcut`（如 `"mod+k"`）由宿主统一监听绑定。
- `registerThemeSetting` 绑定激活的主题插件条目的设置值字典（`{ colorMode, accentColor, ... }`），
  `onChange(key, value)` 写回（value = `undefined` 删除键恢复默认）。

### 具名 UI 槽位 `registerUi`

```ts
// 工具条：单行内联控件
ctx.slots.registerUi({ slot: "toolbar/note/right", component: ToolbarBtn });
// 设置页区块：在某设置页内追加一节（纵向排列，可由多个区块并列）
ctx.slots.registerUi({ slot: "settings/files", component: AttachmentRulesBlock });
```

- 向已声明的具名 UI 槽位（`ctx.slots.list()` 可查，如 `toolbar/note/right`、`settings/files`）贡献一个
  组件；list 槽多贡献按 `priority` 降序渲染。右键菜单项不走本方法（载荷形状不同），见下节 `registerMenu`。
- 槽位须先在宿主声明表登记：**固定具名槽未声明即注册失败**并给近似槽名提示，载荷须匹配声明的字段
  契约（缺必需字段或带未知字段即失败）——失败即该插件行标 failed + 可读原因，不再静默丢失。
- 开放 kind 槽按前缀放行、可自定 kind/type，但各有专用方法（本方法只传 `component`，用于上面的固定
  具名槽）：`view`→`registerView`、`tableview`→`registerTableView`、`node`/`edge`→`registerNode`/
  `registerEdge`、`contextmenu`→`registerMenu`。
- `ctx.slots.list()` 返回声明表（key / 基数 / 载荷字段 / 用途），据此发现可贡献的位置。
- 已接入的槽位：`toolbar/note/right`、`toolbar/table/right`、`toolbar/files`、`statusbar/canvas`、
  `panelhead/status`、`titlebar/right`，以及设置页区块
  `settings/general`、`settings/theme`、`settings/collab`、`settings/modelServices`、
  `settings/search`、`settings/files`、`settings/editor`（区块自行负责标题与卡片外观，
  可用 CSS 变量 `--bg-*`/`--border-*`/`--text-*`）。
- 宿主侧 `SlotListMount`/`SlotMount`（`components/plugins/SlotHost.tsx`）读取并渲染对应槽位。

### 右键菜单项 `registerMenu`

```ts
ctx.slots.registerMenu({
  target: "canvas",          // 菜单目标（当前宿主渲染点：canvas）
  label: "统计选中节点",
  onClick: () => countSelected(),
  priority: 10,              // 可选，list 槽按 priority 降序
});
```

- 向指定菜单目标追加一项；`label` 与 `onClick` 必填。载荷形状与 `registerUi`（只传 `component`）不同，
  两者不可混用。
- 菜单目标须先在声明表登记（`contextmenu/<target>`）：注册未声明的目标即失败并提示可用目标。
  当前宿主渲染点为 `canvas`，可用目标见 `ctx.slots.list()`。

### 插件自声明槽位 `declare` / `host`

插件可以在自己的面板里声明新槽位给其他插件用——槽位注册表成为全局组合织物，UI 扩展不再有宿主/插件分界。
完整演练见 [自定义槽位指南](custom-slots.md)。

```ts
// 插件 A：声明 + 承载
ctx.slots.declare({
  key: "toolbar/com.example.panel",   // 前缀声明：toolbar/com.example.panel/* 下任意槽可被贡献；也可写精确 key
  prefix: true,
  cardinality: "list",       // single 为胜出、list 为多贡献有序（贡献方须按此基数注册）
  required: ["component"],   // 载荷契约：贡献方必须提供 component 才会被 host 渲染
});
const PanelToolbarSlot = ctx.slots.host("toolbar/com.example.panel/export"); // 返回渲染组件
```

```ts
// 插件 B：贡献（同一应用内另一插件）
ctx.slots.registerUi({ slot: "toolbar/com.example.panel/export", component: ExportButton });
```

- **先到先得**：key 未被宿主或他插件占用即可声明；与既有声明（同名 / 前缀吞并 / 落入他人前缀覆盖集）
  重叠即失败并**指名占用者插件 id**。宿主已声明的 key 与开放前缀受保护，声明与之重叠同样失败。
- 声明随插件停用撤销；对声明槽的贡献与声明各自独立撤销——停用声明方后他插件再向该槽贡献会失败
  （「未声明的槽位」可见，不静默）。
- `host(slot)` 返回 React 组件（list = 全部贡献按 priority 降序、single = 胜出者），在插件自己渲染的
  界面里承载他插件贡献；调用时槽须已声明（宿主插件先 `declare` 后 `host`）。贡献按声明的载荷契约校验。
- 声明同样出现在 `ctx.slots.list()`（合并视图含宿主声明表与插件运行时声明），其他插件可发现并贡献。

## AI 工具：`ctx.ai.registerTool`

```ts
ctx.ai.registerTool({
  name: "com_example_lookup",          // 工具名（模型可见；小写字母/数字/下划线）
  description: "按关键词查公司内部术语表",
  parameters: { type: "object", properties: { q: { type: "string" } }, required: ["q"] },
  run: async (args) => {
    const text = await ctx.vault.readFile(`术语表/${String(args.q)}.md`);  // 能力仍走 ctx
    return `已读取 ${String(args.q)}：${text.slice(0, 200)}`;
  },
});
```

- 注册后进入 Agent 名册的「插件」分类（**需在设置 → Agent 里勾选该工具才生效**）；停用/卸载插件时自动撤销。
- `run(args, { signal })` 返回气泡摘要文本；`signal` 在用户中止时置位（长任务请自行检查并尽快返回）；
  抛错即记为失败结果（不中断整轮对话）；工具名与已有工具重复会被拒绝（防覆盖宿主工具）。
- 参数校验/摘要/结果回填由宿主补齐；工具内部访问仓库、网络等能力仍统一经 `ctx`（受同一审计与披露）。

## 组合与替换

- 你的插件与随应用分发的插件同一条注册表/生命周期/审计，无特权差别：安装 + 启用即可生效。
- **替换/增强默认实现**：在 `apply` 里注册同 kind 的视图/节点/边并给更高的 `priority`
  （single 槽 `priority` 高者胜出，同值后者胜）——替换关系由插件自己声明，用户不需要额外配置。
- **依赖提供者**：用 apply 对象的 `inject` 声明依赖的服务；提供者在默认组合中先挂载，
  你的插件随后激活。可选依赖见上文「可选依赖」段（`{ foo: { optional: true } }` + `ctx.services.get`）。
- 行序（装配顺序）由宿主决定，不面向用户配置：默认组合成员在前，其余插件按 id 追加。
