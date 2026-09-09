# ctx API（插件运行时）

插件的 `apply(ctx, config)` 拿到插件上下文 `ctx`。所有能力都经 `ctx` 触达——类型化服务、
事件总线、UI 注册与副作用管理。**无全局变量、无隐藏入口**；入口须自包含，直接使用 `ctx`。

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
| `ctx.ai` | `chat(req, handlers?)` / `listModels()` / `listAgents()` | AI 会话（流式或聚合）、模型/Agent 列表 |
| `ctx.collab` | `peers()` / `setPresence(view, file)` | 协作在线状态 |
| `ctx.canvas` | `snapshot()` / `addNode/updateNode/moveNode/deleteNode/addEdge/deleteEdge/selectNode` | 当前画布读写（由内置画布插件提供，停用即不可用） |
| `ctx.table` | `snapshot()` / `updateCell/addRow/removeRow/selectRow/resolveImage` | 当前表格读写（由内置表格插件提供，停用即不可用） |

依赖某个服务时用 apply 对象声明：`{ name, inject: ["table"], apply(ctx) { ... } }`——
服务缺失（如对应的内置插件被停用）时插件不激活，管理页显示原因。

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

```ts
ctx.effect(() =>
  ctx.events.on("table:changed", () => {
    const snap = ctx.table.snapshot();
    // 刷新视图
  }),
);
```

## UI 注册：`ctx.slots`

### 工作区面板视图 `registerView`

```ts
ctx.slots.registerView({ kind: "com.example.panel", label: "我的面板", component: MyComponent });
```

- `kind`：插件内唯一（建议反向域名式）；注册后出现在工作区「添加面板」菜单。
- `component`：React 组件（无 props 契约）；宿主内 JSX 经转译引用 `React.createElement`
  （宿主已提供 React 全局）。

### 表格视图 `registerTableView`

```ts
ctx.slots.registerTableView({ kind: "com.example.timeline", label: "时间线", component: TimelineView });
```

- 注册后出现在表格编辑器工具条「视图」下拉；与表格视图选中联动经 `ctx.table.selectRow`，
  数据经 `ctx.table.snapshot()` + `table:changed` 事件，图片条目经 `ctx.table.resolveImage(entry)`。

## 配置

`apply(ctx, config)` 的第二参为插件配置（当前为 `{}` 占位，组合配置层开放后生效）。
