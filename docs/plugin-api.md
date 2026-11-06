# 插件 API 参考

Atelyx 插件协议的全部公开类型与方法名。概念、取舍与常见坑见 [插件开发](plugin-development.md)；这里只列契约本身。

> 本文件由 `cargo run -p xtask -- docs` 从 `atelyx-plugin-protocol/` 源码生成，请勿手改。
> 协议改动后重跑一次即可；忘记重跑时 `cargo test --workspace` 会失败。

## 1. 契约版本与清单

`plugin.json` 的结构、能力声明与宿主保留的清单文件名。`apiVersion` 是唯一契约版本，必须与宿主相等，不等即拒绝装载。

```rust
// atelyx-plugin-protocol/src/lib.rs
/// 契约版本：取 [`atelyx_plugin_abi::CONTRACT_VERSION`] 这同一个值，宿主与插件的 manifest、
/// 握手、函数表都按它严格相等校验。协议冻结为稳定契约前不递增本号。
pub const API_VERSION: u32 = atelyx_plugin_abi::CONTRACT_VERSION;
```

```rust
// atelyx-plugin-protocol/src/manifest.rs
/// manifest 文件名（固定放在插件目录根部）。
pub const MANIFEST_FILE: &str = "plugin.json";
```

```rust
// atelyx-plugin-protocol/src/manifest.rs
/// 插件 manifest。缺字段按 [`Default`] 补齐，未知字段忽略。
#[serde(rename_all = "camelCase", default)]
pub struct Manifest {
    /// 反向域名 id（如 `com.atelyx.echo`），发布后不可变；同时是插件目录名与隔离命名空间。
    pub id: String,
    pub name: String,
    pub version: String,
    /// 契约版本（必填），必须等于宿主的 [`crate::API_VERSION`]；缺失按 0 处理并被结构校验拒绝。
    pub api_version: u32,
    /// 最低宿主版本（语义化版本，如 `0.4.0`）；缺省 = 不限。宿主低于它即拒绝装载。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_host_version: Option<String>,
    pub entry: Entry,
    /// 能力声明清单：用于向用户公示与审计，不拦截调用——插件与宿主同进程、持同等权限，
    /// 闸门拦住的是守规矩的插件。宿主据此标出「用到但未声明」的调用。
    pub permissions: Vec<Permission>,
    /// 单实例约束（多窗口共享同一宿主运行时全局单例）。
    pub singleton: bool,
    /// 切换仓库容器（本地 ↔ 空间 / 换仓库）时保持挂载；false = 宿主停掉实例后按新容器重启。
    pub keep_mounted_on_vault_switch: bool,
    pub doc_type_registrations: Vec<DocTypeRegistration>,
}
```

```rust
// atelyx-plugin-protocol/src/manifest.rs
/// 按平台的入口（可执行文件或动态库，相对插件目录的路径）。缺失 = 该平台不支持。
#[serde(rename_all = "camelCase", default)]
pub struct Entry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub windows: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linux: Option<String>,
}
```

```rust
// atelyx-plugin-protocol/src/manifest.rs
/// 能力面枚举（封闭）：扩容随协议升版。名字与宿主能力面一一对应。
#[serde(rename_all = "camelCase")]
pub enum Permission {
    Fs,
    Http,
    Process,
    State,
    Notification,
    Shortcuts,
    Vault,
    Clipboard,
    Window,
    Tray,
    Collab,
    Ai,
    Dialog,
}
```

```rust
// atelyx-plugin-protocol/src/manifest.rs
/// 领域文档类型注册：声明扩展名、内容真源的归属插件与可承载它的视图贡献。
#[serde(rename_all = "camelCase", default)]
pub struct DocTypeRegistration {
    /// 插件内稳定的文档类型 id（如 `table`）；宿主状态与路由以它为准。
    pub id: String,
    /// 不带点的扩展名（如 `board`）；一个类型可声明多个，比较不区分大小写。
    pub extensions: Vec<String>,
    /// 持内容真源、定解析 / 合并 / 保存语义的插件 id；通常与声明插件相同。
    pub owner_plugin: String,
    /// 能承载该文档类型的视图贡献 id 列表（贡献由声明插件注册）。
    pub views: Vec<String>,
    /// 文档内容可编辑；false = 只读。
    pub editable: bool,
    /// 允许同类多文档并行打开（false = 每类型至多一个热文档）。
    pub multi_doc: bool,
    pub label: String,
    /// 宿主内置形状名（未知名回落拼图）。
    pub icon: String,
}
```

## 2. 握手与信封

插件启动后的第一个请求是 `initialize` 握手；此后能力调用与宿主事件都按本节的请求 / 通知 / 响应结构传递——跨同进程动态库只复制 JSON 字符串（函数调用），不成文本流。

```rust
// atelyx-plugin-protocol/src/handshake.rs
pub const INITIALIZE: &str = "initialize";
```

```rust
// atelyx-plugin-protocol/src/handshake.rs
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub api_version: u32,
    pub plugin: PluginInfo,
}
```

```rust
// atelyx-plugin-protocol/src/handshake.rs
#[serde(rename_all = "camelCase")]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub version: String,
}
```

```rust
// atelyx-plugin-protocol/src/handshake.rs
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub api_version: u32,
    pub host: HostInfo,
}
```

```rust
// atelyx-plugin-protocol/src/handshake.rs
#[serde(rename_all = "camelCase")]
pub struct HostInfo {
    pub name: String,
    pub version: String,
}
```

```rust
// atelyx-plugin-protocol/src/envelope.rs
/// 一行信封的三种形态。解析失败给出可读原因（含行首片段），不静默丢弃。
///
/// 行为契约：`id` 必须是数字（字符串 id 与显式 `null` 都按形态非法拒绝）；响应携带 `params`
/// 视为形态非法拒绝；空行是否跳过由读循环决定（协议只承诺「空行解析必报错」）。
pub enum Envelope {
    Request(Request),
    Notification(Notification),
    Response(Response),
}
```

```rust
// atelyx-plugin-protocol/src/envelope.rs
pub struct Request {
    pub jsonrpc: &'static str,
    /// 请求 id：本协议只认数字（发起方自增，单调不复用）。
    pub id: u64,
    pub method: String,
    #[serde(skip_serializing_if = "Value::is_null")]
    pub params: Value,
}
```

```rust
// atelyx-plugin-protocol/src/envelope.rs
pub struct Notification {
    pub jsonrpc: &'static str,
    pub method: String,
    #[serde(skip_serializing_if = "Value::is_null")]
    pub params: Value,
}
```

```rust
// atelyx-plugin-protocol/src/envelope.rs
pub struct Response {
    pub jsonrpc: &'static str,
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}
```

```rust
// atelyx-plugin-protocol/src/envelope.rs
/// 错误码：-32700..-32603 为 JSON-RPC 规范保留段；-32001..-32099 为本协议自定义段。
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}
```

## 3. 能力总线

能力按「服务 + 方法」登记：方法名本身就是 `服务.方法`，所以目录按第一个 `.` 切分即得。内置实现是出厂的活动提供者；外部提供者登记后只成为候选，切换由用户决定。`service.call` 是名字要到运行时才知道时的信封形态，与直接调用同名方法解析到同一条目、同一个实现。

```rust
// atelyx-plugin-protocol/src/service.rs
/// 方法名常量：提供者登记与调用。
pub mod method {
    /// 提供者声明自己提供哪些服务方法（`{ service, version, methods }`）。登记不等于生效：
    /// 同一服务同时只有一个活动提供者，切换由用户决定（见 [`ServiceInfo::active`]）。
    pub const REGISTER: &str = "service.register";
    /// 撤销自己声明的服务（连带把它从候选中摘掉；活动提供者撤销后该服务回落内置实现）。
    pub const UNREGISTER: &str = "service.unregister";
    /// 枚举在册服务：服务名、版本、方法名、活动提供者与其余候选。
    pub const LIST: &str = "service.list";
    /// 调用某个服务的方法：`{ service, method, params }`，应答就是该方法自己的结果。
    pub const CALL: &str = "service.call";
    /// 宿主请求提供者执行一次调用（通知）：提供者用同一 `callId` 经 [`RESULT`] 回报。
    pub const HOST_CALL: &str = "host:service.call";
    /// 提供者回报一次调用的结果（`{ callId, result }` 或 `{ callId, error }`）。
    pub const RESULT: &str = "service.result";
    /// 活动的提供者变了（通知）：`service` 可用性变化时广播给全部运行中插件。
    pub const HOST_CHANGED: &str = "host:service.changed";
}
```

```rust
// atelyx-plugin-protocol/src/service.rs
/// 服务版本的当前取值。契约不兼容的改动递增它，调用方据此判断能否沿用旧参数。
pub const SERVICE_VERSION: u32 = 1;
```

```rust
// atelyx-plugin-protocol/src/service.rs
/// 服务名与方法名的字节上限（两者同口径）。
pub const MAX_METHOD_NAME: usize = 64;
```

```rust
// atelyx-plugin-protocol/src/service.rs
/// 单个服务声明的方法数上限（`RegisterParams::methods` 的长度上限）。
pub const MAX_METHODS_PER_SERVICE: usize = 128;
```

```rust
// atelyx-plugin-protocol/src/service.rs
/// `service.register` 的 params：声明本插件提供某个服务的若干方法。
#[serde(rename_all = "camelCase")]
pub struct RegisterParams {
    /// 服务名（与内置服务同名即成为它的候选提供者）。
    pub service: String,
    /// 契约版本；缺省 = [`SERVICE_VERSION`]。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    /// 本提供者实现的方法名（不含服务前缀；分组方法可自带 `.`，如 `chat.send`）。
    pub methods: Vec<String>,
}
```

```rust
// atelyx-plugin-protocol/src/service.rs
/// `service.unregister` 的 params。
#[serde(rename_all = "camelCase")]
pub struct UnregisterParams {
    pub service: String,
}
```

```rust
// atelyx-plugin-protocol/src/service.rs
/// 一个服务的当前状态（`service.list` 的条目）。
#[serde(rename_all = "camelCase")]
pub struct ServiceInfo {
    pub service: String,
    /// 活动提供者的契约版本。
    pub version: u32,
    /// 活动提供者实现的方法名。
    pub methods: Vec<String>,
    /// 活动提供者：`None` = 内置实现。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// 内置实现能否被替换（字体 / 主题这类平台自救面不可替换）。
    pub replaceable: bool,
    /// 其余候选提供者（插件 id，按登记序）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<String>,
}
```

```rust
// atelyx-plugin-protocol/src/service.rs
/// `service.list` 的 result：按服务名排序。
#[serde(rename_all = "camelCase")]
pub struct ListResult {
    pub services: Vec<ServiceInfo>,
}
```

```rust
// atelyx-plugin-protocol/src/service.rs
/// `service.call` 的 params。`params` 就是目标方法自己的入参（可为 null）。
#[serde(rename_all = "camelCase")]
pub struct CallParams {
    pub service: String,
    /// 方法名（不含服务前缀；分组方法自带 `.`，如 `chat.send`）。
    pub method: String,
    #[serde(default)]
    pub params: Value,
}
```

```rust
// atelyx-plugin-protocol/src/service.rs
/// `host:service.call` 的 params：宿主代调用方请求提供者执行一次调用。
#[serde(rename_all = "camelCase")]
pub struct HostCallParams {
    pub call_id: String,
    pub service: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}
```

```rust
// atelyx-plugin-protocol/src/service.rs
/// `service.result` 的 params：`result` 与 `error` 二选一（同时给出按错误处理）。
#[serde(rename_all = "camelCase")]
pub struct ResultParams {
    pub call_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
```

```rust
// atelyx-plugin-protocol/src/service.rs
/// `host:service.changed` 的 params：活动提供者变化（含提供者下线）。
#[serde(rename_all = "camelCase")]
pub struct ChangedParams {
    pub service: String,
    /// 新的活动提供者：`None` = 内置实现。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// 该服务当前是否可用（提供者全不可用且无内置实现时 false）。
    pub available: bool,
}
```

```rust
// atelyx-plugin-protocol/src/service.rs
/// 调用失败的可读原因（作为 JSON-RPC 错误的 data 回给调用方）。
pub mod error {
    /// 服务无人提供（既无内置实现，也没有活动的外部提供者）。
    pub const UNKNOWN_SERVICE: &str = "unknown_service";
    /// 活动提供者没有声明该方法。
    pub const UNKNOWN_METHOD: &str = "unknown_method";
    /// 提供者在调用途中下线 / 停用 / 崩溃。
    pub const PROVIDER_UNAVAILABLE: &str = "provider_unavailable";
    /// 提供者超时未回报结果。
    pub const TIMEOUT: &str = "timeout";
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 内置能力目录：服务名 → 归属插件 → 它提供的方法（全名，形如 `vault.readText`）。
///
/// 这是「能力只有一种描述方式」的落地：方法名本身就是 `服务.方法`，目录按第一个 `.` 切分即得；
/// `host:` 方法的宿主→插件通知不在其列（那不是插件可调用的能力）。条目引用常量而非字面量，
/// 改名 / 删除常量会直接编译失败，目录不会悄悄漂移。服务随归属插件启停。
pub const BUILTIN_SERVICES: &[(&str, &str, &[&str])] = &[
    ("fs", PLATFORM_PLUGIN, &[fs::PRIVATE_DIR, fs::READ_TEXT, fs::WRITE_TEXT, fs::READ_BINARY, fs::WRITE_BINARY, fs::REMOVE, fs::EXISTS, fs::LIST, fs::DOWNLOADS_DIR]),
    ("process", PLATFORM_PLUGIN, &[process::SPAWN, process::WRITE_STDIN, process::CLOSE_STDIN, process::KILL]),
    ("vault", PLATFORM_PLUGIN, &[vault::READ_TEXT, vault::WRITE_TEXT, vault::READ_BINARY, vault::WRITE_BINARY, vault::IMPORT_FILE, vault::IMAGE_INFO, vault::LIST, vault::EXISTS, vault::TAGS, vault::REMOVE, vault::RENAME, vault::BACKLINKS, vault::FILES,
        // 仓库界面状态（展开集 / 排序 / 目录颜色 / 提示词标记）与条目操作，随仓库走
        vault_ui::READ, vault_ui::WRITE, vault_ui::CREATE, vault_ui::SWITCH_CONTAINER, vault_ui::REMOVE_CONTAINER, vault_ui::LIST_DIRS, vault_ui::REFRESH_TREE]),
    ("drag", PLATFORM_PLUGIN, &[drag::START, drag::HOVER, drag::END]),
    ("menu", PLATFORM_PLUGIN, &[menu::OPEN]),
    ("shell", PLATFORM_PLUGIN, &[
        shell::PICK_COLOR,
        shell::PICK_VAULT,
        shell::REVEAL,
        shell::ABSOLUTE_PATH,
        shell::DUPLICATE,
        shell::OPEN_URL,
    ]),
    ("collab", PLATFORM_PLUGIN, &[collab::PEERS, collab::SEND, collab::META_GET, collab::META_PATCH]),
    // 空间名册与房间事实取自宿主会话与服务端，弹层 / 菜单也在宿主，归平台自身
    ("space", PLATFORM_PLUGIN, &[space::ROOM, space::MEMBERS, space::OPEN_MENU, space::OPEN_SETTINGS, space::FILE_PATCH]),
    // 面板头部状态与 flush 回执是宿主界面原语，随平台常驻
    ("panel", PLATFORM_PLUGIN, &[panel::HEADER, panel::FLUSH_DONE]),
    ("state", PLATFORM_PLUGIN, &[state::READ, state::WRITE]),
    ("notification", PLATFORM_PLUGIN, &[notification::SHOW]),
    ("tray", PLATFORM_PLUGIN, &[tray::SET_ITEMS]),
    ("shortcut", PLATFORM_PLUGIN, &[shortcut::REGISTER, shortcut::UNREGISTER, shortcut::QUERY]),
    ("clipboard", PLATFORM_PLUGIN, &[clipboard::WRITE_TEXT, clipboard::READ_TEXT, clipboard::READ_IMAGE, clipboard::WRITE_IMAGE]),
    ("dialog", PLATFORM_PLUGIN, &[dialog::PICK_FILE, dialog::PICK_DIRECTORY]),
    ("http", PLATFORM_PLUGIN, &[http::REQUEST, http::STREAM_OPEN, http::STREAM_CANCEL]),
    // 会话真源是平台数据：归属平台插件，外部提供者不得接管
    ("chat", PLATFORM_PLUGIN, &[chat::SESSION_READ, chat::SESSION_WRITE, chat::SESSION_SNAPSHOT]),
    // 工具名册与结果回报是宿主记账，不是可由外部提供者接管的能力，故不在目录里；
    // 对话门面由宿主编排（核心插件只是被调用的实现），服务归平台自身
    ("ai", PLATFORM_PLUGIN, &[ai::CHAT_SEND, ai::CHAT_SEND_DIRECT, ai::CHAT_STOP, ai::CHAT_HISTORY, ai::MODELS, ai::AGENTS, ai::CHAT_RENAME, ai::CHAT_DELETE, ai::CHAT_COMPACT, ai::CHAT_COMPACT_DIRECT, ai::CHAT_TITLE, ai::TOOL_LIST, ai::TOOL_EXECUTE, ai::CHAT_EMIT, ai::SETTINGS, ai::CHAT_OPEN_PANEL]),
    ("window", PLATFORM_PLUGIN, &[window::OPEN, window::CLOSE, window::UPDATE, window::SHOW, window::BEGIN_DRAG, window::MINIMIZE, window::TOGGLE_MAXIMIZE]),
    ("query", PLATFORM_PLUGIN, &[query::MEASURE, query::THEME, query::FONTS]),
    // 编辑器打开集合与偏好是宿主界面事实，随平台常驻
    ("editor", PLATFORM_PLUGIN, &[editor::OPEN_FILES, editor::OPEN_FILE, editor::PREFS]),
    // 文档会话表（身份 / 脏 / 磁盘基线 / 落盘）归平台自身，不随插件停用下线
    (
        "doc",
        PLATFORM_PLUGIN,
        &[
            doc::OPEN,
            doc::STATE,
            doc::SAVE,
            doc::CLOSE,
            doc::RENAME,
            // 属性读写由宿主按会话转发给内容所有者，方法名挂在 doc 下
            doc::PROPS_READ,
            doc::PROPS_WRITE,
        ],
    ),
    // 历史读数取自宿主侧文件与服务端聚合，历史弹层也在宿主，与 doc 同理归平台自身
    ("history", PLATFORM_PLUGIN, &[history::ACTIVITY, history::DAY, history::GROUPS, history::OPEN, history::RECORD]),
    // 最近打开清单是宿主按容器身份记账的界面数据，记录与剔除都由宿主维护
    ("recent", PLATFORM_PLUGIN, &[recent::LIST]),
];
```

## 4. 绘制指令

插件把界面画成指令：一条指令一个形状或一段文字，坐标是面本地逻辑像素、左上原点。指令集封闭，`fill` 与 `stroke` 至少声明其一；命中由命中区（见下一节）表达，绘制指令本身不带交互语义。

