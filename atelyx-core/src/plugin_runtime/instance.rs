//! 单个插件的 RPC 泵：进程输出逐行解析、插件→宿主请求按权限闸门与能力分派应答。
//! 方向约定：插件→宿主一律请求，宿主→插件一律通知（无请求 / 响应往返，故无需请求配对表）。
//! stdout 只允许协议信封（非 JSON = 协议违规，停插件并给出可读原因）。
//!
//! 请求在进程 stdout 读线程上**顺序**处理（同一进程的读线程唯一，`state` 写后读天然保序）。
//! 每次启动领一个运行令牌（[`Instance::run_token`]）：令牌不匹配的输出（上次启动遗留）一律丢弃，
//! 否则「停用 → 启用」后到达的旧进程事件会被当成新进程的崩溃，抹掉新 pid 并再起一个实例。

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use atelyx_plugin_protocol::envelope::{Envelope, Request, RequestId, RpcError};
use atelyx_plugin_protocol::event;
use atelyx_plugin_protocol::handshake::{self, HostInfo, InitializeParams, InitializeResult};
use atelyx_plugin_protocol::manifest::Manifest;
use atelyx_plugin_protocol::method;
use atelyx_plugin_protocol::{view, API_VERSION};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use crate::plugin_process::{EventSink, PluginProcessHost, ProcessEvent};

use super::capability;
use super::{AuditRecord, PluginEvent, PluginRuntime, PluginStatus};

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
    events: tokio::sync::broadcast::Sender<PluginEvent>,
    /// 供崩溃退避线程回调用（弱引用，避免与运行时的强引用成环）。
    runtime: Mutex<Weak<PluginRuntime>>,
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
    view_submits: AtomicU64,
    crash_times: Mutex<Vec<Instant>>,
}

