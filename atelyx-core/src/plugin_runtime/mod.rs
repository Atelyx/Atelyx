//! 插件宿主运行时：发现 / 装载 / RPC 泵 / 权限仲裁 / 生命周期 / 安装卸载。
//! 运行时为进程内单例（壳持 `Arc`，多窗口共享）；插件事件经 [`PluginRuntime::subscribe_events`]
//! 的单一广播通道进入壳的 update。进程创建与清理一律经 [`crate::plugin_process`]。

mod capability;
mod child_process;
mod fs;
mod http;
mod install;
mod instance;
mod layout;
mod manifest;
/// 市场数据源（公开给壳：索引拉取 / 缓存 / Release 取数）。
pub mod market;
mod shortcut;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use atelyx_plugin_protocol::event;
use atelyx_plugin_protocol::manifest::Manifest;
use atelyx_plugin_protocol::method;
use serde_json::Value;

use crate::plugin_process::PluginProcessHost;

use instance::Instance;

/// 插件侧搜索查询的等待上限（到点未回结果即该次回落内置提供方）。
const SEARCH_QUERY_TIMEOUT: Duration = Duration::from_secs(15);

/// 内置搜索提供方配置（壳侧「联网搜索」设置的投影；插件上岗时不使用）。
#[derive(Clone, Debug, Default)]
pub struct BuiltinSearchConfig {
    pub provider: String,
    pub searxng_url: Option<String>,
    pub tavily_key: Option<String>,
}

