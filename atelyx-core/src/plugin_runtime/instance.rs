//! 单个插件的 RPC 泵：进程输出逐行解析、插件→宿主请求按权限闸门与能力分派应答。
//! 方向约定：插件→宿主一律请求，宿主→插件一律通知（无请求 / 响应往返，故无需请求配对表）。
//! stdout 只允许协议信封（非 JSON = 协议违规，停插件并给出可读原因）。
//!
//! 请求在进程 stdout 读线程上**顺序**处理（同一进程的读线程唯一，`state` 写后读天然保序）。
//! 每次启动领一个运行令牌（[`Instance::run_token`]）：令牌不匹配的输出（上次启动遗留）一律丢弃，
//! 否则「停用 → 启用」后到达的旧进程事件会被当成新进程的崩溃，抹掉新 pid 并再起一个实例。

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use atelyx_plugin_protocol::envelope::{Envelope, Request, RequestId, RpcError};
use atelyx_plugin_protocol::event;
use atelyx_plugin_protocol::handshake::{self, HostInfo, InitializeParams, InitializeResult};
use atelyx_plugin_protocol::manifest::{Manifest, Permission};
use atelyx_plugin_protocol::method::{
    self, ContributionSlot, RegisterParams, SurfaceAttachedParams, SurfaceDetachedParams,
    UnregisterParams, ViewEventParams,
};
use atelyx_plugin_protocol::{canvas, view, API_VERSION};
use base64::Engine;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};

use crate::plugin_process::{EventSink, PluginProcessHost, ProcessEvent as HostEvent};

use super::{capability, child_process, fs, http, log, shortcut};
use super::{
    AuditRecord, Contribution, DialogKind, DialogRequest, HostRequest, LayoutSpecNode, PaddingSpec,
    PluginEvent, PluginRuntime, PluginStatus, SurfaceKind, ViewEvent, ViewNode,
    MAX_VAULT_BINARY_BYTES,
};

/// 崩溃判定窗口：窗口内累计达到 [`CRASH_LIMIT`] 即熔断。
const CRASH_WINDOW: Duration = Duration::from_secs(60);
/// 窗口内允许的最大崩溃次数。
const CRASH_LIMIT: usize = 5;
/// 退避基数与封顶（第 n 次崩溃延迟 `base * 2^(n-1)`，封顶 [`BACKOFF_MAX`]）。
const BACKOFF_BASE: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// 审计记录保留条数上限（详情页可见最近若干条）。
const MAX_AUDIT: usize = 50;
/// 握手截止：启动后超过此时长仍未收到 `initialize` 即判失败（不留「假运行中」）。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// 宿主自报信息（握手回应）。
const HOST_NAME: &str = "Atelyx";

/// 一个插件的运行实例：进程句柄、在途请求、订阅与审计。
pub(crate) struct Instance {
    pub(crate) id: String,
    manifest: Manifest,
    dir: PathBuf,
    state_dir: PathBuf,
    /// 宿主产品版本（握手自报宿主信息用；由运行时在建立实例时给定）。
    host_version: String,
    process_host: Arc<PluginProcessHost>,
    shortcuts: Arc<shortcut::Registrar>,
    events: tokio::sync::broadcast::Sender<PluginEvent>,
    /// 供崩溃退避线程回调用（弱引用，避免与运行时的强引用成环）。
    runtime: Mutex<Weak<PluginRuntime>>,
    /// 本插件托管的子进程 pid（`process.spawn` 起的是宿主的子进程，不在插件进程树下）。
    children: child_process::Spawned,
    /// 会话闸门：OS 资源获取（子进程 / 全局快捷键）与停用收尾互斥，见 [`Instance::with_session`]。
    resources: Mutex<()>,
    /// 当前插件进程 pid（0 = 未在跑）。
    pid: AtomicU64,
    /// 运行令牌：每次启动 / 停止递增，用于丢弃上次启动遗留的进程输出。
    run_token: AtomicU64,
    /// 会话是否仍活跃：启动成功后置真，停用 / 失败 / 崩溃收尾开始前置假。
    /// 工具调用登记在资源闸门内复查它，收尾扫表之后到达的登记不再落表（见 [`Instance::with_active_session`]）。
    session_active: AtomicBool,
    /// 本次启动是否已完成握手（握手截止检查依据）。
    handshaken: AtomicBool,
    status: Mutex<PluginStatus>,
    /// 启停世代：每次启停递增，用于作废在途的退避重启。
    generation: AtomicU64,
    /// 状态转换互斥：手工启停与退避重启在此串行，杜绝双启动。
    transition: Mutex<()>,
    subscriptions: Mutex<HashSet<String>>,
    denied: Mutex<Vec<AuditRecord>>,
    /// 各面最新界面树（surface id → 树）；会话收尾一并清除（撤贡献）。
    surfaces: Mutex<HashMap<String, ViewNode>>,
    /// 各面画布节点的最新绘图指令清单（整发整换，合帧应用）。
    canvases: CanvasLists,
    /// 已注册贡献点（按注册序）；id 插件内唯一，撤销按 id 摘除。
    contributions: Mutex<Vec<ContributionEntry>>,
    /// 插件经 `view.defineIcon` 定义的自备图标形状（名字 → 24x24 描边路径）；随会话收尾清除。
    icons: Mutex<HashMap<String, String>>,
    view_submits: AtomicU64,
    crash_times: Mutex<Vec<Instant>>,
    /// 本会话声明的默认布局（重启后插件重新声明）。
    default_layout: Mutex<Option<(String, LayoutSpecNode)>>,
    /// 本插件日志（插件 stderr、协议违规与生命周期失败的唯一可查落点）。
    log: Arc<log::LogSink>,
}

/// 画布节点 → 指令清单存档（(面 id, 节点 id) → 清单）。
type CanvasLists = Mutex<HashMap<(String, String), Arc<Vec<canvas::DrawCommand>>>>;

/// 一个已注册贡献点（不含插件 id；插件 id 由汇总侧补上）。
struct ContributionEntry {
    slot: ContributionSlot,
    id: String,
    label: String,
    icon: Option<String>,
    padding: Option<PaddingSpec>,
}

impl Instance {
    pub(crate) fn new(
        manifest: Manifest,
        dir: PathBuf,
        state_dir: PathBuf,
        host_version: String,
        process_host: Arc<PluginProcessHost>,
        shortcuts: Arc<shortcut::Registrar>,
        events: tokio::sync::broadcast::Sender<PluginEvent>,
    ) -> Arc<Self> {
        let log = Arc::new(log::LogSink::new(super::layout::log_file_in(&dir)));
        Arc::new(Self {
            id: manifest.id.clone(),
            manifest,
            dir,
            state_dir,
            host_version,
            process_host,
            shortcuts,
            events,
            runtime: Mutex::new(Weak::new()),
            children: child_process::Spawned::default(),
            resources: Mutex::new(()),
            pid: AtomicU64::new(0),
            run_token: AtomicU64::new(0),
            session_active: AtomicBool::new(false),
            handshaken: AtomicBool::new(false),
            status: Mutex::new(PluginStatus::Stopped),
            generation: AtomicU64::new(0),
            transition: Mutex::new(()),
            subscriptions: Mutex::new(HashSet::new()),
            denied: Mutex::new(Vec::new()),
            surfaces: Mutex::new(HashMap::new()),
            canvases: Mutex::new(HashMap::new()),
            contributions: Mutex::new(Vec::new()),
            icons: Mutex::new(HashMap::new()),
            view_submits: AtomicU64::new(0),
            crash_times: Mutex::new(Vec::new()),
            default_layout: Mutex::new(None),
            log,
        })
    }

    pub(crate) fn attach_runtime(&self, runtime: &Arc<PluginRuntime>) {
        *self.runtime.lock().unwrap() = Arc::downgrade(runtime);
    }

    pub(crate) fn status(&self) -> PluginStatus {
        self.status.lock().unwrap().clone()
    }

    pub(crate) fn audit(&self) -> Vec<AuditRecord> {
        self.denied.lock().unwrap().clone()
    }

    pub(crate) fn denied_count(&self) -> usize {
        self.denied.lock().unwrap().len()
    }

    pub(crate) fn view_submits(&self) -> u64 {
        self.view_submits.load(Ordering::Relaxed)
    }

    /// 某面的最新界面树（会话收尾即清空，插件未提交过 = `None`）。
    pub(crate) fn surface_tree(&self, surface_id: &str) -> Option<ViewNode> {
        self.surfaces.lock().unwrap().get(surface_id).cloned()
    }

    /// 某面某画布节点的最新绘图指令清单（整发整换；会话收尾即清空）。
    pub(crate) fn surface_canvas(
        &self,
        surface_id: &str,
        node_id: &str,
    ) -> Option<Arc<Vec<canvas::DrawCommand>>> {
        self.canvases
            .lock()
            .unwrap()
            .get(&(surface_id.to_string(), node_id.to_string()))
            .cloned()
    }

    /// 本插件已注册贡献点（补上插件 id）。
    pub(crate) fn contributions(&self) -> Vec<Contribution> {
        self.contributions
            .lock()
            .unwrap()
            .iter()
            .map(|entry| Contribution {
                plugin_id: self.id.clone(),
                slot: entry.slot,
                id: entry.id.clone(),
                label: entry.label.clone(),
                icon: entry.icon.clone(),
                padding: entry.padding,
            })
            .collect()
    }

    /// 本插件定义的自备图标形状（名字 → 路径）；会话收尾即清空。
    pub(crate) fn icons(&self) -> HashMap<String, String> {
        self.icons.lock().unwrap().clone()
    }

    /// 本会话声明的默认布局（未声明 = `None`）。
    pub(crate) fn default_layout(&self) -> Option<(String, LayoutSpecNode)> {
        self.default_layout.lock().unwrap().clone()
    }

    /// 手动重试前清零崩溃计数（否则窗口内的旧崩溃会立刻把新会话判为熔断）。
    pub(crate) fn reset_crash_history(&self) {
        self.crash_times.lock().unwrap().clear();
    }

    /// 启动插件进程。已在跑时空操作；启动失败把状态置 `Failed` 并返回原因。
    pub(crate) fn start(self: &Arc<Self>) -> Result<(), String> {
        let _guard = self.transition.lock().unwrap();
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.start_inner()
    }

    /// 退避重启专用：仅当世代与状态仍是崩溃时捕获的值才启动（否则已被手工启停取代）。
    pub(crate) fn start_if_current(self: &Arc<Self>, generation: u64) -> Result<(), String> {
        let _guard = self.transition.lock().unwrap();
        if self.generation.load(Ordering::SeqCst) != generation
            || self.status() != PluginStatus::Restarting
        {
            return Ok(());
        }
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.start_inner()
    }

    fn start_inner(self: &Arc<Self>) -> Result<(), String> {
        if self.status() == PluginStatus::Running {
            return Ok(());
        }
        let Some(entry) = self.manifest.entry.for_current_platform() else {
            let reason = "未声明当前平台的可执行入口".to_string();
            self.set_status(PluginStatus::Failed(reason.clone()));
            return Err(reason);
        };
        let program = self.dir.join(entry).to_string_lossy().into_owned();
        let cwd = self.dir.to_string_lossy().into_owned();
        // 领本次运行的令牌：此后旧进程的输出与事件一律按令牌丢弃
        let token = self.run_token.fetch_add(1, Ordering::SeqCst) + 1;
        self.handshaken.store(false, Ordering::SeqCst);
        let instance = self.clone();
        let sink: EventSink = Arc::new(move |event| instance.handle_process_event(token, event));
        let pid = self
            .process_host
            .spawn(&program, &[], Some(&cwd), None, sink)
            .map_err(|e| format!("启动插件进程失败：{e}"))?;
        self.pid.store(pid as u64, Ordering::SeqCst);
        self.session_active.store(true, Ordering::SeqCst);
        self.set_status(PluginStatus::Running);
        self.log.line(&format!("[启动] 进程已启动（pid {pid}，入口 {entry}）"));
        // 握手截止：到点仍未握手即判失败（进程在跑却永不通信不能被当成正常）
        let watchdog = self.clone();
        std::thread::spawn(move || {
            std::thread::sleep(HANDSHAKE_TIMEOUT);
            watchdog.check_handshake(token);
        });
        Ok(())
    }

    /// 停止插件：作废在途退避重启与本次运行的陈旧输出，并结束整棵进程树。
    pub(crate) fn stop(&self) {
        let _guard = self.transition.lock().unwrap();
        self.generation.fetch_add(1, Ordering::SeqCst);
        // 先作废会话再跑收尾：收尾扫过之后到达的工具调用登记会被闸门拒掉，不能落表等满超时
        self.session_active.store(false, Ordering::SeqCst);
        self.run_token.fetch_add(1, Ordering::SeqCst);
        // 日志写端由收尾关闭，本行须在收尾之前写；本来就没在跑（扫描 / 重装后的兜底停止）不记
        if self.status() != PluginStatus::Stopped {
            self.log.line("[生命周期] 已停止");
        }
        self.teardown();
        self.set_status(PluginStatus::Stopped);
    }

    /// 判定为不可装载 / 握手失败：置 `Failed` 并结束进程（避免僵死连接）。
    fn fail(self: &Arc<Self>, reason: String) {
        self.log.line(&format!("[失败] {reason}"));
        self.session_active.store(false, Ordering::SeqCst);
        self.set_status(PluginStatus::Failed(reason));
        self.run_token.fetch_add(1, Ordering::SeqCst);
        self.teardown();
    }

    /// 结束插件进程树（pid = 0 时跳过）并收掉随插件记账的宿主资源。
    fn teardown(&self) {
        let pid = self.pid.swap(0, Ordering::SeqCst) as u32;
        if pid != 0 {
            if let Err(e) = self.process_host.kill_process_tree(pid) {
                eprintln!("[plugin {}] 结束进程树失败：{e}", self.id);
            }
        }
        // 日志写端不跨会话持有：卸载要删整个插件目录，Windows 上打开着的文件会让删除失败
        self.log.close();
        self.release_resources();
    }

    /// 收掉随插件记账的宿主资源：托管子进程、已注册的全局快捷键、在途的对话框请求、本会话订阅
    /// 与界面贡献（各面树 + 贡献注册）。停用 / 失败 / 崩溃 / 卸载四条路径共用——漏掉任何一条都会
    /// 留下孤儿服务、永久悬挂的调用，或让已停插件的界面继续对外呈现。
    fn release_resources(&self) {
        // 与资源获取互斥：本段跑完之后到达的登记一律被 [`Instance::with_session`] 拒掉
        let _gate = self.resources.lock().unwrap();
        self.subscriptions.lock().unwrap().clear();
        let surfaces_dropped = !self.surfaces.lock().unwrap().is_empty();
        self.surfaces.lock().unwrap().clear();
        let canvases_dropped = !self.canvases.lock().unwrap().is_empty();
        self.canvases.lock().unwrap().clear();
        let contributions_dropped = !self.contributions.lock().unwrap().is_empty();
        self.contributions.lock().unwrap().clear();
        self.icons.lock().unwrap().clear();
        // 默认布局声明属于本次会话：插件重启后重新声明，不让旧规格跨会话生效
        *self.default_layout.lock().unwrap() = None;
        for child in self.children.take_all() {
            if let Err(e) = self.process_host.kill_process_tree(child) {
                eprintln!("[plugin {}] 结束子进程 {child} 失败：{e}", self.id);
            }
        }
        self.shortcuts.release_plugin(&self.id);
        self.fail_pending_dialogs("插件已停止，对话框已取消");
        self.fail_pending_ui_calls("插件已停止，界面服务请求已取消");
        self.fail_pending_shell_calls("插件已停止，宿主调用已取消");
        self.release_ai_tools();
        // 界面与贡献变化广播给壳：壳据此停下已停插件界面的渲染并清掉菜单条目
        if surfaces_dropped || canvases_dropped {
            let _ = self.events.send(PluginEvent::ViewSubmitted { id: self.id.clone() });
        }
        if contributions_dropped {
            let _ = self.events.send(PluginEvent::ContributionsChanged { id: self.id.clone() });
        }
    }

