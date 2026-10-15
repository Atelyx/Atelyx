//! 能力面方法：插件→宿主的无前缀方法与宿主→插件的 `host:` 方法，方法名常量 + 定型 params/result。
//! 全部方法按调用方插件自动绑定插件 id（协议不暴露 id 参数，插件无法指定他人命名空间）。

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------- 方法名常量 ----------

pub mod view {
    /// 插件提交一棵界面树到某个面（面板 / 窗口 / 接管区 / 文档实例）。宿主校验后按键 diff 挂载。
    pub const SUBMIT: &str = "view.submit";
    /// 插件注册一个贡献点（面板 / 表格视图 / 设置页），使其出现在宿主对应的入口里。
    pub const REGISTER: &str = "view.register";
    /// 插件按 id 撤销自己的一个贡献点。
    pub const UNREGISTER: &str = "view.unregister";
    /// 插件请求接管当前聚焦窗口（整页 / 全接管）。宿主按窗应用，退出入口由宿主常驻提供。
    pub const TAKEOVER: &str = "view.takeover";
    /// 宿主回发节点事件（通知）。
    pub const HOST_EVENT: &str = "host:view.event";
    /// 宿主告知插件某个面已就绪 / 已移除（通知）。
    pub const HOST_ATTACHED: &str = "host:surface.attached";
    pub const HOST_DETACHED: &str = "host:surface.detached";
}

pub mod window {
    /// 插件请求打开一个独立窗口承载自己的界面树（声明的 `id` 即该窗口面的 `surfaceId`）。
    pub const OPEN: &str = "window.open";
    /// 插件关闭自己打开的一个窗口。
    pub const CLOSE: &str = "window.close";
    /// 宿主告知插件某个窗口已被用户关闭（通知）。
    pub const HOST_CLOSED: &str = "host:window.closed";
}

pub mod service {
    /// 插件声明自己实现某服务契约，进入该契约的候选上岗名单（上岗与否由用户指定）。
    pub const REGISTER: &str = "service.register";
    /// 插件撤销自己的服务实现声明。
    pub const UNREGISTER: &str = "service.unregister";
}

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

pub mod process {
    pub const SPAWN: &str = "process.spawn";
    pub const WRITE_STDIN: &str = "process.writeStdin";
    pub const CLOSE_STDIN: &str = "process.closeStdin";
    pub const KILL: &str = "process.kill";
    /// 托管进程的输出与退出（通知）。
    pub const HOST_EVENT: &str = "host:process.event";
}

pub mod state {
    pub const READ: &str = "state.read";
    pub const WRITE: &str = "state.write";
}

pub mod notification {
    pub const SHOW: &str = "notification.show";
}

pub mod shortcut {
    pub const REGISTER: &str = "shortcut.register";
    pub const UNREGISTER: &str = "shortcut.unregister";
    /// 全局快捷键触发（通知）。
    pub const HOST_TRIGGERED: &str = "host:shortcut.triggered";
}

pub mod clipboard {
    pub const WRITE_TEXT: &str = "clipboard.writeText";
    pub const READ_TEXT: &str = "clipboard.readText";
}

pub mod dialog {
    pub const PICK_FILE: &str = "dialog.pickFile";
    pub const PICK_DIRECTORY: &str = "dialog.pickDirectory";
}

pub mod http {
    pub const REQUEST: &str = "http.request";
}

pub mod doc {
    /// 用户打开了插件注册的领域文档类型文件（通知）。`surfaceId` 为宿主分配的实例面标识，
    /// 插件据此以 `view.submit` 提交该文档实例的界面树。
    pub const HOST_OPEN: &str = "host:doc.open";
    /// 文档实例已关闭 / 已切换走（通知）。插件此后不得再向该 surfaceId 提交树。
    pub const HOST_CLOSE: &str = "host:doc.close";
}

