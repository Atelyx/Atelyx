//! 插件宿主运行时：发现 / 装载 / RPC 泵 / 权限仲裁 / 生命周期 / 安装卸载。
//! 运行时为进程内单例（壳持 `Arc`，多窗口共享）；插件事件经 [`PluginRuntime::subscribe_events`]
//! 的单一广播通道进入壳的 update。进程创建与清理一律经 [`crate::plugin_process`]。

mod ai_tools;

mod capability;
mod child_process;
mod fs;
mod frame_region;
mod http;
mod install;
mod instance;
mod layout;
mod log;
mod manifest;
mod markdown;
/// 市场数据源（公开给壳：索引拉取 / 缓存 / Release 取数）。
pub mod market;
mod preset;
mod service_dir;
mod shortcut;
/// 表格服务面的纯映射（公开给壳：表格真源 → 协议只读快照）。
pub mod table;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use atelyx_plugin_protocol::event;
use atelyx_plugin_protocol::manifest::Manifest;
use atelyx_plugin_protocol::method;
use serde::Serialize;
use serde_json::Value;

use crate::plugin_process::PluginProcessHost;

use ai_tools::ToolCallWait;
use instance::Instance;
use service_dir::{ServiceCallWait, ServiceDir};

/// 协议类型再导出：壳只依赖 `atelyx-core`，不直接依赖协议 crate。
pub use ai_tools::{PluginToolOutput, PluginToolSpec};
pub use atelyx_plugin_protocol::frame::{
    FrameInput, FrameRect, HitCursor, HitRegion, Key, KeyAction, Modifiers, MouseButton, NamedKey,
    SurfaceFrame,
};
pub use atelyx_plugin_protocol::paint::{
    ColorToken, DrawCommand, ImageFit, ImageSource, PaintColor, PaintPoint, TextAlign, TextFamily,
    TextWeight,
};
pub use atelyx_plugin_protocol::event::{
    ChatChangeKind, ChatChangedPayload, FileChangeKind, FileChangedPayload,
    SpaceMetaChangedPayload, TableChangedPayload, VaultSwitchPayload,
};
pub use atelyx_plugin_protocol::layout::LayoutSpecNode;
pub use atelyx_plugin_protocol::manifest::{DocTypeRegistration, Permission};
/// 能力总线的对外契约类型（服务清单 / 登记参数 / 错误原因）。
pub use atelyx_plugin_protocol::service;
/// 随包预置插件同步的结果。
pub use preset::PresetSyncReport;
pub use atelyx_plugin_protocol::method::{
    AiAgent, AiAgentsResult, AiModel, AiModelPick, AiModelProvider, AiModelsResult,
    AxesPadding, BUILTIN_SERVICES, PLATFORM_PLUGIN, EditorReplaceParams, EditorSelection,
    CalendarAddParams, CalendarIdParams, CalendarItem, CalendarListResult, CalendarUpdateParams,
    CollabMetaEntry, CollabMetaGetParams, CollabMetaPatchParams, CollabMetaResult, DayActivity,
    HistoryActivityResult, HistoryDayEntry, HistoryDayParams, HistoryDayResult, HistoryOpenParams,
    ChatEvent, ChatEventParams, ChatHistoryParams, ChatHistoryResult,
    ChatRenameParams, ChatSendParams, ChatSessionParams, ChatSessionSummary, ChatSessionWire,
    ChatStopParams, ClipboardImage, CollabMessageParams, CollabPeer, CollabPeersResult,
    CollabSendParams, CollabSendQos, CollabSendResult, ContributionSlot, EdgesPadding, FileFilter,
    FontSizes, ImageInfoResult, MeasureBatchParams, MeasureBatchResult, MeasureItem,
    MeasureTextResult, NotificationLevel, PaddingSpec, Radii,
    SetTrayItemsParams, ShortcutQueryResult, SurfaceKind, TableField, TableRemoteSelection,
    TableSelection, TableRow, TableSetCellImageParams, TableSnapshot, TakeoverScope, ThemeColors,
    ThemeResult, TrayClickParams, TrayItemSpec, WindowOpenParams, WindowUpdateParams,
};

/// 插件运行状态。
#[derive(Clone, Debug, PartialEq)]
pub enum PluginStatus {
    /// 未启用或已停用。
    Stopped,
    /// 进程在跑。
    Running,
    /// 崩溃后退避重启中。
    Restarting,
    /// 窗口内崩溃超限，已熔断（需手动重试）。
    CircuitBroken,
    /// 启动失败 / 握手失败 / 不可装载（附可读原因）。
    Failed(String),
}

/// 一条能力调用记录（审计）。调用一律执行，记录用于设置页区分「声明能力」与「实际用到但未声明」。
#[derive(Clone, Debug)]
pub struct AuditRecord {
    /// 调用的方法名（`service.call` 记规范化后的完整方法名）。
    pub method: String,
    /// 调用携带的路径（无则空串）。
    pub detail: String,
    /// 记录时刻（unix 秒）。
    pub at: u64,
    /// 该方法所属能力在 manifest 里声明过（false = 实际用到但未声明）。
    pub declared: bool,
}

/// 运行时 → 壳的广播事件（状态 / 通知 / 未声明调用 / 界面与贡献变化；允许落后丢帧）。
#[derive(Clone, Debug)]
pub enum PluginEvent {
    StatusChanged { id: String, status: PluginStatus },
    Notification { id: String, title: String, body: Option<String>, level: NotificationLevel },
    /// 某插件用到了未在 manifest 声明过的能力：调用照常执行，这里只做知情提示
    /// （同一方法每次会话只广播第一次，避免提示刷屏）。
    PermissionDenied { id: String, method: String, detail: String },
    /// 某插件提交了新的整面帧（壳经 [`PluginRuntime::surface_frame`] 取；丢帧不影响最终渲染）。
    ViewSubmitted { id: String },
    /// 某插件的贡献注册表变化（注册 / 撤销 / 随会话收尾清空）。
    ContributionsChanged { id: String },
    /// 某插件注册的 AI 工具变化（注册 / 撤销 / 随会话收尾清空）。
    AiToolsChanged { id: String },
    /// 服务目录变化（登记 / 撤销 / 切换提供者 / 提供者下线回落）：壳据此重建注册表的服务区。
    ServiceChanged { id: String },
    /// 插件声明了自己的默认布局（壳在首次启用时摆进当前场景，之后不再重复）。
    DefaultLayoutDeclared { id: String, name: String, spec: LayoutSpecNode },
}

/// 一个已注册的贡献点（菜单与标签渲染用）。
#[derive(Clone, Debug, PartialEq)]
pub struct Contribution {
    pub plugin_id: String,
    pub slot: ContributionSlot,
    /// 贡献 id（插件内唯一、不含 `:`），同时是该面挂载后的 `surfaceId`。
    pub id: String,
    pub label: String,
    /// 宿主图标形状表里的图标名；插件未声明时为 `None`（宿主回落拼图）。
    pub icon: Option<String>,
    /// 面外壳内边距，只作用于面板 / 设置 / 表格视图这类有宿主外壳的面；
    /// `None` = 贴边（宿主不加内边距）。插件窗口与接管面无宿主外壳内边距。
    pub padding: Option<PaddingSpec>,
}

/// 一个领域文档类型注册及其所属插件（文件树图标 / 新建菜单 / 打开路由用）。
#[derive(Clone, Debug, PartialEq)]
pub struct DocTypeInfo {
    pub plugin_id: String,
    pub registration: DocTypeRegistration,
}

/// 对话框种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogKind {
    File,
    Directory,
}

/// 对话框请求（运行时 → 壳）：壳按此弹系统对话框，用户的选择经 [`PluginRuntime::resolve_dialog`]
/// 回填给等待中的插件调用。
#[derive(Clone, Debug)]
pub struct DialogRequest {
    pub kind: DialogKind,
    pub title: Option<String>,
    /// 文件对话框是否为「保存」形态（目录对话框恒 false）。
    pub save: bool,
    pub default_path: Option<String>,
    pub filters: Vec<FileFilter>,
}

/// 仓库二进制单次读写上限：内容面数据走 JSON 单行传输，一条消息的峰值内存是载荷的数倍，
/// 超限以可读失败收场，不做截断。更大的文件用 `vault.importFile`——字节留在插件磁盘上，
/// 由宿主按路径收进仓库，不进 RPC 载荷。
pub const MAX_VAULT_BINARY_BYTES: usize = 32 * 1024 * 1024;

/// 剪贴板图片单次写入上限（PNG 字节）：超限拒绝，不做缩放或截断。
pub const MAX_CLIPBOARD_IMAGE_BYTES: usize = 32 * 1024 * 1024;

/// 二进制负载的 wire 形态（`{ "base64": ... }`，与 `fs.readBinary` 同编码）。
pub fn binary_payload(bytes: &[u8]) -> Value {
    serde_json::json!({ "base64": encode_base64(bytes) })
}