    /// 让本插件在途的对话框请求立即以可读失败收场，避免等待它的读线程无限期悬挂。
    fn fail_pending_dialogs(&self, reason: &str) {
        let runtime = self.runtime.lock().unwrap().clone();
        if let Some(runtime) = runtime.upgrade() {
            runtime.fail_dialogs_of(&self.id, reason);
        }
    }

    /// 让本插件在途的接管 / 插件窗口请求以可读失败收场（等待壳回填的线程不再悬挂）。
    fn fail_pending_ui_calls(&self, reason: &str) {
        let runtime = self.runtime.lock().unwrap().clone();
        if let Some(runtime) = runtime.upgrade() {
            runtime.fail_ui_calls_of(&self.id, reason);
        }
    }

    /// 让本插件在途的壳侧服务调用以可读失败收场（等待壳回填的线程不再悬挂）。
    fn fail_pending_shell_calls(&self, reason: &str) {
        let runtime = self.runtime.lock().unwrap().clone();
        if let Some(runtime) = runtime.upgrade() {
            runtime.fail_shell_calls_of(&self.id, reason);
        }
    }

    /// 摘除本插件注册的 AI 工具并让在途工具调用以可读失败收场。
    fn release_ai_tools(&self) {
        let runtime = self.runtime.lock().unwrap().clone();
        if let Some(runtime) = runtime.upgrade() {
            runtime.clear_tools_of(&self.id);
            runtime.fail_tool_calls_of(&self.id, "插件已停止，工具调用已取消");
        }
    }

    /// 下发一条对话事件（`host:ai.chat.event`）。
    pub(crate) fn notify_chat_event(&self, params: &method::ChatEventParams) {
        self.notify(method::ai::HOST_CHAT_EVENT, params);
    }

    /// 在会话闸门内执行一次 OS 资源获取（起子进程 / 注册全局快捷键），锁内复查请求所属的会话令牌。
    ///
    /// 这道闸门是「登记与收尾不得交错」的保证：没有它，读线程上一次已在途的请求可能在
    /// [`Instance::release_resources`] 跑完之后才落表——那个子进程 / 热键就逃过了本次停用收尾，
    /// 变成「插件已停、本机服务仍在跑 / 热键仍被占用」。令牌换代即拒，故跨会话的迟到请求也不会
    /// 污染新会话的登记。
    fn with_session<T>(&self, token: u64, acquire: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        let _gate = self.resources.lock().unwrap();
        if token != self.run_token.load(Ordering::SeqCst) {
            return Err("插件会话已结束，本次请求已作废".to_string());
        }
        acquire()
    }

    /// 在资源闸门内复查会话仍活跃后执行一次登记（工具调用待答表落表 + 下发通知）。
    ///
    /// 收尾路径先置 [`Instance::session_active`] 为假再取闸门扫表，故本闭包要么先于扫表落表
    /// （随后被收尾以可读失败收场），要么在扫表之后被拒——不会留下无人认领、只能等满超时的登记。
    pub(crate) fn with_active_session<T>(
        &self,
        register: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        let _gate = self.resources.lock().unwrap();
        if !self.session_active.load(Ordering::SeqCst) || self.status() != PluginStatus::Running {
            return Err(format!("插件未运行：{}", self.id));
        }
        register()
    }

    /// 握手截止检查：令牌已被取代 / 已握手 / 已不在运行态时都不动作。
    fn check_handshake(self: &Arc<Self>, token: u64) {
        if token != self.run_token.load(Ordering::SeqCst)
            || self.handshaken.load(Ordering::SeqCst)
            || self.status() != PluginStatus::Running
        {
            return;
        }
        self.fail(format!("插件未在 {} 秒内完成握手", HANDSHAKE_TIMEOUT.as_secs()));
    }

    fn set_status(&self, status: PluginStatus) {
        *self.status.lock().unwrap() = status.clone();
        let _ = self.events.send(PluginEvent::StatusChanged { id: self.id.clone(), status });
    }

    // ---------- 进程事件 ----------

    fn handle_process_event(self: &Arc<Self>, token: u64, event: HostEvent) {
        // 上次启动遗留的输出（含延迟到达的退出上报）一律丢弃：pid / 状态已被新一次启动改写
        if token != self.run_token.load(Ordering::SeqCst) {
            return;
        }
        match event {
            HostEvent::Stdout { data } => {
                for line in data.lines() {
                    let line = line.trim();
                    if !line.is_empty() {
                        self.handle_line_for_session(token, line);
                    }
                }
            }
            // 插件 stderr 归入诊断输出（协议正文只走 stdout）：控制台 + 插件日志两处落点
            HostEvent::Stderr { data } => {
                for line in data.lines() {
                    if line.trim().is_empty() {
                        continue;
                    }
                    eprintln!("[plugin {}] {line}", self.id);
                    self.log.line(&format!("[stderr] {line}"));
                }
            }
            HostEvent::Terminated { code } => self.on_exit(code),
            HostEvent::Error { message } => {
                eprintln!("[plugin {}] {message}", self.id);
                self.log.line(&format!("[进程] {message}"));
            }
        }
    }

    /// 处理一行插件输出（当前会话）——测试与不关心会话身份的调用走这条。
    #[cfg(test)]
    fn handle_line(self: &Arc<Self>, line: &str) {
        self.handle_line_for_session(self.run_token.load(Ordering::SeqCst), line);
    }

    fn handle_line_for_session(self: &Arc<Self>, token: u64, line: &str) {
        let envelope = match Envelope::parse(line) {
            Ok(envelope) => envelope,
            Err(e) => {
                // 违规行原样落日志：stdout 被日志污染是插件最常见的一类故障
                self.log.line(&format!("[协议] 非信封输出（已停机）：{line}"));
                self.fail(format!("插件输出非协议内容（stdout 只允许 JSON-RPC 信封）：{e}"));
                return;
            }
        };
        match envelope {
            Envelope::Request(request) => self.handle_request(token, request),
            Envelope::Response(_) => {
                let text = "收到插件的响应（协议约定插件→宿主只有请求，宿主→插件只有通知）";
                eprintln!("[plugin {}] {text}", self.id);
                self.log.line(&format!("[协议] {text}"));
            }
            Envelope::Notification(notification) => {
                let text = format!(
                    "收到插件通知（协议约定插件→宿主只有请求）：{}",
                    notification.method
                );
                eprintln!("[plugin {}] {text}", self.id);
                self.log.line(&format!("[协议] {text}"));
            }
        }
    }

    /// 进程退出：主动停止 / 已判失败 / 已熔断时不动作，其余视为崩溃走退避或熔断。
    fn on_exit(self: &Arc<Self>, code: Option<i32>) {
        let status = self.status();
        if matches!(
            status,
            PluginStatus::Stopped | PluginStatus::Failed(_) | PluginStatus::CircuitBroken
        ) {
            return;
        }
        self.pid.store(0, Ordering::SeqCst);
        // 本次会话就此结束：令牌换代让在途的资源获取请求作废（读线程上一次请求可能正在起子进程 /
        // 注册热键），再把随它记账的资源收掉——托管服务不能因宿主崩溃而留成孤儿。
        // 先置会话失效再跑收尾：收尾扫过之后到达的工具调用登记不得落表（否则等满调用超时）。
        self.session_active.store(false, Ordering::SeqCst);
        self.run_token.fetch_add(1, Ordering::SeqCst);
        self.release_resources();
        let count = record_crash(&mut self.crash_times.lock().unwrap(), Instant::now());
        self.log.line(&format!("[进程] 意外退出（退出码 {code:?}）"));
        if count >= CRASH_LIMIT {
            eprintln!(
                "[plugin {}] {} 秒内崩溃 {count} 次，已熔断（需手动重试）",
                self.id,
                CRASH_WINDOW.as_secs()
            );
            self.log.line(&format!(
                "[熔断] {} 秒内崩溃 {count} 次，需手动重试",
                CRASH_WINDOW.as_secs()
            ));
            self.set_status(PluginStatus::CircuitBroken);
            return;
        }
        let delay = backoff_delay(count);
        eprintln!(
            "[plugin {}] 进程意外退出（退出码 {:?}），{:.1} 秒后自动重试",
            self.id,
            code,
            delay.as_secs_f64()
        );
        self.log
            .line(&format!("[重启] {:.1} 秒后重试（第 {count} 次崩溃）", delay.as_secs_f64()));
        self.set_status(PluginStatus::Restarting);
        let runtime = self.runtime.lock().unwrap().clone();
        let id = self.id.clone();
        let generation = self.generation.load(Ordering::SeqCst);
        std::thread::spawn(move || {
            std::thread::sleep(delay);
            if let Some(runtime) = runtime.upgrade() {
                runtime.restart_if_current(&id, generation);
            }
        });
    }

    // ---------- 插件 → 宿主请求 ----------

    fn handle_request(self: &Arc<Self>, token: u64, request: Request) {
        let method = request.method.as_str();
        let id = request.id;
        if let Some(permission) = capability::permission_for(method) {
            if !self.manifest.permissions.contains(&permission) {
                self.record_denied(method, &request.params);
                self.respond(
                    id,
                    None,
                    Some(RpcError::permission_denied(format!("能力未声明：{method}"))),
                );
                return;
            }
        }
        // 对话框由用户决定耗时（无上限），不能在读线程上等：分流为异步应答（结果由回填线程写回），
        // 读线程继续消费插件输出——否则对话框期间插件输出与退出上报都被拖住。
        // （http 有明确超时上限，按顺序处理不破「读线程串行处理请求」这条既有语义。）
        if method == method::dialog::PICK_FILE || method == method::dialog::PICK_DIRECTORY {
            self.handle_dialog_request(token, id, method, &request.params);
            return;
        }
        // 接管与插件窗口的落地在壳（OS 窗口副作用只由壳触发），结果由壳回填——分流为异步应答，
        // 读线程不等待窗口创建与接管应用。
        if matches!(
            method,
            method::view::TAKEOVER
                | method::window::OPEN
                | method::window::CLOSE
                | method::window::UPDATE
                | method::window::SHOW
        ) {
            self.handle_ui_request(token, id, method, &request.params);
            return;
        }
        // 壳侧执行的服务调用（对话编排、剪贴板、仓库读写、托盘、协作、表格等）分流为异步应答：
        // 结果由回填线程写回，读线程继续消费插件输出。
        if matches!(
            method,
            method::ai::CHAT_SEND
                | method::ai::CHAT_STOP
                | method::ai::CHAT_HISTORY
                | method::ai::CHAT_RENAME
                | method::ai::CHAT_DELETE
                | method::ai::CHAT_COMPACT
                | method::clipboard::WRITE_TEXT
                | method::clipboard::READ_TEXT
                | method::clipboard::READ_IMAGE
                | method::vault::READ_TEXT
                | method::vault::WRITE_TEXT
                | method::vault::READ_BINARY
                | method::vault::WRITE_BINARY
                | method::vault::IMPORT_FILE
                | method::vault::IMAGE_INFO
                | method::vault::LIST
                | method::vault::EXISTS
                | method::table::SNAPSHOT
                | method::table::UPDATE_CELL
                | method::table::ADD_ROW
                | method::table::REMOVE_ROW
                | method::table::SELECT_ROW
                | method::table::SET_CELL_IMAGE
                | method::tray::SET_ITEMS
                | method::collab::PEERS
                | method::collab::SEND
                | method::view::SCROLL_TO
        ) {
            self.handle_shell_request(token, id, method, &request.params);
            return;
        }
        match self.dispatch_for_session(token, method, &request.params) {
            Ok(value) => self.respond(id, Some(value), None),
            Err(error) => self.respond(id, None, Some(error)),
        }
    }

    /// 打开一次对话框并在独立线程上等结果回执。
    fn handle_dialog_request(
        self: &Arc<Self>,
        token: u64,
        id: RequestId,
        method_name: &str,
        params: &Value,
    ) {
        let request = match dialog_request(method_name, params) {
            Ok(request) => request,
            Err(error) => {
                self.respond(id, None, Some(error));
                return;
            }
        };
        let runtime = self.runtime.lock().unwrap().clone();
        let Some(runtime) = runtime.upgrade() else {
            self.respond(id, None, Some(RpcError::failed("对话框不可用：宿主运行时不可用")));
            return;
        };
        let opened = self.with_session(token, || runtime.open_dialog(&self.id, request));
        let rx = match opened {
            Ok(rx) => rx,
            Err(e) => {
                self.respond(id, None, Some(RpcError::failed(e)));
                return;
            }
        };
        let instance = self.clone();
        std::thread::spawn(move || match rx.recv() {
            Ok(Ok(path)) => match encode_picked(path) {
                Ok(value) => instance.respond(id, Some(value), None),
                Err(error) => instance.respond(id, None, Some(error)),
            },
            Ok(Err(reason)) => instance.respond(id, None, Some(RpcError::failed(reason))),
            // 发送端被丢弃（在途登记已失效）：会话结束，不悬挂
            Err(_) => instance.respond(id, None, Some(RpcError::failed("对话框请求已取消"))),
        });
    }

    /// 接管与插件窗口请求：登记后在独立线程上等壳回填结果（窗口创建与接管应用由壳执行）。
    fn handle_ui_request(
        self: &Arc<Self>,
        token: u64,
        id: RequestId,
        method_name: &str,
        params: &Value,
    ) {
        let runtime = self.runtime.lock().unwrap().clone();
        let Some(runtime) = runtime.upgrade() else {
            self.respond(id, None, Some(RpcError::failed("界面服务不可用：宿主运行时不可用")));
            return;
        };
        let opened = self.with_session(token, || {
            // parse 的 RpcError 统一转可读串（本闭包的错误类型是 String）
            match method_name {
                method::view::TAKEOVER => {
                    let p: method::TakeoverParams = parse(params).map_err(|e| e.to_string())?;
                    runtime.open_takeover(&self.id, p.scope, &p.surface_id)
                }
                method::window::OPEN => {
                    let p: method::WindowOpenParams =
                        parse(params).map_err(|e| e.to_string())?;
                    runtime.open_plugin_window(&self.id, p)
                }
                method::window::CLOSE => {
                    let p: method::WindowCloseParams =
                        parse(params).map_err(|e| e.to_string())?;
                    runtime.close_plugin_window(&self.id, &p.id)
                }
                method::window::UPDATE => {
                    let p: method::WindowUpdateParams =
                        parse(params).map_err(|e| e.to_string())?;
                    runtime.update_plugin_window(&self.id, p)
                }
                method::window::SHOW => {
                    let p: method::WindowIdParams = parse(params).map_err(|e| e.to_string())?;
                    runtime.show_plugin_window(&self.id, &p.id)
                }
                _ => Err("未知界面服务请求".to_string()),
            }
        });
        let rx = match opened {
            Ok(rx) => rx,
            Err(e) => {
                self.respond(id, None, Some(RpcError::failed(e)));
                return;
            }
        };
        let instance = self.clone();
        std::thread::spawn(move || match rx.recv() {
            Ok(Ok(())) => instance.respond(id, Some(json!({})), None),
            Ok(Err(reason)) => instance.respond(id, None, Some(RpcError::failed(reason))),
            Err(_) => instance.respond(id, None, Some(RpcError::failed("界面服务请求已取消"))),
        });
    }

