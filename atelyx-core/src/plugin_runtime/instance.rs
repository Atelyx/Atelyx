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
use atelyx_plugin_protocol::manifest::Manifest;
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

use super::{capability, child_process, fs, http, shortcut};
use super::{
    AuditRecord, Contribution, DialogKind, DialogRequest, PluginEvent, PluginRuntime, PluginStatus,
    SurfaceKind, ViewEvent, ViewNode,
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
    view_submits: AtomicU64,
    crash_times: Mutex<Vec<Instant>>,
}

/// 画布节点 → 指令清单存档（(面 id, 节点 id) → 清单）。
type CanvasLists = Mutex<HashMap<(String, String), Arc<Vec<canvas::DrawCommand>>>>;

/// 一个已注册贡献点（不含插件 id；插件 id 由汇总侧补上）。
struct ContributionEntry {
    slot: ContributionSlot,
    id: String,
    label: String,
    icon: Option<String>,
}

impl Instance {
    pub(crate) fn new(
        manifest: Manifest,
        dir: PathBuf,
        state_dir: PathBuf,
        process_host: Arc<PluginProcessHost>,
        shortcuts: Arc<shortcut::Registrar>,
        events: tokio::sync::broadcast::Sender<PluginEvent>,
    ) -> Arc<Self> {
        Arc::new(Self {
            id: manifest.id.clone(),
            manifest,
            dir,
            state_dir,
            process_host,
            shortcuts,
            events,
            runtime: Mutex::new(Weak::new()),
            children: child_process::Spawned::default(),
            resources: Mutex::new(()),
            pid: AtomicU64::new(0),
            run_token: AtomicU64::new(0),
            handshaken: AtomicBool::new(false),
            status: Mutex::new(PluginStatus::Stopped),
            generation: AtomicU64::new(0),
            transition: Mutex::new(()),
            subscriptions: Mutex::new(HashSet::new()),
            denied: Mutex::new(Vec::new()),
            surfaces: Mutex::new(HashMap::new()),
            canvases: Mutex::new(HashMap::new()),
            contributions: Mutex::new(Vec::new()),
            view_submits: AtomicU64::new(0),
            crash_times: Mutex::new(Vec::new()),
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
            })
            .collect()
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
        self.set_status(PluginStatus::Running);
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
        self.run_token.fetch_add(1, Ordering::SeqCst);
        self.teardown();
        self.set_status(PluginStatus::Stopped);
    }

