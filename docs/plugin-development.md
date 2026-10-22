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

生命周期：启用即启动 → 握手（`initialize`，10 秒内未完成判失败）→ 运行 → 停用 / 崩溃 / 卸载。崩溃按 `1s → 2s → 4s → 8s …`（上限 30s）退避重启，60 秒内崩 5 次熔断，用户在设置页可手动重试。停用会结束**整棵**进程树，并收掉插件经能力面创建的一切（托管子进程、全局热键、托盘条目、打开的窗口、注册的 AI 工具）。

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
use std::sync::{Arc, Mutex};

use atelyx_plugin_sdk::protocol::method::{
    ContributionSlot, NotificationLevel, SurfaceAttachedParams, ViewEventParams,
};
use atelyx_plugin_sdk::protocol::view::{ButtonVariant, EventKind};
use atelyx_plugin_sdk::view_builders::{button, column, text, NodeExt, NodeId};
use atelyx_plugin_sdk::{run_stdio, Context, PluginHandler, PluginInfo};

/// 节点 id 与事件匹配共用同一份定义，改名只改常量一处。
const PANEL: NodeId<'static> = NodeId::new("main");
const PING: NodeId<'static> = NodeId::new("ping");

struct Echo {
    clicks: Arc<Mutex<u32>>,
}

#[async_trait::async_trait]
impl PluginHandler for Echo {
    /// 握手完成：注册贡献点、订阅事件、登记界面构建器都放这里。
    async fn on_ready(&self, ctx: &Context) {
        if let Err(e) = ctx
            .register(ContributionSlot::Panel, PANEL.as_str(), "Echo", None, None)
            .await
        {
            eprintln!("注册贡献失败：{e}");
        }
        let clicks = Arc::clone(&self.clicks);
        ctx.ui()
            .mount(PANEL.as_str(), move || {
                let clicks = *clicks.lock().unwrap();
                column(
                    "root",
                    vec![
                        text("hello", "你好，Atelyx"),
                        text("clicks", format!("点击次数：{clicks}")),
                        button(PING.as_str(), "点我", ButtonVariant::Primary)
                            .with_events(vec![EventKind::Click]),
                    ],
                )
            })
            .await;
    }

    /// 面进入布局（面板被添加 / 插件重启后重发）。SDK 已重发界面树，这里只补树以外的内容。
    async fn on_surface_attached(&self, _ctx: &Context, params: SurfaceAttachedParams) {
        eprintln!("面已挂载：{}", params.surface_id);
    }