    /// 转发一次壳侧服务调用并在独立线程上等壳回填（耗时由壳侧操作决定，不能占读线程）。
    fn handle_shell_request(
        self: &Arc<Self>,
        token: u64,
        id: RequestId,
        method_name: &str,
        params: &Value,
    ) {
        let runtime = self.runtime.lock().unwrap().clone();
        let Some(runtime) = runtime.upgrade() else {
            self.respond(id, None, Some(RpcError::failed("宿主服务不可用：宿主运行时不可用")));
            return;
        };
        let opened = self.with_session(token, || {
            // parse 的 RpcError 统一转可读串（本闭包的错误类型是 String）
            let request = match method_name {
                method::ai::CHAT_SEND => HostRequest::ChatSend {
                    plugin_id: self.id.clone(),
                    request_id: 0,
                    params: parse(params).map_err(|e| e.to_string())?,
                },
                method::ai::CHAT_STOP => HostRequest::ChatStop {
                    plugin_id: self.id.clone(),
                    request_id: 0,
                    params: parse(params).map_err(|e| e.to_string())?,
                },
                method::ai::CHAT_HISTORY => HostRequest::ChatHistory {
                    plugin_id: self.id.clone(),
                    request_id: 0,
                    params: parse(params).map_err(|e| e.to_string())?,
                },
                method::clipboard::WRITE_TEXT => {
                    let p: method::TextPayload = parse(params).map_err(|e| e.to_string())?;
                    HostRequest::ClipboardWrite {
                        plugin_id: self.id.clone(),
                        request_id: 0,
                        text: p.text,
                    }
                }
                method::clipboard::READ_TEXT => HostRequest::ClipboardRead {
                    plugin_id: self.id.clone(),
                    request_id: 0,
                },
                method::clipboard::READ_IMAGE => HostRequest::ClipboardReadImage {
                    plugin_id: self.id.clone(),
                    request_id: 0,
                },
                method::vault::READ_TEXT => {
                    let p: method::PathParams = parse(params).map_err(|e| e.to_string())?;
                    HostRequest::VaultReadText {
                        plugin_id: self.id.clone(),
                        request_id: 0,
                        path: p.path,
                    }
                }
                method::vault::WRITE_TEXT => {
                    let p: method::WriteTextParams = parse(params).map_err(|e| e.to_string())?;
                    HostRequest::VaultWriteText {
                        plugin_id: self.id.clone(),
                        request_id: 0,
                        path: p.path,
                        content: p.content,
                    }
                }
                method::vault::LIST => {
                    let p: method::PathParams = parse(params).map_err(|e| e.to_string())?;
                    HostRequest::VaultList {
                        plugin_id: self.id.clone(),
                        request_id: 0,
                        path: p.path,
                    }
                }
                method::vault::READ_BINARY => {
                    let p: method::PathParams = parse(params).map_err(|e| e.to_string())?;
                    HostRequest::VaultReadBinary {
                        plugin_id: self.id.clone(),
                        request_id: 0,
                        path: p.path,
                    }
                }
                method::vault::WRITE_BINARY => {
                    let p: method::WriteBinaryParams = parse(params).map_err(|e| e.to_string())?;
                    // 上限在分派处判：壳不必为一次注定失败的写入再搬一遍数据
                    let bytes = decode_vault_binary(&p.base64, MAX_VAULT_BINARY_BYTES)?;
                    HostRequest::VaultWriteBinary {
                        plugin_id: self.id.clone(),
                        request_id: 0,
                        path: p.path,
                        bytes,
                    }
                }
                method::vault::IMPORT_FILE => {
                    let p: method::ImportFileParams = parse(params).map_err(|e| e.to_string())?;
                    // 源文件校验在此做：壳只认绝对路径，不理解插件目录布局
                    let source = self.import_source(&p.source)?;
                    HostRequest::VaultImportFile {
                        plugin_id: self.id.clone(),
                        request_id: 0,
                        path: p.path,
                        source,
                    }
                }
                method::vault::EXISTS => {
                    let p: method::PathParams = parse(params).map_err(|e| e.to_string())?;
                    HostRequest::VaultExists {
                        plugin_id: self.id.clone(),
                        request_id: 0,
                        path: p.path,
                    }
                }
                method::vault::IMAGE_INFO => {
                    let p: method::PathParams = parse(params).map_err(|e| e.to_string())?;
                    HostRequest::VaultImageInfo {
                        plugin_id: self.id.clone(),
                        request_id: 0,
                        path: p.path,
                    }
                }
                method::tray::SET_ITEMS => {
                    let p: method::SetTrayItemsParams = parse(params).map_err(|e| e.to_string())?;
                    p.validate()?;
                    HostRequest::TraySetItems {
                        plugin_id: self.id.clone(),
                        request_id: 0,
                        params: p,
                    }
                }
                method::collab::PEERS => HostRequest::CollabPeers {
                    plugin_id: self.id.clone(),
                    request_id: 0,
                },
                method::collab::SEND => {
                    let p: method::CollabSendParams = parse(params).map_err(|e| e.to_string())?;
                    p.validate()?;
                    HostRequest::CollabSend {
                        plugin_id: self.id.clone(),
                        request_id: 0,
                        params: p,
                    }
                }
                method::table::SNAPSHOT => HostRequest::TableSnapshot {
                    plugin_id: self.id.clone(),
                    request_id: 0,
                },
                method::table::UPDATE_CELL => {
                    let p: method::TableUpdateCellParams = parse(params).map_err(|e| e.to_string())?;
                    HostRequest::TableUpdateCell {
                        plugin_id: self.id.clone(),
                        request_id: 0,
                        row_id: p.row_id,
                        field_id: p.field_id,
                        value: p.value,
                    }
                }
                method::table::ADD_ROW => HostRequest::TableAddRow {
                    plugin_id: self.id.clone(),
                    request_id: 0,
                },
                method::table::REMOVE_ROW => {
                    let p: method::TableRowIdParams = parse(params).map_err(|e| e.to_string())?;
                    HostRequest::TableRemoveRow {
                        plugin_id: self.id.clone(),
                        request_id: 0,
                        row_id: p.row_id,
                    }
                }
                method::table::SELECT_ROW => {
                    let p: method::TableRowIdParams = parse(params).map_err(|e| e.to_string())?;
                    HostRequest::TableSelectRow {
                        plugin_id: self.id.clone(),
                        request_id: 0,
                        row_id: p.row_id,
                    }
                }
                method::table::SET_CELL_IMAGE => {
                    let p: method::TableSetCellImageParams =
                        parse(params).map_err(|e| e.to_string())?;
                    HostRequest::TableSetCellImage {
                        plugin_id: self.id.clone(),
                        request_id: 0,
                        params: p,
                    }
                }
                method::view::SCROLL_TO => {
                    let p: method::ScrollToParams = parse(params).map_err(|e| e.to_string())?;
                    p.validate()?;
                    HostRequest::ScrollTo {
                        plugin_id: self.id.clone(),
                        request_id: 0,
                        params: p,
                    }
                }
                method::ai::CHAT_RENAME => {
                    let p: method::ChatRenameParams = parse(params).map_err(|e| e.to_string())?;
                    HostRequest::ChatRename {
                        plugin_id: self.id.clone(),
                        request_id: 0,
                        params: p,
                    }
                }
                method::ai::CHAT_DELETE => {
                    let p: method::ChatSessionParams = parse(params).map_err(|e| e.to_string())?;
                    HostRequest::ChatDelete {
                        plugin_id: self.id.clone(),
                        request_id: 0,
                        params: p,
                    }
                }
                method::ai::CHAT_COMPACT => {
                    let p: method::ChatSessionParams = parse(params).map_err(|e| e.to_string())?;
                    HostRequest::ChatCompact {
                        plugin_id: self.id.clone(),
                        request_id: 0,
                        params: p,
                    }
                }
                _ => return Err("未知宿主服务请求".to_string()),
            };
            runtime.open_shell_call(request)
        });
        let rx = match opened {
            Ok(rx) => rx,
            Err(e) => {
                self.respond(id, None, Some(RpcError::failed(e)));
                return;
            }
        };
        let instance = self.clone();
        std::thread::spawn(move || match rx.recv() {
            Ok(Ok(value)) => instance.respond(id, Some(value), None),
            Ok(Err(reason)) => instance.respond(id, None, Some(RpcError::failed(reason))),
            Err(_) => instance.respond(id, None, Some(RpcError::failed("宿主服务请求已取消"))),
        });
    }

    /// 分派一条请求（当前会话）——测试与不关心会话身份的调用走这条。
    #[cfg(test)]
    fn dispatch(self: &Arc<Self>, method: &str, params: &Value) -> Result<Value, RpcError> {
        self.dispatch_for_session(self.run_token.load(Ordering::SeqCst), method, params)
    }

    fn dispatch_for_session(
        self: &Arc<Self>,
        token: u64,
        method: &str,
        params: &Value,
    ) -> Result<Value, RpcError> {
        match method {
            handshake::INITIALIZE => self.on_initialize(params),
            event::SUBSCRIBE => self.on_subscribe(params),
            method::view::SUBMIT => self.on_submit(token, params),
            method::view::REGISTER => self.on_register(token, params),
            method::view::UNREGISTER => self.on_unregister(token, params),
            method::view::DEFINE_ICON => self.on_define_icon(token, params),
            method::canvas::SUBMIT => self.on_canvas_submit(token, params),
            method::state::READ => {
                let json = capability::read_state(&self.state_dir).map_err(RpcError::failed)?;
                Ok(json!({ "json": json }))
            }
            method::state::WRITE => {
                let p: method::StateWriteParams = parse(params)?;
                capability::write_state(&self.state_dir, &p.json).map_err(RpcError::failed)?;
                Ok(json!({}))
            }
            method::notification::SHOW => {
                let p: method::ShowNotificationParams = parse(params)?;
                // 无订阅者（壳已收尾）时 send 失败即回错误：插件不能把「塞进通道」当成「已弹出」
                self.events
                    .send(PluginEvent::Notification {
                        id: self.id.clone(),
                        title: p.title,
                        body: p.body,
                        level: p.level,
                    })
                    .map_err(|_| RpcError::failed("通知通道已关闭，未投递到宿主".to_string()))?;
                Ok(json!({}))
            }
            // ---------- fs ----------
            method::fs::PRIVATE_DIR => {
                let path = fs::private_dir(&self.state_dir).map_err(RpcError::failed)?;
                Ok(json!({ "path": path }))
            }
            method::fs::READ_TEXT => {
                let p: method::PathParams = parse(params)?;
                let content = fs::read_text(&p.path).map_err(RpcError::failed)?;
                Ok(json!({ "content": content }))
            }
            method::fs::WRITE_TEXT => {
                let p: method::WriteTextParams = parse(params)?;
                fs::write_text(&p.path, &p.content).map_err(RpcError::failed)?;
                Ok(json!({}))
            }
            method::fs::READ_BINARY => {
                let p: method::PathParams = parse(params)?;
                let bytes = fs::read_binary(&p.path).map_err(RpcError::failed)?;
                Ok(json!({ "base64": base64::engine::general_purpose::STANDARD.encode(bytes) }))
            }
            method::fs::WRITE_BINARY => {
                let p: method::WriteBinaryParams = parse(params)?;
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(&p.base64)
                    .map_err(|e| RpcError::failed(format!("base64 解码失败：{e}")))?;
                fs::write_binary(&p.path, &bytes).map_err(RpcError::failed)?;
                Ok(json!({}))
            }
            method::fs::REMOVE => {
                let p: method::PathParams = parse(params)?;
                fs::remove(&p.path).map_err(RpcError::failed)?;
                Ok(json!({}))
            }
            method::fs::EXISTS => {
                let p: method::PathParams = parse(params)?;
                Ok(json!({ "exists": fs::exists(&p.path) }))
            }
            method::fs::LIST => {
                let p: method::PathParams = parse(params)?;
                let entries = fs::list(&p.path).map_err(RpcError::failed)?;
                Ok(json!({ "entries": entries }))
            }

            // ---------- 布局声明 ----------
            method::layout::DECLARE_DEFAULT => self.on_declare_default_layout(token, params),

            // ---------- process ----------
            method::process::SPAWN => {
                let p: method::SpawnParams = parse(params)?;
                let pid = self.spawn_child(token, p).map_err(RpcError::failed)?;
                Ok(json!({ "pid": pid }))
            }
            method::process::WRITE_STDIN => {
                let p: method::WriteStdinParams = parse(params)?;
                self.require_child(p.pid)?;
                self.process_host.write_stdin(p.pid, &p.data).map_err(RpcError::failed)?;
                Ok(json!({}))
            }
            method::process::CLOSE_STDIN => {
                let p: method::PidParams = parse(params)?;
                self.require_child(p.pid)?;
                self.process_host.close_stdin(p.pid);
                Ok(json!({}))
            }
            method::process::KILL => {
                let p: method::PidParams = parse(params)?;
                if !self.children.remove(p.pid) {
                    return Err(RpcError::failed(format!("进程 {} 不是本插件启动的", p.pid)));
                }
                self.process_host.kill_process_tree(p.pid).map_err(RpcError::failed)?;
                Ok(json!({}))
            }

            // ---------- shortcuts ----------
            method::shortcut::REGISTER => {
                let p: method::RegisterShortcutParams = parse(params)?;
                self.with_session(token, || self.shortcuts.register(&self.id, &p.accelerator))
                    .map_err(RpcError::failed)?;
                Ok(json!({}))
            }
            method::shortcut::UNREGISTER => {
                let p: method::AcceleratorParams = parse(params)?;
                self.with_session(token, || self.shortcuts.unregister(&self.id, &p.accelerator))
                    .map_err(RpcError::failed)?;
                Ok(json!({}))
            }
            method::shortcut::QUERY => {
                let p: method::ShortcutQueryParams = parse(params)?;
                // 与同面注册 / 注销同口径过会话闸门：已停会话的在途查询不再白跑一次 OS 试探
                let (available, owner) = self
                    .with_session(token, || self.shortcuts.query(&self.id, &p.accelerator))
                    .map_err(RpcError::failed)?;
                let result = method::ShortcutQueryResult { available, owner };
                serde_json::to_value(result)
                    .map_err(|e| RpcError::failed(format!("应答序列化失败：{e}")))
            }

            // ---------- http / dialog ----------
            method::http::REQUEST => {
                let p: method::HttpRequestParams = parse(params)?;
                let result = http::request(&p).map_err(RpcError::failed)?;
                serde_json::to_value(result)
                    .map_err(|e| RpcError::failed(format!("应答序列化失败：{e}")))
            }

            // 对话框走异步应答（见 handle_request 的分流），不经此处同步应答
            method::dialog::PICK_FILE | method::dialog::PICK_DIRECTORY => {
                Err(RpcError::failed("对话框请求须走异步应答路径"))
            }
            // AI 工具注册与回报：登记 / 摘除在会话闸门内，收尾跑完之后到达的迟到请求不得复活工具
            method::ai::REGISTER_TOOL => {
                let p: method::RegisterToolParams = parse(params)?;
                let runtime = self.runtime_upgrade()?;
                self.with_session(token, || {
                    runtime.register_tool(&self.id, &self.manifest.name, p)
                })
                .map_err(RpcError::failed)?;
                Ok(json!({}))
            }
            method::ai::UNREGISTER_TOOL => {
                let p: method::UnregisterToolParams = parse(params)?;
                let runtime = self.runtime_upgrade()?;
                self.with_session(token, || runtime.unregister_tool(&self.id, &p.name))
                    .map_err(RpcError::failed)?;
                Ok(json!({}))
            }
            method::ai::TOOL_RESULT => {
                let p: method::ToolResultParams = parse(params)?;
                self.runtime_upgrade()?
                    .resolve_tool_result(&self.id, &p.call_id, p.content, p.is_error)
                    .map_err(RpcError::failed)?;
                Ok(json!({}))
            }
            // AI 对话 / 剪贴板 / 仓库读写 / 表格读写 / 滚动指令走异步应答（见 handle_request 的分流）
            method::ai::CHAT_SEND
            | method::ai::CHAT_STOP
            | method::ai::CHAT_HISTORY
            | method::ai::CHAT_RENAME
            | method::ai::CHAT_DELETE
            | method::ai::CHAT_COMPACT
            | method::clipboard::WRITE_TEXT
            | method::clipboard::READ_TEXT
            | method::clipboard::READ_IMAGE
            | method::vault::READ_TEXT
            | method::vault::WRITE_TEXT
            | method::vault::READ_BINARY
            | method::vault::WRITE_BINARY
            | method::vault::IMPORT_FILE
            | method::vault::IMAGE_INFO
            | method::vault::LIST
            | method::vault::EXISTS
            | method::table::SNAPSHOT
            | method::table::UPDATE_CELL
            | method::table::ADD_ROW
            | method::table::REMOVE_ROW
            | method::table::SELECT_ROW
            | method::table::SET_CELL_IMAGE
            | method::tray::SET_ITEMS
            | method::collab::PEERS
            | method::collab::SEND
            | method::view::SCROLL_TO
            | method::window::UPDATE
            | method::window::SHOW => {
                Err(RpcError::failed("宿主服务请求须走异步应答路径"))
            }
            // 已声明但尚未实现的能力面：以可读失败收场，不静默成功
            other if capability::permission_for(other).is_some() => {
                Err(RpcError::failed(format!("能力面尚未实现：{other}")))
            }
            other => Err(RpcError::failed(format!("未知方法：{other}"))),
        }
    }

