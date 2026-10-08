# 插件自带原生能力（宿主半）

插件的逻辑不必都挤在 WebView 里：**重活放进一个独立进程（宿主半），界面留在插件入口（客户端半）**，
两半之间走 `ctx.rpc` 通道。典型适合放宿主半的活：

- **批量文件 I/O**：递归遍历、全文检索、批量读改——逐文件走 `ctx.fs` 会把等待串行摊在 UI 侧；
- **计算密集**：校验和、解析、压缩——不卡界面渲染；
- **Node 生态能力**：`crypto` / `zlib` / `child_process` 等内置模块与 npm 包。

判断口径：**这件事需要 OS 级能力或重计算，且 `ctx` 服务面没有现成的领域方法**——那就是宿主半的活。
通道契约（线格式、握手、消息形状）见 [插件双半与 stdio RPC](rpc.md)，本文讲怎么组织一个双半插件。

## 运行时载体与降级

宿主半 = 任何「按行读写 stdin/stdout」的程序。最常用的载体是宿主随应用分发的 Node：

```ts
const rt = await ctx.process.bundledRuntime();
if (!rt) {
  // 未分发运行时的平台（安卓等）：按插件自己的口径降级
  //（隐藏功能入口 / 提示文案切换 / 回退纯客户端半实现）
  return;
}
const channel = await ctx.rpc.connect({ command: rt.path, args: [scriptPath] });
```

宿主半也可以是插件自带的独立可执行程序（侧车二进制随插件仓库分发，绝对路径启动）——通道契约与
载体无关。安卓上 `bundledRuntime()` 返回 `null` 且进程执行不可用：插件应在连接前探测并降级，
宿主不做隐式兜底。

## 宿主半脚本的落点

插件的 ctx 面**不暴露插件自身安装目录**，宿主半脚本用哪种落点：

- **开发态（本地目录来源）**：直接引用源目录里的脚本绝对路径，改脚本即时生效；
- **分发场景**：把脚本整段内嵌在客户端半代码里，启动前经 `ctx.fs.writeFile` 写进私有目录
  （`ctx.fs.privateDir()`——跨更新保留、随卸载清除），以「版本号一致即跳过写入」的戳控制重复：

```ts
const HOST_SCRIPT_VERSION = 3; // 改脚本时递增，旧文件被新内容覆盖
async function ensureHostScript(): Promise<string> {
  const dir = (await ctx.fs.privateDir()).replace(/[\\/]+$/, "");
  const path = `${dir}/host-v${HOST_SCRIPT_VERSION}.mjs`;
  try {
    if ((await ctx.fs.readFile(path)) === HOST_SCRIPT) return path;
  } catch {
    // 首次或被清理：照常写入
  }
  await ctx.fs.writeFile(path, HOST_SCRIPT);
  return path;
}
```

宿主半需要 npm 依赖时，随插件仓库把 `node_modules` 一起带上（本地目录来源直接可用；脚本放在
插件数据目录之外的依赖引用要自己在启动时解析）。通道里的方法语义完全自定义——宿主半只要求实现
`initialize` 握手应答（模板见 [rpc.md](rpc.md)）。

## 生命周期与停止

- 宿主半进程按调用方插件记账：**插件停用 / 卸载 / 更新 / 回退 / 重载与应用退出时宿主统一结束**——
  长驻服务不活过插件本身；
- 通道用完即 `close()`（结束进程树）；长驻服务型宿主半保留通道，并在插件 UI 给用户一个停止入口；
- 需要活过切仓库时在清单声明 `keepMountedOnVaultSwitch`（见[清单](manifest.md)）；
- 重连 = 重新 `connect`，上一轮句柄不再有效。

完整可运行的例子（含进度通知、超时收尾、版本戳脚本落点）见
[examples/dir-search](examples/dir-search/README.md)。
