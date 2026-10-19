# Atelyx 插件开发

Atelyx 的插件是一个**独立进程**：宿主启动它、通过标准输入输出与它说话；它把界面描述成一份声明式的数据交给宿主渲染。没有 WebView，也没有内置的 JS 运行时——插件可以用任何能读写 stdio 的语言写，本文以 Rust + 官方 SDK 为例。

本文讲**怎么做**：插件形态、15 分钟跑起来的最小实现、界面与能力面的用法、构建打包、本地调试，以及上架到内置市场。逐字段、逐方法的契约在 [API 参考](plugin-api.md)——那份文档由协议源码生成，总会与当前版本一致。

> 协议包 `atelyx-plugin-protocol` 是契约的唯一真源；三处冲突时以包内类型和文档注释为准。

## 1. 形态与边界

| 事实 | 含义 |
| --- | --- |
| 插件 = 原生可执行文件 | 宿主按 `plugin.json` 里的 `entry` 启动它，不带参数，工作目录 = 插件目录 |
| 通道 = stdin / stdout 上的 JSON-RPC 2.0（NDJSON） | stdout **只能**是协议信封；日志一律走 stderr |
| 界面 = 声明式控件树 | 插件提交 `ViewNode` 树或画布绘图指令，宿主解释渲染；插件进程里没有 UI 代码 |
| 能力面按声明开放 | `plugin.json` 的 `permissions` 未声明就调用，宿主拒绝并记审计（fail-closed） |
| 信任模型 = 本地程序 | 宿主不做沙箱：插件本来就能读写文件、起进程、联网。「权限」是透明度与审计口径，不是隔离边界 |
| 一个插件一个进程 | 宿主多窗口共享同一份进程；进程从启动到停用是一个「会话」 |

生命周期：启用即启动 → 握手（`initialize`，10 秒内未完成判失败）→ 运行 → 停用 / 崩溃 / 卸载。崩溃按 `1s → 2s → 4s → 8s …`（上限 30s）退避重启，60 秒内崩 5 次熔断，用户在设置页可手动重试。停用会结束**整棵**进程树，并收掉插件经能力面创建的一切（托管子进程、全局热键、打开的窗口、注册的 AI 工具）。

## 2. 环境准备

- Rust 1.85+（`edition = "2021"`）。其它语言也能写插件：只要实现同样的 NDJSON 信封即可，但官方只提供 Rust SDK。
- SDK 与协议目前**未发布到 crates.io**，以 git 依赖宿主仓库：

```toml
atelyx-plugin-sdk = { git = "https://github.com/Atelyx/Atelyx" }
```

同时开发宿主与插件时，用本地路径更快：

```toml
atelyx-plugin-sdk = { path = "../Atelyx/atelyx-plugin-sdk" }
```

## 3. 最小插件

### 3.1 目录

```
echo-panel/
├── Cargo.toml
├── plugin.json
└── src/main.rs
```

### 3.2 `plugin.json`

```json
{
  "id": "com.example.echo",
  "name": "Echo",
  "version": "0.1.0",
  "apiVersion": 1,
  "entry": { "windows": "echo-panel.exe", "linux": "echo-panel" },
  "permissions": ["state", "notification"]
}
```

