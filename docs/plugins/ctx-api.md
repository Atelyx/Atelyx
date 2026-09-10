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

| 服务 | 方法 | 说明 |
| --- | --- | --- |
| `ctx.state` | `read(pluginId)` / `write(pluginId, data)` | 插件自持状态（单 JSON 对象，按插件 id 隔离） |
| `ctx.app` | `version()` / `platform()` / `openPage(pageId)` | 宿主信息与页面打开 |
| `ctx.shell` | `exec(opts, handlers?)` | 执行外部程序（敏感：可执行任意命令；传 handlers 流式） |
| `ctx.vault` | `listFiles/readFile/readFileWindow/listDir/glob/grep/writeFile/editFile/appendFile/renameFile/moveFile/deleteFile/deleteDir/createFolder` | 仓库文件读写（写方法语义与 AI 文件工具一致；失败返回 `{ ok, summary }` 不抛断） |
| `ctx.dialog` | `pickDirectory()` / `pickFile(filters?)` / `saveFile(opts?)` | 系统对话框（用户取消返回 null） |
| `ctx.clipboard` | `readText()` / `writeText(text)` / `copyImage(dataUrl)` | 剪贴板读写（敏感） |
| `ctx.window` | `minimize()` / `toggleMaximize()` / `close()` | 窗口控制 |
| `ctx.ai` | `chat(req, handlers?)` / `listModels()` / `listAgents()` / `registerTool(opts)` | AI 会话（流式或聚合）、模型/Agent 列表、注册模型可调用的工具；`req.signal` 可中止流式（中止后按 `end` 收敛） |
| `ctx.collab` | `peers()` / `setPresence(view, file)` | 协作在线状态 |
| `ctx.canvas` | `snapshot()` / `addNode/updateNode/moveNode/deleteNode/addEdge/deleteEdge/selectNode` | 当前画布读写（由随应用分发的画布插件提供，停用即不可用） |
| `ctx.table` | `snapshot()` / `updateCell/addRow/removeRow/selectRow/resolveImage` | 当前表格读写（由随应用分发的表格插件提供，停用即不可用） |
| `ctx.note` | `currentFile()` / `open(file, title)` / `read(file?)` / `write(content)` / `save()` | 当前笔记读写（由随应用分发的笔记插件提供，停用即不可用） |
| `ctx.chat` | `sessions()` / `activeSession()` / `isStreaming()` / `openSession(id)` / `startSession()` / `sendMessage(content)` / `stop()` / `deleteSession(id)` | AI 会话管理（由随应用分发的 AI 对话插件提供，停用即不可用） |
| `ctx.history` | `list(kind, file)` / `rollback(kind, file, seq)` / `repoHistory()` | 领域历史读 + 回滚（`kind` = note/canvas/table） |
| `ctx.layout` | `activeLayoutId()` / `layouts()` / `addView(panelId, view)` / `op(op)` | 布局读 + 安全操作子集（权威在 Rust） |
| `ctx.uiState` | `read()` | 应用级 UI 使用状态读（只读非布局字段 + 布局镜像） |

依赖某个服务时用 apply 对象声明：`{ name, inject: ["table"], apply(ctx) { ... } }`——
服务缺失（如提供该服务的插件被停用）时插件不激活，管理页显示原因。

## 事件：`ctx.events.on` / `ctx.emit`

领域事件总线（订阅经 `ctx.effect` 包裹随插件撤销）：

| 事件 | 载荷 | 触发时机 |
| --- | --- | --- |
| `vault:switch` | `{ root, id }` | 进仓/切仓完成 |
| `vault:clear` | — | 离开仓库/回启动页：清空仓库上下文 |
| `canvas:changed` | `{ file }` | 当前画布变更（轻量信号，按需再调 `ctx.canvas.snapshot()`） |
| `table:changed` | `{ file }` | 当前表格变更（轻量信号） |
| `collab:changed` | `{ peers }` | 协作在线用户变更 |
| `vault:changed` | — | 仓库文件树变更 |
| `note:opened` | `{ file }` | 笔记打开/切换（`file: null` = 关闭当前笔记） |
| `note:changed` | `{ file }` | 当前笔记保存落盘（轻量信号，按需再读内容） |
| `chat:started` | `{ sessionId }` | AI 会话请求开始 |
| `chat:message` | `{ sessionId, role, content }` | AI 会话消息（`role: "user"` 发送时、`"assistant"` 流式完成后） |
| `chat:finished` | `{ sessionId }` | AI 会话结束（正常/中止/出错统一收敛） |

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

- `registerSetting` 注册的是**独立设置页**：并入设置页左侧栏（`plugin:<pluginId>:<key>` 形式 tab，
  整页由你的组件渲染）；想在既有设置页里追加一节，用下面的设置区块槽。
- 命令出现在管理页「运行命令」；`shortcut`（如 `"mod+k"`）由宿主统一监听绑定。
- `registerThemeSetting` 绑定激活的主题插件条目的设置值字典（`{ colorMode, accentColor, ... }`），
  `onChange(key, value)` 写回（value = `undefined` 删除键恢复默认）。

### 任意 UI 槽位 `registerUi`

```ts
// 工具条：单行内联控件
ctx.slots.registerUi({ slot: "toolbar/note/right", component: ToolbarBtn });
// 设置页区块：在某设置页内追加一节（纵向排列，可由多个区块并列）
ctx.slots.registerUi({ slot: "settings/files", component: AttachmentRulesBlock });
```

- 向任意具名 UI 槽位贡献一个组件（`toolbar/<region>`、`panelhead/<region>`、`contextmenu/<target>`、
  `settings/<tab>`、`statusbar/<region>` 等）；list 槽多贡献按 `priority` 降序渲染。
- 已接入的槽位：`toolbar/note/right`、`toolbar/table/right`、`toolbar/files`、`statusbar/canvas`、
  `panelhead/status`、`titlebar/right`，以及设置页区块
  `settings/general`、`settings/theme`、`settings/collab`、`settings/modelServices`、
  `settings/search`、`settings/files`、`settings/editor`（区块自行负责标题与卡片外观，
  可用 CSS 变量 `--bg-*`/`--border-*`/`--text-*`）。
- 宿主侧 `SlotListMount`/`SlotMount`（`components/plugins/SlotHost.tsx`）读取并渲染对应槽位。

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
