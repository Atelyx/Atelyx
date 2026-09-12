# ctx API（插件运行时）

插件的 `apply(ctx, config)` 拿到插件上下文 `ctx`。所有能力都经 `ctx` 触达——类型化服务、
事件总线、UI 注册与副作用管理。**无全局变量、无隐藏入口**；入口须自包含，直接使用 `ctx`。
`config` 目前没有宿主侧配置来源（恒为 `undefined`），插件不要依赖它。

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
| `ctx.state` | `read(pluginId: string): Promise<unknown>` / `write(pluginId: string, data: unknown): Promise<void>` | 插件自持状态服务（按插件 id 读写；单 JSON 对象）。 |
| `ctx.storage` | `get(pluginId: string, key: string): Promise<unknown>` / `set(pluginId: string, key: string, value: unknown): Promise<void>` / `delete(pluginId: string, key: string): Promise<void>` / `keys(pluginId: string): Promise<string[]>` / `clear(pluginId: string): Promise<void>` | 插件键值存储服务（按插件 id 隔离，独立于 `ctx.state`；值须为 JSON 可序列化，整表落 `data/kv.json`）。 |
| `ctx.http` | `request(req: HttpRequestInput): Promise<HttpResponseResult>` | 通用 HTTP 请求服务（Rust 代理：CORS 绕行 + SSRF 防护；20s 超时 + 1MB 响应上限，内网/回环地址拒绝）。 |
| `ctx.notification` | `notify(input: NotificationInput): string` / `dismiss(id: string): void` | 应用内通知服务（右下角通知堆叠；`level` = info/success/warning/error，自动消失）。 |
| `ctx.app` | `version(): Promise<string>` / `platform(): Promise<string>` / `openPage(pageId: string): Promise<boolean>` | 宿主信息服务（版本 / 平台 / 打开插件应用页）。 |
| `ctx.shell` | `exec(opts: ShellExecOptions, handlers?: ShellStreamHandlers): Promise<ShellExecResult | undefined>` | 外部程序执行服务（敏感：宿主只登记 `sh`（Unix，配 `-c`）/ `cmd.exe`（Windows，配 `/C`），`args` 全开，等价任意命令执行）。 |
| `ctx.vault` | `listFiles(): Promise<FileTreeNode[]>` / `readFile(file: string): Promise<string>` / `readFileWindow(file: string, opts?: { offset?: number; limit?: number }): Promise<ReadWindowResult>` / `listDir(dir?: string): Promise<ListDirResult>` / `glob(pattern: string, opts?: { path?: string }): Promise<GlobVaultResult>` / `grep(pattern: string, opts?: { path?: string; include?: string }): Promise<GrepVaultResult>` / `writeFile(file: string, content: string): Promise<{ ok: boolean; summary: string }>` / `editFile(file: string, edits: { oldText: string; newText: string }[]): Promise<{ ok: boolean; summary: string }>` / `appendFile(file: string, content: string): Promise<{ ok: boolean; summary: string }>` / `renameFile(oldPath: string, newName: string): Promise<{ ok: boolean; summary: string; actualPath: string }>` / `moveFile(oldPath: string, targetDir: string): Promise<{ ok: boolean; summary: string; actualPath: string }>` / `deleteFile(path: string): Promise<{ ok: boolean; summary: string }>` / `deleteDir(dir: string, force?: boolean): Promise<{ ok: boolean; summary: string; needsConfirm?: boolean; itemCount?: number }>` / `createFolder(dir: string): Promise<{ ok: boolean; summary: string; path: string }>` | 仓库文件读写服务（读写全开；写方法语义与 AI 文件工具一致；失败返回 { ok:false, summary } 不抛断）。 |
| `ctx.dialog` | `pickDirectory(): Promise<string | null>` / `pickFile(filters?: DialogFilters[]): Promise<string | null>` / `saveFile(opts?: { defaultPath?: string; filters?: DialogFilters[] }): Promise<string | null>` | 系统对话框服务（用户取消返回 null）。 |
| `ctx.clipboard` | `readText(): Promise<string>` / `writeText(text: string): Promise<void>` / `copyImage(dataUrl: string): Promise<void>` | 剪贴板服务（文本 + 图片 dataURL；敏感：可读写用户剪贴板）。 |
| `ctx.window` | `minimize(): Promise<void>` / `toggleMaximize(): Promise<void>` / `close(): Promise<void>` | 窗口控制服务（自定义标题栏窗口）。 |
| `ctx.ai` | `chat(req: ChatRequest, handlers?: ChatStreamHandlers): Promise<ChatResult | undefined>` / `listModels(): Promise<Array<{ providerId: string; providerName: string; modelId: string; label: string }>>` / `listAgents(): Promise<Array<{ id: string; name: string }>>` / `registerTool(opts: PluginToolOptions): () => void` | AI 会话服务：模型/Agent 列表 + 流式对话 + 插件工具贡献；`req.signal` 可中止流式（中止后按 `end` 收敛）。 |
| `ctx.collab` | `peers(): CollabPeer[]` / `setPresence(view: string | null, file: string | null): void` | 协作在线状态服务（读 peers + 上报本端 presence）。 |
| `ctx.canvas` | `snapshot(): PluginCanvasSnapshot` / `addNode(node: { type: string; position: { x: number; y: number }; data?: Record<string, unknown> }): string` / `updateNode(nodeId: string, patch: Record<string, unknown>): void` / `moveNode(nodeId: string, position: { x: number; y: number }): void` / `deleteNode(nodeId: string): void` / `addEdge(edge: { source: string; target: string; sourceHandle?: string; targetHandle?: string; directed?: boolean; linkMode?: string; }): string` / `deleteEdge(edgeId: string): void` / `selectNode(nodeId: string | null): void` | 画布数据服务（由随应用分发的画布插件提供，停用即不可用；写方法要求已打开可写画布）。 |
| `ctx.table` | `snapshot(): PluginTableSnapshot` / `updateCell(rowId: string, fieldId: string, value: CellValue | undefined): void` / `addRow(): void` / `removeRow(rowId: string): void` / `selectRow(rowId: string | null): void` / `resolveImage(entry: string): Promise<string>` | 表格数据服务（由随应用分发的表格插件提供，停用即不可用；写操作要求已接线）。 |
| `ctx.note` | `currentFile(): string | null` / `open(file: string, title: string): void` / `read(file?: string): Promise<string>` / `write(content: string): Promise<void>` / `save(): Promise<void>` | 笔记内容服务（由随应用分发的笔记插件提供，停用即不可用；读写走当前仓库上下文的编辑器链）。 写入 `.md` 时若该笔记正被编辑且有未落盘输入，按「磁盘与本地正文不同」转冲突条由用户决策，不静默覆盖任何一侧。 |
| `ctx.chat` | `sessions(): EditorChatSession[]` / `activeSession(): EditorChatSession | null` / `isStreaming(): boolean` / `openSession(id: string): void` / `startSession(): void` / `sendMessage(content: string): Promise<void>` / `stop(): void` / `deleteSession(id: string): void` | AI 会话服务（由随应用分发的 AI 对话插件提供，停用即不可用；会话历史 + 发起/停止会话）。 |
| `ctx.history` | `list(kind: HistoryKind, file: string): Promise<HistoryVersion[]>` / `rollback(kind: HistoryKind, file: string, seq: number): Promise<void>` / `repoHistory(): RepoHistoryResult | null` | 领域历史服务（笔记/画布/表格的版本历史读 + 回滚）。 |
| `ctx.layout` | `activeLayoutId(): string | null` / `layouts(): WorkspaceLayout[]` / `addView(panelId: string, view: string): Promise<LayoutOpResult>` / `op(op: LayoutOp): Promise<LayoutOpResult>` | 布局服务（读取布局镜像 + 发布布局操作；`op` 与 Rust `LayoutOp` 逐字段对齐，改布局一律经 `layout_op`，布局权威在 Rust）。 |
| `ctx.uiState` | `read(): AppUiState` | 应用级 UI 使用状态读服务（只读非布局字段 + 布局镜像）。 |
| `ctx.slots` | `registerView(opts: RegisterViewOptions): () => void` / `registerTableView(opts: RegisterTableViewOptions): () => void` / `registerNode(opts: RegisterNodeOptions): () => void` / `registerEdge(opts: RegisterEdgeOptions): () => void` / `registerSetting(opts: RegisterSettingOptions): () => void` / `registerAppPage(opts: RegisterAppPageOptions): () => void` / `registerCommand(opts: RegisterCommandOptions): () => void` / `registerThemeSetting(opts: RegisterThemeSettingOptions): () => void` / `registerUi(opts: RegisterUiOptions): () => void` / `registerMenu(opts: RegisterMenuOptions): () => void` / `list(): readonly SlotDeclaration[]` | 插件 UI 注册服务（视图/节点/边/表格视图/设置项/应用页/命令/主题设置项/具名槽位/右键菜单）。 |
<!-- generated:ctx-api:services:end -->

依赖某个服务时用 apply 对象声明：`{ name, inject: ["table"], apply(ctx) { ... } }`——
服务缺失（如提供该服务的插件被停用）时插件不激活，管理页显示原因。

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
| `vault:changed` | — | emit | 仓库文件树变更。 |
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
  你的插件随后激活。
- 行序（装配顺序）由宿主决定，不面向用户配置：默认组合成员在前，其余插件按 id 追加。
