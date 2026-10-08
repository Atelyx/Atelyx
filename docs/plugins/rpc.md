# 插件双半与 stdio RPC（`ctx.rpc`）

插件可以拆成**两半**跑在两个地方：

- **客户端半**：插件的常规入口（WebView 内），能力 = UI 槽位 / 浮层 / 面板与 `ctx` 服务面；
- **宿主半**：一个由客户端半启动的进程（典型 = 捆绑 Node 跑插件自带的脚本），能力 =
  宿主半运行时的一切（Node 内置 `fs` / `crypto` / `child_process` 等，即 OS）。

两半之间**只有一条通道**：stdio 上的 JSON-RPC 2.0（NDJSON 帧）。宿主维护的只是这条通道本身
（帧化、握手、请求结算与进程记账），方法语义完全由插件自定义——逻辑放哪一半由插件自己决定。

## 客户端半用法

```ts
// 宿主半脚本：以捆绑 Node 跑插件写在私有目录的 host.mjs 为例
const rt = await ctx.process.bundledRuntime();
if (!rt) {
  // 未分发运行时的平台（安卓等）：按插件自己的口径降级
  return;
}
const channel = await ctx.rpc.connect({
  command: rt.path,
  args: [hostScriptPath],
});
if (channel.remoteInfo) console.log("宿主半版本", channel.remoteInfo.version);

// 请求 / 响应
const result = await channel.call<MyResult>("scan", { dir }, { timeoutMs: 30_000 });

// 宿主半主动通知（长任务进度等）
channel.on("progress", (p) => updateUi(p));

// 宿主半反向请求（宿主半要客户端半做事时）
channel.onRequest("pickFile", async (params) => await ctx.dialog.open(params));

// 收尾：结束进程并关闭通道（幂等）；插件停用 / 卸载时进程也会随插件记账被结束
await channel.close();
```

- `connect(opts)`：启动宿主半进程（程序来源口径与 `ctx.process` 相同）→ 发 `initialize` 握手
  （校验协议版本，超时缺省 30 秒，`initializeTimeoutMs` 可调）→ 返回就绪通道。握手失败或进程
  先行退出即 reject，进程不残留。`opts` 不接受 `input`（stdin 由协议占用）。
- `call(method, params, opts?)`：等响应；远端错误抛 `RpcRemoteError`（`code` / `message` / `data`）；
  `timeoutMs` 缺省不设超时（通道关闭仍会结算）。
- `notify` / `on` / `onRequest`：单向通知与宿主半反向请求；`on` / `onRequest` 返回退订 / 注销函数。
- `done`：通道关闭时 resolve（带进程退出码与原因）；其后一切收发被拒。
- 进程归属与 `ctx.process.spawn` 完全同源：按调用方插件记账，插件停用 / 卸载 / 更新 / 回退 /
  重载与应用退出（含 Windows 强杀）时一律结束。审计按 `connect` 披露程序名与参数个数，
  管理页插件详情「能力面」可见（敏感面）。

## 线上契约（v1）

**帧**：每行一个 JSON 值（紧凑输出，字符串内的换行必须转义——`JSON.stringify` 保证），`\n` 结尾。
空行与解析失败的行**跳过不报错**——诊断信息写 stderr，stderr 不参与协议。行缓冲由对端负责
（`ctx.rpc` 客户端侧已按行收帧；宿主半用任意按行读 stdin 的方式即可）。

**消息**（JSON-RPC 2.0 形状）：

| 方向 | 形状 | 说明 |
| --- | --- | --- |
| 请求 | `{ "jsonrpc": "2.0", "id", "method", "params"? }` | `id` 数字或字符串；双向可用 |
| 响应 | `{ "jsonrpc": "2.0", "id", "result" }` 或 `{ ..., "error": { "code", "message", "data"? } }` | `id` 与请求对应 |
| 通知 | `{ "jsonrpc": "2.0", "method", "params"? }` | 无 `id`，不等响应；双向可用 |

**握手**：连接建立后客户端半发第一条请求 `initialize`：

- 请求 `params`：`{ "protocolVersion": 1, "client": { "name"?, "version"? } }`；
- 宿主半必须回 `result`：`{ "protocolVersion": 1, "serverInfo"?: { "name"?, "version"? } }`；
- 版本不一致（客户端按「同版本即支持」判定）或超时即连接失败。宿主半在收到 `initialize`
  之前**不应主动发请求**；通知可发但客户端半此时可能还没有订阅者。

