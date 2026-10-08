# 目录检索（双半示例插件）

本示例演示**插件双半**形态：重活放进宿主半进程，插件入口只做界面。

- **客户端半**（`index.ts` 的面板组件）：输入关键词、选目录、渲染结果；
- **宿主半**（内嵌脚本 `HOST_SCRIPT`，由宿主分发的 Node 运行）：递归遍历目录，逐文件匹配关键词，
  每扫 500 个文件把进度通知推回客户端半，超过 5 秒自动收尾并把结果标为截断。

## 安装

设置 → 插件 → 添加本地插件，选择本目录（`docs/plugins/examples/dir-search`），启用后在工作区
「添加面板」里打开「目录检索（双半示例）」。

## 代码导览

| 段落 | 做什么 |
| --- | --- |
| `HOST_SCRIPT` | 宿主半脚本（ESM，跑在捆绑 Node 上）：`initialize` 握手应答 + `search` 方法（遍历/匹配/进度/超时）；按行读写 stdin/stdout，每行一条 JSON-RPC |
| `ensureHostScript` | 脚本落点：内嵌源码经 `ctx.fs.writeFile` 写进插件私有目录，版本号一致即跳过——改脚本时递增 `HOST_SCRIPT_VERSION` |
| `search` | 一次检索的通道生命周期：`connect` → 订阅 `progress` → `call`（30s 兜底超时）→ `close`；插件停用时进程由宿主统一结束 |
| `Panel` | 界面：只有输入、按钮与结果渲染，没有任何检索逻辑 |

## 改造成自己的插件

- 换宿主半干的事：改 `HOST_SCRIPT` 里的 `methods`（Node 内置模块全可用），同步递增
  `HOST_SCRIPT_VERSION`；
- 换运行时载体：`connect` 的 `command` 可以是任何按行读写 stdio 的可执行程序，通道契约不变；
- 协议细节（握手、帧格式、错误码）见[插件双半与 stdio RPC](../rpc.md)，组织双半插件的通用
  说明见[插件自带原生能力](../native-capabilities.md)。