/// 标准 base64 编码（协议里所有二进制通道共用一种编码）。
pub fn encode_base64(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// 剪贴板图片的 wire 形态（`{ "image": { base64, width, height } }`；无图片 = 空对象）。
pub fn clipboard_image_payload(image: Option<ClipboardImage>) -> Value {
    match image {
        Some(image) => serde_json::json!({ "image": image }),
        None => serde_json::json!({}),
    }
}

/// 运行时 → 壳的服务请求。与 [`PluginEvent`] 广播分属两条通道：服务请求必须无损送达——丢一条
/// 会让插件的调用永久悬挂，故走无界通道而非可落后的广播。
#[derive(Clone, Debug)]
pub enum HostRequest {
    Dialog { plugin_id: String, request_id: u64, dialog: DialogRequest },
    /// 插件请求接管当前聚焦窗口（scope = 整页 / 全接管，surface = 插件的贡献 id）；壳应用后经
    /// [`PluginRuntime::resolve_ui_call`] 回填结果（失败带可读原因）。
    Takeover { plugin_id: String, request_id: u64, scope: TakeoverScope, surface_id: String },
    /// 插件请求打开一个独立窗口承载自己的界面树。
    OpenWindow { plugin_id: String, request_id: u64, params: WindowOpenParams },
    /// 插件请求关闭自己打开的一个窗口。
    CloseWindow { plugin_id: String, request_id: u64, window_id: String },
    /// 插件请求发起 / 续聊一轮 AI 对话（须声明 `ai` 权限；壳编排后经
    /// [`PluginRuntime::resolve_shell_call`] 回填，事件随后经 `host:ai.chat.event` 流达插件）。
    ChatSend { plugin_id: String, request_id: u64, params: method::ChatSendParams },
    /// 插件请求中断一个会话的活跃 run。
    ChatStop { plugin_id: String, request_id: u64, params: method::ChatStopParams },
    /// 插件请求读会话清单或单会话消息历史。
    ChatHistory { plugin_id: String, request_id: u64, params: method::ChatHistoryParams },
    /// 插件请求读可用模型清单与面板当前选择。
    AiModels { plugin_id: String, request_id: u64 },
    /// 插件请求读 Agent 清单与面板当前生效项。
    AiAgents { plugin_id: String, request_id: u64 },
    /// 插件请求写系统剪贴板（壳在 Elm 循环外执行，结果经 [`PluginRuntime::resolve_shell_call`] 回填）。
    ClipboardWrite { plugin_id: String, request_id: u64, text: String },
    /// 插件请求读系统剪贴板（无文本内容时回空串）。
    ClipboardRead { plugin_id: String, request_id: u64 },
    /// 插件请求读当前仓库内一个文本文件（`path` 为相对仓库根路径）。
    VaultReadText { plugin_id: String, request_id: u64, path: String },
    /// 插件请求写当前仓库内一个文本文件（路径校验与打开中文件的写保护由壳执行）。
    VaultWriteText { plugin_id: String, request_id: u64, path: String, content: String },
    /// 插件请求读当前仓库内一个二进制文件（结果以 [`binary_payload`] 回填）。
    VaultReadBinary { plugin_id: String, request_id: u64, path: String },
    /// 插件请求写当前仓库内一个二进制文件（`bytes` 已在分派处解码并校验大小）。
    VaultWriteBinary { plugin_id: String, request_id: u64, path: String, bytes: Vec<u8> },
    /// 插件请求把私有目录内的一个文件收进仓库：`source` 已在分派处校验并规范化为绝对路径，
    /// 路径校验与打开中文件保护由壳执行（复制语义，源文件不删）。
    VaultImportFile { plugin_id: String, request_id: u64, path: String, source: String },
    /// 插件请求读剪贴板中的图片（结果以 [`ClipboardImage`] 形态回填，无图 = `null`）。
    ClipboardReadImage { plugin_id: String, request_id: u64 },
    /// 插件请求写一张 PNG 图片到系统剪贴板（`bytes` 已在分派处解码并校验大小）。
    ClipboardWriteImage { plugin_id: String, request_id: u64, bytes: Vec<u8> },
    /// 插件请求列当前仓库内一个目录（空路径 = 仓库根）。
    VaultList { plugin_id: String, request_id: u64, path: String },
    /// 插件请求删除仓库内一个路径（文件或文件夹；仓库根不可删）。
    VaultRemove { plugin_id: String, request_id: u64, path: String },
    /// 插件请求改名 / 移动仓库内一个路径（`from` / `to` 均为仓库相对路径）。
    VaultRename { plugin_id: String, request_id: u64, from: String, to: String },
    /// 插件请求判定当前仓库内一个文件是否存在。
    VaultExists { plugin_id: String, request_id: u64, path: String },
    /// 插件请求读当前打开表格的只读快照（真源在壳的表格会话）。
    TableSnapshot { plugin_id: String, request_id: u64 },
    /// 插件请求逐格写表格（经壳的表格会话与补丁 / 保存链，与本地编辑同出口）。
    TableUpdateCell {
        plugin_id: String,
        request_id: u64,
        row_id: String,
        field_id: String,
        value: Value,
    },
    /// 插件请求新增一行（结果回带宿主分配的新行 id）。
    TableAddRow { plugin_id: String, request_id: u64 },
    /// 插件请求删除一行。
    TableRemoveRow { plugin_id: String, request_id: u64, row_id: String },
    /// 插件请求写宿主表格选中态。
    TableSelectRow { plugin_id: String, request_id: u64, row_id: String },
    /// 插件请求向图片字段单元格写入图片引用（壳侧走与本地编辑同一出口）。
    TableSetCellImage {
        plugin_id: String,
        request_id: u64,
        params: method::TableSetCellImageParams,
    },
    /// 插件请求批量量算文字（`query.measure`）：逐条给出宽度 / 行高 / 行数。
    MeasureBatch { plugin_id: String, request_id: u64, params: method::MeasureBatchParams },
    /// 插件请求当前主题的颜色 / 字号档 / 圆角档（`query.theme`）。
    QueryTheme { plugin_id: String, request_id: u64 },
    /// 插件请求运行时改一个已打开窗口的属性（壳侧 OS 窗口副作用）。
    WindowUpdate { plugin_id: String, request_id: u64, params: method::WindowUpdateParams },
    /// 插件请求重新显示一个因失焦被隐藏的窗口。
    WindowShow { plugin_id: String, request_id: u64, window_id: String },
    /// 插件请求拖动承载某面的窗口（自绘标题栏）。
    WindowBeginDrag { plugin_id: String, request_id: u64, surface_id: String },
    /// 插件请求重命名一个会话（写元数据标题）。
    ChatRename { plugin_id: String, request_id: u64, params: method::ChatRenameParams },
    /// 插件请求删除一个会话（连同其消息文件）。
    ChatDelete { plugin_id: String, request_id: u64, params: method::ChatSessionParams },
    /// 插件请求压缩一个会话（回执 = 已受理，结果经 `ai.chat.history` 复核）。
    ChatCompact { plugin_id: String, request_id: u64, params: method::ChatSessionParams },
    /// 插件请求读当前仓库内一张图片的固有像素尺寸。
    VaultImageInfo { plugin_id: String, request_id: u64, path: String },
    /// 插件请求整发整换自己的托盘菜单（条目存于壳的托盘服务）。
    TraySetItems { plugin_id: String, request_id: u64, params: method::SetTrayItemsParams },
    /// 插件请求当前空间的连接状态与在场成员。
    CollabPeers { plugin_id: String, request_id: u64 },
    /// 插件请求向同空间的对等插件发一条消息。
    CollabSend { plugin_id: String, request_id: u64, params: method::CollabSendParams },
    /// 插件请求当前打开笔记的选区（真源在壳的编辑面会话）。
    EditorSelection { plugin_id: String, request_id: u64 },
    /// 插件请求用给定文本替换当前选区（空串 = 删除选区；写入经笔记同一套保存链）。
    EditorReplaceSelection { plugin_id: String, request_id: u64, text: String },
    /// 插件请求读当前空间的团队元数据（`key` 缺省 = 全部键；仅协作空间可用）。
    CollabMetaGet { plugin_id: String, request_id: u64, params: method::CollabMetaGetParams },
    /// 插件请求按键写当前空间的团队元数据（只改给出的键；只读身份由壳拒绝）。
    CollabMetaPatch { plugin_id: String, request_id: u64, params: method::CollabMetaPatchParams },
    /// 插件请求某一天（本地日期 `YYYY-MM-DD`）的仓库历史版本记录。
    HistoryDay { plugin_id: String, request_id: u64, params: method::HistoryDayParams },
    /// 插件请求打开某文件的仓库历史弹层（界面动作，回执 = 已受理）。
    HistoryOpen { plugin_id: String, request_id: u64, params: method::HistoryOpenParams },
    /// 插件请求按天聚合的仓库活动（只有本地仓库有本地历史侧文件）。
    HistoryActivity { plugin_id: String, request_id: u64 },
}

/// 插件列表项（设置页渲染用）。
#[derive(Clone, Debug)]
pub struct PluginSummary {
    pub id: String,
    pub name: String,
    pub version: String,
    pub api_version: u32,
    pub permissions: Vec<Permission>,
    pub singleton: bool,
    pub keep_mounted_on_vault_switch: bool,
    /// 是否启用（`.enabled/` 标记存在）。
    pub enabled: bool,
    pub status: PluginStatus,
    /// 不可装载的可读原因（None = 可装载）。
    pub load_error: Option<String>,
    /// 实际用到但未在 manifest 声明过的能力调用条数。
    pub undeclared: usize,
    /// 已接收的整面帧提交次数。
    pub submits: u64,
    /// 安装来源目录（本地目录安装时记下的绝对路径；zip / 市场安装为 `None`）。
    pub source: Option<String>,
    /// 随包预置：内容由发行包的 `preset-plugins/` 铺盘（设置页归入内置插件）。
    pub preset: bool,
}

/// 一次安装的结果：内容已落位。`start_error` = 曾启用的插件未能用新内容启动——
/// 内容装上了、只是没跑起来（与「安装失败」是两回事，提示与重试入口都不同）。
#[derive(Clone, Debug)]
pub struct InstallOutcome {
    pub id: String,
    pub start_error: Option<String>,
}

/// 磁盘上发现的一个插件。
#[derive(Clone)]
struct Discovered {
    manifest: Result<Manifest, String>,
    dir: PathBuf,
    enabled: bool,
}

/// 对话框结果回传通道（用户选定值；取消 = `None`）。
type DialogReplyTx = std::sync::mpsc::Sender<Result<Option<String>, String>>;

/// 在途对话框等待项：发起插件 id + 结果回传通道。
type DialogWait = (String, DialogReplyTx);

/// 界面服务（接管 / 插件窗口）结果回传通道。
type UiReplyTx = std::sync::mpsc::Sender<Result<(), String>>;

/// 在途界面服务等待项：发起插件 id + 结果回传通道。
type UiCallWait = (String, UiReplyTx);

/// 壳侧服务调用结果回传通道（AI 对话 / 剪贴板 / 仓库读写共用，载荷为可序列化结果）。
type ShellReplyTx = std::sync::mpsc::Sender<Result<Value, String>>;

/// 在途壳侧服务调用：发起插件 id + 结果回传通道。
type ShellCallWait = (String, ShellReplyTx);

/// 插件宿主运行时。
pub struct PluginRuntime {
    root: PathBuf,
    /// 宿主产品版本：比较插件 `minHostVersion` 与握手自报宿主信息用。
    /// core 自身的包版本与产品版本不同，故由壳在启动时传入（默认取 core 包版本）。
    host_version: String,
    process_host: Arc<PluginProcessHost>,
    shortcuts: Arc<shortcut::Registrar>,
    events: tokio::sync::broadcast::Sender<PluginEvent>,
    host_requests: tokio::sync::mpsc::UnboundedSender<HostRequest>,
    /// 服务请求接收端（壳侧订阅流持有；见 [`PluginRuntime::recv_host_request`]）。
    host_requests_rx: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<HostRequest>>,
    /// 在途对话框：request_id → 等待项。插件进程的读线程在此等待壳回填结果。
    dialogs: Mutex<HashMap<u64, DialogWait>>,
    next_dialog_id: AtomicU64,
    /// 在途界面服务请求（接管 / 插件窗口）：request_id → 等待项。
    ui_calls: Mutex<HashMap<u64, UiCallWait>>,
    next_ui_call_id: AtomicU64,
    /// 在途壳侧服务调用（AI 对话 / 剪贴板 / 仓库读写）：request_id → 等待项。
    shell_calls: Mutex<HashMap<u64, ShellCallWait>>,
    next_shell_call_id: AtomicU64,
    /// 已注册的插件 AI 工具（工具名 → 声明；名在内置工具与其他插件之间全局唯一）。
    tools: Mutex<HashMap<String, PluginToolSpec>>,
    /// 在途 AI 工具调用：callId → 等待项。
    tool_calls: Mutex<HashMap<String, ToolCallWait>>,
    next_tool_call_id: AtomicU64,
    /// 服务目录：服务名 → 活动提供者与候选（能力总线的单例型语义）。
    services: Mutex<ServiceDir>,
    /// 在途跨插件服务调用：callId → (提供者 id, 结果回传通道)。
    service_calls: Mutex<HashMap<String, ServiceCallWait>>,
    next_service_call_id: AtomicU64,
    /// 跨插件转发的执行器（首次转发时建立；读线程上没有 tokio 上下文可借）。
    service_runtime: OnceLock<tokio::runtime::Handle>,
    /// 当前容器身份（见 [`PluginRuntime::on_vault_switch`]）：同一容器重复进入不动作，据此判别首次激活。
    container: Mutex<Option<String>>,
    /// 最近一次切容器 payload：迟到订阅 `vault:switch` 的插件由此补发当前容器身份。
    last_vault_switch: Mutex<Option<VaultSwitchPayload>>,
    /// 最近一次 `table:changed` 广播的表格路径：插件迟到订阅时补发当前状态（否则订阅前的
    /// 广播已丢失，插件只能盲等下一次变更）。
    current_table: Mutex<Option<String>>,
    discovered: Mutex<HashMap<String, Discovered>>,
    instances: Mutex<HashMap<String, Arc<Instance>>>,
}

impl PluginRuntime {
    /// 建立运行时（`root` = 应用数据目录下 `plugins/`）。顺带清理遗留的安装临时目录。
    /// 宿主版本默认取 core 包版本——产品壳应改用 [`PluginRuntime::with_host_version`] 传入应用版本。
    pub fn new(root: PathBuf) -> Arc<Self> {
        Self::with_host_version(root, env!("CARGO_PKG_VERSION"))
    }

    /// 建立运行时并显式给出宿主产品版本（`minHostVersion` 校验与握手自报都用它）。
    pub fn with_host_version(root: PathBuf, host_version: impl Into<String>) -> Arc<Self> {
        layout::sweep_temp(&root);
        frame_region::sweep_stale_regions();
        let (events, _) = tokio::sync::broadcast::channel(256);
        let (host_requests, host_requests_rx) = tokio::sync::mpsc::unbounded_channel();
        let runtime = Arc::new(Self {
            root,
            host_version: host_version.into(),
            process_host: Arc::new(PluginProcessHost::new()),
            shortcuts: shortcut::Registrar::new(),
            events,
            host_requests,
            host_requests_rx: tokio::sync::Mutex::new(host_requests_rx),
            dialogs: Mutex::new(HashMap::new()),
            next_dialog_id: AtomicU64::new(1),
            ui_calls: Mutex::new(HashMap::new()),
            next_ui_call_id: AtomicU64::new(1),
            shell_calls: Mutex::new(HashMap::new()),
            next_shell_call_id: AtomicU64::new(1),
            tools: Mutex::new(HashMap::new()),
            tool_calls: Mutex::new(HashMap::new()),
            next_tool_call_id: AtomicU64::new(1),
            services: Mutex::new(ServiceDir::new()),
            service_calls: Mutex::new(HashMap::new()),
            next_service_call_id: AtomicU64::new(1),
            service_runtime: OnceLock::new(),
            container: Mutex::new(None),
            last_vault_switch: Mutex::new(None),
            current_table: Mutex::new(None),
            discovered: Mutex::new(HashMap::new()),
            instances: Mutex::new(HashMap::new()),
        });
        // 快捷键触发在 OS 消息线程上到达：经注册器的派发线程回查归属后转投对应插件
        let weak = Arc::downgrade(&runtime);
        runtime.shortcuts.set_sink(Arc::new(move |plugin_id, accelerator| {
            if let Some(runtime) = weak.upgrade() {
                runtime.notify_shortcut(&plugin_id, &accelerator);
            }
        }));
        runtime
    }

    /// 订阅运行时事件；壳把它接成订阅流（内层用广播，重复订阅 / 订阅重建都安全）。
    pub fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<PluginEvent> {
        self.events.subscribe()
    }

    /// 取一条待处理的服务请求（对话框等）；壳把它接成订阅流。返回 `None` = 运行时已销毁。
    /// 无界通道不丢帧：请求被落到这里之前就已在队列里，壳无需担心「收不到」。
    pub async fn recv_host_request(&self) -> Option<HostRequest> {
        self.host_requests_rx.lock().await.recv().await
    }

    /// 登记一条对话框请求并把请求交给壳，返回结果接收端。调用方在结果到达后自行回执调用方——
    /// 对话框由用户决定耗时，不得占住插件进程的输出读线程（否则该期间插件输出与退出上报都被拖住）。
    /// 插件停用 / 崩溃 / 宿主退出会经 [`PluginRuntime::fail_dialogs_of`] / [`PluginRuntime::shutdown`]
    /// 把在途请求以可读失败收场，不会永久悬挂。
    pub(crate) fn open_dialog(
        &self,
        plugin_id: &str,
        dialog: DialogRequest,
    ) -> Result<std::sync::mpsc::Receiver<Result<Option<String>, String>>, String> {
        let request_id = self.next_dialog_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = std::sync::mpsc::channel();
        self.dialogs
            .lock()
            .unwrap()
            .insert(request_id, (plugin_id.to_string(), tx));
        let request = HostRequest::Dialog {
            plugin_id: plugin_id.to_string(),
            request_id,
            dialog,
        };
        if self.host_requests.send(request).is_err() {
            self.dialogs.lock().unwrap().remove(&request_id);
            return Err("对话框不可用：宿主已退出".to_string());
        }
        Ok(rx)
    }

    /// 壳侧回填对话框结果（用户选定路径；取消 = `None`）。未登记的 id 是空操作
    /// （插件已停用 / 已崩溃时在途请求已由 [`PluginRuntime::fail_dialogs_of`] 收场）。
    pub fn resolve_dialog(&self, request_id: u64, path: Option<String>) {
        let waiter = self.dialogs.lock().unwrap().remove(&request_id);
        if let Some((_, tx)) = waiter {
            let _ = tx.send(Ok(path));
        }
    }

    /// 让某插件全部在途对话框以可读失败收场（插件停用 / 失败 / 崩溃时调用）。
    pub(crate) fn fail_dialogs_of(&self, plugin_id: &str, reason: &str) {
        let ids: Vec<u64> = {
            let dialogs = self.dialogs.lock().unwrap();
            dialogs
                .iter()
                .filter(|(_, (owner, _))| owner == plugin_id)
                .map(|(id, _)| *id)
                .collect()
        };
        for id in ids {
            let waiter = self.dialogs.lock().unwrap().remove(&id);
            if let Some((_, tx)) = waiter {
                let _ = tx.send(Err(reason.to_string()));
            }
        }
    }

    /// 让全部在途对话框以可读失败收场（宿主退出收尾；逐插件路径已先收一遍，此处防漏网）。
    fn fail_all_dialogs(&self, reason: &str) {
        let waiters: Vec<std::sync::mpsc::Sender<Result<Option<String>, String>>> = {
            let mut dialogs = self.dialogs.lock().unwrap();
            dialogs.drain().map(|(_, (_, tx))| tx).collect()
        };
        for tx in waiters {
            let _ = tx.send(Err(reason.to_string()));
        }
    }

    /// 登记一个界面服务请求（接管 / 插件窗口）并把请求交给壳，返回结果接收端。
    fn open_ui_call(&self, request: HostRequest) -> Result<std::sync::mpsc::Receiver<Result<(), String>>, String> {
        let request_id = self.next_ui_call_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = std::sync::mpsc::channel();
        let request = match request {
            HostRequest::Takeover { plugin_id, scope, surface_id, .. } => HostRequest::Takeover {
                plugin_id,
                request_id,
                scope,
                surface_id,
            },
            HostRequest::OpenWindow { plugin_id, params, .. } => HostRequest::OpenWindow {
                plugin_id,
                request_id,
                params,
            },
            HostRequest::CloseWindow { plugin_id, window_id, .. } => HostRequest::CloseWindow {
                plugin_id,
                request_id,
                window_id,
            },
            HostRequest::WindowUpdate { plugin_id, params, .. } => HostRequest::WindowUpdate {
                plugin_id,
                request_id,
                params,
            },
            HostRequest::WindowBeginDrag { plugin_id, surface_id, .. } => {
                HostRequest::WindowBeginDrag { plugin_id, request_id, surface_id }
            }
            HostRequest::WindowShow { plugin_id, window_id, .. } => HostRequest::WindowShow {
                plugin_id,
                request_id,
                window_id,
            },
            HostRequest::Dialog { .. } => return Err("界面服务请求形态不符".to_string()),
            HostRequest::ChatSend { .. }
            | HostRequest::ChatStop { .. }
            | HostRequest::ChatHistory { .. }
            | HostRequest::ChatRename { .. }
            | HostRequest::ChatDelete { .. }
            | HostRequest::ChatCompact { .. }
            | HostRequest::AiModels { .. }
            | HostRequest::AiAgents { .. }
            | HostRequest::ClipboardWrite { .. }
            | HostRequest::ClipboardRead { .. }
            | HostRequest::ClipboardReadImage { .. }
            | HostRequest::ClipboardWriteImage { .. }
            | HostRequest::VaultReadText { .. }
            | HostRequest::VaultWriteText { .. }
            | HostRequest::VaultReadBinary { .. }
            | HostRequest::VaultWriteBinary { .. }
            | HostRequest::VaultImportFile { .. }
            | HostRequest::VaultList { .. }
            | HostRequest::VaultExists { .. }
            | HostRequest::VaultRemove { .. }
            | HostRequest::VaultRename { .. }
            | HostRequest::TableSnapshot { .. }
            | HostRequest::TableUpdateCell { .. }
            | HostRequest::TableAddRow { .. }
            | HostRequest::TableRemoveRow { .. }
            | HostRequest::TableSelectRow { .. }
            | HostRequest::TableSetCellImage { .. }
            | HostRequest::MeasureBatch { .. }
            | HostRequest::QueryTheme { .. }
            | HostRequest::VaultImageInfo { .. }
            | HostRequest::TraySetItems { .. }
            | HostRequest::CollabPeers { .. }
            | HostRequest::CollabSend { .. }
            | HostRequest::CollabMetaGet { .. }
            | HostRequest::CollabMetaPatch { .. }
            | HostRequest::EditorSelection { .. }
            | HostRequest::EditorReplaceSelection { .. }
            | HostRequest::HistoryDay { .. }
            | HostRequest::HistoryOpen { .. }
            | HostRequest::HistoryActivity { .. } => {
                return Err("界面服务请求形态不符".to_string())
            }
        };
        self.ui_calls
            .lock()
            .unwrap()
            .insert(request_id, (request_plugin_id(&request).to_string(), tx));
        if self.host_requests.send(request).is_err() {
            self.ui_calls.lock().unwrap().remove(&request_id);
            return Err("界面服务不可用：宿主已退出".to_string());
        }
        Ok(rx)
    }

    /// 壳侧回填界面服务请求结果（接管已应用 / 窗口已创建或关闭；失败带可读原因）。
    pub fn resolve_ui_call(&self, request_id: u64, result: Result<(), String>) {
        let waiter = self.ui_calls.lock().unwrap().remove(&request_id);
        if let Some((_, tx)) = waiter {
            let _ = tx.send(result);
        }
    }

    /// 让某插件全部在途界面服务请求以可读失败收场（插件停用 / 失败 / 崩溃时调用）。
    pub(crate) fn fail_ui_calls_of(&self, plugin_id: &str, reason: &str) {
        let ids: Vec<u64> = {
            let ui_calls = self.ui_calls.lock().unwrap();
            ui_calls
                .iter()
                .filter(|(_, (owner, _))| owner == plugin_id)
                .map(|(id, _)| *id)
                .collect()
        };
        for id in ids {
            let waiter = self.ui_calls.lock().unwrap().remove(&id);
            if let Some((_, tx)) = waiter {
                let _ = tx.send(Err(reason.to_string()));
            }
        }
    }

    /// 登记一个壳侧服务调用并把请求交给壳，返回结果接收端。
    fn open_shell_call(&self, request: HostRequest) -> Result<std::sync::mpsc::Receiver<Result<Value, String>>, String> {
        let request_id = self.next_shell_call_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = std::sync::mpsc::channel();
        let request = match request {
            HostRequest::ChatSend { plugin_id, params, .. } => HostRequest::ChatSend {
                plugin_id,
                request_id,
                params,
            },
            HostRequest::ChatStop { plugin_id, params, .. } => HostRequest::ChatStop {
                plugin_id,
                request_id,
                params,
            },
            HostRequest::ChatHistory { plugin_id, params, .. } => HostRequest::ChatHistory {
                plugin_id,
                request_id,
                params,
            },
            HostRequest::AiModels { plugin_id, .. } => HostRequest::AiModels {
                plugin_id,
                request_id,
            },
            HostRequest::AiAgents { plugin_id, .. } => HostRequest::AiAgents {
                plugin_id,
                request_id,
            },
            HostRequest::ClipboardWrite { plugin_id, text, .. } => HostRequest::ClipboardWrite {
                plugin_id,
                request_id,
                text,
            },
            HostRequest::ClipboardRead { plugin_id, .. } => HostRequest::ClipboardRead {
                plugin_id,
                request_id,
            },
            HostRequest::ClipboardReadImage { plugin_id, .. } => HostRequest::ClipboardReadImage {
                plugin_id,
                request_id,
            },
            HostRequest::ClipboardWriteImage { plugin_id, bytes, .. } => {
                HostRequest::ClipboardWriteImage {
                    plugin_id,
                    request_id,
                    bytes,
                }
            }
            HostRequest::VaultReadText { plugin_id, path, .. } => HostRequest::VaultReadText {
                plugin_id,
                request_id,
                path,
            },
            HostRequest::VaultWriteText { plugin_id, path, content, .. } => HostRequest::VaultWriteText {
                plugin_id,
                request_id,
                path,
                content,
            },
            HostRequest::VaultReadBinary { plugin_id, path, .. } => HostRequest::VaultReadBinary {
                plugin_id,
                request_id,
                path,
            },
            HostRequest::VaultImportFile { plugin_id, path, source, .. } => {
                HostRequest::VaultImportFile {
                    plugin_id,
                    request_id,
                    path,
                    source,
                }
            }
            HostRequest::VaultWriteBinary { plugin_id, path, bytes, .. } => {
                HostRequest::VaultWriteBinary {
                    plugin_id,
                    request_id,
                    path,
                    bytes,
                }
            }
            HostRequest::VaultList { plugin_id, path, .. } => HostRequest::VaultList {
                plugin_id,
                request_id,
                path,
            },
            HostRequest::VaultExists { plugin_id, path, .. } => HostRequest::VaultExists {
                plugin_id,
                request_id,
                path,
            },
            HostRequest::VaultRemove { plugin_id, path, .. } => HostRequest::VaultRemove {
                plugin_id,
                request_id,
                path,
            },
            HostRequest::VaultRename { plugin_id, from, to, .. } => HostRequest::VaultRename {
                plugin_id,
                request_id,
                from,
                to,
            },
            HostRequest::TableSnapshot { plugin_id, .. } => HostRequest::TableSnapshot {
                plugin_id,
                request_id,
            },
            HostRequest::TableUpdateCell { plugin_id, row_id, field_id, value, .. } => {
                HostRequest::TableUpdateCell {
                    plugin_id,
                    request_id,
                    row_id,
                    field_id,
                    value,
                }
            }
            HostRequest::TableAddRow { plugin_id, .. } => HostRequest::TableAddRow {
                plugin_id,
                request_id,
            },
            HostRequest::TableRemoveRow { plugin_id, row_id, .. } => HostRequest::TableRemoveRow {
                plugin_id,
                request_id,
                row_id,
            },
            HostRequest::TableSelectRow { plugin_id, row_id, .. } => HostRequest::TableSelectRow {
                plugin_id,
                request_id,
                row_id,
            },
            HostRequest::TableSetCellImage { plugin_id, params, .. } => HostRequest::TableSetCellImage {
                plugin_id,
                request_id,
                params,
            },
            HostRequest::MeasureBatch { plugin_id, params, .. } => HostRequest::MeasureBatch {
                plugin_id,
                request_id,
                params,
            },
            HostRequest::QueryTheme { plugin_id, .. } => HostRequest::QueryTheme {
                plugin_id,
                request_id,
            },
            HostRequest::ChatRename { plugin_id, params, .. } => HostRequest::ChatRename {
                plugin_id,
                request_id,
                params,
            },
            HostRequest::ChatDelete { plugin_id, params, .. } => HostRequest::ChatDelete {
                plugin_id,
                request_id,
                params,
            },
            HostRequest::ChatCompact { plugin_id, params, .. } => HostRequest::ChatCompact {
                plugin_id,
                request_id,
                params,
            },
            HostRequest::VaultImageInfo { plugin_id, path, .. } => HostRequest::VaultImageInfo {
                plugin_id,
                request_id,
                path,
            },
            HostRequest::TraySetItems { plugin_id, params, .. } => HostRequest::TraySetItems {
                plugin_id,
                request_id,
                params,
            },
            HostRequest::CollabPeers { plugin_id, .. } => HostRequest::CollabPeers {
                plugin_id,
                request_id,
            },
            HostRequest::CollabSend { plugin_id, params, .. } => HostRequest::CollabSend {
                plugin_id,
                request_id,
                params,
            },
            HostRequest::CollabMetaGet { plugin_id, params, .. } => HostRequest::CollabMetaGet {
                plugin_id,
                request_id,
                params,
            },
            HostRequest::CollabMetaPatch { plugin_id, params, .. } => {
                HostRequest::CollabMetaPatch {
                    plugin_id,
                    request_id,
                    params,
                }
            }
            HostRequest::EditorSelection { plugin_id, .. } => HostRequest::EditorSelection {
                plugin_id,
                request_id,
            },
            HostRequest::EditorReplaceSelection { plugin_id, text, .. } => {
                HostRequest::EditorReplaceSelection {
                    plugin_id,
                    request_id,
                    text,
                }
            }
            HostRequest::HistoryDay { plugin_id, params, .. } => HostRequest::HistoryDay {
                plugin_id,
                request_id,
                params,
            },
            HostRequest::HistoryOpen { plugin_id, params, .. } => HostRequest::HistoryOpen {
                plugin_id,
                request_id,
                params,
            },
            HostRequest::HistoryActivity { plugin_id, .. } => HostRequest::HistoryActivity {
                plugin_id,
                request_id,
            },
            _ => return Err("壳侧服务请求形态不符".to_string()),
        };
        self.shell_calls
            .lock()
            .unwrap()
            .insert(request_id, (request_plugin_id(&request).to_string(), tx));
        if self.host_requests.send(request).is_err() {
            self.shell_calls.lock().unwrap().remove(&request_id);
            return Err("壳侧服务不可用：宿主已退出".to_string());
        }
        Ok(rx)
    }

    /// 壳侧回填一次服务调用的结果（AI 对话：会话 / 消息 id、清单或历史、模型 / Agent 清单；
    /// 剪贴板：文本或图片；仓库：文件内容 / 写入完成 / 目录清单 / 存在性 / 图片尺寸 / 改名与删除回执；
    /// 表格：快照与写入回执；托盘：条目已应用；协作：在场清单 / 消息已投递）。失败带可读原因。
    pub fn resolve_shell_call(&self, request_id: u64, result: Result<Value, String>) {
        let waiter = self.shell_calls.lock().unwrap().remove(&request_id);
        if let Some((_, tx)) = waiter {
            let _ = tx.send(result);
        }
    }

    /// 让某插件全部在途壳侧服务调用以可读失败收场（插件停用 / 失败 / 崩溃时调用）。
    pub(crate) fn fail_shell_calls_of(&self, plugin_id: &str, reason: &str) {
        let ids: Vec<u64> = {
            let shell_calls = self.shell_calls.lock().unwrap();
            shell_calls
                .iter()
                .filter(|(_, (owner, _))| owner == plugin_id)
                .map(|(id, _)| *id)
                .collect()
        };
        for id in ids {
            let waiter = self.shell_calls.lock().unwrap().remove(&id);
            if let Some((_, tx)) = waiter {
                let _ = tx.send(Err(reason.to_string()));
            }
        }
    }

    /// 向发起会话的插件下发一条对话事件（`host:ai.chat.event`）。进程未在跑时静默丢弃
    /// （插件停用后的 run 不中断——会话归仓库，事件随进程消失）。
    pub fn notify_chat_event(&self, plugin_id: &str, params: &method::ChatEventParams) {
        let instance = {
            let instances = self.instances.lock().unwrap();
            instances.get(plugin_id).cloned()
        };
        if let Some(instance) = instance {
            instance.notify_chat_event(params);
        }
    }

    /// 让全部在途界面服务请求以可读失败收场（宿主退出收尾）。
    fn fail_all_ui_calls(&self, reason: &str) {
        let waiters: Vec<UiReplyTx> = {
            let mut ui_calls = self.ui_calls.lock().unwrap();
            ui_calls.drain().map(|(_, (_, tx))| tx).collect()
        };
        for tx in waiters {
            let _ = tx.send(Err(reason.to_string()));
        }
    }

    /// 让全部在途壳侧服务调用以可读失败收场（宿主退出收尾）。
    fn fail_all_shell_calls(&self, reason: &str) {
        let waiters: Vec<ShellReplyTx> = {
            let mut shell_calls = self.shell_calls.lock().unwrap();
            shell_calls.drain().map(|(_, (_, tx))| tx).collect()
        };
        for tx in waiters {
            let _ = tx.send(Err(reason.to_string()));
        }
    }

    // ---------- 界面服务请求入口（instance 调用） ----------

    /// 插件请求接管当前聚焦窗口（`surface_id` 须为已注册的贡献 id：接管面与该贡献共享同一棵提交树）。
    fn open_takeover(
        &self,
        plugin_id: &str,
        scope: TakeoverScope,
        surface_id: &str,
    ) -> Result<std::sync::mpsc::Receiver<Result<(), String>>, String> {
        if surface_id.trim().is_empty() {
            return Err("接管的贡献 id 不能为空".to_string());
        }
        let registered = self
            .instances
            .lock()
            .unwrap()
            .get(plugin_id)
            .map(|i| i.contributions().iter().any(|c| c.id == surface_id))
            .unwrap_or(false);
        if !registered {
            return Err(format!("接管的贡献未注册：{surface_id}"));
        }
        self.open_ui_call(HostRequest::Takeover {
            plugin_id: plugin_id.to_string(),
            request_id: 0,
            scope,
            surface_id: surface_id.to_string(),
        })
    }

    /// 插件请求打开一个独立窗口（重复 id 由壳拒绝并回填可读原因）。
    fn open_plugin_window(&self, plugin_id: &str, params: WindowOpenParams) -> Result<std::sync::mpsc::Receiver<Result<(), String>>, String> {
        if params.id.trim().is_empty() {
            return Err("窗口 id 不能为空".to_string());
        }
        if params.id.contains(':') {
            return Err(format!("窗口 id 不得含 `:`：{}", params.id));
        }
        if params.title.trim().is_empty() {
            return Err("窗口标题不能为空".to_string());
        }
        self.open_ui_call(HostRequest::OpenWindow {
            plugin_id: plugin_id.to_string(),
            request_id: 0,
            params,
        })
    }

    /// 插件请求关闭自己打开的一个窗口（未打开的 id 由壳拒绝并回填可读原因）。
    fn close_plugin_window(&self, plugin_id: &str, window_id: &str) -> Result<std::sync::mpsc::Receiver<Result<(), String>>, String> {
        self.open_ui_call(HostRequest::CloseWindow {
            plugin_id: plugin_id.to_string(),
            request_id: 0,
            window_id: window_id.to_string(),
        })
    }

    /// 插件请求运行时改一个已打开窗口的属性（无字段可改即拒；窗口不存在由壳拒绝并回填）。
    fn update_plugin_window(&self, plugin_id: &str, params: WindowUpdateParams) -> Result<std::sync::mpsc::Receiver<Result<(), String>>, String> {
        if params.id.trim().is_empty() {
            return Err("窗口 id 不能为空".to_string());
        }
        if params.width.is_none() && params.height.is_none() && params.always_on_top.is_none() {
            return Err("window.update 未给出任何要改的属性".to_string());
        }
        self.open_ui_call(HostRequest::WindowUpdate {
            plugin_id: plugin_id.to_string(),
            request_id: 0,
            params,
        })
    }

    /// 插件请求重新显示一个因失焦被隐藏的窗口（窗口不存在由壳拒绝并回填）。
    fn show_plugin_window(&self, plugin_id: &str, window_id: &str) -> Result<std::sync::mpsc::Receiver<Result<(), String>>, String> {
        if window_id.trim().is_empty() {
            return Err("窗口 id 不能为空".to_string());
        }
        self.open_ui_call(HostRequest::WindowShow {
            plugin_id: plugin_id.to_string(),
            request_id: 0,
            window_id: window_id.to_string(),
        })
    }

    /// 插件请求拖动承载某面的窗口（面归属的窗口由壳解析）。
    fn begin_window_drag(&self, plugin_id: &str, surface_id: &str) -> Result<std::sync::mpsc::Receiver<Result<(), String>>, String> {
        if surface_id.trim().is_empty() {
            return Err("surfaceId 不能为空".to_string());
        }
        self.open_ui_call(HostRequest::WindowBeginDrag {
            plugin_id: plugin_id.to_string(),
            request_id: 0,
            surface_id: surface_id.to_string(),
        })
    }

    // ---------- 领域文档类型 ----------

    /// 全部已安装插件注册的领域文档类型（按插件 id + 扩展名排序；文件树 / 新建菜单 / 打开路由用）。
    pub fn doc_types(&self) -> Vec<DocTypeInfo> {
        let discovered = self.discovered.lock().unwrap();
        let mut all: Vec<DocTypeInfo> = discovered
            .iter()
            .filter_map(|(id, entry)| {
                let manifest = entry.manifest.as_ref().ok()?;
                Some(
                    manifest
                        .doc_type_registrations
                        .iter()
                        .map(|reg| DocTypeInfo {
                            plugin_id: id.clone(),
                            registration: reg.clone(),
                        })
                        .collect::<Vec<_>>(),
                )
            })
            .flatten()
            .collect();
        all.sort_by(|a, b| {
            (&a.plugin_id, &a.registration.extension).cmp(&(&b.plugin_id, &b.registration.extension))
        });
        all
    }

    /// 告知插件一个文档实例已打开（未在跑时静默丢弃；插件重启后由壳按打开中的实例重发）。
    pub fn notify_doc_open(&self, plugin_id: &str, surface_id: &str, path: &str) {
        let instance = self.instances.lock().unwrap().get(plugin_id).cloned();
        if let Some(instance) = instance {
            instance.notify_doc_open(surface_id, path);
        }
    }

    /// 告知插件一个文档实例已关闭。
    pub fn notify_doc_close(&self, plugin_id: &str, surface_id: &str, path: &str) {
        let instance = self.instances.lock().unwrap().get(plugin_id).cloned();
        if let Some(instance) = instance {
            instance.notify_doc_close(surface_id, path);
        }
    }

    /// 告知插件一个它打开的窗口已被用户关闭（未在跑时静默丢弃）。
    pub fn notify_window_closed(&self, plugin_id: &str, window_id: &str) {
        let instance = self.instances.lock().unwrap().get(plugin_id).cloned();
        if let Some(instance) = instance {
            instance.notify_window_closed(window_id);
        }
    }

    /// 扫描磁盘：刷新已安装清单与启用标记，并停掉磁盘上已消失的插件实例。
    pub fn scan(self: &Arc<Self>) {
        let mut discovered = HashMap::new();
        for (name, dir) in layout::discover(&self.root) {
            let manifest = manifest::read_manifest(&dir)
                .and_then(|m| {
                    if m.id == name {
                        Ok(m)
                    } else {
                        Err(format!("目录名与 manifest id 不一致：目录 {name}，声明 {}", m.id))
                    }
                })
                // 版本闸门在装载期判：宿主产品版本低于插件声明的最低版本即不可装载
                .and_then(|m| manifest::check_host_version(&m, &self.host_version).map(|()| m));
            let enabled = layout::is_enabled(&self.root, &name);
            discovered.insert(name.clone(), Discovered { manifest, dir, enabled });
        }
        *self.discovered.lock().unwrap() = discovered;

        let stale: Vec<String> = {
            let discovered = self.discovered.lock().unwrap();
            let instances = self.instances.lock().unwrap();
            instances
                .keys()
                .filter(|id| !discovered.contains_key(id.as_str()))
                .cloned()
                .collect()
        };
        for id in stale {
            let instance = self.instances.lock().unwrap().remove(&id);
            if let Some(instance) = instance {
                instance.stop();
            }
        }
    }

    /// 启动全部已启用插件（启动期与扫描后调用）。
    pub fn start_enabled(self: &Arc<Self>) {
        let targets: Vec<(String, PathBuf, Manifest)> = {
            let discovered = self.discovered.lock().unwrap();
            discovered
                .iter()
                .filter(|(_, d)| d.enabled)
                .filter_map(|(id, d)| {
                    d.manifest.as_ref().ok().map(|m| (id.clone(), d.dir.clone(), m.clone()))
                })
                .collect()
        };
        for (id, dir, manifest) in targets {
            let instance = self.instance_for(&manifest, &dir);
            if let Err(e) = instance.start() {
                eprintln!("[plugin {id}] 启动失败：{e}");
            }
        }
    }


    pub fn surface_frame(
        &self,
        plugin_id: &str,
        surface_id: &str,
    ) -> Option<std::sync::Arc<atelyx_plugin_protocol::frame::SurfaceFrame>> {
        self.instances
            .lock()
            .unwrap()
            .get(plugin_id)
            .and_then(|i| i.surface_frame(surface_id))
    }

    /// 登记一次改宽等帧：须在向插件发 `Resize` 之前登记。登记期间退回的帧由
    /// [`PluginRuntime::finish_surface_wait`] 取走画上，运行时不再为它发 `ViewSubmitted`
    /// （一条通知会让 iced 整窗重建一遍，拖动期间那等于每拍白重建一次）。
    pub fn begin_surface_wait(
        &self,
        plugin_id: &str,
        surface_id: &str,
        seen: std::sync::Arc<atelyx_plugin_protocol::frame::SurfaceFrame>,
    ) {
        let instance = self.instances.lock().unwrap().get(plugin_id).cloned();
        if let Some(instance) = instance {
            if matches!(instance.status(), PluginStatus::Running) {
                instance.begin_frame_wait(surface_id, seen);
            }
        }
    }

    /// 等登记过的面出现一张新帧（改宽那一拍同步取新帧用）。插件不在运行态、未登记、
    /// 超时都返回 `None`，调用方按手上的帧画。
    pub fn finish_surface_wait(
        &self,
        plugin_id: &str,
        surface_id: &str,
        timeout: std::time::Duration,
    ) -> Option<std::sync::Arc<atelyx_plugin_protocol::frame::SurfaceFrame>> {
        let instance = self.instances.lock().unwrap().get(plugin_id).cloned()?;
        if !matches!(instance.status(), PluginStatus::Running) {
            return None;
        }
        instance.finish_frame_wait(surface_id, timeout)
    }

    /// 等某面出现一张不同于 `seen` 的新帧（登记与等待一步完成，供不需要抢先登记的调用方与测试）。
    pub fn wait_surface_frame(
        &self,
        plugin_id: &str,
        surface_id: &str,
        seen: &std::sync::Arc<atelyx_plugin_protocol::frame::SurfaceFrame>,
        timeout: std::time::Duration,
    ) -> Option<std::sync::Arc<atelyx_plugin_protocol::frame::SurfaceFrame>> {
        let instance = self.instances.lock().unwrap().get(plugin_id).cloned()?;
        if !matches!(instance.status(), PluginStatus::Running) {
            return None;
        }
        instance.begin_frame_wait(surface_id, seen.clone());
        instance.finish_frame_wait(surface_id, timeout)
    }

    /// 是否有帧被绘制期同步取走、投影尚未补齐。壳在心跳里据此补一次投影
    /// （图片载入与键位声明读的是投影缓存，同步取走的帧不走通知）。
    pub fn take_observed_pending(&self) -> bool {
        let instances: Vec<Arc<Instance>> =
            self.instances.lock().unwrap().values().cloned().collect();
        let mut pending = false;
        for instance in instances {
            pending |= instance.take_observed_pending();
        }
        pending
    }

    /// 向某插件的运行日志追加一条宿主侧诊断（如帧里引用了未注册的图片源）。
    /// 返回是否真正写入：实例不在（未启用 / 已卸载 / 装载失败）时无处落盘，调用方据此
    /// 决定是否保留重试，而不是把「写不进去」当成「已报告」。
    pub fn log_to_plugin(&self, plugin_id: &str, text: &str) -> bool {
        let instance = self.instances.lock().unwrap().get(plugin_id).cloned();
        match instance {
            Some(instance) => {
                instance.log_host_note(text);
                true
            }
            None => false,
        }
    }

    /// 回发一条整面帧的原始输入（`host:frame.input`）。插件未在跑时静默丢弃。
    pub fn notify_frame_input(
        &self,
        plugin_id: &str,
        surface_id: &str,
        input: &atelyx_plugin_protocol::frame::FrameInput,
    ) {
        let instance = self.instances.lock().unwrap().get(plugin_id).cloned();
        if let Some(instance) = instance {
            instance.notify_frame_input(surface_id, input);
        }
    }

    /// 是否有画布或整面帧在等帧（壳据此决定要不要挂帧节拍：没有动画时不产生周期唤醒）。
    pub fn has_frame_ticks(&self) -> bool {
        // 只读判定，锁内直接遍历（避免每次调用都克隆实例表）
        let instances = self.instances.lock().unwrap();
        instances.values().any(|instance| instance.has_frame_ticks())
    }

    /// 推一次帧节拍：给到期的画布与整面帧各发一条帧通知（壳的帧 tick 调）。
    /// 先在锁内取一份实例快照再下发，表锁不跟着每个插件的通知走。
    pub fn pump_frame_ticks(&self, now: std::time::Instant) {
        let instances: Vec<Arc<Instance>> = self.instances.lock().unwrap().values().cloned().collect();
        for instance in instances {
            instance.pump_frames(now);
        }
    }

    /// 告知插件某个窗口因失焦被收起（`host:window.hidden`）；插件可用 `window.show` 重新显示。
    pub fn notify_window_hidden(&self, plugin_id: &str, window_id: &str) {
        let instance = self.instances.lock().unwrap().get(plugin_id).cloned();
        if let Some(instance) = instance {
            instance.notify_window_hidden(window_id);
        }
    }

    /// 告知插件某个面已挂载（贡献标签出现在布局里）。插件未在跑时静默丢弃。
    pub fn notify_surface_attached(&self, plugin_id: &str, surface_id: &str, kind: SurfaceKind) {
        let instance = self.instances.lock().unwrap().get(plugin_id).cloned();
        if let Some(instance) = instance {
            instance.notify_surface_attached(surface_id, kind);
        }
    }

    /// 告知插件某个面已移除（贡献标签不再出现在布局里；插件重启时宿主会重新 attached）。
    pub fn notify_surface_detached(&self, plugin_id: &str, surface_id: &str) {
        let instance = self.instances.lock().unwrap().get(plugin_id).cloned();
        if let Some(instance) = instance {
            instance.notify_surface_detached(surface_id);
        }
    }

    /// 全部已注册贡献（跨插件汇总，按插件 id + 贡献 id 排序；未运行插件无贡献）。
    pub fn contributions(&self) -> Vec<Contribution> {
        let instances: Vec<Arc<Instance>> =
            self.instances.lock().unwrap().values().cloned().collect();
        let mut all: Vec<Contribution> = instances.iter().flat_map(|i| i.contributions()).collect();
        all.sort_by(|a, b| (&a.plugin_id, &a.id).cmp(&(&b.plugin_id, &b.id)));
        all
    }

    /// 某插件的私有数据目录（`<插件目录>/data`）；未安装 = `None`。壳据此读插件自带资源（界面图片）。
    pub fn plugin_data_dir(&self, plugin_id: &str) -> Option<PathBuf> {
        self.discovered
            .lock()
            .unwrap()
            .contains_key(plugin_id)
            .then(|| layout::data_dir(&self.root, plugin_id))
    }

    /// 列表（按 id 排序）：已安装插件 + 启用态 + 运行状态 + 审计计数。
    pub fn list(&self) -> Vec<PluginSummary> {
        let mut summaries: Vec<PluginSummary> = {
            let discovered = self.discovered.lock().unwrap();
            let instances = self.instances.lock().unwrap();
            let mut ids: Vec<&String> = discovered.keys().collect();
            ids.sort();
            ids.into_iter()
                .map(|id| {
                    let entry = &discovered[id];
                    let instance = instances.get(id);
                    let manifest = entry.manifest.as_ref().ok();
                    PluginSummary {
                        id: id.clone(),
                        name: manifest.map(|m| m.name.clone()).unwrap_or_default(),
                        version: manifest.map(|m| m.version.clone()).unwrap_or_default(),
                        api_version: manifest.map(|m| m.api_version).unwrap_or(0),
                        permissions: manifest.map(|m| m.permissions.clone()).unwrap_or_default(),
                        singleton: manifest.is_some_and(|m| m.singleton),
                        keep_mounted_on_vault_switch: manifest
                            .is_some_and(|m| m.keep_mounted_on_vault_switch),
                        enabled: entry.enabled,
                        status: instance.map(|i| i.status()).unwrap_or(PluginStatus::Stopped),
                        load_error: entry.manifest.as_ref().err().cloned(),
                        undeclared: instance.map(|i| i.undeclared_count()).unwrap_or(0),
                        submits: instance.map(|i| i.submits()).unwrap_or(0),
                        source: None,
                        preset: false,
                    }
                })
                .collect()
        };
        // 安装来源与预置标记是磁盘事实（不进 discovered 缓存）：出锁后再读，不持锁做 I/O
        for summary in &mut summaries {
            summary.source = layout::read_source(&self.root, &summary.id);
            summary.preset = layout::is_preset(&self.root, &summary.id);
        }
        summaries
    }

    /// 启用 / 停用。启用时不可装载或启动失败即回滚启用态并返回原因。
    pub fn set_enabled(self: &Arc<Self>, id: &str, on: bool) -> Result<(), String> {
        if !on {
            layout::set_enabled(&self.root, id, false)?;
            let instance = self.instances.lock().unwrap().get(id).cloned();
            if let Some(instance) = instance {
                instance.stop();
            }
            self.mark_enabled(id, false);
            return Ok(());
        }
        let (manifest, dir) = self.resolve(id)?;
        layout::set_enabled(&self.root, id, true)?;
        let instance = self.instance_for(&manifest, &dir);
        if let Err(e) = instance.start() {
            let _ = layout::set_enabled(&self.root, id, false);
            return Err(e);
        }
        self.mark_enabled(id, true);
        Ok(())
    }

    /// 熔断后手动重试：清零崩溃计数、恢复启用标记后重启（重试即用户意图启用）。
    pub fn retry(self: &Arc<Self>, id: &str) -> Result<(), String> {
        let (manifest, dir) = self.resolve(id)?;
        let instance = self.instance_for(&manifest, &dir);
        instance.reset_crash_history();
        layout::set_enabled(&self.root, id, true)?;
        self.mark_enabled(id, true);
        instance.start()
    }

    /// 从本地目录安装：只落位内容，不置启用标记；记下来源目录供「从来源更新」。
    /// 曾启用的插件更新后由宿主重启；重启失败不影响安装结果（见 [`InstallOutcome`]）。
    pub fn install_dir(self: &Arc<Self>, src: &Path) -> Result<InstallOutcome, String> {
        let id = install::install_from_dir(&self.root, src)?;
        self.after_install(&id)
    }

    /// 从 zip 安装；`expected_id` 为来源方声明的插件 id（市场条目），与安装包清单不一致即拒收。
    pub fn install_zip(
        self: &Arc<Self>,
        zip_path: &Path,
        expected_id: Option<&str>,
    ) -> Result<InstallOutcome, String> {
        let id = install::install_from_zip(&self.root, zip_path, expected_id)?;
        self.after_install(&id)
    }

    /// 同步随包预置插件：`bundled` = 发行包内插件目录，`marker` = 记录装过哪些预置项的标记文件。
    /// 首装即启用，已装在随包版本更新或内容变化时替换；只落位内容，不启动进程。
    pub fn sync_presets(&self, bundled: &Path, marker: &Path) -> Result<PresetSyncReport, String> {
        preset::sync(&self.root, bundled, marker)
    }

    /// 用上次的安装来源目录重新安装（开发回路：构建产物更新后一键重装并重启）。
    /// 来源缺失 / 目录已不存在 / 来源清单 id 与已安装插件不一致都以可读原因拒绝。
    pub fn install_from_source(self: &Arc<Self>, id: &str) -> Result<InstallOutcome, String> {
        let source = layout::read_source(&self.root, id).ok_or_else(|| {
            format!("{id} 没有安装来源记录：市场或 zip 安装的插件不支持从来源更新")
        })?;
        let src = PathBuf::from(&source);
        if !src.is_dir() {
            return Err(format!("安装来源已不存在：{source}"));
        }
        let manifest = manifest::read_manifest(&src)?;
        if manifest.id != id {
            return Err(format!(
                "来源目录的插件 id（{}）与已安装插件不一致：{id}",
                manifest.id
            ));
        }
        self.install_dir(&src)
    }

    /// 卸载：先停插件，再删安装内容、私有数据与上一代。
    pub fn uninstall(self: &Arc<Self>, id: &str) -> Result<(), String> {
        let instance = self.instances.lock().unwrap().remove(id);
        if let Some(instance) = instance {
            instance.stop();
        }
        install::uninstall(&self.root, id)?;
        self.scan();
        Ok(())
    }

    /// 某插件的被拒调用记录（详情页展示）。
    pub fn audit(&self, id: &str) -> Vec<AuditRecord> {
        self.instances
            .lock()
            .unwrap()
            .get(id)
            .map(|i| i.audit())
            .unwrap_or_default()
    }

    /// 某插件日志的尾部（详情页展示；没有日志 = 空串，读取失败返回原因）。
    pub fn log_tail(&self, id: &str) -> Result<String, String> {
        log::tail(&layout::log_file(&self.root, id))
    }

    /// 清空某插件的运行日志（当前文件与轮转出的上一代）。
    /// 实例在跑时经它的日志写端清：先关写端再删，避免删掉打开中的文件后它继续写进不可见句柄。
    pub fn clear_log(&self, id: &str) -> Result<(), String> {
        let instance = self.instances.lock().unwrap().get(id).cloned();
        match instance {
            Some(instance) => instance.clear_log(),
            None => log::clear_file(&layout::log_file(&self.root, id)),
        }
    }

    /// 某插件日志文件的路径（设置页展示位置；未安装 = `None`）。
    pub fn log_path(&self, id: &str) -> Option<String> {
        self.discovered
            .lock()
            .unwrap()
            .contains_key(id)
            .then(|| layout::log_file(&self.root, id).to_string_lossy().into_owned())
    }

    /// 应用退出：停全部插件并结束全部托管进程（含插件再起的子进程）。
    pub fn shutdown(&self) {
        let instances: Vec<Arc<Instance>> =
            self.instances.lock().unwrap().values().cloned().collect();
        for instance in instances {
            instance.stop();
        }
        self.fail_all_dialogs("宿主已退出");
        self.fail_all_ui_calls("宿主已退出");
        self.fail_all_shell_calls("宿主已退出");
        self.fail_all_tool_calls("宿主已退出");
        self.fail_all_service_calls("宿主已退出");
        self.process_host.shutdown_children();
    }

    /// 退避重启回调（由实例的崩溃线程调用）。
    pub(crate) fn restart_if_current(self: &Arc<Self>, id: &str, generation: u64) {
        let instance = self.instances.lock().unwrap().get(id).cloned();
        if let Some(instance) = instance {
            let _ = instance.start_if_current(generation);
        }
    }

    /// 切仓库容器（本地仓库 ↔ 协作空间 / 换仓库）后的插件侧处置：
    /// 声明 `keepMountedOnVaultSwitch` 的插件保持挂载，订阅了 `vault:switch` 的收到新容器身份；
    /// 未声明的插件由宿主重载（停进程再起）——它拿的旧仓库上下文已失效，新进程订阅时由缓存补发。
    ///
    /// 容器未变（同一仓库被再次选中）不动作；首次激活（启动自动进入）不重载——那一刻插件刚由
    /// 宿主启动、本就没有旧容器上下文，但运行中的订阅者仍要收到容器身份（它们往往早于切换就
    /// 已订阅，错过了订阅时的补发）。
    pub fn on_vault_switch(self: &Arc<Self>, payload: VaultSwitchPayload) {
        // 后到的订阅者（含切容器后被重载的新进程）靠这份缓存补发容器身份
        *self.last_vault_switch.lock().unwrap() = Some(payload.clone());
        let identity = container_identity(&payload);
        let first_activation = {
            let mut container = self.container.lock().unwrap();
            if container.as_deref() == Some(identity.as_str()) {
                return;
            }
            let first = container.is_none();
            *container = Some(identity);
            first
        };
        let targets: Vec<Arc<Instance>> = self.instances.lock().unwrap().values().cloned().collect();
        for instance in targets {
            // 只有跑着的实例才持有旧容器上下文：未运行的重载无意义、也无从送达事件
            if instance.status() != PluginStatus::Running {
                continue;
            }
            // 未声明保活的实例换容器时重载：新进程订阅时由缓存补发当前容器身份
            if !instance.keep_mounted_on_vault_switch() && !first_activation {
                instance.stop();
                if let Err(e) = instance.start() {
                    eprintln!("[plugin {}] 切仓库重载失败：{e}", instance.id);
                }
                continue;
            }
            if instance.subscribed_to(event::name::VAULT_SWITCH) {
                instance.notify_domain_event(event::name::VAULT_SWITCH, &payload);
            }
        }
    }

    /// 广播当前打开表格的变更（打开 / 切换 / 关闭表格、本地编辑结算、协作远端补丁落地）；
    /// `path` = 无打开表格（payload 缺省路径）。路径同步缓存，供迟到的订阅者补发当前状态
    /// （见 [`PluginRuntime::current_table_path`]）。
    pub fn on_table_changed(self: &Arc<Self>, path: Option<&str>) {
        *self.current_table.lock().unwrap() = path.map(str::to_string);
        let payload = TableChangedPayload { path: path.map(str::to_string) };
        self.broadcast_event(event::name::TABLE_CHANGED, &payload);
    }

    /// 广播仓库内文件变更给订阅了 `vault:file.changed` 的运行中插件。只覆盖宿主发起的写入
    /// （保存 / 新建 / 删除 / 改名 / 插件经 `vault` 写），外部程序直接改磁盘不发。
    pub fn on_file_changed(self: &Arc<Self>, path: &str, kind: FileChangeKind, old_path: Option<&str>) {
        let payload = FileChangedPayload {
            path: path.to_string(),
            kind,
            old_path: old_path.map(str::to_string),
        };
        self.broadcast_event(event::name::VAULT_FILE_CHANGED, &payload);
    }

    /// 广播协作空间团队元数据的键变更（宿主写入与成员远端推送同口径）。订阅方按 `key`
    /// 重取该键；元数据只在协作空间存在，本地仓库无此事件。
    pub fn on_space_meta_changed(self: &Arc<Self>, key: &str) {
        let payload = SpaceMetaChangedPayload { key: key.to_string() };
        self.broadcast_event(event::name::SPACE_META_CHANGED, &payload);
    }

    /// 最近一次切容器的 payload（迟到订阅 `vault:switch` 时补发）。
    pub(crate) fn current_vault_switch(&self) -> Option<VaultSwitchPayload> {
        self.last_vault_switch.lock().unwrap().clone()
    }

    /// 某插件声明的默认布局（未声明 = `None`；崩溃重启后重开进程会重新声明）。
    pub fn declared_default_layout(&self, plugin_id: &str) -> Option<(String, LayoutSpecNode)> {
        self.instances
            .lock()
            .unwrap()
            .get(plugin_id)
            .and_then(|instance| instance.default_layout())
    }

    /// 运行中插件已存档的全部默认布局声明：广播不保留历史，壳的事件订阅建立晚于插件
    /// 启动时靠这份快照补发，避免声明落在订阅之前而永久丢失。
    pub fn declared_default_layouts(&self) -> Vec<PluginEvent> {
        self.instances
            .lock()
            .unwrap()
            .values()
            .filter(|instance| instance.status() == PluginStatus::Running)
            .filter_map(|instance| {
                instance.default_layout().map(|(name, spec)| PluginEvent::DefaultLayoutDeclared {
                    id: instance.id.clone(),
                    name,
                    spec,
                })
            })
            .collect()
    }

    /// 广播会话变更给订阅了 `ai:chat.changed` 的运行中插件。不缓存补发：订阅者需要当前
    /// 状态时用 `ai.chat.history` 拉取。
    pub fn on_chat_changed(self: &Arc<Self>, session_id: &str, kind: ChatChangeKind) {
        let payload = ChatChangedPayload {
            session_id: session_id.to_string(),
            kind,
        };
        self.broadcast_event(event::name::AI_CHAT_CHANGED, &payload);
    }

    /// 领域事件广播：只投给运行中且订阅了该名字的实例（停用 / 崩溃 / 退避中的实例不收）。
    fn broadcast_event(self: &Arc<Self>, name: &str, payload: &impl Serialize) {
        let targets: Vec<Arc<Instance>> = self.instances.lock().unwrap().values().cloned().collect();
        for instance in targets {
            if instance.status() != PluginStatus::Running {
                continue;
            }
            if instance.subscribed_to(name) {
                instance.notify_domain_event(name, payload);
            }
        }
    }

    /// 最近一次 `table:changed` 广播的表格路径（无打开表格 = `None`）。
    pub(crate) fn current_table_path(&self) -> Option<String> {
        self.current_table.lock().unwrap().clone()
    }

    /// 转投一条全局快捷键触发给登记它的插件。
    pub(crate) fn notify_shortcut(&self, plugin_id: &str, accelerator: &str) {
        let instance = self.instances.lock().unwrap().get(plugin_id).cloned();
        if let Some(instance) = instance {
            instance.notify_shortcut(accelerator);
        }
    }

    /// 转投一次托盘条目点击给拥有该条目的插件。
    pub fn notify_tray_click(&self, plugin_id: &str, item_id: &str) {
        let instance = self.instances.lock().unwrap().get(plugin_id).cloned();
        if let Some(instance) = instance {
            instance.notify_tray_click(item_id);
        }
    }

    /// 转投一条同空间对等插件的消息给目标插件。
    pub fn notify_collab_message(&self, plugin_id: &str, params: &CollabMessageParams) {
        let instance = self.instances.lock().unwrap().get(plugin_id).cloned();
        if let Some(instance) = instance {
            instance.notify_collab_message(params);
        }
    }

    /// 安装 / 更新后的收尾：重扫清单。更新的若是曾启用的插件，先停旧进程再用新内容重启
    /// （否则旧进程继续跑、新内容不生效）；**新装的插件不自动启用**，由用户在设置页启用。
    /// 重启失败不改判安装结果——内容已落位，失败只是这次没跑起来。
    fn after_install(self: &Arc<Self>, id: &str) -> Result<InstallOutcome, String> {
        let was_enabled = layout::is_enabled(&self.root, id);
        self.scan();
        // 内容换代后旧实例的清单与目录内容都已过期：丢掉它，重启时按新清单重建
        // （崩溃计数一并归零，重装后的首次启动不会直接落进熔断）。
        let previous = self.instances.lock().unwrap().remove(id);
        if let Some(instance) = previous {
            instance.stop();
        }
        let start_error = if was_enabled { self.start_by_id(id).err() } else { None };
        Ok(InstallOutcome { id: id.to_string(), start_error })
    }

    fn start_by_id(self: &Arc<Self>, id: &str) -> Result<(), String> {
        let (manifest, dir) = self.resolve(id)?;
        let instance = self.instance_for(&manifest, &dir);
        instance.start()
    }

    /// 解析某已安装插件的 manifest 与目录（未安装 / 不可装载都以可读原因拒绝）。
    fn resolve(&self, id: &str) -> Result<(Manifest, PathBuf), String> {
        let discovered = self
            .discovered
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| format!("插件未安装：{id}"))?;
        let manifest = discovered
            .manifest
            .clone()
            .map_err(|e| format!("不可启用：{e}"))?;
        Ok((manifest, discovered.dir))
    }

    fn mark_enabled(&self, id: &str, enabled: bool) {
        if let Some(entry) = self.discovered.lock().unwrap().get_mut(id) {
            entry.enabled = enabled;
        }
    }

    fn instance_for(self: &Arc<Self>, manifest: &Manifest, dir: &Path) -> Arc<Instance> {
        let mut instances = self.instances.lock().unwrap();
        if let Some(existing) = instances.get(&manifest.id) {
            return existing.clone();
        }
        let instance = Instance::new(
            manifest.clone(),
            dir.to_path_buf(),
            layout::data_dir(&self.root, &manifest.id),
            self.host_version.clone(),
            self.process_host.clone(),
            self.shortcuts.clone(),
            self.events.clone(),
        );
        instance.attach_runtime(self);
        instances.insert(manifest.id.clone(), instance.clone());
        instance
    }
}

