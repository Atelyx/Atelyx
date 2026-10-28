# 插件 API 参考

Atelyx 插件协议的全部公开类型与方法名。概念、取舍与常见坑见 [插件开发](plugin-development.md)；这里只列契约本身。

> 本文件由 `cargo run -p xtask -- docs` 从 `atelyx-plugin-protocol/` 源码生成，请勿手改。
> 协议改动后重跑一次即可；忘记重跑时 `cargo test --workspace` 会失败。

## 1. 协议版本与清单

`plugin.json` 的结构、能力声明与宿主保留的清单文件名。`apiVersion` 必须与宿主相等，不等即拒绝装载。

```rust
// atelyx-plugin-protocol/src/lib.rs
/// API 版本。破坏性变更（方法/控件/事件语义不兼容）递增后，握手时双方不一致即失败。
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
    /// 能力声明清单：未声明的能力调用一律被宿主拒绝（fail-closed）。
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
    /// 宿主内置形状名或经 `view.defineIcon` 定义的自备名（内置名优先）。
    pub icon: String,
    /// 承载该文档实例界面树的贡献 id（插件须同时以 `view.register` 注册同名贡献，
    /// `slot` 为面板）；打开该扩展名文件时宿主按文件实例分配 surfaceId 并通知插件提交树。
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

## 3. 界面控件树

插件提交的声明式控件树：容器带 `children`，叶子不带；`id` 在同一棵树内唯一，事件按节点 `events` 声明回发。

```rust
// atelyx-plugin-protocol/src/view.rs
/// 一棵界面树。`id` 在同一棵树内必须唯一（宿主 diff 与事件回发都按它定位）；
/// `children` 仅容器类节点可携带（见 [`NodeBody::allows_children`]），`events` 声明该节点要回发哪些事件。
pub struct ViewNode {
    pub id: String,
    #[serde(flatten)]
    pub body: NodeBody,
    /// 交互外观（见 [`Interaction`]）；缺省该节点只按自身控件渲染，不参与悬停 / 激活着色。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interaction: Option<Interaction>,
    /// 在父级行 / 列 / 网格里按权重分剩余空间（缺省不参与分配，按控件自身尺寸）。
    /// 与兄弟的权重之比即分得空间之比（35 / 65 即写 35 与 65）；权重须大于 0。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grow: Option<u16>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<ViewNode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<EventKind>,
}
```

```rust
// atelyx-plugin-protocol/src/view.rs
/// 节点体：`kind` 标签 + 各自的属性字段。字段全部带默认值，插件只写关心的项。
#[serde(tag = "kind", rename_all = "camelCase")]
#[serde(rename_all_fields = "camelCase")]
pub enum NodeBody {
    Row {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gap: Option<f32>,
        /// 内边距：数字 = 四边等距，也可按轴 / 按边声明。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        padding: Option<PaddingSpec>,
        /// 主轴分布：`spaceBetween` 在行占满宽度时把子项推向两端（工具行：左信息 + 右操作）。
        /// 缺省 `start`（子项紧贴左缘）；声明 `size.width` 为 `fill` 时仍生效。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        justify: Option<Justify>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cross_align: Option<Align>,
        /// 自身尺寸：缺省按内容收缩（有 `grow` 子项或声明 `justify` 时按占满宽度布局）；`width: fill` 即占满父容器。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<BoxSize>,
    },
    Column {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gap: Option<f32>,
        /// 内边距：数字 = 四边等距，也可按轴 / 按边声明。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        padding: Option<PaddingSpec>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cross_align: Option<Align>,
        /// 宽度不足父容器时的水平摆放（`center` 居中、`end` 右对齐）；缺省靠左。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        align: Option<Align>,
        /// 自身尺寸：缺省宽度占满父容器、高度按内容。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<BoxSize>,
    },
    /// 卡片容器：主题化背景 + 边框 + 圆角的分组盒（列表条目、信息分组）。
    /// 声明 `interaction` 且当前状态命中某个外观档时，底色与边框由该档提供，本控件自身不再绘制（单一底色来源）；
    /// 当前状态没有对应档位时仍按本控件的 `tone` / `accent` / `bordered` / `radius` 绘制。
    Card {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gap: Option<f32>,
        /// 内边距：数字 = 四边等距，也可按轴 / 按边声明。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        padding: Option<PaddingSpec>,
        /// 强调态（列表选中项）：accent 边框。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        accent: Option<bool>,
        /// 底色档位（缺省主题卡底）。插件只选主题档位，不写死色值。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tone: Option<CardTone>,
        /// 圆角半径（px）；缺省用宿主卡片圆角。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        radius: Option<f32>,
        /// 是否画边框；缺省 true。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bordered: Option<bool>,
        /// 宽度不足父容器时的水平摆放（`center` 居中、`end` 右对齐）；缺省靠左。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        align: Option<Align>,
        /// 自身尺寸：缺省宽度占满父容器、高度按内容。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<BoxSize>,
    },
    /// 叠层：子节点按提交顺序重叠（后提交的在上），同尺寸对齐于左上角。
    /// 用于「自绘画布当底 + 宿主控件叠其上」或徽标叠图。
    Stack {},
    /// 面内浮层（模态）：唯一子节点即浮层内容，宿主把它铺满整个插件面——一层遮罩 + 居中内容，
    /// 且不参与父容器排版（挂在任何一层效果相同，浮层之间按树序叠放）。
    /// 打开 = 节点出现在树里，关闭 = 从树里移除；遮罩拦下所有指针事件（按下 / 抬起 / 滚轮），
    /// 下层控件不响应点击也不滚动。声明 `click` 时点击遮罩（内容之外）回发 `Click`，
    /// 插件据此收起浮层；不声明则遮罩只拦截。浮层自身不参与排版与交互外观（`size` / `grow` /
    /// `interaction` 不生效，外观取 `scrim` 与内容节点），键盘与焦点不被改写。浮层不能作根节点。
    Overlay {
        /// 遮罩底色（缺省主题遮罩；`transparent` 不画底色，只保留事件拦截）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scrim: Option<ColorSpec>,
    },
    Scroll {
        /// 子节点按主轴单向排列：`horizontal` 排成行，其余（含缺省）排成列。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        direction: Option<Axis>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gap: Option<f32>,
        /// 交叉轴高度（px）：纵向滚动时收住自身高度，横向滚动时按该高度做图条；
        /// 缺省占满父容器剩余高度。节点声明 `scroll` 事件即上报滚动位置。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        height: Option<f32>,
        /// 贴底跟随：用户停在终点时，内容增高后自动跟到新的终点；用户往回滚即脱钩，
        /// 再滚回终点自动恢复。`view.scrollTo(end)` 也会恢复跟随（其它位置则脱钩）。
        /// 缺省 false。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        follow_end: Option<bool>,
    },
    /// 网格：`columns` 固定列数；`min_column_width` 按可用宽度换列（`columns` 同时给定时作列数上限）。
    /// 二者都缺省按单列渲染。`row_height` 缺省按列宽 1:1 定行高。
    Grid {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        columns: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min_column_width: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gap: Option<f32>,
        /// 行高：数字 = 固定逻辑像素，`content` = 按单元格内容高度（短内容行不被拉成方格）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        row_height: Option<RowHeight>,
    },
    /// 可滚动列表：语义等同纵向 scroll + column（宿主对子项按键复用控件态）。
    /// 位置可用 `view.scrollTo` 指令控制；声明 `scroll` 事件即上报滚动位置。
    List {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gap: Option<f32>,
        /// 贴底跟随（语义同 [`NodeBody::Scroll`] 的 `followEnd`）；缺省 false。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        follow_end: Option<bool>,
    },
    Text {
        content: String,
        /// 字号倍率（相对界面基准正文字号；缺省 1.0）。跟界面字号缩放，不是绝对像素。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        weight: Option<TextWeight>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<ColorSpec>,
        /// 水平对齐：声明后节点占满父容器并按该值摆放入文本；缺省按内容宽度收缩。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        align: Option<Align>,
        /// 字体族：`ui` 跟随界面字体（缺省），`mono` 用系统等宽族（数值 / 路径 / 日志按位列对齐）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        family: Option<TextFamily>,
        /// 是否允许按词换行；缺省 true。`false` 即单行（长标签不把行撑高）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wrap: Option<bool>,
        /// 最多渲染几行（至少 1）；超出时最后一行以省略号收尾。缺省不限。
        /// `wrap: false` 时恒单行，`maxLines` 只决定是否截断。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_lines: Option<u32>,
    },
    /// 只读 Markdown 正文：宿主按笔记同源解析器渲染（标题 / 列表 / 引用 / 代码块 / 表格 / 链接）。
    /// 块级图片与笔记同源载入：http(s) 走下载，其余按仓库相对路径。
    /// 字重与取色默认走宿主主题与 Markdown 语义（标题层级 / 强调），`weight` / `color` 作整体覆盖。
    /// 链接可点击：宿主按形态默认打开（外链 / wiki / 仓库路径），节点声明 `linkClick` 时同时回发事件。
    /// 无编辑态；其余交互请用其他控件。
    Markdown {
        content: String,
        /// 整体字号倍率（相对界面基准正文字号；缺省 1.0）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<f32>,
        /// 基础字重（标题与加粗片段恒为加粗，不受此项影响）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        weight: Option<TextWeight>,
        /// 基础文字色（链接 / 代码块等语义色不受影响）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<ColorSpec>,
        /// 字体族：`ui` 跟随界面字体（缺省），`mono` 用系统等宽族。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        family: Option<TextFamily>,
    },
    TextInput {
        #[serde(default)]
        value: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
        /// 多行：宿主按多行编辑器渲染。声明 `submit` 时 Enter 提交、Shift+Enter 换行；
        /// 未声明时 Enter 换行。单行恒按 Enter 提交。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        multiline: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        password: Option<bool>,
        /// 只读：保留输入控件外观但不接受编辑，不产生事件。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        read_only: Option<bool>,
        /// 值上报档位（缺省逐键上报）。非逐键时输入值由宿主本地持有并显示，插件不必回显。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        report: Option<ReportMode>,
    },
    NumberInput {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<f64>,
        /// 步进值（正数，缺省 1）：节点带 `change` 事件时，输入框右侧渲染上下步进按钮，
        /// 每次点击按该值增减，结果受 min / max 约束。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
        /// 只读：保留输入控件外观但不接受编辑、无步进，不产生事件。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        read_only: Option<bool>,
        /// 值上报档位（缺省逐键上报）。非逐键时输入值由宿主本地持有并显示，插件不必回显。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        report: Option<ReportMode>,
    },
    Dropdown {
        options: Vec<String>,
        /// 选中项下标；`change` 事件的值同为下标（[`EventValue::Number`]）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selected: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
        /// 只读：保留触发钮外观但不展开、不产生事件。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        read_only: Option<bool>,
    },
    Checkbox {
        #[serde(default)]
        checked: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        /// 只读：保留勾选态展示但不响应点击。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        read_only: Option<bool>,
    },
    Button {
        label: String,
        /// 前置图标（内置形状名或 `view.defineIcon` 登记的自备名）；缺省只有文字。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        variant: Option<ButtonVariant>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        enabled: Option<bool>,
        /// 尺寸档位（缺省 Sm，高 24 的紧凑钮）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<ControlSize>,
    },
    /// 图标名：宿主内置形状（lucide 线性形状）或经 `view.defineIcon` 定义的自备名（内置名优先）；
    /// 两者都未命中时画空。
    IconButton {
        icon: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tooltip: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        enabled: Option<bool>,
        /// 尺寸档位（缺省 Sm：14px 图标 + 28 方框）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<ControlSize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        variant: Option<ButtonVariant>,
        /// 悬停才显示（列表行的操作钮常驻会喧宾夺主）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hover_only: Option<bool>,
    },
    /// 纯装饰图标（不接受点击与悬停；空态、状态行、标题前的语义标记）。
    /// 需要可点的图标用 `iconButton`。
    Icon {
        /// 名字解析与 `iconButton` 同口径。
        name: String,
        /// 图标边长（px，缺省 16）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<ColorSpec>,
    },
    /// 图片：`width` / `height` 给定时按该尺寸渲染（数字 = px，`"fill"` = 撑满父容器该轴，
    /// `"shrink"` = 按原图；缺省按原图尺寸）；`fit` 决定尺寸内的适配方式，缺省 `contain`（不裁切不拉伸）。
    /// 节点声明 `click` 事件即成为可点击图片（大图预览入口）。
    Image {
        source: ImageSource,
        /// 尺寸内适配：`contain` 完整可见、`cover` 铺满并裁掉溢出、`fill` 拉伸铺满。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fit: Option<ImageFit>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        width: Option<SizeSpec>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        height: Option<SizeSpec>,
    },
    /// 进度条：0.0 ~ 1.0（越界值宿主钳制）。
    Progress {
        value: f64,
        /// 轨道粗细（px，缺省 30 = 系统标准粗条）；细进度条写 3 / 4。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        girth: Option<f32>,
        /// 完成段颜色（缺省主题强调色）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tone: Option<ColorSpec>,
    },
    /// 菜单：`label` 与 `icon` 可并存（并存时渲染为紧凑的「图标 + 文字」触发钮，非全宽下拉钮）；
    /// 只给 `icon` 为纯图标触发钮，只给 `label` 为全宽下拉钮。二者都缺省时回落到 `…` 触发钮。
    Menu {
        items: Vec<MenuItem>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<String>,
    },
    /// 自绘画布：宿主保留一块区域执行 `canvas.submit` 提交的指令清单（见 [`crate::canvas`]）。
    /// 指针事件按 `mouseEvents` 声明子集回发，未声明不上行；命中由插件按指令几何自算。
    /// `height` 给定时取该高度；`fill` = 占满父容器剩余空间（父容器可分配空间才有实际意义）。
    /// 画布尺寸变化时宿主回发 `host:canvas.resized`（插件据此对齐绘制坐标与滚动范围）。
    Canvas {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        mouse_events: Vec<CanvasEventKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        height: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fill: Option<bool>,
        /// 目标帧率（1–120）：声明后宿主按该节拍回发 `host:canvas.frame`，插件逐帧提交绘制内容；
        /// 缺省不订阅，静态画布不产生帧开销。不要求帧回调的插件不要声明。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fps: Option<u16>,
    },
    /// 分隔线：主题弱边界色的 1px 水平线（分组容器内的轻分割）。
    Divider {},
    /// 拖动热区：自身不渲染任何内容，用户在其占位区域内按下即由宿主发起整窗拖动
    /// （全接管或无系统边框的插件窗口里自绘标题栏的拖动把手）。事件不回发。
    DragRegion {},
}
```

```rust
// atelyx-plugin-protocol/src/view.rs
#[serde(rename_all = "camelCase")]
pub enum Align {
    Start,
    Center,
    End,
}
```

```rust
// atelyx-plugin-protocol/src/view.rs
/// 行主轴分布（[`NodeBody::Row`] 的 `justify`）。
#[serde(rename_all = "camelCase")]
pub enum Justify {
    Start,
    Center,
    End,
    /// 两端对齐：行占满宽度时首个贴左、末个贴右，中间拉开（工具行）。
    SpaceBetween,
}
```

```rust
// atelyx-plugin-protocol/src/view.rs
#[serde(rename_all = "camelCase")]
pub enum Axis {
    Horizontal,
    Vertical,
}
```

```rust
// atelyx-plugin-protocol/src/view.rs
/// 容器自身尺寸与上下限（`Row` / `Column` / `Card` 的 `size`）。
/// 上下限只对固定或收缩尺寸的子项生效：流体（`fill` 或父级按 `grow` 分配）子项的空间由父容器分配，
/// 此时上下限不参与布局，需要约束空间用父级的 `grow` 权重。最小尺寸抬的是节点外框，内容仍按自身
/// 尺寸贴左上摆放（要让内容一起撑开，把内容声明为 `fill`）。
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoxSize {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<SizeSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<SizeSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_width: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_width: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_height: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_height: Option<f32>,
}
```

```rust
// atelyx-plugin-protocol/src/view.rs
/// 单轴尺寸：`fill` 占满父容器该轴、`shrink` 按内容收缩、数字为固定逻辑像素。
pub enum SizeSpec {
    Fill,
    Shrink,
    Fixed(f32),
}
```

```rust
// atelyx-plugin-protocol/src/view.rs
/// 网格行高（[`NodeBody::Grid`] 的 `row_height`）：数字 = 固定逻辑像素，`content` = 按内容。
pub enum RowHeight {
    Fixed(f32),
    Content,
}
```

```rust
// atelyx-plugin-protocol/src/view.rs
/// 交互外观（节点的 `interaction`）：决定该节点的命中与状态着色。
/// 交互态按「禁用 > 激活 > 焦点 > 悬停 > 常态」取第一个已声明且命中的档位；该档未声明时继续往后找，最终落到常态（连常态也没有则不绘制外观）。
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Interaction {
    /// 当前状态：缺省 `normal`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<InteractionState>,
    /// 常态外观。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normal: Option<Surface>,
    /// 悬停外观（鼠标进入节点矩形）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hover: Option<Surface>,
    /// 键盘焦点外观（节点持有键盘焦点时；键盘用户据此看见当前目标）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused: Option<Surface>,
    /// 激活外观（列表选中项、当前 tab）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<Surface>,
    /// 禁用外观（不接受点击，也不响应悬停）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled: Option<Surface>,
}
```

```rust
// atelyx-plugin-protocol/src/view.rs
/// 交互状态。
#[serde(rename_all = "camelCase")]
pub enum InteractionState {
    Normal,
    Active,
    /// 不接受点击与悬停，可选声明 `disabled` 外观。
    Disabled,
}
```

```rust
// atelyx-plugin-protocol/src/view.rs
/// 一层外观：底色 + 边框色 + 圆角。三者都可省，只声明的项生效。
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Surface {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<ColorSpec>,
    /// 边框色（1px 描边）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub border: Option<ColorSpec>,
    /// 圆角半径（px）；缺省用宿主小圆角。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radius: Option<f32>,
}
```

```rust
// atelyx-plugin-protocol/src/view.rs
/// 取色：引用宿主主题 token，或给 `#rgb` / `#rgba` / `#rrggbb` / `#rrggbbaa` 自定义色。
/// 主题 token 随主题与字号设置联动；自定义色由插件自负对比度。未知 token 与非法色值都拒收。
#[serde(untagged)]
pub enum ColorSpec {
    Token(ColorToken),
    Custom(Rgba),
}
```

```rust
// atelyx-plugin-protocol/src/view.rs
/// 自定义色（sRGB，各通道 0.0–1.0）。
pub struct Rgba(pub [f32; 4]);
```

```rust
// atelyx-plugin-protocol/src/view.rs
/// 字体族：界面字体（跟随用户设置）或系统等宽族。
#[serde(rename_all = "camelCase")]
pub enum TextFamily {
    Ui,
    Mono,
}
```

```rust
// atelyx-plugin-protocol/src/view.rs
#[serde(rename_all = "camelCase")]
pub enum TextWeight {
    Normal,
    Medium,
    SemiBold,
}
```

```rust
// atelyx-plugin-protocol/src/view.rs
/// 主题 token：取色引用宿主调色板，主题与字号设置变化时自动跟随。
/// `accentSoft` / `warningSoft` / `dangerSoft` / `successSoft` 为对应语义色的低饱和底（提示块、徽标），
/// `surface*` / `hover` / `border*` 为层次底色与边界（分组盒、分隔、悬停反馈）。
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
// atelyx-plugin-protocol/src/view.rs
#[serde(rename_all = "camelCase")]
pub enum ButtonVariant {
    Primary,
    Ghost,
    Danger,
}
```

```rust
// atelyx-plugin-protocol/src/view.rs
/// 卡片底色档位（[`NodeBody::Card`] 的 `tone`）。插件只选主题档位，不写死色值。
#[serde(rename_all = "camelCase")]
pub enum CardTone {
    /// 默认卡片底（比面板底色略深一档）。
    Default,
    /// 更淡的底（次级信息分组）。
    Muted,
    /// 强调软底（accent 的低饱和底）。
    AccentSoft,
    /// 无底色（只用边框分组）。
    Transparent,
}
```

```rust
// atelyx-plugin-protocol/src/view.rs
/// 控件尺寸档位。`Sm` = 紧凑（Button 高 24、IconButton 14px 图标 + 28 方框）；
/// `Md` = 常规（Button 高 32、IconButton 16px 图标 + 32 方框）；`Lg` = 宽松。
#[serde(rename_all = "camelCase")]
pub enum ControlSize {
    Sm,
    Md,
    Lg,
}
```

```rust
// atelyx-plugin-protocol/src/view.rs
#[serde(rename_all = "camelCase")]
pub enum ImageFit {
    Contain,
    Cover,
    Fill,
}
```

```rust
// atelyx-plugin-protocol/src/view.rs
/// 图片来源：`private` = 插件私有目录内相对路径；`vault` = 仓库根相对路径（宿主按仓库内容读取，
/// 协作空间走同构内容面）；`url` = http(s) 地址（缺省按公网策略下载并缓存，`local` 改为本机 /
/// 局域网策略）；`file` = 本机绝对路径（插件自己读得到的文件，交给宿主显示）。
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ImageSource {
    Private { path: String },
    Vault { path: String },
    Url {
        url: String,
        /// 按本机 / 局域网策略取图：放行回环与私网、仅拒链路本地与广播（插件连接本机服务时用）。
        /// 缺省 = 公网策略（拒回环与私网）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        local: Option<bool>,
    },
    /// 本机绝对路径。渲染层的图源不做权限门禁（与 `private` / `vault` 同口径），宿主按
    /// 原路径读盘显示；插件要读它的内容本来就能经 `fs` 自己读，这里不增加也不收紧范围。
    File { path: String },
}
```

```rust
// atelyx-plugin-protocol/src/view.rs
#[serde(rename_all = "camelCase")]
pub struct MenuItem {
    pub label: String,
    /// 条目动作标识（须在本菜单内唯一），点击时随 `change` 事件以 [`EventValue::Text`] 回发。
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub danger: Option<bool>,
    /// 当前项标记（宿主在条目前画勾）：用于「历史会话列表」这类单选菜单。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<bool>,
    /// 分组名（单层）：相邻同名条目归入同一组，宿主在组首画一次组标题；无分组条目独立成行。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}
```

```rust
// atelyx-plugin-protocol/src/view.rs
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
// atelyx-plugin-protocol/src/view.rs
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AxesPadding {
    #[serde(default)]
    pub horizontal: f32,
    #[serde(default)]
    pub vertical: f32,
}
```

```rust
// atelyx-plugin-protocol/src/view.rs
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

## 4. 视图事件与指针

宿主回发给插件的事件种类与取值形态；画布指针事件单列在下一节。

```rust
// atelyx-plugin-protocol/src/view.rs
/// 输入控件的上报档位：决定宿主何时把用户输入回发给插件。
///
/// `Immediate` 下宿主每次编辑都回发 `change`，插件须把值写回树——宿主用「已上报值 = 插件值」
/// 判定能否显示本机文本，不回写即表现为输入不显示。`Commit` / `Debounced` 下输入值完全由
/// 宿主本地持有并显示，插件不必回显；宿主仅在插件下发的值发生变化时视为主动改值、接管输入。
#[serde(rename_all = "camelCase")]
pub enum ReportMode {
    /// 逐键上报。
    Immediate,
    /// 提交时上报：单行按 Enter；多行在停止输入后上报，声明 `submit` 时按 Enter 立即上报
    /// （Enter 另发 `submit`，Shift+Enter 换行）。
    Commit,
    /// 停顿上报：停止编辑 `Debounced` 毫秒后回发 `change`（毫秒为 0 按 [`ReportMode::Immediate`] 处理）。
    Debounced(u32),
}
```

```rust
// atelyx-plugin-protocol/src/view.rs
/// 节点事件种类：声明在 [`ViewNode::events`] 里宿主才回发，未声明的事件不上行（省流量、省误触）。
///
/// 声明 `focus` / `blur` / `key` 任一项即把节点标为**可聚焦**：在该节点上按下即取得键盘焦点，
/// 在别处按下即失去；同一面内至多一个节点持有焦点，同一窗口同一时刻至多一个面接收键盘。
/// 输入类控件（单行 / 多行 / 数字）自己管键盘，不得声明 `key`（提交时拒收）。
#[serde(rename_all = "camelCase")]
pub enum EventKind {
    Click,
    /// 右键在节点矩形内按下（与 `click` 相互独立，可同时声明）。
    /// 画布节点自带带按键的指针事件，右键用 `mouseEvents` 的 `pointerDown` 即可；两者都声明时
    /// 一次右键会各回发一次。
    SecondaryClick,
    Change,
    Submit,
    /// 取得键盘焦点（仅可聚焦节点）。
    Focus,
    /// 失去键盘焦点（仅可聚焦节点）。
    Blur,
    /// 键盘按键（仅可聚焦节点）：持焦点期间回发全部**无命令修饰**的按键，带 Ctrl / Alt / Super
    /// 的组合键归宿主、不上行。按键本身不做逐键声明——需要更细的筛选由插件在回调里判。
    Key,
    /// Markdown 正文内的链接点击（仅 `markdown` 节点；宿主默认打开行为不受声明影响）。
    LinkClick,
    /// 滚动位置变化（仅 `scroll` / `list` 节点）：滚动时回发当前偏移与可滚动余量；
    /// 内容不足一屏时没有滚动，不发事件。
    Scroll,
}
```

```rust
// atelyx-plugin-protocol/src/view.rs
/// 宿主回发给插件的事件负载。`change`/`submit` 携带新值；菜单条目选择复用 `change`，
/// 值为条目的 `action` 文本（[`EventValue::Text`]）。
#[serde(tag = "kind", rename_all = "camelCase")]
#[serde(rename_all_fields = "camelCase")]
pub enum ViewEvent {
    Click,
    SecondaryClick,
    Change { value: EventValue },
    Submit { value: EventValue },
    Focus,
    Blur,
    /// 键盘按键（仅声明 `key` 的可聚焦节点）：`key` 为按键本身，`action` 区分按下 / 抬起，
    /// `modifiers` 为当时的修饰键快照。
    Key { key: Key, action: KeyAction, modifiers: Modifiers },
    /// Markdown 链接点击（仅声明 `linkClick` 的节点）。`href` 为源文本原样（未解码；
    /// wiki 为笔记名），`form` 与宿主笔记链接解析同一口径。
    LinkClick { href: String, form: LinkForm },
    /// 画布指针事件（仅 `canvas` 节点按 `mouseEvents` 声明后回发）。
    Canvas { pointer: CanvasPointer },
    /// 滚动位置（仅声明 `scroll` 的 `scroll` / `list` 节点）：`offset` = 主轴当前偏移，
    /// `max_offset` = 主轴可滚动余量；`offset >= max_offset` 即停在末端。
    Scroll { offset: f32, max_offset: f32 },
}
```

```rust
// atelyx-plugin-protocol/src/view.rs
/// 按键：具名键（功能键与编辑键）或可打印字符。字符按布局产出（与用户所见一致），
/// 大小写与符号跟随 Shift，因此 `a` 与 `A` 是两个值、`1` 与 `!` 也是两个值。
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Key {
    Named { name: NamedKey },
    Character { text: String },
}
```

```rust
// atelyx-plugin-protocol/src/view.rs
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
// atelyx-plugin-protocol/src/view.rs
/// 键盘焦点事件与按键的归属：`Press` 含系统重复按键（长按方向键逐次到达），`Release` 在抬起时到达。
#[serde(rename_all = "camelCase")]
pub enum KeyAction {
    Press,
    Release,
}
```

```rust
// atelyx-plugin-protocol/src/view.rs
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
// atelyx-plugin-protocol/src/view.rs
/// Markdown 链接形态：外链 / wiki / 仓库路径 / 可新建 / 未识别（保持原文）。
/// `create` / `plain` 在 Markdown 正文里不可点击，`linkClick` 只会带来前三种。
#[serde(rename_all = "lowercase")]
pub enum LinkForm {
    External,
    Wiki,
    Path,
    Create,
    Plain,
}
```

```rust
// atelyx-plugin-protocol/src/view.rs
/// 事件值的四形态。untagged 按序匹配即可无歧义区分（字符串 / 数值 / 布尔 / null 互不重叠）；
/// 数字输入清空时上报 `Null`，与「值为 0」区分。
#[serde(untagged)]
pub enum EventValue {
    Text(String),
    Number(f64),
    Bool(bool),
    Null,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
#[serde(rename_all = "camelCase")]
pub struct ViewEventParams {
    pub surface_id: String,
    pub node_id: String,
    pub event: crate::view::ViewEvent,
}
```

## 5. 自绘画布

画布是树里的叶子控件（占位 + 指针事件 + 可选帧率），内容经 `canvas.submit` 整表提交、`canvas.patch` 按 id 增量维护；文字占位经 `canvas.measureText` 量算（不要按字符数估算）；坐标一律画布本地逻辑像素、左上原点。

```rust
// atelyx-plugin-protocol/src/view.rs
/// 画布指针事件种类：节点 `mouseEvents` 声明要收的子集，宿主只回发声明的。
#[serde(rename_all = "camelCase")]
pub enum CanvasEventKind {
    PointerDown,
    PointerMove,
    PointerUp,
    PointerLeave,
    Wheel,
    DoubleClick,
}
```

```rust
// atelyx-plugin-protocol/src/view.rs
/// 鼠标按键。
#[serde(rename_all = "camelCase")]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}
```

```rust
// atelyx-plugin-protocol/src/view.rs
/// 画布指针事件负载。坐标 = 节点本地逻辑像素、左上原点；
/// wheel 增量沿用设备单位（滚轮为行、触控板为像素），正 = 向下 / 向右（Web 惯例，宿主已完成方向换算）。
#[serde(tag = "kind", rename_all = "camelCase")]
#[serde(rename_all_fields = "camelCase")]
pub enum CanvasPointer {
    Down { x: f32, y: f32, button: MouseButton },
    Move { x: f32, y: f32 },
    Up { x: f32, y: f32, button: MouseButton },
    Leave,
    Wheel { x: f32, y: f32, delta_x: f32, delta_y: f32 },
    /// 宿主判定（同键二次按下、间隔与位移都在阈值内），插件无需自己算。
    DoubleClick { x: f32, y: f32, button: MouseButton },
}
```

```rust
// atelyx-plugin-protocol/src/canvas.rs
/// 颜色：主题 token（跟随宿主主题）或插件自带 RGBA（内容谱系色，如连线分类、播放头高亮）。
#[serde(tag = "type", rename_all = "camelCase")]
#[serde(rename_all_fields = "camelCase")]
pub enum CanvasColor {
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
// atelyx-plugin-protocol/src/canvas.rs
/// 二维坐标点（逻辑像素）。
pub struct CanvasPoint {
    pub x: f32,
    pub y: f32,
}
```

```rust
// atelyx-plugin-protocol/src/canvas.rs
/// 文字水平对齐：以 `maxWidth` 为对齐区域、`x` 为区域左缘，按量算出的整段宽度换算（换行后段内各行仍左对齐）。
#[serde(rename_all = "camelCase")]
pub enum TextAlign {
    Start,
    Center,
    End,
}
```

```rust
// atelyx-plugin-protocol/src/canvas.rs
/// 绘图指令。`fill` 与 `stroke` 至少声明其一；`stroke_width` 只在 `stroke` 存在时生效（缺省 1.0）。
#[serde(tag = "kind", rename_all = "camelCase")]
#[serde(rename_all_fields = "camelCase")]
pub enum DrawCommand {
    Line {
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        stroke: CanvasColor,
        #[serde(default = "default_stroke_width")]
        width: f32,
    },
    Rect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        fill: Option<CanvasColor>,
        stroke: Option<CanvasColor>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stroke_width: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        corner_radius: Option<f32>,
    },
    Path {
        points: Vec<CanvasPoint>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        closed: bool,
        fill: Option<CanvasColor>,
        stroke: Option<CanvasColor>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stroke_width: Option<f32>,
    },
    Circle {
        cx: f32,
        cy: f32,
        r: f32,
        fill: Option<CanvasColor>,
        stroke: Option<CanvasColor>,
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
        color: CanvasColor,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        weight: Option<TextWeight>,
        /// 字体族：`ui` 跟随界面字体（缺省），`mono` 用系统等宽族。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        family: Option<TextFamily>,
        /// 可用宽度：给定即按该宽度换行，并作为 `align` 的对齐区域。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_width: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        align: Option<TextAlign>,
    },
    /// 图片贴片：绘制在给定矩形内。`fit` 缺省 = 拉伸填满；`contain` 等比完整可见（留空边），
    /// `cover` 等比覆盖并裁剪溢出部分。
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
// atelyx-plugin-protocol/src/canvas.rs
/// 增量提交的一项：按 `id` 更新已有指令，或追加到清单末尾（`id` 由插件自定，同一画布内唯一）。
pub struct CanvasUpsert {
    pub id: String,
    pub command: DrawCommand,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// `canvas.submit` 的 params：整表重建（空清单 = 清空画布），此前经增量建立的 id 槽一并丢弃。
#[serde(rename_all = "camelCase")]
pub struct CanvasSubmitParams {
    pub surface_id: String,
    /// 目标画布节点 id（须在当前 `view.submit` 树里且为 canvas 节点）。
    pub node_id: String,
    pub commands: Vec<crate::canvas::DrawCommand>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// `canvas.patch` 的 params：按 id 更新 / 追加 / 删除条目，次序按 `upsert` 在前、`remove` 在后。
/// 同一 `id` 不得在同一请求里既 upsert 又 remove（语义有歧义，直接拒收）。
#[serde(rename_all = "camelCase")]
pub struct CanvasPatchParams {
    pub surface_id: String,
    pub node_id: String,
    #[serde(default)]
    pub upsert: Vec<crate::canvas::CanvasUpsert>,
    #[serde(default)]
    pub remove: Vec<String>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// `host:canvas.resized` 的 params：画布节点当前的像素尺寸（逻辑像素），尺寸变化时发。
#[serde(rename_all = "camelCase")]
pub struct CanvasResizedParams {
    pub surface_id: String,
    pub node_id: String,
    pub width: f32,
    pub height: f32,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`canvas::HOST_FRAME`] 的 params：一帧的拍号与距上一帧的间隔，插件据此推进动画相位。
#[serde(rename_all = "camelCase")]
pub struct CanvasFrameParams {
    pub surface_id: String,
    pub node_id: String,
    /// 该画布的帧序号（从 0 起，宿主按画布记账）。
    pub frame: u64,
    /// 距该画布上一帧的毫秒数（首帧为 0）。动画相位请以帧序号为准，此值只作节拍参考。
    pub delta_ms: u32,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`canvas::MEASURE_TEXT`] 的 params：`size` 为绝对字号（逻辑像素，与画布 `text` 指令同口径）。
/// 参数面与画布 `text` 指令保持一致，量出来的宽度才能在画布上复现。
#[serde(rename_all = "camelCase")]
pub struct MeasureTextParams {
    pub text: String,
    pub size: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<crate::view::TextWeight>,
    /// 字体族：`ui` 跟随界面字体（缺省），`mono` 用系统等宽族。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<crate::view::TextFamily>,
    /// 可用宽度上限：给定即按该宽度换行量算（缺省按单行）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_width: Option<f32>,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`canvas::MEASURE_TEXT`] 的 result：占位宽高（逻辑像素）与换行后的行数（单行量算恒为 1）。
#[serde(rename_all = "camelCase")]
pub struct MeasureTextResult {
    pub width: f32,
    pub height: f32,
    pub lines: u32,
}
```

## 6. 整面帧

整面帧把面的绘制与输入整体交给插件：`frame.submit` 提交指令清单 + 命中区 + 输入声明，宿主只按几何绘制并把原始输入原样回发；与同面的界面树互斥，面按最后提交的通道渲染。

```rust
// atelyx-plugin-protocol/src/method.rs
/// 面帧：整个面交给插件自绘（与 `view.submit` 互斥，面以最后提交的为准）。
pub mod frame {
    /// 插件提交一份整面帧（指令清单 + 命中区 + 输入法光标 + 键盘 / 帧率声明）。整帧替换。
    pub const SUBMIT: &str = "frame.submit";
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
    /// 滚轮增量沿用设备单位（滚轮为行、触控板为像素），正 = 向下 / 向右（Web 惯例，宿主已完成方向换算）。
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
/// `frame.submit` 的 params：整面重建（一帧即当前完整界面，未提交过的面显示占位）。
#[serde(rename_all = "camelCase")]
pub struct FrameSubmitParams {
    pub surface_id: String,
    #[serde(flatten)]
    pub frame: SurfaceFrame,
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

## 7. 纯查询：量算与主题

纯计算调用，不改界面也不产生帧；同一段文字反复量算时插件应自行缓存结果。

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
/// 单次批量量算的条目上限。
pub const MAX_MEASURE_ITEMS: usize = 512;
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// [`query::MEASURE`] 的一条：字段与 [`MeasureTextParams`] 同口径。
#[serde(rename_all = "camelCase")]
pub struct MeasureItem {
    pub text: String,
    pub size: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<crate::view::TextWeight>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<crate::view::TextFamily>,
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
/// 主题色：十六进制 `#rrggbb` / `#rrggbbaa`，字段名与 [`crate::view::ColorToken`] 的 wire 名一致。
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

## 8. 贡献点与窗口面

贡献点决定插件界面出现在哪里；`surfaceId` 即贡献 id（插件窗口与文档实例另有宿主分配的 id）。

```rust
// atelyx-plugin-protocol/src/method.rs
/// 贡献点位置（封闭枚举；扩容随协议升版）。
#[serde(rename_all = "camelCase")]
pub enum ContributionSlot {
    /// 工作区面板（「添加面板 / 切换视图」菜单条目）。
    Panel,
    /// 表格编辑器工具条「视图」下拉条目。
    TableView,
    /// 设置页顶层 tab（注册即挂载：宿主立即回 `host:surface.attached`，插件常驻设置树）。
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
#[serde(rename_all = "camelCase")]
pub struct SubmitParams {
    pub surface_id: String,
    pub tree: crate::view::ViewNode,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
#[serde(rename_all = "camelCase")]
pub struct SubmitResult {
    pub applied: bool,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// `view.register` 的 params：声明一个贡献点。`id` 插件内唯一且不得含 `:`（宿主的面标识以它作分隔），
/// 同时是该面挂载后的 `surfaceId`；`icon` 为宿主内置形状名或 `view.defineIcon` 定义的自备名
/// （内置名优先），缺省 / 未注册时宿主用拼图图标。
#[serde(rename_all = "camelCase")]
pub struct RegisterParams {
    pub slot: ContributionSlot,
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// 面外壳内边距：数字 = 四边等距，也可按轴 / 按边声明（见 [`crate::view::PaddingSpec`]）；
    /// 0 = 贴边，缺省 = 宿主默认。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub padding: Option<crate::view::PaddingSpec>,
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
/// `view.defineIcon` 的 params：定义一枚自备图标形状，此后按 `name` 在贡献注册与图标钮里引用。
/// `path` 为 24x24 网格的描边路径（与宿主形状表同口径）；名字须为字母 / 数字 / `_` / `-` 且不与
/// 宿主内置形状重名。定义随插件会话收尾清除。
#[serde(rename_all = "camelCase")]
pub struct DefineIconParams {
    pub name: String,
    pub path: String,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// 可滚动容器的主轴一端（[`view::SCROLL_TO`] 的 `edge`）。
#[serde(rename_all = "camelCase")]
pub enum ScrollEdge {
    /// 纵向容器 = 顶部；横向容器 = 最左。
    Start,
    /// 纵向容器 = 底部；横向容器 = 最右。
    End,
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
/// `view.scrollTo` 的 params：把某可滚动节点（`list` / `scroll`）滚到某处。
/// `edge` 与 `offset` 二选一，同时给出或都不给即拒收。
#[serde(rename_all = "camelCase")]
pub struct ScrollToParams {
    pub surface_id: String,
    pub node_id: String,
    /// 滚到主轴一端。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge: Option<ScrollEdge>,
    /// 按比例定位：0 = 起点、1 = 终点（越界与内容不足一屏时由控件钳制）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<f32>,
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
/// 已注册的贡献 id（与该面共享同一棵提交树）；退出接管入口由宿主常驻提供，插件无需声明退路。
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

## 9. 默认布局

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
        direction: crate::view::Axis,
        children: Vec<LayoutSpecNode>,
        sizes: Vec<f64>,
    },
    /// 面板节点：按顺序放置的视图 kind（首个为激活标签）。
    Panel { views: Vec<String> },
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

## 10. 能力面方法名

每个模块是一类能力面的方法名常量，与 `permissions` 里的能力声明一一对应；未声明能力的调用会被宿主拒绝并记入审计。

```rust
// atelyx-plugin-protocol/src/method.rs
pub mod view {
    /// 插件提交一棵界面树到某个面（面板 / 窗口 / 接管区 / 文档实例）。宿主校验后按键 diff 挂载。
    pub const SUBMIT: &str = "view.submit";
    /// 插件注册一个贡献点（面板 / 表格视图 / 设置页），使其出现在宿主对应的入口里。
    pub const REGISTER: &str = "view.register";
    /// 插件按 id 撤销自己的一个贡献点。
    pub const UNREGISTER: &str = "view.unregister";
    /// 插件定义一枚自备图标形状（24x24 网格的描边路径），此后按名字在贡献与控件里引用。
    pub const DEFINE_ICON: &str = "view.defineIcon";
    /// 把一个可滚动容器（list / scroll）滚到一端。目标节点须在当前树里且为可滚动容器。
    pub const SCROLL_TO: &str = "view.scrollTo";
    /// 插件请求接管当前聚焦窗口（整页 / 全接管）。宿主按窗应用，退出入口由宿主常驻提供。
    pub const TAKEOVER: &str = "view.takeover";
    /// 宿主回发节点事件（通知）。
    pub const HOST_EVENT: &str = "host:view.event";
    /// 宿主告知插件某个面已就绪 / 已移除（通知）。
    pub const HOST_ATTACHED: &str = "host:surface.attached";
    pub const HOST_DETACHED: &str = "host:surface.detached";
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
pub mod window {
    /// 插件请求打开一个独立窗口承载自己的界面树（声明的 `id` 即该窗口面的 `surfaceId`）。
    /// 缺省用系统窗口边框；`decorations: false` 时去掉系统边框，由宿主提供标题栏
    /// （标题 / 拖动 / 最小化 / 最大化 / 关闭）。
    pub const OPEN: &str = "window.open";
    /// 插件关闭自己打开的一个窗口。
    pub const CLOSE: &str = "window.close";
    /// 插件运行时改一个已打开窗口的属性（尺寸 / 置顶；「不进任务栏」只能在开窗时声明）。
    pub const UPDATE: &str = "window.update";
    /// 插件重新显示一个因失焦被隐藏的窗口（`hideOnBlur` 的唤起回路）。
    pub const SHOW: &str = "window.show";
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
    /// 插件据此以 `view.submit` 提交该文档实例的界面树。
    pub const HOST_OPEN: &str = "host:doc.open";
    /// 文档实例已关闭 / 已切换走（通知）。插件此后不得再向该 surfaceId 提交树。
    pub const HOST_CLOSE: &str = "host:doc.close";
}
```

```rust
// atelyx-plugin-protocol/src/method.rs
pub mod canvas {
    /// 插件提交一份绘图指令清单到某个画布节点。宿主合帧应用（渲染每帧只取最新一份）。
    /// 整表重建：清单里的条目都没有 id，此前经 `PATCH` 建立的增量条目一并丢弃。
    pub const SUBMIT: &str = "canvas.submit";
    /// 按 id 增量维护画布清单：更新 / 追加 / 删除单个条目，只上行改动的条目，不必重发整表。
    /// 未提交过清单时以空表为基线（首次也可直接用增量建表）。
    pub const PATCH: &str = "canvas.patch";
    /// 量算一段文字在给定字号 / 字体下的占位尺寸（画布排版要按真实字宽摆放，不能按字符数估）。
    /// 纯计算：不改清单、不产生帧、不要求节点存在。
    pub const MEASURE_TEXT: &str = "canvas.measureText";
    /// 宿主告知插件画布节点的像素尺寸（通知，尺寸变化时发）。
    pub const HOST_RESIZED: &str = "host:canvas.resized";
    /// 宿主按节点声明的 `fps` 发帧拍（通知）：插件收到后提交该帧的绘制内容。
    pub const HOST_FRAME: &str = "host:canvas.frame";
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

## 11. 能力面负载：文件、进程与系统服务

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

## 12. 能力面负载：文档与对话

领域文档实例、AI 对话与工具桥的负载形态。

```rust
// atelyx-plugin-protocol/src/method.rs
#[serde(rename_all = "camelCase")]
pub struct DocOpenParams {
    /// 仓库容器内的相对路径（本地仓库）或服务端内容路径（协作空间），与 vault 口径一致。
    pub path: String,
    /// 宿主为本实例分配的面标识：插件据此以 `view.submit` 提交该文档实例的界面树。
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

## 13. 能力面负载：表格

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