```rust
// atelyx-plugin-protocol/src/paint.rs
/// 绘图指令。矩形 / 路径 / 圆：`fill` 与 `stroke` 至少声明其一，`stroke_width` 只在 `stroke`
/// 存在时生效（缺省 1.0）；线只有 `stroke`（字段名 `width`）；文字与图片没有描边概念。
/// 裁剪：`PushClip` 之后的指令按该矩形与已压栈矩形的交集绘制，`PopClip` 弹出最近一层。
#[serde(tag = "kind", rename_all = "camelCase")]
#[serde(rename_all_fields = "camelCase")]
pub enum DrawCommand {
    Line {
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        stroke: PaintColor,
        #[serde(default = "default_stroke_width")]
        width: f32,
        /// 虚线：`[实线长, 空白长]`（像素，沿线的行程计）。缺省 = 实线。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dash: Option<[f32; 2]>,
    },
    Rect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        fill: Option<PaintColor>,
        stroke: Option<PaintColor>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stroke_width: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        corner_radius: Option<f32>,
        /// 投影：只对矩形生效，绘制在填充与描边之下。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shadow: Option<PaintShadow>,
    },
    Path {
        points: Vec<PaintPoint>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        closed: bool,
        fill: Option<PaintColor>,
        stroke: Option<PaintColor>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stroke_width: Option<f32>,
    },
    Circle {
        cx: f32,
        cy: f32,
        r: f32,
        fill: Option<PaintColor>,
        stroke: Option<PaintColor>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stroke_width: Option<f32>,
    },
    /// 文字以 (x, y) 为左上角。给 `maxWidth` 时按该宽度换行并作为对齐区域（`x` 为区域左缘）；
    /// `align` 只在给了 `maxWidth` 时成立，缺省 `start`。
    Text {
        x: f32,
        y: f32,
        content: String,
        size: f32,
        color: PaintColor,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        weight: Option<TextWeight>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        family: Option<TextFamily>,
        /// 斜体：字族没有斜体字形时按合成倾斜绘制（强调正文里的 `*斜体*` 用它）。
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        italic: bool,
        /// 显式字体族名（用户在单元格样式里选的系统字体）：给出时优先于 `family`，
        /// 该族不存在则回落 `family` 口径。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        family_name: Option<String>,
        /// 可用宽度：给定即按该宽度换行，并作为 `align` 的对齐区域。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_width: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        align: Option<TextAlign>,
        /// 排版口径（行高倍数 / 换行 / 整形）：与 `query.measure` 的同一项同口径，
        /// 量出来的行高与折行点才对得上绘制结果。缺省 = 宿主默认。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        layout: Option<TextLayout>,
    },
    /// 图片贴片：绘制在给定矩形内。
    Image {
        source: ImageSource,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fit: Option<ImageFit>,
    },
    /// 压入裁剪矩形：之后的指令只画在该矩形内，嵌套时取交集。
    PushClip { x: f32, y: f32, w: f32, h: f32 },
    /// 弹出最近一次 `PushClip`；与 `PushClip` 必须成对出现。
    PopClip,
}
```

```rust
// atelyx-plugin-protocol/src/paint.rs
/// 颜色：主题 token（跟随宿主主题）或插件自带 RGBA（内容谱系色，如连线分类、播放头高亮）。
#[serde(tag = "type", rename_all = "camelCase")]
#[serde(rename_all_fields = "camelCase")]
pub enum PaintColor {
    Token {
        name: ColorToken,
    },
    Rgba {
        /// `[r, g, b, a]`，各 0~255。
        rgba: [u8; 4],
    },
}
```

```rust
// atelyx-plugin-protocol/src/paint.rs
/// 矩形投影：偏移、模糊半径与颜色。模糊按宿主渲染层的 quad 阴影实现，只对矩形生效。
#[serde(rename_all = "camelCase")]
pub struct PaintShadow {
    #[serde(default)]
    pub offset_x: f32,
    #[serde(default)]
    pub offset_y: f32,
    #[serde(default)]
    pub blur: f32,
    pub color: PaintColor,
}
```

```rust
// atelyx-plugin-protocol/src/paint.rs
/// 二维坐标点（逻辑像素）。
pub struct PaintPoint {
    pub x: f32,
    pub y: f32,
}
```

```rust
// atelyx-plugin-protocol/src/paint.rs
/// 文字水平对齐：以 `maxWidth` 为对齐区域、`x` 为区域左缘，按量算出的整段宽度换算
/// （换行后段内各行仍左对齐）。
#[serde(rename_all = "camelCase")]
pub enum TextAlign {
    Start,
    Center,
    End,
}
```

```rust
// atelyx-plugin-protocol/src/paint.rs
/// 字体族：界面字体（跟随用户设置）或系统等宽族。
#[serde(rename_all = "camelCase")]
pub enum TextFamily {
    Ui,
    Mono,
}
```

```rust
// atelyx-plugin-protocol/src/paint.rs
/// 字重四档（正文 400 / 中等 500 / 半粗 600 / 粗 700）。
#[serde(rename_all = "camelCase")]
pub enum TextWeight {
    Normal,
    Medium,
    SemiBold,
    Bold,
}
```

```rust
// atelyx-plugin-protocol/src/paint.rs
/// 主题 token：取色引用宿主调色板，主题变化时自动跟随。
/// `accentSoft` / `warningSoft` / `dangerSoft` / `successSoft` 为对应语义色的低饱和底，
/// `surface*` / `hover` / `border*` 为层次底色与边界。
#[serde(rename_all = "camelCase")]
pub enum ColorToken {
    Text,
    TextSecondary,
    TextMuted,
    Accent,
    AccentSoft,
    Danger,
    DangerSoft,
    Success,
    SuccessSoft,
    Warning,
    WarningSoft,
    /// 内嵌表面（比面板底色深一档：输入框、内嵌卡片）。
    Surface,
    /// 次级表面（面板头 / 工具条底）。
    SurfaceMuted,
    /// 最深表面（内陷区域）。
    SurfaceSunken,
    /// 悬停反馈底色。
    Hover,
    Border,
    BorderStrong,
    /// 画布 / 内容区的底色（比次级表面深一档）。
    Background,
    /// 最弱的分隔线（格线、列表内分隔）。
    BorderSubtle,
    /// 文本选区底。
    Selection,
    /// 模态遮罩（灯箱 / 确认层）。
    Scrim,
    /// 无底色 / 无边框。
    Transparent,
    /// 浮层表面（菜单 / 提示条底，比面板底亮一档）。
    SurfaceOverlay,
}
```

```rust
// atelyx-plugin-protocol/src/paint.rs
/// 图片填充方式：`contain` 等比完整可见（留空边），`cover` 等比覆盖并裁剪溢出，缺省 = 拉伸填满。
#[serde(rename_all = "camelCase")]
pub enum ImageFit {
    Contain,
    Cover,
    Fill,
}
```

```rust
// atelyx-plugin-protocol/src/paint.rs
/// 图片来源：`private` = 插件私有目录内相对路径；`vault` = 仓库根相对路径（宿主按仓库内容读取，
/// 协作空间走同构内容面）；`url` = http(s) 地址（缺省按公网策略下载并缓存，`local` 改为本机 /
/// 局域网策略）；`file` = 本机绝对路径（插件自己读得到的文件，交给宿主显示）。
#[serde(tag = "type", rename_all = "camelCase")]
#[serde(rename_all_fields = "camelCase")]
pub enum ImageSource {
    Private { path: String },
    Vault {
        path: String,
        /// 缩略图长边上限（像素）：给定时宿主按 EXIF 方向摆正并等比缩到该长边以内再上传，
        /// 同一路径的不同上限各占一份缓存。缺省 = 原图。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_edge: Option<u32>,
    },
    Url {
        url: String,
        /// 按本机 / 局域网策略取图：放行回环与私网、仅拒链路本地与广播。
        /// 缺省 = 公网策略（拒回环与私网）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        local: Option<bool>,
    },
    /// 本机绝对路径。插件要读它的内容本来就能经 `fs` 自己读，这里不增加也不收紧范围。
    File { path: String },
}
```

```rust
// atelyx-plugin-protocol/src/paint.rs
/// 文字排版口径：量算、换行与绘制共用同一份，缺省项取宿主默认。
#[serde(rename_all = "camelCase")]
pub struct TextLayout {
    /// 相对字号的行高倍数（缺省 = 宿主默认行高）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_height: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrap: Option<TextWrap>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shaping: Option<TextShaping>,
}
```

```rust
// atelyx-plugin-protocol/src/paint.rs
/// 换行口径：`word` 只在词间断行（超长词溢出），`wordOrGlyph` 超长词退化为逐字形断行。
#[serde(rename_all = "camelCase")]
pub enum TextWrap {
    Word,
    WordOrGlyph,
}
```

```rust
// atelyx-plugin-protocol/src/paint.rs
/// 字形整形策略：`auto` 纯 ASCII 走基础整形、含非 ASCII 走高级整形（含字体回退）。
#[serde(rename_all = "camelCase")]
pub enum TextShaping {
    Auto,
    Basic,
    Advanced,
}
```

```rust
// atelyx-plugin-protocol/src/paint.rs
/// 帧率上限（帧声明的 `fps` 上限，也是宿主调度帧通知的最细节拍）。
pub const MAX_FPS: u16 = 120;
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 纯查询：不改界面、不产生帧。
pub mod query {
    /// 批量量算文字占位尺寸（排版按真实字宽摆放，不能按字符数估）。
    pub const MEASURE: &str = "query.measure";
    /// 系统已安装字体族清单（排序后的族名；单元格样式里的字体选择用）。
    pub const FONTS: &str = "query.fonts";
    /// 取宿主主题表（颜色 / 字号档 / 圆角档），插件据此与宿主观感一致。
    pub const THEME: &str = "query.theme";
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 单次批量量算的条目上限。
pub const MAX_MEASURE_ITEMS: usize = 512;
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`query::MEASURE`] 的一条：字段与 [`MeasureBatchParams`] 的条目同口径。
#[serde(rename_all = "camelCase")]
pub struct MeasureItem {
    pub text: String,
    pub size: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<crate::paint::TextWeight>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<crate::paint::TextFamily>,
    /// 显式字体族名（与绘制指令同口径；缺省 = 按 `family` 取）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_width: Option<f32>,
    /// 排版口径（行高倍数 / 换行 / 整形）：与绘制指令的 `layout` 同口径，量算与绘制才对得上。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<crate::paint::TextLayout>,
    /// 要逐行行段（结果带 `segments`）：逐行排版 / 逐行绘制的调用方用。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub segments: bool,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`query::MEASURE`] 的 params：一次量算多段文字（排一屏文本的量算往返要落在单批里）。
#[serde(rename_all = "camelCase")]
pub struct MeasureBatchParams {
    pub items: Vec<MeasureItem>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`query::MEASURE`] 的 result：顺序与 params 的 `items` 一一对应。
#[serde(rename_all = "camelCase")]
pub struct MeasureBatchResult {
    pub sizes: Vec<MeasureTextResult>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`query::MEASURE`] 的 result：占位宽高（逻辑像素）与换行后的行数（单行量算恒为 1）。
/// `segments` 在条目声明 `segments` 时给出逐行文本与宽度，供调用方自行逐行排版与绘制。
#[serde(rename_all = "camelCase")]
pub struct MeasureTextResult {
    pub width: f32,
    pub height: f32,
    pub lines: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub segments: Option<Vec<MeasureSegment>>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 量算结果的单行：`text` 是该行的源文本切片，`width` 是实测行宽。
#[serde(rename_all = "camelCase")]
pub struct MeasureSegment {
    pub text: String,
    pub width: f32,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`query::FONTS`] 的 result：系统已安装字体族（去重排序；宿主启动时扫描一次）。
#[serde(rename_all = "camelCase")]
pub struct FontFamiliesResult {
    pub families: Vec<String>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`fs::DOWNLOADS_DIR`] 的 result：系统下载目录的绝对路径。
#[serde(rename_all = "camelCase")]
pub struct DownloadsDirResult {
    pub path: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`query::THEME`] 的 result：颜色 / 字号档 / 圆角档 / 事件色板
/// （值随用户的主题与字号设置变化）。
#[serde(rename_all = "camelCase")]
pub struct ThemeResult {
    pub colors: ThemeColors,
    pub font_sizes: FontSizes,
    pub radii: Radii,
    /// 事件条目可选色板（十六进制，宿主界面同序）；插件按它给用户选色。
    #[serde(default)]
    pub event_colors: Vec<String>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 主题色：十六进制 `#rrggbb` / `#rrggbbaa`，字段名与 [`crate::paint::ColorToken`] 的 wire 名一致。
