# 桥 API：插件与 App 的通信接口

插件按运行平面通过统一的桥接口与 App 通信。桥是**能力注册表**模型：宿主与插件在同一
注册表里对等提供能力——宿主命名空间（不含点：`state`/`app`/`shell` 等）与插件命名空间
（反向域名含点）都能被 `bridge.call(namespace, method, args)` 调用，路由由宿主完成。
线协议见 [bridge-protocol.md](bridge-protocol.md)（子进程语言与 Worker 说同一套 JSON 消息）。

## worker / 子进程平面（tool / background / command）

逻辑入口在独立 Web Worker 或子进程中执行。**与 App 进程隔离、崩溃不影响 App**；worker 内
没有 `window`，只能经 `bridge` 触达 App 能力。注意：**Python 子进程以与 App 相同的
用户权限运行**（可读写文件/联网/执行程序）——完全自由模型下安全责任在用户知情与安装警告。
JS/TS 与 Python 的 `bridge` 方法签名一致（Python 顶层直接用注入的全局 `bridge`，
无需 import）。**插件一律经 `bridge` 输出结果，不要直接 `print` 到 stdout**——那会
污染协议通道（子进程 stdout 是桥的线）。

### 注册 AI 工具

```js
bridge.registerTool({
  name: "hello",                 // 工具名（发给模型的 id）
  description: "打招呼",          // 工具描述
  parameters: {                  // JSON Schema 参数
    type: "object",
    properties: { name: { type: "string", description: "称呼" } }
  },
  execute: async (args, ctx) => {
    // args = 模型生成的参数对象；ctx = { aborted, stream? }
    return { ok: true, summary: `你好，${args.name ?? "世界"}！` };
  },
  parallelSafe: true             // 可选：只读/无副作用工具标 true，可并行执行
});
```

注册后工具出现在设置页 Agent 的「插件」分组，勾选后即可被模型调用。

### 定义能力（插件可写能力）

```js
bridge.registerCapability({
  namespace: "com.acme.database",   // 反向域名必含点；与宿主命名空间/其他插件不冲突
  methods: {
    query: async (sql, ctx) => { /* 实现 */ },
    insert: async (row, ctx) => { /* 实现 */ },
  },
});
```

注册后，其他插件（或未来宿主功能）可经 `bridge.call("com.acme.database", "query", [sql])` 调用；
宿主在运行时之间中转。

### 调用能力（宿主 + 其他插件）

```js
const ver = await bridge.call("app", "version");                      // 宿主命名空间
await bridge.call("state", "write", [{ lastRun: Date.now() }]);       // 宿主命名空间
const rows = await bridge.call("com.acme.database", "query", [sql]);  // 其他插件的能力
```

> 签名是 `call(namespace, method, args)`，不要写成 `call("ns.method", …)`——`ns` 与 `method`
> 是两个独立参数。

### 流式调用（callStream）

```js
const cancel = bridge.callStream("shell", "exec", [{ command: "ls", args: ["-la"] }], {
  chunk: (d) => console.log(d),   // d = { stream: "stdout"|"stderr", data }
  end: (d) => console.log("exit", d),   // d = { code }
  error: (msg) => console.error(msg),
});
// cancel(); // 停止本地消费（宿主中转在 end/error 时自行清理）
```

宿主能力 `shell.exec` 流式输出 stdout/stderr；`call`（非流式）聚合返回 `{ code, stdout, stderr }`。

### 注册命令

```js
bridge.registerCommand({
  id: "greet",
  label: "打招呼",
  run: async (ctx) => { /* 执行逻辑 */ }
});
```

### 通用扩展点（registerContribution）

```js
bridge.registerContribution({
  point: "toolbar:button",          // 任意字符串；宿主/其他插件消费
  id: "greet",
  payload: { label: "你好", action: () => { /* 载荷内函数自动序列化为 fnId */ } },
});
```

### 读写自身状态（持久化）

```js
const state = await bridge.stateRead();      // 读插件自持数据（JSON 对象）
await bridge.stateWrite({ lastRun: Date.now() }); // 写（原子落盘）
```

### 事件：订阅 + 发布

```js
bridge.on("vault:switch", (payload) => { /* { root, id }：仓库切换 */ });
bridge.on("vault:clear", () => { /* 回到仓库选择页 */ });

// 跨插件：命名空间主题 `<插件id>:<主题>`
bridge.emit("com.acme.database:changed", { table: "t1" });
```

### 初始化完成

```js
bridge.ready(); // 顶层逻辑跑完时调用（可选；首个任意 bridge 调用即视为已激活）
```

## 主线程平面（panel / tableview / setting / app / node）

入口在主线程执行，可渲染 React 界面。代码经 `window.__atelyxPlugin__.forPlugin(插件id)`
取得 facade。**主线程插件与 App 同上下文**——完全自由信任模型：请只经 facade 注册贡献，
不要绕过它访问 App 内部（隔离只防崩溃，不防恶意）。