impl Instance {
    pub(crate) fn new(
        manifest: Manifest,
        dir: PathBuf,
        state_dir: PathBuf,
        process_host: Arc<PluginProcessHost>,
        events: tokio::sync::broadcast::Sender<PluginEvent>,
    ) -> Arc<Self> {
        Arc::new(Self {
            id: manifest.id.clone(),
            manifest,
            dir,
            state_dir,
            process_host,
            events,
            runtime: Mutex::new(Weak::new()),
            pid: AtomicU64::new(0),
            run_token: AtomicU64::new(0),
            handshaken: AtomicBool::new(false),
            status: Mutex::new(PluginStatus::Stopped),
            generation: AtomicU64::new(0),
            transition: Mutex::new(()),
            subscriptions: Mutex::new(HashSet::new()),
            denied: Mutex::new(Vec::new()),
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
        let pid = self.pid.swap(0, Ordering::SeqCst) as u32;
        if pid != 0 {
            if let Err(e) = self.process_host.kill_process_tree(pid) {
                eprintln!("[plugin {}] 结束进程树失败：{e}", self.id);
            }
        }
        self.set_status(PluginStatus::Stopped);
    }

    /// 判定为不可装载 / 握手失败：置 `Failed` 并结束进程（避免僵死连接）。
    fn fail(self: &Arc<Self>, reason: String) {
        self.set_status(PluginStatus::Failed(reason));
        self.run_token.fetch_add(1, Ordering::SeqCst);
        let pid = self.pid.swap(0, Ordering::SeqCst) as u32;
        if pid != 0 {
            if let Err(e) = self.process_host.kill_process_tree(pid) {
                eprintln!("[plugin {}] 结束进程树失败：{e}", self.id);
            }
        }
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

    fn handle_process_event(self: &Arc<Self>, token: u64, event: ProcessEvent) {
        // 上次启动遗留的输出（含延迟到达的退出上报）一律丢弃：pid / 状态已被新一次启动改写
        if token != self.run_token.load(Ordering::SeqCst) {
            return;
        }
        match event {
            ProcessEvent::Stdout { data } => {
                for line in data.lines() {
                    let line = line.trim();
                    if !line.is_empty() {
                        self.handle_line(line);
                    }
                }
            }
            // 插件 stderr 归入诊断输出（协议正文只走 stdout）
            ProcessEvent::Stderr { data } => eprint!("[plugin {}] {data}", self.id),
            ProcessEvent::Terminated { code } => self.on_exit(code),
            ProcessEvent::Error { message } => eprintln!("[plugin {}] {message}", self.id),
        }
    }

    fn handle_line(self: &Arc<Self>, line: &str) {
        let envelope = match Envelope::parse(line) {
            Ok(envelope) => envelope,
            Err(e) => {
                self.fail(format!("插件输出非协议内容（stdout 只允许 JSON-RPC 信封）：{e}"));
                return;
            }
        };
        match envelope {
            Envelope::Request(request) => self.handle_request(request),
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

    fn handle_request(self: &Arc<Self>, request: Request) {
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
        match self.dispatch(method, &request.params) {
            Ok(value) => self.respond(id, Some(value), None),
            Err(error) => self.respond(id, None, Some(error)),
        }
    }

    fn dispatch(self: &Arc<Self>, method: &str, params: &Value) -> Result<Value, RpcError> {
        match method {
            handshake::INITIALIZE => self.on_initialize(params),
            event::SUBSCRIBE => self.on_subscribe(params),
            method::view::SUBMIT => self.on_submit(params),
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

    /// 校验并接收界面树：只校验与计数，本层不保留树（挂载由宿主界面层承担）。surfaceId 为空即拒。
    fn on_submit(&self, params: &Value) -> Result<Value, RpcError> {
        let p: method::SubmitParams = parse(params)?;
        if p.surface_id.trim().is_empty() {
            return Err(RpcError::failed("surfaceId 不能为空"));
        }
        view::ViewNode::validate(&p.tree).map_err(RpcError::failed)?;
        self.view_submits.fetch_add(1, Ordering::Relaxed);
        Ok(json!({ "applied": true }))
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

    /// 回执一条响应（写往插件 stdin）。进程已停时静默丢弃（对端已不在）。
    fn respond(&self, id: RequestId, result: Option<Value>, error: Option<RpcError>) {
        let envelope = match (result, error) {
            (Some(result), None) => Envelope::success(id, result),
            (_, Some(error)) => Envelope::failure(id, error),
            (None, None) => Envelope::success(id, Value::Null),
        };
        let pid = self.pid.load(Ordering::SeqCst) as u32;
        if pid == 0 {
            return;
        }
        let mut line = envelope.to_line();
        line.push('\n');
        if let Err(e) = self.process_host.write_stdin(pid, &line) {
            eprintln!("[plugin {}] 回执写入失败：{e}", self.id);
        }
    }
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

fn parse<T: DeserializeOwned>(params: &Value) -> Result<T, RpcError> {
    serde_json::from_value(params.clone()).map_err(|e| RpcError::failed(format!("params 不可解析：{e}")))
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
            events,
        );
        (tmp, instance)
    }

    fn initialize_params(api_version: u32, id: &str) -> Value {
        json!({ "apiVersion": api_version, "plugin": { "id": id, "name": "Demo", "version": "1.0.0" } })
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
        let (_tmp, instance) = make_instance(vec![Permission::Fs]);
        let error = instance
            .dispatch(method::fs::READ_TEXT, &json!({ "path": "x" }))
            .expect_err("已声明但未实现的能力面应返回失败");
        assert_eq!(error.code, RpcError::FAILED);
        assert!(error.message.contains("尚未实现"), "{}", error.message);
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
        instance.handle_process_event(stale, ProcessEvent::Terminated { code: Some(1) });
        assert_eq!(instance.status(), PluginStatus::Running);
        // 令牌匹配时按崩溃处理：进入退避重启
        let current = instance.run_token.load(Ordering::SeqCst);
        instance.handle_process_event(current, ProcessEvent::Terminated { code: Some(1) });
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
}