**错误码**：沿用 JSON-RPC 保留码——未注册方法 `-32601`；处理抛错回 `-32603`（message 取异常文本）。
宿主半自定义业务错误用任意码（推荐应用层区间 `-32000` 起自定）。

**生命周期**：进程退出（或通道收发失败）= 通道关闭，未决 `call` 统一 reject（带原因），
`done` 带退出码。重连 = 重新 `connect`（进程生灭由插件自理）。

## 宿主半参考模板

宿主半 = 任何「按行读 stdin、按行写 stdout」的程序。捆绑 Node 上跑的脚本模板：

```js
// host.mjs —— 双半宿主半模板：方法表 + 行循环
import { createInterface } from "node:readline";
import { stdin, stdout } from "node:process";

const PROTOCOL_VERSION = 1;
const methods = {
  ping: () => "pong",
  // 在此注册宿主半方法：Node 内置模块（fs / crypto / child_process…）全部可用
};

function send(message) {
  stdout.write(JSON.stringify(message) + "\n");
}

const pending = new Map(); // 宿主半主动调客户端半时的未决请求

function callClient(method, params) {
  const id = `h${Date.now()}${Math.random().toString(36).slice(2)}`;
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject });
    send({ jsonrpc: "2.0", id, method, params });
  });
}

createInterface({ input: stdin }).on("line", (line) => {
  if (!line.trim()) return;
  let message;
  try {
    message = JSON.parse(line);
  } catch {
    return; // 坏行跳过；诊断写 stderr（console.error 即可）
  }
  if (message.method === "initialize") {
    send({
      jsonrpc: "2.0",
      id: message.id,
      result: { protocolVersion: PROTOCOL_VERSION, serverInfo: { name: "my-host", version: "0.1.0" } },
    });
    return;
  }
  if (message.id !== undefined && message.method !== undefined) {
    // 客户端半请求
    Promise.resolve()
      .then(() => methods[message.method]?.(message.params))
      .then((result) => send({ jsonrpc: "2.0", id: message.id, result: result ?? null }))
      .catch((error) =>
        send({
          jsonrpc: "2.0",
          id: message.id,
          error: { code: -32603, message: String(error?.message ?? error) },
        }),
      );
    return;
  }
  if (message.id !== undefined) {
    // 客户端半对宿主半反向请求的响应
    const waiter = pending.get(String(message.id));
    pending.delete(String(message.id));
    if (!waiter) return;
    if (message.error) waiter.reject(new Error(message.error.message));
    else waiter.resolve(message.result);
    return;
  }
  // 客户端半通知：按 message.method 自行分发
});
```

## 宿主半脚本的定位

插件的 ctx 面**不暴露插件自身安装目录**，宿主半脚本用哪种落点取决于分发方式：

- **本地目录来源**（开发态）：直接引用源目录里的脚本绝对路径；
- **分发场景**：插件把宿主半脚本（整段源码内嵌在客户端半代码里）经 `ctx.fs.writeFile` 写进
  私有目录（`ctx.fs.privateDir()`，跨更新保留、随卸载清除），以「版本号一致即跳过写入」的
  戳控制重复，再以私有目录里的绝对路径启动。

运行时载体：桌面用 `ctx.process.bundledRuntime()` 取捆绑 Node（未分发返回 `null` 即降级）；
宿主半也可以是任何独立可执行程序——通道契约与载体无关。

## 平台与降级

- 桌面（windows-x64 / linux-x64）：`ctx.rpc` 全功能可用；
- 安卓：进程执行能力缺失，`connect` 以可读错误拒绝——插件应在连接前按平台能力探测并降级
  （与 `ctx.process` 同一口径：未就绪即降级 + 提示文案随能力切换），宿主不做隐式兜底。

## 明确不做

- 通道不做鉴权与加密：stdio 双方同机，**进程所有权即信任边界**（与 `ctx.process` 一致）；
- 宿主不托管宿主半进程的生命周期（无自动重启、无常驻运行时、无按清单声明启动）：进程生灭归插件，
  重连 = 重新 `connect`。