/// 容器身份的规范化串（本地仓库与协作空间分别加前缀，两者取值域互不重叠）。
fn container_identity(payload: &VaultSwitchPayload) -> String {
    match payload {
        VaultSwitchPayload::Local { root } => format!("local:{root}"),
        VaultSwitchPayload::Space { space_id, .. } => format!("space:{space_id}"),
    }
}

/// 壳侧服务请求的发起插件 id（登记归属用）。
fn request_plugin_id(request: &HostRequest) -> &str {
    match request {
        HostRequest::Dialog { plugin_id, .. }
        | HostRequest::Takeover { plugin_id, .. }
        | HostRequest::OpenWindow { plugin_id, .. }
        | HostRequest::CloseWindow { plugin_id, .. }
        | HostRequest::ChatSend { plugin_id, .. }
        | HostRequest::ChatStop { plugin_id, .. }
        | HostRequest::ChatHistory { plugin_id, .. }
        | HostRequest::ClipboardWrite { plugin_id, .. }
        | HostRequest::ClipboardRead { plugin_id, .. }
        | HostRequest::ClipboardReadImage { plugin_id, .. }
        | HostRequest::VaultReadText { plugin_id, .. }
        | HostRequest::VaultWriteText { plugin_id, .. }
        | HostRequest::VaultReadBinary { plugin_id, .. }
        | HostRequest::VaultWriteBinary { plugin_id, .. }
        | HostRequest::VaultImportFile { plugin_id, .. }
        | HostRequest::VaultList { plugin_id, .. }
        | HostRequest::VaultExists { plugin_id, .. }
        | HostRequest::TableSnapshot { plugin_id, .. }
        | HostRequest::TableUpdateCell { plugin_id, .. }
        | HostRequest::TableAddRow { plugin_id, .. }
        | HostRequest::TableRemoveRow { plugin_id, .. }
        | HostRequest::TableSelectRow { plugin_id, .. }
        | HostRequest::TableSetCellImage { plugin_id, .. }
        | HostRequest::MeasureBatch { plugin_id, .. }
        | HostRequest::QueryTheme { plugin_id, .. }
        | HostRequest::WindowUpdate { plugin_id, .. }
        | HostRequest::WindowShow { plugin_id, .. }
        | HostRequest::WindowBeginDrag { plugin_id, .. }
        | HostRequest::ChatRename { plugin_id, .. }
        | HostRequest::ChatDelete { plugin_id, .. }
        | HostRequest::ChatCompact { plugin_id, .. }
        | HostRequest::AiModels { plugin_id, .. }
        | HostRequest::AiAgents { plugin_id, .. }
        | HostRequest::ClipboardWriteImage { plugin_id, .. }
        | HostRequest::VaultRemove { plugin_id, .. }
        | HostRequest::VaultRename { plugin_id, .. }
        | HostRequest::VaultImageInfo { plugin_id, .. }
        | HostRequest::TraySetItems { plugin_id, .. }
        | HostRequest::CollabPeers { plugin_id, .. }
        | HostRequest::CollabSend { plugin_id, .. }
        | HostRequest::CollabMetaGet { plugin_id, .. }
        | HostRequest::CollabMetaPatch { plugin_id, .. }
        | HostRequest::EditorSelection { plugin_id, .. }
        | HostRequest::EditorReplaceSelection { plugin_id, .. }
        | HostRequest::HistoryDay { plugin_id, .. }
        | HostRequest::HistoryOpen { plugin_id, .. }
        | HostRequest::HistoryActivity { plugin_id, .. } => plugin_id,
    }
}