pub mod search {
    /// 宿主向已上岗的插件搜索提供方发起一次查询（通知）；插件以 [`RESPOND`] 请求回结果。
    pub const HOST_QUERY: &str = "host:search.query";
    /// 插件回一次查询结果（插件→宿主请求）；`error` 非空 = 本次查询失败。
    pub const RESPOND: &str = "search.respond";
}

pub mod canvas {
    /// 插件提交一份绘图指令清单到某个画布节点。宿主合帧应用（渲染每帧只取最新一份）。
    pub const SUBMIT: &str = "canvas.submit";
}

pub mod ai {
    /// 插件发起 / 续聊一轮对话（须声明 `ai` 权限）。回执 = run 已就位，事件随后经
    /// [`HOST_CHAT_EVENT`] 流达。会话 = 仓库普通会话（主面板可见）；供应商 / 模型 / Agent
    /// 一律用仓库 AI 设置的默认值，插件不指定、不带 key。
    pub const CHAT_SEND: &str = "ai.chat.send";
    /// 中断一个会话的活跃 run（幂等：无活跃 run 也成功）。
    pub const CHAT_STOP: &str = "ai.chat.stop";
    /// 读当前激活仓库的会话清单或单会话消息历史。
    pub const CHAT_HISTORY: &str = "ai.chat.history";
    /// 对话事件流（通知）：快照节流拍 + 终态 + 命名结果。
    pub const HOST_CHAT_EVENT: &str = "host:ai.chat.event";
}

// ---------- view / surface ----------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SurfaceAttachedParams {
    pub surface_id: String,
    /// 面的类型：面板 / 插件窗口 / 整页接管 / 全接管 / 设置页。
    pub kind: SurfaceKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SurfaceKind {
    Panel,
    Window,
    Page,
    Takeover,
    Settings,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SurfaceDetachedParams {
    pub surface_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitParams {
    pub surface_id: String,
    pub tree: crate::view::ViewNode,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitResult {
    pub applied: bool,
}

/// 贡献点位置（封闭枚举；扩容随协议升版）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ContributionSlot {
    /// 工作区面板（「添加面板 / 切换视图」菜单条目）。
    Panel,
    /// 表格编辑器工具条「视图」下拉条目。
    TableView,
    /// 设置页顶层 tab（注册即挂载：宿主立即回 `host:surface.attached`，插件常驻设置树）。
    Settings,
}

/// `view.register` 的 params：声明一个贡献点。`id` 插件内唯一且不得含 `:`（宿主的面标识以它作分隔），
/// 同时是该面挂载后的 `surfaceId`；`icon` 缺省时宿主用拼图图标。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterParams {
    pub slot: ContributionSlot,
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

/// `view.unregister` 的 params：按 id 撤销自己的贡献；id 未注册时报可读错误。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnregisterParams {
    pub id: String,
}

/// 接管范围（封闭枚举）。`page` = 内容区全给插件（宿主标题栏保留）；`full` = 整窗归插件呈现
/// （含标题栏区域，窗口拖动 / 缩放 / 关闭由宿主代执行）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TakeoverScope {
    Page,
    Full,
}

/// `view.takeover` 的 params：把本插件的一个贡献面接管到当前聚焦窗口。`surfaceId` 须为
/// 已注册的贡献 id（与该面共享同一棵提交树）；退出接管入口由宿主常驻提供，插件无需声明退路。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TakeoverParams {
    pub scope: TakeoverScope,
    pub surface_id: String,
}

/// `window.open` 的 params：`id` 插件内唯一、非空且不含 `:`，同时是该窗口面的 `surfaceId`。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
}

/// `window.close` 的 params。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowCloseParams {
    pub id: String,
}

/// [`window::HOST_CLOSED`] 的 params：用户（而非插件）关闭了该窗口。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowClosedParams {
    pub id: String,
}

/// 服务契约（封闭枚举；扩容随协议升版）。每个契约定义宿主与插件间的调用形态。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ServiceContract {
    /// 网页搜索提供方：宿主经 [`search::HOST_QUERY`] 下发查询，插件经 [`search::RESPOND`] 回结果
    /// （与内置 Tavily / SearXNG 的结果同构）。
    WebSearch,
}