/// 协议类型再导出：壳只依赖 `atelyx-core`，不直接依赖协议 crate。
pub use atelyx_plugin_protocol::canvas::{CanvasColor, CanvasPoint, DrawCommand};
pub use atelyx_plugin_protocol::event::VaultSwitchPayload;
pub use atelyx_plugin_protocol::manifest::{DocTypeRegistration, Permission};
pub use atelyx_plugin_protocol::method::{
    ChatEvent, ChatEventParams, ChatHistoryParams, ChatHistoryResult, ChatSendParams,
    ChatSessionSummary, ChatSessionWire, ChatStopParams, ContributionSlot, FileFilter,
    NotificationLevel, SearchResult, ServiceContract, SurfaceKind, TakeoverScope, WindowOpenParams,
};
pub use atelyx_plugin_protocol::view::{
    Align, Axis, ButtonVariant, CanvasEventKind, CanvasPointer, ColorToken, EventKind, EventValue,
    Fill, ImageFit, ImageSource, MenuItem, MouseButton, NodeBody, TextWeight, ViewEvent, ViewNode,
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

/// 一条被拒的能力调用（审计记录）。
#[derive(Clone, Debug)]
pub struct AuditRecord {
    pub method: String,
    /// 调用携带的路径（无则空串）。
    pub detail: String,
    /// 记录时刻（unix 秒）。
    pub at: u64,
}

/// 运行时 → 壳的广播事件（状态 / 通知 / 拒调用 / 界面与贡献变化；允许落后丢帧）。
#[derive(Clone, Debug)]
pub enum PluginEvent {
    StatusChanged { id: String, status: PluginStatus },
    Notification { id: String, title: String, body: Option<String>, level: NotificationLevel },
    PermissionDenied { id: String, method: String, detail: String },
    /// 某插件提交了新的界面树（树本身经 [`PluginRuntime::surface_tree`] 取；丢帧不影响最终渲染）。
    ViewSubmitted { id: String },
    /// 某插件的贡献注册表变化（注册 / 撤销 / 随会话收尾清空）。
    ContributionsChanged { id: String },
    /// 某插件的服务实现声明变化（注册 / 撤销 / 随会话收尾清空）。
    ServicesChanged { id: String },
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
    /// [`PluginRuntime::resolve_chat_call`] 回填，事件随后经 `host:ai.chat.event` 流达插件）。
    ChatSend { plugin_id: String, request_id: u64, params: method::ChatSendParams },
    /// 插件请求中断一个会话的活跃 run。
    ChatStop { plugin_id: String, request_id: u64, params: method::ChatStopParams },
    /// 插件请求读会话清单或单会话消息历史。
    ChatHistory { plugin_id: String, request_id: u64, params: method::ChatHistoryParams },
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
    /// 被拒的能力调用条数。
    pub denied: usize,
    /// 已接收的界面树提交次数。
    pub view_submits: u64,
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

/// 在途搜索查询等待项：上岗插件 id + 结果回传通道。
type SearchWait = (String, std::sync::mpsc::Sender<Result<Vec<SearchResult>, String>>);

/// AI 对话调用结果回传通道（send / stop / history 的应答载荷）。
type ChatReplyTx = std::sync::mpsc::Sender<Result<Value, String>>;

/// 在途 AI 对话调用等待项：发起插件 id + 结果回传通道。
type ChatCallWait = (String, ChatReplyTx);

/// 插件宿主运行时。
pub struct PluginRuntime {
    root: PathBuf,
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
    /// 在途搜索查询：request_id → 等待项（上岗插件回结果或超时回落）。
    search_pending: Mutex<HashMap<u64, SearchWait>>,
    next_search_id: AtomicU64,
    /// 在途 AI 对话调用（send / stop / history）：request_id → 等待项。
    chat_calls: Mutex<HashMap<u64, ChatCallWait>>,
    next_chat_call_id: AtomicU64,
    /// 服务实现声明簿：契约 → 已声明插件 id 列表（声明是候选不是上岗）。
    services: Mutex<HashMap<ServiceContract, Vec<String>>>,
    /// 用户指定的搜索提供方上岗插件 id（`None` = 内置 Tavily / SearXNG）。
    search_provider: Mutex<Option<String>>,
    /// 当前容器身份（见 [`PluginRuntime::on_vault_switch`]）：同一容器重复进入不动作，据此判别首次激活。
    container: Mutex<Option<String>>,
    discovered: Mutex<HashMap<String, Discovered>>,
    instances: Mutex<HashMap<String, Arc<Instance>>>,
}

impl PluginRuntime {
    /// 建立运行时（`root` = 应用数据目录下 `plugins/`）。顺带清理遗留的安装临时目录。
    pub fn new(root: PathBuf) -> Arc<Self> {
        layout::sweep_temp(&root);
        let (events, _) = tokio::sync::broadcast::channel(256);
        let (host_requests, host_requests_rx) = tokio::sync::mpsc::unbounded_channel();
        let runtime = Arc::new(Self {
            root,
            process_host: Arc::new(PluginProcessHost::new()),
            shortcuts: shortcut::Registrar::new(),
            events,
            host_requests,
            host_requests_rx: tokio::sync::Mutex::new(host_requests_rx),
            dialogs: Mutex::new(HashMap::new()),
            next_dialog_id: AtomicU64::new(1),
            ui_calls: Mutex::new(HashMap::new()),
            next_ui_call_id: AtomicU64::new(1),
            search_pending: Mutex::new(HashMap::new()),
            next_search_id: AtomicU64::new(1),
            chat_calls: Mutex::new(HashMap::new()),
            next_chat_call_id: AtomicU64::new(1),
            services: Mutex::new(HashMap::new()),
            search_provider: Mutex::new(None),
            container: Mutex::new(None),
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
            HostRequest::Dialog { .. } => return Err("界面服务请求形态不符".to_string()),
            HostRequest::ChatSend { .. }
            | HostRequest::ChatStop { .. }
            | HostRequest::ChatHistory { .. } => return Err("界面服务请求形态不符".to_string()),
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

    /// 登记一个 AI 对话调用（send / stop / history）并把请求交给壳，返回结果接收端。
    fn open_chat_call(&self, request: HostRequest) -> Result<std::sync::mpsc::Receiver<Result<Value, String>>, String> {
        let request_id = self.next_chat_call_id.fetch_add(1, Ordering::SeqCst);
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
            _ => return Err("AI 对话请求形态不符".to_string()),
        };
        self.chat_calls
            .lock()
            .unwrap()
            .insert(request_id, (request_plugin_id(&request).to_string(), tx));
        if self.host_requests.send(request).is_err() {
            self.chat_calls.lock().unwrap().remove(&request_id);
            return Err("AI 对话服务不可用：宿主已退出".to_string());
        }
        Ok(rx)
    }

    /// 壳侧回填一次 AI 对话调用的结果（send 的会话 / 消息 id、history 的清单或会话；失败带可读原因）。
    pub fn resolve_chat_call(&self, request_id: u64, result: Result<Value, String>) {
        let waiter = self.chat_calls.lock().unwrap().remove(&request_id);
        if let Some((_, tx)) = waiter {
            let _ = tx.send(result);
        }
    }

    /// 让某插件全部在途 AI 对话调用以可读失败收场（插件停用 / 失败 / 崩溃时调用）。
    pub(crate) fn fail_chat_calls_of(&self, plugin_id: &str, reason: &str) {
        let ids: Vec<u64> = {
            let chat_calls = self.chat_calls.lock().unwrap();
            chat_calls
                .iter()
                .filter(|(_, (owner, _))| owner == plugin_id)
                .map(|(id, _)| *id)
                .collect()
        };
        for id in ids {
            let waiter = self.chat_calls.lock().unwrap().remove(&id);
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

    /// 让全部在途 AI 对话调用以可读失败收场（宿主退出收尾）。
    fn fail_all_chat_calls(&self, reason: &str) {
        let waiters: Vec<ChatReplyTx> = {
            let mut chat_calls = self.chat_calls.lock().unwrap();
            chat_calls.drain().map(|(_, (_, tx))| tx).collect()
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

    // ---------- 服务接管 ----------

    /// 插件声明实现某服务契约（幂等；候选名单变化才广播给壳）。
    pub(crate) fn declare_service(&self, plugin_id: &str, contract: ServiceContract) {
        let changed = {
            let mut services = self.services.lock().unwrap();
            let list = services.entry(contract).or_default();
            let changed = !list.iter().any(|id| id == plugin_id);
            if changed {
                list.push(plugin_id.to_string());
            }
            changed
        };
        if changed {
            let _ = self.events.send(PluginEvent::ServicesChanged { id: plugin_id.to_string() });
        }
    }

    /// 插件撤销服务实现声明；未注册的契约报可读错误（测试与运行时收尾共用）。
    pub fn retract_service(&self, plugin_id: &str, contract: ServiceContract) -> Result<(), String> {
        let mut services = self.services.lock().unwrap();
        let Some(list) = services.get_mut(&contract) else {
            return Err(format!("未声明该服务契约：{contract:?}"));
        };
        let before = list.len();
        list.retain(|id| id != plugin_id);
        let removed = list.len() != before;
        if list.is_empty() {
            services.remove(&contract);
        }
        drop(services);
        if !removed {
            return Err(format!("未声明该服务契约：{contract:?}"));
        }
        let _ = self.events.send(PluginEvent::ServicesChanged { id: plugin_id.to_string() });
        Ok(())
    }

    /// 收掉某插件的全部服务声明（随会话收尾；停用 / 崩溃 / 卸载即自动摘除候选资格）。
    pub(crate) fn retract_services_of(&self, plugin_id: &str) {
        let dropped = {
            let mut services = self.services.lock().unwrap();
            let mut dropped = false;
            services.retain(|_, list| {
                let before = list.len();
                list.retain(|id| id != plugin_id);
                dropped = dropped || list.len() != before;
                !list.is_empty()
            });
            dropped
        };
        if dropped {
            let _ = self.events.send(PluginEvent::ServicesChanged { id: plugin_id.to_string() });
        }
    }

    /// 已声明某契约的全部插件 id（设置页候选名单）。
    pub fn service_candidates(&self, contract: ServiceContract) -> Vec<String> {
        self.services
            .lock()
            .unwrap()
            .get(&contract)
            .cloned()
            .unwrap_or_default()
    }

    /// 用户指定的搜索提供方上岗插件 id（`None` = 内置）。
    pub fn search_provider(&self) -> Option<String> {
        self.search_provider.lock().unwrap().clone()
    }

    /// 设置搜索提供方上岗者（壳从持久化配置应用；`None` = 回到内置）。
    pub fn set_search_provider(&self, plugin_id: Option<String>) {
        *self.search_provider.lock().unwrap() = plugin_id;
    }

    /// 当前搜索上岗者是否有效（已声明契约且进程在跑）。失效即调用方直接走内置，不发起插件查询。
    fn assigned_search_plugin(&self) -> Option<String> {
        let assigned = self.search_provider.lock().unwrap().clone()?;
        let declared = self
            .services
            .lock()
            .unwrap()
            .get(&ServiceContract::WebSearch)
            .map(|list| list.iter().any(|id| id == &assigned))
            .unwrap_or(false);
        if !declared {
            return None;
        }
        let running = self
            .instances
            .lock()
            .unwrap()
            .get(&assigned)
            .map(|i| i.status() == PluginStatus::Running)
            .unwrap_or(false);
        running.then_some(assigned)
    }

    /// 执行一次网页搜索：上岗插件有效则向其下发查询（超时 / 失败即该次回落内置并留可见提示），
    /// 否则直接走内置 Tavily / SearXNG。
    pub async fn web_search(
        &self,
        query: &str,
        builtin: &BuiltinSearchConfig,
    ) -> Result<Vec<SearchResult>, String> {
        let assigned = self.assigned_search_plugin();
        if let Some(id) = assigned {
            match self.query_plugin_search(&id, query).await {
                Ok(results) => return Ok(results),
                Err(reason) => {
                    let _ = self.events.send(PluginEvent::Notification {
                        id,
                        title: "网页搜索已回落内置提供方".to_string(),
                        body: Some(reason),
                        level: NotificationLevel::Warning,
                    });
                }
            }
        }
        crate::search::search_web(
            builtin.provider.clone(),
            query.to_string(),
            builtin.searxng_url.clone(),
            builtin.tavily_key.clone(),
        )
        .await
        .map(|items| {
            items
                .into_iter()
                .map(|i| SearchResult { title: i.title, url: i.url, snippet: i.snippet })
                .collect()
        })
    }

    /// 向上岗插件下发一次查询并等结果（超时 / 插件停止 / 插件报错都以可读失败收场）。
    /// 登记与进程在跑检查之间仍有微小竞态窗口（teardown 执行中），由收尾的
    /// [`PluginRuntime::fail_searches_of`] 与下方超时兜底——最坏一次调用等到超时后回落内置。
    async fn query_plugin_search(&self, plugin_id: &str, query: &str) -> Result<Vec<SearchResult>, String> {
        let instance = {
            let instances = self.instances.lock().unwrap();
            instances
                .get(plugin_id)
                .filter(|i| i.status() == PluginStatus::Running)
                .cloned()
                .ok_or_else(|| "插件未在运行".to_string())?
        };
        let request_id = self.next_search_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = std::sync::mpsc::channel();
        self.search_pending
            .lock()
            .unwrap()
            .insert(request_id, (plugin_id.to_string(), tx));
        // 登记后复查：插件在登记与下发之间被收尾时立即失败收场，不挂满超时
        if instance.status() != PluginStatus::Running {
            self.search_pending.lock().unwrap().remove(&request_id);
            return Err("插件未在运行".to_string());
        }
        instance.notify_search_query(request_id, query);
        let wait = tokio::task::spawn_blocking(move || {
            rx.recv_timeout(SEARCH_QUERY_TIMEOUT)
                .map_err(|_| format!("搜索查询超时（{}s）", SEARCH_QUERY_TIMEOUT.as_secs()))?
        });
        let joined = wait.await;
        // 无论等待成功、超时还是任务被取消，在途登记一律摘除（不随 `?` 提前返回泄漏）
        self.search_pending.lock().unwrap().remove(&request_id);
        joined.map_err(|e| format!("搜索等待任务失败：{e}"))?
    }

    /// 插件回一次查询结果；未知请求 id（已超时回落 / 已取消）静默丢弃。
    pub(crate) fn resolve_search(&self, request_id: u64, result: Result<Vec<SearchResult>, String>) {
        let waiter = self.search_pending.lock().unwrap().remove(&request_id);
        if let Some((_, tx)) = waiter {
            let _ = tx.send(result);
        }
    }

    /// 让等待某插件结果的在途搜索查询立即失败收场（插件停用 / 崩溃时调用；调用方按单次回落内置处理）。
    pub(crate) fn fail_searches_of(&self, plugin_id: &str, reason: &str) {
        let ids: Vec<u64> = {
            let pending = self.search_pending.lock().unwrap();
            pending
                .iter()
                .filter(|(_, (owner, _))| owner == plugin_id)
                .map(|(id, _)| *id)
                .collect()
        };
        for id in ids {
            let waiter = self.search_pending.lock().unwrap().remove(&id);
            if let Some((_, tx)) = waiter {
                let _ = tx.send(Err(reason.to_string()));
            }
        }
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
            let manifest = manifest::read_manifest(&dir).and_then(|m| {
                if m.id == name {
                    Ok(m)
                } else {
                    Err(format!("目录名与 manifest id 不一致：目录 {name}，声明 {}", m.id))
                }
            });
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

    /// 取某插件某个面的最新界面树（从未提交 / 已随会话收尾清除时为 `None`）。
    pub fn surface_tree(&self, plugin_id: &str, surface_id: &str) -> Option<ViewNode> {
        self.instances
            .lock()
            .unwrap()
            .get(plugin_id)
            .and_then(|i| i.surface_tree(surface_id))
    }

    /// 取某插件某面某画布节点的最新绘图指令清单（整发整换；未提交 / 已清除时为 `None`）。
    pub fn surface_canvas(
        &self,
        plugin_id: &str,
        surface_id: &str,
        node_id: &str,
    ) -> Option<std::sync::Arc<Vec<DrawCommand>>> {
        self.instances
            .lock()
            .unwrap()
            .get(plugin_id)
            .and_then(|i| i.surface_canvas(surface_id, node_id))
    }

    /// 回发一条界面事件给插件。插件未在跑时静默丢弃（对端已不在）。
    pub fn send_view_event(&self, plugin_id: &str, surface_id: &str, node_id: &str, event: ViewEvent) {
        let instance = self.instances.lock().unwrap().get(plugin_id).cloned();
        if let Some(instance) = instance {
            instance.notify_view_event(surface_id, node_id, event);
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
                    keep_mounted_on_vault_switch: manifest.is_some_and(|m| m.keep_mounted_on_vault_switch),
                    enabled: entry.enabled,
                    status: instance.map(|i| i.status()).unwrap_or(PluginStatus::Stopped),
                    load_error: entry.manifest.as_ref().err().cloned(),
                    denied: instance.map(|i| i.denied_count()).unwrap_or(0),
                    view_submits: instance.map(|i| i.view_submits()).unwrap_or(0),
                }
            })
            .collect()
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

    /// 从本地目录安装（安装即启用）。返回安装的插件 id。
    pub fn install_dir(self: &Arc<Self>, src: &Path) -> Result<String, String> {
        let id = install::install_from_dir(&self.root, src)?;
        self.after_install(&id)
    }

    /// 从 zip 安装（安装即启用）。返回安装的插件 id。
    pub fn install_zip(self: &Arc<Self>, zip_path: &Path) -> Result<String, String> {
        let id = install::install_from_zip(&self.root, zip_path)?;
        self.after_install(&id)
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

    /// 应用退出：停全部插件并结束全部托管进程（含插件再起的子进程）。
    pub fn shutdown(&self) {
        let instances: Vec<Arc<Instance>> =
            self.instances.lock().unwrap().values().cloned().collect();
        for instance in instances {
            instance.stop();
        }
        self.fail_all_dialogs("宿主已退出");
        self.fail_all_ui_calls("宿主已退出");
        self.fail_all_chat_calls("宿主已退出");
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
    /// 未声明的插件由宿主重载（停进程再起）——它拿的旧仓库上下文已失效。
    ///
    /// 两种情形不动作：容器未变（同一仓库被再次选中）；首次激活（启动自动进入）——那一刻插件刚由
    /// 宿主启动、本就没有旧容器上下文，重载纯属白起一次，而广播仍是保活插件得知容器身份的唯一来源。
    pub fn on_vault_switch(self: &Arc<Self>, payload: VaultSwitchPayload) {
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
            if !instance.keep_mounted_on_vault_switch() {
                if first_activation {
                    continue;
                }
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

    /// 转投一条全局快捷键触发给登记它的插件。
    pub(crate) fn notify_shortcut(&self, plugin_id: &str, accelerator: &str) {
        let instance = self.instances.lock().unwrap().get(plugin_id).cloned();
        if let Some(instance) = instance {
            instance.notify_shortcut(accelerator);
        }
    }

    /// 安装 / 更新后的收尾：重扫清单。更新的若是曾启用的插件，先停旧进程再用新内容重启
    /// （否则旧进程继续跑、新内容不生效）；**新装的插件不自动启用**，由用户在设置页启用。
    fn after_install(self: &Arc<Self>, id: &str) -> Result<String, String> {
        let was_enabled = layout::is_enabled(&self.root, id);
        self.scan();
        if let Some(instance) = self.instances.lock().unwrap().get(id).cloned() {
            instance.stop();
        }
        if was_enabled {
            self.start_by_id(id)?;
        }
        Ok(id.to_string())
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
        VaultSwitchPayload::Space { space_id } => format!("space:{space_id}"),
    }
}

/// 界面服务请求的发起插件 id（登记归属用）。
fn request_plugin_id(request: &HostRequest) -> &str {
    match request {
        HostRequest::Dialog { plugin_id, .. }
        | HostRequest::Takeover { plugin_id, .. }
        | HostRequest::OpenWindow { plugin_id, .. }
        | HostRequest::CloseWindow { plugin_id, .. }
        | HostRequest::ChatSend { plugin_id, .. }
        | HostRequest::ChatStop { plugin_id, .. }
        | HostRequest::ChatHistory { plugin_id, .. } => plugin_id,
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
    }

    #[test]
    fn service_declarations_roundtrip_and_stop_retracts() {
        let tmp = TempDir::new("runtime-service");
        let runtime = PluginRuntime::new(tmp.to_path_buf());
        runtime.declare_service("com.a", ServiceContract::WebSearch);
        runtime.declare_service("com.a", ServiceContract::WebSearch);
        assert_eq!(
            runtime.service_candidates(ServiceContract::WebSearch),
            vec!["com.a".to_string()],
            "重复声明应幂等"
        );
        runtime.retract_service("com.a", ServiceContract::WebSearch).unwrap();
        assert!(runtime.service_candidates(ServiceContract::WebSearch).is_empty());
        assert!(runtime.retract_service("com.a", ServiceContract::WebSearch).is_err());
    }

    #[tokio::test]
    async fn search_falls_back_to_builtin_when_provider_not_running() {
        let tmp = TempDir::new("runtime-search");
        let runtime = PluginRuntime::new(tmp.to_path_buf());
        // 指定了上岗者但插件未运行 / 未声明：不走插件路径，直接回落内置
        runtime.set_search_provider(Some("com.example.search".into()));
        let builtin = BuiltinSearchConfig {
            provider: "unknown-provider".into(),
            ..BuiltinSearchConfig::default()
        };
        let err = runtime.web_search("q", &builtin).await.unwrap_err();
        assert!(err.contains("未知搜索源"), "未上岗时应走内置分发路径：{err}");
    }

    #[test]
    fn search_results_are_resolved_and_failed_per_plugin() {
        let tmp = TempDir::new("runtime-search-pending");
        let runtime = PluginRuntime::new(tmp.to_path_buf());
        let (tx, rx) = std::sync::mpsc::channel();
        runtime
            .search_pending
            .lock()
            .unwrap()
            .insert(7, ("com.a".to_string(), tx));
        runtime.resolve_search(
            7,
            Ok(vec![SearchResult {
                title: "t".into(),
                url: "https://a".into(),
                snippet: "s".into(),
            }]),
        );
        assert_eq!(rx.recv().unwrap().unwrap().len(), 1);
        assert!(runtime.search_pending.lock().unwrap().is_empty());

        // 未知请求 id 静默丢弃；按插件失败只影响该插件
        runtime.resolve_search(99, Ok(Vec::new()));
        let (tx, rx) = std::sync::mpsc::channel();
        runtime
            .search_pending
            .lock()
            .unwrap()
            .insert(8, ("com.a".to_string(), tx));
        runtime.fail_searches_of("com.b", "插件已停止");
        assert_eq!(runtime.search_pending.lock().unwrap().len(), 1);
        runtime.fail_searches_of("com.a", "插件已停止");
        assert_eq!(rx.recv().unwrap(), Err("插件已停止".to_string()));
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
}
