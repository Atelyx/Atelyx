# 桥协议：插件与宿主之间的线协议（跨语言契约）

所有运行平面（浏览器 Web Worker 与 Python 子进程）与宿主（`services/plugins/bridge.ts`）
说**同一套 JSON 消息**，逐行传输（子进程 = stdin/stdout 每行一个 JSON；worker = postMessage）。

> 这是语言 SDK 与宿主实现的唯一契约：新增语言 = 照此协议写一个桥 shim，宿主侧零改动。

## 消息总览

| 方向 | kind | 说明 |
| --- | --- | --- |
| 插件 → 宿主 | `call` | 调用桥方法（`{ seq, method, args }`） |
| 宿主 → 插件 | `reply` | call 的返回值（`{ seq, ok, result \| error }`） |
| 宿主 → 插件 | `invoke` | 运行插件注册的函数（`{ seq, fnId, args, stream? }`） |
| 双向 | `stream` | 流式帧（`{ seq, event: "chunk"\|"end"\|"error", data }`） |
| 宿主 → 插件 | `event` | 应用事件投递（`{ event, payload }`） |

## 桥方法（`call` 的 method 取值）

| method | args | 说明 |
| --- | --- | --- |
| `registerTool` | `[{ name, description, parameters, parallelSafe, executeId }]` | 注册 AI 工具（execute 存为 fnId） |
| `registerCommand` | `[{ id, label, runId }]` | 注册命令 |
| `registerCapability` | `[{ namespace, methodIds }]` | 定义能力（反向域名命名空间；methodIds = 方法名 → fnId） |
| `registerContribution` | `[{ point, id?, payload }]` | 通用扩展点注册（载荷内函数序列化为 `{ "$fn": fnId }`） |
| `call` | `[namespace, method, args, opts?]` | 调用能力（宿主命名空间或他插件命名空间；`opts.stream` 时改走流式） |
| `subscribe` | `[event]` | 订阅事件 |
| `emit` | `[topic, payload]` | 发布事件（命名空间 `插件id:主题` 跨插件） |
| `stateRead` / `stateWrite` | `[]` / `[data]` | 插件自持 JSON 状态（原子落盘） |
| `ready` | `[]` | 初始化完成（可选；首个任意桥调用即视为已激活） |

## 函数与 fnId

注册类方法里的函数（`execute`/`run`/能力方法/贡献载荷内的函数）不直接序列化：
代理/SDK 把它存入本地表并返回 `fN` 形式的 fnId，宿主只持 fnId。
宿主执行时发 `invoke { seq, fnId, args }`，插件跑完回 `reply { seq, ok, result|error }`。

`invoke` 的 `args` 末尾固定追加上下文参数 `ctx`：
`ctx = { aborted: false, stream? }`——`stream` 仅在 `invoke` 带 `stream: true` 时提供，
插件用它推送流帧（`ctx.stream.chunk(d)` / `ctx.stream.end(d?)` / `ctx.stream.error(msg)`）。

## 流式调用（callStream）

插件侧 `bridge.callStream(namespace, method, args, { chunk, end, error })`：
- 发 `call { method:"call", args:[ns, m, args, { stream: true }], seq }`；
- 宿主对该 seq **不回 reply**，改发 `stream` 帧：`chunk` 若干 → `end`（正常）或 `error`（失败）；
- 跨插件流式由宿主中转（提供者的流帧经 invoke seq 映射转发给调用方的 call seq）；
- `callStream` 返回取消函数：只停止本地消费，宿主中转在提供者 end/error 时自行清理。

## 错误

`reply`/`stream` 的失败统一为 `error: string`。能力路由错误示例：
`能力 <ns> 不存在` / `能力 <ns> 的提供者未运行` / `能力 <ns> 无方法 <m>`。

## 事件

宿主 → 插件 `event { event, payload }`，仅投递给 `subscribe` 过的插件。
内置事件：`vault:switch`（`{ root, id }`）、`vault:clear`。跨插件用 `emit` +
订阅 `<插件id>:<主题>`。

## 运行时接入

- **JS/TS**：浏览器 Web Worker（blob），代理源见 `services/plugins/worker.ts`；
  TS 插件入口经 esbuild-wasm 转译后走同一路径（`services/plugins/transpile.ts`）。
- **Python**：宿主内嵌 `src-tauri/plugin_runtime/python_runner.py`（写入临时目录后
  `python|python3` 执行）；插件 main 顶层直接用注入的全局 `bridge`，无需 import。
  单线程重入式 readloop：`bridge.call` 阻塞时嵌套分派消息，插件函数内再调 bridge 不死锁。
- 子进程生命周期/stdio 转发在 Rust `commands/plugin_process.rs`：`plugin_process_start`
  （spawn + 事件上报）/ `plugin_process_write`（写 stdin）/ `plugin_process_kill`（终止）。

## 能力注册表与信任模型

宿主与插件在同一注册表对等提供能力：宿主命名空间不含点（`state`/`app` 等第一方），
插件命名空间必为反向域名（含点）；`call` 由宿主按注册表路由。完全自由模型：
能力声明（清单 `declares`）仅披露与审计，无运行时拒绝——稳定性由进程/worker 隔离兜底。