/// `service.register` / `service.unregister` 的 params：声明或撤销自己实现某契约。
/// 注册是声明不是上岗——是否上岗由用户在设置页指定；停用 / 崩溃 / 卸载随会话收尾自动摘除声明。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceRegisterParams {
    pub contract: ServiceContract,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewEventParams {
    pub surface_id: String,
    pub node_id: String,
    pub event: crate::view::ViewEvent,
}

/// `canvas.submit` 的 params：清单整发整换（无增量协议），空清单 = 清空画布。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CanvasSubmitParams {
    pub surface_id: String,
    /// 目标画布节点 id（须在当前 `view.submit` 树里且为 canvas 节点）。
    pub node_id: String,
    pub commands: Vec<crate::canvas::DrawCommand>,
}

// ---------- fs ----------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivateDirResult {
    pub path: String,
}

/// `path` 一律绝对路径。信任模型与进程托管一致：插件本就是任意权限的用户进程，
/// 声明式权限是透明度机制而非沙箱边界；`fs` 服务的价值在统一出口与审计事实。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathParams {
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextContent {
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BinaryContent {
    pub base64: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteTextParams {
    pub path: String,
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteBinaryParams {
    pub path: String,
    pub base64: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExistsResult {
    pub exists: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListResult {
    pub entries: Vec<FsEntry>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsEntry {
    pub name: String,
    pub kind: FsEntryKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FsEntryKind {
    File,
    Dir,
}

// ---------- process ----------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpawnResult {
    pub pid: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PidParams {
    pub pid: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteStdinParams {
    pub pid: u32,
    pub data: String,
}

/// 托管进程事件负载（[`process::HOST_EVENT`]）。形态与宿主进程托管的上报一一对应。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ProcessEvent {
    Stdout { data: String },
    Stderr { data: String },
    Terminated { code: Option<i32> },
    Error { message: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessEventParams {
    pub pid: u32,
    pub event: ProcessEvent,
}

// ---------- state / notification / shortcut ----------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StateReadResult {
    /// 该插件的自持数据；从未写过时为 null。
    pub json: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StateWriteParams {
    pub json: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShowNotificationParams {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default)]
    pub level: NotificationLevel,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NotificationLevel {
    #[default]
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterShortcutParams {
    /// 加速键描述（如 `Shift+Alt+E`）；与既注册项冲突时报可读错误。
    pub accelerator: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcceleratorParams {
    pub accelerator: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShortcutTriggeredParams {
    pub accelerator: String,
}

// ---------- clipboard / dialog / http ----------

/// 剪贴板写 params 与读 result 共用：一段纯文本。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextPayload {
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileFilter {
    pub name: String,
    /// 不带点的扩展名列表（如 `["png", "jpg"]`）。
    pub extensions: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PickDirectoryParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PickedPath {
    /// 用户取消时该字段缺省（serde 对 null 与缺省等价处理）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpResult {
    pub status: u16,
    pub headers: std::collections::BTreeMap<String, String>,
    pub body_base64: String,
}

// ---------- doc ----------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocOpenParams {
    /// 仓库容器内的相对路径（本地仓库）或服务端内容路径（协作空间），与 vault 口径一致。
    pub path: String,
    /// 宿主为本实例分配的面标识：插件据此以 `view.submit` 提交该文档实例的界面树。
    pub surface_id: String,
}

/// [`doc::HOST_CLOSE`] 的 params：实例随标签关闭 / 换文件被关闭。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocCloseParams {
    pub surface_id: String,
    pub path: String,
}

// ---------- search ----------

/// 一条搜索结果（与内置 Tavily / SearXNG 提供方的结果同构）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// [`search::HOST_QUERY`] 的 params（宿主 → 插件通知）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchQueryParams {
    /// 查询标识：插件经 [`search::RESPOND`] 回结果时原样带回。
    pub request_id: u64,
    pub query: String,
}

/// [`search::RESPOND`] 的 params（插件 → 宿主请求）。`error` 非空 = 本次查询失败
/// （宿主按单次回落内置处理），此时 `results` 为空。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchRespondParams {
    pub request_id: u64,
    #[serde(default)]
    pub results: Vec<SearchResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// ---------- ai ----------

/// [`ai::CHAT_SEND`] 的 params：`session_id` 缺省 = 新建会话；非缺省 = 在该会话续聊
/// （会话须存在于当前仓库，否则可读拒绝）。同会话已有活跃 run 时拒绝（每会话单 run）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSendParams {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// [`ai::CHAT_SEND`] 的 result：run 已就位；流式事件随后经 [`ai::HOST_CHAT_EVENT`] 流达。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSendResult {
    pub session_id: String,
    /// 本轮 assistant 消息 id（后续事件载荷同源）。
    pub message_id: String,
}

/// [`ai::CHAT_STOP`] 的 params。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatStopParams {
    pub session_id: String,
}

/// [`ai::CHAT_HISTORY`] 的 params：`session_id` 缺省 = 列会话清单；非缺省 = 读该会话消息。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatHistoryParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// 消息条数上限（读单会话时生效；缺省全量，超出取末尾）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// 一条会话摘要（清单模式）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSessionSummary {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub updated_at: i64,
    pub message_count: u64,
}

/// 单会话读取模式的 result：元数据 + 消息数组（消息形态与会话磁盘格式同源：
/// `id` / `role` / `content` / `displayContent` / `steps` / 压缩注解）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSessionWire {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub messages: Vec<Value>,
}

/// [`ai::CHAT_HISTORY`] 的 result：清单模式只填 `sessions`；单会话模式只填 `session`。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatHistoryResult {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<ChatSessionSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<ChatSessionWire>,
}

/// [`ai::HOST_CHAT_EVENT`] 的 params：载荷按 [`ChatEvent`] 分型。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatEventParams {
    pub session_id: String,
    pub message_id: String,
    pub event: ChatEvent,
}

/// 对话事件（快照级）：整条消息替换渲染，丢拍由后拍自愈；终态只发一次。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_and_surface_kind_serde_uses_camel_case() {
        assert_eq!(serde_json::to_value(ContributionSlot::Panel).unwrap(), "panel");
        assert_eq!(serde_json::to_value(ContributionSlot::TableView).unwrap(), "tableView");
        assert_eq!(serde_json::to_value(ContributionSlot::Settings).unwrap(), "settings");
        assert_eq!(serde_json::to_value(SurfaceKind::Settings).unwrap(), "settings");
        assert_eq!(serde_json::to_value(TakeoverScope::Page).unwrap(), "page");
        assert_eq!(serde_json::to_value(TakeoverScope::Full).unwrap(), "full");
        assert_eq!(serde_json::to_value(ServiceContract::WebSearch).unwrap(), "webSearch");
        let kind: SurfaceKind = serde_json::from_value(serde_json::json!("settings")).unwrap();
        assert_eq!(kind, SurfaceKind::Settings);
        let slot: ContributionSlot = serde_json::from_value(serde_json::json!("settings")).unwrap();
        assert_eq!(slot, ContributionSlot::Settings);
        let scope: TakeoverScope = serde_json::from_value(serde_json::json!("full")).unwrap();
        assert_eq!(scope, TakeoverScope::Full);
        // ai 面：事件 tagged 形态与 send 缺省 sessionId
        assert_eq!(
            serde_json::to_value(ChatEvent::Finished { reason: "stop".into() }).unwrap(),
            serde_json::json!({ "kind": "finished", "reason": "stop" })
        );
        let send: ChatSendParams = serde_json::from_value(serde_json::json!({ "text": "hi" })).unwrap();
        assert_eq!(send.session_id, None);
    }
}