字段含义与取值见 [API 参考 · 协议版本与清单](plugin-api.md#1-协议版本与清单)。四条硬约束不在类型里，写在这里：

- `id` 是反向域名（至少两段，每段字母 / 数字 / `-`），发布后不可变：它同时是安装目录名、数据命名空间与市场条目 id；
- `apiVersion` 必须与宿主相等（当前 `1`），不等即拒绝装载，不做兼容推断；
- `entry` 是相对插件目录的可执行路径，当前平台那一项必须存在且不得越出目录；
- `version` 是市场更新比对的版本号（与 Release tag 比对，只做升级方向）。

**宿主保留的插件目录顶层名**：`data/`（你的私有数据）、`logs/`（宿主写日志）、`previous/`（上一代内容）、`enabled`（启用标记）、`source`（本地目录安装的来源记录）。插件内容里出现这些名字会被拒绝安装。

### 3.3 `Cargo.toml`

```toml
[package]
name = "echo-panel"
version = "0.1.0"
edition = "2021"

# 插件目录若正好在别的 Cargo workspace 里，声明自己为根，避免被合并进去
[workspace]

[[bin]]
name = "echo-panel"          # 与 plugin.json 的 entry 同名
path = "src/main.rs"

[dependencies]
atelyx-plugin-sdk = { git = "https://github.com/Atelyx/Atelyx" }
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
async-trait = "0.1"

[profile.release]
strip = true
lto = true
```

### 3.4 `src/main.rs`

```rust
use atelyx_plugin_sdk::protocol::method::{
    ContributionSlot, NotificationLevel, SurfaceAttachedParams, ViewEventParams,
};
use atelyx_plugin_sdk::protocol::view::{ButtonVariant, EventKind};
use atelyx_plugin_sdk::view_builders::{button, column, text, NodeExt};
use atelyx_plugin_sdk::{run_stdio, Context, PluginHandler, PluginInfo};

struct Echo;

#[async_trait::async_trait]
impl PluginHandler for Echo {
    /// 握手完成：注册贡献点、订阅事件都放这里。
    async fn on_ready(&self, ctx: &Context) {
        if let Err(e) = ctx
            .register(ContributionSlot::Panel, "main", "Echo", None)
            .await
        {
            eprintln!("注册贡献失败：{e}");
        }
        // 先把界面树交上去：面挂载后宿主直接渲染这份存档
        let tree = column(
            "root",
            vec![
                text("hello", "你好，Atelyx"),
                button("ping", "点我", ButtonVariant::Primary)
                    .with_events(vec![EventKind::Click]),
            ],
        );
        if let Err(e) = ctx.submit_view("main", tree).await {
            eprintln!("提交界面失败：{e}");
        }
    }

    /// 面进入布局（面板被添加 / 插件重启后重发）。
    async fn on_surface_attached(&self, _ctx: &Context, params: SurfaceAttachedParams) {
        eprintln!("面已挂载：{}", params.surface_id);
    }

    /// 节点事件：只有 events 里声明过的种类才会到达。
    async fn on_view_event(&self, ctx: &Context, params: ViewEventParams) {
        if params.node_id == "ping" {
            let _ = ctx
                .notify("Echo", Some("按钮被点击"), NotificationLevel::Info)
                .await;
        }
    }
}

#[tokio::main]
async fn main() {
    let info = PluginInfo {
        id: "com.example.echo".into(),
        name: "Echo".into(),
        version: "0.1.0".into(),
    };
    // 连接结束分两种：宿主关闭 stdin（正常退出，返回 Ok）/ 协议违规或读写失败（Err，非零码退出）
    if let Err(e) = run_stdio(info, Echo).await {
        eprintln!("连接异常结束：{e}");
        std::process::exit(1);
    }
}
```

三条容易踩的规则：

1. **stdout 只允许协议**。`println!` 一行就足以让宿主判定「输出非协议内容」并停掉插件；调试输出用 `eprintln!`（进插件日志）。
2. **事件要先声明**。`ViewNode.events` 里没有 `click`，按了就什么都不会发生。
3. **`on_ready` 里别做长耗时阻塞**。握手回执由 SDK 发出，但 `on_ready` 之后的调用都在同一个连接上；需要长时间准备的工作放 `tokio::spawn`。

## 4. 通道与调用约定

- 插件 → 宿主：一律**请求**，SDK 的 `Context` 方法已经包好；默认等待上限 30 秒（`Context::call_raw`），对话框与 http 不设上限。
- 宿主 → 插件：一律**通知**（`host:` 前缀的方法），由 `PluginHandler` 的对应回调接收；回调之间没有先后保证，`on_ready` 之前也可能先到通知。
- 迟到 / 超时：调用超时只代表「本次调用放弃」，宿主侧业务可能仍在进行（`ai.chat.send` 这类有副作用的调用尤其如此），需要时用查询方法核对。
- 宿主退出：stdin 被关闭，`run_stdio` 返回 `Ok`，插件应正常退出；此时宿主已结束整棵进程树，不要试图常驻。

## 5. 界面（ViewNode）

一棵树 = 根节点 + 子节点。`id` 在同一棵树里必须唯一（宿主按 id 做控件态复用与事件回传）；容器节点才能带 `children`；节点要回发的事件写在 `events` 里。

```json
{
  "id": "root", "kind": "column", "gap": 8, "padding": 12,
  "children": [
    { "id": "title", "kind": "text", "content": "标题", "weight": "semiBold" },
    { "id": "go", "kind": "button", "label": "执行", "variant": "primary", "events": ["click"] }
  ]
}
```

### 5.1 控件

控件集：`column` / `row` / `card` / `scroll` / `grid` / `list` / `text` / `markdown` / `textInput` / `numberInput` / `dropdown` / `checkbox` / `button` / `iconButton` / `image` / `progress` / `menu` / `canvas` / `dragRegion`。每个 `kind` 的字段与语义以 [API 参考 · 界面控件树](plugin-api.md#3-界面控件树) 为准（含 `NodeBody` 的完整声明）。

几条不在类型里的约定：

- 颜色只用 `color` 引用主题 token（`text` / `textSecondary` / `textMuted` / `accent` / `danger` / `success` / `warning`），插件不写死色值，主题切换自动跟随；
- `text` 的 `size` 是绝对像素，省略时跟随界面基准字号——写死像素就不随用户缩放；
- 行内 `Fill` 子项按内容宽解析，要均分剩余宽度用行的 `fill`（`both` / `width`）；
- `image` 没有尺寸字段：按原图尺寸渲染，`fit` 在父容器给定尺寸时生效；
- `iconButton` 的图标名取自宿主内置形状表（lucide 线性图标），表外名字不渲染；
- `menu` 有 `label` 即全宽下拉钮，没 `label` 才是图标触发。

### 5.2 事件与取值

节点在 `events` 里声明了哪几种，宿主才回发哪几种（`click` / `change` / `submit`，`focus` / `blur` 为保留种类、当前无控件产出）。事件值按控件定型：

- 文本类 → 字符串；`submit` 只在单行输入回车时触发；
- 数字输入 → 数值（不可解析为数值不发事件；清空 → `null`，与 0 区分）；
- 勾选 → 布尔；下拉 → 选中项**下标**；菜单条目 → 条目的 `action` 字符串。

三者的完整声明见 [API 参考 · 视图事件与指针](plugin-api.md#4-视图事件与指针)。

### 5.3 输入回填规则（插件只需遵守一条）

宿主为每个输入节点自持草稿：用户输入立刻显示，并在节点声明了 `change` 时回发新值。插件把**刚收到的值原样写回**新树不会打断输入；插件写入**另一个值**时接管显示。多行输入额外按「既不同于本机当前文本、也不同于已上报值」判断，避免重建编辑器时丢光标。

### 5.4 重发与 diff

界面按整棵树重发，宿主按键 diff（纵向容器按节点 id 复用控件态）。所以：结构尽量稳定、节点 id 稳定且唯一；不要把值编码进 id。

### 5.5 面外层内边距

面板、表格视图与设置页这三个面，宿主会在根树外层加 12/10 的内边距；整页接管与插件窗口不加。需要贴边的内容放进接管面或窗口面，面板里不要自带同样的外边距（会叠加）。

## 6. 自绘画布

画布是树里的叶子控件（`kind: "canvas"`），负责占位与指针事件；内容单独提交：

```rust
ctx.submit_canvas("main", "board", commands).await?;   // 整发整换，空清单 = 清空
```

指令集是封闭枚举：线 / 矩形（含圆角）/ 路径（折线·多边形）/ 圆 / 文字 / 图片贴片，`fill` 与 `stroke` 至少给一个。字段与校验规则见 [API 参考 · 自绘画布](plugin-api.md#5-自绘画布)（`DrawCommand`）。

坐标一律画布本地逻辑像素、左上原点，宿主不做坐标变换（平移缩放由插件算好坐标）。颜色双轨：`{ "type": "token", "name": "accent" }` 跟主题，`{ "type": "rgba", "rgba": [r,g,b,a] }` 用于内容谱系色（连线分类、播放头等）。

指针事件在节点的 `mouseEvents` 里声明要收的子集：`pointerDown` / `pointerMove` / `pointerUp` / `pointerLeave` / `wheel` / `doubleClick`。`wheel` 增量是行数（正 = 向下 / 向右）；双击由宿主判定（同键二次按下、500ms 内、位移 < 4 像素）。命中测试由插件按指令几何自算——宿主只负责把事件送到画布。

两条限制：按住拖出画布不会丢事件（绝对坐标换算，抬起为止）；画布文字永远绘制在所有图形之上，需要被图形遮盖的文字请改用树里的 `text` 控件分层。

## 7. 能力面

在 `permissions` 里声明后，SDK 的 `Context` 对应方法才可用；未声明调用会返回可读错误（`能力未声明`）并计入设置页的「被拒调用」审计。

每个权限名对应的 SDK 方法与协议方法名、params / result 形态，以 [API 参考 · 能力面方法名](plugin-api.md#7-能力面方法名) 为准。这里只记各自的边界与坑：

| 权限 | 边界与约束 |
| --- | --- |
| `state` | 按插件隔离的自持 JSON，落 `<插件目录>/data/state.json`；损坏时宿主先备份原文再降级 |
| `fs` | `private_dir()` 给私有数据目录；其余接受**任意绝对路径**，父目录自动创建 |
| `vault` | 路径相对当前仓库根，不能越出仓库、不开放隐藏段；本地仓库与协作空间同构；**正被编辑器打开的文件拒写** |
| `table` | 读写当前打开的表格。真源在宿主，写入与用户编辑同一出口（合并落盘 + 协作补丁）；**禁止**用 `fs` 旁路读写 `.atb` |
| `notification` | 应用内通知条 |
| `shortcuts` | 系统级全局快捷键（如 `Shift+Alt+E`）；被占用时注册返回可读错误，停用自动注销 |
| `clipboard` | 纯文本，由宿主执行 |
| `dialog` | 系统对话框，等用户交互不设上限；取消返回 `None` |
| `http` | 便利层：目标地址、方法、大小、超时全由插件自负，宿主不设限。响应体以 base64 返回；大文件请插件进程自己直连 |
| `process` | 托管子进程：输出与退出经 `host:process.event` 转发；停用 / 崩溃 / 卸载时连同子孙进程一起结束 |
| `window` | 见 §8.2 |
| `ai` | 见 §8.4 |
| `tray` / `collab` | 预留，当前没有对应方法 |

`vault` 的读写**必须**经能力面：直接改盘会绕过写盘队列与协作通道，属于内容丢失级缺陷。

## 8. 进阶

### 8.1 接管窗口

```rust
ctx.takeover(TakeoverScope::Page, "main").await?;   // 内容区归插件，宿主标题栏保留
ctx.takeover(TakeoverScope::Full, "main").await?;   // 整窗归插件（含标题栏区域）
```

`surfaceId` 必须是已注册贡献的 id（与该面共享同一棵树）。接管按窗口生效：布局真源不动，退出即恢复原视图。全接管时插件自绘标题栏——拖动用 `dragRegion` 热区（宿主代执行），缩放沿用窗口边缘，关闭走宿主关窗链；退出入口由宿主常驻（右上角按钮 + Esc）。

### 8.2 插件窗口

```rust
let params = WindowOpenParams {
    id: "palette".into(),      // 插件内唯一、不含 ':'，同时是窗口面的 surfaceId
    title: "命令面板".into(),
    width: Some(560.0), height: Some(360.0),
    always_on_top: Some(true), skip_taskbar: Some(true), hide_on_blur: None,
};
ctx.window_open(params).await?;   // 之后用 submit_view("palette", tree) 提交内容
ctx.window_close("palette").await?;
```

窗口属性只在开窗时给定，没有运行时更新；重复 id 会被拒。用户关窗时宿主撤掉该面并回 `host:surface.detached`（`kind: "window"`）。`hideOnBlur` 是「隐藏窗口」而非关闭，宿主没有提供再次显示的方法——需要反复唤起的场景请用 `window_close` + `window_open`。

### 8.3 领域文档类型

在 `plugin.json` 里注册扩展名，宿主负责文件树图标、新建菜单与打开路由：

```json
"docTypeRegistrations": [
  { "extension": "board", "label": "白板", "icon": "layout-dashboard", "surface": "doc" }
]
```

`surface` 必须同时用 `view.register` 注册为 `panel` 贡献。用户打开这类文件时宿主发 `host:doc.open { surfaceId, path }`，插件按 `surfaceId` 提交文档界面树；关闭或换文件时先发 `host:doc.close`。同一时间至多一个打开的插件文档（切换文件即换实例）；文件被删除或切仓库也会关闭。同一扩展名被多个插件注册时按插件 id 取第一个并提示。

### 8.4 AI 总线

```rust
// 发起对话（会话 = 仓库里的普通会话，对话面板可见）；事件经 on_chat_event 流达
let sent = ctx.chat_send("帮我总结这篇笔记", None).await?;

// 注册一个 Agent 可调用的工具
ctx.register_tool("echo.search", "在笔记里搜索", json!({
    "type": "object",
    "properties": { "query": { "type": "string" } },
    "required": ["query"]
})).await?;
```

```rust
async fn on_tool_call(&self, ctx: &Context, params: ToolCallParams) {
    let result = format!("收到查询：{}", params.arguments["query"]);
    let _ = ctx.tool_result(&params.call_id, &result, false).await;
}
```

- 工具名全局唯一（不得与内置工具或其他插件重名），在 Agent 的工具勾选里以 `plugin:<插件 id>:<工具名>` 出现；用户没勾就不派发。
- 工具调用等待上限 120 秒，超时 / 插件未运行都以错误文本回给模型。
- 对话事件：`message`（流式快照，整条消息替换）、`finished`（正常收敛或用户中断）、`failed`、`titled`（话题自动命名结果）。
- 停用 / 崩溃 / 卸载自动摘除工具登记，插件不需要自己收尾。

### 8.5 事件订阅

```rust
ctx.subscribe("vault:switch").await?;    // 容器切换（本地仓库 / 协作空间 / 换仓库）
ctx.subscribe("table:changed").await?;   // 当前打开表格变更
```

- `vault:switch` 只在声明了 `keepMountedOnVaultSwitch` 时才有意义：未声明的插件在收到它之前就被宿主重载了。payload：`{ kind: "local", root }` 或 `{ kind: "space", spaceId }`。
- `table:changed` payload 是当前表格的仓库相对路径；订阅时会先补发一次当前状态（迟到订阅不丢当前表格）。

## 9. 构建与打包

```bash
cargo build --release
mkdir -p dist
cp target/release/echo-panel.exe dist/     # Linux: target/release/echo-panel
cp plugin.json dist/
```

`dist/` 里**只放** `plugin.json` 与平台入口（以及确实需要的随包文件）——宿主会整目录复制安装，别把 `target/`、`.git/`、源码打进去。

发布用的 zip 必须按市场约定命名：

```
<id>-<version>-windows-x64.zip      # 例 com.example.echo-0.1.0-windows-x64.zip
<id>-<version>-linux-x64.zip
```

体积与格式上的硬约束（安装时校验，违反即拒收）：

- 单条目压缩比 > 100:1 且原始体积 ≥ 64 KiB → 判为解压炸弹（常见诱因是打包了很大的纯文本 / 全零数据）；
- 解压总量 ≤ 4 GiB；
- 不允许符号链接，不允许绝对路径或 `..`；
- 插件内容里不能出现宿主保留名（`data` / `logs` / `previous` / `enabled` / `source`）；
- 安装包里的 `plugin.json` id 必须与市场条目 id 一致（防止一份包覆盖另一个插件）。

Linux 上宿主会为入口补可执行位（zip 不保留权限位也不影响），但建议打包时仍保留 `755`。

## 10. 本地调试

1. **装**：设置 → 插件 → 「从本地目录」选 `dist/`（不是仓库根：目录安装会整树复制，选根会把 `target/` 一起装进去）。zip 包走「从 zip 安装」。
2. **启**：安装只落位、不自动启用，在「已安装」列表里打开开关。
3. **看日志**：详情 →「日志」。这里能看到插件的 stderr、协议违规行、握手 / 崩溃 / 熔断原因；文件在 `<应用数据目录>/plugins/<id>/logs/runtime.log`（超过 2 MiB 轮转一份 `.1`）。
4. **改码后重装**：同一次本地目录安装会记下来源，构建完点该插件的「从来源更新」——重装内容并按启用态重启，不用重新选目录。
5. **数据**：插件私有目录 `<应用数据目录>/plugins/<id>/data/`；更新保留、卸载删除。

常见失败对照：

| 现象 | 原因 |
| --- | --- |
| 列表里「不可装载：协议版本不匹配」 | `apiVersion` 与宿主不一致（当前 `1`） |
| 「不可装载：入口文件不存在」 | `entry` 写的文件名 / 路径与包内不一致，或漏了当前平台 |
| 启用后立刻「启动失败：插件未在 10 秒内完成握手」 | 没有走 `run_stdio`、进程启动即退出、或 `main` 前就阻塞了 |
| 「输出非协议内容」并停机 | stdout 上出现了非协议的行（`println!`、第三方库日志） |
| 安装了但面板菜单里没有条目 | 没有 `view.register`，或注册失败（看日志） |
| 按钮点击没反应 | 节点 `events` 没声明 `click`，或 `on_view_event` 没处理该节点 id |
| 输入框打字被打断 / 光标乱跳 | 插件在 `change` 回填时写入了与收到值不同的值（宿主据此判定接管） |
| 「能力未声明」 | `plugin.json` 的 `permissions` 少了对应项 |
| 安装被拒「含宿主保留路径」 | 包里有 `data/`、`logs/` 等宿主保留名 |
| 安装被拒「压缩比过高」 | 打包了大体积高压缩比的文本 / 数据文件 |

## 11. 上架到内置市场

插件 = 一个独立 git 仓库（一插件一仓库）。上架零申请：

1. 仓库根放 `plugin.json`（与发布包一致），源码照常提交；
2. 用标签发布 Release：tag 形如 `v0.1.0`，资产名严格按 §9 的约定挂平台 zip（没有对应平台的资产，该平台市场里点了也只会提示「未挂本平台安装包」）；
3. 给仓库打 topic `atelyx-plugin`。

官方索引每 6 小时扫描一次带该 topic 的仓库：读默认分支的 `plugin.json`、校验 id / 版本 / 协议版本 / 平台入口，并核对最新 Release 是否挂了约定命名的平台包，通过即收录。收录后所有用户能在「设置 → 插件 → 市场」里搜到。

想在市场里带类型筛选，可在 `plugin.json` 额外写一个索引用的 `type` 字段（如 `"type": "panel"` / `"type": "tableview"`）——宿主运行时不读它，只用于市场分类展示。

条目徽标按**仓库归属**判定（不按自报 id）：仓库 owner 在官方名单里的显示「官方」，其余条目不显示徽标。

安装非官方条目会先弹确认（提示插件可读写文件、运行程序、联网）；官方条目直接安装。更新按 Release tag 与已装 `version` 比较，只做升级方向。

## 12. 兼容性

- `apiVersion` 是硬闸门：宿主只接受完全相等的版本，版本不符的插件响亮拒绝，不做兼容推断。协议破坏性变更一定伴随 `apiVersion` 递增。
- 控件集、事件、贡献槽位、画布指令都是封闭枚举，扩容随协议升版；未知取值会被拒绝而不是被忽略。
- 插件应把「宿主关闭 stdin」当作正常退出信号，不要依赖宿主发来的额外终止通知。
