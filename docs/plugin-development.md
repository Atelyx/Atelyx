# Atelyx 插件开发

Atelyx 的插件有两种承载：**同进程动态库**（`cdylib`，清单声明 `abiVersion`）与**独立进程**（宿主启动它、通过标准输入输出与它说话）。两种形态都把整个面画成一份帧（绘制指令 + 命中区）交给宿主执行，宿主再把原始输入原样回发；`Context`、`FrameUi`、`PluginHandler` 的写法完全相同。没有 WebView，也没有内置的 JS 运行时。本文以独立进程承载为主线，同进程动态库的差异见 [1.1](#11-同进程动态库承载)。

本文讲**怎么做**：插件形态、15 分钟跑起来的最小实现、界面与能力面的用法、构建打包、本地调试，以及上架到内置市场。逐字段、逐方法的契约在 [API 参考](plugin-api.md)——那份文档由协议源码生成，总会与当前版本一致。

> 协议包 `atelyx-plugin-protocol` 是契约的唯一真源；三处冲突时以包内类型和文档注释为准。

## 1. 形态与边界

| 事实 | 含义 |
| --- | --- |
| 插件 = 原生可执行文件或动态库 | 宿主按 `plugin.json` 里的 `entry` 启动进程，或在宿主进程内加载动态库；工作目录 = 插件目录 |
| 通道 = 进程承载走 stdin / stdout 上的 JSON-RPC 2.0（NDJSON），动态库承载走同一套方法名的 C ABI 回调 | stdout **只能**是协议信封；日志走 stderr（动态库用 `Context::log_error` 写进宿主插件日志） |
| 界面 = 一整面帧 | 插件提交整面帧（绘制指令 + 命中区 + 输入声明）：进程承载写进握手时拿到的共享内存槽位、信封只带槽位坐标，动态库承载由宿主在 `submit_frame` 回调内同步复制；宿主按几何绘制并把原始输入回发，插件里没有宿主 UI 代码，控件只存在于 SDK 控件层 |
| 能力 = 服务 + 方法 | 内置能力（`vault` / `ai` / `fs` …）与插件提供的服务走同一条总线，方法名即 `服务.方法`（见 7） |
| `permissions` = 公示与审计 | 声明用于告诉用户这份插件会碰哪些宿主资源；调用本身不按声明拦截，未声明的调用照常执行但记为「未声明」并在设置页可见 |
| 信任模型 = 本地程序 | 宿主不做沙箱：插件本来就能读写文件、起进程、联网。声明是透明度与审计口径，不是隔离边界 |
| 一个插件一个会话 | 宿主多窗口共享同一份实例；进程承载从启动到停用是一个进程，动态库承载是宿主进程内的一个实例 |

生命周期：启用即启动 → 握手（`initialize`，10 秒内未完成判失败）→ 运行 → 停用 / 崩溃 / 卸载。崩溃按 `1s → 2s → 4s → 8s …`（上限 30s）退避重启，60 秒内崩 5 次熔断，用户在设置页可手动重试。停用会结束**整棵**进程树（动态库形态则销毁实例），并收掉插件经能力面创建的一切（托管子进程、全局热键、托盘条目、打开的窗口、注册的 AI 工具、登记的服务）。

### 1.1 同进程动态库承载

动态库插件的清单多一个 `abiVersion`，入口是 `.dll` / `.so`：

```json
{
  "id": "com.example.echo",
  "name": "Echo",
  "version": "0.1.0",
  "apiVersion": 1,
  "abiVersion": 1,
  "entry": { "windows": "echo_panel.dll", "linux": "libecho_panel.so" },
  "permissions": ["state", "notification"]
}
```

Cargo 声明 `[lib] crate-type = ["cdylib"]`，并从库导出唯一入口：

```rust
#[no_mangle]
pub extern "C" fn atelyx_plugin_api() -> *const atelyx_plugin_sdk::abi::PluginApi {
    atelyx_plugin_sdk::in_process::plugin_api(
        || PluginInfo { id: "com.example.echo".into(), name: "Echo".into(), version: env!("CARGO_PKG_VERSION").into() },
        || Box::new(Echo::new()),
    )
}
```

与进程承载的差异：

- 宿主不启动进程；帧经 `submit_frame` 同步复制，面挂载 / 输入 / 帧拍走 typed 回调，能力调用复用同一套方法名、权限与权限审计。
- 插件日志用 `Context::log_error`（写进宿主插件日志）；动态库的 stderr 在正式构建里没有控制台可看。
- 插件自建线程与持有的 `Context` 克隆要在 `PluginHandler::on_shutdown` 里收掉，否则实例销毁会等它们退出。
- 宿主串行调用插件入口，同一线程可再入；入口里同步调宿主也能拿到回执。需要壳应答的调用（对话框 / 窗口）请在独立线程发起，别占着宿主调用入口的那条线程等——SDK 的泵线程已经这样做。
- `abiVersion` 必须与宿主严格相等；`cargo build` 产出动态库后，用 `scripts/build-plugins.sh` 或安装链落位。

## 2. 环境准备

- Rust 1.85+（`edition = "2021"`）。其它语言也能写插件：只要实现同样的 NDJSON 信封即可，但官方只提供 Rust SDK。
- SDK 与协议目前**未发布到 crates.io**，以 git 依赖宿主仓库；同时开发宿主与插件时改用本地路径更快：

```toml
atelyx-plugin-sdk = { git = "https://github.com/Atelyx/Atelyx" }
# atelyx-plugin-sdk = { path = "../Atelyx/atelyx-plugin-sdk" }
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
use std::sync::atomic::{AtomicU32, Ordering};

use atelyx_plugin_sdk::frame_ui::{FrameUi, TextRole, UiEvent};
use atelyx_plugin_sdk::protocol::frame::FrameInputParams;
use atelyx_plugin_sdk::protocol::method::{ContributionSlot, SurfaceAttachedParams};
use atelyx_plugin_sdk::{run_stdio, Context, PluginHandler, PluginInfo};

/// 面板贡献 id，同时是该面的 surfaceId。
const PANEL: &str = "main";

struct Echo {
    /// 一个面持一个控件层：交互态、量算缓存与主题都随它跨帧保留。
    ui: tokio::sync::Mutex<FrameUi>,
    clicks: AtomicU32,
}

impl Echo {
    /// 出帧 = 提交：状态怎么变，界面就怎么变；帧不会被自动重发。
    async fn render(&self, ctx: &Context) {
        let clicks = self.clicks.load(Ordering::SeqCst);
        let mut ui = self.ui.lock().await;
        let drawn = ui.render(ctx, PANEL, |ui| {
            ui.column(8.0, |c| {
                c.text("你好，Atelyx", TextRole::Heading);
                c.text(format!("点击次数：{clicks}"), TextRole::Label);
                c.button("ping", "点我");
            });
        });
        if let Err(e) = drawn.await {
            eprintln!("提交面帧失败：{e}");
        }
    }
}

#[async_trait::async_trait]
impl PluginHandler for Echo {
    /// 握手完成：注册贡献点、登记服务、提交订阅与首帧都放这里。
    async fn on_ready(&self, ctx: &Context) {
        if let Err(e) = ctx.register(ContributionSlot::Panel, PANEL, "Echo", None, None).await {
            eprintln!("注册贡献失败：{e}");
        }
        self.render(ctx).await;
    }

    /// 面重新进入布局（面板被重新添加 / 插件重启）：补交当前这一帧。
    async fn on_surface_attached(&self, ctx: &Context, params: SurfaceAttachedParams) {
        if params.surface_id == PANEL { self.render(ctx).await; }
    }

    /// 原始输入按到达顺序逐条进来：先出语义事件并改状态，再重画。
    async fn on_frame_input(&self, ctx: &Context, params: FrameInputParams) {
        if params.surface_id != PANEL { return; }
        let events = {
            let mut ui = self.ui.lock().await;
            ui.event(ctx, &params.input, |_| None).await
        };
        for event in events {
            if let UiEvent::Click { id } = event {
                if id == "ping" { self.clicks.fetch_add(1, Ordering::SeqCst); }
            }
        }
        self.render(ctx).await;
    }
}

#[tokio::main]
async fn main() {
    let info = PluginInfo {
        id: "com.example.echo".into(),
        name: "Echo".into(),
        version: "0.1.0".into(),
    };
    let plugin = Echo { ui: tokio::sync::Mutex::new(FrameUi::new()), clicks: AtomicU32::new(0) };
    // 连接结束分两种：宿主关闭 stdin（正常退出，返回 Ok）/ 协议违规或读写失败（Err，非零码退出）
    if let Err(e) = run_stdio(info, plugin).await {
        eprintln!("连接异常结束：{e}");
        std::process::exit(1);
    }
}
```

四条容易踩的规则：

1. **stdout 只允许协议**。`println!` 一行就足以让宿主判定「输出非协议内容」并停掉插件；调试输出用 `eprintln!`（进插件日志）。
2. **帧要自己提交**。帧不自动重发：改完状态不 `render`，界面就停在上一帧；面重新挂载也一样，靠 `on_surface_attached` 补一帧。
3. **输入要先声明**。帧里没有命中区，指针事件就没有 `hit`；没有 `keys` / `ime`，键盘与输入法不进你的回调（见 5.3）。
4. **`on_ready` 里别做长耗时阻塞**。握手回执由 SDK 发出，但 `on_ready` 之后的调用都在同一个连接上；需要长时间准备的工作放 `tokio::spawn`。

## 4. 通道与调用约定

- 插件 → 宿主：一律**请求**，SDK 的 `Context` 方法已经包好；默认等待上限 30 秒（`Context::call_raw`），对话框与 http 不设上限。
- 宿主 → 插件：一律**通知**（`host:` 前缀的方法），由 `PluginHandler` 的对应回调接收。回调**按到达顺序逐个执行**（一条处理完才接下一条），因此改同一份状态不必自己加锁；代价是单个回调卡住会挡住后续通知——弹文件对话框、等长请求这类阻塞调用要么接受「界面整体暂停到用户点完」，要么在回调里另起任务自己驱动。回调可能早于 `on_ready` 到达，依赖初始化结果的逻辑要自带守卫。
- 迟到 / 超时：调用超时只代表「本次调用放弃」，宿主侧业务可能仍在进行（`ai.chat.send` 这类有副作用的调用尤其如此），需要时用查询方法核对。
- 宿主退出：stdin 被关闭，`run_stdio` 返回 `Ok`，插件应正常退出；此时宿主已结束整棵进程树，不要试图常驻。

## 5. 界面：画一帧

一个面一份帧，`ctx.submit_frame(surface_id, frame)` 一次交齐绘制内容、命中区与输入声明；`surface_id` 是 `register` 时的贡献 id，插件窗口面用 `window.open` 的 `id`，文档实例面由宿主分配。帧的逻辑内容如下（SDK 编码后写进握手时拿到的共享内存槽位，信封只带槽位坐标；裸协议自行映射见《插件 API 参考》的「帧共享内存」）：

```json
{
  "commands": [
    { "kind": "rect", "x": 12, "y": 12, "w": 120, "h": 32,
      "fill": { "type": "token", "name": "surface" },
      "stroke": { "type": "token", "name": "border" }, "cornerRadius": 6 },
    { "kind": "text", "x": 28, "y": 20, "content": "点我", "size": 13, "color": { "type": "token", "name": "text" } }
  ],
  "hits": [{ "id": "ping", "x": 12, "y": 12, "w": 120, "h": 32, "cursor": "pointer" }],
  "keys": true
}
```

三条固定语义：

- **整帧替换**：这一帧就是该面此刻的完整界面，没画的东西就没了；空指令 = 空面。要改任何一处都重发整帧，宿主不做增量。
- **没有提交过的面显示占位**（「等待插件提交界面」）；贡献注册了但一直不提交，用户看到的就是这块占位。
- 帧只描述画面与几何，**宿主不理解语义**（哪个矩形是按钮、点下去该做什么，全在插件里），并在提交时校验：指令逐条（错误带指令下标）、命中区 id 非空且唯一、矩形有限非负、`fps` 在 1–120。不合法的帧整份拒收，不会半帧生效。

### 5.1 绘制指令

指令集是封闭枚举，六种：`line` / `rect`（含圆角）/ `path` / `circle` / `text` / `image`。SDK 提供薄构造器：

| 指令 | 构造器 | 要点 |
| --- | --- | --- |
| 线 | `line(x1, y1, x2, y2, stroke)` | 线宽固定 1；要更粗自己拼 `DrawCommand::Line { width, .. }` |
| 矩形 | `rect(x, y, w, h, fill, stroke)` / `rounded_rect(…, radius, fill, stroke)` | `radius` 四角同值 |
| 路径 | `path(points, closed, fill, stroke)` | 至少 2 点；`closed` 闭合首尾（多边形填充用它） |
| 圆 | `circle(cx, cy, r, fill, stroke)` | 圆心 + 半径 |
| 文字 | `text(x, y, content, size, color)` | `(x, y)` 是**左上角** |
| 图片 | `image_tile(source, x, y, w, h)` / `image_tile_fit(…, fit)` | 缺省按矩形拉伸；`contain` / `cover` 等比 |

```rust
use atelyx_plugin_sdk::frame_builders::{
    circle, frame, hit_with_cursor, keys, line, rect, rounded_rect, text, text_wrapped, token,
};
use atelyx_plugin_sdk::protocol::frame::{HitCursor, SurfaceFrame};
use atelyx_plugin_sdk::protocol::paint::ColorToken;

fn frame_of(size: (f32, f32), clicks: u32) -> SurfaceFrame {
    let (w, h) = size;
    let surface = frame(
        vec![
            rect(0.0, 0.0, w, h, Some(token(ColorToken::SurfaceSunken)), None),
            rounded_rect(12.0, 12.0, 120.0, 32.0, 6.0,
                Some(token(ColorToken::Surface)), Some(token(ColorToken::Border))),
            text(28.0, 20.0, format!("点我 {clicks}"), 13.0, token(ColorToken::Text)),
            circle(w - 24.0, 28.0, 6.0, Some(token(ColorToken::Accent)), None),
            line(12.0, 56.0, w - 12.0, 56.0, token(ColorToken::Border)),
            text_wrapped(12.0, 64.0, "长说明按可用宽度换行", 12.0,
                token(ColorToken::TextMuted), w - 24.0, None),
        ],
        vec![hit_with_cursor("ping", 12.0, 12.0, 120.0, 32.0, HitCursor::Pointer)],
    );
    keys(surface)   // 需要键盘才声明，见 5.3
}
```

- **坐标是面本地逻辑像素、左上原点**。宿主不做坐标变换，指令集也没有裁剪原语：要滚动就自己减偏移，要裁剪就自己按矩形收敛坐标（整帧最终按面边界裁剪，不会画进相邻面板）。矩形要投影时给 `shadow` 字段，用 `shadow(offsetX, offsetY, blur, color)` 构造。
- `fill` 与 `stroke` 至少给其一（都不给是空指令，会被拒收）；`strokeWidth` 只在有描边时生效，缺省 1。
- **指令严格按声明顺序绘制**：文字与形状交错，投影随矩形画在它的填充与描边之下。要「被形状盖住的文字」，就把形状声明在文字之后；覆盖式浮层用 SDK 的 `overlay`（见 5.5）。
- 字号 `size` 是绝对逻辑像素（与 `query.measure` 同口径），不是倍率；`weight` 取 `normal` / `medium` / `semiBold`，`family` 取 `ui`（缺省，跟随界面字体）或 `mono`。
- 文字给 `maxWidth` 才换行，`align`（`start` / `center` / `end`）只在给了 `maxWidth` 时成立——对齐区域是 `x` 起、宽 `maxWidth` 的矩形。不给 `maxWidth` 就单行原样绘制，超长部分不截断、直接溢出。
- **颜色两轨**：`token(ColorToken::…)` 跟主题（`text` / `textSecondary` / `textMuted` / `accent` / `accentSoft` / `success` / `warning` / `danger` 及其 `…Soft` / `surface` / `surfaceMuted` / `surfaceSunken` / `hover` / `border` / `borderStrong` / `transparent`），`rgba(r, g, b, a)` 用于内容谱系色（连线分类、播放头）。
- 图片来源四档：`ImageSource::Private { path }`（插件私有目录相对路径）、`Vault { path }`（仓库相对路径）、`Url { url, local }`（`local: true` 走本机 / 局域网策略）、`File { path }`（本机绝对路径）。原图长宽用 `ctx.vault_image_info(path)` 取。

### 5.2 命中区

`hits` 是矩形 + 自定 id + 光标形状，宿主只做几何命中：指针落在哪个命中区上，就把该事件里的 `hit` 填成那个 id，语义由插件解释。

- id 同帧内唯一、非空、≤64 字节，单帧最多 4096 个；**后声明的压住先声明的**（同一坐标落在多个矩形上取最后声明的那条）——要「卡片整块可点、里面的图标钮还能单独点」，先声明卡片、后声明图标钮。
- `cursor` 决定悬停时的指针（`default` / `pointer` / `text` / `grab` / `grabbing` / `crosshair` / `notAllowed` / `resizeHorizontal` / `resizeVertical`），缺省是系统箭头；命中区与绘制指令互不相干——画了按钮忘了声明命中区，它就只是张图，反过来看不见的命中区也能接住点击（热区）。

### 5.3 输入

宿主把原始输入经 `host:frame.input` 回发，SDK 转成 `on_frame_input`，载荷按 `kind` 区分：

| `kind` | 字段 | 说明 |
| --- | --- | --- |
| `pointerDown` / `pointerUp` | `x` `y` `button` `hit` | 左 / 右 / 中键；抬起时 `hit` 是抬起点所在的命中区 |
| `pointerMove` | `x` `y` `hit` | 悬停中只在面内发；按住拖出面外仍继续发（绝对坐标） |
| `pointerLeave` | 无 | 从面内移到面外触发一次 |
| `doubleClick` | 同按下 | 宿主判定（同键、500ms 内、位移 < 4px）；**第二次按下不再单独发 `pointerDown`** |
| `wheel` | `x` `y` `deltaX` `deltaY` `hit` | 像素增量（宿主已把「行」折成 40px），正 = 向下 / 向右 |
| `key` | `key` `action` `modifiers` `text` | `key` 是 `{ "type": "named", "name": "arrowLeft" }` 或 `{ "type": "character", "text": "A" }`；`action` = `press` / `release`（长按会连发 press）；`modifiers` 是 `shift` / `control` / `alt` / `meta` 快照 |
| `text` | `text` `composing` `caret` `candidates` | 输入法：`composing: true` 是组合串更新（空串 = 组合结束，不插入文本），`false` 是已提交文本；`candidates` 是候选窗锚点 |
| `focus` | `focused` | 指针在面内按下即取焦，按到面外即失焦 |
| `resize` | `width` `height` | 面的实际分配尺寸变化时发一次，按它重排 |

三种声明决定你收什么：

- `keys: true` 才收 `key`。**声明后键盘整体归本面**：连 Ctrl / Alt / Super 组合也回发给你，宿主自己的快捷键在此期间让位。只在真有键盘交互（绘图板、编辑器、命令面板）时声明。
- `ime: { x, y, w, h }` 才收 `text`，该矩形是输入光标位置，宿主据此摆放候选窗。**不声明就收不到中文**——输入框必须声明它。
- `fps: 30` 才收 `host:frame.tick { frame, deltaMs }`（1–120）。`frame` 从 0 起自增，`deltaMs` 是距上一拍的实际间隔（首帧 0）；落后多拍时宿主只补一拍。不声明就没有周期唤醒，动画停了下一次出帧把 `fps` 去掉即停拍。

帧输入与帧拍都**不会**自动重发帧：回调里改完状态要自己提交新的一帧。

### 5.4 文字量算与主题

指令里的文字按 `(x, y)` 原样绘制，宿主不做排版，所以「这段字有多宽」要自己算。**不要按字符数估**：中文近似 1 字宽、西文只有一半左右，混排会错位。

```rust
use atelyx_plugin_sdk::protocol::method::MeasureItem;

let items = vec![
    MeasureItem { text: "标题".into(), size: 14.0, weight: None, family: None, max_width: None },
    MeasureItem { text: "说明文字".into(), size: 12.0, weight: None, family: None, max_width: Some(240.0) },
];
let sizes = ctx.measure_batch(items).await?.sizes;   // 顺序与入参一一对应：width / height / lines
```

- 单批最多 **512** 条；给 `max_width` 就按该宽度换行量算并回行数，不给按单行量。一屏文字攒成一批量完，别一条一个往返。
- 结果按「文本 + 字号 + 字重 + 字体族 + 可用宽度」缓存，同一段字反复要用的（行高、列宽、省略位置）自己存住，别在帧回调里每帧量。
- `ctx.query_theme()` 取宿主主题：`colors`（各 token 的 `#rrggbb`）、`fontSizes`（`micro` / `caption` / `ui` / `body` / `h2` / `h1`）、`radii`（`xs` / `sm` / `md`）。主题变了没有通知通道，面重新挂载时重取。

### 5.5 控件层（推荐写法）

不想自己算坐标、命中区与文本编辑，用 `atelyx_plugin_sdk::frame_ui`：控件编译成绘制指令与命中区，悬停 / 按下 / 焦点 / 光标 / 选区 / 输入法组合串 / 复制粘贴 / Tab 切换都在控件层里。宿主仍然只收帧，不认识控件。

一个面持一个 `FrameUi`，按两阶段出帧：**先把输入翻成事件并改状态，再声明控件**。

```rust
use std::sync::Mutex;

use atelyx_plugin_sdk::frame_ui::{FrameUi, Icon, TextRole, UiEvent};
use atelyx_plugin_sdk::protocol::frame::FrameInputParams;
use atelyx_plugin_sdk::{Context, PluginHandler};

#[derive(Default)]
struct TodoState { items: Vec<String>, draft: String }

struct Todo {
    ui: tokio::sync::Mutex<FrameUi>,
    state: Mutex<TodoState>,
}

impl Todo {
    async fn render(&self, ctx: &Context) {
        let (items, draft) = {
            let state = self.state.lock().unwrap();
            (state.items.clone(), state.draft.clone())
        };
        let mut ui = self.ui.lock().await;
        let drawn = ui.render(ctx, "main", |ui| {
            ui.column(8.0, |c| {
                c.text("待办", TextRole::Heading);
                c.text_input("draft", draft.clone(), "输入后回车添加");
                if items.is_empty() { c.caption("还没有条目"); }
                c.scroll("list", 4.0, |s| {
                    for (index, item) in items.iter().enumerate() {
                        s.row(6.0, |r| {
                            r.text(item.clone(), TextRole::Value);
                            r.grow();
                            r.icon_button(format!("del-{index}"), Icon::Eraser);
                        });
                    }
                });
            });
        });
        if let Err(e) = drawn.await {
            eprintln!("提交面帧失败：{e}");
        }
    }
}

#[async_trait::async_trait]
impl PluginHandler for Todo {
    async fn on_ready(&self, ctx: &Context) { self.render(ctx).await; }

    async fn on_frame_input(&self, ctx: &Context, params: FrameInputParams) {
        // 输入框的值以插件状态为准：先取一份快照，闭包里别再取同一把锁
        let draft = self.state.lock().unwrap().draft.clone();
        let events = {
            let mut ui = self.ui.lock().await;
            ui.event(ctx, &params.input, move |id| (id == "draft").then(|| draft.clone())).await
        };
        // 改状态的作用域要先收掉：出帧还要再取一次锁，不能带过去
        {
            let mut state = self.state.lock().unwrap();
            for event in events {
                match event {
                    UiEvent::Text { value, .. } => state.draft = value,
                    UiEvent::Submit { .. } if !state.draft.is_empty() => {
                        let draft = std::mem::take(&mut state.draft);
                        state.items.push(draft);
                    }
                    UiEvent::Click { id } if id.starts_with("del-") => {
                        if let Ok(index) = id[4..].parse::<usize>() {
                            if index < state.items.len() { state.items.remove(index); }
                        }
                    }
                    _ => {}
                }
            }
        }
        self.render(ctx).await;
    }
}
```

几条用法约定：

- **`event` 已经把交互态就地更新**（焦点、光标、选区、组合串、滚动位置），返回的是语义事件：`Click` / `RightClick` / `Text`（完整新值）/ `Submit`（单行输入框回车）/ `Select`（选项下标）/ `Focus` / `Scroll`（偏移与可滚余量）。插件只改自己的业务状态。
- **`value_of` 每次现读**：`event(ctx, input, |id| …)` 按 id 返回输入框当前值，编辑永远从事实出发；**`Text` 要写回**，收到 `UiEvent::Text { value, .. }` 就存进自己的状态，不写回下一帧还是旧值。
- **`render` 的闭包会被调用多次**（文字量算缺项时先按估算排、量完重建，最多 3 轮）：闭包必须只读状态，不要在里面改状态或发请求。
- **内容相同的帧不会重复提交**：指针移动这类没改画面的输入不会把宿主拖进重绘风暴，不必自己判重。
- **尺寸变化立即跟手**：`resize` 到了而文字量算还没补齐时，`render` 会先按估算字宽交一帧，量算落地后再交精确帧（画面没变则不再交）。宿主同一面同时只等一笔尺寸回帧，拖动中的中间尺寸只留最新一笔；拖动持续而插件迟迟不回帧时按 150ms 重发最新尺寸，2 秒仍无新帧就撤掉等待、等下一次尺寸变化。
- **主题只取一次**：首次 `render` 查 `query.theme` 拿字号 / 圆角 / 颜色；面重新挂载时新建一个 `FrameUi` 即重取。**尺寸**由宿主回发的 `resize` 决定并按它排版，首次绘制前用缺省尺寸（首帧可能偏窄，`resize` 一到就重排）。

控件（细节以源码文档注释为准）：

| 控件 | 说明 |
| --- | --- |
| `text(content, role)` | 单行、超出按宽度省略；`role` = `Title` / `Heading` / `Label` / `FieldLabel` / `Caption` / `Value`，字号与颜色由角色定。`caption` / `field_label` 是同角色的换行版（按可用宽度折行） |
| `text_styled(content, TextStyle)` | 按样式绘制：`TextStyle::new(role).color(token).mono().wrap()`（等宽、换行、语义色） |
| `button` / `ghost_button` / `button_enabled` / `ghost_button_enabled` | 常规底 / 无底色 / 带禁用态，点击回 `Click { id }` |
| `text_input(id, value, placeholder)` | 单行输入框；`text_input_sized` 固定宽，`text_input_password` 密文；回车回 `Submit { id }` |
| `text_area(id, value, placeholder, height)` | 多行输入框：软换行与显式换行都排，回车**插入换行**不回 `Submit`，上下键 / 行首尾键按可视行走，光标超出内框时纵向跟随。`text_area_submit` 是回车提交、Shift+回车换行的变体（聊天输入） |
| `select(id, options, selected)` | 下拉；选中回 `Select { id, index }`，展开时**就地撑开**后续内容 |
| `checkbox(id, checked, label)` | 勾选，点击回 `Click { id }`（勾选值由插件自己翻转） |
| `chip(label, color)` | 小号圆角徽标（计数 / 状态标签），不承接点击 |
| `icon_button(id, Icon)` / `icon(Icon)` | 方形图标钮（回 `Click`）/ 纯装饰图标；内置 `Copy` / `Refresh` / `Eraser` / `Download` / `Search` / `HardDrive` / `Wrench` / `FileText` / `Box` |
| `progress(value)` | 0–1 的进度条 |
| `card(id, selected, ...)` | 整块可点卡片（选中档强调底色与描边），回 `Click` / `RightClick`；子控件压住卡片的命中 |
| `card_with_shadow(id, selected, shadow, ...)` | 同上并给卡片加投影（`shadow(offsetX, offsetY, blur, color)` 构造） |
| `overlay(rect, ...)` | 覆盖层：子项按 `rect` 浮在主内容之后绘制，整块承接点击（子控件优先，空白处不穿透下层）；打开期间滚轮只落到浮层内声明过的滚动容器 |
| `panel(...)` | 圆角表面容器，承接子项、不承接点击 |
| `notice(text, color)` | 提示块：整行底色 + 语义色描边与文字 |
| `markdown(content)` | 只读 Markdown：块由宿主解析（`markdown.parse`），控件层按宽度折行并画列表标记 / 引用条 / 代码底 |
| `scroll(id, gap, ...)` / `list(id, gap, ...)` | 滚动容器（吃父容器剩余高度，只画视口内的子项），滚轮回 `Scroll { id, offset, max_offset }`，右下细滚动条可拖 |
| `scroll_sized(id, height, gap, follow_end, ...)` | 定高滚动区；`follow_end` 时停在终点就跟到新终点（日志、消息流） |
| `rule()` / `spacer(h)` / `grow()` | 分隔线 / 固定高度空白 / 吃剩余宽度的弹簧 |
| `pad(horizontal, vertical, ...)` | 内边距容器：子项按列排布并整体内缩（滚动条要贴边、内容又要留白时用） |
| `column(gap, ...)` / `row(gap, ...)` / `row_fill(gap, ...)` | 纵向 / 横向 / 占满剩余高度的横向容器（并排分栏各自滚动） |

Markdown 不要自己解析：`ui.markdown(text)` 首次出现时向宿主发一次 `markdown.parse`，按原文缓存（缓存有限，滚出视野的旧内容会被收敛掉），内容不变不重取。

三条边界：指令集没有裁剪，滚动只画视口内的子项；需要覆盖主内容的浮层用 `ui.overlay(rect, ...)`（画在主内容之后，打开时滚轮只落到浮层里的滚动区），下拉仍就地展开；无障碍基本没有（自绘帧对读屏器只是一块区域，键鼠可达性要自己保证）；观感一致性靠 SDK 默认样式，不要写死色值和像素。

要自带 GUI 运行时的插件可以自己创建系统窗口跑自有渲染（不走 `window.open`），但那种界面不参与工作台组合：没有面板 dock、没有标签页，宿主主题与快捷键也不生效，插件一停就跟着消失。想留在工作台里，就走帧。

### 5.6 自绘标题栏与无边框窗口

全接管面与插件窗口可以自绘标题栏。拖动不要自己算窗口坐标，把按下交给宿主：

```rust
// 标题栏的命中区收到 pointerDown 后
ctx.begin_window_drag("main").await?;   // 宿主接管拖动手势
```

面在插件窗口 / 接管面里就拖那个窗口；解析不到时拖当前聚焦窗口。缩放沿用窗口边缘，关闭走宿主关窗链。插件窗口用 `window.open` 开且 `decorations: false` 时，缺省由宿主补一条标题栏（标题 / 拖动 / 最小化 / 最大化 / 关闭）；再给 `titlebar: Some(false)` 就整面归插件，自绘标题栏里的最小化 / 最大化按钮调 `ctx.window_minimize(id)` / `ctx.window_toggle_maximize(id)`。

### 5.7 面外层内边距

面板、表格视图与设置页这三个面，宿主默认把帧铺满内容区（贴边）；接管面与插件窗口同样贴边，且不受 `padding` 影响。需要留白时在注册这三个面的贡献时给 `padding`：数字 = 四边等距，也可按轴 `{ "horizontal": 16, "vertical": 8 }` 或按边 `{ "top": 4, "right": 0, "bottom": 4, "left": 0 }` 声明，缺省 / `0` 即贴边。

## 6. 能力面

`plugin.json` 的 `permissions` 是**公示与审计**口径，不是开关：调用本身不按声明拦截，未声明的调用照常执行，但会被记为「未声明」——插件详情页的「能力调用记录」里能看见，首次出现还会给用户弹一条提示。所以声明要写全：漏写不会让功能坏掉，但会让用户看到一条不该出现的记录。

每个权限名对应的 SDK 方法与协议方法名、params / result 形态，以 [API 参考 · 能力面方法名](plugin-api.md#9-能力面方法名) 为准。这里只记各自的边界与坑：

| 权限 | 边界与约束 |
| --- | --- |
| `state` | 按插件隔离的自持 JSON，落 `<插件目录>/data/state.json`；损坏时宿主先备份原文再降级 |
| `fs` | `private_dir()` 给私有数据目录；其余接受**任意绝对路径**，父目录自动创建 |
| `vault` | 路径相对当前仓库根，不能越出仓库；隐藏段可访问（插件自管附件可放 `.atelyx/temp/`），本地仓库与协作空间的 `vault_list` / `vault_exists` 同口径保留隐藏项（原子写残留 `.tmp` 除外）；**正被编辑器打开的文件拒写**。文本与二进制两条通道，二进制单次上限 32 MiB；更大的文件先落到插件私有目录再用 `vault_import_file` 让宿主按路径收进仓库（字节不进 RPC，源文件须在私有目录内，复制语义，见 §8.9）。`vault_image_info` 只解文件头给出图片固有像素尺寸（png / jpeg / gif / bmp / webp），不送回文件内容。结构变更用 `vault_rename(from, to)` / `vault_remove(path)`：文件或文件夹（递归），`to` 不可已存在、文件夹不可移入自身、仓库根不可删；改名会同步 `.md` 内部链接与画布节点引用。**路径正被笔记 / 表格 / 画布 / 插件文档打开，或被当前画布引用时拒收**——这些持有者仍按旧路径工作，放行后写回会产生幽灵文件，先关闭对应面再操作 |
| `table` | 读写当前打开的表格。真源在宿主，写入与用户编辑同一出口（合并落盘 + 协作补丁）；**禁止**用 `fs` 旁路读写 `.atb`。快照含稳定表格 id、列宽、行内单元格样式、用户当前选区与其他协作者选区（显示名 + 身份色）；向图片字段写图片用 `table_set_cell_image`（图片先经 `vault_write_binary` 落仓库，再传仓库相对路径） |
| `notification` | 应用内通知条 |
| `shortcuts` | 系统级全局快捷键（如 `Shift+Alt+E`）；被占用时注册返回可读错误，停用自动注销；同一组键重复注册幂等，触发上报的是最后一次注册的写法。`query_shortcut` 可事前问某键位是否被占用（`owner` 为占用方插件 id；被系统 / 其他应用占用时为缺省） |
| `clipboard` | 文本读写、图片读取（PNG + 尺寸，由宿主读系统剪贴板）与图片写入（`clipboard_write_image(&png)`，PNG 载荷单次 32 MiB，解码后的像素量再挡一次；失败返回可读错误） |
| `dialog` | 系统对话框，等用户交互不设上限；取消返回 `None` |
| `http` | 便利层：目标地址、方法、大小、超时全由插件自负，宿主不设限。响应体以 base64 返回；大文件请插件进程自己直连 |
| `process` | 托管子进程：输出与退出经 `host:process.event` 转发；停用 / 崩溃 / 卸载时连同子孙进程一起结束 |
| `window` | 见 §8.2 |
| `ai` | 见 §8.4 |
| `tray` | 见 §8.6 |
| `collab` | 见 §8.7 |

`vault` 的读写**必须**经能力面：直接改盘会绕过写盘队列与协作通道，属于内容丢失级缺陷。

## 7. 能力总线（服务）

能力只有一种描述方式：**服务 + 方法**。方法名本身就是 `服务.方法`——`vault.readText` 是服务 `vault` 的方法 `readText`，SDK 的 `ctx.vault_read_text(...)` 与 `ctx.call_raw("vault.readText", …)` 落到同一条目录条目、同一个实现。

```rust
let dir = ctx.service_list().await?;   // 每个条目：service / version / methods / provider / replaceable / candidates
let text = ctx.service_call("vault", "readText", json!({ "path": "notes/a.md" })).await?;
```

- `service_list` 是目录：`provider` 缺省表示当前由内置实现承载，给了值就是提供它的插件 id；`candidates` 是其余候选；`replaceable: false` 表示这个服务不允许被顶替。
- `service_call` 只是「名字要到运行时才知道」时的调用形态（例如让用户在下拉里选服务再调）；名字写死的场合直接用 `Context` 的定型方法，等价且更省心。

### 7.1 做提供者

```rust
use serde_json::json;
use atelyx_plugin_sdk::protocol::service::HostCallParams;

#[async_trait::async_trait]
impl PluginHandler for Demo {
    async fn on_ready(&self, ctx: &Context) {
        if let Err(e) = ctx.register_service("demo", &["echo"]).await {
            eprintln!("登记服务失败：{e}");
        }
    }

    /// 本插件是 demo 的活动提供者时，宿主把调用转到这里；用同一个 callId 回报。
    async fn on_service_call(&self, ctx: &Context, params: HostCallParams) {
        let replied = match params.method.as_str() {
            "echo" => ctx.service_result(&params.call_id, json!({ "echo": params.params })).await,
            other => ctx.service_error(&params.call_id, &format!("unknown_method：{other}")).await,
        };
        if let Err(e) = replied {
            eprintln!("回报服务调用失败：{e}");
        }
    }
}
```

- **登记不等于生效**：同一服务同时只有一个活动提供者。已有内置实现的服务（`vault` / `ai` / `markdown` …）登记后只成为候选，是否顶替由用户在设置页决定；内置实现的归属插件停用期间该实现不在场，此时登记的候选直接接任（用户重新启用归属插件后再由设置页决定）；目录里没有的服务，首个提供者即活动提供者。调用方最终拿到的是**你回报的那份数据**：`service_result` 给结果、`service_error` 给可读原因，二者都带调用方的 `callId`；不实现 `on_service_call` 时默认回报「本插件不提供该服务」，宿主不必等到超时。
- 30 秒不回报按 `timeout` 失败；**停用 / 崩溃 / 卸载时在途调用以 `provider_unavailable` 失败**，之后的调用回落内置实现（没有内置实现就成了 `unknown_service`）。`host:service.changed` 广播活动提供者变化（`{ service, provider, available }`，`provider` 缺省 = 内置），要靠「本插件现在是否生效」改界面就覆写 `on_service_changed`。
- **不要在自己的 `on_service_call` 里等自己提供的方法**：宿主通知与这条调用走同一个通知 worker，会自锁。要自检就 `tokio::spawn` 到别的任务里调。

### 7.2 不能做的

- 不能登记宿主的通道命名空间：`service` / `view` / `frame` / `layout` / `event` / `doc`。这些是界面提交与事件通路的入口，被接管等于把插件自己从界面上摘掉，登记时直接拒收。
- 不能替换 `query`：字体与主题取的是宿主 OS 与用户设置资源，外置后宿主自身外观会和设置脱节。
- `markdown.parse` **可以**被替换（解析语义与宿主笔记同源即可）。替换生效后，已经按原文缓存过解析结果的内容不会自动重取——重建 `FrameUi` 或内容变化才会再取一次。

## 8. 进阶

### 8.1 接管窗口

```rust
ctx.takeover(TakeoverScope::Page, "main").await?;   // 内容区归插件，宿主标题栏保留
ctx.takeover(TakeoverScope::Full, "main").await?;   // 整窗归插件（含标题栏区域）
```

`surfaceId` 必须是已注册贡献的 id——该面的帧原样接管过去。接管按窗口生效：布局真源不动，退出即恢复原视图。全接管时插件自绘标题栏：标题栏的命中区收到按下后调 `ctx.begin_window_drag("main")` 交给宿主拖动手势，缩放沿用窗口边缘，关闭走宿主关窗链；退出入口由宿主常驻（右上角按钮 + Esc）。

### 8.2 插件窗口

```rust
let params = WindowOpenParams {
    id: "palette".into(),      // 插件内唯一、不含 ':'，同时是窗口面的 surfaceId
    title: "命令面板".into(),
    width: Some(560.0), height: Some(360.0),
    always_on_top: Some(true), skip_taskbar: Some(true), hide_on_blur: Some(true),
    decorations: None,          // Some(false) = 去掉系统边框，由宿主补标题栏（标题 / 拖动 / 最小化 / 最大化 / 关闭）
    titlebar: None,             // 仅 decorations: Some(false) 时生效；Some(false) = 标题栏也交给自己画
};
ctx.window_open(params).await?;   // 窗口内容同样是帧：ui.render(ctx, "palette", ...) 提交
// 运行时改尺寸 / 置顶 / 标题（只给一维时另一维取现值；「不进任务栏」只能在开窗时给）
ctx.window_update(WindowUpdateParams {
    id: "palette".into(), width: Some(640.0), height: None, always_on_top: None, title: None,
}).await?;
ctx.window_minimize("palette").await?;         // 自绘标题栏的最小化钮
ctx.window_toggle_maximize("palette").await?;  // 自绘标题栏的最大化钮
ctx.window_close("palette").await?;
// 因失焦被收起后的唤起回路（宿主收起时会发 host:window.hidden）
ctx.window_show("palette").await?;
```

`title` 就是窗口标题：有系统边框时作为 OS 标题栏文字，`decorations: Some(false)` 时由宿主画在自己的标题栏上（附带拖动、双击切换最大化、最小化 / 最大化 / 关闭），插件内容占满标题栏以下的区域。`titlebar: Some(false)` 则宿主连标题栏也不画，整面归插件自绘（拖动 / 最小化 / 最大化 / 关闭见 5.6）。`window_update` 改 `title` 更新宿主自绘标题栏上的文字；系统边框窗口的 OS 标题由系统绘制，运行期不随之变化。仍开着的同一 id 会被拒，但**插件请求关闭后可以立刻用同 id 重开**——`window_close` 的回执表示「关闭已受理」，不必等系统销毁完成。用户关窗时宿主先发 `host:window.closed`（`{ id }`）、再发 `host:surface.detached`（`kind: "window"`）；插件自己请求的关闭只发后者。`hideOnBlur` 是「隐藏窗口」而非关闭：宿主隐藏时发 `host:window.hidden`，插件可用 `window_show` 重新显示。

### 8.3 领域文档类型

在 `plugin.json` 里注册扩展名，宿主负责文件树图标、新建菜单与打开路由：

```json
"docTypeRegistrations": [
  { "extension": "board", "label": "白板", "icon": "layout-dashboard", "surface": "doc" }
]
```

`surface` 必须同时用 `view.register` 注册为 `panel` 贡献。用户打开这类文件时宿主发 `host:doc.open { surfaceId, path }`，插件按 `surfaceId` 提交这一面的帧；关闭或换文件时先发 `host:doc.close`。同一时间至多一个打开的插件文档（切换文件即换实例）；文件被删除或切仓库也会关闭。同一扩展名被多个插件注册时按插件 id 取第一个并提示。

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

// 枚举可用项（自建选择器用）：模型按供应商分组、Agent 含内置与仓库自定义，各带面板当前选择
let models = ctx.ai_models().await?;   // providers: [{ id, name, models: [{ id, nickname }] }], current
let agents = ctx.ai_agents().await?;   // agents: [{ id, name, builtin }], current

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
ctx.subscribe("table:selection.changed").await?; // 当前表格选区移动（本端与协作者）
ctx.subscribe("ai:chat.changed").await?; // AI 会话内容变更（含对话面板里的改动）
ctx.subscribe("vault:file.changed").await?; // 仓库文件被宿主写入（保存 / 新建 / 删除 / 改名）
```

- `vault:switch` 只在声明了 `keepMountedOnVaultSwitch` 时才有意义：未声明的插件在收到它之前就被宿主重载了。payload：`{ kind: "local", root }` 或 `{ kind: "space", spaceId }`。
- `ai:chat.changed` payload：`{ sessionId, kind }`，`kind` 为 `created` / `messages` / `meta` / `deleted`（新会话、消息增减、标题或 Agent 等元数据变化、删除）。只在变更提交后广播、不补发历史，需要当前内容用 `ctx.chat_history` 重读。
- `table:changed` payload 是当前表格的仓库相对路径；订阅时会先补发一次当前状态（迟到订阅不丢当前表格）。
- `table:selection.changed` payload：`{ path?, selection?, remoteSelections? }`——当前表格路径、本端选区与协作者选区（`name` / `color`（`#rrggbb`）/ `selection`），选区形态为 `cell` / `range` / `row` / `column` / `all`。内容不变、只有选区移动也发；没有打开表格时用空载荷通知一次。载荷直接带选区，不必为一次拖选重取快照。
- `vault:file.changed` payload：`{ path, kind, oldPath? }`，`kind` 为 `created` / `updated` / `deleted` / `renamed`（改名另带旧路径）。只覆盖**宿主发起的写入**——壳自身保存 / 新建 / 复制 / 删除 / 改名与插件经 `vault` 的写入；外部程序直接改磁盘不发，需要感知外部改动仍要按 mtime 轮询。删除目录只发目录本身一条，其下文件不逐条发（按前缀作废缓存，需要明细就重新列举）。

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

### 8.8 默认布局声明

插件可以声明一套默认面板分栏，首次启用时宿主自动摆进当前场景：

```rust
use atelyx_plugin_sdk::protocol::layout::{Axis, LayoutSpecNode};

ctx.declare_default_layout(
    "控制台",
    LayoutSpecNode::Split {
        direction: Axis::Horizontal,
        children: vec![
            LayoutSpecNode::Panel { views: vec!["files".into()] },
            LayoutSpecNode::Panel { views: vec!["note".into(), "aichat".into()] },
        ],
        sizes: vec![30.0, 70.0],
    },
).await?;
```

- `spec` 是分割树：`split { direction: "horizontal" | "vertical", children, sizes }` 或 `panel { views }`；`views` 写视图 kind——本插件的面板贡献为 `plugin:<插件 id>:<贡献 id>`，预置插件的视图用 `canvas` / `note` / `table` / `files` / `search` / `aichat` 等，首个为激活标签。
- 只摆一次：宿主在 `ui-state.json` 记账，用户改名 / 删除后不会再补回；首次启用时当前场景已含规格里的任一视图则只记账不新建，避免与用户已摆好的界面重复。
- 布局名与既有布局重名时宿主追加序号并激活新布局；规格超限（深度 16、面板 32、单面板视图 16、名字 64 字节）或非法即拒收。

### 8.9 大文件进仓库

超过单次载荷上限（32 MiB）的文件不走字节通道：先落到插件私有目录，再让宿主按路径收进去。

```rust
let dir = ctx.private_dir().await?;                          // <插件目录>/data
let staged = format!("{dir}/download.bin");                  // 自己的进程用 std::fs 写即可
ctx.vault_import_file("models/big.bin", &staged).await?;     // 宿主原子落库（本地复制 / 空间上传）
ctx.remove(&staged).await.ok();                              // 导完自己清掉暂存
```

- 源文件必须在 `<插件目录>/data` 内（解析符号链接后按真实落点判定），目录外一律拒收——`vault` 不替插件读仓库外的路径，要读别处仍走 `fs`。`private_dir()` 与删除都属 `fs` 面，`permissions` 里要同时声明 `vault` 与 `fs`。
- 复制语义：落库成功后源文件仍在，要清理自己删（大文件建议导完即删，别把私有目录撑爆）。
- 不受单次 32 MiB 上限约束，也不按默认 30s 调用超时判失败；协作空间仍受服务端单文件上限（默认 50 MB）。
- 目标路径规则与 `vault_write_binary` 相同：仓库相对、父目录自动创建、正在编辑器中打开的文件拒写。

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
3. **看日志**：详情 →「日志」。这里能看到插件的 stderr、协议违规行、握手 / 崩溃 / 熔断原因；文件在 `<应用数据目录>/plugins/<id>/logs/runtime.log`（超过 2 MiB 轮转一份 `.1`；版本号变了，装载时会清空旧日志）。
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
| 面一直显示「等待插件提交界面」 | 没提交帧，或提交的 `surfaceId` 与贡献 id 不一致（面按贡献 id 取帧） |
| 界面停在旧内容 | 改了状态没重新提交帧——帧不自动重发，回调里要自己 `render` / `submit_frame` |
| 点了没反应 | 帧里没声明该处的命中区、`hit` 的 id 与处理分支不一致，或点击被后声明的命中区抢走 |
| 中文打不进 / 收不到按键 | 帧没声明 `ime`（输入法）/ `keys`（按键）；输入框聚焦后要重新出帧才会带上 `ime` 光标 |
| 文字重叠或错位 | 没走 `query.measure` 而按字符数估宽；换行只在给了 `maxWidth` 时成立 |
| 声明的快捷键被界面吞掉 | 帧声明了 `keys`：键盘整体归本面（含 Ctrl / Alt 组合），只在真有键盘交互时声明 |
| 插件详情页出现「未声明」记录 | `permissions` 漏了对应项：调用照常执行，但会被记为未声明并提示用户 |
| `service.call` 报 `provider_unavailable` | 提供者没启用、已停用 / 崩溃，或活动提供者已被用户切回内置 |
| `service.register` 被拒 | 服务名落在宿主通道命名空间（`view` / `frame` / …），或该服务不可替换（`query`） |
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

- `apiVersion` / `abiVersion` 都是硬闸门：宿主只接受完全相等的版本，版本不符的插件响亮拒绝，不做兼容推断。破坏性变更是否递增版本号以宿主 1.0.0 为界——之前不递增（握手 / 加载仍要求严格相等，插件随宿主更新重建即对齐），之后必须递增。
- 绘制指令、输入形态、贡献槽位、服务方法名都是封闭枚举 / 受限形状，扩容随协议升版；未知取值会被拒绝而不是被忽略。
- 宿主正式发布 1.0.0 之前不承诺向后兼容：协议形状直接改，插件随宿主重建即对齐（`apiVersion` 仍要求严格相等）。
- SDK 的 `PluginHandler` 新增通知回调都带缺省实现，不重写即忽略；`frame_ui` 的控件与容器新增不影响既有调用。
- 插件应把「宿主关闭 stdin」当作正常退出信号，不要依赖宿主发来的额外终止通知。
