# 插件 API 参考

Atelyx 插件协议的全部公开类型与方法名。概念、取舍与常见坑见 [插件开发](plugin-development.md)；这里只列契约本身。

> 本文件由 `cargo run -p xtask -- docs` 从 `atelyx-plugin-protocol/` 源码生成，请勿手改。
> 协议改动后重跑一次即可；忘记重跑时 `cargo test --workspace` 会失败。

## 1. 协议版本与清单

`plugin.json` 的结构、能力声明与宿主保留的清单文件名。`apiVersion` 必须与宿主相等，不等即拒绝装载。

```rust
// atelyx-plugin-protocol/src/lib.rs
/// API 版本。破坏性变更（方法/指令/事件语义不兼容）递增后，握手时双方不一致即失败。
/// 协议冻结为稳定契约前不递增本号（同版本发布，避免每次扩容都要求重建在装插件）。
pub const API_VERSION: u32 = 1;
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
    /// 协议版本，必须等于宿主的 [`crate::API_VERSION`]。
    pub api_version: u32,
    /// 最低宿主版本（语义化版本，如 `0.4.0`）；缺省 = 不限。宿主低于它即拒绝装载。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_host_version: Option<String>,
    pub entry: Entry,
    /// 能力声明清单：用于向用户公示与审计，不拦截调用——插件是原生进程，闸门拦住的是
    /// 守规矩的插件。宿主据此标出「用到但未声明」的调用。
    pub permissions: Vec<Permission>,
    /// 进程型插件的单实例约束（多窗口共享同一宿主运行时全局单例）。
    pub singleton: bool,
    /// 切换仓库容器（本地 ↔ 空间 / 换仓库）时保持挂载；false = 宿主重载插件进程。
    pub keep_mounted_on_vault_switch: bool,
    pub doc_type_registrations: Vec<DocTypeRegistration>,
}
```

```rust
// atelyx-plugin-protocol/src/manifest.rs
/// 按平台的可执行入口（相对插件目录的路径）。缺失 = 该平台不支持。
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
    Table,
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
/// 领域文档类型注册：扩展名、展示标签、图标名与承载文档内容的贡献 id。
#[serde(rename_all = "camelCase")]
pub struct DocTypeRegistration {
    /// 不带点的扩展名（如 `board`）。
    pub extension: String,
    pub label: String,
    /// 宿主内置形状名（未知名回落拼图）。
    pub icon: String,
    /// 承载该文档实例的贡献 id（插件须同时以 `view.register` 注册同名贡献，
    /// `slot` 为面板）；打开该扩展名文件时宿主分配 surfaceId 并通知插件提交整面帧。
    pub surface: String,
}
```

## 2. 握手与信封