    /// 判定为不可装载 / 握手失败：置 `Failed` 并结束进程（避免僵死连接）。
    fn fail(self: &Arc<Self>, reason: String) {
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
        for child in self.children.take_all() {
            if let Err(e) = self.process_host.kill_process_tree(child) {
                eprintln!("[plugin {}] 结束子进程 {child} 失败：{e}", self.id);
            }
        }
        self.shortcuts.release_plugin(&self.id);
        self.fail_pending_dialogs("插件已停止，对话框已取消");
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
            // 插件 stderr 归入诊断输出（协议正文只走 stdout）
            HostEvent::Stderr { data } => eprint!("[plugin {}] {data}", self.id),
            HostEvent::Terminated { code } => self.on_exit(code),
            HostEvent::Error { message } => eprintln!("[plugin {}] {message}", self.id),
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
                self.fail(format!("插件输出非协议内容（stdout 只允许 JSON-RPC 信封）：{e}"));
                return;
            }
        };
        match envelope {
            Envelope::Request(request) => self.handle_request(token, request),
            Envelope::Response(_) => eprintln!(
                "[plugin {}] 收到插件的响应（协议约定插件→宿主只有请求，宿主→插件只有通知）",
                self.id
            ),
            Envelope::Notification(notification) => eprintln!(
                "[plugin {}] 收到插件通知（协议约定插件→宿主只有请求）：{}",
                self.id, notification.method
            ),
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
        // 注册热键），再把随它记账的资源收掉——托管服务不能因宿主崩溃而留成孤儿
        self.run_token.fetch_add(1, Ordering::SeqCst);
        self.release_resources();
        let count = record_crash(&mut self.crash_times.lock().unwrap(), Instant::now());
        if count >= CRASH_LIMIT {
            eprintln!(
                "[plugin {}] {} 秒内崩溃 {count} 次，已熔断（需手动重试）",
                self.id,
                CRASH_WINDOW.as_secs()
            );
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
                let _ = self.events.send(PluginEvent::Notification {
                    id: self.id.clone(),
                    title: p.title,
                    body: p.body,
                    level: p.level,
                });
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
        let result = InitializeResult {
            api_version: API_VERSION,
            host: HostInfo {
                name: HOST_NAME.to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
        };
        serde_json::to_value(result).map_err(|e| RpcError::failed(format!("握手应答序列化失败：{e}")))
    }

    fn on_subscribe(&self, params: &Value) -> Result<Value, RpcError> {
        let p: event::SubscribeParams = parse(params)?;
        if p.name != event::name::VAULT_SWITCH {
            return Err(RpcError::failed(format!("未知事件名：{}", p.name)));
        }
        self.subscriptions.lock().unwrap().insert(p.name);
        Ok(json!({ "subscribed": true }))
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
            });
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

    /// 回发一条节点界面事件（`host:view.event`）。
    pub(crate) fn notify_view_event(&self, surface_id: &str, node_id: &str, event: ViewEvent) {
        let params = ViewEventParams {
            surface_id: surface_id.to_string(),
            node_id: node_id.to_string(),
            event,
        };
        self.notify(method::view::HOST_EVENT, &params);
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
    use atelyx_plugin_protocol::view::NodeBody;

    /// 构造一个不落进程的实例（`entry` 留空：单元测试只驱动请求分派与状态，不启动进程）。
    fn make_instance(permissions: Vec<Permission>) -> (TempDir, Arc<Instance>) {
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
        let (events, _rx) = tokio::sync::broadcast::channel(16);
        let instance = Instance::new(
            manifest,
            dir,
            tmp.join("state"),
            Arc::new(PluginProcessHost::new()),
            // 无 OS 层的注册器：单测不落真实热键，注册路径以可读失败收场
            shortcut::Registrar::unavailable("全局快捷键不可用：测试"),
            events,
        );
        (tmp, instance)
    }

    fn initialize_params(api_version: u32, id: &str) -> Value {
        json!({ "apiVersion": api_version, "plugin": { "id": id, "name": "Demo", "version": "1.0.0" } })
    }

    /// 造一行插件→宿主请求（走 `handle_line`，闸门与审计都在那条路径上）。
    fn request_line(method_name: &str, params: &Value) -> String {
        json!({ "jsonrpc": "2.0", "id": 1, "method": method_name, "params": params }).to_string()
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
    fn declared_but_unimplemented_capability_fails() {
        let (_tmp, instance) = make_instance(vec![Permission::Clipboard]);
        let error = instance
            .dispatch(method::clipboard::READ_TEXT, &Value::Null)
            .expect_err("已声明但未实现的能力面应返回失败");
        assert_eq!(error.code, RpcError::FAILED);
        assert!(error.message.contains("尚未实现"), "{}", error.message);
    }

    /// 各能力面「未声明被拒」：闸门只认 manifest 声明，未声明一律 fail-closed 且记审计。
    #[test]
    fn undeclared_calls_of_each_face_are_denied() {
        let cases: [(Permission, &str, Value); 5] = [
            // 入参一律取「必然执行失败但不会真的落盘 / 出网 / 弹框」的形态
            (Permission::Process, method::process::SPAWN, json!({ "program": "atelyx-no-such-program" })),
            (Permission::Fs, method::fs::LIST, json!({ "path": "atelyx-no-such-dir" })),
            (Permission::Shortcuts, method::shortcut::REGISTER, json!({ "accelerator": "Ctrl+F9" })),
            (Permission::Http, method::http::REQUEST, json!({ "method": "GET", "url": "ftp://example.com" })),
            (Permission::Dialog, method::dialog::PICK_DIRECTORY, Value::Null),
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

    #[test]
    fn state_roundtrip_through_dispatch() {
        let (_tmp, instance) = make_instance(vec![Permission::State]);
        instance
            .dispatch(method::state::WRITE, &json!({ "json": { "n": 1 } }))
            .expect("写 state 应成功");
        let read = instance.dispatch(method::state::READ, &Value::Null).expect("读 state 应成功");
        assert_eq!(read["json"]["n"], 1);
    }

    #[test]
    fn subscribe_only_accepts_known_events() {
        let (_tmp, instance) = make_instance(Vec::new());
        assert!(instance
            .dispatch(event::SUBSCRIBE, &json!({ "name": event::name::VAULT_SWITCH }))
            .is_ok());
        assert!(instance.dispatch(event::SUBSCRIBE, &json!({ "name": "nope" })).is_err());
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
}