    fn on_initialize(self: &Arc<Self>, params: &Value) -> Result<Value, RpcError> {
        let p: InitializeParams = parse(params)?;
        if !handshake::compatible(p.api_version, API_VERSION) {
            let reason = format!("协议版本不匹配：插件 {}，宿主 {API_VERSION}", p.api_version);
            self.fail(reason);
            return Err(RpcError::failed("协议版本不匹配"));
        }
        if p.plugin.id != self.id {
            let reason = format!("插件自报 id {} 与安装 id {} 不一致", p.plugin.id, self.id);
            self.fail(reason);
            return Err(RpcError::failed("插件自报 id 与安装 id 不一致"));
        }
        self.handshaken.store(true, Ordering::SeqCst);
        self.log.line(&format!(
            "[握手] 完成（插件 {} v{}，apiVersion {}）",
            p.plugin.name, p.plugin.version, p.api_version
        ));
        let result = InitializeResult {
            api_version: API_VERSION,
            host: HostInfo {
                name: HOST_NAME.to_string(),
                version: self.host_version.clone(),
            },
        };
        serde_json::to_value(result).map_err(|e| RpcError::failed(format!("握手应答序列化失败：{e}")))
    }

    fn on_subscribe(&self, params: &Value) -> Result<Value, RpcError> {
        let p: event::SubscribeParams = parse(params)?;
        if !matches!(
            p.name.as_str(),
            event::name::VAULT_SWITCH
                | event::name::TABLE_CHANGED
                | event::name::AI_CHAT_CHANGED
                | event::name::VAULT_FILE_CHANGED
        ) {
            return Err(RpcError::failed(format!("未知事件名：{}", p.name)));
        }
        self.subscriptions.lock().unwrap().insert(p.name.clone());
        // 表格变更订阅：已有打开表格即补发当前路径——广播只达订阅时刻之后的时点，
        // 迟到的订阅者（插件启动晚于表格打开）不补发就不知道当前状态
        if p.name == event::name::TABLE_CHANGED {
            if let Ok(runtime) = self.runtime_upgrade() {
                if let Some(path) = runtime.current_table_path() {
                    self.notify_domain_event(
                        event::name::TABLE_CHANGED,
                        &event::TableChangedPayload { path: Some(path) },
                    );
                }
            }
        }
        Ok(json!({ "subscribed": true }))
    }

    /// 校验并规范化导入源：必须是插件私有目录（`<插件目录>/data`）内的既有普通文件。
    /// 解析符号链接后按真实落点判定包含关系，链接指向目录外即拒收。
    fn import_source(&self, source: &str) -> Result<String, String> {
        if source.trim().is_empty() {
            return Err("导入源路径不能为空".to_string());
        }
        let private = fs::private_dir(&self.state_dir)?;
        let root = std::fs::canonicalize(&private)
            .map_err(|e| format!("插件私有目录不可达（{private}）：{e}"))?;
        let real = std::fs::canonicalize(source)
            .map_err(|e| format!("导入源不存在或不可访问（{source}）：{e}"))?;
        if !real.starts_with(&root) {
            return Err("导入源必须位于插件私有目录内".to_string());
        }
        let meta = std::fs::metadata(&real).map_err(|e| format!("读取导入源失败：{e}"))?;
        if !meta.is_file() {
            return Err("导入源不是文件".to_string());
        }
        Ok(real.to_string_lossy().into_owned())
    }

    /// 声明默认布局：只存档并广播给壳，是否摆进场景、摆进哪个场景由壳决定。
    fn on_declare_default_layout(
        self: &Arc<Self>,
        token: u64,
        params: &Value,
    ) -> Result<Value, RpcError> {
        let p: method::DeclareDefaultLayoutParams = parse(params)?;
        if !crate::layout::layout_spec_name_valid(&p.name) {
            return Err(RpcError::failed("布局名不能为空且不超过长度上限"));
        }
        if !crate::layout::layout_spec_valid(&p.spec) {
            return Err(RpcError::failed(
                "布局规格非法：检查方向、尺寸、面板与视图数量",
            ));
        }
        self.with_session(token, || {
            *self.default_layout.lock().unwrap() = Some((p.name.clone(), p.spec.clone()));
            Ok(())
        })
        .map_err(RpcError::failed)?;
        let _ = self.events.send(PluginEvent::DefaultLayoutDeclared {
            id: self.id.clone(),
            name: p.name,
            spec: p.spec,
        });
        Ok(json!({}))
    }

    /// 校验并接收界面树：只校验与存档，渲染由宿主界面层承担。surfaceId 为空即拒。
    /// 存档与贡献登记都在会话闸门内——收尾跑完之后到达的提交不得让已停插件的界面继续存活。
    fn on_submit(self: &Arc<Self>, token: u64, params: &Value) -> Result<Value, RpcError> {
        let p: method::SubmitParams = parse(params)?;
        if p.surface_id.trim().is_empty() {
            return Err(RpcError::failed("surfaceId 不能为空"));
        }
        view::ViewNode::validate(&p.tree).map_err(RpcError::failed)?;
        self.with_session(token, || {
            let mut surfaces = self.surfaces.lock().unwrap();
            // 树整发整换：新树里已不存在的画布节点，其清单随之清除（清单存档不得比界面树活得久）
            let mut canvases = self.canvases.lock().unwrap();
            canvases.retain(|(surface_id, node_id), _| {
                surface_id != &p.surface_id || has_canvas_node(&p.tree, node_id)
            });
            surfaces.insert(p.surface_id.clone(), p.tree);
            self.view_submits.fetch_add(1, Ordering::Relaxed);
            Ok(())
        })
        .map_err(RpcError::failed)?;
        let _ = self.events.send(PluginEvent::ViewSubmitted { id: self.id.clone() });
        Ok(json!({ "applied": true }))
    }

    /// 校验并接收画布指令清单：面须已有界面树、节点须在树里且为 canvas 节点，指令逐条校验。
    /// 清单整发整换，空清单 = 清空画布；存档与会话闸门同口径（收尾后到达的提交不得让清单复活）。
    fn on_canvas_submit(self: &Arc<Self>, token: u64, params: &Value) -> Result<Value, RpcError> {
        let p: method::CanvasSubmitParams = parse(params)?;
        if p.surface_id.trim().is_empty() {
            return Err(RpcError::failed("surfaceId 不能为空"));
        }
        if p.node_id.trim().is_empty() {
            return Err(RpcError::failed("nodeId 不能为空"));
        }
        canvas::validate_commands(&p.commands).map_err(RpcError::failed)?;
        self.with_session(token, || {
            let surfaces = self.surfaces.lock().unwrap();
            let tree = surfaces
                .get(&p.surface_id)
                .ok_or_else(|| format!("面尚未提交界面树：{}", p.surface_id))?;
            let node = tree
                .find(&p.node_id)
                .ok_or_else(|| format!("画布节点不在当前界面树里：{}", p.node_id))?;
            if !matches!(node.body, view::NodeBody::Canvas { .. }) {
                return Err(format!("节点不是画布：{}", p.node_id));
            }
            drop(surfaces);
            self.canvases
                .lock()
                .unwrap()
                .insert((p.surface_id.clone(), p.node_id.clone()), Arc::new(p.commands));
            Ok(())
        })
        .map_err(RpcError::failed)?;
        let _ = self.events.send(PluginEvent::ViewSubmitted { id: self.id.clone() });
        Ok(json!({ "applied": true }))
    }

    /// 注册一个贡献点：id 插件内唯一、非空且不含 `:`（宿主的面标识以它作命名空间分隔）。
    /// 贡献注册是 UI 面声明，无权限门槛；停用 / 崩溃 / 卸载随会话收尾由宿主清除。
    fn on_register(self: &Arc<Self>, token: u64, params: &Value) -> Result<Value, RpcError> {
        let p: RegisterParams = parse(params)?;
        if p.id.trim().is_empty() {
            return Err(RpcError::failed("贡献 id 不能为空"));
        }
        if p.id.contains(':') {
            return Err(RpcError::failed(format!("贡献 id 不得含 `:`：{}", p.id)));
        }
        if p.label.trim().is_empty() {
            return Err(RpcError::failed("贡献 label 不能为空"));
        }
        if let Some(padding) = p.padding {
            if padding.edges().iter().any(|value| !value.is_finite() || *value < 0.0) {
                return Err(RpcError::failed("贡献内边距须为有限的非负数"));
            }
        }
        self.with_session(token, || {
            let mut contributions = self.contributions.lock().unwrap();
            if contributions.iter().any(|entry| entry.id == p.id) {
                return Err(format!("贡献 id 已注册：{}", p.id));
            }
            contributions.push(ContributionEntry {
                slot: p.slot,
                id: p.id,
                label: p.label,
                icon: p.icon,
                padding: p.padding,
            });
            Ok(())
        })
        .map_err(RpcError::failed)?;
        let _ = self.events.send(PluginEvent::ContributionsChanged { id: self.id.clone() });
        Ok(json!({}))
    }

    /// 定义一枚自备图标形状：名字须为字母 / 数字 / `_` / `-` 且不超过 64，路径非空。
    /// 同名重复定义即原地更新。定义是界面通道（不占权限声明），随会话收尾清除。
    fn on_define_icon(self: &Arc<Self>, token: u64, params: &Value) -> Result<Value, RpcError> {
        let p: method::DefineIconParams = parse(params)?;
        if !valid_icon_name(&p.name) {
            return Err(RpcError::failed(format!(
                "图标名非法（字母 / 数字 / `_` / `-`，最长 64）：{}",
                p.name
            )));
        }
        if p.path.trim().is_empty() {
            return Err(RpcError::failed("图标路径不能为空"));
        }
        self.with_session(token, || {
            self.icons.lock().unwrap().insert(p.name, p.path);
            Ok(())
        })
        .map_err(RpcError::failed)?;
        let _ = self.events.send(PluginEvent::ContributionsChanged { id: self.id.clone() });
        Ok(json!({}))
    }

    /// 撤销一个贡献点并撤掉它的界面树；未注册的 id 报可读错误。
    fn on_unregister(self: &Arc<Self>, token: u64, params: &Value) -> Result<Value, RpcError> {
        let p: UnregisterParams = parse(params)?;
        self.with_session(token, || {
            let mut contributions = self.contributions.lock().unwrap();
            let before = contributions.len();
            contributions.retain(|entry| entry.id != p.id);
            if contributions.len() == before {
                return Err(format!("未注册的贡献 id：{}", p.id));
            }
            self.surfaces.lock().unwrap().remove(&p.id);
            self.canvases
                .lock()
                .unwrap()
                .retain(|(surface_id, _), _| surface_id != &p.id);
            Ok(())
        })
        .map_err(RpcError::failed)?;
        let _ = self.events.send(PluginEvent::ContributionsChanged { id: self.id.clone() });
        let _ = self.events.send(PluginEvent::ViewSubmitted { id: self.id.clone() });
        Ok(json!({}))
    }

    // ---------- 能力面实现 ----------

    /// 起一个托管子进程并记账。事件回调要先于 spawn 就绪、pid 只有 spawn 返回后才知道，故用槽位
    /// 中转：pid 未定前到达的事件先按序暂存，定下后补齐——直接丢弃会丢掉子进程最早几行输出
    /// （`plugin_process` 的读线程先于 spawn 返回启动，启动快的子进程能在那之前就写出第一行）。
    /// 整段在会话闸门内完成——「起进程 + 记账」与停用收尾互斥，否则刚起的子进程会逃过本次收尾。
    fn spawn_child(self: &Arc<Self>, token: u64, p: method::SpawnParams) -> Result<u32, String> {
        let env: Option<std::collections::HashMap<String, String>> =
            p.env.map(|env| env.into_iter().collect());
        let slot = Arc::new(Mutex::new(SpawnSlot::default()));
        let slot_for_sink = slot.clone();
        let instance = self.clone();
        let sink: EventSink = Arc::new(move |event| {
            let mut slot = slot_for_sink.lock().unwrap();
            if let Some(pid) = slot.pid {
                drop(slot);
                instance.on_child_event(pid, event);
            } else {
                slot.pending.push(event);
            }
        });
        self.with_session(token, || {
            let pid =
                self.process_host
                    .spawn(&p.program, &p.args, p.cwd.as_deref(), env.as_ref(), sink)?;
            self.children.add(pid);
            let queued = {
                let mut slot = slot.lock().unwrap();
                slot.pid = Some(pid);
                std::mem::take(&mut slot.pending)
            };
            for event in queued {
                self.on_child_event(pid, event);
            }
            Ok(pid)
        })
    }

    /// 校验 pid 确属本插件（`process.writeStdin` / `closeStdin` 不得操作别人的进程）。
    fn require_child(&self, pid: u32) -> Result<(), RpcError> {
        if self.children.contains(pid) {
            Ok(())
        } else {
            Err(RpcError::failed(format!("进程 {pid} 不是本插件启动的")))
        }
    }

    /// 托管子进程的输出与退出：转发为 `host:process.event` 通知；退出即摘除记账。
    fn on_child_event(self: &Arc<Self>, pid: u32, event: HostEvent) {
        let terminated = matches!(&event, HostEvent::Terminated { .. });
        self.notify(
            method::process::HOST_EVENT,
            &method::ProcessEventParams { pid, event: child_process::to_protocol_event(event) },
        );
        if terminated {
            self.children.remove(pid);
        }
    }

    /// 是否声明「切仓库容器时保持挂载」（未声明者由宿主在切容器时重载）。
    pub(crate) fn keep_mounted_on_vault_switch(&self) -> bool {
        self.manifest.keep_mounted_on_vault_switch
    }

    /// 是否订阅了某宿主广播事件。
    pub(crate) fn subscribed_to(&self, name: &str) -> bool {
        self.subscriptions.lock().unwrap().contains(name)
    }

    /// 转投一条全局快捷键触发（`host:shortcut.triggered`）。
    pub(crate) fn notify_shortcut(&self, accelerator: &str) {
        self.notify(method::shortcut::HOST_TRIGGERED, &shortcut::triggered_params(accelerator));
    }

    /// 转投一次托盘条目点击（`host:tray.click`）。条目只由声明过 `tray` 的插件建立，
    /// 点击事件不再判权限。
    pub(crate) fn notify_tray_click(&self, item_id: &str) {
        let params = method::TrayClickParams { id: item_id.to_string() };
        self.notify(method::tray::HOST_CLICK, &params);
    }

    /// 转投一条同空间对等插件的消息（`host:collab.message`）。通道名由发送方填写，
    /// 未声明 `collab` 的插件不收（与能力面同一条闸门）。
    pub(crate) fn notify_collab_message(&self, params: &method::CollabMessageParams) {
        if !self.manifest.permissions.contains(&Permission::Collab) {
            return;
        }
        self.notify(method::collab::HOST_MESSAGE, params);
    }

    /// 回发一条节点界面事件（`host:view.event`）。
    pub(crate) fn notify_view_event(&self, surface_id: &str, node_id: &str, event: ViewEvent) {
        let params = ViewEventParams {
            surface_id: surface_id.to_string(),
            node_id: node_id.to_string(),
            event,
        };
        self.notify(method::view::HOST_EVENT, &params);
    }

    /// 回发画布节点当前的像素尺寸（`host:canvas.resized`；尺寸变化时发）。
    pub(crate) fn notify_canvas_resized(&self, surface_id: &str, node_id: &str, width: f32, height: f32) {
        let params = method::CanvasResizedParams {
            surface_id: surface_id.to_string(),
            node_id: node_id.to_string(),
            width,
            height,
        };
        self.notify(method::canvas::HOST_RESIZED, &params);
    }