```js
const { React, h, registerPanel } = window.__atelyxPlugin__.forPlugin("com.example.hello");

function MyPanel() {
  return h("div", { style: { padding: 12 } }, "面板内容");
}
registerPanel({ kind: "com.example.hello.panel", label: "我的面板", component: MyPanel });
```

> 主线程 UI 代码运行在浏览器环境、不经过构建编译：**用 `React.createElement`（或 `h` 简写）而非 JSX**；
> TS/TSX 插件入口会由宿主转译后再注入（`runtime: "ts"`）。

facade 提供：

- `registerPanel({ kind, label, component })` — 注册工作区面板视图（kind 即视图类型，出现在「添加视图」菜单，与内置视图并列自选）
- `registerSetting({ key, label, component })` — 注册设置页条目（出现在设置左侧栏）
- `registerAppPage({ id, label, component })` — 注册应用级页面（插件命令可经 App 能力打开，全页接管）
- `registerNode({ type, component })` — 注册画布节点类型
- `registerCommand({ id, label, run })` — 注册全局命令（直接持有 run 函数）
- `registerTableView({ kind, label, component })` — 注册**表格编辑器内的表格视图**
- `registerContribution({ point, id?, payload })` — 通用扩展点注册（payload 直接持有引用）
- `listFiles()` / `openCanvasFile` / `openNote` / `openTable` — **仓库访问方法**（文件树 + 打开文件；任何面板插件可用，与内置搜索面板同一输入面）
- `React` / `h` — 构建组件所用

### 注册第三方搜索面板（与内置并列）

内置视图与第三方面板在**同一视图贡献注册表**里注册——kind 全局唯一：**重复注册、或占用内置
保留 kind（如 `canvas`/`search`）都会抛错**（冲突会中断该插件脚本的后续注册，插件作者须用
反向域名命名自己的 kind）。不同 kind 并列出现在「添加视图」菜单，用户自选、可同时打开在不同
面板；内置「搜索」始终可用，停用/卸载插件后其视图项随之消失。

```js
const { React, h, registerPanel, listFiles, openNote } = window.__atelyxPlugin__.forPlugin("com.example.hello-search");

function HelloSearch() {
  const [files, setFiles] = React.useState(null);
  React.useEffect(() => { listFiles().then(setFiles).catch(() => setFiles([])); }, []);
  // 用文件树实现自己的搜索……
  return h("div", null, "我的搜索面板");
}
// 自己的 kind，与内置「搜索」并列，用户自选
registerPanel({ kind: "com.example.hello-search", label: "示例搜索", component: HelloSearch });
```

- **输入面**：`listFiles()`（仓库文件树）+ `openCanvasFile`/`openNote`/`openTable`（打开文件），与内置面板同源（宿主经 provider 注入）。
- 完整可运行示例见 [`example/hello-search`](example/hello-search/README.md)。

### 表格数据（表格视图类插件）

```js
const unsub = bridge.subscribeTableData((snap) => { /* 快照结构见 types/plugin.ts */ });
bridge.selectTableRow(rowId);   // 跳选行（null = 取消）
const dataUrl = await bridge.resolveTableImage(entry);
```

表格视图组件渲染在表格工具条下方，样式建议只用 inline style + CSS 变量
（`var(--bg-primary)`/`var(--accent)` 等），不要依赖 Tailwind 类（插件不参与构建）。

## 宿主第一方能力（命名空间一览）

| 命名空间 | 方法 | 说明 |
| --- | --- | --- |
| `state` | `read` / `write` | 插件自持 JSON 状态（原子落盘） |
| `app` | `version` / `platform` | 宿主版本与平台 |
| `shell` | `exec` | 执行外部进程（敏感；流式：`callStream` 得 `chunk{stream,data}`/`end{code}`；`call` 聚合返回 `{ code, stdout, stderr }`） |
| `ai` | —（糖方法面） | 注册 AI 工具（`registerTool`）经此审计/标注 |
| `command` | —（糖方法面） | 注册命令（`registerCommand`） |
| `event` | —（糖方法面） | 事件订阅与发布（`on`/`emit`） |

能力面随版本扩展；命名空间不含点的是宿主能力（UI 显示注册表标签 + 敏感标记），
含点的是插件能力（原样显示）。

## 事件一览

| 事件 | payload | 说明 |
| --- | --- | --- |
| `vault:switch` | `{ root, id }` | 进入/切换仓库 |
| `vault:clear` | — | 回到仓库选择页 |

跨插件事件用 `emit` + 订阅 `<插件id>:<主题>`。

## 能力披露与信任模型

清单 `declares` 列出插件**会调用的能力命名空间**（与 `provides` 同一词汇表：宿主命名空间
如 `state`/`shell`，或他插件反向域名）——市场展示 + 管理页「声明 vs 实际调用」审计对照
（实际调用按命名空间记录）。**完全自由：无运行时拒绝**——能力声明是诚实披露，恶意插件
拦不住，安全责任在用户知情（安装警告）与稳定性隔离（Worker/子进程崩溃不影响 App）。
敏感能力（如执行外部程序 `shell`）在安装与详情页以「敏感」高亮标注。