#[serde(rename_all = "camelCase")]
pub struct ThemeColors {
    pub text: String,
    pub text_secondary: String,
    pub text_muted: String,
    pub accent: String,
    pub accent_soft: String,
    pub danger: String,
    pub danger_soft: String,
    pub success: String,
    pub success_soft: String,
    pub warning: String,
    pub warning_soft: String,
    pub surface: String,
    pub surface_muted: String,
    pub surface_sunken: String,
    pub hover: String,
    pub border: String,
    pub border_strong: String,
    pub background: String,
    pub border_subtle: String,
    pub selection: String,
    pub scrim: String,
    pub transparent: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 字号档（绝对逻辑像素）：`micro` / `caption` / `ui` / `body` / `h2` / `h1`。
#[serde(rename_all = "camelCase")]
pub struct FontSizes {
    pub micro: f32,
    pub caption: f32,
    pub ui: f32,
    pub body: f32,
    pub h2: f32,
    pub h1: f32,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 圆角档（逻辑像素）。
#[serde(rename_all = "camelCase")]
pub struct Radii {
    pub xs: f32,
    pub sm: f32,
    pub md: f32,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 内边距声明：数字 = 四边等距；`{horizontal, vertical}` = 分轴；`{top, right, bottom, left}` = 分边。
/// 两个对象形态都不接受未声明字段，避免分边写法被分轴形态吞掉。
#[serde(untagged)]
pub enum PaddingSpec {
    All(f32),
    Axes(AxesPadding),
    Edges(EdgesPadding),
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AxesPadding {
    #[serde(default)]
    pub horizontal: f32,
    #[serde(default)]
    pub vertical: f32,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EdgesPadding {
    #[serde(default)]
    pub top: f32,
    #[serde(default)]
    pub right: f32,
    #[serde(default)]
    pub bottom: f32,
    #[serde(default)]
    pub left: f32,
}
```

## 5. 整面帧与输入事件

插件经 ABI `submit_frame` 提交一帧即该面的完整界面（指令 + 命中区 + 输入声明），SDK 编帧借用帧内字符串，宿主在回调内同步复制成协议帧；宿主只按几何绘制并把原始输入经 `host:frame.input` 原样回发，插件按帧自行维护悬停 / 按下 / 焦点 / 滚动 / 文本编辑状态。

```rust
// atelyx-plugin-protocol/src/method.rs
/// 面帧：整个面交给插件自绘，一帧即当前完整界面。
pub mod frame {
    /// 宿主回发原始输入（通知）：指针 / 滚轮 / 按键 / 输入法 / 焦点 / 尺寸。
    pub const HOST_INPUT: &str = "host:frame.input";
    /// 宿主按面声明的帧率发帧拍（通知）：插件收到后提交该帧的内容。
    pub const HOST_TICK: &str = "host:frame.tick";
}
```

```rust
// atelyx-plugin-protocol/src/frame.rs
/// 一面一帧的完整内容。
#[serde(rename_all = "camelCase")]
pub struct SurfaceFrame {
    /// 本帧按多大尺寸排版。宿主据此判定这张帧是否对得上当前面尺寸——改宽期间手里那帧常常
    /// 还是上一拍宽度排的，直接画出来右对齐项与滚动条就会停在旧边缘。
    /// 缺省 = 未声明，宿主无从判定，只能按「拿到什么画什么」处理。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<FrameSize>,
    pub commands: Vec<DrawCommand>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hits: Vec<HitRegion>,
    /// 声明后宿主承接输入法：组合串与提交文本经 [`FrameInput::Text`] 回发，光标与候选窗按此矩形定位。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ime: Option<FrameRect>,
    /// 声明后宿主把键盘交给本面：指针按下取焦，随后全部按键（含修饰键组合）经 [`FrameInput::Key`] 回发。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub keys: bool,
    /// 帧率（1–120）：声明后宿主按该节拍回发 `host:frame.tick`；缺省不推帧。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fps: Option<u16>,
    /// 浮层（下拉 / 菜单 / 气泡 / 灯箱）：宿主画在本面之上并按窗口裁剪，可越出面矩形。
    /// 本帧不带 = 收起。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub popup: Option<SurfacePopup>,
}
```

```rust
// atelyx-plugin-protocol/src/frame.rs
/// 面的浮层：按宿主窗口裁剪，可越出面矩形。
/// `rect` / `commands` / `hits` / `ime` 全部用**面本地坐标**（与主帧同一坐标系）。
/// 与主帧同寿命：本帧带浮层即展开，下一帧不带即收起。
#[serde(rename_all = "camelCase")]
pub struct SurfacePopup {
    /// 相对面左上角的位置与尺寸：可为负（例如盖住面板顶部的头部行），也可大于面尺寸。
    pub rect: FrameRect,
    #[serde(default)]
    pub capture: PopupCapture,
    /// 叠放层级：缺省 = 面板内。窗口级浮层不随面裁剪，宿主在所有面板之后绘制。
    #[serde(default, skip_serializing_if = "is_panel_layer")]
    pub layer: PopupLayer,
    /// 窗口级浮层的叠放序：越大越靠上；同值按面 id 稳定序。面板内层不参与排序。
    #[serde(default, skip_serializing_if = "is_zero_order")]
    pub order: u16,
    pub commands: Vec<DrawCommand>,
    /// 浮层自己的命中区：浮层矩形内的按下先在这里命中（坐标同面本地）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hits: Vec<HitRegion>,
    /// 声明后输入法候选窗按此矩形（面本地坐标）定位；缺省用主帧 `ime`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ime: Option<FrameRect>,
}
```

```rust
// atelyx-plugin-protocol/src/frame.rs
/// 浮层的叠放层级。
#[serde(rename_all = "camelCase")]
pub enum PopupLayer {
    /// 面板内：随本面图层绘制，先于相邻面板出图，可能被后绘制的面板盖住。
    #[default]
    Panel,
    /// 窗口级：画在所有面板之后，可盖住任何面板的头部与相邻面板。
    Window,
}
```

```rust
// atelyx-plugin-protocol/src/frame.rs
/// 浮层承接点击的范围：`inside` = 只有落在浮层矩形内的按下交给浮层（外部按下照常送本面），
/// `all` = 浮层展开期间由宿主处理其覆盖区外的按下，并向插件回发 [`FrameInput::Outside`]。
#[serde(rename_all = "camelCase")]
pub enum PopupCapture {
    #[default]
    Inside,
    All,
}
```

```rust
// atelyx-plugin-protocol/src/frame.rs
/// 命中区：矩形 + 标识 + 光标形状。同帧内按声明序叠放（后声明的在上）。
/// 宿主把落在其上的指针事件的 `hit` 填成该 id，插件按 id 自行解释语义。
#[serde(rename_all = "camelCase")]
pub struct HitRegion {
    /// 插件自定义的命中标识，同帧内唯一。
    pub id: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// 缺省 = 系统默认箭头。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<HitCursor>,
}
```

```rust
// atelyx-plugin-protocol/src/frame.rs
/// 命中区光标形状（悬停时的鼠标指针）。
#[serde(rename_all = "camelCase")]
pub enum HitCursor {
    #[default]
    Default,
    Pointer,
    Text,
    Grab,
    Grabbing,
    Crosshair,
    NotAllowed,
    ResizeHorizontal,
    ResizeVertical,
}
```

```rust
// atelyx-plugin-protocol/src/frame.rs
/// 面本地矩形（输入法光标 / 候选框锚点）。
#[serde(rename_all = "camelCase")]
pub struct FrameRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}
```

```rust
// atelyx-plugin-protocol/src/frame.rs
/// 宿主回发的原始输入。坐标 = 面本地逻辑像素；`hit` 为命中的命中区 id（未命中缺省）。
/// 插件按帧自行维护交互状态（悬停 / 按下 / 焦点 / 滚动位置 / 文本编辑）。
#[serde(tag = "kind", rename_all = "camelCase")]
#[serde(rename_all_fields = "camelCase")]
pub enum FrameInput {
    PointerDown {
        x: f32,
        y: f32,
        button: MouseButton,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hit: Option<String>,
    },
    PointerUp {
        x: f32,
        y: f32,
        button: MouseButton,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hit: Option<String>,
    },
    PointerMove {
        x: f32,
        y: f32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hit: Option<String>,
    },
    /// 指针离开本面（从内到外边沿触发一次）。
    PointerLeave,
    /// 按在浮层外：宿主已按浮层声明的承接范围处理这次按下（`all` 时不再送往本面），
    /// 插件据此丢弃浮层状态。`x` / `y` 为窗口坐标（仅供描述位置）。
    Outside {
        x: f32,
        y: f32,
    },
    /// 同键二次按下（间隔与位移都在宿主阈值内），插件无需自己算。
    DoubleClick {
        x: f32,
        y: f32,
        button: MouseButton,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hit: Option<String>,
    },
    /// 滚轮增量统一为像素（宿主已把滚轮「行」折算成像素），正 = 向下 / 向右（Web 惯例，宿主已完成方向换算）。
    Wheel {
        x: f32,
        y: f32,
        delta_x: f32,
        delta_y: f32,
        /// 滚轮发生时的修饰键快照（Ctrl+滚轮缩放等）。
        #[serde(default, skip_serializing_if = "Modifiers::is_empty")]
        modifiers: Modifiers,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hit: Option<String>,
    },
    /// 键盘：`text` 为该键在输入法提交后可插入的文本（字符键才有，宿主已滤掉控制字符）。
    Key {
        key: Key,
        action: KeyAction,
        modifiers: Modifiers,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
    /// 输入法：`composing` = 组合串更新（空串 = 组合结束，不插入文本），
    /// 否则 `text` 为已提交的文本；`caret` 为组合期的光标字节位；`candidates` 为候选窗锚点矩形。
    Text {
        text: String,
        composing: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caret: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        candidates: Option<FrameRect>,
    },
    /// 焦点进入 / 离开本面（按下即取焦，按在面外即失焦）。
    Focus { focused: bool },
    /// 面尺寸变化（逻辑像素）：插件据此重排并重绘。
    Resize { width: f32, height: f32 },
}
```

```rust
// atelyx-plugin-protocol/src/frame.rs
/// 命中区数量上限（单帧）。
pub const MAX_HITS: usize = 4096;
```

```rust
// atelyx-plugin-protocol/src/frame.rs
/// 命中区 id 上限（字节）：插件可以把它当作不透明标识用（仓库相对路径这类天然偏长的串），
/// 上限按 Linux 的 `PATH_MAX` 留足，中文一字 3 字节也不至于撑破。
pub const MAX_HIT_ID_LEN: usize = 4096;
```

```rust
// atelyx-plugin-protocol/src/frame.rs
/// `host:frame.input` 的 params。
#[serde(rename_all = "camelCase")]
pub struct FrameInputParams {
    pub surface_id: String,
    pub input: FrameInput,
}
```

```rust
// atelyx-plugin-protocol/src/frame.rs
/// `host:frame.tick` 的 params：帧序号与距上一帧的毫秒数（首帧为 0）。
#[serde(rename_all = "camelCase")]
pub struct FrameTickParams {
    pub surface_id: String,
    pub frame: u64,
    pub delta_ms: u32,
}
```

```rust
// atelyx-plugin-protocol/src/frame.rs
/// 鼠标按键。
#[serde(rename_all = "camelCase")]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}
```

```rust
// atelyx-plugin-protocol/src/frame.rs
/// 按键：具名键（功能键与编辑键）或可打印字符。字符按布局产出（与用户所见一致），
/// 大小写与符号跟随 Shift，因此 `a` 与 `A` 是两个值、`1` 与 `!` 也是两个值。
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Key {
    Named { name: NamedKey },
    Character { text: String },
}
```

```rust
// atelyx-plugin-protocol/src/frame.rs
/// 具名键。可打印字符走 [`Key::Character`]；这里放不可打印的编辑 / 功能键，外加空格
/// （它与编辑语义绑定，且布局差异下字符形态不稳定）。
#[serde(rename_all = "camelCase")]
pub enum NamedKey {
    Escape,
    Enter,
    Tab,
    Space,
    Backspace,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
}
```

```rust
// atelyx-plugin-protocol/src/frame.rs
/// 键盘焦点事件与按键的归属：`Press` 含系统重复按键（长按方向键逐次到达），`Release` 在抬起时到达。
#[serde(rename_all = "camelCase")]
pub enum KeyAction {
    Press,
    Release,
}
```

```rust
// atelyx-plugin-protocol/src/frame.rs
/// 修饰键状态（按键发生时的快照）：`control` = Ctrl（macOS 的 Control，不含 Command），
/// `meta` = Windows / Super / Command 键。
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Modifiers {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub shift: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub control: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub alt: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub meta: bool,
}
```

## 6. 贡献点与窗口面

贡献点决定插件界面出现在哪里；`surfaceId` 即贡献 id（文档实例另由宿主分配 id，插件窗口面由 `window.open` 的 `id` 指定）。

```rust
// atelyx-plugin-protocol/src/method.rs
/// 贡献点位置（封闭枚举；扩容随协议升版）。
#[serde(rename_all = "camelCase")]
pub enum ContributionSlot {
    /// 工作区面板（「添加面板 / 切换视图」菜单条目）。
    Panel,
    /// 表格编辑器工具条「视图」下拉条目。
    TableView,
    /// 设置页顶层 tab（注册即挂载：宿主立即回 `host:surface.attached`，插件常驻设置面）。
    Settings,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
#[serde(rename_all = "camelCase")]
pub enum SurfaceKind {
    Panel,
    Window,
    Page,
    Takeover,
    Settings,
    /// 表格编辑器内的插件表格视图（挂载在表格面板内，替掉网格）。
    TableView,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
#[serde(rename_all = "camelCase")]
pub struct SurfaceAttachedParams {
    pub surface_id: String,
    /// 面的类型：面板 / 插件窗口 / 整页接管 / 全接管 / 设置页。
    pub kind: SurfaceKind,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
#[serde(rename_all = "camelCase")]
pub struct SurfaceDetachedParams {
    pub surface_id: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 整面帧提交（`submit_frame`）的回执：`applied` = 宿主已接受并生效。
#[serde(rename_all = "camelCase")]
pub struct SubmitResult {
    pub applied: bool,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// `view.register` 的 params：声明一个贡献点。`id` 插件内唯一且不得含 `:`（宿主的面标识以它作分隔），
/// 同时是该面挂载后的 `surfaceId`；`icon` 为宿主内置形状名或插件自备形状名
/// （内置名优先），缺省 / 未注册时宿主用拼图图标。
#[serde(rename_all = "camelCase")]
pub struct RegisterParams {
    pub slot: ContributionSlot,
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// 面外壳内边距：数字 = 四边等距，也可按轴 / 按边声明（见 [`PaddingSpec`]）；
    /// 缺省 / 0 = 贴边。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub padding: Option<PaddingSpec>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// `view.unregister` 的 params：按 id 撤销自己的贡献；id 未注册时报可读错误。
#[serde(rename_all = "camelCase")]
pub struct UnregisterParams {
    pub id: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 接管范围（封闭枚举）。`page` = 内容区全给插件（宿主标题栏保留）；`full` = 整窗归插件呈现
/// （含标题栏区域，窗口拖动 / 缩放 / 关闭由宿主代执行）。
#[serde(rename_all = "camelCase")]
pub enum TakeoverScope {
    Page,
    Full,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// `view.takeover` 的 params：把本插件的一个贡献面接管到当前聚焦窗口。`surfaceId` 须为
/// 已注册的贡献 id（接管的就是该面自己的整面帧）；退出接管入口由宿主常驻提供，插件无需声明退路。
#[serde(rename_all = "camelCase")]
pub struct TakeoverParams {
    pub scope: TakeoverScope,
    pub surface_id: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// `window.open` 的 params：`id` 插件内唯一、非空且不含 `:`，同时是该窗口面的 `surfaceId`。
#[serde(rename_all = "camelCase")]
pub struct WindowOpenParams {
    pub id: String,
    pub title: String,
    /// 逻辑像素；缺省由宿主取默认尺寸。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<f32>,
    /// 置顶浮窗。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub always_on_top: Option<bool>,
    /// 不进任务栏。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_taskbar: Option<bool>,
    /// 失焦即收起（隐藏窗口，从托盘 / 快捷键等入口再唤起是插件自己的事）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hide_on_blur: Option<bool>,
    /// 系统窗口边框；缺省 true。`false` 时由宿主提供标题栏与窗口控制（拖动 / 最小化 /
    /// 最大化 / 关闭），插件内容占满剩余区域。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decorations: Option<bool>,
    /// 仅 `decorations: false` 时生效：缺省 true = 宿主画标题栏；false = 不画，插件自绘标题栏
    /// （拖动走 [`window::BEGIN_DRAG`]，窗口控制走 [`window::MINIMIZE`] / [`window::TOGGLE_MAXIMIZE`]
    /// / [`window::CLOSE`]）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub titlebar: Option<bool>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// `window.close` 的 params。
#[serde(rename_all = "camelCase")]
pub struct WindowCloseParams {
    pub id: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// `window.update` 的 params：运行时改一个已打开窗口的属性（缺省字段不动）。至少给一项。
/// 「不进任务栏」是开窗期设置，运行时不支持改。
#[serde(rename_all = "camelCase")]
pub struct WindowUpdateParams {
    /// 插件声明的窗口 id（`window.open` 的 `id`）。
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub always_on_top: Option<bool>,
    /// 改窗口标题：宿主自绘标题栏上的文字立即更新；系统边框窗口的 OS 标题由系统绘制，
    /// 运行期不随之变化。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// `window.show` / `host:window.hidden` 的 params：按插件声明的窗口 id。
#[serde(rename_all = "camelCase")]
pub struct WindowIdParams {
    pub id: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`window::HOST_CLOSED`] 的 params：用户（而非插件）关闭了该窗口。
#[serde(rename_all = "camelCase")]
pub struct WindowClosedParams {
    pub id: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// `window.beginDrag` 的 params：要拖动的窗口按承载该面的窗口解析（插件窗口 / 接管面 / 所在面板）。
#[serde(rename_all = "camelCase")]
pub struct WindowDragParams {
    pub surface_id: String,
}
```

## 7. 默认布局

插件声明自己期望的默认布局：宿主在插件首次启用时摆进当前场景，之后改名 / 删除都不再补回。`views` 填视图 kind（本插件贡献为 `plugin:<插件 id>:<贡献 id>`）。

```rust
// atelyx-plugin-protocol/src/method.rs
/// 当前场景的布局面：插件声明自己期望的默认布局，宿主在首次启用时摆进当前场景。
pub mod layout {
    /// 声明默认布局（每个插件至多生效一次；用户可自行改名 / 删除）。
    pub const DECLARE_DEFAULT: &str = "layout.declareDefaultLayout";
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// `layout.declareDefaultLayout` 的 params：给宿主一套默认面板布局。`name` 为布局名
/// （与既有布局重名时宿主追加序号），`spec` 只描述结构与视图 kind。
#[serde(rename_all = "camelCase")]
pub struct DeclareDefaultLayoutParams {
    pub name: String,
    pub spec: crate::layout::LayoutSpecNode,
}
```

```rust
// atelyx-plugin-protocol/src/layout.rs
/// 插件默认布局规格节点（`layout.declareDefaultLayout` 载荷）。
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum LayoutSpecNode {
    /// 分割节点：方向 + 子树 + 占比。
    Split {
        direction: Axis,
        children: Vec<LayoutSpecNode>,
        sizes: Vec<f64>,
    },
    /// 面板节点：按顺序放置的视图 kind（首个为激活标签）。
    Panel { views: Vec<String> },
}
```

```rust
// atelyx-plugin-protocol/src/layout.rs
/// 分割方向。
#[serde(rename_all = "camelCase")]
pub enum Axis {
    Horizontal,
    Vertical,
}
```

```rust
// atelyx-plugin-protocol/src/layout.rs
/// 默认布局名的字节上限（多字节字符按字节计）。
pub const MAX_NAME_BYTES: usize = 64;
```

```rust
// atelyx-plugin-protocol/src/layout.rs
/// 规格树的深度上限。
pub const MAX_DEPTH: usize = 16;
```

```rust
// atelyx-plugin-protocol/src/layout.rs
/// 规格树的面板数上限。
pub const MAX_PANELS: usize = 32;
```

```rust
// atelyx-plugin-protocol/src/layout.rs
/// 单个面板的视图数上限。
pub const MAX_VIEWS_PER_PANEL: usize = 16;
```

## 8. 能力面方法名

每个模块是一类能力面的方法名常量，与 `permissions` 里的能力声明一一对应；声明用于公示与审计，不是拦截：插件是原生进程，能力面上的限制只作便利层语义。

```rust
// atelyx-plugin-protocol/src/method.rs
pub mod view {
    /// 插件注册一个贡献点（面板 / 表格视图 / 设置页），使其出现在宿主对应的入口里。
    pub const REGISTER: &str = "view.register";
    /// 插件按 id 撤销自己的一个贡献点。
    pub const UNREGISTER: &str = "view.unregister";
    /// 插件请求接管当前聚焦窗口（整页 / 全接管）。宿主按窗应用，退出入口由宿主常驻提供。
    pub const TAKEOVER: &str = "view.takeover";
    /// 宿主告知插件某个面已就绪 / 已移除（通知）。
    pub const HOST_ATTACHED: &str = "host:surface.attached";
    pub const HOST_DETACHED: &str = "host:surface.detached";
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
pub mod window {
    /// 插件请求打开一个独立窗口承载自己的面（声明的 `id` 即该窗口面的 `surfaceId`）。
    /// 缺省用系统窗口边框；`decorations: false` 时去掉系统边框，由宿主提供标题栏
    /// （标题 / 拖动 / 最小化 / 最大化 / 关闭）。
    pub const OPEN: &str = "window.open";
    /// 插件关闭自己打开的一个窗口。
    pub const CLOSE: &str = "window.close";
    /// 插件运行时改一个已打开窗口的属性（尺寸 / 置顶 / 标题；「不进任务栏」只能在开窗时声明）。
    pub const UPDATE: &str = "window.update";
    /// 插件最小化自己打开的一个窗口。
    pub const MINIMIZE: &str = "window.minimize";
    /// 插件切换自己打开窗口的最大化态。
    pub const TOGGLE_MAXIMIZE: &str = "window.toggleMaximize";
    /// 插件重新显示一个因失焦被隐藏的窗口（`hideOnBlur` 的唤起回路）。
    pub const SHOW: &str = "window.show";
    /// 插件请求拖动承载某个面的窗口（自绘标题栏走这里：宿主接管拖动，插件不必自己算窗口坐标）。
    pub const BEGIN_DRAG: &str = "window.beginDrag";
    /// 宿主告知插件某个窗口已被用户关闭（通知）。
    pub const HOST_CLOSED: &str = "host:window.closed";
    /// 宿主告知插件某个窗口因失焦被收起（通知）；插件可用 [`SHOW`] 重新显示。
    pub const HOST_HIDDEN: &str = "host:window.hidden";
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
pub mod fs {
    pub const PRIVATE_DIR: &str = "fs.privateDir";
    pub const READ_TEXT: &str = "fs.readText";
    pub const WRITE_TEXT: &str = "fs.writeText";
    pub const READ_BINARY: &str = "fs.readBinary";
    pub const WRITE_BINARY: &str = "fs.writeBinary";
    pub const REMOVE: &str = "fs.remove";
    pub const EXISTS: &str = "fs.exists";
    pub const LIST: &str = "fs.list";
    /// 系统下载目录的绝对路径（定位失败回落用户主目录）：图片 / 附件下载落盘用。
    pub const DOWNLOADS_DIR: &str = "fs.downloadsDir";
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
pub mod process {
    pub const SPAWN: &str = "process.spawn";
    pub const WRITE_STDIN: &str = "process.writeStdin";
    pub const CLOSE_STDIN: &str = "process.closeStdin";
    pub const KILL: &str = "process.kill";
    /// 托管进程的输出与退出（通知）。
    pub const HOST_EVENT: &str = "host:process.event";
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 当前激活仓库的按路径读写（`path` 为相对仓库根的路径）。协作空间模式下同一能力面
/// 由宿主转发到服务端，插件不感知容器类型。写入与宿主自身写同出口（写盘队列 / 服务端内容面），
/// 插件不得用 `fs` 直改仓库（会与内存态和协作广播互踩）。
pub mod vault {
    pub const READ_TEXT: &str = "vault.readText";
    pub const WRITE_TEXT: &str = "vault.writeText";
    /// 读二进制文件（base64；单次上限见宿主实现，超限可读失败）。
    pub const READ_BINARY: &str = "vault.readBinary";
    /// 写二进制文件（base64；父目录自动创建；正在编辑器中打开的文件拒写；单次上限 32 MiB）。
    pub const WRITE_BINARY: &str = "vault.writeBinary";
    /// 把插件私有目录里的一个文件收进仓库（大文件走这条：字节不进 RPC 载荷）。
    pub const IMPORT_FILE: &str = "vault.importFile";
    /// 读图片的像素宽高（png / jpeg / gif / bmp / webp），不返回文件内容。
    pub const IMAGE_INFO: &str = "vault.imageInfo";
    pub const LIST: &str = "vault.list";
    pub const EXISTS: &str = "vault.exists";
    /// 全仓库标签词表（宿主缓存的快照，按出现次数降序、最多 1000 条）：属性标签值候选这类
    /// 需要「仓库里已有哪些标签」的能力。刚写入的新标签要等宿主下次重建词表才出现。
    pub const TAGS: &str = "vault.tags";
    /// 删除一个仓库相对路径（文件或文件夹；文件夹连同内容递归删除）。仓库根不可删。
    /// 被编辑面打开或被当前画布节点引用的文件拒删。
    pub const REMOVE: &str = "vault.remove";
    /// 把一个仓库相对路径改名 / 移动到另一路径（文件或文件夹；目标不存在，父目录自动创建）。
    /// 引用同步：`.md` 内部链接与画布文件引用随改；源文件被编辑面打开或被当前画布节点引用时拒绝。
    pub const RENAME: &str = "vault.rename";
    /// 引用某篇文档的其它文档（反链；按 wiki 链接索引查询）。
    pub const BACKLINKS: &str = "vault.backlinks";
    /// 当前仓库可检索文件清单（按宿主文件树的树序；本地仓库与协作空间同构）。
    /// 与 [`LIST`] 的区别：一次给全仓库文件、带行图标与可打开判定，供按名检索类界面用。
    pub const FILES: &str = "vault.files";
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 表格面：读写当前打开表格。方法由表格插件提供（模型与保存链都在该插件内），其它插件
/// （表格视图贡献）经这些方法读数据、写改动；改动与插件自身编辑同一出口（同撤销栈、同保存链、
/// 同协作广播）。
pub mod table {
    /// 读当前打开表格的只读快照（字段 / 行 / 仓库相对路径）；未打开表格 = `open: false`。
    pub const SNAPSHOT: &str = "table.snapshot";
    /// 逐格写：只改指定字段，不整行替换（不覆盖同行其它字段与并发编辑）。
    pub const UPDATE_CELL: &str = "table.updateCell";
    /// 新增一行（结果回传新行 id，避免按行 id 差集定位的并发歧义）。
    pub const ADD_ROW: &str = "table.addRow";
    pub const REMOVE_ROW: &str = "table.removeRow";
    /// 写宿主选中态（视图与表格联动用）。
    pub const SELECT_ROW: &str = "table.selectRow";
    /// 向图片字段单元格写入图片引用（图片须先经 `vault.writeBinary` 落到仓库）。
    pub const SET_CELL_IMAGE: &str = "table.setCellImage";
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 面板头部：插件声明自己面板顶部那一行工具条的内容，宿主按当前主题渲染（状态胶囊 / 错误文本 /
/// 协作者胶囊 / 右侧文本 / 菜单项 / 视图下拉）。头部数据全部由插件提供，宿主只负责画与转发点击。
pub mod panel {
    /// 声明某个面板视图的头部状态；`header` 缺省 = 收起头部。
    pub const HEADER: &str = "panel.header";
    /// 回应 [`crate::event::name::PANEL_FLUSH`]：`ok: false` 时带可读原因。
    pub const FLUSH_DONE: &str = "panel.flushDone";
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
pub mod state {
    pub const READ: &str = "state.read";
    pub const WRITE: &str = "state.write";
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
pub mod notification {
    /// 在应用内弹一条通知（宿主已收尾、通知未投递时报可读错误）。
    pub const SHOW: &str = "notification.show";
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
pub mod shortcut {
    pub const REGISTER: &str = "shortcut.register";
    pub const UNREGISTER: &str = "shortcut.unregister";
    /// 事前查询某键位是否已被占用（返回占用方插件 id，或仅知「被系统 / 其他应用占用」）。
    pub const QUERY: &str = "shortcut.query";
    /// 全局快捷键触发（通知）。
    pub const HOST_TRIGGERED: &str = "host:shortcut.triggered";
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
pub mod clipboard {
    pub const WRITE_TEXT: &str = "clipboard.writeText";
    pub const READ_TEXT: &str = "clipboard.readText";
    /// 读剪贴板中的图片（PNG，base64；无图片时字段缺省）。
    pub const READ_IMAGE: &str = "clipboard.readImage";
    /// 写一张 PNG 图片到剪贴板（base64；单次上限 32 MiB，解码失败或超限即拒绝）。
    pub const WRITE_IMAGE: &str = "clipboard.writeImage";
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
pub mod dialog {
    pub const PICK_FILE: &str = "dialog.pickFile";
    pub const PICK_DIRECTORY: &str = "dialog.pickDirectory";
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
pub mod http {
    pub const REQUEST: &str = "http.request";
    /// 打开一条流式出网连接：宿主建连后按块下发 [`HOST_STREAM_CHUNK`]，结束 / 失败经
    /// [`HOST_STREAM_END`]。地址按供应商口径校验（公网目标拒内网，本机 / 局域网服务放行）；
    /// [`REQUEST`] 是不过地址校验的便利层。
    pub const STREAM_OPEN: &str = "http.stream.open";
    /// 取消一条仍在进行的流式连接（幂等：已结束的流也成功）。
    pub const STREAM_CANCEL: &str = "http.stream.cancel";
    /// 流式分片的原始字节（base64；通知）。
    pub const HOST_STREAM_CHUNK: &str = "host:http.stream.chunk";
    /// 流式连接结束（正常 / 失败 / 取消；通知）。
    pub const HOST_STREAM_END: &str = "host:http.stream.end";
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
pub mod doc {
    /// 打开一个文档会话：宿主读盘、建立会话（本地与协作空间同构），回执即当前正文与可写性。
    pub const OPEN: &str = "doc.open";
    /// 内容所有者上报会话状态（未落盘修改与保存失败原因）：宿主只记账，不推断。
    pub const STATE: &str = "doc.state";
    /// 请求一次保存（手动 / 自动）。回执即落盘结果（成功 / 冲突 / 失败三态），宿主据此
    /// 立即更新会话的脏标记与错误，随后内容所有者可用 `doc.state` 修正（如合流后仍脏）。
    pub const SAVE: &str = "doc.save";
    /// 关闭当前文档会话（换文件 / 关标签 / 切仓库）。
    pub const CLOSE: &str = "doc.close";
    /// 把当前文档改名为新的主名（同目录、保留扩展名；引用与历史随宿主既有改名链）。
    /// 有未落盘修改时拒绝，插件须先 `doc.save`。
    pub const RENAME: &str = "doc.rename";
    /// 读一份文档的属性：宿主按会话找到内容所有者后原样转发，载荷形态由所有者定。
    pub const PROPS_READ: &str = "doc.props.read";
    /// 写一份文档的属性：`value` 原样转发给内容所有者，正文写入走它自己的保存链。
    pub const PROPS_WRITE: &str = "doc.props.write";
    /// 用户打开了插件注册的领域文档类型文件（通知）。`surfaceId` 为宿主分配的实例面标识，
    /// 插件据此为该面提交整面帧。
    pub const HOST_OPEN: &str = "host:doc.open";
    /// 文档实例已关闭 / 已切换走（通知）。插件此后不得再向该 surfaceId 提交帧。
    pub const HOST_CLOSE: &str = "host:doc.close";
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 内容所有者对宿主提供的内容服务：宿主按文档会话寻址后原样转发，租约与正文都在插件。
pub mod content {
    pub const SERVICE: &str = "content";
    /// 读一份文档的属性区。
    pub const PROPS_READ: &str = "props.read";
    /// 写一份文档的属性区（正文由所有者自己的保存链落盘）。
    pub const PROPS_WRITE: &str = "props.write";
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 编辑器：打开集合与全局编辑偏好。正文与选区归内容所有者插件，宿主不认识。
pub mod editor {
    /// 读当前打开的文件（仓库相对路径，按打开顺序）。候选列表这类要「打开中的优先」的界面用。
    pub const OPEN_FILES: &str = "editor.openFiles";
    /// 让宿主打开一个仓库内文件（按扩展名分流；回执 = 已受理）。`reveal` 见
    /// [`EditorOpenFileParams`]。
    pub const OPEN_FILE: &str = "editor.openFile";
    /// 读全局编辑偏好（软换行 / 页面内标题 / 正文宽度）；变更时宿主另发
    /// [`crate::event::name::EDITOR_PREFS_CHANGED`]。
    pub const PREFS: &str = "editor.prefs";
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
pub mod history {
    /// 按天聚合的仓库活动：新建 / 修改 / 删除的文件数合计。
    pub const ACTIVITY: &str = "history.activity";
    /// 本地某一天的版本记录（本地历史侧文件 / 空间聚合，宿主统一按本地日界归日）。
    pub const DAY: &str = "history.day";
    /// 全仓库按文件分组的版本聚合（组按最近版本时间新→旧，只给组与版本条数）。
    pub const GROUPS: &str = "history.groups";
    /// 在宿主界面打开某个文件的历史弹层（版本列表与回滚由宿主承载）。
    pub const OPEN: &str = "history.open";
    /// 域插件落盘后记一版（本地记侧文件；与上一版内容相同则不新增）。
    pub const RECORD: &str = "history.record";
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 最近打开：宿主按仓库容器身份记录的打开清单（记录与失效剔除都在宿主，插件只读）。
pub mod recent {
    /// 服务名。
    pub const SERVICE: &str = "recent";
    /// 当前容器的最近打开条目（按打开时刻新→旧）。
    pub const LIST: &str = "recent.list";
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
pub mod ai {
    /// 插件发起 / 续聊一轮对话（须声明 `ai` 权限）。回执 = run 已就位，事件随后经
    /// [`HOST_CHAT_EVENT`] 流达。会话 = 仓库普通会话（主面板可见）；可指定 Agent / 模型 /
    /// 图片（见 [`ChatSendParams`]），Agent 缺省沿用会话或内置「对话」、模型缺省跟随面板
    /// 当前选择；解析失败时拒绝并回传原因。
    pub const CHAT_SEND: &str = "ai.chat.send";
    /// 中断一个会话的活跃 run（幂等：无活跃 run 也成功）。
    pub const CHAT_STOP: &str = "ai.chat.stop";
    /// 读当前激活仓库的会话清单或单会话消息历史。
    pub const CHAT_HISTORY: &str = "ai.chat.history";
    /// 读当前仓库可用的模型清单（按供应商分组，只含 id / 名称，不含地址与密钥）
    /// 与面板当前的模型选择。
    pub const MODELS: &str = "ai.models";
    /// 读当前仓库可用的 Agent 清单（内置 + 仓库自定义）与面板当前生效的 Agent。
    pub const AGENTS: &str = "ai.agents";
    /// 重命名一个会话（写其元数据标题）。
    pub const CHAT_RENAME: &str = "ai.chat.rename";
    /// 删除一个会话（连同其消息文件）。
    pub const CHAT_DELETE: &str = "ai.chat.delete";
    /// 压缩一个会话（把较早消息总结为压缩起点；回执 = 已受理，结果经 `ai.chat.history` 复核）。
    pub const CHAT_COMPACT: &str = "ai.chat.compact";
    /// 让模型重新生成会话标题（结果写回元数据，并经 [`HOST_CHAT_EVENT`] 发 `Titled`）。
    pub const CHAT_TITLE: &str = "ai.chat.title";
    /// 对话事件流（通知）：快照节流拍 + 终态 + 命名结果。
    pub const HOST_CHAT_EVENT: &str = "host:ai.chat.event";
    /// 注册一个可被 Agent 调用的工具（名称全局唯一，不得与内置工具或其他插件的工具重名）。
    pub const REGISTER_TOOL: &str = "ai.registerTool";
    /// 撤销自己注册的工具（未注册的名称报可读错误）。
    pub const UNREGISTER_TOOL: &str = "ai.unregisterTool";
    /// 回报一次工具调用结果（`callId` 原样带回宿主下发值；不在册或不属于本插件的 `callId`
    /// 报可读错误——结果不会进入模型上下文）。
    pub const TOOL_RESULT: &str = "ai.toolResult";
    /// 宿主请求插件执行一次工具调用（通知）；插件处理完经 [`TOOL_RESULT`] 回报。
    pub const HOST_TOOL_CALL: &str = "host:ai.toolCall";
    /// 列当前可用的工具声明（内置 + 已注册插件工具；按调用方给的工具 id 过滤）。
    pub const TOOL_LIST: &str = "ai.tool.list";
    /// 执行一次工具调用：内置工具由宿主执行，插件工具转发给提供者。结果即模型可见文本。
    pub const TOOL_EXECUTE: &str = "ai.tool.execute";
    /// 插件向宿主发布一条对话事件；宿主点对点发给订阅了 [`HOST_CHAT_EVENT`] 的插件，
    /// 并经无界通道交给壳侧。
    pub const CHAT_EMIT: &str = "ai.chat.emit";
    /// 起一轮直写型 run：消息真源归调用方（画布卡），宿主解析模型配置后交给 AI 核心插件。
    /// 事件经 [`HOST_CHAT_EVENT`] 以 `sessionId` 归属，调用方据此把消息写回自己的容器。
    pub const CHAT_SEND_DIRECT: &str = "ai.chat.sendDirect";
    /// 压缩调用方给出的一段消息（真源不在会话语料里）：只回摘要，调用方自行落存储。
    pub const CHAT_COMPACT_DIRECT: &str = "ai.chat.compactDirect";
    /// 读与 AI 运行相关的仓库设置（落卡开关等）；插件据此决定本地派生行为。
    pub const SETTINGS: &str = "ai.settings";
    /// 激活已打开的 AI 对话面板；未打开时不新建、不提示。
    pub const CHAT_OPEN_PANEL: &str = "ai.chat.openPanel";
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 系统托盘：插件提交自己的菜单条目，宿主维护唯一托盘图标（无条目时图标消失）。
pub mod tray {
    /// 整发整换本插件的托盘菜单（空清单 = 收起）。宿主自有的「显示 / 退出」条目恒在末尾。
    pub const SET_ITEMS: &str = "tray.setItems";
    /// 用户点了本插件的某个托盘条目（通知）。
    pub const HOST_CLICK: &str = "host:tray.click";
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 协作空间内的插件消息与团队元数据：消息按「通道」隔离，宿主把插件 id 作为通道前缀，
/// 同空间的同插件实例互通（跨空间不达、不可抢占他人通道）；元数据是空间团队层共享键值。
pub mod collab {
    /// 当前空间的连接状态与在场成员清单（不含自己）。
    pub const PEERS: &str = "collab.peers";
    /// 向同空间的对等插件发一条消息：缺省广播给全部对端，给了目标即单播。
    pub const SEND: &str = "collab.send";
    /// 读当前空间的团队元数据（`key` 缺省 = 全部键）。团队所有插件共享同一份键空间。
    pub const META_GET: &str = "collab.metaGet";
    /// 按键写当前空间的团队元数据：只改给出的键，其余键保持原值。
    pub const META_PATCH: &str = "collab.metaPatch";
    /// 对端消息到达（通知）。
    pub const HOST_MESSAGE: &str = "host:collab.message";
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 协作空间状态与成员名册：房间这类需要「当前是不是空间、谁在线、成员有哪些」的界面用。
/// 名册读宿主缓存（与宿主成员管理弹层共用），缓存为空时宿主顺带发起一次拉取。
pub mod space {
    /// 当前容器与在线成员快照（含本端身份与各端打开的文件）。给房间这类展示界面用；
    /// 消息投递用的对端 id / 名字在 `collab.peers`。
    pub const ROOM: &str = "space.room";
    /// 空间成员名册快照（列表 / 加载中 / 错误）。
    pub const MEMBERS: &str = "space.members";
    /// 打开宿主的「协作空间」菜单（登录与空间列表入口）。
    pub const OPEN_MENU: &str = "space.openMenu";
    /// 打开宿主的「协作设置」页。
    pub const OPEN_SETTINGS: &str = "space.openSettings";
    /// 提交一次文本文件的补丁保存（空间内容真源）：宿主按 `kind` 把「基线 → 冻结快照」的实体差异
    /// 交服务端仲裁，回执带服务端最终路径（补丁带标题时服务端按标题改名）。
    pub const FILE_PATCH: &str = "space.file.patch";
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 仓库界面状态（随仓库走的界面数据）：展开集 / 排序 / 目录颜色 / 提示词标记这类
/// 「宿主与插件都读、重启后要保留」的键值，落仓库配置与对应侧文件，不落插件私有目录。
pub mod vault_ui {
    /// 读一组仓库界面状态：缺省键回落到宿主默认值。
    pub const READ: &str = "vault.ui";
    /// 按键写仓库界面状态：只改给出的键（`null` 删除该键），返回写后的完整快照。
    pub const WRITE: &str = "vault.updateUi";
    /// 新建一个条目（笔记 / 表格 / 画布 / 文件夹 / 插件文档类型）：宿主起空内容并落盘。
    pub const CREATE: &str = "vault.create";
    /// 切换当前容器（本地仓库或协作空间）。
    pub const SWITCH_CONTAINER: &str = "vault.switchContainer";
    /// 从容器列表移除一个容器（只移出最近列表，不动磁盘内容与服务端数据）。
    pub const REMOVE_CONTAINER: &str = "vault.removeContainer";
    /// 列出全仓库的目录（仓库相对路径；「展开全部」这类需要整棵骨架的入口用）。
    pub const LIST_DIRS: &str = "vault.listDirs";
    /// 让宿主重读文件树（磁盘无监听，外部改动只能靠刷新感知）。
    pub const REFRESH_TREE: &str = "vault.refreshTree";
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 文件拖拽会话：ghost、落点判定与悬停自动展开都在宿主，插件只发起 / 更新落点 / 结束。
pub mod drag {
    /// 发起一次拖拽会话（同一插件同一时刻至多一个；重复发起以最新的为准）。
    pub const START: &str = "drag.start";
    /// 插件面在指针移动时上报本面内的落点候选（命中区 id 由插件给出）。
    pub const HOVER: &str = "drag.hover";
    /// 结束拖拽会话（释放即落位；取消则不落位）。
    pub const END: &str = "drag.end";
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 宿主弹层菜单：插件声明条目（可带子面板与只读文字行），宿主按统一外观弹出并回传选中项。
pub mod menu {
    /// 打开一个宿主菜单，锚点为当前指针位置。关闭时经 [`HOST_CLOSED`] 回传原因。
    pub const OPEN: &str = "menu.open";
    /// 宿主关闭了菜单（选中某项 / 点击外点 / Esc）；`selected` 缺省 = 未选中任何项。
    pub const HOST_CLOSED: &str = "host:menu.closed";
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 宿主自有的系统单例界面与容器入口：插件只发起，实际动作与结果回填都在壳。
pub mod shell {
    /// 打开宿主的取色器（HSV 拖动预览 / 松手提交），回执 = 选中色（取消时缺省）。
    pub const PICK_COLOR: &str = "shell.pickColor";
    /// 让宿主选一个文件夹并作为仓库打开（选择器与进入仓库链都在壳）。
    pub const PICK_VAULT: &str = "shell.pickVault";
    /// 在系统文件管理器中显示一个仓库内条目（空路径 = 仓库根；协作空间没有本机路径，宿主拒绝）。
    pub const REVEAL: &str = "shell.reveal";
    /// 取一个仓库内条目的绝对路径（空路径 = 仓库根；协作空间没有本机路径，宿主拒绝）。
    pub const ABSOLUTE_PATH: &str = "shell.absolutePath";
    /// 在同目录创建副本（同名自动加序号；.atlx / .atb 连同内部 id 重建），回执 = 落定的新路径。
    pub const DUPLICATE: &str = "shell.duplicate";
    /// 用系统默认浏览器打开一个外链 URL（只接受 http(s) / mailto，其余由宿主拒绝）。
    pub const OPEN_URL: &str = "shell.openUrl";
}
```

## 9. 能力面负载：文件面板

仓库界面状态（展开集 / 排序 / 目录颜色 / 提示词标记）、容器切换与移除、条目新建与副本、拖拽会话、宿主菜单与系统入口的负载形态。

```rust
// atelyx-plugin-protocol/src/method.rs
/// 文件树的下一层条目。
#[serde(rename_all = "camelCase")]
pub struct UiDirEntry {
    /// 相对本目录的名字（不含父路径）。
    pub name: String,
    /// 相对仓库根的路径（`/` 分隔）。
    pub path: String,
    pub is_dir: bool,
    /// 修改时刻（unix 秒）：按时间排序用。
    pub mtime: i64,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 随仓库走的界面状态：展开集 / 排序 / 目录颜色 / 提示词标记。
#[serde(rename_all = "camelCase")]
pub struct VaultUiState {
    /// 文件树展开的目录（仓库相对路径；空串 = 仓库根）。
    #[serde(default)]
    pub expanded: Vec<String>,
    /// 排序键（`name-asc` / `name-desc` / `mtime-desc` / `mtime-asc`）。
    pub sort: String,
    /// 目录图标颜色（目录相对路径 → hex 色）。
    #[serde(default)]
    pub folder_colors: std::collections::BTreeMap<String, String>,
    /// 已注册为系统提示词的笔记（仓库相对路径）。
    #[serde(default)]
    pub prompt_notes: Vec<String>,
    /// 重命名撞名自动顺延后的提醒文案（`None` = 无提醒）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rename_notice: Option<String>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`vault_ui::READ`] 的 params：`dir` 给定时另带该目录条目。
#[serde(rename_all = "camelCase", default)]
pub struct VaultUiParams {
    /// 需要一个目录的条目（空 = 不要）。
    pub dir: Option<String>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`vault_ui::WRITE`] 的 params：只改给出的键。
#[serde(rename_all = "camelCase", default)]
pub struct VaultUiPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expanded: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder_colors: Option<std::collections::BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_notes: Option<Vec<String>>,
    /// `Some(true)` = 用户关闭重名提醒条（宿主清掉该条）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dismiss_rename_notice: Option<bool>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`vault_ui::READ`] 的 result：状态快照 + 可选目录条目 + 容器清单。
#[serde(rename_all = "camelCase")]
pub struct VaultUiResult {
    pub state: VaultUiState,
    /// 请求了 `dir` 时的条目（宿主已按当前排序排好）；未请求 = 空。
    #[serde(default)]
    pub entries: Vec<UiDirEntry>,
    /// 可切换的容器清单（本地仓库在前、激活项在前）。
    #[serde(default)]
    pub containers: Vec<ContainerEntry>,
    /// 插件注册的文档类型（「新建」菜单条目用；扩展名不带点）。
    #[serde(default)]
    pub doc_types: Vec<DocTypeEntry>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 一个插件注册的文档类型（文件面板「新建」菜单条目）。
#[serde(rename_all = "camelCase")]
pub struct DocTypeEntry {
    /// 扩展名（不带点）。
    pub extension: String,
    /// 菜单标签（如「笔记」）。
    pub label: String,
    /// 宿主图标形状名（文件树条目与新建菜单用；空 = 按扩展名兜底）。
    #[serde(default)]
    pub icon: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`vault_ui::LIST_DIRS`] 的回执：全部目录的仓库相对路径（不含仓库根）。
#[serde(rename_all = "camelCase", default)]
pub struct DirListResult {
    pub dirs: Vec<String>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 一个可切换的仓库容器（本地仓库或协作空间）。
#[serde(rename_all = "camelCase")]
pub struct ContainerEntry {
    /// 唯一键（本地 = `local:<root>`，空间 = `space:<server>#<spaceId>`）。
    pub id: String,
    pub label: String,
    /// 本地仓库根路径（空间缺省）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    /// 协作空间标识（本地缺省）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_id: Option<String>,
    /// 当前激活的容器。
    #[serde(default)]
    pub active: bool,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 容器切换（`vault.switchContainer`）：切到清单里给的那个容器。
#[serde(rename_all = "camelCase")]
pub struct SwitchContainerParams {
    /// [`ContainerEntry::id`] 的原值。
    pub id: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 容器移除（`vault.removeContainer`）：把该容器移出最近列表。
#[serde(rename_all = "camelCase")]
pub struct RemoveContainerParams {
    /// [`ContainerEntry::id`] 的原值。
    pub id: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 新建一个条目（`vault.create`）：宿主按 `kind` 起空内容并落盘（同名自动加序号）。
#[serde(rename_all = "camelCase")]
pub struct CreateEntryParams {
    /// 目标目录（仓库相对路径；空串 = 仓库根）。
    pub dir: String,
    /// 条目名（不含扩展名；`kind = folder` 时即文件夹名）。
    pub name: String,
    /// `note` / `table` / `canvas` / `folder`，或插件注册文档类型的不带点扩展名。
    pub kind: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 新建结果：落定的仓库相对路径。
#[serde(rename_all = "camelCase")]
pub struct CreateEntryResult {
    pub path: String,
    /// 新建的是文件夹。
    #[serde(default)]
    pub is_dir: bool,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 创建副本（`shell.duplicate`）：`path` 为源条目的仓库相对路径。
#[serde(rename_all = "camelCase")]
pub struct DuplicateParams {
    pub path: String,
    /// 源是文件夹（递归复制并重建内部 id）。
    #[serde(default)]
    pub is_dir: bool,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 在文件管理器中显示一个条目（`shell.reveal`）：`path` 为仓库相对路径，空串 = 仓库根。
#[serde(rename_all = "camelCase", default)]
pub struct RevealParams {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub path: String,
    /// 目标是文件夹（文件夹用「打开」语义，文件用「选中」语义）。
    #[serde(default)]
    pub is_dir: bool,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 在外链打开 URL（`shell.openUrl`）：`url` 必须是 http(s) / mailto。
#[serde(rename_all = "camelCase", default)]
pub struct OpenUrlParams {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 条目绝对路径（`shell.absolutePath` 回执）。
#[serde(rename_all = "camelCase")]
pub struct AbsolutePathResult {
    pub path: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 取色器（`shell.pickColor`）：宿主的 HSV 取色器是系统单例 UI，插件只给初始色。
#[serde(rename_all = "camelCase", default)]
pub struct PickColorParams {
    /// 初始色（`#rrggbb`）；缺省 = 宿主强调色。
    pub initial: Option<String>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 取色结果：用户取消时 `color` 缺省。
#[serde(rename_all = "camelCase", default)]
pub struct PickColorResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 拖拽会话里被拖的条目。
#[serde(rename_all = "camelCase")]
pub struct DragItem {
    /// 仓库相对路径。
    pub path: String,
    pub is_dir: bool,
    /// ghost 上显示的文案（缺省 = 路径末段）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`drag::START`] 的 params：指针起点取宿主当前指针位置。
#[serde(rename_all = "camelCase")]
pub struct DragStartParams {
    pub item: DragItem,
    /// 承载发起面所在的窗口面标识（宿主据此把 ghost 画在正确窗口）。
    pub surface_id: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`drag::HOVER`] 的 params：本面上指针所指的落点候选。
/// 几何（哪一行）由插件判定，能不能落由宿主按同一套规则复核。
#[serde(rename_all = "camelCase", default)]
pub struct DragHoverParams {
    /// 落点目录（仓库相对路径；空串 = 仓库根）；缺省 = 本面不接受落入。
    pub target: Option<String>,
    /// 该落点是不是目录（文件行不接受落入）。
    pub is_dir: bool,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`drag::END`] 的 params：`cancelled` = 不落位。
#[serde(rename_all = "camelCase", default)]
pub struct DragEndParams {
    pub cancelled: bool,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 宿主回推给插件的拖拽状态（[`crate::event::name::DRAG_STATE`]）：驱动落点高亮与拒绝提示。
#[serde(rename_all = "camelCase")]
pub struct DragStateParams {
    /// 当前落点（仓库相对路径）；`None` = 指针不在本面的任何落点上。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// 落点不可用时的可读原因。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reject: Option<String>,
    /// 悬停折叠目录到点会自动展开。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub springing: bool,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 菜单条目：与宿主内置菜单同一套形态。
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum MenuItem {
    /// 动作项：`id` 随选中结果回传。
    Action {
        id: String,
        label: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        danger: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        disabled: bool,
        /// 行尾角标（如「当前」）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        /// 悬停提示。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hint: Option<String>,
        /// 所属面板（0 = 根面板）。
        #[serde(default)]
        pane: usize,
    },
    /// 当前选中项（强调色文字 + 右侧对勾）。
    Checked {
        id: String,
        label: String,
        #[serde(default)]
        pane: usize,
    },
    /// 钻入子面板。
    Submenu {
        label: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        disabled: bool,
        /// 所属面板（0 = 根面板）。
        #[serde(default)]
        pane: usize,
        /// 点击后钻入的面板号（必须 ≥ 1：根面板不能再钻回自己）。
        target: usize,
    },
    /// 只读文字行（确认说明这类）。
    Text {
        text: String,
        #[serde(default)]
        pane: usize,
    },
    /// 分组头（灰底小字）。
    Group {
        label: String,
        #[serde(default)]
        pane: usize,
    },
    /// 分隔线。
    Divider {
        #[serde(default)]
        pane: usize,
    },
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`menu::OPEN`] 的 params：`items` 顺序即渲染顺序。
#[serde(rename_all = "camelCase")]
pub struct MenuOpenParams {
    pub items: Vec<MenuItem>,
    /// 菜单宽度（逻辑像素；宿主按内容与视口钳制）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f32>,
    /// 声明后宿主在菜单顶部渲染搜索行：只匹配条目标签（去首尾空白、忽略大小写的子串）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<MenuSearch>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 菜单顶部搜索行；声明后宿主渲染输入框并按标签过滤条目。
#[serde(rename_all = "camelCase")]
pub struct MenuSearch {
    /// 输入框提示文字。
    pub placeholder: String,
    /// 无匹配结果时的提示；缺省不显示空态行。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub empty: Option<String>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`menu::HOST_CLOSED`] 的 params：关闭原因与选中的条目 id。
#[serde(rename_all = "camelCase", default)]
pub struct MenuClosedParams {
    /// 选中的动作 / 勾选项 id；未选中（点击外部 / Esc）时缺省。
    pub selected: Option<String>,
}
```

## 10. 能力面负载：文件、进程与系统服务

上表方法的 params / result 形态（`path` 在 `fs` 面是绝对路径，在 `vault` 面是仓库相对路径）。

```rust
// atelyx-plugin-protocol/src/method.rs
#[serde(rename_all = "camelCase")]
pub struct PrivateDirResult {
    pub path: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// `path` 的口径由能力面决定：`fs` 用绝对路径，`vault` 用相对仓库根的路径。
/// 信任模型：插件与宿主同进程、持同等权限，声明式权限是透明度机制而非沙箱边界；
/// `fs` 服务的价值在统一出口与审计事实。
#[serde(rename_all = "camelCase")]
pub struct PathParams {
    pub path: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
#[serde(rename_all = "camelCase")]
pub struct TextContent {
    pub content: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
#[serde(rename_all = "camelCase")]
pub struct BinaryContent {
    pub base64: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
#[serde(rename_all = "camelCase")]
pub struct WriteTextParams {
    pub path: String,
    pub content: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
#[serde(rename_all = "camelCase")]
pub struct WriteBinaryParams {
    pub path: String,
    pub base64: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`vault::IMPORT_FILE`] 的 params：`path` 为仓库相对目标，`source` 为插件私有目录内的绝对路径。
/// 复制语义：落库成功后源文件仍归插件，由插件自行清理。
#[serde(rename_all = "camelCase")]
pub struct ImportFileParams {
    pub path: String,
    pub source: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
#[serde(rename_all = "camelCase")]
pub struct ExistsResult {
    pub exists: bool,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`vault::IMAGE_INFO`] 的 result：图片固有像素尺寸（不随界面缩放）。
#[serde(rename_all = "camelCase")]
pub struct ImageInfoResult {
    pub width: u32,
    pub height: u32,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`vault::RENAME`] 的 params：`from` / `to` 均为仓库相对路径，`to` 不可已存在。
#[serde(rename_all = "camelCase")]
pub struct VaultRenameParams {
    pub from: String,
    pub to: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`vault::TAGS`] 的 result：仓库里出现过的标签（按出现次数降序）。
#[serde(rename_all = "camelCase")]
pub struct VaultTagsResult {
    pub tags: Vec<String>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`vault::FILES`] 的 result：全仓库可检索文件，按宿主文件树的遍历序（同层按名字排序，
/// 目录内容跟在该目录的位置）。
#[serde(rename_all = "camelCase")]
pub struct FilesResult {
    pub files: Vec<VaultFileEntry>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 可检索文件的一行。`icon` 是宿主注册表的图标键（插件注册的领域文档类型按注册图标，
/// 其余按内置扩展名映射）。`openable` = 行是否挂打开动作：图片附件不挂（由画布媒体卡承载），
/// 其余类型交给宿主按扩展名分流，没有对应视图时宿主提示。
#[serde(rename_all = "camelCase")]
pub struct VaultFileEntry {
    /// 仓库相对路径（`/` 分隔）。
    pub path: String,
    /// 文件名（路径末段）。
    pub name: String,
    pub icon: String,
    pub openable: bool,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
#[serde(rename_all = "camelCase")]
pub struct ListResult {
    pub entries: Vec<FsEntry>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
#[serde(rename_all = "camelCase")]
pub struct FsEntry {
    pub name: String,
    pub kind: FsEntryKind,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
#[serde(rename_all = "camelCase")]
pub enum FsEntryKind {
    File,
    Dir,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
#[serde(rename_all = "camelCase")]
pub struct SpawnParams {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// 追加 / 覆盖宿主环境（不清空——被托管服务需要 PATH/TEMP 等才能正常行为）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<std::collections::BTreeMap<String, String>>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
#[serde(rename_all = "camelCase")]
pub struct SpawnResult {
    pub pid: u32,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
#[serde(rename_all = "camelCase")]
pub struct PidParams {
    pub pid: u32,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
#[serde(rename_all = "camelCase")]
pub struct WriteStdinParams {
    pub pid: u32,
    pub data: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 托管进程事件负载（[`process::HOST_EVENT`]）。形态与宿主进程托管的上报一一对应。
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ProcessEvent {
    Stdout { data: String },
    Stderr { data: String },
    Terminated { code: Option<i32> },
    Error { message: String },
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
#[serde(rename_all = "camelCase")]
pub struct ProcessEventParams {
    pub pid: u32,
    pub event: ProcessEvent,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
#[serde(rename_all = "camelCase")]
pub struct StateReadResult {
    /// 该插件的自持数据；从未写过时为 null。
    pub json: Value,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
#[serde(rename_all = "camelCase")]
pub struct StateWriteParams {
    pub json: Value,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
#[serde(rename_all = "camelCase")]
pub struct ShowNotificationParams {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default)]
    pub level: NotificationLevel,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
#[serde(rename_all = "camelCase")]
pub enum NotificationLevel {
    #[default]
    Info,
    Success,
    Warning,
    Error,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
#[serde(rename_all = "camelCase")]
pub struct RegisterShortcutParams {
    /// 加速键描述（如 `Shift+Alt+E`）；与既注册项冲突时报可读错误。
    pub accelerator: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
#[serde(rename_all = "camelCase")]
pub struct AcceleratorParams {
    pub accelerator: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
#[serde(rename_all = "camelCase")]
pub struct ShortcutTriggeredParams {
    pub accelerator: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// `shortcut.query` 的 params。
#[serde(rename_all = "camelCase")]
pub struct ShortcutQueryParams {
    pub accelerator: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// `shortcut.query` 的 result：`available` = 可注册；`owner` = 占用方插件 id（被系统 / 其他应用占用
/// 或无占用时为缺省——此时只有 `available` 可信，宿主无从得知占用者身份）。
#[serde(rename_all = "camelCase")]
pub struct ShortcutQueryResult {
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 剪贴板写 params 与读 result 共用：一段纯文本。
#[serde(rename_all = "camelCase")]
pub struct TextPayload {
    pub text: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 剪贴板图片：统一编成 PNG（插件不需要自己处理系统剪贴板的像素格式）。
#[serde(rename_all = "camelCase")]
pub struct ClipboardImage {
    pub base64: String,
    pub width: u32,
    pub height: u32,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
#[serde(rename_all = "camelCase")]
pub struct ClipboardImageResult {
    /// 剪贴板里没有图片时整个字段缺省（与「有图片但为空」不做区分——空图片不成立）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<ClipboardImage>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`clipboard::WRITE_IMAGE`] 的 params：PNG 字节的 base64。
#[serde(rename_all = "camelCase")]
pub struct ClipboardWriteImageParams {
    pub base64: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
#[serde(rename_all = "camelCase")]
pub struct PickFileParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// true = 保存对话框；取消时 result 的 `path` 字段缺省。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub save: Option<bool>,
    /// 允许多选（只对打开对话框有意义）：结果按选择顺序给全，`path` 仍是第一个。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub multiple: bool,
    /// 预填路径：目录部分作起始目录，文件名部分预填进名称栏。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_path: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filters: Vec<FileFilter>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
#[serde(rename_all = "camelCase")]
pub struct FileFilter {
    pub name: String,
    /// 不带点的扩展名列表（如 `["png", "jpg"]`）。
    pub extensions: Vec<String>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
#[serde(rename_all = "camelCase")]
pub struct PickDirectoryParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
#[serde(rename_all = "camelCase")]
pub struct PickedPath {
    /// 用户取消时该字段缺省（serde 对 null 与缺省等价处理）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// 多选时的全部路径（顺序同用户选择；单选或取消为空）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
#[serde(rename_all = "camelCase")]
pub struct HttpRequestParams {
    pub method: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<std::collections::BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_base64: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
#[serde(rename_all = "camelCase")]
pub struct HttpResult {
    pub status: u16,
    pub headers: std::collections::BTreeMap<String, String>,
    pub body_base64: String,
}
```

## 11. 能力面负载：文档与对话

文档实例、文档会话、AI 对话与工具桥的负载形态。

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`doc::HOST_OPEN`] 的 params：文档实例被打开。
#[serde(rename_all = "camelCase")]
pub struct DocInstanceOpenParams {
    /// 仓库容器内的相对路径（本地仓库）或服务端内容路径（协作空间），与 vault 口径一致。
    pub path: String,
    /// 宿主为本实例分配的面标识：插件据此为该面提交整面帧。
    pub surface_id: String,
    /// 文档会话 id（与 `path` 同值；插件按它寻址缓冲）。
    pub doc_id: String,
    /// 本次打开动作的序号；迟到回执按 `(docId, openSeq)` 丢弃。
    pub open_seq: u64,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`doc::HOST_CLOSE`] 的 params：实例随标签关闭 / 换文件被关闭。
#[serde(rename_all = "camelCase")]
pub struct DocInstanceCloseParams {
    pub surface_id: String,
    pub path: String,
    pub doc_id: String,
    pub open_seq: u64,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`editor::OPEN_FILE`] 的 params：`path` 为仓库相对路径。
/// `reveal` 缺省 = 打开并让对应视图显示（布局里没有就新增该视图标签）；
/// `false` = 只打开不改布局（列表类入口用，已有对应视图照常显示当前文件）。
#[serde(rename_all = "camelCase")]
pub struct EditorOpenFileParams {
    pub path: String,
    #[serde(default = "reveal_default")]
    pub reveal: bool,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`editor::PREFS`] 的 result：全局编辑偏好。`content_width` = 正文列最大宽度
/// （0 = 不限，铺满可用宽度）。
#[serde(rename_all = "camelCase")]
pub struct EditorPrefs {
    /// 宽松换行：开 = 段内单个换行保留为换行；关 = 折叠为空格。
    #[serde(default)]
    pub soft_wrap: bool,
    /// 正文列顶部显示文件名（点击就地改名）。
    #[serde(default)]
    pub in_page_title: bool,
    pub content_width: u32,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 单次提交的文档正文上限（超限按可读错误拒绝）。
pub const MAX_DOC_TEXT_BYTES: usize = 8 * 1024 * 1024;
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`doc::OPEN`] 的 params：按会话身份读取内容所有者手里的正文。
#[serde(rename_all = "camelCase")]
pub struct DocOpenParams {
    /// 文档会话 id（仓库相对路径 / 服务端内容路径，与 `vault` 口径一致）。
    pub doc_id: String,
    /// 本次打开动作的序号；同一文档重开即递增，迟到回执按它丢弃。
    pub open_seq: u64,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`doc::OPEN`] 的 result：原始正文、磁盘基线与可写性。
#[serde(rename_all = "camelCase")]
pub struct DocOpenResult {
    pub doc_id: String,
    pub open_seq: u64,
    pub path: String,
    /// 原始磁盘正文（宿主不解析、不合并）。
    pub text: String,
    pub can_write: bool,
    /// `text` 所对应磁盘内容的完整 SHA-256（十六进制）；插件提交保存时原样回传。
    pub disk_rev: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`doc::STATE`] 的 params：内容所有者判定的会话状态。宿主只按上报值记账，不自行推断。
#[serde(rename_all = "camelCase")]
pub struct DocStateParams {
    pub doc_id: String,
    pub open_seq: u64,
    /// 是否有未落盘修改。
    #[serde(default)]
    pub dirty: bool,
    /// 最近一次保存失败原因（`None` = 无；再次成功保存即清除）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub save_error: Option<String>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`doc::SAVE`] 的 params：内容所有者提交完整正文，宿主按 `expected_rev` 校验后原子写。
/// `manual` = 手动保存（成功 / 失败均有提示）；缺省 = 自动保存。
#[serde(rename_all = "camelCase")]
pub struct DocSaveParams {
    pub doc_id: String,
    pub open_seq: u64,
    /// 插件读到 / 上次写成磁盘内容的完整 SHA-256；与写前即时读盘结果不符即冲突。
    pub expected_rev: String,
    pub text: String,
    #[serde(default)]
    pub manual: bool,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`doc::SAVE`] 的回执：成功给新基线；磁盘已被外部改写时把磁盘原文原样交回插件合并。
#[serde(tag = "status", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum DocSaveResult {
    Ok { new_rev: String },
    Conflict { disk_rev: String, disk_content: String },
    Err { message: String },
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`doc::CLOSE`] 的 params：关闭会话（按下标识别，迟到回执据此丢弃）。
#[serde(rename_all = "camelCase")]
pub struct DocCloseParams {
    pub doc_id: String,
    pub open_seq: u64,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 一条反链：引用该文档的文件路径与显示标题。
#[serde(rename_all = "camelCase")]
pub struct BacklinkEntry {
    pub path: String,
    pub title: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`vault::BACKLINKS`] 的 result。
#[serde(rename_all = "camelCase")]
pub struct BacklinksResult {
    pub entries: Vec<BacklinkEntry>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`doc::RENAME`] 的 params：当前路径与新主名（不含扩展名）。
#[serde(rename_all = "camelCase")]
pub struct DocRenameParams {
    pub path: String,
    pub name: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 按文档寻址：`doc_id` 为空 = 宿主当前活动文档。
#[serde(rename_all = "camelCase")]
pub struct DocTargetParams {
    #[serde(default)]
    pub doc_id: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`doc::PROPS_WRITE`] 的入参：`value` 是属性面板给内容所有者的完整请求载荷，宿主原样转发。
/// `doc_id` 只用于寻址（须与当前热文档一致），不参与载荷解释。
#[serde(rename_all = "camelCase")]
pub struct DocPropsWriteParams {
    #[serde(default)]
    pub doc_id: String,
    pub value: Value,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`content::PROPS_READ`] 的 params：要读属性的文档会话。
#[serde(rename_all = "camelCase")]
pub struct ContentPropsReadParams {
    pub doc_id: String,
    pub open_seq: u64,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`content::PROPS_READ`] 的 result：属性区原文与可写性。
#[serde(rename_all = "camelCase")]
pub struct ContentPropsResult {
    /// 读取时的文档路径；会话已失效时为空串。
    pub path: String,
    /// 读取时正文的指纹；写回须原样回传，正文已变即被拒绝。
    pub fingerprint: String,
    /// frontmatter 块原文（含首尾 `---` 定界）；没有属性块时为空串。
    pub frontmatter: String,
    /// 是否有属性区（含未闭合的 `---` 起始行）。
    #[serde(default)]
    pub present: bool,
    /// 属性区 YAML 是否可解析（未闭合或语法错误为 `false`）。
    pub parsed: bool,
    /// 当前文档是否可写（只读身份 / 会话已失效为 `false`）。
    pub editable: bool,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`content::PROPS_WRITE`] 的 params：`value` 是属性面板的完整写回载荷，语义归内容所有者
/// （宿主只补会话身份并转发）。
#[serde(rename_all = "camelCase")]
pub struct ContentPropsWriteParams {
    pub doc_id: String,
    pub open_seq: u64,
    pub value: Value,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`ai::CHAT_SEND`] 的 params：`session_id` 缺省 = 新建会话；非缺省 = 在该会话续聊
/// （会话须存在于当前仓库，否则可读拒绝）。同会话已有活跃 run 时拒绝（每会话单 run）。
#[serde(rename_all = "camelCase")]
pub struct ChatSendParams {
    /// 正文；为空时 `images` 必须非空（纯图片消息）。
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Agent id（当前仓库名册）；缺省 = 新建会话用内置「对话」工具集、续聊沿用会话已记录的
    /// Agent。新建会话时写入会话记录，续聊时只作用于本轮；指定但不存在 = 拒绝。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// 模型 id（供应商配置里的模型名）；缺省 = 面板当前选择，指定但未配置 = 拒绝。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// 供应商 id：给 `model` 消歧（同模型多供应商时必填；单独给出 = 拒绝）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    /// 推理等级（`low` / `medium` / `high`）；缺省 = 面板当前覆盖值，`off` = 不传该参数。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// 仓库内图片相对路径（png / jpg / jpeg / webp / gif）；逐张校验，任一不可用 = 整条拒绝。
    /// 校验通过后到 run 读取之间文件可能消失：读不到的那张在该轮请求里跳过。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
    /// 用户原始输入（未拼引用路径块）；缺省 = `text` 即原文。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_text: Option<String>,
    /// 正文中登记的 # 引用；宿主按显示文本扫描命中后拼进消息开头。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<ChatRef>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`ai::CHAT_SEND`] 的 result：run 已就位；流式事件随后经 [`ai::HOST_CHAT_EVENT`] 流达。
#[serde(rename_all = "camelCase")]
pub struct ChatSendResult {
    pub session_id: String,
    /// 本轮 assistant 消息 id（后续事件载荷同源）。
    pub message_id: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`ai::CHAT_SEND_DIRECT`] 的 params：宿主解析模型配置后把同一份负载交给 AI 核心插件，
/// 消息真源不进入会话语料。
#[serde(rename_all = "camelCase")]
pub struct ChatDirectParams {
    /// 事件归属的会话键（画布卡等合成 id）。
    pub session_id: String,
    /// 本轮之前的消息（不含即将追加的用户消息）。
    pub history: Vec<Value>,
    /// 已构造好的用户消息（含引用路径块、附件与 displayContent）。
    pub user_message: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<Value>,
    /// Agent id（当前仓库名册）；缺省 = 内置「对话」工具集。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// 显式模型（可只给模型 id，在仓库供应商配置里唯一命中即采用；
    /// 指定 `providerId` 时必须同时给出模型 id）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    /// 推理等级（`off` / `low` / `medium` / `high`）；缺省 = 面板当前覆盖值。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// 空间模式下已取回的 Agent 提示词正文（本地模式缺省，宿主自取）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_prompt: Option<String>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`ai::CHAT_COMPACT_DIRECT`] 的 params：压缩调用方给出的消息，不读写会话语料。
#[serde(rename_all = "camelCase")]
pub struct ChatDirectCompactParams {
    /// 事件归属的会话键（与直写 run 同口径）；缺省时仅按消息压缩。
    #[serde(default)]
    pub session_id: String,
    pub messages: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`ai::CHAT_COMPACT_DIRECT`] 的 result。
#[serde(rename_all = "camelCase")]
pub struct ChatDirectCompactResult {
    pub summary: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`ai::SETTINGS`] 的 result：与 AI 运行相关的仓库设置。
#[serde(rename_all = "camelCase")]
pub struct AiSettingsResult {
    /// 画布对话落卡开关；缺省 = 开启。
    #[serde(default = "default_auto_materialize")]
    pub auto_materialize: bool,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`ai::CHAT_STOP`] 的 params。
#[serde(rename_all = "camelCase")]
pub struct ChatStopParams {
    pub session_id: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`ai::CHAT_HISTORY`] 的 params：`session_id` 缺省 = 列会话清单；非缺省 = 读该会话消息。
/// 单会话模式可给 `offset` / `query` 做分页与搜索（`offset` 为末尾起算的跳过条数）。
#[serde(rename_all = "camelCase")]
pub struct ChatHistoryParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// 消息条数上限（读单会话时生效；缺省全量，超出取末尾）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// 从末尾起跳过的条数（分页：先按 `query` 过滤，再取 `[len-offset-limit, len-offset)`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u32>,
    /// 关键词过滤（在消息文本中不区分大小写搜索；缺省不过滤）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`ai::CHAT_RENAME`] 的 params。
#[serde(rename_all = "camelCase")]
pub struct ChatRenameParams {
    pub session_id: String,
    /// 新标题（非空；空白视为非法）。
    pub title: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`ai::CHAT_DELETE`] / [`ai::CHAT_COMPACT`] 的 params。
#[serde(rename_all = "camelCase")]
pub struct ChatSessionParams {
    pub session_id: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 一条会话摘要（清单模式）。
#[serde(rename_all = "camelCase")]
pub struct ChatSessionSummary {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub updated_at: i64,
    pub message_count: u64,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 单会话读取模式的 result：元数据 + 消息数组（消息形态与会话磁盘格式同源：
/// `id` / `role` / `content` / `displayContent` / `steps` / 压缩注解）。
#[serde(rename_all = "camelCase")]
pub struct ChatSessionWire {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub messages: Vec<Value>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`ai::CHAT_HISTORY`] 的 result：清单模式只填 `sessions`；单会话模式只填 `session`。
#[serde(rename_all = "camelCase")]
pub struct ChatHistoryResult {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<ChatSessionSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<ChatSessionWire>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
#[serde(rename_all = "camelCase")]
pub struct AiModel {
    /// 模型名（`ChatSendParams.model` 的取值）。
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 一个供应商及其模型（不含地址与密钥）。
#[serde(rename_all = "camelCase")]
pub struct AiModelProvider {
    pub id: String,
    pub name: String,
    pub models: Vec<AiModel>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 面板当前的模型选择（供应商 id + 模型名）。
#[serde(rename_all = "camelCase")]
pub struct AiModelPick {
    pub provider_id: String,
    pub model: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`ai::MODELS`] 的 result：`current` = 面板当前选择，未显式选择时缺省。
#[serde(rename_all = "camelCase")]
pub struct AiModelsResult {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<AiModelProvider>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<AiModelPick>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 一个可用 Agent（内置 + 仓库自定义）。
#[serde(rename_all = "camelCase")]
pub struct AiAgent {
    pub id: String,
    pub name: String,
    /// 预置 Agent（不可删除，可改配置）。
    pub builtin: bool,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`ai::AGENTS`] 的 result：`current` = 面板当前生效的 Agent id，无生效项时缺省。
#[serde(rename_all = "camelCase")]
pub struct AiAgentsResult {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<AiAgent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`ai::HOST_CHAT_EVENT`] 的 params：载荷按 [`ChatEvent`] 分型。
#[serde(rename_all = "camelCase")]
pub struct ChatEventParams {
    pub session_id: String,
    pub message_id: String,
    pub event: ChatEvent,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 对话事件（快照级）：整条消息替换渲染，丢拍由后拍自愈；终态只发一次。
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ChatEvent {
    /// 流式节流拍：该消息当前完整形态（含 steps 行）。
    Message { rev: u64, message: Value },
    /// run 收敛（含中断：`reason` = `stop` 正常收敛 / `interrupted` 用户中断，中断的行已归一）。
    Finished { reason: String },
    /// run 失败收场。
    Failed { error: String },
    /// 话题自动命名完成（`None` = 命名未启用或失败，插件维持原展示）。
    Titled { title: Option<String> },
    /// 直写模式的空占位移除（中断且没有产生任何内容）。
    Removed,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`ai::REGISTER_TOOL`] 的 params：`name` 为模型可见的工具名（字母 / 数字 / `_` / `-`，
/// 最长 64 字符），`parameters` 为 JSON Schema 对象。
#[serde(rename_all = "camelCase")]
pub struct RegisterToolParams {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`ai::UNREGISTER_TOOL`] 的 params。
#[serde(rename_all = "camelCase")]
pub struct UnregisterToolParams {
    pub name: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`ai::HOST_TOOL_CALL`] 的 params：插件须用同一 `callId` 经 [`ai::TOOL_RESULT`] 回报。
#[serde(rename_all = "camelCase")]
pub struct ToolCallParams {
    pub call_id: String,
    pub name: String,
    pub arguments: Value,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`ai::TOOL_RESULT`] 的 params：`content` 即模型可见结果文本。
#[serde(rename_all = "camelCase")]
pub struct ToolResultParams {
    pub call_id: String,
    pub content: String,
    #[serde(default)]
    pub is_error: bool,
}
```

## 12. 会话真源、工具桥与流式出网

会话真源由宿主保管：插件用 `chat.session.*` 读写，批次形状与宿主磁盘模型一致。工具桥由宿主记账（名册 / 执行 / 结果回报），对话事件只允许 AI 核心插件经 `ai.chat.emit` 发布。`http.stream.*` 把原始响应分片交给发起插件，由插件自己解析协议。

```rust
// atelyx-plugin-protocol/src/method.rs
/// 会话真源读写（`chat.session.*`）：宿主保持消息形状与写盘链，插件提交声明式批次。
pub mod chat {
    /// 读单个会话（元数据 + 消息）。
    pub const SESSION_READ: &str = "chat.session.read";
    /// 提交一个批次（创建 / 元数据 / 追加 / 补丁 / 截断 / 删除 / 覆盖项）。
    pub const SESSION_WRITE: &str = "chat.session.write";
    /// 读全仓库快照（会话清单元数据 + 覆盖层 + 落盘失败；消息按会话单独读）。
    pub const SESSION_SNAPSHOT: &str = "chat.session.snapshot";
}
```

```rust
// atelyx-plugin-protocol/src/chat.rs
/// 新会话登记。`file` 缺省由宿主按会话 id 补齐（插件不需要知道磁盘目录）。
#[serde(rename_all = "camelCase")]
pub struct SessionDef {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<Value>,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default)]
    pub messages: Vec<Value>,
}
```

```rust
// atelyx-plugin-protocol/src/chat.rs
/// 既有会话的元数据字段更新（缺省字段不动）。
#[serde(rename_all = "camelCase")]
pub struct MetaPatch {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// 双层 Option：外层缺省 = 不动，内层 `None` = 清除（切回默认 Agent）。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "double_option"
    )]
    pub agent_id: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
}
```

```rust
// atelyx-plugin-protocol/src/chat.rs
#[serde(rename_all = "camelCase")]
pub struct AppendBatch {
    pub session_id: String,
    pub messages: Vec<Value>,
}
```

```rust
// atelyx-plugin-protocol/src/chat.rs
/// 消息内容补丁：`rev` 单调，迟到的旧补丁不覆盖新内容。
#[serde(rename_all = "camelCase")]
pub struct PatchEntry {
    pub session_id: String,
    pub message_id: String,
    pub rev: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<Value>,
}
```

```rust
// atelyx-plugin-protocol/src/chat.rs
#[serde(rename_all = "camelCase")]
pub struct TruncateEntry {
    pub session_id: String,
    pub keep_count: usize,
}
```

```rust
// atelyx-plugin-protocol/src/chat.rs
/// 会话删除：`removeFiles = false` 只摘除内存登记（磁盘文件已在外部消失的对账路径）。
#[serde(rename_all = "camelCase")]
pub struct DeleteEntry {
    pub session_id: String,
    #[serde(default = "default_true")]
    pub remove_files: bool,
}
```

```rust
// atelyx-plugin-protocol/src/chat.rs
#[serde(rename_all = "camelCase")]
pub struct MessageRemoval {
    pub session_id: String,
    pub message_id: String,
}
```

```rust
// atelyx-plugin-protocol/src/chat.rs
/// 覆盖项更新（双层 Option：外层缺省 = 不动，内层 `None` = 清除覆盖）。
#[serde(rename_all = "camelCase")]
pub struct OverridesPatch {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "double_option"
    )]
    pub model_override: Option<Option<Value>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "double_option"
    )]
    pub effort_override: Option<Option<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "double_option"
    )]
    pub agent_override: Option<Option<String>>,
}
```

```rust
// atelyx-plugin-protocol/src/chat.rs
/// 一次提交：登记 / 元数据 / 消息追加 / 内容补丁 / 截断 / 删除 / 覆盖项。
#[serde(rename_all = "camelCase")]
pub struct CommitBatch {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub created: Vec<SessionDef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metas: Vec<MetaPatch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub appends: Vec<AppendBatch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub patches: Vec<PatchEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub truncations: Vec<TruncateEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deletes: Vec<DeleteEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub message_removals: Vec<MessageRemoval>,
    #[serde(default)]
    pub overrides: OverridesPatch,
}
```

```rust
// atelyx-plugin-protocol/src/chat.rs
/// [`crate::method::chat::SESSION_READ`] 的 params。
#[serde(rename_all = "camelCase")]
pub struct SessionReadParams {
    pub session_id: String,
}
```

```rust
// atelyx-plugin-protocol/src/chat.rs
/// 单会话读面：元数据 + 消息数组（消息形态与磁盘格式同源）。
#[serde(rename_all = "camelCase")]
pub struct SessionRecord {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<Value>,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default)]
    pub messages: Vec<Value>,
}
```

```rust
// atelyx-plugin-protocol/src/chat.rs
/// 落盘失败信息（`message` + 时间戳）。
#[serde(rename_all = "camelCase")]
pub struct ChatPersistError {
    pub message: String,
    pub at: i64,
}
```

```rust
// atelyx-plugin-protocol/src/chat.rs
/// 全仓库读面快照（进仓 / 切仓 / 覆盖层引导）：会话只出元数据，
/// 消息按会话用 `chat.session.read` 取，快照本身不随历史长度增长。
#[serde(rename_all = "camelCase")]
pub struct ChatStoreSnapshot {
    #[serde(default)]
    pub session_vault_key: String,
    #[serde(default)]
    pub sessions: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_override: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort_override: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_override: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persist_error: Option<ChatPersistError>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 一条 # 引用：仓库相对路径 + 显示标签 + 领域类型。
#[serde(rename_all = "camelCase")]
pub struct ChatRef {
    pub path: String,
    pub label: String,
    /// 领域类型：`note` / `table` / `canvas` / `dir`。
    pub ref_kind: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 一条工具声明（模型侧 schema）。
#[serde(rename_all = "camelCase")]
pub struct ToolDecl {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    /// 分类：`web` / `file` / `task` / `plugin`。
    pub category: String,
    /// 可与其他工具并发执行（读写 / 未知工具串行屏障）。
    pub parallel_safe: bool,
    /// 勾选 id（内置工具 = 名称，插件工具 = `plugin:<插件 id>:<工具名>`）。
    pub selection_id: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`ai::TOOL_LIST`] 的 params：缺省 = 全部可用工具。
#[serde(rename_all = "camelCase")]
pub struct ToolListParams {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_ids: Vec<String>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`ai::TOOL_LIST`] 的 result。
#[serde(rename_all = "camelCase")]
pub struct ToolListResult {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolDecl>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`ai::TOOL_EXECUTE`] 的 params。
#[serde(rename_all = "camelCase")]
pub struct ToolExecParams {
    /// 工具名（内置工具或插件工具；插件工具用注册时的裸名称）。
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`ai::TOOL_EXECUTE`] 的 result：`content` 即模型可见结果文本。
#[serde(rename_all = "camelCase")]
pub struct ToolExecResult {
    pub content: String,
    #[serde(default)]
    pub is_error: bool,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`http::STREAM_OPEN`] 的 params。
#[serde(rename_all = "camelCase")]
pub struct HttpStreamOpenParams {
    /// 调用方指定的流 id（可选）：指定后宿主按该 id 下发分片，插件可先登记接收端再开流。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_id: Option<String>,
    pub method: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub headers: std::collections::BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// 空闲超时（毫秒）：超过该时长没有新数据即按失败结束；缺省由宿主设定。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`http::STREAM_OPEN`] 的 result。
#[serde(rename_all = "camelCase")]
pub struct HttpStreamOpenResult {
    pub stream_id: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`http::STREAM_CANCEL`] 的 params。
#[serde(rename_all = "camelCase")]
pub struct HttpStreamCancelParams {
    pub stream_id: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`http::HOST_STREAM_CHUNK`] 的 params。
#[serde(rename_all = "camelCase")]
pub struct HttpStreamChunkParams {
    pub stream_id: String,
    /// 原始字节（base64）。
    pub base64: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`http::HOST_STREAM_END`] 的 params：`error` 缺省 = 正常结束；
/// `cancelled` = 调用方主动取消（`http.stream.cancel` / 插件停用），不是流自然结束；
/// `status` = 响应状态码（建连成功时给出），供调用方区分鉴权 / 限流等非 2xx 失败。
#[serde(rename_all = "camelCase")]
pub struct HttpStreamEndParams {
    pub stream_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub cancelled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
}
```

## 13. 能力面负载：表格

表格面读写当前打开的表格；内容真源是磁盘上的 `.atb` 文件，插件写盘经宿主 `vault.writeText`。

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`table::SNAPSHOT`] 的 result：当前打开表格的只读快照。未打开表格 = `open: false`（其余字段缺省）。
#[serde(rename_all = "camelCase", default)]
pub struct TableSnapshot {
    pub open: bool,
    /// 表格稳定 id（改名 / 移动后不变；未打开表格时缺省）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// 当前打开表格的仓库相对路径。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<TableField>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rows: Vec<TableRow>,
    /// 用户当前选区（未打开表格或无选区时缺省）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<TableSelection>,
    /// 其他协作者的当前选区（不含本端；未打开表格或无人时缺省）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remote_selections: Vec<TableRemoteSelection>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 表格选区形态（与宿主表格会话同构）：插件据此把视图对齐到用户正在看的行 / 格。
#[serde(tag = "kind", rename_all = "camelCase")]
#[serde(rename_all_fields = "camelCase")]
pub enum TableSelection {
    /// 单个单元格。
    Cell { row_id: String, field_id: String },
    /// 矩形区域：`anchor` 为拖选起点，另一组为当前端点。
    Range { anchor_row_id: String, anchor_field_id: String, row_id: String, field_id: String },
    /// 整行。
    Row { row_id: String },
    /// 整列。
    Column { field_id: String },
    /// 全表。
    All,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 快照里的远端协作者选区：显示名、身份色（`#rrggbb`）与范围。
#[serde(rename_all = "camelCase")]
pub struct TableRemoteSelection {
    pub name: String,
    pub color: String,
    pub selection: TableSelection,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 快照里的字段：id / 名称 / 类型 / 选项 / 列宽。
#[serde(rename_all = "camelCase")]
pub struct TableField {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub field_type: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    /// 用户拖拽调整后的列宽（px）；缺省 = 宿主按字段名自适应。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 快照里的行：单元格值按字段 id 存（缺 key = 空单元格）。
#[serde(rename_all = "camelCase")]
pub struct TableRow {
    pub id: String,
    #[serde(default)]
    pub values: serde_json::Map<String, Value>,
    /// 单元格显示样式按字段 id 存（粗体 / 斜体 / 删除线 / 字色 / 底色 / 字号等，形态随宿主
    /// 表格格式模型）。原样透传：宿主不解释内容，插件按需取用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub styles: Option<serde_json::Map<String, Value>>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`table::UPDATE_CELL`] 的 params：`value` 为单元格新值（`null` = 清空；形态随字段类型）。
#[serde(rename_all = "camelCase")]
pub struct TableUpdateCellParams {
    pub row_id: String,
    pub field_id: String,
    pub value: Value,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`table::ADD_ROW`] 的 result：新行 id（宿主分配，插件按它继续引用该行）。
#[serde(rename_all = "camelCase")]
pub struct TableAddRowResult {
    pub row_id: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`table::REMOVE_ROW`] 与 [`table::SELECT_ROW`] 的 params。
#[serde(rename_all = "camelCase")]
pub struct TableRowIdParams {
    pub row_id: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`table::SET_CELL_IMAGE`] 的 params：整格替换图片引用列表（`images` 为仓库相对路径，
/// 须先经 `vault.writeBinary` 落盘）。`display` 为显示形态（如 `grid`），缺省保持宿主默认。
#[serde(rename_all = "camelCase")]
pub struct TableSetCellImageParams {
    pub row_id: String,
    pub field_id: String,
    pub images: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`crate::event::name::TABLE_COMMAND`] 的 payload：宿主按表格作用域拦截的快捷键命令。
#[serde(rename_all = "camelCase")]
pub struct TableCommandParams {
    /// `undo` / `redo` / `copy` / `cut` / `paste` / `select-all` / `delete` / `save`。
    pub command: String,
}
```

## 14. 能力面负载：画布

画布面读写当前打开的画布：内容真源是磁盘上的 `.atlx` 文件，插件写盘经宿主 `vault.writeText`。`canvas.snapshot` 由预置画布插件提供，返回选中卡属性与统计，属性面板据此渲染。

```rust
// atelyx-plugin-protocol/src/method.rs
/// 画布面：读取当前打开画布的结构化快照（选中卡属性 / 画布统计）。方法由画布插件提供，
/// 属性面板等第三方视图经它与画布数据同源。
pub mod canvas {
    /// 读当前打开画布的只读快照；未打开画布 = `path: None` 与全零统计。
    pub const SNAPSHOT: &str = "canvas.snapshot";
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 画布选中卡的引用加载状态。
#[serde(rename_all = "camelCase")]
pub enum CanvasRefStatus {
    /// 已加载（文本 / 图片 / 表格可见）。
    Loaded,
    /// 读到字节但无法按文本呈现。
    Binary,
    /// 引用的文件不存在或读取失败。
    Missing,
    /// 引用内容仍在读取。
    Loading,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 选中卡的一条连线。
#[serde(rename_all = "camelCase")]
pub struct CanvasEdgeInfo {
    /// `association` / `reference-consumed` / `reference-unconsumed` / `lineage` / `flow`。
    pub role: String,
    /// `out` = 选中卡是源；`in` = 选中卡是目标。
    pub direction: String,
    /// 对端卡标题。
    pub peer_title: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 画布选中卡的属性快照（属性面板数据源）。
#[serde(rename_all = "camelCase")]
pub struct CanvasSelectionInfo {
    pub title: String,
    /// 节点类型原文（`conversation` / `text` / `table` / `media` / `link` / 分组等）。
    pub node_type: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    /// 卡引用的仓库相对路径。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// 引用内容状态；无引用文件 = 缺省。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_status: Option<CanvasRefStatus>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edges: Vec<CanvasEdgeInfo>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 画布统计（无选中时属性面板展示）。
#[serde(rename_all = "camelCase")]
pub struct CanvasStats {
    pub cards: usize,
    pub groups: usize,
    pub edges: usize,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`canvas::SNAPSHOT`] 的 result。
#[serde(rename_all = "camelCase")]
pub struct CanvasSnapshotResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default)]
    pub dirty: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<CanvasSelectionInfo>,
    #[serde(default)]
    pub stats: CanvasStats,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`crate::event::name::CANVAS_COMMAND`] 的 payload：宿主按画布作用域拦截的快捷键命令。
#[serde(rename_all = "camelCase")]
pub struct CanvasCommandParams {
    /// `undo` / `redo` / `copy` / `paste` / `select-all` / `delete` / `escape`。
    pub command: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 历史回滚把某个版本交回当前画布（`canvas:restore` 的 payload）。
#[serde(rename_all = "camelCase")]
pub struct CanvasContentParams {
    /// 目标画布的仓库相对路径（订阅方按自己打开的文件匹配）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// `.atlx` 全文（JSON 文本）。
    pub content: String,
}
```

## 15. 日历服务（插件提供）

日程读写由预置插件 `com.atelyx.calendar` 以 `calendar` 服务开放，宿主不内置实现：调用方经 `service.call` 传 `service = calendar` 与下列方法名，或直接调 `calendar.<方法>`——两条路径落到同一登记。插件停用时该服务随之消失，调用方收到「服务不可用」。日程真源随容器走（本地仓库 `.atelyx/calendar.json` / 协作空间团队 meta）。

```rust
// atelyx-plugin-protocol/src/method.rs
/// 日历服务（由日历插件提供）：方法名即 `calendar.<方法>`，也可经 `service.call` 以
/// `service = calendar` 调用。日程真源随容器走（本地仓库 / 协作空间团队 meta），插件不自留副本。
pub mod calendar {
    /// 服务名。
    pub const SERVICE: &str = "calendar";
    /// 当前仓库的日程（按日期、创建时间与 id 排序）。
    pub const LIST: &str = "list";
    /// 新增一条日程；`id` 与创建时刻由服务补齐。
    pub const ADD: &str = "add";
    /// 改写一条日程（只改给出的字段）。
    pub const UPDATE: &str = "update";
    /// 删除一条日程。
    pub const REMOVE: &str = "remove";
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 一条日程（`date` 为本地日期 `YYYY-MM-DD`；`color` 为 hex，缺省 = 默认强调色）。
#[serde(rename_all = "camelCase")]
pub struct CalendarItem {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub date: String,
    #[serde(default)]
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// 创建时刻（Unix 毫秒），同日条目按它排序。
    #[serde(default)]
    pub created_at: i64,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`calendar::LIST`] 的 result：当前仓库的日程（按开始日期排序）。
#[serde(rename_all = "camelCase")]
pub struct CalendarListResult {
    pub items: Vec<CalendarItem>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`calendar::ADD`] 的 params：`id` / `createdAt` 由日历服务补齐；`color` 未给出即按强调色显示。
#[serde(rename_all = "camelCase")]
pub struct CalendarAddParams {
    pub date: String,
    #[serde(default)]
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`calendar::UPDATE`] 的 params：只改给出的字段（`None` = 保持原值；空串清空备注）。
#[serde(rename_all = "camelCase")]
pub struct CalendarUpdateParams {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`calendar::REMOVE`] 的 params。
#[serde(rename_all = "camelCase")]
pub struct CalendarIdParams {
    pub id: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 标题字符上限（超限按可读错误拒绝）。
pub const MAX_CALENDAR_TITLE_CHARS: usize = 200;
```

## 16. 能力面负载：仓库活动、版本分组与最近打开

按天聚合的文件活动计数与当日版本记录按本地日界归日；版本分组按文件聚合、组按最近版本时间新→旧。都取自当前仓库（本地历史侧文件 / 空间聚合）。最近打开清单由宿主按容器身份记账，记录与失效剔除都不在插件侧。

```rust
// atelyx-plugin-protocol/src/method.rs
/// 一天的活动计数（`day` = 自纪元天数）。
#[serde(rename_all = "camelCase")]
pub struct DayActivity {
    pub day: i64,
    pub count: u32,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`history::ACTIVITY`] 的 result：按天升序的活动计数。
#[serde(rename_all = "camelCase")]
pub struct HistoryActivityResult {
    pub days: Vec<DayActivity>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`history::DAY`] 的 params：`day` 为本地日期 `YYYY-MM-DD`。
#[serde(rename_all = "camelCase")]
pub struct HistoryDayParams {
    pub day: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 一条当日版本记录：`kind` / `action` 取宿主历史侧文件的取值域（`note` / `canvas` / `table`、
/// `edit` / `restore`）；`file` 为仓库相对路径，`ts` 为毫秒时间戳。
#[serde(rename_all = "camelCase")]
pub struct HistoryDayEntry {
    pub kind: String,
    pub file: String,
    pub ts: i64,
    pub action: String,
    pub author_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`history::DAY`] 的 result：按时间新→旧排列。
#[serde(rename_all = "camelCase")]
pub struct HistoryDayResult {
    pub entries: Vec<HistoryDayEntry>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`history::GROUPS`] 的 params：`refresh` = 丢弃宿主聚合缓存后重扫（手动刷新 / 重试用）。
#[serde(rename_all = "camelCase", default)]
pub struct HistoryGroupsParams {
    pub refresh: bool,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 一个文件的历史分组：`kind` 取 [`HISTORY_KINDS`] 的取值域，`count` = 版本条数。
/// 版本明细不在聚合里给出——浏览与回滚走 [`history::OPEN`] 的宿主弹层。
#[serde(rename_all = "camelCase")]
pub struct HistoryFileGroup {
    pub kind: String,
    pub file: String,
    pub count: u32,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`history::GROUPS`] 的 result：`ready` = 当前容器可用（false 时 `groups` 为空）。
#[serde(rename_all = "camelCase")]
pub struct HistoryGroupsResult {
    pub ready: bool,
    pub groups: Vec<HistoryFileGroup>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`HistoryRecordParams`] 的 kind 与 action 取值域见 [`HISTORY_KINDS`]。
#[serde(rename_all = "camelCase")]
pub struct HistoryRecordParams {
    /// [`HISTORY_KINDS`] 的取值。
    pub kind: String,
    /// 仓库相对路径。
    pub file: String,
    /// 该版本的全文内容。
    pub content: String,
    /// `edit` / `restore`，缺省 `edit`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// 编辑合并窗口（毫秒）：同窗口内的连续编辑并成一版。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coalesce_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`crate::event::name::TABLE_RESTORE`] 的 payload：历史回滚交回当前表格的全文快照。
#[serde(rename_all = "camelCase")]
pub struct TableContentParams {
    /// 目标表格的仓库相对路径（订阅方按自己打开的文件匹配）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// `.atb` 全文（JSON 文本）。
    pub content: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 历史弹层支持的文件类型。
pub const HISTORY_KINDS: &[&str] = &["note", "canvas", "table"];
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`history::OPEN`] 的 params：打开某文件的宿主历史弹层。
#[serde(rename_all = "camelCase")]
pub struct HistoryOpenParams {
    pub kind: String,
    pub file: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 最近打开条目的文件类型。
#[serde(rename_all = "lowercase")]
pub enum RecentKind {
    Note,
    Table,
    Canvas,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 一条最近打开记录：仓库相对路径 + 类型 + 打开时刻（unix 秒）。
#[serde(rename_all = "camelCase")]
pub struct RecentEntry {
    pub file: String,
    pub kind: RecentKind,
    pub opened_at: i64,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`recent::LIST`] 的 result：`ready` = 当前容器可用（false 时 `entries` 为空）。
#[serde(rename_all = "camelCase")]
pub struct RecentListResult {
    pub ready: bool,
    pub entries: Vec<RecentEntry>,
}
```

## 17. 能力面负载：托盘与协作

系统托盘条目与空间内插件间消息的负载形态；两类能力的可用性都随宿主平台与容器类型而变。

```rust
// atelyx-plugin-protocol/src/method.rs
/// 托盘菜单条目：`id` 插件内唯一、非空，用户点击时原样随 [`tray::HOST_CLICK`] 回发。
#[serde(rename_all = "camelCase")]
pub struct TrayItemSpec {
    pub id: String,
    /// 条目文字（非空）。
    pub label: String,
    /// 置灰不可点（缺省 true）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`tray::SET_ITEMS`] 的 params：整发整换本插件的托盘菜单，空清单 = 收起。
#[serde(rename_all = "camelCase")]
pub struct SetTrayItemsParams {
    pub items: Vec<TrayItemSpec>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`tray::HOST_CLICK`] 的 params：用户点击的条目 id。
#[serde(rename_all = "camelCase")]
pub struct TrayClickParams {
    pub id: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 同空间的一个在场成员。
#[serde(rename_all = "camelCase")]
pub struct CollabPeer {
    pub peer_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`collab::PEERS`] 的 result：`connected` = 当前空间连接可用；未连接时 `selfId` 与 `peers` 缺省。
/// `peers` 不含自己，宿主已剔除。
#[serde(rename_all = "camelCase")]
pub struct CollabPeersResult {
    pub connected: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub self_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub peers: Vec<CollabPeer>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 协作消息投递语义。`reliable` = 广播帧按房间序号缓存、重连后可补投（内容类增量）；
/// `ephemeral` = 不缓存、不补投、不占序号（光标 / 选中这类随时可被新状态覆盖的在场数据）。
#[serde(rename_all = "camelCase")]
pub enum CollabSendQos {
    #[default]
    Reliable,
    Ephemeral,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`collab::SEND`] 的 params：`channel` 为插件自定义的通道名（非空、不含 `:`、≤ 64 字符），
/// 宿主在投递时加插件前缀；`payload` 任意 JSON。`targetPeerId` 取 [`CollabPeersResult`] 里的
/// `peerId` 原值，缺省 = 广播给全部对端。
#[serde(rename_all = "camelCase")]
pub struct CollabSendParams {
    pub channel: String,
    pub payload: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_peer_id: Option<String>,
    /// 投递语义，缺省 = `reliable`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qos: Option<CollabSendQos>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`collab::SEND`] 的 result：`queued` = 消息已交本机空间连接发出。
/// 发出不等于送达：广播不确认，单播目标离线时被服务端丢弃，`queued` 仍为 true；
/// 未连接空间时 `queued` = false。
#[serde(rename_all = "camelCase")]
pub struct CollabSendResult {
    pub queued: bool,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`collab::HOST_MESSAGE`] 的 params：`channel` 为发送方给的通道名（不含插件前缀），
/// `peerId` / `peerName` 标识发送方。
#[serde(rename_all = "camelCase")]
pub struct CollabMessageParams {
    pub channel: String,
    pub payload: Value,
    pub peer_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_name: Option<String>,
    /// 空间名册里该成员的身份色（`#RRGGBB`）；由宿主填，插件不再自行哈希。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_color: Option<String>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 团队元数据键的字节上限。
pub const MAX_META_KEY_BYTES: usize = 200;
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 团队元数据单键值的字节上限（与服务端默认单键上限同口径）。
pub const MAX_META_VALUE_BYTES: usize = 10 * 1024 * 1024;
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`collab::META_GET`] 的 params：`key` 缺省 = 读全部键。
#[serde(rename_all = "camelCase")]
pub struct CollabMetaGetParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 团队元数据快照：键 → UTF-8 文本值（键按字典序，读全量时输出稳定）。
#[serde(rename_all = "camelCase")]
pub struct CollabMetaResult {
    pub values: std::collections::BTreeMap<String, String>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 一次按键写入。
#[serde(rename_all = "camelCase")]
pub struct CollabMetaEntry {
    pub key: String,
    pub value: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`collab::META_PATCH`] 的 params：只写给出的键，未列出的键保持原值。
#[serde(rename_all = "camelCase")]
pub struct CollabMetaPatchParams {
    pub entries: Vec<CollabMetaEntry>,
}
```

## 18. 能力面负载：协作房间（空间事实）

房间这类「当前是不是空间、谁在线、成员有哪些」的界面用。容器、连接态、名册与各端打开的文件都由宿主给：插件不连服务端，名册缓存与宿主的成员管理共用；同名同设备的多条连接由调用方按 `device` 合并。

```rust
// atelyx-plugin-protocol/src/method.rs
/// 当前容器的种类（决定房间展示的是空间名册还是「未进入协作空间」）。
#[serde(rename_all = "camelCase")]
pub enum SpaceContainerKind {
    #[default]
    Local,
    Space,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`space::ROOM`] 的 result：容器种类、连接态、本端身份与在线设备明细。
/// `peers` 不含自己；同一身份的多条连接由订阅方按「昵称 + 设备」合并。
#[serde(rename_all = "camelCase")]
pub struct SpaceRoomResult {
    pub kind: SpaceContainerKind,
    pub connected: bool,
    /// 本端在当前容器是否可写（协作空间 viewer 只读；本地仓库恒可写）。
    /// 插件据此提前禁用编辑入口，服务端 403 仍为最终兜底。
    pub can_write: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub self_peer_id: Option<String>,
    /// 本端身份（协作空间才有；本地仓库 = 缺省）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub self_identity: Option<SpaceSelfIdentity>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub peers: Vec<SpacePeer>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 本端在线设备的展示资料（名字 / 成员色 / 平台 / 版本 / 打开的文件）。
#[serde(rename_all = "camelCase")]
pub struct SpaceSelfIdentity {
    pub name: String,
    /// 成员色（`#rrggbb`）。
    pub color: String,
    /// 平台标识（与 `std::env::consts::OS` 同词表）。
    pub platform: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<SpacePeerFile>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 同空间的一个在线设备。
#[serde(rename_all = "camelCase")]
pub struct SpacePeer {
    pub peer_id: String,
    pub nickname: String,
    /// 设备名（「昵称 + 设备」是合并键）。
    pub device: String,
    /// 成员色（`#rrggbb`）。
    pub color: String,
    /// 平台标识（旧端缺省）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    /// 客户端版本（旧端缺省）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<SpacePeerFile>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 一个打开中的文件：仓库相对路径 + 行图标键（宿主注册表口径）。
#[serde(rename_all = "camelCase")]
pub struct SpacePeerFile {
    pub path: String,
    pub icon: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`space::MEMBERS`] 的 result：名册缓存快照。`ready` 区分「缓存里有名册（可能为空）」
/// 与「宿主还没拉到」，两者都会给 `loading` 与 `error`。
#[serde(rename_all = "camelCase")]
pub struct SpaceMembersResult {
    /// 缓存里已有名册（`members` 可信）；false = 尚未拉到，展示方按「加载中 / 暂无名册」处理。
    pub ready: bool,
    pub loading: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<SpaceMember>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`space::FILE_PATCH`] 的 params：本端「上次落盘基线」与「冻结快照」两份全文，
/// 宿主按 `kind` 求实体差异交服务端仲裁。
#[serde(rename_all = "camelCase")]
pub struct SpaceFilePatchParams {
    /// 仓库相对路径。
    pub path: String,
    /// 文件族：`table` / `canvas`——决定服务端补丁端点与历史类型。
    pub kind: String,
    /// 上次成功保存的全文（补丁合并的基线）。
    pub baseline: String,
    /// 本端当前全文（冻结快照）。
    pub frozen: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`space::FILE_PATCH`] 的 result：服务端落地后的路径（补丁带标题时服务端按标题改名）。
#[serde(rename_all = "camelCase")]
pub struct SpaceFilePatchResult {
    pub path: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`crate::event::name::SPACE_FILE_PATCH`] 的 payload：别的成员改动了本端打开的空间文件。
#[serde(rename_all = "camelCase")]
pub struct SpaceFilePatchPayload {
    pub path: String,
    /// 文件族（`table`），决定 `patch` 的形态。
    pub kind: String,
    pub patch: Value,
    /// 服务端是否已落盘（true = 已仲裁落地；false = 转发帧）。
    pub landed: bool,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 空间内的一名成员。
#[serde(rename_all = "camelCase")]
pub struct SpaceMember {
    pub username: String,
    pub display_name: String,
    pub role: SpaceMemberRole,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 成员角色（命名与 `SpaceRole` 的 wire 形态一致）。
#[serde(rename_all = "camelCase")]
pub enum SpaceMemberRole {
    Owner,
    Editor,
    Viewer,
}
```

## 19. 能力面负载：面板头部与落盘握手

面板顶部那一行工具条由宿主渲染：插件只声明状态胶囊、错误文本、胶囊列表、右侧文本与菜单项，菜单点击按条目 id 回给插件；宿主的脏拦截需要落盘时按 `token` 请求插件立即保存。

```rust
// atelyx-plugin-protocol/src/method.rs
/// 一条面板头部状态：宿主按当前主题画成面板顶部那一行工具条。
#[serde(rename_all = "camelCase")]
pub struct PanelHeaderState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<HeaderStatus>,
    /// 错误文本（红色，过长由宿主截断）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pills: Vec<HeaderPill>,
    /// 右侧依次排列的文本（缩放百分比 / 行列统计这类）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub texts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<HeaderItem>,
    /// 显示「视图」下拉时给出贡献槽（当前只有 `tableView`）；缺省不显示下拉。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view_slot: Option<ContributionSlot>,
    /// 本面板有未落盘改动：宿主的脏拦截据此先要求插件 flush 再放行动作。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dirty: bool,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`panel::HEADER`] 的 params：`header` 缺省 = 收起头部。
#[serde(rename_all = "camelCase")]
pub struct PanelHeaderParams {
    /// 面板视图 id（壳侧视图 id，例如表格面板的 `table`）。
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<PanelHeaderState>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 头部左侧的状态胶囊（未保存 / 保存失败这类）。
#[serde(rename_all = "camelCase")]
pub struct HeaderStatus {
    pub text: String,
    #[serde(default)]
    pub tone: HeaderTone,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 头部状态胶囊的色调。
#[serde(rename_all = "camelCase")]
pub enum HeaderTone {
    #[default]
    Normal,
    /// 有待落盘改动（琥珀色）。
    Pending,
    /// 失败（红色）。
    Danger,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 头部右侧的协作者胶囊（`color` 为 `#rrggbb` 身份色）。
#[serde(rename_all = "camelCase")]
pub struct HeaderPill {
    pub text: String,
    pub color: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 头部「⋯」菜单的一项：点击经 [`crate::event::name::PANEL_HEADER_ACTION`] 回发 `id`。
#[serde(rename_all = "camelCase")]
pub struct HeaderItem {
    pub id: String,
    pub label: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`crate::event::name::PANEL_FLUSH`] 的 payload：宿主请插件立即落盘，`token` 原样随
/// [`panel::FLUSH_DONE`] 回传。
#[serde(rename_all = "camelCase")]
pub struct PanelFlushParams {
    pub target: String,
    pub token: u64,
    /// 文档会话身份（仅文档面携带；贯穿提交正文协议后再回执）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_seq: Option<u64>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`panel::FLUSH_DONE`] 的 params。
#[serde(rename_all = "camelCase")]
pub struct PanelFlushDoneParams {
    pub token: u64,
    /// 宿主发起请求时给的面板视图 id：回执原样带回，宿主据此确认是哪个面板。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// 文档会话身份原样带回；宿主只认与请求相同的会话。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_seq: Option<u64>,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`crate::event::name::PANEL_HEADER_ACTION`] 的 payload：用户点了头部菜单的某一项。
#[serde(rename_all = "camelCase")]
pub struct PanelHeaderActionParams {
    pub target: String,
    pub item: String,
}
```

## 20. 宿主广播事件

插件经 `event.subscribe` 声明订阅的事件名与载荷；未上表的名字宿主拒绝订阅。

```rust
// atelyx-plugin-protocol/src/event.rs
/// 方法常量：插件订阅请求、插件发布域事件与宿主广播通知。
pub const SUBSCRIBE: &str = "event.subscribe";
```

```rust
// atelyx-plugin-protocol/src/event.rs
/// 插件发布一条它自己拥有的域事件（例如表格插件发布 `table:changed`）：宿主校验事件名与
/// 归属后按普通广播发给订阅方。
pub const EMIT: &str = "event.emit";
```

```rust
// atelyx-plugin-protocol/src/event.rs
/// [`EMIT`] 的 params：事件名 + 按该事件名定型的载荷。
#[serde(rename_all = "camelCase")]
pub struct EventEmitParams {
    pub name: String,
    pub payload: serde_json::Value,
}
```

```rust
// atelyx-plugin-protocol/src/event.rs
pub const HOST_PUBLISH: &str = "host:event";
```

```rust
// atelyx-plugin-protocol/src/event.rs
/// 事件名常量。订阅不存在的名字宿主回 `RpcError::FAILED`。
pub mod name {
    /// 当前仓库容器切换（本地仓库 ↔ 协作空间，或换仓库）。payload 见 [`VaultSwitchPayload`]。
    /// 未声明 `keepMountedOnVaultSwitch` 的插件收到它之前就已被宿主重载。
    pub const VAULT_SWITCH: &str = "vault:switch";
    /// 当前打开表格的内容变更（打开 / 切换 / 关闭表格、本地编辑结算、协作远端补丁落地）。
    /// payload 见 [`TableChangedPayload`]。
    pub const TABLE_CHANGED: &str = "table:changed";
    /// 当前打开表格的选区变更（本端点选 / 拖选 / 键盘移动，协作者进入 / 移动 / 离场）。
    /// 内容不变、只有选区移动时也发。payload 见 [`TableSelectionChangedPayload`]。
    pub const TABLE_SELECTION_CHANGED: &str = "table:selection.changed";
    /// 历史回滚把某个版本交回当前画布：payload 见 [`crate::method::CanvasContentParams`]
    /// （订阅方按自己打开的路径匹配；`content` 是 `.atlx` 全文）。
    pub const CANVAS_RESTORE: &str = "canvas:restore";
    /// 画布内容已结算（本插件落盘 / 关闭）：payload 见 [`CanvasChangedPayload`]。
    pub const CANVAS_CHANGED: &str = "canvas:changed";
    /// 宿主拦截的画布快捷键命令（撤销 / 复制 / 粘贴等，按用户自定义键位触发）：
    /// payload 见 [`crate::method::CanvasCommandParams`]。
    pub const CANVAS_COMMAND: &str = "canvas:command";
    /// 宿主拦截的表格快捷键命令（撤销 / 复制 / 保存等，非编辑态按用户自定义键位触发）：
    /// payload 见 [`crate::method::TableCommandParams`]。
    pub const TABLE_COMMAND: &str = "table:command";
    /// 画布是否正展示在前台（后台标签页不算）：payload 见 [`CanvasVisiblePayload`]。
    pub const CANVAS_VISIBLE: &str = "canvas:visible";
    /// 会话语义变更（流式增量不广播）。payload 见 [`ChatChangedPayload`]。
    pub const AI_CHAT_CHANGED: &str = "ai:chat.changed";
    /// 会话真源整体重载完成（换仓 / 空间装载），订阅方应整体重取。
    /// payload 见 [`ChatReloadedPayload`]。
    pub const AI_CHAT_RELOAD: &str = "ai:chat.reload";
    /// 会话落盘状态变化（失败置位 / 恢复）。payload 见 [`ChatPersistChangedPayload`]。
    pub const AI_CHAT_PERSIST: &str = "ai:chat.persist";
    /// 仓库内文件变更（宿主保存 / 新建 / 删除 / 改名 / 插件经 `vault` 写入）。只覆盖宿主发起的
    /// 写入，外部程序直接改磁盘不发。payload 见 [`FileChangedPayload`]。
    pub const VAULT_FILE_CHANGED: &str = "vault:file.changed";
    /// 当前仓库的可检索文件清单重建（进 / 换仓库、宿主刷新文件清单时）。payload 为空对象；
    /// 订阅方重取 `vault.files` 拿整份清单（条数与顺序都随重建变化）。插件经 `vault` 新建的
    /// 文件不在其列，要等下一次清单重建才出现。
    pub const VAULT_FILES_CHANGED: &str = "vault:files.changed";
    /// 协作空间团队元数据的某个键变更（任何成员经 `collab.metaPatch` 或宿主写入）。
    /// payload 见 [`SpaceMetaChangedPayload`]；订阅方按 `key` 过滤后重取该键。
    pub const SPACE_META_CHANGED: &str = "space:meta.changed";
    /// 空间的连接态 / 在线设备 / 各端打开文件变化（连接建立、名册刷新、在场更新、断开）。
    /// payload 为空对象；订阅方重取 `space.room`。
    pub const SPACE_PEERS_CHANGED: &str = "space:peers.changed";
    /// 空间成员名册缓存变化（拉取开始 / 成功 / 失败、离开空间清空）。payload 为空对象；
    /// 订阅方重取 `space.members`。
    pub const SPACE_MEMBERS_CHANGED: &str = "space:members.changed";
    /// 与服务端的实时频道重新接上（重连成功、补投完成）：payload 为空对象。
    /// 订阅方按自己的通道重发当前状态——断连期间的帧已丢失，宿主不代存内容。
    pub const SPACE_RESYNC: &str = "space:resync";
    /// 外部文件拖入窗口（宿主只转发绝对路径，读取由订阅方经 `fs` 自理）。
    /// payload 见 [`FileDropPayload`]；只投给指针所在面的插件，该面未挂载即不投递。
    pub const WINDOW_FILE_DROP: &str = "window:file.drop";
    /// 当前文档会话状态变化（打开 / 落盘状态 / 改名 / 回滚）；正文归内容所有者插件，
    /// 宿主只广播会话事实。payload 见 [`DocChangedPayload`]。
    pub const DOC_CHANGED: &str = "doc:changed";
    /// 历史回滚把某个版本交回当前文本文档：payload 见 [`DocRestorePayload`]
    /// （订阅方按自己的会话身份匹配；`content` 是整篇正文）。
    pub const DOC_RESTORE: &str = "doc:restore";
    /// 全局编辑偏好变更（软换行 / 页面内标题 / 正文宽度）。payload 同
    /// `editor.prefs` 的 result（[`crate::method::EditorPrefs`]）。
    pub const EDITOR_PREFS_CHANGED: &str = "editor:prefs.changed";
    /// 当前容器的最近打开清单变化（打开文件确认成功、失效条目被剔除）。payload 为空对象；
    /// 订阅方重取 `recent.list` 拿整份清单（条数与顺序都随记录变化）。
    pub const RECENT_CHANGED: &str = "recent:changed";
    /// 当前容器的历史版本集合变化（新版本记入侧文件、回滚落盘）。payload 为空对象；
    /// 订阅方重取 `history.groups` / `history.day`（宿主已丢弃聚合缓存）。
    pub const HISTORY_CHANGED: &str = "history:changed";
    /// 拖拽会话的落点状态（发起方收到）：落点 / 拒绝原因 / 悬停自动展开。
    /// payload 见 [`crate::method::DragStateParams`]。
    pub const DRAG_STATE: &str = "drag:state";
    /// 当前打开的文件集合变化（打开 / 切换 / 关闭笔记 / 表格 / 画布）。payload 为空对象；
    /// 订阅方重取 `editor.openFiles` 拿当前集合（笔记 / 表格 / 画布各一个，至多 3 个）。
    pub const EDITOR_OPEN_CHANGED: &str = "editor:open.changed";
    /// 历史回滚把某个版本交回当前表格：payload 见 [`crate::method::TableContentParams`]
    /// （订阅方按自己打开的路径匹配；`content` 是 `.atb` 全文）。
    pub const TABLE_RESTORE: &str = "table:restore";
    /// 别的成员改动了空间里的表格：payload 见 [`crate::method::SpaceFilePatchPayload`]；
    /// 订阅方按 `path` 与自己打开的文件匹配后合入。
    pub const SPACE_FILE_PATCH: &str = "space:file.patch";
    /// 宿主请面板插件立即落盘：payload 见 [`crate::method::PanelFlushParams`]，
    /// 结果经 `panel.flushDone` 按 `token` 回执。
    pub const PANEL_FLUSH: &str = "panel:flush";
    /// 用户点了面板头部声明的菜单项：payload 见 [`crate::method::PanelHeaderActionParams`]。
    pub const PANEL_HEADER_ACTION: &str = "panel:header.action";
}
```

```rust
// atelyx-plugin-protocol/src/event.rs
/// [`name::VAULT_SWITCH`] 的 payload（`kind` 与字段名均 camelCase，与协议其余类型同一口径）。
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum VaultSwitchPayload {
    Local { root: String },
    /// `can_write` = 当前身份在空间内可写（viewer 只读；写操作仍由服务端终审）。
    Space {
        space_id: String,
        #[serde(default)]
        can_write: bool,
    },
}
```

```rust
// atelyx-plugin-protocol/src/event.rs
/// [`name::TABLE_CHANGED`] 的 payload：当前打开表格的仓库相对路径（无打开表格 = 缺省）。
#[serde(rename_all = "camelCase", default)]
pub struct TableChangedPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}
```

```rust
// atelyx-plugin-protocol/src/event.rs
/// [`name::TABLE_SELECTION_CHANGED`] 的 payload：当前表格路径 + 本端与远端选区。
/// 无打开表格 = 全缺省。
#[serde(rename_all = "camelCase", default)]
pub struct TableSelectionChangedPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<crate::method::TableSelection>,
    /// 其他协作者的当前选区（不含本端）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remote_selections: Vec<crate::method::TableRemoteSelection>,
}
```

```rust
// atelyx-plugin-protocol/src/event.rs
/// [`name::CANVAS_CHANGED`] 的 payload：当前打开画布的路径 / 落盘状态 / 选中卡属性 / 统计。
/// 关闭画布时 `path` 缺省、`selection` 缺省、统计全零。
#[serde(rename_all = "camelCase")]
pub struct CanvasChangedPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default)]
    pub dirty: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<crate::method::CanvasSelectionInfo>,
    #[serde(default)]
    pub stats: crate::method::CanvasStats,
}
```

```rust
// atelyx-plugin-protocol/src/event.rs
/// [`name::CANVAS_VISIBLE`] 的 payload：画布是否展示在前台。
#[serde(rename_all = "camelCase")]
pub struct CanvasVisiblePayload {
    pub visible: bool,
}
```

```rust
// atelyx-plugin-protocol/src/event.rs
/// [`name::DOC_CHANGED`] 的 payload：会话路径与落盘状态。
/// 正文归内容所有者插件，宿主不转发副本；插件按 `(docId, openSeq)` 丢弃迟到通知。
#[serde(rename_all = "camelCase")]
pub struct DocChangedPayload {
    pub path: String,
    /// 文档会话身份；插件只认当前热文档的 `(docId, openSeq)`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_seq: Option<u64>,
    #[serde(default)]
    pub dirty: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub save_error: Option<String>,
}
```

```rust
// atelyx-plugin-protocol/src/event.rs
/// [`name::DOC_RESTORE`] 的 payload：宿主把历史版本正文交回内容所有者插件。
#[serde(rename_all = "camelCase")]
pub struct DocRestorePayload {
    /// 文档会话身份；插件只认当前热文档的 `(docId, openSeq)`。
    pub doc_id: String,
    pub open_seq: u64,
    /// 整篇正文（回滚目标版本的原文）。
    pub content: String,
}
```

```rust
// atelyx-plugin-protocol/src/event.rs
/// [`name::AI_CHAT_CHANGED`] 的 payload。
#[serde(rename_all = "camelCase")]
pub struct ChatChangedPayload {
    pub session_id: String,
    pub kind: ChatChangeKind,
}
```

```rust
// atelyx-plugin-protocol/src/event.rs
/// 会话变更种类：创建 / 消息列表变化 / 元数据变化 / 删除。
#[serde(rename_all = "camelCase")]
pub enum ChatChangeKind {
    Created,
    Messages,
    Meta,
    Deleted,
}
```

```rust
// atelyx-plugin-protocol/src/event.rs
/// [`name::VAULT_FILE_CHANGED`] 的 payload：仓库相对路径 + 变更种类（改名另带旧路径）。
#[serde(rename_all = "camelCase")]
pub struct FileChangedPayload {
    pub path: String,
    pub kind: FileChangeKind,
    /// 改名 / 移动前的路径（其余种类缺省）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
}
```

```rust
// atelyx-plugin-protocol/src/event.rs
/// 文件变更种类：新建 / 内容写入 / 删除 / 改名或移动。
#[serde(rename_all = "camelCase")]
pub enum FileChangeKind {
    Created,
    Updated,
    Deleted,
    Renamed,
}
```

```rust
// atelyx-plugin-protocol/src/event.rs
/// [`name::SPACE_META_CHANGED`] 的 payload：变更的键名（团队元数据单键）。
#[serde(rename_all = "camelCase")]
pub struct SpaceMetaChangedPayload {
    pub key: String,
}
```

```rust
// atelyx-plugin-protocol/src/event.rs
/// [`name::WINDOW_FILE_DROP`] 的 payload：本次拖入的绝对路径（一次事件可含多张图）与落点面。
#[serde(rename_all = "camelCase", default)]
pub struct FileDropPayload {
    pub paths: Vec<String>,
    /// 指针落点面板的活动视图贡献 id：同一插件有多个面时按它区分。
    pub surface_id: String,
}
```

```rust
// atelyx-plugin-protocol/src/event.rs
/// 宿主广播的通知 params（[`HOST_PUBLISH`]）。
#[serde(rename_all = "camelCase")]
pub struct PublishParams {
    pub name: String,
    /// 按事件名定型的 payload；未知名字宿主不广播。
    pub payload: serde_json::Value,
}
```

```rust
// atelyx-plugin-protocol/src/event.rs
/// 插件订阅请求（[`SUBSCRIBE`]）的 params 与结果。
#[serde(rename_all = "camelCase")]
pub struct SubscribeParams {
    pub name: String,
}
```

```rust
// atelyx-plugin-protocol/src/event.rs
#[serde(rename_all = "camelCase")]
pub struct SubscribeResult {
    pub subscribed: bool,
}
```