    /// 告知插件某窗口因失焦被收起（`host:window.hidden`）。
    pub(crate) fn notify_window_hidden(&self, window_id: &str) {
        let params = method::WindowIdParams { id: window_id.to_string() };
        self.notify(method::window::HOST_HIDDEN, &params);
    }

    /// 告知插件某个面已挂载（`host:surface.attached`）。
    pub(crate) fn notify_surface_attached(&self, surface_id: &str, kind: SurfaceKind) {
        let params = SurfaceAttachedParams { surface_id: surface_id.to_string(), kind };
        self.notify(method::view::HOST_ATTACHED, &params);
    }

    /// 告知插件某个面已移除（`host:surface.detached`）。
    pub(crate) fn notify_surface_detached(&self, surface_id: &str) {
        let params = SurfaceDetachedParams { surface_id: surface_id.to_string() };
        self.notify(method::view::HOST_DETACHED, &params);
    }

    /// 广播一条宿主领域事件（`host:event`）。
    pub(crate) fn notify_domain_event(&self, name: &str, payload: &impl Serialize) {
        let payload = match serde_json::to_value(payload) {
            Ok(payload) => payload,
            Err(e) => {
                eprintln!("[plugin {}] 事件 {name} 载荷序列化失败：{e}", self.id);
                return;
            }
        };
        self.notify(event::HOST_PUBLISH, &event::PublishParams { name: name.to_string(), payload });
    }

    /// 告知插件一个文档实例已打开（`host:doc.open`）：surfaceId 为宿主分配的实例面标识。
    pub(crate) fn notify_doc_open(&self, surface_id: &str, path: &str) {
        let params = method::DocOpenParams {
            path: path.to_string(),
            surface_id: surface_id.to_string(),
        };
        self.notify(method::doc::HOST_OPEN, &params);
    }

    /// 告知插件一个文档实例已关闭（`host:doc.close`）。
    pub(crate) fn notify_doc_close(&self, surface_id: &str, path: &str) {
        let params = method::DocCloseParams {
            path: path.to_string(),
            surface_id: surface_id.to_string(),
        };
        self.notify(method::doc::HOST_CLOSE, &params);
    }

    /// 告知插件一个它打开的窗口已被用户关闭（`host:window.closed`）。
    pub(crate) fn notify_window_closed(&self, window_id: &str) {
        let params = method::WindowClosedParams { id: window_id.to_string() };
        self.notify(method::window::HOST_CLOSED, &params);
    }

    /// 向插件发一条宿主通知（`host:` 前缀方法）。进程未在跑时静默丢弃（对端已不在）。
    fn notify(&self, method_name: &str, params: &impl Serialize) {
        let params = match serde_json::to_value(params) {
            Ok(value) => value,
            Err(e) => {
                eprintln!("[plugin {}] 通知 {method_name} 序列化失败：{e}", self.id);
                return;
            }
        };
        self.write_envelope(Envelope::notification(method_name, params), "通知");
    }

    /// 取宿主运行时的强引用（弱引用失效 = 宿主已销毁）。
    fn runtime_upgrade(&self) -> Result<Arc<PluginRuntime>, RpcError> {
        self.runtime
            .lock()
            .unwrap()
            .upgrade()
            .ok_or_else(|| RpcError::failed("宿主运行时不可用"))
    }

    /// 下发一次 AI 工具调用（`host:ai.toolCall`）；插件须用同一 `callId` 经 `ai.toolResult` 回报。
    pub(crate) fn notify_tool_call(&self, call_id: &str, name: &str, arguments: Value) {
        let params = method::ToolCallParams {
            call_id: call_id.to_string(),
            name: name.to_string(),
            arguments,
        };
        self.notify(method::ai::HOST_TOOL_CALL, &params);
    }

    fn record_denied(&self, method: &str, params: &Value) {
        let detail = params
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        {
            let mut denied = self.denied.lock().unwrap();
            denied.push(AuditRecord { method: method.to_string(), detail: detail.clone(), at: now_secs() });
            if denied.len() > MAX_AUDIT {
                denied.remove(0);
            }
        }
        let _ = self.events.send(PluginEvent::PermissionDenied {
            id: self.id.clone(),
            method: method.to_string(),
            detail,
        });
    }

    /// 回执一条响应（写往插件 stdin）。
    fn respond(&self, id: RequestId, result: Option<Value>, error: Option<RpcError>) {
        let (envelope, what) = match (result, error) {
            (Some(result), None) => (Envelope::success(id, result), "回执"),
            (_, Some(error)) => (Envelope::failure(id, error), "错误回执"),
            (None, None) => (Envelope::success(id, Value::Null), "回执"),
        };
        self.write_envelope(envelope, what);
    }

    /// 写一条信封到插件 stdin。进程已停时静默丢弃（对端已不在）；写失败只留诊断输出。
    fn write_envelope(&self, envelope: Envelope, what: &str) {
        let pid = self.pid.load(Ordering::SeqCst) as u32;
        if pid == 0 {
            return;
        }
        let mut line = envelope.to_line();
        line.push('\n');
        if let Err(e) = self.process_host.write_stdin(pid, &line) {
            eprintln!("[plugin {}] {what}写入失败：{e}", self.id);
        }
    }
}

/// `process.spawn` 的 pid 槽位：pid 定下前到达的子进程事件先按序暂存，定下后补齐（见 `spawn_child`）。
#[derive(Default)]
struct SpawnSlot {
    pid: Option<u32>,
    pending: Vec<HostEvent>,
}

/// 按方法名解析对话框请求（`dialog.pickFile` / `dialog.pickDirectory`）。
fn dialog_request(method_name: &str, params: &Value) -> Result<DialogRequest, RpcError> {
    match method_name {
        method::dialog::PICK_FILE => {
            let p: method::PickFileParams = parse(params)?;
            Ok(DialogRequest {
                kind: DialogKind::File,
                title: p.title,
                save: p.save.unwrap_or(false),
                default_path: p.default_path,
                filters: p.filters,
            })
        }
        method::dialog::PICK_DIRECTORY => {
            let p: method::PickDirectoryParams = parse(params)?;
            Ok(DialogRequest {
                kind: DialogKind::Directory,
                title: p.title,
                save: false,
                default_path: None,
                filters: Vec::new(),
            })
        }
        _ => Err(RpcError::failed(format!("不是对话框方法：{method_name}"))),
    }
}

/// 对话框结果 → 应答（用户取消时 `path` 缺省，与 [`method::PickedPath`] 的序列化口径一致）。
fn encode_picked(path: Option<String>) -> Result<Value, RpcError> {
    serde_json::to_value(method::PickedPath { path })
        .map_err(|e| RpcError::failed(format!("应答序列化失败：{e}")))
}

/// 退避延迟：第 n 次崩溃取 `base * 2^(n-1)`，封顶 [`BACKOFF_MAX`]。
fn backoff_delay(count: usize) -> Duration {
    let exponent = count.saturating_sub(1).min(5) as u32;
    BACKOFF_BASE.saturating_mul(1 << exponent).min(BACKOFF_MAX)
}

/// 记录一次崩溃并返回窗口内累计次数（先剔除窗口外的旧记录）。
fn record_crash(times: &mut Vec<Instant>, now: Instant) -> usize {
    times.retain(|t| now.duration_since(*t) < CRASH_WINDOW);
    times.push(now);
    times.len()
}

/// 自备图标名的形态：字母 / 数字 / `_` / `-`，非空且不超过 64。
fn valid_icon_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// 树里是否存在指定 id 的 canvas 节点（树重发时清理孤儿清单的判据）。
fn has_canvas_node(tree: &ViewNode, node_id: &str) -> bool {
    tree.find(node_id).is_some_and(|node| matches!(node.body, view::NodeBody::Canvas { .. }))
}

/// 解析 params。缺省 params（信封里 `params` 为 null）按空对象处理：全可选入参的方法
/// （如 `dialog.pickDirectory`）本就允许不带参数，null 与 `{}` 是同一件事。
fn parse<T: DeserializeOwned>(params: &Value) -> Result<T, RpcError> {
    let value = if params.is_null() { json!({}) } else { params.clone() };
    serde_json::from_value(value).map_err(|e| RpcError::failed(format!("params 不可解析：{e}")))
}