impl Drop for PluginRuntime {
    fn drop(&mut self) {
        // 运行时销毁即应用退出：停全部插件并结束整棵托管进程树（Unix 无作业对象兜底，必须显式收尾）。
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::test_support::TempDir;
    use atelyx_plugin_protocol::manifest::MANIFEST_FILE;

    /// 造一个可扫描到的插件目录（合法 manifest + 平台入口占位文件）。
    fn seed_plugin(root: &Path, id: &str) {
        let dir = layout::plugin_dir(root, id);
        std::fs::create_dir_all(&dir).unwrap();
        let entry = if cfg!(windows) { "app.exe" } else { "app" };
        std::fs::write(
            dir.join(MANIFEST_FILE),
            format!(
                r#"{{"id":"{id}","name":"Demo","version":"1.0.0","apiVersion":1,"entry":{{"windows":"app.exe","linux":"app"}},"permissions":[]}}"#
            ),
        )
        .unwrap();
        std::fs::write(dir.join(entry), b"stub").unwrap();
    }

    #[test]
    fn scan_and_list_reflect_enabled_marks() {
        let tmp = TempDir::new("runtime-scan");
        let root: &Path = &tmp;
        seed_plugin(root, "com.example.one");
        seed_plugin(root, "com.example.two");
        let runtime = PluginRuntime::new(root.to_path_buf());
        runtime.scan();
        let listed = runtime.list();
        assert_eq!(listed.len(), 2);
        assert!(listed.iter().all(|p| !p.enabled && p.load_error.is_none()));
        // 置启用标记后重扫：状态随之变化
        let _ = layout::set_enabled(root, "com.example.one", true);
        runtime.scan();
        let one = runtime
            .list()
            .into_iter()
            .find(|p| p.id == "com.example.one")
            .unwrap();
        assert!(one.enabled);
        assert!(!runtime.list().iter().find(|p| p.id == "com.example.two").unwrap().enabled);
    }

    #[test]
    fn invalid_manifest_surfaces_load_error() {
        let tmp = TempDir::new("runtime-bad");
        let root: &Path = &tmp;
        std::fs::create_dir_all(layout::plugin_dir(root, "com.example.bad")).unwrap();
        std::fs::write(layout::plugin_dir(root, "com.example.bad").join(MANIFEST_FILE), "{}").unwrap();
        let runtime = PluginRuntime::new(root.to_path_buf());
        runtime.scan();
        let listed = runtime.list();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].load_error.is_some(), "非法 manifest 应以可读原因呈现");
    }

    #[test]
    fn disable_removes_marker_and_rejects_uninstalled_enable() {
        let tmp = TempDir::new("runtime-disable");
        let root: &Path = &tmp;
        seed_plugin(root, "com.example.one");
        let runtime = PluginRuntime::new(root.to_path_buf());
        let _ = layout::set_enabled(root, "com.example.one", true);
        runtime.scan();
        runtime.set_enabled("com.example.one", false).unwrap();
        assert!(!layout::is_enabled(root, "com.example.one"));
        // 未安装的 id 不可启用（可读错误）
        assert!(runtime.set_enabled("com.example.none", true).is_err());
        // 未安装的 id 卸载幂等成功
        assert!(runtime.uninstall("com.example.none").is_ok());
    }

    fn dialog(kind: DialogKind) -> DialogRequest {
        DialogRequest { kind, title: None, save: false, default_path: None, filters: Vec::new() }
    }

    #[test]
    fn dialog_request_is_resolved_by_the_shell() {
        let tmp = TempDir::new("runtime-dialog");
        let root: &Path = &tmp;
        let runtime = PluginRuntime::new(root.to_path_buf());
        let rx = runtime.open_dialog("com.a.one", dialog(DialogKind::File)).unwrap();
        let request_id = *runtime.dialogs.lock().unwrap().keys().next().unwrap();
        runtime.resolve_dialog(request_id, Some("/models/m.gguf".into()));
        assert_eq!(rx.recv().unwrap().unwrap().as_deref(), Some("/models/m.gguf"));
        // 登记随结果落地而摘除；重复回填是空操作
        assert!(runtime.dialogs.lock().unwrap().is_empty());
        runtime.resolve_dialog(request_id, None);
    }

    #[test]
    fn pending_dialogs_fail_with_the_plugin_and_are_per_plugin() {
        let tmp = TempDir::new("runtime-dialog-fail");
        let root: &Path = &tmp;
        let runtime = PluginRuntime::new(root.to_path_buf());
        let mine = runtime.open_dialog("com.a.one", dialog(DialogKind::Directory)).unwrap();
        let other = runtime.open_dialog("com.a.two", dialog(DialogKind::Directory)).unwrap();

        runtime.fail_dialogs_of("com.a.one", "插件已停止，对话框已取消");
        assert_eq!(mine.recv().unwrap().unwrap_err(), "插件已停止，对话框已取消");
        assert_eq!(runtime.dialogs.lock().unwrap().len(), 1, "别的插件的在途请求不受影响");

        runtime.shutdown();
        assert_eq!(other.recv().unwrap().unwrap_err(), "宿主已退出");
        assert!(runtime.dialogs.lock().unwrap().is_empty());
    }

    #[test]
    fn shell_calls_resolve_and_fail_per_plugin() {
        let tmp = TempDir::new("runtime-shell-call");
        let runtime = PluginRuntime::new(tmp.to_path_buf());
        let rx = runtime
            .open_shell_call(HostRequest::ClipboardRead {
                plugin_id: "com.a.one".into(),
                request_id: 0,
            })
            .unwrap();
        let request_id = *runtime.shell_calls.lock().unwrap().keys().next().unwrap();
        runtime.resolve_shell_call(request_id, Ok(serde_json::json!({ "text": "hi" })));
        assert_eq!(rx.recv().unwrap().unwrap()["text"], "hi");
        assert!(runtime.shell_calls.lock().unwrap().is_empty());

        // 按插件失败只影响该插件；宿主退出收掉全部在途调用
        let mine = runtime
            .open_shell_call(HostRequest::VaultReadText {
                plugin_id: "com.a.one".into(),
                request_id: 0,
                path: "a.md".into(),
            })
            .unwrap();
        let other = runtime
            .open_shell_call(HostRequest::VaultReadText {
                plugin_id: "com.a.two".into(),
                request_id: 0,
                path: "a.md".into(),
            })
            .unwrap();
        runtime.fail_shell_calls_of("com.a.one", "插件已停止");
        assert_eq!(mine.recv().unwrap().unwrap_err(), "插件已停止");
        assert_eq!(runtime.shell_calls.lock().unwrap().len(), 1, "别的插件的在途调用不受影响");
        runtime.shutdown();
        assert_eq!(other.recv().unwrap().unwrap_err(), "宿主已退出");
        assert!(runtime.shell_calls.lock().unwrap().is_empty());
    }

    #[test]
    fn ui_call_roundtrip_and_per_plugin_failure() {
        let tmp = TempDir::new("runtime-ui-call");
        let runtime = PluginRuntime::new(tmp.to_path_buf());
        // 接管要求贡献已注册：未注册即拒绝（无实例时的可读失败路径）
        assert!(runtime.open_takeover("com.a.one", TakeoverScope::Full, "main").is_err());

        let rx = runtime
            .open_plugin_window(
                "com.a.one",
                ui_open_window_params("w1"),
            )
            .expect("开窗请求应登记成功");
        let request_id = *runtime.ui_calls.lock().unwrap().keys().next().unwrap();
        runtime.resolve_ui_call(request_id, Ok(()));
        assert_eq!(rx.recv().unwrap(), Ok(()));
        assert!(runtime.ui_calls.lock().unwrap().is_empty());

        // 停用收尾：该插件的在途请求以可读失败收场，别的插件不受影响
        let mine = runtime.open_plugin_window(
            "com.a.one",
            WindowOpenParams {
                id: "w".into(),
                title: "窗口".into(),
                width: None,
                height: None,
                always_on_top: None,
                skip_taskbar: None,
                hide_on_blur: None,
                decorations: None,
            },
        ).unwrap();
        let other = runtime
            .open_plugin_window("com.a.two", ui_open_window_params("w2"))
            .unwrap();
        runtime.fail_ui_calls_of("com.a.one", "插件已停止");
        assert_eq!(mine.recv().unwrap(), Err("插件已停止".to_string()));
        assert_eq!(runtime.ui_calls.lock().unwrap().len(), 1);

        runtime.shutdown();
        assert_eq!(other.recv().unwrap(), Err("宿主已退出".to_string()));
    }

    fn ui_open_window_params(id: &str) -> WindowOpenParams {
        WindowOpenParams {
            id: id.into(),
            title: "窗口".into(),
            width: None,
            height: None,
            always_on_top: None,
            skip_taskbar: None,
            hide_on_blur: None,
            decorations: None,
        }
    }

    #[test]
    fn plugin_window_params_are_validated() {
        let tmp = TempDir::new("runtime-window-validate");
        let runtime = PluginRuntime::new(tmp.to_path_buf());
        let mut blank = ui_open_window_params("w");
        blank.id = "  ".into();
        assert!(runtime.open_plugin_window("com.a", blank).is_err(), "空 id 应被拒");
        let mut colon = ui_open_window_params("a:b");
        assert!(runtime.open_plugin_window("com.a", colon.clone()).is_err(), "含 `:` 的 id 应被拒");
        colon.id = "ok".into();
        colon.title = " ".into();
        assert!(runtime.open_plugin_window("com.a", colon).is_err(), "空标题应被拒");
        assert!(runtime.open_plugin_window("com.a", ui_open_window_params("ok")).is_ok());

        // window.update：空 id 与「一个属性都不改」都在分派处拒，不投递请求
        let update = |id: &str| WindowUpdateParams {
            id: id.into(),
            width: None,
            height: None,
            always_on_top: None,
        };
        assert!(runtime.update_plugin_window("com.a", update("  ")).is_err(), "空 id 应被拒");
        assert!(
            runtime.update_plugin_window("com.a", update("w")).is_err(),
            "未给任何要改的属性应被拒"
        );
        let mut sized = update("w");
        sized.height = Some(400.0);
        assert!(runtime.update_plugin_window("com.a", sized).is_ok());
        assert!(runtime.show_plugin_window("com.a", "w").is_ok());
        assert!(runtime.show_plugin_window("com.a", "  ").is_err(), "空窗口 id 应被拒");
    }

    #[test]
    fn doc_types_are_collected_from_manifests() {
        let tmp = TempDir::new("runtime-doctypes");
        let root: &Path = &tmp;
        let dir = layout::plugin_dir(root, "com.example.board");
        std::fs::create_dir_all(&dir).unwrap();
        let entry = if cfg!(windows) { "app.exe" } else { "app" };
        std::fs::write(
            dir.join(MANIFEST_FILE),
            r#"{"id":"com.example.board","name":"Board","version":"1.0.0","apiVersion":1,
                "entry":{"windows":"app.exe","linux":"app"},"permissions":[],
                "docTypeRegistrations":[{"extension":"board","label":"画板","icon":"layout-dashboard","surface":"board-view"}]}"#,
        )
        .unwrap();
        std::fs::write(dir.join(entry), b"stub").unwrap();
        seed_plugin(root, "com.example.plain");
        let runtime = PluginRuntime::new(root.to_path_buf());
        runtime.scan();
        let doc_types = runtime.doc_types();
        assert_eq!(doc_types.len(), 1, "未注册文档类型的插件不产生条目");
        assert_eq!(doc_types[0].plugin_id, "com.example.board");
        assert_eq!(doc_types[0].registration.extension, "board");
        assert_eq!(doc_types[0].registration.surface, "board-view");
        assert_eq!(doc_types[0].registration.label, "画板");
    }

    fn tool_params(
        name: &str,
        description: &str,
    ) -> atelyx_plugin_protocol::method::RegisterToolParams {
        atelyx_plugin_protocol::method::RegisterToolParams {
            name: name.into(),
            description: description.into(),
            parameters: serde_json::json!({ "type": "object", "properties": {} }),
        }
    }

    #[test]
    fn plugin_tools_register_uniquely_and_validate() {
        let tmp = TempDir::new("runtime-ai-tools");
        let runtime = PluginRuntime::new(tmp.to_path_buf());
        let mut events = runtime.subscribe_events();
        runtime
            .register_tool("com.a.one", "One", tool_params("find_notes", "按关键词找笔记"))
            .unwrap();
        assert!(matches!(
            events.try_recv(),
            Ok(PluginEvent::AiToolsChanged { id }) if id == "com.a.one"
        ));
        let tools = runtime.plugin_tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].selection_id(), "plugin:com.a.one:find_notes");

        // 同名跨插件被拒；同插件重复注册原地更新
        assert!(runtime
            .register_tool("com.a.two", "Two", tool_params("find_notes", "别的"))
            .is_err());
        runtime
            .register_tool("com.a.one", "One", tool_params("find_notes", "新描述"))
            .unwrap();
        assert_eq!(runtime.plugin_tools().len(), 1);
        assert_eq!(runtime.plugin_tools()[0].description, "新描述");

        // 非法名 / 空描述 / 非对象 schema / 内置工具重名
        assert!(runtime
            .register_tool("com.a.one", "One", tool_params("bad name", "x"))
            .is_err());
        assert!(runtime
            .register_tool("com.a.one", "One", tool_params("ok_name", "  "))
            .is_err());
        let mut scalar = tool_params("ok_name", "x");
        scalar.parameters = serde_json::json!("object");
        assert!(runtime.register_tool("com.a.one", "One", scalar).is_err());
        assert!(runtime
            .register_tool("com.a.one", "One", tool_params("read_file", "抢占内置名"))
            .is_err());
        assert_eq!(runtime.plugin_tools().len(), 1, "被拒的注册不得留下条目");
    }

    #[test]
    fn plugin_tool_unregister_and_clear_are_owner_scoped() {
        let tmp = TempDir::new("runtime-ai-tools-owner");
        let runtime = PluginRuntime::new(tmp.to_path_buf());
        runtime
            .register_tool("com.a.one", "One", tool_params("tool_one", "一"))
            .unwrap();
        runtime
            .register_tool("com.a.two", "Two", tool_params("tool_two", "二"))
            .unwrap();
        assert!(
            runtime.unregister_tool("com.a.one", "tool_two").is_err(),
            "不得摘除别的插件的工具"
        );
        assert!(runtime.unregister_tool("com.a.two", "tool_two").is_ok());
        assert!(runtime.unregister_tool("com.a.two", "tool_two").is_err(), "重复摘除报可读错误");
        assert_eq!(runtime.plugin_tools().len(), 1);
        runtime.clear_tools_of("com.a.one");
        assert!(runtime.plugin_tools().is_empty());
    }

    /// 从来源更新 = 按记录下来的本地目录重装：内容替换为新构建，来源与 id 都要对得上。
    #[test]
    fn install_from_recorded_source_replaces_content() {
        let tmp = TempDir::new("runtime-install-source");
        let root = tmp.join("plugins");
        let src = tmp.join("dist");
        std::fs::create_dir_all(&src).unwrap();
        let entry = if cfg!(windows) { "app.exe" } else { "app" };
        std::fs::write(
            src.join(MANIFEST_FILE),
            r#"{"id":"com.example.demo","name":"Demo","version":"1.0.0","apiVersion":1,
                "entry":{"windows":"app.exe","linux":"app"},"permissions":[]}"#,
        )
        .unwrap();
        std::fs::write(src.join(entry), b"v1").unwrap();

        let runtime = PluginRuntime::new(root.clone());
        let outcome = runtime.install_dir(&src).expect("目录安装应成功");
        assert_eq!(outcome.id, "com.example.demo");
        assert!(outcome.start_error.is_none(), "新装插件不启动，也就没有启动失败");
        let source = runtime.list()[0].source.clone().expect("应记录安装来源");
        assert_eq!(std::path::PathBuf::from(source), dunce::canonicalize(&src).unwrap());

        // 构建产物更新后从来源重装
        std::fs::write(src.join(entry), b"v2").unwrap();
        runtime.install_from_source("com.example.demo").expect("从来源更新应成功");
        assert_eq!(
            std::fs::read(layout::plugin_dir(&root, "com.example.demo").join(entry)).unwrap(),
            b"v2"
        );

        // 来源清单换成另一个 id：拒收（否则会用别的插件覆盖这一份）
        std::fs::write(
            src.join(MANIFEST_FILE),
            r#"{"id":"com.example.other","name":"Other","version":"1.0.0","apiVersion":1,
                "entry":{"windows":"app.exe","linux":"app"},"permissions":[]}"#,
        )
        .unwrap();
        let err = runtime
            .install_from_source("com.example.demo")
            .expect_err("来源 id 不一致应被拒");
        assert!(err.contains("不一致"), "{err}");

        // 来源目录消失与无来源记录（zip / 市场安装）都按可读原因拒绝
        std::fs::remove_dir_all(&src).unwrap();
        let err = runtime
            .install_from_source("com.example.demo")
            .expect_err("来源目录不存在应被拒");
        assert!(err.contains("安装来源"), "{err}");
        assert!(runtime.install_from_source("com.example.absent").is_err());
    }
}