插件启动后的第一个请求是 `initialize`；此后双向都是一行一个 JSON-RPC 2.0 信封（NDJSON）。

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
    /// 整面帧共享内存区域：插件按它映射，此后每次提交只走这条通路。
    pub frame_region: FrameRegionInfo,
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
/// 改名 / 删除常量会直接编译失败，目录不会悄悄漂移。服务随归属插件启停：停用笔记插件后
/// 别的插件调不到 `markdown`，除非另有外部提供者接管。
pub const BUILTIN_SERVICES: &[(&str, &str, &[&str])] = &[
    ("fs", PLATFORM_PLUGIN, &[fs::PRIVATE_DIR, fs::READ_TEXT, fs::WRITE_TEXT, fs::READ_BINARY, fs::WRITE_BINARY, fs::REMOVE, fs::EXISTS, fs::LIST]),
    ("process", PLATFORM_PLUGIN, &[process::SPAWN, process::WRITE_STDIN, process::CLOSE_STDIN, process::KILL]),
    ("vault", PLATFORM_PLUGIN, &[vault::READ_TEXT, vault::WRITE_TEXT, vault::READ_BINARY, vault::WRITE_BINARY, vault::IMPORT_FILE, vault::IMAGE_INFO, vault::LIST, vault::EXISTS, vault::REMOVE, vault::RENAME]),
    ("collab", PLATFORM_PLUGIN, &[collab::PEERS, collab::SEND]),
    ("table", "atelyx.tables", &[table::SNAPSHOT, table::UPDATE_CELL, table::ADD_ROW, table::REMOVE_ROW, table::SELECT_ROW, table::SET_CELL_IMAGE]),
    ("state", PLATFORM_PLUGIN, &[state::READ, state::WRITE]),
    ("notification", PLATFORM_PLUGIN, &[notification::SHOW]),
    ("tray", PLATFORM_PLUGIN, &[tray::SET_ITEMS]),
    ("shortcut", PLATFORM_PLUGIN, &[shortcut::REGISTER, shortcut::UNREGISTER, shortcut::QUERY]),
    ("clipboard", PLATFORM_PLUGIN, &[clipboard::WRITE_TEXT, clipboard::READ_TEXT, clipboard::READ_IMAGE, clipboard::WRITE_IMAGE]),
    ("dialog", PLATFORM_PLUGIN, &[dialog::PICK_FILE, dialog::PICK_DIRECTORY]),
    ("http", PLATFORM_PLUGIN, &[http::REQUEST]),
    // 工具名册与结果回报是宿主记账，不是可由外部提供者接管的能力，故不在目录里
    ("ai", "atelyx.ai", &[ai::CHAT_SEND, ai::CHAT_STOP, ai::CHAT_HISTORY, ai::MODELS, ai::AGENTS, ai::CHAT_RENAME, ai::CHAT_DELETE, ai::CHAT_COMPACT]),
    ("window", PLATFORM_PLUGIN, &[window::OPEN, window::CLOSE, window::UPDATE, window::SHOW, window::BEGIN_DRAG]),
    ("query", PLATFORM_PLUGIN, &[query::MEASURE, query::THEME]),
    ("markdown", "atelyx.notes", &[markdown::PARSE]),
    ("editor", "atelyx.notes", &[editor::SELECTION, editor::REPLACE_SELECTION]),
    ("calendar", "atelyx.calendar", &[calendar::LIST, calendar::ADD, calendar::UPDATE, calendar::REMOVE]),
    ("history", "atelyx.history", &[history::ACTIVITY]),
];
```

## 4. 绘制指令

插件把界面画成指令：一条指令一个形状或一段文字，坐标是面本地逻辑像素、左上原点。指令集封闭，`fill` 与 `stroke` 至少声明其一；命中由命中区（见下一节）表达，绘制指令本身不带交互语义。

```rust
// atelyx-plugin-protocol/src/paint.rs
/// 绘图指令。矩形 / 路径 / 圆：`fill` 与 `stroke` 至少声明其一，`stroke_width` 只在 `stroke`
/// 存在时生效（缺省 1.0）；线只有 `stroke`（字段名 `width`）；文字与图片没有描边概念。
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
        /// 可用宽度：给定即按该宽度换行，并作为 `align` 的对齐区域。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_width: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        align: Option<TextAlign>,
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
/// 字重三档。
#[serde(rename_all = "camelCase")]
pub enum TextWeight {
    Normal,
    Medium,
    SemiBold,
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
    /// 无底色 / 无边框。
    Transparent,
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
pub enum ImageSource {
    Private { path: String },
    Vault { path: String },
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
/// 帧率上限（帧声明的 `fps` 上限，也是宿主调度帧通知的最细节拍）。
pub const MAX_FPS: u16 = 120;
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 纯查询：不改界面、不产生帧。
pub mod query {
    /// 批量量算文字占位尺寸（排版按真实字宽摆放，不能按字符数估）。
    pub const MEASURE: &str = "query.measure";
    /// 取宿主主题表（颜色 / 字号档 / 圆角档），插件据此与宿主观感一致。
    pub const THEME: &str = "query.theme";
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// Markdown 解析：内容不变则结果不变，插件应按原文缓存，不必逐帧往返。
pub mod markdown {
    /// 把 Markdown 文本转成结构化块（与笔记同一套解析语义），排版与绘制仍归插件。
    /// 服务名 `markdown`、方法 `parse`，因此外部提供者可以整体替换解析实现。
    pub const PARSE: &str = "markdown.parse";
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_width: Option<f32>,
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
#[serde(rename_all = "camelCase")]
pub struct MeasureTextResult {
    pub width: f32,
    pub height: f32,
    pub lines: u32,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`query::THEME`] 的 result：颜色 / 字号档 / 圆角档（值随用户的主题与字号设置变化）。
#[serde(rename_all = "camelCase")]
pub struct ThemeResult {
    pub colors: ThemeColors,
    pub font_sizes: FontSizes,
    pub radii: Radii,
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

`frame.submitShared` 一帧即该面的完整界面（指令 + 命中区 + 输入声明），帧内容写进共享内存槽位，信封只带槽位坐标；宿主只按几何绘制并把原始输入经 `host:frame.input` 原样回发，插件按帧自行维护悬停 / 按下 / 焦点 / 滚动 / 文本编辑状态。

```rust
// atelyx-plugin-protocol/src/method.rs
/// 面帧：整个面交给插件自绘，一帧即当前完整界面。
pub mod frame {
    /// 插件提交一份整面帧：内容写在共享内存槽里，params 只带槽位定位与校验信息。整帧替换。
    pub const SUBMIT_SHARED: &str = "frame.submitShared";
    /// 帧超出当前槽容量时请求宿主换一份更大的共享内存区域（应答 = 新区域描述）。
    pub const REGION: &str = "frame.region";
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
/// 命中区 id 上限（字节）。
pub const MAX_HIT_ID_LEN: usize = 64;
```

```rust
// atelyx-plugin-protocol/src/frame.rs
/// `frame.submitShared` 的 params：帧内容写在共享内存的 `slot` 槽里，信封只带定位与校验信息。
/// 宿主按 `generation` 拒绝过期映射，按 `seq` / `len` 校验槽内内容是否仍是本次写入。
#[serde(rename_all = "camelCase")]
pub struct FrameSubmitSharedParams {
    pub surface_id: String,
    pub generation: u64,
    pub slot: u32,
    pub seq: u64,
    pub len: u32,
}
```

```rust
// atelyx-plugin-protocol/src/frame.rs
/// `frame.region` 的 params：帧超出当前槽容量时请求宿主换一份更大的区域。
#[serde(rename_all = "camelCase")]
pub struct FrameRegionRequest {
    /// 本次至少要能放下的帧字节数；实际容量按倍增取到不小于它的值。
    pub min_bytes: u32,
}
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

```rust
// atelyx-plugin-protocol/src/method.rs
/// 单次转换的源文本上限（字节）：超限按可读错误拒绝，插件应自行分段。
pub const MAX_MARKDOWN_BYTES: usize = 512 * 1024;
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`markdown::PARSE`] 的 params：交给宿主按笔记同一套解析语义转成结构化块。
/// 宿主只做解析（块与行内片段），排版与绘制仍归插件。
#[serde(rename_all = "camelCase")]
pub struct MarkdownParams {
    pub content: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`markdown::PARSE`] 的 result：按源序排列的块。
#[serde(rename_all = "camelCase")]
pub struct MarkdownResult {
    pub blocks: Vec<MarkdownBlock>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 块：`depth` 为嵌套层数（列表 / 引用内部块递增），由插件换算缩进。
/// 表格单元格只给纯文本（单元格行内样式不展开）。
#[serde(tag = "kind", rename_all = "camelCase")]
#[serde(rename_all_fields = "camelCase")]
pub enum MarkdownBlock {
    Heading {
        level: u8,
        depth: u32,
        spans: Vec<MarkdownSpan>,
    },
    Paragraph {
        depth: u32,
        spans: Vec<MarkdownSpan>,
    },
    /// 列表项：`index` 对有序列表取源里的序号（`3.` 就是 3），无序列表恒为 1；`ordered` 决定画圆点还是序号。
    ListItem {
        depth: u32,
        ordered: bool,
        index: u32,
        spans: Vec<MarkdownSpan>,
    },
    /// 引用段落：插件在左侧画竖条。
    Quote {
        depth: u32,
        spans: Vec<MarkdownSpan>,
    },
    Code {
        depth: u32,
        lang: String,
        content: String,
    },
    /// 表格：`rows[0]` 是表头行，其余为数据行；单元格只给纯文本。
    Table {
        depth: u32,
        rows: Vec<Vec<String>>,
    },
    Divider,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 行内片段：同一段文字里的样式差异（字重、等宽、链接目标）。
#[serde(rename_all = "camelCase")]
pub struct MarkdownSpan {
    pub text: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub strong: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub code: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}
```

## 6. 帧共享内存

握手应答给出区域描述（路径 / 代次 / 槽数 / 槽容量）：插件把编码后的帧写进一个槽位，再以`frame.submitShared` 报告槽位坐标。区域文件全部小端：64 字节区域头（魔数 `ATLXFRM\0` / 版本 / 槽数 /槽容量 / 代次 / 头长 / 槽步长），每槽 16 字节槽头（序号 u64、长度 u32）+ 槽容量。写入顺序是「序号置奇 →写长度与内容 → 序号置偶」，序号取非零偶数、每次 +2；宿主首尾序号一致才认这一帧，撕裂或错位的整帧丢弃。帧超出槽容量时用 `frame.region` 请求扩容，宿主换新文件并递增代次，旧代次的提交一律拒绝。

```rust
// atelyx-plugin-protocol/src/frame_region.rs
/// 一个共享内存区域的描述（握手应答与 `frame.region` 扩容应答同形）。
#[serde(rename_all = "camelCase")]
pub struct FrameRegionInfo {
    /// 区域文件绝对路径（宿主建在系统临时目录；插件按此映射）。
    pub path: String,
    /// 代次：每次扩容或新会话换新文件并递增；提交时原样带回，宿主据此拒绝过期映射。
    pub generation: u64,
    pub slot_count: u32,
    /// 每槽可写的最大帧字节数。
    pub slot_capacity: u32,
}
```

```rust
// atelyx-plugin-protocol/src/frame_region.rs
/// 区域头魔数（8 字节）。
pub const REGION_MAGIC: [u8; 8] = *b"ATLXFRM\0";
```

```rust
// atelyx-plugin-protocol/src/frame_region.rs
/// 区域布局版本：字段含义变化即递增，双端不一致按不可用报错。
pub const REGION_VERSION: u32 = 1;
```

```rust
// atelyx-plugin-protocol/src/frame_region.rs
/// 区域头长度。
pub const REGION_HEADER_SIZE: usize = 64;
```

```rust
// atelyx-plugin-protocol/src/frame_region.rs
/// 槽头长度。
pub const SLOT_HEADER_SIZE: usize = 16;
```

```rust
// atelyx-plugin-protocol/src/frame_region.rs
/// 槽数：写入端同时只占一槽，其余留作宿主读取与紧邻两次提交的隔离。
pub const SLOT_COUNT: u32 = 4;
```

```rust
// atelyx-plugin-protocol/src/frame_region.rs
/// 槽容量（字节）：帧只装绘制指令与图片来源，不装图片像素。
pub const SLOT_CAPACITY: u32 = 256 * 1024;
```

```rust
// atelyx-plugin-protocol/src/frame_region.rs
/// 槽容量上限：超出即报可读错误，不再扩容。
pub const MAX_SLOT_CAPACITY: u32 = 16 * 1024 * 1024;
```

```rust
// atelyx-plugin-protocol/src/frame_region.rs
/// 填区域头：调用方须已把文件扩到 [`FrameRegionInfo::byte_len`]，且槽位从全零起步（新建文件即全零，
/// 槽序号从 2 起递增，全零槽不会被误认）。
///
/// 布局（全部小端）：64 字节区域头（魔数 / 版本 / 槽数 / 槽容量 / 代次 / 头长 / 槽步长）→
/// 每槽 16 字节槽头（序号 u64、长度 u32）+ 定长槽容量。
///
/// 只写区域头，槽区不动；缓冲不足或槽数 / 槽容量越界即报错。
pub fn init_region(map: &mut [u8], info: &FrameRegionInfo) -> Result<(), String> {
    info.check_layout()?;
    let len = info.byte_len();
    if map.len() < len {
        return Err(format!("区域缓冲不足：需要 {len} 字节，实际 {}", map.len()));
    }
    map[..REGION_HEADER_SIZE].fill(0);
    map[MAGIC_OFFSET..MAGIC_OFFSET + REGION_MAGIC.len()].copy_from_slice(&REGION_MAGIC);
    write_u32(map, VERSION_OFFSET, REGION_VERSION);
    write_u32(map, SLOT_COUNT_OFFSET, info.slot_count);
    write_u32(map, SLOT_CAPACITY_OFFSET, info.slot_capacity);
    write_u64(map, GENERATION_OFFSET, info.generation);
    write_u32(map, HEADER_SIZE_OFFSET, REGION_HEADER_SIZE as u32);
    write_u32(map, SLOT_STRIDE_OFFSET, info.slot_stride() as u32);
    Ok(())
}
```

```rust
// atelyx-plugin-protocol/src/frame_region.rs
/// 校验区域头与描述一致（映射端开箱即用的一次把关）。
///
/// 缓冲不足、魔数 / 版本 / 代次不符或槽数与步长和描述不同即报错。
pub fn validate_region(map: &[u8], info: &FrameRegionInfo) -> Result<(), String> {
    info.check_layout()?;
    let len = info.byte_len();
    if map.len() < len {
        return Err(format!("区域文件不足：需要 {len} 字节，实际 {}", map.len()));
    }
    if map[MAGIC_OFFSET..MAGIC_OFFSET + REGION_MAGIC.len()] != REGION_MAGIC {
        return Err("区域魔数不符".into());
    }
    let version = read_u32(map, VERSION_OFFSET);
    if version != REGION_VERSION {
        return Err(format!("区域版本不符：文件 {version}，宿主 {REGION_VERSION}"));
    }
    let slot_count = read_u32(map, SLOT_COUNT_OFFSET);
    let capacity = read_u32(map, SLOT_CAPACITY_OFFSET);
    let generation = read_u64(map, GENERATION_OFFSET);
    let header_size = read_u32(map, HEADER_SIZE_OFFSET);
    let stride = read_u32(map, SLOT_STRIDE_OFFSET);
    if generation != info.generation {
        return Err(format!("区域代次不符：文件 {generation}，描述 {}", info.generation));
    }
    if slot_count != info.slot_count
        || capacity != info.slot_capacity
        || header_size as usize != REGION_HEADER_SIZE
        || stride as usize != info.slot_stride()
    {
        return Err(format!(
            "区域头与描述不一致：槽数 {slot_count}，槽容量 {capacity}，槽头 {header_size}，步长 {stride}"
        ));
    }
    Ok(())
}
```

```rust
// atelyx-plugin-protocol/src/frame_region.rs
/// 把一帧编码后的内容写进槽位：序号必须是大于零的偶数。
///
/// 区域头与描述不符、槽位越界、内容超出槽容量或序号非法即报错，槽内内容不动。
pub fn write_slot(
    map: &mut [u8],
    info: &FrameRegionInfo,
    slot: u32,
    seq: u64,
    payload: &[u8],
) -> Result<(), String> {
    validate_region(map, info)?;
    if seq == 0 || seq % 2 != 0 {
        return Err(format!("槽序号须为非零偶数：{seq}"));
    }
    let (header, content) =
        info.slot_offsets(slot).ok_or_else(|| format!("槽位越界：{slot}"))?;
    if payload.len() > info.slot_capacity as usize {
        return Err(format!(
            "帧超出槽容量：{} > {}",
            payload.len(),
            info.slot_capacity
        ));
    }
    // 先置奇数宣告写入中，再写长度与内容；释放栅栏确保读者看到偶数序号时内容已落地
    write_u64(map, header + SLOT_SEQ_OFFSET, seq - 1);
    fence(Ordering::SeqCst);
    write_u32(map, header + SLOT_LEN_OFFSET, payload.len() as u32);
    map[content..content + payload.len()].copy_from_slice(payload);
    fence(Ordering::Release);
    write_u64(map, header + SLOT_SEQ_OFFSET, seq);
    Ok(())
}
```

```rust
// atelyx-plugin-protocol/src/frame_region.rs
/// 读一槽内容：序号对得上且首尾一致才返回 `Some`。
///
/// 写入进行中 / 被后续写入覆盖返回 `None`（按错位丢弃）；区域头与描述不符、序号非法、槽位或
/// 长度越界、槽内长度与提交不符即报错。
pub fn read_slot(
    map: &[u8],
    info: &FrameRegionInfo,
    slot: u32,
    seq: u64,
    len: u32,
) -> Result<Option<Vec<u8>>, String> {
    validate_region(map, info)?;
    if seq == 0 || seq % 2 != 0 {
        return Err(format!("槽序号须为非零偶数：{seq}"));
    }
    let (header, content) =
        info.slot_offsets(slot).ok_or_else(|| format!("槽位越界：{slot}"))?;
    if len > info.slot_capacity {
        return Err(format!("帧长度越界：{len} > {}", info.slot_capacity));
    }
    let before = read_u64(map, header + SLOT_SEQ_OFFSET);
    if before != seq || before % 2 != 0 {
        return Ok(None);
    }
    fence(Ordering::Acquire);
    let stored_len = read_u32(map, header + SLOT_LEN_OFFSET);
    let payload = map[content..content + len as usize].to_vec();
    fence(Ordering::Acquire);
    let after = read_u64(map, header + SLOT_SEQ_OFFSET);
    if after != before {
        // 读的过程中被后续写入整槽覆盖：内容已不可信，按错位丢弃（不是插件错误）
        return Ok(None);
    }
    if stored_len != len {
        return Err(format!("槽内长度与提交不符：槽内 {stored_len}，提交 {len}"));
    }
    Ok(Some(payload))
}
```

```rust
// atelyx-plugin-protocol/src/frame_codec.rs
/// 把一帧编成字节序列。
pub fn encode(frame: &SurfaceFrame) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&MAGIC);
    out.push(VERSION);
    encode_frame(&mut out, frame);
    out
}
```

```rust
// atelyx-plugin-protocol/src/frame_codec.rs
/// 从字节序列还原一帧：魔数 / 版本 / 长度对不上或内容截断都返回可读错误。
pub fn decode(bytes: &[u8]) -> Result<SurfaceFrame, String> {
    if bytes.len() < MAGIC.len() + 1 {
        return Err("帧编码过短".into());
    }
    if bytes[..MAGIC.len()] != MAGIC {
        return Err("帧编码魔数不符".into());
    }
    if bytes[MAGIC.len()] != VERSION {
        return Err(format!("帧编码版本不符：{}", bytes[MAGIC.len()]));
    }
    let mut reader = Reader { buf: bytes, pos: MAGIC.len() + 1 };
    let frame = decode_frame(&mut reader)?;
    if reader.pos != bytes.len() {
        return Err(format!("帧编码尾部有多余字节：{}", bytes.len() - reader.pos));
    }
    Ok(frame)
}
```

## 7. 贡献点与窗口面

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
/// 界面提交类方法的回执（`frame.submitShared`）：`applied` = 宿主已接受并生效。
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
    /// 0 = 贴边，缺省 = 宿主默认。
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

## 8. 默认布局

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

## 9. 能力面方法名

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
    /// 插件运行时改一个已打开窗口的属性（尺寸 / 置顶；「不进任务栏」只能在开窗时声明）。
    pub const UPDATE: &str = "window.update";
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
    /// 删除一个仓库相对路径（文件或文件夹；文件夹连同内容递归删除）。仓库根不可删。
    /// 被编辑面打开或被当前画布节点引用的文件拒删。
    pub const REMOVE: &str = "vault.remove";
    /// 把一个仓库相对路径改名 / 移动到另一路径（文件或文件夹；目标不存在，父目录自动创建）。
    /// 引用同步：`.md` 内部链接与画布文件引用随改；源文件被编辑面打开或被当前画布节点引用时拒绝。
    pub const RENAME: &str = "vault.rename";
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 表格面：读写当前打开表格。写入与宿主自身编辑同一出口（宿主合并落盘并广播协作补丁），
/// 插件不得旁路读写 `.atb` 真源。
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
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
pub mod doc {
    /// 用户打开了插件注册的领域文档类型文件（通知）。`surfaceId` 为宿主分配的实例面标识，
    /// 插件据此以 `frame.submitShared` 提交该文档实例的界面。
    pub const HOST_OPEN: &str = "host:doc.open";
    /// 文档实例已关闭 / 已切换走（通知）。插件此后不得再向该 surfaceId 提交帧。
    pub const HOST_CLOSE: &str = "host:doc.close";
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 编辑器：当前打开笔记的选区读写（由笔记插件提供，其他插件据此做「改写选中内容」这类动作）。
/// 只在编辑状态有意义；阅读态选区不属于编辑器能力。
pub mod editor {
    /// 读当前文档路径与选区（无选区时 `from == to`、`text` 为空）。
    pub const SELECTION: &str = "editor.selection";
    /// 用给定文本替换当前选区（空串 = 删除选区）；写入经笔记同一套保存链。
    pub const REPLACE_SELECTION: &str = "editor.replaceSelection";
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
pub mod calendar {
    /// 当前仓库的日程（按日期、创建时间与 id 排序）。
    pub const LIST: &str = "calendar.list";
    /// 新增一条日程；缺省字段由宿主补齐。
    pub const ADD: &str = "calendar.add";
    /// 改写一条日程（只改给出的字段）。
    pub const UPDATE: &str = "calendar.update";
    /// 删除一条日程。
    pub const REMOVE: &str = "calendar.remove";
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
pub mod history {
    /// 按天聚合的仓库活动：新建 / 修改 / 删除的文件数合计。
    pub const ACTIVITY: &str = "history.activity";
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
/// 协作空间内的插件消息：按「通道」隔离，宿主把插件 id 作为通道前缀，
/// 同空间的同插件实例互通（跨空间不达、不可抢占他人通道）。
pub mod collab {
    /// 当前空间的连接状态与在场成员清单（不含自己）。
    pub const PEERS: &str = "collab.peers";
    /// 向同空间的对等插件发一条消息：缺省广播给全部对端，给了目标即单播。
    pub const SEND: &str = "collab.send";
    /// 对端消息到达（通知）。
    pub const HOST_MESSAGE: &str = "host:collab.message";
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
/// 信任模型与进程托管一致：插件本就是任意权限的用户进程，
/// 声明式权限是透明度机制而非沙箱边界；`fs` 服务的价值在统一出口与审计事实。
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

领域文档实例、AI 对话与工具桥的负载形态。

```rust
// atelyx-plugin-protocol/src/method.rs
#[serde(rename_all = "camelCase")]
pub struct DocOpenParams {
    /// 仓库容器内的相对路径（本地仓库）或服务端内容路径（协作空间），与 vault 口径一致。
    pub path: String,
    /// 宿主为本实例分配的面标识：插件据此以 `frame.submitShared` 提交该文档实例的界面。
    pub surface_id: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`doc::HOST_CLOSE`] 的 params：实例随标签关闭 / 换文件被关闭。
#[serde(rename_all = "camelCase")]
pub struct DocCloseParams {
    pub surface_id: String,
    pub path: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 单次替换文本的字节上限（超限按可读错误拒绝）。
pub const MAX_EDITOR_TEXT_BYTES: usize = 512 * 1024;
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`editor::SELECTION`] 的 result：当前文档与选区。
/// `path` 缺省 = 未打开笔记；`from` / `to` 是正文字节偏移（frontmatter 之后的部分）。
#[serde(rename_all = "camelCase")]
pub struct EditorSelection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub text: String,
    pub from: usize,
    pub to: usize,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`editor::REPLACE_SELECTION`] 的 params：替换文本（空串 = 删除当前选区）。
#[serde(rename_all = "camelCase")]
pub struct EditorReplaceParams {
    pub text: String,
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
    /// 仓库内图片相对路径（png / jpg / jpeg / webp / gif）；逐张校验，任一不可用 = 整条拒绝。
    /// 校验通过后到 run 读取之间文件可能消失：读不到的那张在该轮请求里跳过。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
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

## 12. 能力面负载：表格

表格面读写当前打开的表格；真源在宿主会话，插件不得旁路读写 `.atb`。

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

## 13. 能力面负载：日程与活动

日程读写与按天活动统计；两者都取自当前仓库（本地文件 / 空间团队 meta），与宿主视图同一真源。

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
/// [`calendar::ADD`] 的 params：`id` / `createdAt` 由宿主补齐；`color` 未给出即按强调色显示。
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

## 14. 能力面负载：托盘与协作

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
}
```

## 15. 宿主广播事件

插件经 `event.subscribe` 声明订阅的事件名与载荷；未上表的名字宿主拒绝订阅。

```rust
// atelyx-plugin-protocol/src/event.rs
/// 方法常量：插件订阅请求与宿主广播通知。
pub const SUBSCRIBE: &str = "event.subscribe";
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
    /// 会话语义变更（流式增量不广播）。payload 见 [`ChatChangedPayload`]。
    pub const AI_CHAT_CHANGED: &str = "ai:chat.changed";
    /// 仓库内文件变更（宿主保存 / 新建 / 删除 / 改名 / 插件经 `vault` 写入）。只覆盖宿主发起的
    /// 写入，外部程序直接改磁盘不发。payload 见 [`FileChangedPayload`]。
    pub const VAULT_FILE_CHANGED: &str = "vault:file.changed";
    /// 当前仓库的日程变更（视图编辑、插件经 `calendar.*` 写入，本地与协作空间同口径）。
    /// payload 为空对象：日程量小，订阅方重新 `calendar.list` 取最新清单。
    pub const CALENDAR_CHANGED: &str = "calendar:changed";
}
```

```rust
// atelyx-plugin-protocol/src/event.rs
/// [`name::VAULT_SWITCH`] 的 payload（`kind` 与字段名均 camelCase，与协议其余类型同一口径）。
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum VaultSwitchPayload {
    Local { root: String },
    Space { space_id: String },
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