/// 解码仓库二进制写入负载并判上限。超限不截断——截断写进仓库就是内容损坏。
fn decode_vault_binary(base64_text: &str, limit: usize) -> Result<Vec<u8>, String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(base64_text)
        .map_err(|e| format!("base64 解码失败：{e}"))?;
    if bytes.len() > limit {
        return Err(format!(
            "内容超过单次载荷上限（{} MiB），大文件请用 vault.importFile 按路径导入",
            limit / (1024 * 1024)
        ));
    }
    Ok(bytes)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::test_support::TempDir;
    use atelyx_plugin_protocol::manifest::{Entry, Permission};
    use atelyx_plugin_protocol::method::TakeoverScope;
    use atelyx_plugin_protocol::view::NodeBody;

    #[allow(unused_imports)]
    use super::super::HostRequest;

    /// 构造一个不落进程的实例（`entry` 留空：单元测试只驱动请求分派与状态，不启动进程）。
    fn make_instance(permissions: Vec<Permission>) -> (TempDir, Arc<Instance>) {
        let (tmp, instance, _rx) = make_instance_with_events(permissions);
        (tmp, instance)
    }

    /// 同 [`make_instance`]，但保留事件订阅端（验证通知确实投递到壳侧）。
    fn make_instance_with_events(
        permissions: Vec<Permission>,
    ) -> (
        TempDir,
        Arc<Instance>,
        tokio::sync::broadcast::Receiver<PluginEvent>,
    ) {
        let tmp = TempDir::new("plugin-instance");
        let dir = tmp.join("com.example.demo");
        std::fs::create_dir_all(&dir).unwrap();
        let manifest = Manifest {
            id: "com.example.demo".into(),
            name: "Demo".into(),
            version: "1.0.0".into(),
            api_version: API_VERSION,
            entry: Entry::default(),
            permissions,
            ..Manifest::default()
        };
        let (events, rx) = tokio::sync::broadcast::channel(16);
        let instance = Instance::new(
            manifest,
            dir,
            tmp.join("state"),
            env!("CARGO_PKG_VERSION").to_string(),
            Arc::new(PluginProcessHost::new()),
            // 无 OS 层的注册器：单测不落真实热键，注册路径以可读失败收场
            shortcut::Registrar::unavailable("全局快捷键不可用：测试"),
            events,
        );
        (tmp, instance, rx)
    }

    fn initialize_params(api_version: u32, id: &str) -> Value {
        json!({ "apiVersion": api_version, "plugin": { "id": id, "name": "Demo", "version": "1.0.0" } })
    }

    /// 造一行插件→宿主请求（走 `handle_line`，闸门与审计都在那条路径上）。
    fn request_line(method_name: &str, params: &Value) -> String {
        json!({ "jsonrpc": "2.0", "id": 1, "method": method_name, "params": params }).to_string()
    }

    #[test]
    fn subscribe_accepts_domain_event_names_and_rejects_unknown() {
        let (_tmp, instance) = make_instance(Vec::new());
        for name in [
            event::name::VAULT_SWITCH,
            event::name::TABLE_CHANGED,
            event::name::AI_CHAT_CHANGED,
            event::name::VAULT_FILE_CHANGED,
        ] {
            let ack = instance
                .dispatch(event::SUBSCRIBE, &json!({ "name": name }))
                .unwrap();
            assert_eq!(ack["subscribed"], json!(true));
            assert!(instance.subscribed_to(name));
        }
        assert!(instance
            .dispatch(event::SUBSCRIBE, &json!({ "name": "unknown:event" }))
            .is_err());
    }

    /// 通知必须真正投递；事件通道无订阅者时回可读错误，不把「塞进通道」当成已弹出。
    #[test]
    fn notification_show_reports_delivery() {
        let (_tmp, instance, mut rx) = make_instance_with_events(vec![Permission::Notification]);
        instance
            .dispatch(
                method::notification::SHOW,
                &json!({ "title": "完成", "body": "已生成" }),
            )
            .expect("有订阅者应投递成功");
        assert!(matches!(
            rx.try_recv(),
            Ok(PluginEvent::Notification { title, body, .. })
                if title == "完成" && body.as_deref() == Some("已生成")
        ));

        let (_tmp, instance) = make_instance(vec![Permission::Notification]);
        let err = instance
            .dispatch(
                method::notification::SHOW,
                &json!({ "title": "完成", "body": "已生成" }),
            )
            .expect_err("无订阅者不得回成功");
        assert!(err.to_string().contains("通知通道已关闭"), "错误应可读：{err}");
    }

    #[test]
    fn backoff_grows_then_caps() {
        assert_eq!(backoff_delay(1), Duration::from_secs(1));
        assert_eq!(backoff_delay(2), Duration::from_secs(2));
        assert_eq!(backoff_delay(3), Duration::from_secs(4));
        assert_eq!(backoff_delay(4), Duration::from_secs(8));
        assert_eq!(backoff_delay(60), BACKOFF_MAX);
    }

    #[test]
    fn crash_window_prunes_old_and_counts() {
        let mut times = Vec::new();
        let t0 = Instant::now();
        for _ in 0..CRASH_LIMIT {
            assert!(record_crash(&mut times, t0) <= CRASH_LIMIT);
        }
        assert_eq!(times.len(), CRASH_LIMIT, "窗口内累计达到熔断阈值");
        // 窗口外的旧记录被剔除：新一次崩溃只算 1
        let later = t0 + CRASH_WINDOW + Duration::from_secs(1);
        assert_eq!(record_crash(&mut times, later), 1);
    }

    #[test]
    fn vault_binary_decode_enforces_limit_without_truncating() {
        use base64::Engine;
        let ok = base64::engine::general_purpose::STANDARD.encode([0u8, 1, 2]);
        assert_eq!(decode_vault_binary(&ok, 3).unwrap(), vec![0, 1, 2]);
        let err = decode_vault_binary(&ok, 2).expect_err("超限应拒绝");
        assert!(err.contains("上限"), "{err}");
        assert!(decode_vault_binary("不是 base64", 3).is_err());
    }

    #[test]
    fn initialize_ok_reports_host_info() {
        let (_tmp, instance) = make_instance(Vec::new());
        let result = instance
            .dispatch(handshake::INITIALIZE, &initialize_params(API_VERSION, "com.example.demo"))
            .expect("版本一致应握手成功");
        assert_eq!(result["apiVersion"], API_VERSION);
        assert_eq!(result["host"]["name"], HOST_NAME);
    }

    #[test]
    fn initialize_mismatch_marks_failed() {
        let (_tmp, instance) = make_instance(Vec::new());
        assert!(instance
            .dispatch(handshake::INITIALIZE, &initialize_params(API_VERSION + 1, "com.example.demo"))
            .is_err());
        assert!(matches!(instance.status(), PluginStatus::Failed(_)));
    }

    #[test]
    fn initialize_id_mismatch_marks_failed() {
        let (_tmp, instance) = make_instance(Vec::new());
        assert!(instance
            .dispatch(handshake::INITIALIZE, &initialize_params(API_VERSION, "com.other.id"))
            .is_err());
        assert!(matches!(instance.status(), PluginStatus::Failed(_)));
    }

    #[test]
    fn undeclared_capability_is_denied_and_audited() {
        let (_tmp, instance) = make_instance(Vec::new());
        instance.handle_line(
            r#"{"jsonrpc":"2.0","id":1,"method":"fs.readText","params":{"path":"C:/x"}}"#,
        );
        assert_eq!(instance.denied_count(), 1, "未声明的能力调用必须被拒并留下记录");
        let audit = instance.audit();
        assert_eq!(audit[0].method, "fs.readText");
        assert_eq!(audit[0].detail, "C:/x");
    }

    #[test]
    fn undeclared_window_call_is_denied_and_audited() {
        let (_tmp, instance) = make_instance(Vec::new());
        instance.handle_line(&request_line(
            method::window::OPEN,
            &json!({ "id": "w", "title": "窗口" }),
        ));
        assert_eq!(instance.denied_count(), 1, "未声明 window 能力的开窗请求必须被拒并留记录");
        assert_eq!(instance.audit()[0].method, method::window::OPEN);
    }

    #[test]
    fn undeclared_ai_calls_are_denied_and_audited() {
        // ai 权限 fail-closed：send / stop / history 逐项未声明被拒并留审计
        let (_tmp, instance) = make_instance(Vec::new());
        for (name, params) in [
            (method::ai::CHAT_SEND, json!({ "text": "你好" })),
            (method::ai::CHAT_STOP, json!({ "sessionId": "s1" })),
            (method::ai::CHAT_HISTORY, json!({})),
        ] {
            instance.handle_line(&request_line(name, &params));
        }
        assert_eq!(
            instance.denied_count(),
            3,
            "未声明 ai 能力的三方法必须全部被拒并留记录"
        );
        for record in instance.audit() {
            assert!(record.method.starts_with("ai.chat."));
        }
    }

    #[test]
    fn takeover_and_window_requests_route_to_shell_and_resolve() {
        let tmp = TempDir::new("instance-ui-request");
        let runtime = PluginRuntime::new(tmp.to_path_buf());
        let (_t, instance) = make_instance(vec![Permission::Window]);
        instance.attach_runtime(&runtime);
        // 界面服务请求的目标校验（贡献注册）查运行时实例表：手工构造的实例须先登记
        runtime
            .instances
            .lock()
            .unwrap()
            .insert("com.example.demo".to_string(), instance.clone());
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        // 接管要求目标贡献已注册：先注册再请求
        instance
            .dispatch(method::view::REGISTER, &json!({ "slot": "panel", "id": "main", "label": "主面" }))
            .expect("贡献注册应成功");
        instance.handle_line(&request_line(
            method::view::TAKEOVER,
            &json!({ "scope": "full", "surfaceId": "main" }),
        ));
        let request = rt.block_on(runtime.recv_host_request()).unwrap();
        let HostRequest::Takeover { request_id, scope, surface_id, .. } = request else {
            panic!("应投递接管请求");
        };
        assert_eq!(scope, TakeoverScope::Full);
        assert_eq!(surface_id, "main");
        runtime.resolve_ui_call(request_id, Ok(()));

        instance.handle_line(&request_line(
            method::window::OPEN,
            &json!({ "id": "float", "title": "浮窗", "alwaysOnTop": true }),
        ));
        let request = rt.block_on(runtime.recv_host_request()).unwrap();
        let HostRequest::OpenWindow { request_id, params, .. } = request else {
            panic!("应投递开窗请求");
        };
        assert_eq!(params.id, "float");
        assert_eq!(params.always_on_top, Some(true));
        runtime.resolve_ui_call(request_id, Err("窗口数量已达上限".to_string()));

        instance.handle_line(&request_line(method::window::CLOSE, &json!({ "id": "float" })));
        let request = rt.block_on(runtime.recv_host_request()).unwrap();
        let HostRequest::CloseWindow { request_id, window_id, .. } = request else {
            panic!("应投递关窗请求");
        };
        assert_eq!(window_id, "float");
        runtime.resolve_ui_call(request_id, Ok(()));

        instance.handle_line(&request_line(
            method::window::UPDATE,
            &json!({ "id": "float", "height": 520.0, "alwaysOnTop": false }),
        ));
        let request = rt.block_on(runtime.recv_host_request()).unwrap();
        let HostRequest::WindowUpdate { request_id, params, .. } = request else {
            panic!("应投递窗口更新请求");
        };
        assert_eq!(params.height, Some(520.0));
        assert_eq!(params.always_on_top, Some(false));
        runtime.resolve_ui_call(request_id, Ok(()));

        instance.handle_line(&request_line(method::window::SHOW, &json!({ "id": "float" })));
        let request = rt.block_on(runtime.recv_host_request()).unwrap();
        let HostRequest::WindowShow { window_id, .. } = request else {
            panic!("应投递窗口显示请求");
        };
        assert_eq!(window_id, "float");
    }

    /// AI 对话三方法路由到壳并可回填（已声明 ai 权限；声明缺失走 fail-closed 被拒测试）。
    #[test]
    fn chat_requests_route_to_shell_and_resolve() {
        let tmp = TempDir::new("instance-chat-request");
        let runtime = PluginRuntime::new(tmp.to_path_buf());
        let (_t, instance) = make_instance(vec![Permission::Ai]);
        instance.attach_runtime(&runtime);
        runtime
            .instances
            .lock()
            .unwrap()
            .insert("com.example.demo".to_string(), instance.clone());
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        instance.handle_line(&request_line(method::ai::CHAT_SEND, &json!({ "text": "你好" })));
        let request = rt.block_on(runtime.recv_host_request()).unwrap();
        let HostRequest::ChatSend { request_id, params, .. } = request else {
            panic!("应投递对话发送请求");
        };
        assert_eq!(params.text, "你好");
        runtime.resolve_shell_call(request_id, Ok(json!({ "sessionId": "s1", "messageId": "m1" })));

        instance.handle_line(&request_line(method::ai::CHAT_STOP, &json!({ "sessionId": "s1" })));
        let request = rt.block_on(runtime.recv_host_request()).unwrap();
        let HostRequest::ChatStop { request_id, params, .. } = request else {
            panic!("应投递对话中断请求");
        };
        assert_eq!(params.session_id, "s1");
        runtime.resolve_shell_call(request_id, Ok(json!({})));

        instance.handle_line(&request_line(method::ai::CHAT_HISTORY, &json!({ "sessionId": "s1" })));
        let request = rt.block_on(runtime.recv_host_request()).unwrap();
        let HostRequest::ChatHistory { request_id, params, .. } = request else {
            panic!("应投递历史读取请求");
        };
        assert_eq!(params.session_id.as_deref(), Some("s1"));
        runtime.resolve_shell_call(request_id, Err("会话 s1 不存在".to_string()));
    }

    /// 壳侧服务请求路由到壳：request 落到宿主通道，结果经统一回填接口送回。
    #[test]
    fn shell_side_service_requests_route_and_resolve() {
        let tmp = TempDir::new("instance-shell-request");
        let runtime = PluginRuntime::new(tmp.to_path_buf());
        let (_t, instance) = make_instance(vec![
            Permission::Clipboard,
            Permission::Vault,
            Permission::Table,
            Permission::Ai,
            Permission::Tray,
            Permission::Collab,
        ]);
        instance.attach_runtime(&runtime);
        runtime
            .instances
            .lock()
            .unwrap()
            .insert("com.example.demo".to_string(), instance.clone());
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        instance.handle_line(&request_line(method::clipboard::READ_TEXT, &Value::Null));
        let request = rt.block_on(runtime.recv_host_request()).unwrap();
        let HostRequest::ClipboardRead { request_id, .. } = request else {
            panic!("应投递剪贴板读取请求");
        };
        runtime.resolve_shell_call(request_id, Ok(json!({ "text": "hi" })));

        instance.handle_line(&request_line(method::clipboard::WRITE_TEXT, &json!({ "text": "hi" })));
        let request = rt.block_on(runtime.recv_host_request()).unwrap();
        let HostRequest::ClipboardWrite { request_id, text, .. } = request else {
            panic!("应投递剪贴板写入请求");
        };
        assert_eq!(text, "hi");
        runtime.resolve_shell_call(request_id, Ok(json!({})));

        instance.handle_line(&request_line(method::vault::READ_TEXT, &json!({ "path": "a.md" })));
        let request = rt.block_on(runtime.recv_host_request()).unwrap();
        let HostRequest::VaultReadText { request_id, path, .. } = request else {
            panic!("应投递仓库读取请求");
        };
        assert_eq!(path, "a.md");
        runtime.resolve_shell_call(request_id, Ok(json!({ "content": "正文" })));

        instance.handle_line(&request_line(
            method::vault::WRITE_TEXT,
            &json!({ "path": "a.md", "content": "正文" }),
        ));
        let request = rt.block_on(runtime.recv_host_request()).unwrap();
        let HostRequest::VaultWriteText { request_id, path, content, .. } = request else {
            panic!("应投递仓库写入请求");
        };
        assert_eq!((path.as_str(), content.as_str()), ("a.md", "正文"));
        runtime.resolve_shell_call(request_id, Ok(json!({})));

        instance.handle_line(&request_line(method::vault::READ_BINARY, &json!({ "path": "a.png" })));
        let request = rt.block_on(runtime.recv_host_request()).unwrap();
        let HostRequest::VaultReadBinary { request_id, path, .. } = request else {
            panic!("应投递仓库二进制读取请求");
        };
        assert_eq!(path, "a.png");
        runtime.resolve_shell_call(request_id, Ok(crate::plugin_runtime::binary_payload(&[1, 2])));

        instance.handle_line(&request_line(
            method::vault::WRITE_BINARY,
            &json!({ "path": "a.png", "base64": "AAEC" }),
        ));
        let request = rt.block_on(runtime.recv_host_request()).unwrap();
        let HostRequest::VaultWriteBinary { request_id, path, bytes, .. } = request else {
            panic!("应投递仓库二进制写入请求");
        };
        assert_eq!(path, "a.png");
        assert_eq!(bytes, vec![0, 1, 2]);
        runtime.resolve_shell_call(request_id, Ok(json!({})));

        instance.handle_line(&request_line(method::clipboard::READ_IMAGE, &Value::Null));
        let request = rt.block_on(runtime.recv_host_request()).unwrap();
        let HostRequest::ClipboardReadImage { request_id, .. } = request else {
            panic!("应投递剪贴板读图请求");
        };
        runtime.resolve_shell_call(request_id, Ok(json!({})));

        instance.handle_line(&request_line(method::vault::LIST, &json!({ "path": "" })));
        let request = rt.block_on(runtime.recv_host_request()).unwrap();
        let HostRequest::VaultList { request_id, path, .. } = request else {
            panic!("应投递目录列举请求");
        };
        assert_eq!(path, "");
        runtime.resolve_shell_call(request_id, Ok(json!({ "entries": [] })));

        instance.handle_line(&request_line(method::vault::EXISTS, &json!({ "path": "a.md" })));
        let request = rt.block_on(runtime.recv_host_request()).unwrap();
        let HostRequest::VaultExists { request_id, path, .. } = request else {
            panic!("应投递存在性请求");
        };
        assert_eq!(path, "a.md");
        runtime.resolve_shell_call(request_id, Ok(json!({ "exists": true })));

        instance.handle_line(&request_line(method::table::SNAPSHOT, &Value::Null));
        let request = rt.block_on(runtime.recv_host_request()).unwrap();
        let HostRequest::TableSnapshot { request_id, .. } = request else {
            panic!("应投递表格快照请求");
        };
        runtime.resolve_shell_call(request_id, Ok(json!({ "open": false })));

        instance.handle_line(&request_line(
            method::table::UPDATE_CELL,
            &json!({ "rowId": "r1", "fieldId": "f1", "value": "甲" }),
        ));
        let request = rt.block_on(runtime.recv_host_request()).unwrap();
        let HostRequest::TableUpdateCell { request_id, row_id, field_id, value, .. } = request else {
            panic!("应投递逐格写请求");
        };
        assert_eq!((row_id.as_str(), field_id.as_str()), ("r1", "f1"));
        assert_eq!(value, json!("甲"));
        runtime.resolve_shell_call(request_id, Ok(json!({})));

        instance.handle_line(&request_line(method::table::ADD_ROW, &Value::Null));
        let request = rt.block_on(runtime.recv_host_request()).unwrap();
        let HostRequest::TableAddRow { request_id, .. } = request else {
            panic!("应投递新增行请求");
        };
        runtime.resolve_shell_call(request_id, Ok(json!({ "rowId": "r9" })));

        instance.handle_line(&request_line(method::table::REMOVE_ROW, &json!({ "rowId": "r1" })));
        let request = rt.block_on(runtime.recv_host_request()).unwrap();
        let HostRequest::TableRemoveRow { request_id, row_id, .. } = request else {
            panic!("应投递删行请求");
        };
        assert_eq!(row_id, "r1");
        runtime.resolve_shell_call(request_id, Ok(json!({})));

        instance.handle_line(&request_line(method::table::SELECT_ROW, &json!({ "rowId": "r1" })));
        let request = rt.block_on(runtime.recv_host_request()).unwrap();
        let HostRequest::TableSelectRow { request_id, row_id, .. } = request else {
            panic!("应投递选行请求");
        };
        assert_eq!(row_id, "r1");
        runtime.resolve_shell_call(request_id, Ok(json!({})));

        instance.handle_line(&request_line(
            method::table::SET_CELL_IMAGE,
            &json!({ "rowId": "r1", "fieldId": "f2", "images": ["assets/a.png"], "display": "grid" }),
        ));
        let request = rt.block_on(runtime.recv_host_request()).unwrap();
        let HostRequest::TableSetCellImage { request_id, params, .. } = request else {
            panic!("应投递图片单元格写入请求");
        };
        assert_eq!(params.images, vec!["assets/a.png"]);
        assert_eq!(params.display.as_deref(), Some("grid"));
        runtime.resolve_shell_call(request_id, Ok(json!({})));

        instance.handle_line(&request_line(
            method::view::SCROLL_TO,
            &json!({ "surfaceId": "panel", "nodeId": "log", "edge": "end" }),
        ));
        let request = rt.block_on(runtime.recv_host_request()).unwrap();
        let HostRequest::ScrollTo { request_id, params, .. } = request else {
            panic!("应投递滚动指令");
        };
        assert_eq!(params.edge, Some(method::ScrollEdge::End));
        runtime.resolve_shell_call(request_id, Ok(json!({})));

        instance.handle_line(&request_line(
            method::view::SCROLL_TO,
            &json!({ "surfaceId": "panel", "nodeId": "log", "offset": 0.25 }),
        ));
        let request = rt.block_on(runtime.recv_host_request()).unwrap();
        let HostRequest::ScrollTo { request_id, params, .. } = request else {
            panic!("应投递滚动指令");
        };
        assert_eq!((params.edge, params.offset), (None, Some(0.25)));
        runtime.resolve_shell_call(request_id, Ok(json!({})));

        instance.handle_line(&request_line(method::vault::IMAGE_INFO, &json!({ "path": "a.png" })));
        let request = rt.block_on(runtime.recv_host_request()).unwrap();
        let HostRequest::VaultImageInfo { request_id, path, .. } = request else {
            panic!("应投递图片尺寸请求");
        };
        assert_eq!(path, "a.png");
        runtime.resolve_shell_call(request_id, Ok(json!({ "width": 10, "height": 20 })));

        instance.handle_line(&request_line(
            method::tray::SET_ITEMS,
            &json!({ "items": [{ "id": "show", "label": "显示" }] }),
        ));
        let request = rt.block_on(runtime.recv_host_request()).unwrap();
        let HostRequest::TraySetItems { request_id, params, .. } = request else {
            panic!("应投递托盘条目请求");
        };
        assert_eq!(params.items[0].id, "show");
        runtime.resolve_shell_call(request_id, Ok(json!({})));

        instance.handle_line(&request_line(method::collab::PEERS, &Value::Null));
        let request = rt.block_on(runtime.recv_host_request()).unwrap();
        let HostRequest::CollabPeers { request_id, .. } = request else {
            panic!("应投递协作在场清单请求");
        };
        runtime.resolve_shell_call(request_id, Ok(json!({ "connected": false })));

        instance.handle_line(&request_line(
            method::collab::SEND,
            &json!({ "channel": "chat", "payload": { "n": 1 }, "targetPeerId": "2" }),
        ));
        let request = rt.block_on(runtime.recv_host_request()).unwrap();
        let HostRequest::CollabSend { request_id, params, .. } = request else {
            panic!("应投递协作消息请求");
        };
        assert_eq!(params.channel, "chat");
        assert_eq!(params.target_peer_id.as_deref(), Some("2"));
        runtime.resolve_shell_call(request_id, Ok(json!({ "queued": true })));

        instance.handle_line(&request_line(method::ai::CHAT_RENAME, &json!({ "sessionId": "s1", "title": "周报" })));
        let request = rt.block_on(runtime.recv_host_request()).unwrap();
        let HostRequest::ChatRename { request_id, params, .. } = request else {
            panic!("应投递重命名请求");
        };
        assert_eq!(params.title, "周报");
        runtime.resolve_shell_call(request_id, Ok(json!({})));

        instance.handle_line(&request_line(method::ai::CHAT_DELETE, &json!({ "sessionId": "s1" })));
        let request = rt.block_on(runtime.recv_host_request()).unwrap();
        let HostRequest::ChatDelete { request_id, .. } = request else {
            panic!("应投递删除请求");
        };
        runtime.resolve_shell_call(request_id, Ok(json!({})));

        instance.handle_line(&request_line(method::ai::CHAT_COMPACT, &json!({ "sessionId": "s1" })));
        let request = rt.block_on(runtime.recv_host_request()).unwrap();
        let HostRequest::ChatCompact { request_id, .. } = request else {
            panic!("应投递压缩请求");
        };
        runtime.resolve_shell_call(request_id, Ok(json!({})));
    }

    #[test]
    fn shell_routed_capabilities_reject_synchronous_dispatch() {
        let (_tmp, instance) = make_instance(vec![Permission::Clipboard]);
        let error = instance
            .dispatch(method::clipboard::READ_TEXT, &Value::Null)
            .expect_err("剪贴板调用必须走异步应答路径");
        assert_eq!(error.code, RpcError::FAILED);
        assert!(error.message.contains("异步应答"), "{}", error.message);
    }

    /// 各能力面「未声明被拒」：闸门只认 manifest 声明，未声明一律 fail-closed 且记审计。
    #[test]
    fn undeclared_calls_of_each_face_are_denied() {
        let cases: [(Permission, &str, Value); 9] = [
            // 入参一律取「必然执行失败但不会真的落盘 / 出网 / 弹框」的形态
            (Permission::Process, method::process::SPAWN, json!({ "program": "atelyx-no-such-program" })),
            (Permission::Fs, method::fs::LIST, json!({ "path": "atelyx-no-such-dir" })),
            (Permission::Shortcuts, method::shortcut::REGISTER, json!({ "accelerator": "Ctrl+F9" })),
            (Permission::Shortcuts, method::shortcut::QUERY, json!({ "accelerator": "Ctrl+F9" })),
            (Permission::Http, method::http::REQUEST, json!({ "method": "GET", "url": "ftp://example.com" })),
            (Permission::Dialog, method::dialog::PICK_DIRECTORY, Value::Null),
            (Permission::Table, method::table::SNAPSHOT, Value::Null),
            (Permission::Table, method::table::SET_CELL_IMAGE, json!({ "rowId": "r1", "fieldId": "f1", "images": [] })),
            (Permission::Ai, method::ai::CHAT_RENAME, json!({ "sessionId": "s1", "title": "T" })),
        ];
        for (permission, method_name, params) in cases {
            // 未声明：拒绝并留痕（响应写往不存在的插件进程，观测点在审计记录）
            let (_tmp, undeclared) = make_instance(Vec::new());
            undeclared.handle_line(&request_line(method_name, &params));
            assert_eq!(undeclared.denied_count(), 1, "{method_name} 未声明必须被拒并留痕");
            // 已声明：进入真实执行路径（不再被闸门挡下）
            let (_tmp2, declared) = make_instance(vec![permission]);
            let error = declared.dispatch(method_name, &params).expect_err("入参不成立，应报执行错误");
            assert_ne!(
                error.code,
                RpcError::PERMISSION_DENIED,
                "{method_name} 已声明却仍被闸门拒绝"
            );
        }
    }

    #[test]
    fn fs_roundtrip_through_dispatch() {
        let (tmp, instance) = make_instance(vec![Permission::Fs]);
        let file = tmp.join("note.txt").to_string_lossy().into_owned();
        instance
            .dispatch(method::fs::WRITE_TEXT, &json!({ "path": file, "content": "hello" }))
            .expect("写文件应成功");
        let read = instance
            .dispatch(method::fs::READ_TEXT, &json!({ "path": file }))
            .expect("读文件应成功");
        assert_eq!(read["content"], "hello");
        let exists = instance
            .dispatch(method::fs::EXISTS, &json!({ "path": file }))
            .expect("存在性查询应成功");
        assert_eq!(exists["exists"], true);
        instance
            .dispatch(method::fs::WRITE_BINARY, &json!({ "path": file, "base64": "AAEC" }))
            .expect("写二进制应成功");
        let binary = instance
            .dispatch(method::fs::READ_BINARY, &json!({ "path": file }))
            .expect("读二进制应成功");
        assert_eq!(binary["base64"], "AAEC");
        instance.dispatch(method::fs::REMOVE, &json!({ "path": file })).expect("删除应成功");
        assert!(!std::path::Path::new(&file).exists());
        // privateDir 返回插件私有目录且已建立
        let private = instance.dispatch(method::fs::PRIVATE_DIR, &Value::Null).unwrap();
        assert!(std::path::Path::new(private["path"].as_str().unwrap()).is_dir());
    }

    #[test]
    fn process_child_calls_are_confined_to_own_bookkeeping() {
        let (_tmp, instance) = make_instance(vec![Permission::Process]);
        for (method_name, params) in [
            (method::process::KILL, json!({ "pid": 4242 })),
            (method::process::WRITE_STDIN, json!({ "pid": 4242, "data": "x" })),
            (method::process::CLOSE_STDIN, json!({ "pid": 4242 })),
        ] {
            let error = instance.dispatch(method_name, &params).expect_err("非本插件的进程不得操作");
            assert!(error.message.contains("不是本插件启动的"), "{method_name}: {}", error.message);
        }
    }

    #[test]
    fn shortcut_registration_reports_platform_unavailability() {
        let (_tmp, instance) = make_instance(vec![Permission::Shortcuts]);
        let error = instance
            .dispatch(method::shortcut::REGISTER, &json!({ "accelerator": "Ctrl+F9" }))
            .expect_err("无 OS 层时注册必须可读失败");
        assert!(error.message.contains("不可用"), "{}", error.message);
        // 注销是幂等空操作（本就没注册成）
        assert!(instance
            .dispatch(method::shortcut::UNREGISTER, &json!({ "accelerator": "Ctrl+F9" }))
            .is_ok());
    }

    /// 会话闸门：令牌换代（停用 / 崩溃收尾）之后到达的资源获取请求一律被拒——否则在停用收尾跑完
    /// 之后才落表的子进程 / 全局快捷键会逃过本次收尾（「插件已停、本机服务仍在跑 / 热键仍被占用」）。
    #[test]
    fn stale_session_resource_acquisition_is_refused() {
        let (_tmp, instance) = make_instance(vec![Permission::Process, Permission::Shortcuts]);
        let stale = instance.run_token.load(Ordering::SeqCst) + 1;
        for (method_name, params) in [
            (method::process::SPAWN, json!({ "program": "atelyx-no-such-program" })),
            (method::shortcut::REGISTER, json!({ "accelerator": "Ctrl+F9" })),
        ] {
            let error = instance
                .dispatch_for_session(stale, method_name, &params)
                .expect_err("旧会话的资源获取应被拒");
            assert!(error.message.contains("会话已结束"), "{method_name}: {}", error.message);
        }
        // 当前会话照常放行到实现层（此处的注册器没有 OS 层，故以「不可用」收场）
        let current = instance.run_token.load(Ordering::SeqCst);
        let error = instance
            .dispatch_for_session(current, method::shortcut::REGISTER, &json!({ "accelerator": "Ctrl+F9" }))
            .expect_err("无 OS 层时注册应可读失败");
        assert!(error.message.contains("不可用"), "{}", error.message);
    }

    #[test]
    fn dialog_request_maps_params() {
        // 缺省 params（params 为 null）按空对象处理：全可选入参的对话框方法本就允许不带参数
        let dir = dialog_request(method::dialog::PICK_DIRECTORY, &Value::Null).expect("缺省 params 应可解析");
        assert_eq!(dir.kind, DialogKind::Directory);
        assert!(!dir.save);

        let file = dialog_request(
            method::dialog::PICK_FILE,
            &json!({ "title": "选模型", "save": true, "filters": [{ "name": "模型", "extensions": ["gguf"] }] }),
        )
        .expect("pickFile 应可解析");
        assert_eq!(file.kind, DialogKind::File);
        assert!(file.save);
        assert_eq!(file.title.as_deref(), Some("选模型"));
        assert_eq!(file.filters[0].extensions, vec!["gguf"]);

        assert!(dialog_request(method::fs::READ_TEXT, &Value::Null).is_err(), "非对话框方法应被拒");
    }

    #[test]
    fn unknown_method_fails() {
        let (_tmp, instance) = make_instance(Vec::new());
        assert!(instance.dispatch("nope.method", &Value::Null).is_err());
    }

    /// 自备图标定义：名字 / 路径校验通过即登记，随会话收尾清除。
    #[test]
    fn define_icon_validates_and_clears_on_teardown() {
        let (_tmp, instance) = make_instance(Vec::new());
        instance
            .dispatch(method::view::DEFINE_ICON, &json!({ "name": "model-chip", "path": "M4 4h16v16H4z" }))
            .expect("合法自备图标应被登记");
        assert_eq!(instance.icons().get("model-chip").map(String::as_str), Some("M4 4h16v16H4z"));
        for bad in [
            json!({ "name": "", "path": "M4 4" }),
            json!({ "name": "has space", "path": "M4 4" }),
            json!({ "name": "ok", "path": "   " }),
        ] {
            assert!(instance.dispatch(method::view::DEFINE_ICON, &bad).is_err(), "应拒绝：{bad}");
        }
        instance.stop();
        assert!(instance.icons().is_empty(), "收尾后自备图标应清空");
    }

    /// 快捷键占用查询：无 OS 层时以可读失败收场（与注册同一原因），非法加速键亦拒。
    #[test]
    fn shortcut_query_reports_platform_unavailability() {
        let (_tmp, instance) = make_instance(vec![Permission::Shortcuts]);
        let error = instance
            .dispatch(method::shortcut::QUERY, &json!({ "accelerator": "Ctrl+F9" }))
            .expect_err("无 OS 层时查询应可读失败");
        assert!(error.message.contains("不可用"), "{}", error.message);
        assert!(
            instance.dispatch(method::shortcut::QUERY, &json!({ "accelerator": "不认识的键" })).is_err(),
            "非法加速键应被拒"
        );
    }

    /// 运行时时序方法（界面通道）不占权限：未声明任何能力的插件也能定义图标 / 发滚动指令。
    #[test]
    fn interface_channel_methods_need_no_permission() {
        let (_tmp, instance) = make_instance(Vec::new());
        instance
            .dispatch(method::view::DEFINE_ICON, &json!({ "name": "chip", "path": "M4 4h4v4H4z" }))
            .expect("定义图标不占权限声明");
        assert_eq!(instance.denied_count(), 0);
    }

    #[test]
    fn state_roundtrip_through_dispatch() {
        let (_tmp, instance) = make_instance(vec![Permission::State]);
        instance
            .dispatch(method::state::WRITE, &json!({ "json": { "n": 1 } }))
            .expect("写 state 应成功");
        let read = instance.dispatch(method::state::READ, &Value::Null).expect("读 state 应成功");
        assert_eq!(read["json"]["n"], 1);
    }

    /// table:changed 订阅注册，且运行时缓存最近广播的表格路径（迟到订阅补发的数据源）。
    #[test]
    fn table_changed_subscription_tracks_current_table() {
        let tmp = TempDir::new("instance-table-sub");
        let runtime = PluginRuntime::new(tmp.to_path_buf());
        let (_t, instance) = make_instance(Vec::new());
        instance.attach_runtime(&runtime);
        runtime
            .instances
            .lock()
            .unwrap()
            .insert("com.example.demo".to_string(), instance.clone());

        assert!(instance
            .dispatch(event::SUBSCRIBE, &json!({ "name": event::name::TABLE_CHANGED }))
            .is_ok());
        assert!(instance.subscribed_to(event::name::TABLE_CHANGED));

        runtime.on_table_changed(Some("a.atb"));
        assert_eq!(runtime.current_table_path().as_deref(), Some("a.atb"));
        // 关闭表格 = 缓存清空，迟到订阅者不再补发旧路径
        runtime.on_table_changed(None);
        assert_eq!(runtime.current_table_path(), None);
    }

    #[test]
    fn stale_process_events_do_not_count_as_crash() {
        let (_tmp, instance) = make_instance(Vec::new());
        instance.set_status(PluginStatus::Running);
        // 令牌不匹配（上次启动遗留）的退出上报必须被丢弃：不判崩溃、不改状态
        let stale = instance.run_token.load(Ordering::SeqCst) + 1;
        instance.handle_process_event(stale, HostEvent::Terminated { code: Some(1) });
        assert_eq!(instance.status(), PluginStatus::Running);
        // 令牌匹配时按崩溃处理：进入退避重启
        let current = instance.run_token.load(Ordering::SeqCst);
        instance.handle_process_event(current, HostEvent::Terminated { code: Some(1) });
        assert_eq!(instance.status(), PluginStatus::Restarting);
    }

    #[test]
    fn handshake_deadline_marks_silent_plugin_failed() {
        let (_tmp, instance) = make_instance(Vec::new());
        instance.set_status(PluginStatus::Running);
        instance.check_handshake(instance.run_token.load(Ordering::SeqCst));
        assert!(matches!(instance.status(), PluginStatus::Failed(_)));

        // 已握手或已不在运行态时不判失败
        let (_tmp2, instance) = make_instance(Vec::new());
        instance.set_status(PluginStatus::Running);
        instance.handshaken.store(true, Ordering::SeqCst);
        instance.check_handshake(instance.run_token.load(Ordering::SeqCst));
        assert_eq!(instance.status(), PluginStatus::Running);
    }

    #[test]
    fn view_submit_validates_tree_and_requires_surface() {
        let (_tmp, instance) = make_instance(Vec::new());
        let tree = json!({ "id": "root", "kind": "text", "content": "x" });
        instance
            .dispatch(method::view::SUBMIT, &json!({ "surfaceId": "main", "tree": tree }))
            .expect("合法界面树应被接收");
        assert_eq!(instance.view_submits(), 1);
        assert!(
            instance
                .dispatch(method::view::SUBMIT, &json!({ "surfaceId": "", "tree": tree }))
                .is_err(),
            "surfaceId 为空应被拒"
        );
    }

    /// 界面树按面存档（壳据此渲染最新树）；重复提交覆盖同一面。
    #[test]
    fn submitted_trees_are_stored_per_surface() {
        let (_tmp, instance) = make_instance(Vec::new());
        let tree = |content: &str| json!({ "id": "root", "kind": "text", "content": content });
        instance
            .dispatch(method::view::SUBMIT, &json!({ "surfaceId": "panel", "tree": tree("一") }))
            .unwrap();
        instance
            .dispatch(method::view::SUBMIT, &json!({ "surfaceId": "other", "tree": tree("二") }))
            .unwrap();
        instance
            .dispatch(method::view::SUBMIT, &json!({ "surfaceId": "panel", "tree": tree("三") }))
            .unwrap();
        let stored = |id: &str| match instance.surface_tree(id) {
            Some(ViewNode { body: NodeBody::Text { content, .. }, .. }) => content,
            other => panic!("面 {id} 的树应存在且为文本节点，实际：{other:?}"),
        };
        assert_eq!(stored("panel"), "三", "重复提交应覆盖同一面的旧树");
        assert_eq!(stored("other"), "二");
        assert!(instance.surface_tree("missing").is_none());
    }

    /// 贡献注册：登记成功、重复 id 与非法 id 被拒、撤销后消失。
    #[test]
    fn contributions_register_and_unregister() {
        let (_tmp, instance) = make_instance(Vec::new());
        let panel = json!({ "slot": "panel", "id": "models", "label": "模型", "icon": "box" });
        instance.dispatch(method::view::REGISTER, &panel).expect("面板注册应成功");
        instance
            .dispatch(method::view::REGISTER, &json!({ "slot": "tableView", "id": "timeline", "label": "时间线" }))
            .expect("表格视图注册应成功");
        let listed = instance.contributions();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, "models");
        assert_eq!(listed[0].slot, ContributionSlot::Panel);
        assert_eq!(listed[0].icon.as_deref(), Some("box"));
        assert_eq!(listed[0].plugin_id, "com.example.demo");

        // 重复 id 与非法的空 id / 含 `:` 的 id 一律可读拒绝
        for bad in [
            panel.clone(),
            json!({ "slot": "panel", "id": "  ", "label": "空" }),
            json!({ "slot": "panel", "id": "a:b", "label": "分隔符" }),
            json!({ "slot": "panel", "id": "blank", "label": " " }),
        ] {
            assert!(instance.dispatch(method::view::REGISTER, &bad).is_err(), "应拒绝：{bad}");
        }

        instance
            .dispatch(method::view::UNREGISTER, &json!({ "id": "models" }))
            .expect("撤销应成功");
        assert_eq!(instance.contributions().len(), 1);
        assert!(
            instance.dispatch(method::view::UNREGISTER, &json!({ "id": "models" })).is_err(),
            "撤销未注册的 id 应报可读错误"
        );
    }

    /// 会话收尾清空界面树与贡献注册：停用 / 崩溃 / 卸载后壳不该再看到已停插件的界面。
    #[test]
    fn teardown_drops_surfaces_and_contributions() {
        let (_tmp, instance) = make_instance(Vec::new());
        instance
            .dispatch(method::view::REGISTER, &json!({ "slot": "panel", "id": "main", "label": "面板" }))
            .unwrap();
        instance
            .dispatch(
                method::view::SUBMIT,
                &json!({ "surfaceId": "main", "tree": json!({ "id": "root", "kind": "text", "content": "x" }) }),
            )
            .unwrap();
        instance.stop();
        assert!(instance.surface_tree("main").is_none(), "收尾后界面树应清空");
        assert!(instance.contributions().is_empty(), "收尾后贡献注册应清空");
    }

    /// 会话闸门同样覆盖界面登记：令牌换代后到达的提交 / 注册一律作废（不得让已停插件的界面存活）。
    #[test]
    fn stale_session_view_registration_is_refused() {
        let (_tmp, instance) = make_instance(Vec::new());
        let stale = instance.run_token.load(Ordering::SeqCst) + 1;
        let tree = json!({ "id": "root", "kind": "text", "content": "x" });
        for (method_name, params) in [
            (method::view::SUBMIT, json!({ "surfaceId": "main", "tree": tree })),
            (method::view::REGISTER, json!({ "slot": "panel", "id": "main", "label": "面板" })),
        ] {
            let error = instance
                .dispatch_for_session(stale, method_name, &params)
                .expect_err("旧会话的界面登记应被拒");
            assert!(error.message.contains("会话已结束"), "{method_name}: {}", error.message);
        }
        assert!(instance.surface_tree("main").is_none());
        assert!(instance.contributions().is_empty());
    }

    /// 画布清单提交：面须已有树、节点须在树里且为 canvas、指令逐条校验；存档按 (面, 节点) 键合帧覆盖。
    #[test]
    fn canvas_submit_requires_canvas_node_in_tree() {
        let (_tmp, instance) = make_instance(Vec::new());
        let canvas_tree = || {
            json!({ "id": "root", "kind": "column", "children": [
                { "id": "board", "kind": "canvas", "mouseEvents": ["pointerDown"] },
                { "id": "hint", "kind": "text", "content": "x" }
            ]})
        };
        let commands = json!([
            { "kind": "line", "x1": 0.0, "y1": 0.0, "x2": 5.0, "y2": 5.0, "stroke": { "type": "rgba", "rgba": [0, 0, 0, 255] } }
        ]);
        let submit = |node_id: &str| {
            json!({ "surfaceId": "main", "nodeId": node_id, "commands": commands })
        };
        assert!(
            instance.dispatch(method::canvas::SUBMIT, &submit("board")).is_err(),
            "面没有界面树时应被拒"
        );
        instance
            .dispatch(method::view::SUBMIT, &json!({ "surfaceId": "main", "tree": canvas_tree() }))
            .unwrap();
        assert!(
            instance.dispatch(method::canvas::SUBMIT, &submit("missing")).is_err(),
            "节点不在树里应被拒"
        );
        assert!(
            instance.dispatch(method::canvas::SUBMIT, &submit("hint")).is_err(),
            "非 canvas 节点应被拒"
        );
        instance.dispatch(method::canvas::SUBMIT, &submit("board")).expect("合法提交应被接收");
        assert_eq!(instance.surface_canvas("main", "board").unwrap().len(), 1);
        assert!(instance.surface_canvas("main", "hint").is_none());

        let bad = json!({ "surfaceId": "main", "nodeId": "board", "commands": [
            { "kind": "circle", "cx": 0.0, "cy": 0.0, "r": 1.0, "fill": null, "stroke": null }
        ]});
        assert!(
            instance.dispatch(method::canvas::SUBMIT, &bad).is_err(),
            "无填充也无描边的指令应被拒"
        );
        assert_eq!(instance.surface_canvas("main", "board").unwrap().len(), 1, "被拒的提交不得改写存档");
    }

    /// 树重发清掉已消失节点的清单；重复提交覆盖同一节点；空清单 = 清空画布；收尾清全部。
    #[test]
    fn canvas_lists_follow_the_tree_lifecycle() {
        let (_tmp, instance) = make_instance(Vec::new());
        let tree = |with_second: bool| {
            let mut children = vec![json!({ "id": "board", "kind": "canvas" })];
            if with_second {
                children.push(json!({ "id": "second", "kind": "canvas" }));
            }
            json!({ "id": "root", "kind": "column", "children": children })
        };
        let submit_canvas = |node_id: &str, count: usize| {
            let commands: Vec<Value> = (0..count)
                .map(|_| json!({ "kind": "circle", "cx": 1.0, "cy": 1.0, "r": 1.0, "fill": { "type": "rgba", "rgba": [1, 2, 3, 4] } }))
                .collect();
            json!({ "surfaceId": "main", "nodeId": node_id, "commands": commands })
        };
        instance
            .dispatch(method::view::SUBMIT, &json!({ "surfaceId": "main", "tree": tree(true) }))
            .unwrap();
        instance.dispatch(method::canvas::SUBMIT, &submit_canvas("board", 1)).unwrap();
        instance.dispatch(method::canvas::SUBMIT, &submit_canvas("second", 2)).unwrap();
        instance.dispatch(method::canvas::SUBMIT, &submit_canvas("board", 3)).unwrap();
        assert_eq!(instance.surface_canvas("main", "board").unwrap().len(), 3, "同节点重复提交应覆盖");
        assert_eq!(instance.surface_canvas("main", "second").unwrap().len(), 2);

        // 树重发后 second 不在树里：清单随树清除；空清单 = 清空 board（存档仍在）
        instance
            .dispatch(method::view::SUBMIT, &json!({ "surfaceId": "main", "tree": tree(false) }))
            .unwrap();
        assert!(instance.surface_canvas("main", "second").is_none(), "孤儿清单应随树清除");
        instance.dispatch(method::canvas::SUBMIT, &submit_canvas("board", 0)).unwrap();
        let cleared = instance.surface_canvas("main", "board").unwrap();
        assert_eq!(cleared.len(), 0, "空清单 = 清空画布");

        instance.stop();
        assert!(instance.surface_canvas("main", "board").is_none(), "收尾后清单应清空");
    }

    /// 等在途工具调用落表（发起任务被调度后才登记）。
    async fn wait_call_id(runtime: &PluginRuntime) -> String {
        for _ in 0..100 {
            if let Some(id) = runtime.tool_calls.lock().unwrap().keys().next() {
                return id.clone();
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("工具调用未在预期时间内登记");
    }

    #[tokio::test]
    async fn ai_tool_registration_and_call_roundtrip() {
        let tmp = TempDir::new("instance-ai-tool");
        let runtime = PluginRuntime::new(tmp.to_path_buf());
        let (_t, instance) = make_instance(vec![Permission::Ai]);
        instance.attach_runtime(&runtime);
        instance.set_status(PluginStatus::Running);
        instance.session_active.store(true, Ordering::SeqCst);
        runtime
            .instances
            .lock()
            .unwrap()
            .insert(instance.id.clone(), instance.clone());

        instance
            .dispatch(
                method::ai::REGISTER_TOOL,
                &json!({
                    "name": "find_notes",
                    "description": "按关键词搜索仓库笔记",
                    "parameters": { "type": "object", "properties": { "q": { "type": "string" } } }
                }),
            )
            .expect("注册应成功");
        assert_eq!(runtime.plugin_tools().len(), 1);

        let call = tokio::spawn({
            let runtime = runtime.clone();
            async move { runtime.call_tool("find_notes", json!({ "q": "周报" })).await }
        });
        let call_id = wait_call_id(&runtime).await;
        assert!(
            runtime
                .resolve_tool_result("com.example.other", &call_id, "冒领".into(), false)
                .is_err(),
            "他插件的回报不得认领本插件的调用"
        );
        instance
            .dispatch(
                method::ai::TOOL_RESULT,
                &json!({ "callId": call_id, "content": "命中 2 条", "isError": false }),
            )
            .expect("结果回报应成功");
        let output = call.await.unwrap().expect("调用应拿到插件回传");
        assert_eq!(output.content, "命中 2 条");
        assert!(!output.is_error);

        // 已收场的 callId：迟到回报不得回成功，插件才能区分「已进模型上下文」与「被丢弃」
        let late = instance
            .dispatch(
                method::ai::TOOL_RESULT,
                &json!({ "callId": "tool-missing", "content": "迟到" }),
            )
            .expect_err("已收场的 callId 不得回成功");
        assert!(late.to_string().contains("已收场"), "错误应可读：{late}");

        // 停用：工具随会话摘除，在途调用以可读失败收场
        let pending = tokio::spawn({
            let runtime = runtime.clone();
            async move { runtime.call_tool("find_notes", json!({})).await }
        });
        let _ = wait_call_id(&runtime).await;
        instance.stop();
        assert!(runtime.plugin_tools().is_empty(), "停用后工具必须摘除");
        assert_eq!(pending.await.unwrap().unwrap_err(), "插件已停止，工具调用已取消");
    }

    /// 工具调用登记在资源闸门内复查会话活跃：会话已失效时立即可读失败，不落表等满调用超时。
    #[tokio::test]
    async fn tool_call_is_refused_once_session_is_inactive() {
        let tmp = TempDir::new("instance-ai-tool-session");
        let runtime = PluginRuntime::new(tmp.to_path_buf());
        let (_t, instance) = make_instance(vec![Permission::Ai]);
        instance.attach_runtime(&runtime);
        instance.set_status(PluginStatus::Running);
        instance.session_active.store(true, Ordering::SeqCst);
        runtime
            .instances
            .lock()
            .unwrap()
            .insert(instance.id.clone(), instance.clone());
        instance
            .dispatch(
                method::ai::REGISTER_TOOL,
                &json!({
                    "name": "find_notes",
                    "description": "按关键词搜索仓库笔记",
                    "parameters": { "type": "object", "properties": { "q": { "type": "string" } } }
                }),
            )
            .expect("注册应成功");

        // 仅作废会话（状态仍是 Running）：模拟收尾已开始、在途调用到达的那一瞬
        instance.session_active.store(false, Ordering::SeqCst);
        let error = runtime
            .call_tool("find_notes", json!({ "q": "周报" }))
            .await
            .expect_err("会话失效后不得再登记工具调用");
        assert!(error.contains("插件未运行"), "{error}");
        assert!(runtime.tool_calls.lock().unwrap().is_empty(), "被拒的调用不得落表");
    }

    /// 导入源只能是插件私有目录内的既有文件：目录外、目录本身、空路径都拒收。
    #[test]
    fn import_source_must_be_a_file_inside_private_dir() {
        let (tmp, instance) = make_instance(vec![Permission::Vault, Permission::Fs]);
        let private = fs::private_dir(&instance.state_dir).unwrap();
        let inside = PathBuf::from(&private).join("staged.bin");
        std::fs::write(&inside, b"abc").unwrap();
        let resolved = instance.import_source(inside.to_str().unwrap()).expect("私有目录内的文件可导入");
        assert_eq!(PathBuf::from(&resolved), std::fs::canonicalize(&inside).unwrap());

        let outside = tmp.join("outside.bin");
        std::fs::write(&outside, b"abc").unwrap();
        let err = instance.import_source(outside.to_str().unwrap()).expect_err("目录外文件应拒收");
        assert!(err.contains("私有目录"), "{err}");
        assert!(instance.import_source("   ").is_err(), "空路径应拒收");
        assert!(instance.import_source(&private).is_err(), "目录本身不是可导入的文件");
        assert!(instance
            .import_source(inside.join("missing").to_str().unwrap())
            .is_err(), "不存在的源文件应拒收");
    }

    /// 导入请求路由到壳：源路径已规范化为绝对路径，目标路径原样带上。
    #[test]
    fn import_file_routes_to_shell() {
        let tmp = TempDir::new("instance-import-file");
        let runtime = PluginRuntime::new(tmp.to_path_buf());
        let (_t, instance) = make_instance(vec![Permission::Vault, Permission::Fs]);
        instance.attach_runtime(&runtime);
        runtime
            .instances
            .lock()
            .unwrap()
            .insert("com.example.demo".to_string(), instance.clone());
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let private = fs::private_dir(&instance.state_dir).unwrap();
        let staged = PathBuf::from(&private).join("big.bin");
        std::fs::write(&staged, b"0123").unwrap();
        instance.handle_line(&request_line(
            method::vault::IMPORT_FILE,
            &json!({ "source": staged.to_str().unwrap(), "path": "media/big.bin" }),
        ));
        let request = rt.block_on(runtime.recv_host_request()).unwrap();
        let HostRequest::VaultImportFile { request_id, path, source, .. } = request else {
            panic!("应投递文件导入请求");
        };
        assert_eq!(path, "media/big.bin");
        assert_eq!(PathBuf::from(&source), std::fs::canonicalize(&staged).unwrap());
        assert_eq!(std::fs::read(&source).unwrap(), b"0123", "宿主按源路径读取内容");
        runtime.resolve_shell_call(request_id, Ok(json!({})));
    }

    /// 默认布局：合法规格存档并广播给壳；非法名 / 非法规格拒收且不覆盖已存档值。
    #[test]
    fn declare_default_layout_validates_and_broadcasts() {
        let (_tmp, instance, mut rx) = make_instance_with_events(Vec::new());
        let spec = json!({
            "kind": "split",
            "direction": "horizontal",
            "children": [
                { "kind": "panel", "views": ["files"] },
                { "kind": "panel", "views": ["note", "aichat"] }
            ],
            "sizes": [30, 70]
        });
        instance
            .dispatch(
                method::layout::DECLARE_DEFAULT,
                &json!({ "name": "LLaMA", "spec": spec }),
            )
            .expect("合法布局应被接收");
        let (name, stored) = instance.default_layout().expect("应存档");
        assert_eq!(name, "LLaMA");
        assert!(matches!(stored, LayoutSpecNode::Split { .. }));
        assert!(matches!(
            rx.try_recv(),
            Ok(PluginEvent::DefaultLayoutDeclared { id, name, .. })
                if id == "com.example.demo" && name == "LLaMA"
        ));

        for bad in [
            json!({ "name": "  ", "spec": spec }),
            json!({ "name": "空面板", "spec": { "kind": "panel", "views": [] } }),
        ] {
            assert!(
                instance.dispatch(method::layout::DECLARE_DEFAULT, &bad).is_err(),
                "应拒绝：{bad}"
            );
        }
        assert_eq!(instance.default_layout().unwrap().0, "LLaMA", "拒收不得覆盖已存档布局");
    }
}