    /// 节点事件：只有 events 里声明过的种类才会到达。
    async fn on_view_event(&self, ctx: &Context, params: ViewEventParams) {
        if params.node_id == PING {
            *self.clicks.lock().unwrap() += 1;
            if let Err(e) = ctx.ui().notify(PANEL.as_str()).await {
                eprintln!("提交界面失败：{e}");
            }
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
    let plugin = Echo { clicks: Arc::new(Mutex::new(0)) };
    // 连接结束分两种：宿主关闭 stdin（正常退出，返回 Ok）/ 协议违规或读写失败（Err，非零码退出）
    if let Err(e) = run_stdio(info, plugin).await {
        eprintln!("连接异常结束：{e}");
        std::process::exit(1);
    }
}
```

界面提交由 SDK 托管：

- `ctx.ui().mount(surfaceId, build)` 登记这一面的构建器——`build` 只读当前状态并返回整棵树；重复挂载同一面即替换构建器。
- `ctx.ui().notify(surfaceId)` 触发一次「构建 → 与上次成功提交的树比较 → 未变跳过」，返回 `Ok(false)` 表示没有提交。事件处理里改完状态调它即可，不需要自己保存上次提交的树。
- 面重新挂载时 SDK 先强制重发一次树再回调 `on_surface_attached`；该回调只补树以外的内容（如画布指令清单），不要在这里重复整树。
- 构建器在 SDK 内同步执行，读状态用短锁、不要跨 `await` 持锁。

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

一棵树 = 根节点 + 子节点。`id` 在同一棵树里必须唯一（宿主按 id 做控件态复用与事件回传）；容器节点才能带 `children`；节点要回发的事件写在 `events` 里。Rust SDK 里用 `NodeId` 常量定义 id，构建与事件匹配共用同一份定义。

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

控件集：`column` / `row` / `card` / `stack` / `overlay` / `scroll` / `grid` / `list` / `text` / `markdown` / `textInput` / `numberInput` / `dropdown` / `checkbox` / `button` / `iconButton` / `icon` / `image` / `progress` / `menu` / `divider` / `canvas` / `dragRegion`。每个 `kind` 的字段与语义以 [API 参考 · 界面控件树](plugin-api.md#3-界面控件树) 为准（含 `NodeBody` 的完整声明）。

控件之上还有四个**节点级能力**，写在节点上（与 `kind` 并列）：

- `size`（`column` / `row` / `card`）：自身尺寸 `{ width, height, minWidth, maxWidth, minHeight, maxHeight }`。`width` / `height` 取 `"fill"` / `"shrink"` / 数字像素；缺省列与卡片占满宽、行按内容收缩（行内有 `grow` 子项或声明了 `justify` 时按占满宽布局）。**上下限只对固定或收缩尺寸的子项生效**：流体子项（`"fill"` 或按 `grow` 分配）的空间由父容器分配，此时声明上下限不参与布局。
- `grow`：在父级行 / 列 / 网格里按权重分剩余空间，`grow: 35` 与 `grow: 65` 就是 35% 与 65%。带 `grow` 子项的行会自动占满宽度、列会自动占满高度（否则没有剩余空间可分配）；交叉轴尺寸仍由子项自己的 `size` 决定。
- `interaction`：交互外观与状态，`{ state, normal, hover, active, disabled }`，每一档给 `{ background, border, radius }`。取档顺序 **禁用 > 激活 > 悬停 > 常态**，某档没声明就继续往后找、最终取常态；`state: "disabled"` 下不接受点击与悬停。列表选中项写 `state: "active"`，配 `active` 底色与描边即可——选中项在指针下仍保留选中外观，不会被悬停态盖掉。按钮与图标钮自带底色与按下态，`interaction` 在它们身上只决定可用性（`state: "disabled"` 即不可点）。
- `events` 里的 `click` 对任何节点生效（容器也行），可点击的节点鼠标呈指针；点击落在子控件上时由子控件优先处理。按钮与图标钮的按下由控件自身处理，不必再包一层。

取色（`color`、`interaction` 各档的 `background` / `border`、`icon.color`、`progress.tone`）统一两种写法：

- 主题 token：`text` / `textSecondary` / `textMuted` / `accent` / `accentSoft` / `success` / `successSoft` / `warning` / `warningSoft` / `danger` / `dangerSoft` / `surface` / `surfaceMuted` / `surfaceSunken` / `hover` / `border` / `borderStrong` / `transparent`；
- 自定义色：`#rgb` / `#rgba` / `#rrggbb` / `#rrggbbaa`。

token 随主题与界面字号设置联动，自定义色由插件自负对比度（写错色值或拼错 token 会被拒收，不会静默回退成默认色）。

排版（`text` / `markdown`）：`family` 取 `"ui"`（跟随界面字体，缺省）或 `"mono"`（系统等宽族，数值、路径、日志按位列对齐）；`weight` 取 `normal` / `medium` / `semiBold`，系统字体族缺该字重时由字体库匹配最近一档；`text.wrap: false` 单行不换行（长标签不把行撑高；不给 `maxLines` 时超长文本会溢出节点框）；`text.maxLines` 限制渲染行数（至少 1），超出时宿主按实际排版在最后一行补省略号，长文件名 / 路径 / 单行标签用它收口，不要自己按字符数截断。

其余约定：

- `text` 的 `size` 是**字号倍率**（相对界面基准正文，缺省 1.0），写 `1.5` 即放大一半并跟随用户字号缩放；`align` 给水平对齐（`end` 右对齐气泡、`center` 居中标题），**声明了 `align` 才占满父容器宽度**，不给就按内容宽度收缩；
- 列的宽度收缩配 `size.maxWidth` 与 `align`，就能做居中的窄阅读列表或内容自适应的右对齐气泡；工具行的左右分布用 `justify: "spaceBetween"`；
- `grid` 两种列法：`columns` 固定列数，`minColumnWidth` 按可用宽度自动换列（面板变窄时不会把各列一起挤扁），同时给定时后者只作列数上限；都不给按单列。`rowHeight` 取数字像素或 `"content"`（按内容高度）——缺省按列宽 1:1 定行高，「标签 + 单行控件」这类短内容会被拉成方格；
- `card` 的底色档 `tone`（`default` / `muted` / `accentSoft` / `transparent`）、圆角 `radius`、边框开关 `bordered` 可调。**当前状态命中某个交互外观档时，底色与边框由该档提供**，卡片自身的 `tone` / `accent` / `bordered` / `radius` 让位（避免两层底色互相盖住）；当前状态没有对应档位时仍按卡片自身的档位绘制；分组内轻分割用 `divider`；`stack` 把子节点叠起来（后提交的在上，可做「画布当底 + 控件叠其上」）；
- `image` 的 `width` / `height`：数字 = 固定像素，`"fill"` = 占满父容器该轴，`"shrink"` / 不给按原图尺寸；`fit` 决定尺寸内的适配方式（缺省 `contain`——不裁切不拉伸、留空边；`cover` 铺满并裁掉溢出；`fill` 拉伸），要看原图长宽用 `ctx.vault_image_info(path)` 取；声明 `click` 事件即成为可点击图片（大图预览入口）；
- `iconButton` 的图标名默认取自宿主内置形状表（lucide 线性图标）；先用 `ctx.define_icon(name, path)` 登记自备的 24x24 描边路径，即可用自己的图形（`size` 档 `sm`/`md`/`lg`，`variant` 复用按钮档位，`hoverOnly` 悬停才显示）；同一批自备名也可用作贡献注册的 `icon`（面板标签头 / 设置 tab / 文档类型图标）；
- `icon` 是纯装饰图标（空态、状态行、标题前的标记），不接受点击；需要可点的图标用 `iconButton`。`button` 可给 `icon` 做「图标 + 文字」的主按钮；
- `progress` 的 `girth` 是轨道粗细（px，缺省 30 的粗条；细进度条写 `3` / `4`），`tone` 是完成段颜色（缺省主题强调色）；
- `markdown` 的 `content` 用笔记同源解析器渲染，`size` 是整体字号倍率，`weight` / `color` 覆盖基础字重与文字色（标题与加粗片段恒为加粗，链接与代码块语义色不受影响）；正文里的链接可点击，宿主按形态默认打开（外链 / wiki / 仓库路径），节点声明 `linkClick` 时同时回发链接点击事件；
- `menu` 的 `label` 与 `icon` 可并存（并存为紧凑的「图标 + 文字」触发钮），只给 `label` 才是全宽下拉钮；条目可标 `current`（宿主在条目前画勾）；
- `overlay` 是面内浮层：唯一子节点即浮层内容，宿主把它铺满整个插件面——一层遮罩 + 居中内容。**节点在树里就是打开**，从树里移除即关闭；挂在哪个容器下效果都一样（不参与父容器排版），多个浮层按树序叠放、后出现的盖在上面。遮罩拦下所有指针事件（下层既不响应点击也不滚动）；声明 `click` 时点击遮罩（内容之外）回发点击事件，用来收起浮层。`scrim` 换底色（缺省主题遮罩，`"transparent"` 只拦截不画底）。浮层不改写键盘与焦点，也不能作根节点；弹层类界面（大图预览 / 确认框 / 表单）用它，不要用 `stack` 模拟；
- 可滚动容器（`list` / `scroll`）用 `ctx.scroll_to(surface, node, ScrollEdge::End)` 滚到主轴一端，无需插件自己维护滚动状态；`scroll` 的 `direction` 决定子项排布与滚动主轴（`horizontal` = 子项排成行并横向滚动），`height` 给定即收住自身高度（把横向图条嵌进纵向内容流），缺省占满父容器剩余高度；
- 输入控件（`textInput` / `numberInput` / `dropdown` / `checkbox`）给 `readOnly: true` 即保留控件外观展示只读值，不接受编辑、不产生事件；
- 滚动位置：给 `list` / `scroll` 节点声明 `scroll` 事件，宿主就回发 `{ offset, maxOffset }`（主轴当前偏移与可滚动余量，`offset >= maxOffset` 即停在末端）。内容不足一屏没有可滚动余量，不会发事件。「用户停在底部才跟随最新」「滚离底部时露出回底入口」都按这一对值判断，不用插件自己记账。

### 5.2 事件与取值

节点在 `events` 里声明了哪几种，宿主才回发哪几种（`click` / `change` / `submit` / `linkClick` / `scroll`，`focus` / `blur` 为保留种类、当前无控件产出）。事件值按控件定型：

- `click` 对任何节点生效（含容器）；点击落在子控件上时由子控件优先处理，父节点收不到该次点击；
- 文本类 → 字符串；`submit` 只在单行输入回车时触发；
- 数字输入 → 数值（不可解析为数值不发事件；清空 → `null`，与 0 区分）；
- 勾选 → 布尔；下拉 → 选中项**下标**；菜单条目 → 条目的 `action` 字符串；
- 滚动容器（`list` / `scroll`）→ `{ offset, maxOffset }`（主轴当前偏移与可滚动余量）；
- `markdown` 链接点击 → `{ href, form }`（`form` 为 `external` / `wiki` / `path`；可新建与未识别链接不可点，不产生事件）；`href` 是源文本原样（未解码，wiki 即笔记名），声明与否不影响宿主默认打开。

三者的完整声明见 [API 参考 · 视图事件与指针](plugin-api.md#4-视图事件与指针)。

### 5.3 输入上报：两种模式，二选一

**逐键（缺省）**：用户每次编辑都回发 `change`，插件须把**刚收到的值原样写回**新树——宿主按「插件值是否等于已上报值」判断能否显示本机文本，不回写表现为输入框打不进字。插件写入**另一个值**时接管显示。

**非逐键**：输入控件声明 `report` 后，输入值完全由宿主本地持有并显示，**插件不需要回显**，树里的值可以是旧的：

```jsonc
{ "kind": "textInput", "id": "query", "value": "上次提交的词", "report": "commit", "events": ["change"] }
```

| `report` | 何时回发 `change` |
| --- | --- |
| 缺省 / `"immediate"` | 每次编辑 |
| `"commit"` | 单行按 Enter；多行输入停顿约 600ms（编辑器没有提交信号） |
| `{"debounced": 300}` | 停止编辑 300ms 后 |

非逐键模式下，宿主只在插件下发的值**发生变化**时接管输入；值没变就重发树不会打断正在输入的内容。多行输入另按「既不同于本机当前文本、也不同于已上报值」判断，避免重建编辑器时丢光标。

### 5.4 重发与 diff

界面按整棵树重发，宿主按键 diff（纵向容器按节点 id 复用控件态）。所以：结构尽量稳定、节点 id 稳定且唯一；不要把值编码进 id。

### 5.5 面外层内边距

面板、表格视图与设置页这三个面，宿主默认在根树外层加 12/10 的内边距；整页接管与插件窗口不加。注册贡献时给 `padding` 可改这个值：数字 = 四边等距（`0` 即贴边），也可按轴 `{ "horizontal": 16, "vertical": 8 }` 或按边 `{ "top": 4, "right": 0, "bottom": 4, "left": 0 }` 声明。需要贴边的内容也可放进接管面或窗口面；面板里不要自带同样的外边距（会叠加）。

### 5.6 组合配方（先拼，再加控件）

宿主只提供基元与节点级能力，常用形态直接组合出来。SDK 的 `presets` 模块已把这批组合做成函数，
优先用它；需要自定义时按表里的组合关系拼：

| 形态 | 组合 | SDK |
| --- | --- | --- |
| 可点列表项 | `row` + `interaction`（悬停 / 选中档）+ 调用方声明 `click` | `presets::list_item` |
| 徽标 / chip | `row` + 收缩宽 + 交互外观取语义软底 + 圆角 + 小号 `text` | `presets::chip` |
| 提示块 | `column` + 交互外观（软底 + 同色描边） | `presets::alert` |
| tab 条 | 一排 `row` 条目，当前项 `state: "active"`，各带 `click` | `presets::tabs` |
| 状态点 | 空 `row` + 8×8 尺寸 + 实色底 + 圆角 | `presets::status_dot` |
| 表单行 | `row` + 定宽标签列 + 控件 `grow: 1` | `presets::field_row` |
| 空态 | `column` + `icon` + 两行居中 `text` | `presets::empty_state` |
| 左右分栏 | 行占满宽高，两栏 `grow` 写 35 / 65 | 手写 |
| 状态行 | `row` + `status_dot` + `text`（数值用 `mono`） | 手写 |

拼不出来的形态才值得回到宿主侧讨论——一个诉求一个控件会让协议与渲染分支同时膨胀。

## 6. 自绘画布

画布是树里的叶子控件（`kind: "canvas"`），负责占位与指针事件；内容单独提交：

```rust
ctx.submit_canvas("main", "board", commands).await?;   // 整发整换，空清单 = 清空
```

指令集是封闭枚举：线 / 矩形（含圆角）/ 路径（折线·多边形）/ 圆 / 文字 / 图片贴片，`fill` 与 `stroke` 至少给一个。图片贴片的 `fit` 缺省 = 按给定矩形拉伸；`contain` / `cover` 走等比（cover 的溢出部分裁掉），原图长宽用 `ctx.vault_image_info(path)` 取。字段与校验规则见 [API 参考 · 自绘画布](plugin-api.md#5-自绘画布)（`DrawCommand`）。

坐标一律画布本地逻辑像素、左上原点，宿主不做坐标变换（平移缩放由插件算好坐标）。颜色双轨：`{ "type": "token", "name": "accent" }` 跟主题，`{ "type": "rgba", "rgba": [r,g,b,a] }` 用于内容谱系色（连线分类、播放头等）。

指针事件在节点的 `mouseEvents` 里声明要收的子集：`pointerDown` / `pointerMove` / `pointerUp` / `pointerLeave` / `wheel` / `doubleClick`。`wheel` 增量是行数（正 = 向下 / 向右）；双击由宿主判定（同键二次按下、500ms 内、位移 < 4 像素）。命中测试由插件按指令几何自算——宿主只负责把事件送到画布。

画布节点用 `height` 指定高度、或用 `fill: true` 占满父容器剩余空间。宿主在画布尺寸变化时回发 `host:canvas.resized { surfaceId, nodeId, width, height }`（尺寸不变不发），插件据此对齐绘制坐标与滚动范围——不必再按名义宽度兜底。

两条限制：按住拖出画布不会丢事件（绝对坐标换算，抬起为止）；画布文字永远绘制在所有图形之上，需要被图形遮盖的文字请改用树里的 `text` 控件分层。

## 7. 能力面

在 `permissions` 里声明后，SDK 的 `Context` 对应方法才可用；未声明调用会返回可读错误（`能力未声明`）并计入设置页的「被拒调用」审计。

每个权限名对应的 SDK 方法与协议方法名、params / result 形态，以 [API 参考 · 能力面方法名](plugin-api.md#7-能力面方法名) 为准。这里只记各自的边界与坑：

| 权限 | 边界与约束 |
| --- | --- |
| `state` | 按插件隔离的自持 JSON，落 `<插件目录>/data/state.json`；损坏时宿主先备份原文再降级 |
| `fs` | `private_dir()` 给私有数据目录；其余接受**任意绝对路径**，父目录自动创建 |
| `vault` | 路径相对当前仓库根，不能越出仓库、不开放隐藏段；本地仓库与协作空间同构；**正被编辑器打开的文件拒写**。文本与二进制两条通道，二进制单次上限 32 MiB（更大的文件请用 `fs`）。`vault_image_info` 只解文件头给出图片固有像素尺寸（png / jpeg / gif / bmp / webp），不送回文件内容 |
| `table` | 读写当前打开的表格。真源在宿主，写入与用户编辑同一出口（合并落盘 + 协作补丁）；**禁止**用 `fs` 旁路读写 `.atb`。快照含列宽、行内单元格样式与用户当前选区；向图片字段写图片用 `table_set_cell_image`（图片先经 `vault_write_binary` 落仓库，再传仓库相对路径） |
| `notification` | 应用内通知条 |
| `shortcuts` | 系统级全局快捷键（如 `Shift+Alt+E`）；被占用时注册返回可读错误，停用自动注销；同一组键重复注册幂等，触发上报的是最后一次注册的写法。`query_shortcut` 可事前问某键位是否被占用（`owner` 为占用方插件 id；被系统 / 其他应用占用时为缺省） |
| `clipboard` | 文本与图片（图片为 PNG + 尺寸，由宿主读系统剪贴板） |
| `dialog` | 系统对话框，等用户交互不设上限；取消返回 `None` |
| `http` | 便利层：目标地址、方法、大小、超时全由插件自负，宿主不设限。响应体以 base64 返回；大文件请插件进程自己直连 |
| `process` | 托管子进程：输出与退出经 `host:process.event` 转发；停用 / 崩溃 / 卸载时连同子孙进程一起结束 |
| `window` | 见 §8.2 |
| `ai` | 见 §8.4 |
| `tray` | 见 §8.6 |
| `collab` | 见 §8.7 |

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
    always_on_top: Some(true), skip_taskbar: Some(true), hide_on_blur: Some(true),
};
ctx.window_open(params).await?;   // 内容用 mount("palette", build) 登记后 notify("palette") 提交
// 运行时改尺寸 / 置顶（只给一维时另一维取现值；「不进任务栏」只能在开窗时给）
ctx.window_update(WindowUpdateParams { id: "palette".into(), width: Some(640.0), height: None, always_on_top: None }).await?;
ctx.window_close("palette").await?;
// 因失焦被收起后的唤起回路（宿主收起时会发 host:window.hidden）
ctx.window_show("palette").await?;
```

`title` 就是 OS 标题栏显示的文字。仍开着的同一 id 会被拒，但**插件请求关闭后可以立刻用同 id 重开**——`window_close` 的回执表示「关闭已受理」，不必等系统销毁完成。用户关窗时宿主先发 `host:window.closed`（`{ id }`）、再发 `host:surface.detached`（`kind: "window"`）；插件自己请求的关闭只发后者。`hideOnBlur` 是「隐藏窗口」而非关闭：宿主隐藏时发 `host:window.hidden`，插件可用 `window_show` 重新显示。

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
use atelyx_plugin_sdk::protocol::method::ChatSendParams;

// 发起对话（会话 = 仓库里的普通会话，对话面板可见）；事件经 on_chat_event 流达
let sent = ctx.chat_send("帮我总结这篇笔记", None).await?;

// 完整参数：指定 Agent / 模型，附带仓库内图片
let sent = ctx.chat_send_with(ChatSendParams {
    text: "这张截图里是什么错误？".into(),
    agent_id: Some("my-agent".into()),
    model: Some("gpt-4o".into()),
    images: vec!["attachments/error.png".into()],
    ..ChatSendParams::default()
}).await?;

// 注册一个 Agent 可调用的工具
ctx.register_tool("echo.search", "在笔记里搜索", json!({
    "type": "object",
    "properties": { "query": { "type": "string" } },
    "required": ["query"]
})).await?;
```

- `agent_id`：缺省时新建会话用内置「对话」工具集、续聊沿用会话已记录的 Agent；新建会话的记录会写入该 Agent，续聊时只作用于本轮；指定但不在当前仓库名册即拒绝。
- `model` 缺省沿用对话面板当前选择；同一模型名被多个供应商配置时须给 `provider_id` 消歧（单独给 `provider_id` 拒绝）。
- `images` 是仓库相对路径（png / jpg / jpeg / webp / gif），逐张校验，任一张不可用即整条拒绝（不部分发送）；`text` 为空时 `images` 必须非空。校验通过后、run 读取前文件仍可能消失，读不到的那张在该轮请求里跳过。

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
ctx.subscribe("ai:chat.changed").await?; // AI 会话内容变更（含对话面板里的改动）
```

- `vault:switch` 只在声明了 `keepMountedOnVaultSwitch` 时才有意义：未声明的插件在收到它之前就被宿主重载了。payload：`{ kind: "local", root }` 或 `{ kind: "space", spaceId }`。
- `ai:chat.changed` payload：`{ sessionId, kind }`，`kind` 为 `created` / `messages` / `meta` / `deleted`（新会话、消息增减、标题或 Agent 等元数据变化、删除）。只在变更提交后广播、不补发历史，需要当前内容用 `ctx.chat_history` 重读。
- `table:changed` payload 是当前表格的仓库相对路径；订阅时会先补发一次当前状态（迟到订阅不丢当前表格）。

### 8.6 系统托盘

```rust
ctx.tray_set_items(vec![
    TrayItemSpec { id: "open".into(), label: "打开面板".into(), enabled: None },
    TrayItemSpec { id: "pause".into(), label: "暂停".into(), enabled: Some(false) },
]).await?;   // 整发整换本插件的条目；空清单 = 收起
```

- 全应用共用一枚托盘图标，图标只在「还有条目」时出现；各插件的条目按插件 id 排序，宿主自有的「显示主窗口 / 退出」恒在末尾。
- 用户点击经 `on_tray_click` 回发条目 `id`；置灰条目（`enabled: false`）不响应。
- 至多 32 条，`id` 与文字都要非空、`id` 不得重复，违反即整批拒绝并给可读原因。
- 停用 / 崩溃 / 卸载时宿主摘掉本插件的条目，重启后需要重新提交；插件不必自己收尾。
- 托盘目前只有 Windows 实现，其余平台调用返回可读错误。按失败降级、不要在循环里重试，也别把托盘当必需的入口。

### 8.7 同空间插件互联

只在协作空间里生效，收发双方是**同一空间的同一个插件的其他实例**（各客户端上跑着的本插件）。

```rust
let peers = ctx.collab_peers().await?;      // connected / selfId / peers（不含自己）
let sent = ctx.collab_send("sync", payload, None).await?;   // None = 广播给全部对端
if !sent.queued { /* 本机没连上空间 */ }
ctx.collab_send("sync", payload, Some(&peers.peers[0].peer_id)).await?;   // 定向单播

// 光标 / 选中这类随时被新状态覆盖的在场数据用瞬时档
ctx.collab_send_ephemeral("cursor", payload, None).await?;
```

- 通道名按插件命名空间隔离：宿主在投递时给通道加上发送方插件 id 前缀，收端只会拿到**同一插件的其他实例**发出的消息，`on_collab_message` 里的 `channel` 已去掉前缀。通道名非空、不含 `:`、不超过 64 字符。
- `peerId` 取 `collab_peers()` 里的原值，不要自己拼或猜。未连接空间时 `peers` 为空、`connected = false`；`send` 回 `queued = false`（这是状态而非错误），`queued = true` 只表示已交本机连接发出，不代表对端收到。
- **广播不会回到发送方自己**；自己那份要显示在自己的界面上，请本地回显。
- 两档投递（`collab_send` = 可靠档，`collab_send_ephemeral` = 瞬时档）：
  - 可靠档：断线期间的广播由服务端短期缓存并在重连后补投（缓存有容量上限，超出后补不回来），适合内容类增量；定向单播不缓存、不补投。
  - 瞬时档：不缓存、不补投、不占房间序号，延迟最低；对端断线期间发出的消息**不会**在重连后补达。把本端状态整份重发即可自愈——变化时发、空闲时按十几秒的节奏重发一次，对端据此判断你在场；对端消失后你那份状态会在半分钟内自然过期，不必发「离开」也能收敛。
- 需要可靠的共享请落在仓库内容上，这两档都只适合在线协作期的状态同步。

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

1. **装**：设置 → 插件 → 「从本地目录」选仓库根或 `dist/` 都行——目录安装按所选目录的 `.gitignore` 取内容（`.git` 恒跳过），`target/` 之类被忽略的目录不会进插件目录；总量上限 4 GiB。入口文件若被 `.gitignore` 排除，安装会当场拒收。zip 包走「从 zip 安装」。
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
| 输入框打字被打断 / 光标乱跳 | 逐键模式下插件在 `change` 回填时写入了与收到值不同的值（宿主据此判定接管）；改用 `report` 非逐键上报即可避免 |
| 声明了 `report` 但界面不刷新 | 该档位只在提交/停顿后回发一次 `change`，检查是否漏了 `submit` 事件或停顿时长是否过长 |
| 「能力未声明」 | `plugin.json` 的 `permissions` 少了对应项 |
| 安装被拒「含宿主保留路径」 | 包里有 `data/`、`logs/` 等宿主保留名 |
| 安装被拒「入口文件不在安装内容里」 | 入口被源目录的 `.gitignore` 排除了（常见于产物目录被忽略） |
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
- 扩容只加不改：新增的控件字段都可缺省（旧树照常解析），SDK 的 `PluginHandler` 新增通知回调都带缺省实现，不重写即忽略。
- 插件应把「宿主关闭 stdin」当作正常退出信号，不要依赖宿主发来的额外终止通知。
