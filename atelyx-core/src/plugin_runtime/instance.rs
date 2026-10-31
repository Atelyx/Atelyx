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
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

use atelyx_plugin_protocol::envelope::{Envelope, Request, RpcError};
use atelyx_plugin_protocol::event;
use atelyx_plugin_protocol::handshake::{self, HostInfo, InitializeParams, InitializeResult};
use atelyx_plugin_protocol::manifest::Manifest;
use atelyx_plugin_protocol::method::{
    self, ContributionSlot, RegisterParams, SurfaceAttachedParams, SurfaceDetachedParams,
    UnregisterParams,
};
use atelyx_plugin_protocol::frame_region::FrameRegionInfo;
use atelyx_plugin_protocol::service::{self, CallParams, ResultParams};
use atelyx_plugin_protocol::{frame, frame_codec, API_VERSION};
use base64::Engine;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};

use atelyx_plugin_host::HostCall;

use crate::plugin_process::{EventSink, PluginProcessHost, ProcessEvent as HostEvent};

use super::{abi, capability, child_process, fs, http, log, shortcut};
use super::frame_region::FrameRegionStore;
use super::service_dir::{Forward, ServiceTarget};
use super::{
    AuditRecord, Contribution, DialogKind, DialogRequest, HostRequest, LayoutSpecNode, PaddingSpec,
    PluginEvent, PluginRuntime, PluginStatus, SurfaceKind, MAX_CLIPBOARD_IMAGE_BYTES,
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
/// 尺寸变更兜底：新帧回执迟迟不来时，等这么久就把最新一笔尺寸再发一次。
const RESIZE_FALLBACK: Duration = Duration::from_millis(150);
/// 尺寸变更放弃：自最后一笔尺寸下发起超过此时长仍无新帧，判定该面不会再回帧，闸门就此撤掉。
const RESIZE_GIVE_UP: Duration = Duration::from_secs(2);
/// stdin 写失败记日志的最小间隔：帧拍 / 输入高频失败时不重复写满插件日志。
const WRITE_ERROR_NOTE_INTERVAL: Duration = Duration::from_secs(5);

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
    /// 能力调用审计（声明与否都执行；未声明的记 `declared: false`）。
    audits: Mutex<Vec<AuditRecord>>,
    /// 本次会话里出现过「用到但未声明」的方法名（提示只发一次）与累计条数（不受审计列表截断影响）。
    undeclared_methods: Mutex<HashSet<String>>,
    undeclared_calls: AtomicU64,
    /// 各面整面帧（surface id → 帧）；会话收尾一并清除。
    frames: Mutex<FrameStore>,
    /// 新帧落地信号：与 `frames` 同一把锁配对，供绘制期同步等新帧（见 [`Instance::finish_frame_wait`]）。
    frame_signal: Condvar,
    /// 有帧被绘制期同步取走后置位，壳在心跳里据此补一次投影。
    observed_pending: AtomicBool,
    /// 声明了帧率的整面帧（面 id → 节拍）；随帧提交同步。
    surface_clocks: Mutex<HashMap<String, FrameClock>>,
    /// 整面帧共享内存区域：握手时建，扩容换新，会话收尾释放。
    frame_region: FrameRegionStore,
    /// 各面的尺寸变更闸门：拖动中的中间尺寸只保留最新一笔，等新帧回执或兜底到点再放下一笔。
    resize_gates: Mutex<HashMap<String, ResizeGate>>,
    /// 上一次「stdin 写入失败」记日志的时刻（限频用）。
    last_write_error: Mutex<Option<Instant>>,
    /// 已注册贡献点（按注册序）；id 插件内唯一，撤销按 id 摘除。
    contributions: Mutex<Vec<ContributionEntry>>,
    submits: AtomicU64,
    crash_times: Mutex<Vec<Instant>>,
    /// 本会话声明的默认布局（重启后插件重新声明）。
    default_layout: Mutex<Option<(String, LayoutSpecNode)>>,
    /// 本插件日志（插件 stderr、协议违规与生命周期失败的唯一可查落点）。
    log: Arc<log::LogSink>,
    /// 动态库传输；进程插件始终为空。
    abi: abi::AbiSession,
}

/// 各面已落地的整面帧与正在等帧的面。
#[derive(Default)]
struct FrameStore {
    frames: HashMap<String, Arc<frame::SurfaceFrame>>,
    /// 已登记等帧的面 → 等帧前画在屏幕上的那一帧：登记期间退回的帧由等帧方取走画上，
    /// 不再另发一条通知（否则每拍都会多一次整窗重建）。
    waiting: HashMap<String, Arc<frame::SurfaceFrame>>,
}

/// 一个画布节点的帧节拍：声明了 `fps` 才有，宿主按它决定什么时候发 `host:canvas.frame`。
#[derive(Clone, Copy, Debug)]
struct FrameClock {
    /// 声明帧率：界面树重发时帧率未变的节点沿用原钟（帧序号不因界面更新而重置）。
    fps: u16,
    interval: Duration,
    /// 已发出的帧数（下发时取当前值再自增，故帧序号从 0 起）。
    frame: u64,
    /// 上一帧的下发时刻（首帧尚无 = 0）。
    last: Option<Instant>,
    next: Instant,
}

impl FrameClock {
    fn new(fps: u16, now: Instant) -> Self {
        let fps = fps.max(1);
        let interval = Duration::from_millis((1000.0 / f32::from(fps)).round().max(1.0) as u64);
        Self { fps, interval, frame: 0, last: None, next: now + interval }
    }

    /// 到期即推进节拍并给出本帧的（帧序号, 距上一帧毫秒）；未到期返回 `None`。
    /// 落后多拍时对齐到 `now` 而不是补发中间的帧。
    fn take_due(&mut self, now: Instant) -> Option<(u64, u32)> {
        if now < self.next {
            return None;
        }
        let delta_ms = self
            .last
            .map(|last| now.duration_since(last).as_millis().min(u128::from(u32::MAX)) as u32)
            .unwrap_or(0);
        let frame = self.frame;
        self.frame += 1;
        self.last = Some(now);
        self.next = now + self.interval;
        Some((frame, delta_ms))
    }
}

/// 一个已注册贡献点（不含插件 id；插件 id 由汇总侧补上）。
struct ContributionEntry {
    slot: ContributionSlot,
    id: String,
    label: String,
    icon: Option<String>,
    padding: Option<PaddingSpec>,
}

/// 一个面的尺寸变更闸门：拖动中的中间尺寸只留最新一笔，等新帧回执或兜底到点再放下一笔。
#[derive(Default)]
struct ResizeGate {
    pending: Option<(f32, f32)>,
    /// 已发出的那笔的发出时刻；兜底定时器据此判断该重发还是撤闸。
    in_flight: Option<Instant>,
    /// 该面是否已有兜底定时器：有则后续派发只更新在途时刻，不再起线程。
    timer_active: bool,
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
        let manifest_id = manifest.id.clone();
        Arc::new(Self {
            id: manifest_id.clone(),
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
            audits: Mutex::new(Vec::new()),
            undeclared_methods: Mutex::new(HashSet::new()),
            undeclared_calls: AtomicU64::new(0),
            frames: Mutex::new(FrameStore::default()),
            frame_signal: Condvar::new(),
            observed_pending: AtomicBool::new(false),
            surface_clocks: Mutex::new(HashMap::new()),
            frame_region: FrameRegionStore::new(&manifest_id),
            resize_gates: Mutex::new(HashMap::new()),
            last_write_error: Mutex::new(None),
            contributions: Mutex::new(Vec::new()),
            submits: AtomicU64::new(0),
            crash_times: Mutex::new(Vec::new()),
            default_layout: Mutex::new(None),
            log,
            abi: abi::AbiSession::default(),
        })
    }

    fn is_abi(&self) -> bool {
        self.manifest.abi_version.is_some()
    }

    pub(crate) fn attach_runtime(&self, runtime: &Arc<PluginRuntime>) {
        *self.runtime.lock().unwrap() = Arc::downgrade(runtime);
    }

    pub(crate) fn status(&self) -> PluginStatus {
        self.status.lock().unwrap().clone()
    }

    pub(crate) fn audit(&self) -> Vec<AuditRecord> {
        self.audits.lock().unwrap().clone()
    }

    /// 实际用到但未声明过的调用条数（声明过的调用也进审计，但不计入这里）。
    pub(crate) fn undeclared_count(&self) -> usize {
        self.undeclared_calls.load(Ordering::Relaxed) as usize
    }

    pub(crate) fn submits(&self) -> u64 {
        self.submits.load(Ordering::Relaxed)
    }

    /// 某面的整面帧（会话收尾即清空）。
    pub(crate) fn surface_frame(&self, surface_id: &str) -> Option<Arc<frame::SurfaceFrame>> {
        self.frames.lock().unwrap().frames.get(surface_id).cloned()
    }

    /// 登记一次改宽等帧。须在向插件发 `Resize` 之前登记：登记期间退回的帧由
    /// [`Instance::finish_frame_wait`] 取走并画上，提交路径不再发通知。
    pub(crate) fn begin_frame_wait(&self, surface_id: &str, seen: Arc<frame::SurfaceFrame>) {
        self.frames.lock().unwrap().waiting.insert(surface_id.to_string(), seen);
    }

    /// 等某面出现一张不同于登记时画面的新帧（改宽那一拍同步取新帧用）。
    /// 超时、未登记、该面已无可渲染帧时返回 `None`：调用方按手上的帧画。
    pub(crate) fn finish_frame_wait(
        &self,
        surface_id: &str,
        timeout: Duration,
    ) -> Option<Arc<frame::SurfaceFrame>> {
        let deadline = Instant::now() + timeout;
        let mut store = self.frames.lock().unwrap();
        let seen = store.waiting.get(surface_id).cloned()?;
        loop {
            match store.frames.get(surface_id) {
                Some(frame) if !Arc::ptr_eq(frame, &seen) => {
                    let frame = frame.clone();
                    store.waiting.remove(surface_id);
                    drop(store);
                    self.observed_pending.store(true, Ordering::Relaxed);
                    return Some(frame);
                }
                Some(_) => {}
                None => {
                    store.waiting.remove(surface_id);
                    return None;
                }
            }
            let now = Instant::now();
            if now >= deadline {
                store.waiting.remove(surface_id);
                return None;
            }
            store = self.frame_signal.wait_timeout(store, deadline - now).ok()?.0;
        }
    }

    /// 取走「有帧被同步取走」的待补投影标记。
    pub(crate) fn take_observed_pending(&self) -> bool {
        self.observed_pending.swap(false, Ordering::Relaxed)
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

    /// 宿主侧诊断写入本插件日志（带 `[宿主]` 前缀，与插件自身的 stderr 区分归属）。
    pub(crate) fn log_host_note(&self, text: &str) {
        self.log.line(&format!("[宿主] {text}"));
    }

    /// 清空本插件日志（当前文件与轮转出的上一代）；写端由日志自己关闭后重建。
    pub(crate) fn clear_log(&self) -> Result<(), String> {
        self.log.clear()
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
        log::reset_on_version_change(
            &self.log,
            &super::layout::loaded_version_file(&self.dir),
            &self.manifest.version,
        );
        // 领本次运行的令牌：此后上一次会话的输出与事件一律按令牌丢弃
        let token = self.run_token.fetch_add(1, Ordering::SeqCst) + 1;
        self.handshaken.store(false, Ordering::SeqCst);
        if self.is_abi() {
            return self.start_abi(token, entry);
        }
        let program = self.dir.join(entry).to_string_lossy().into_owned();
        let cwd = self.dir.to_string_lossy().into_owned();
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

    /// 启动动态库插件：库在本实例内跨启停保留；加载库是同步的，建实例与握手放到后台线程，
    /// 宿主调用线程不等待插件初始化（与进程插件同一口径，失败经状态与日志呈现）。
    fn start_abi(self: &Arc<Self>, token: u64, entry: &str) -> Result<(), String> {
        let path = self.dir.join(entry);
        let library = match self.abi.library() {
            Some(library) => library,
            None => match atelyx_plugin_host::PluginLibrary::load(&path) {
                Ok(library) => {
                    let library = Arc::new(library);
                    self.abi.set_library(library.clone());
                    library
                }
                Err(error) => {
                    self.set_status(PluginStatus::Failed(error.clone()));
                    self.log.line(&format!("[失败] {error}"));
                    return Err(error);
                }
            },
        };
        self.session_active.store(true, Ordering::SeqCst);
        // 会话先就绪再广播 Running：壳收到 Running 会重发已挂面的 attached，早于 begin_session 会被丢掉
        self.abi.begin_session(token);
        self.set_status(PluginStatus::Running);
        let host: Arc<dyn atelyx_plugin_host::Host> =
            Arc::new(abi::InstanceHost::new(self, token));
        let instance = self.clone();
        let entry = entry.to_string();
        std::thread::spawn(move || match library.create(host) {
            Ok(plugin) => {
                if instance.abi.install(plugin, token) {
                    instance.log.line(&format!("[启动] 动态库已加载（入口 {entry}）"));
                }
            }
            Err(error) => instance.fail_abi_start(token, error),
        });
        // 只持弱引用：卸载后实例应随最后一个引用释放，不因握手截止线程多留十秒
        let watchdog = Arc::downgrade(self);
        std::thread::spawn(move || {
            std::thread::sleep(HANDSHAKE_TIMEOUT);
            if let Some(instance) = watchdog.upgrade() {
                instance.check_handshake(token);
            }
        });
        Ok(())
    }

    /// 后台建实例失败：仅在本次运行仍有效时落失败状态；停用 / 换代后的迟到失败不覆盖新会话。
    fn fail_abi_start(self: &Arc<Self>, token: u64, error: String) {
        let _guard = self.transition.lock().unwrap();
        if self.run_token.load(Ordering::SeqCst) != token {
            return;
        }
        self.release_resources();
        self.session_active.store(false, Ordering::SeqCst);
        self.abi.shutdown();
        self.set_status(PluginStatus::Failed(error.clone()));
        self.log.line(&format!("[失败] {error}"));
        self.log.close();
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
        if self.is_abi() {
            // 先让在途调用以可读失败收场（此时插件还活着，回执能送达），再停 worker
            self.release_resources();
            self.abi.shutdown();
            self.log.close();
        } else {
            self.teardown();
        }
        self.set_status(PluginStatus::Stopped);
    }

    /// 判定为不可装载 / 握手失败：置 `Failed` 并结束进程（避免僵死连接）。
    /// `token` 是引发失败的会话令牌；取到启停锁后令牌已被取代就不再动新会话。
    fn fail(self: &Arc<Self>, token: u64, reason: String) {
        let _guard = self.transition.lock().unwrap();
        if self.run_token.load(Ordering::SeqCst) != token {
            return;
        }
        self.log.line(&format!("[失败] {reason}"));
        self.session_active.store(false, Ordering::SeqCst);
        self.set_status(PluginStatus::Failed(reason));
        self.run_token.fetch_add(1, Ordering::SeqCst);
        if self.is_abi() {
            // 先作废会话：销毁可能卡在插件的 worker 上，期间迟到的建实例结果不得再装入。
            self.abi.invalidate(token);
            self.release_resources();
            // 销毁会等插件的 worker / 泵线程退出：另起线程收尾，当前调用先返回。
            // 收尾前复查世代：期间已被重新启用（start 换代）就不再动新会话。
            let generation = self.generation.load(Ordering::SeqCst);
            let instance = self.clone();
            std::thread::spawn(move || {
                let _guard = instance.transition.lock().unwrap();
                if instance.generation.load(Ordering::SeqCst) != generation {
                    return;
                }
                instance.abi.shutdown();
                instance.log.close();
            });
        } else {
            self.teardown();
        }
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
    /// 与界面贡献。停用 / 失败 / 崩溃 / 卸载四条路径共用——漏掉任何一条都会留下孤儿进程、
    /// 永久悬挂的调用，或让已停插件的界面继续对外呈现。
    fn release_resources(&self) {
        let frames_dropped;
        let contributions_dropped;
        {
            // 与资源获取互斥：本段跑完之后到达的登记一律被 [`Instance::with_session`] 拒掉
            let _gate = self.resources.lock().unwrap();
            self.subscriptions.lock().unwrap().clear();
            self.undeclared_methods.lock().unwrap().clear();
            self.undeclared_calls.store(0, Ordering::Relaxed);
            {
                let mut store = self.frames.lock().unwrap();
                frames_dropped = !store.frames.is_empty();
                store.frames.clear();
                store.waiting.clear();
            }
            self.observed_pending.store(false, Ordering::Relaxed);
            self.frame_signal.notify_all();
            self.surface_clocks.lock().unwrap().clear();
            self.resize_gates.lock().unwrap().clear();
            self.frame_region.release();
            contributions_dropped = !self.contributions.lock().unwrap().is_empty();
            self.contributions.lock().unwrap().clear();
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
        }
        // 服务回落要向其余插件各写一次 stdin：投递只入队不阻塞，放在闸门外是为缩短本实例持闸时间，
        // 不与其它实例的写入路径互等
        self.release_services();
        // 界面与贡献变化广播给壳：壳据此停下已停插件界面的渲染并清掉菜单条目
        if frames_dropped {
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

    /// 摘除本插件的服务登记（活动提供者消失即回落内置）并让以它为提供者的在途调用立即失败。
    fn release_services(&self) {
        let runtime = self.runtime.lock().unwrap().clone();
        if let Some(runtime) = runtime.upgrade() {
            runtime.drop_plugin_services(&self.id);
            runtime.fail_service_calls_of(&self.id);
        }
    }

    /// 下发一次服务调用请求（`host:service.call`）：提供者须用同一 `callId` 经 `service.result` 回报。
    pub(crate) fn notify_service_call(
        &self,
        call_id: &str,
        service_name: &str,
        method_name: &str,
        params: Value,
    ) {
        if self.is_abi() {
            let Some(id) = self.abi.begin_service_call(call_id.to_string()) else {
                self.fail_service_call(call_id, "插件已停止，服务调用已取消");
                return;
            };
            let params = serde_json::to_string(&params).unwrap_or_else(|_| "null".into());
            let state = self.abi.service_call(id, service_name, method_name, &params);
            if let abi::CallState::Delivered = state {
                return;
            }
            if let Some(call_id) = self.abi.finish_service_call(id) {
                // 初始化窗口里没装入的实例不是「已停止」：分开写，排障才看得准
                let reason = match state {
                    abi::CallState::Starting => "插件尚未就绪，服务调用已取消",
                    _ => "插件已停止，服务调用已取消",
                };
                self.fail_service_call(&call_id, reason);
            }
            return;
        }
        let params = service::HostCallParams {
            call_id: call_id.to_string(),
            service: service_name.to_string(),
            method: method_name.to_string(),
            params,
        };
        self.notify(service::method::HOST_CALL, &params);
    }

    /// 广播活动提供者的变化（`host:service.changed`）。进程不在跑时由写端静默丢弃。
    pub(crate) fn notify_service_changed(&self, params: &service::ChangedParams) {
        self.notify(service::method::HOST_CHANGED, params);
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

    /// 启动截止检查：令牌已被取代 / 已完成启动 / 已不在运行态时都不动作。
    /// 进程插件以握手完成为准；动态库插件以实例装入为准（`create` 里还包含 `on_ready`）。
    fn check_handshake(self: &Arc<Self>, token: u64) {
        if token != self.run_token.load(Ordering::SeqCst)
            || self.status() != PluginStatus::Running
        {
            return;
        }
        let (ready, what) = if self.is_abi() {
            (self.abi.ready(), "启动")
        } else {
            (self.handshaken.load(Ordering::SeqCst), "握手")
        };
        if ready {
            return;
        }
        self.fail(token, format!("插件未在 {} 秒内完成{what}", HANDSHAKE_TIMEOUT.as_secs()));
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
                self.fail(token, format!("插件输出非协议内容（stdout 只允许 JSON-RPC 信封）：{e}"));
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
        self.record_audit(method, &request.params);
        // `service.call` 是「名字要到运行时才知道」的信封：先按目录定落点，内置提供者把方法名
        // 规范化后与直接调用同名方法走同一条分派，外部提供者转发。
        if method == service::method::CALL {
            self.handle_service_call(token, id, &request.params);
            return;
        }
        self.route_request(token, id, method, &request.params);
    }

    /// 把一条请求落到实现：对话框 / 界面 / 壳侧服务分流为异步应答；活动提供者是外部插件的
    /// 普通方法转发出去；其余同步分派。
    fn route_request(self: &Arc<Self>, token: u64, id: u64, method: &str, params: &Value) {
        // 普通方法先过目录：外部提供者顶替某服务后，直接调 `vault.readText` 的老插件同样被
        // 路由到它；没有顶替则维持本地内置实现。转发判定必须早于壳侧分流——被顶替的方法
        // 本来就是壳侧实现的那些。
        if let Some((service_name, method_name)) = PluginRuntime::split_service(method) {
            if let Ok(runtime) = self.runtime_upgrade() {
                match runtime.forward_to(service_name, method_name) {
                    Forward::Local => {}
                    Forward::Undeclared => {
                        let error = PluginRuntime::undeclared_service_method(service_name, method_name);
                        self.respond(id, None, Some(error));
                        return;
                    }
                    Forward::Unavailable => {
                        let error = PluginRuntime::unavailable_service(service_name);
                        self.respond(id, None, Some(error));
                        return;
                    }
                    Forward::Plugin(provider) => {
                        runtime.forward_service_call(
                            self,
                            id,
                            &provider,
                            service_name,
                            method_name,
                            params.clone(),
                        );
                        return;
                    }
                }
            }
        }
        // 对话框由用户决定耗时（无上限），不能在读线程上等：分流为异步应答（结果由回填线程写回），
        // 读线程继续消费插件输出——否则对话框期间插件输出与退出上报都被拖住。
        // （http 有明确超时上限，按顺序处理不破「读线程串行处理请求」这条既有语义。）
        if method == method::dialog::PICK_FILE || method == method::dialog::PICK_DIRECTORY {
            self.handle_dialog_request(token, id, method, params);
            return;
        }
        // 接管与插件窗口的落地在壳（OS 窗口副作用只由壳触发），结果由壳回填——分流为异步应答，
        // 读线程不等待窗口创建与接管应用。
        if is_ui_method(method) {
            self.handle_ui_request(token, id, method, params);
            return;
        }
        // 壳侧执行的服务调用（对话编排、剪贴板、仓库读写、托盘、协作、表格、文字量算等）
        // 分流为异步应答：结果由回填线程写回，读线程继续消费插件输出。
        if is_shell_method(method) {
            self.handle_shell_request(token, id, method, params);
            return;
        }
        match self.dispatch_for_session(token, method, params) {
            Ok(value) => self.respond(id, Some(value), None),
            Err(error) => self.respond(id, None, Some(error)),
        }
    }

    /// 一次 `service.call`：先校验服务 / 方法与活动提供者的声明，再用。内置提供者按规范化后的
    /// 完整方法名重走 [`Instance::route_request`]（与直接调用同名方法同一条分支，不复制实现）。
    fn handle_service_call(self: &Arc<Self>, token: u64, id: u64, params: &Value) {
        let call: CallParams = match parse(params) {
            Ok(call) => call,
            Err(error) => {
                self.respond(id, None, Some(error));
                return;
            }
        };
        let runtime = match self.runtime_upgrade() {
            Ok(runtime) => runtime,
            Err(error) => {
                self.respond(id, None, Some(error));
                return;
            }
        };
        match runtime.service_target(call) {
            Ok(ServiceTarget::Builtin { wire, params }) => self.route_request(token, id, &wire, &params),
            Ok(ServiceTarget::Plugin { provider, service, method, params }) => {
                runtime.forward_service_call(self, id, &provider, &service, &method, params);
            }
            Err(error) => self.respond(id, None, Some(error)),
        }
    }

    /// 打开一次对话框并在独立线程上等结果回执。
    fn handle_dialog_request(
        self: &Arc<Self>,
        token: u64,
        id: u64,
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
        id: u64,
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
                method::window::MINIMIZE => {
                    let p: method::WindowIdParams = parse(params).map_err(|e| e.to_string())?;
                    runtime.minimize_plugin_window(&self.id, &p.id)
                }
                method::window::TOGGLE_MAXIMIZE => {
                    let p: method::WindowIdParams = parse(params).map_err(|e| e.to_string())?;
                    runtime.toggle_maximize_plugin_window(&self.id, &p.id)
                }
                method::window::BEGIN_DRAG => {
                    let p: method::WindowDragParams = parse(params).map_err(|e| e.to_string())?;
                    runtime.begin_window_drag(&self.id, &p.surface_id)
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
        id: u64,
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
                method::ai::MODELS => HostRequest::AiModels {
                    plugin_id: self.id.clone(),
                    request_id: 0,
                },
                method::ai::AGENTS => HostRequest::AiAgents {
                    plugin_id: self.id.clone(),
                    request_id: 0,
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
                method::clipboard::WRITE_IMAGE => {
                    let p: method::ClipboardWriteImageParams =
                        parse(params).map_err(|e| e.to_string())?;
                    let bytes = decode_clipboard_image(&p.base64)?;
                    HostRequest::ClipboardWriteImage {
                        plugin_id: self.id.clone(),
                        request_id: 0,
                        bytes,
                    }
                }
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
                method::vault::REMOVE => {
                    let p: method::PathParams = parse(params).map_err(|e| e.to_string())?;
                    HostRequest::VaultRemove {
                        plugin_id: self.id.clone(),
                        request_id: 0,
                        path: p.path,
                    }
                }
                method::vault::RENAME => {
                    let p: method::VaultRenameParams = parse(params).map_err(|e| e.to_string())?;
                    HostRequest::VaultRename {
                        plugin_id: self.id.clone(),
                        request_id: 0,
                        from: p.from,
                        to: p.to,
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
                method::collab::META_GET => {
                    let p: method::CollabMetaGetParams = parse(params).map_err(|e| e.to_string())?;
                    p.validate()?;
                    HostRequest::CollabMetaGet {
                        plugin_id: self.id.clone(),
                        request_id: 0,
                        params: p,
                    }
                }
                method::collab::META_PATCH => {
                    let p: method::CollabMetaPatchParams =
                        parse(params).map_err(|e| e.to_string())?;
                    p.validate()?;
                    HostRequest::CollabMetaPatch {
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
                method::query::MEASURE => {
                    let p: method::MeasureBatchParams = parse(params).map_err(|e| e.to_string())?;
                    p.validate()?;
                    HostRequest::MeasureBatch {
                        plugin_id: self.id.clone(),
                        request_id: 0,
                        params: p,
                    }
                }
                method::query::THEME => HostRequest::QueryTheme {
                    plugin_id: self.id.clone(),
                    request_id: 0,
                },
                method::editor::SELECTION => HostRequest::EditorSelection {
                    plugin_id: self.id.clone(),
                    request_id: 0,
                },
                method::editor::REPLACE_SELECTION => {
                    let p: method::EditorReplaceParams = parse(params).map_err(|e| e.to_string())?;
                    p.validate()?;
                    HostRequest::EditorReplaceSelection {
                        plugin_id: self.id.clone(),
                        request_id: 0,
                        text: p.text,
                    }
                }
                method::history::ACTIVITY => HostRequest::HistoryActivity {
                    plugin_id: self.id.clone(),
                    request_id: 0,
                },
                method::history::DAY => {
                    let p: method::HistoryDayParams = parse(params).map_err(|e| e.to_string())?;
                    p.validate()?;
                    HostRequest::HistoryDay {
                        plugin_id: self.id.clone(),
                        request_id: 0,
                        params: p,
                    }
                }
                method::history::OPEN => {
                    let p: method::HistoryOpenParams = parse(params).map_err(|e| e.to_string())?;
                    p.validate()?;
                    HostRequest::HistoryOpen {
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
        method_name: &str,
        params: &Value,
    ) -> Result<Value, RpcError> {
        match method_name {
            // ---------- 能力总线 ----------
            // 登记 / 撤销走会话闸门：与其它资源获取同口径，收尾跑完之后到达的登记不得复活服务。
            service::method::REGISTER => {
                let p: service::RegisterParams = parse(params)?;
                let runtime = self.runtime_upgrade()?;
                self.with_session(token, || runtime.register_service(&self.id, p))
                    .map_err(RpcError::failed)?;
                Ok(json!({}))
            }
            service::method::UNREGISTER => {
                let p: service::UnregisterParams = parse(params)?;
                let runtime = self.runtime_upgrade()?;
                self.with_session(token, || runtime.unregister_service(&self.id, &p.service))
                    .map_err(RpcError::failed)?;
                Ok(json!({}))
            }
            service::method::LIST => {
                let runtime = self.runtime_upgrade()?;
                serde_json::to_value(runtime.service_list())
                    .map_err(|e| RpcError::failed(format!("应答序列化失败：{e}")))
            }
            service::method::RESULT => {
                let p: ResultParams = parse(params)?;
                self.runtime_upgrade()?
                    .resolve_service_call(&self.id, p)
                    .map_err(RpcError::failed)?;
                Ok(json!({}))
            }
            _ => self.dispatch_builtin(token, method_name, params),
        }
    }

    /// 按方法名落到内置实现（与 `service.call` 规范化后的名字共用这一条分派）。
    fn dispatch_builtin(
        self: &Arc<Self>,
        token: u64,
        name: &str,
        params: &Value,
    ) -> Result<Value, RpcError> {
        match name {
            handshake::INITIALIZE => self.on_initialize(token, params),
            event::SUBSCRIBE => self.on_subscribe(params),
            method::view::REGISTER => self.on_register(token, params),
            method::view::UNREGISTER => self.on_unregister(token, params),
            method::frame::SUBMIT_SHARED => self.on_frame_submit(token, params),
            method::frame::REGION => self.on_frame_region(token, params),
            method::markdown::PARSE => {
                let p: method::MarkdownParams = parse(params)?;
                p.validate().map_err(RpcError::failed)?;
                let result = super::markdown::blocks(&p.content);
                serde_json::to_value(result).map_err(|e| RpcError::failed(e.to_string()))
            }
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
            // 壳侧服务方法不在这里执行（`handle_request` 已分流到异步应答）；走到这里说明分流
            // 名单与分派名单失配，按可读失败收场而不是当成未知方法
            other if is_shell_method(other) || is_ui_method(other) => {
                Err(RpcError::failed("宿主服务请求须走异步应答路径"))
            }
            // 已声明但尚未实现的能力面：以可读失败收场，不静默成功
            other if capability::permission_for(other).is_some() => {
                Err(RpcError::failed(format!("能力面尚未实现：{other}")))
            }
            other => Err(RpcError::failed(format!("未知方法：{other}"))),
        }
    }

    fn on_initialize(self: &Arc<Self>, token: u64, params: &Value) -> Result<Value, RpcError> {
        let p: InitializeParams = parse(params)?;
        if !handshake::compatible(p.api_version, API_VERSION) {
            let reason = format!("协议版本不匹配：插件 {}，宿主 {API_VERSION}", p.api_version);
            if self.is_abi() {
                self.log.line(&format!("[失败] {reason}"));
            } else {
                self.fail(token, reason);
            }
            return Err(RpcError::failed("协议版本不匹配"));
        }
        if p.plugin.id != self.id {
            let reason = format!("插件自报 id {} 与安装 id {} 不一致", p.plugin.id, self.id);
            if self.is_abi() {
                self.log.line(&format!("[失败] {reason}"));
            } else {
                self.fail(token, reason);
            }
            return Err(RpcError::failed("插件自报 id 与安装 id 不一致"));
        }
        self.handshaken.store(true, Ordering::SeqCst);
        self.log.line(&format!(
            "[握手] 完成（插件 {} v{}，apiVersion {}）",
            p.plugin.name, p.plugin.version, p.api_version
        ));
        // 同进程插件整帧走 typed 回调，不建共享内存区域；进程插件仍按原通路建区。
        let region = if self.is_abi() {
            FrameRegionInfo { path: String::new(), generation: 0, slot_count: 0, slot_capacity: 0 }
        } else {
            // 建区域与收尾同在资源闸门内：迟到的握手不得在收尾之后留下无人回收的映射与临时文件
            match self.with_session(token, || Ok::<_, String>(self.frame_region.ensure())) {
                Ok(Ok(info)) => info,
                Ok(Err(reason)) => {
                    let message = format!("帧共享内存准备失败：{reason}");
                    self.fail(token, message);
                    return Err(RpcError::failed("帧共享内存准备失败"));
                }
                Err(reason) => return Err(RpcError::failed(reason)),
            }
        };
        let result = InitializeResult {
            api_version: API_VERSION,
            host: HostInfo {
                name: HOST_NAME.to_string(),
                version: self.host_version.clone(),
            },
            frame_region: region,
        };
        serde_json::to_value(result).map_err(|e| RpcError::failed(format!("握手应答序列化失败：{e}")))
    }

    fn on_subscribe(&self, params: &Value) -> Result<Value, RpcError> {
        let p: event::SubscribeParams = parse(params)?;
        if !matches!(
            p.name.as_str(),
            event::name::VAULT_SWITCH
                | event::name::TABLE_CHANGED
                | event::name::TABLE_SELECTION_CHANGED
                | event::name::AI_CHAT_CHANGED
                | event::name::VAULT_FILE_CHANGED
                | event::name::SPACE_META_CHANGED
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
        // 晚于切换的订阅者（切容器后被重载的新进程、切换后才启动的插件）靠补发得知当前容器身份
        if p.name == event::name::VAULT_SWITCH {
            if let Ok(runtime) = self.runtime_upgrade() {
                if let Some(payload) = runtime.current_vault_switch() {
                    self.notify_domain_event(event::name::VAULT_SWITCH, &payload);
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

    /// 从共享内存取帧并接收：内容解码、指令 / 命中区 / 帧率一并校验后整发整换。
    /// 存档与会话闸门同口径——收尾后到达的提交不得让已停插件的界面继续存活。
    ///
    /// 序号对不上（写入被后续提交覆盖 / 读取时正在写）时整帧丢弃并回 `applied: false`：
    /// 这是并发路径的正常结果，不是插件错误，是否重发由插件按下一帧自行决定。
    fn on_frame_submit(self: &Arc<Self>, token: u64, params: &Value) -> Result<Value, RpcError> {
        let p: frame::FrameSubmitSharedParams = parse(params)?;
        if p.surface_id.trim().is_empty() {
            return Err(RpcError::failed("surfaceId 不能为空"));
        }
        // 拷贝在会话闸门内（映射可能被收尾或扩容换掉），解码与校验在闸门外完成
        let bytes = self
            .with_session(token, || self.frame_region.read(&p))
            .map_err(RpcError::failed)?;
        let Some(bytes) = bytes else {
            self.log_host_note(&format!(
                "丢弃错位帧（面 {}，槽 {}，序号 {}）",
                p.surface_id, p.slot, p.seq
            ));
            return Ok(json!({ "applied": false }));
        };
        let frame = frame_codec::decode(&bytes)
            .map_err(|e| RpcError::failed(format!("帧解码失败：{e}")))?;
        self.accept_frame(token, p.surface_id, frame)?;
        Ok(json!({ "applied": true }))
    }

    /// 接收一帧（共享内存与 ABI 两个来源共用）：校验后整发整换并推进帧钟。
    fn accept_frame(
        self: &Arc<Self>,
        token: u64,
        surface_id: String,
        frame: frame::SurfaceFrame,
    ) -> Result<(), RpcError> {
        if surface_id.trim().is_empty() {
            return Err(RpcError::failed("surfaceId 不能为空"));
        }
        frame
            .validate()
            .map_err(|e| RpcError::failed(format!("帧内容不合法：{e}")))?;
        let submitted = Arc::new(frame);
        let claimed = self
            .with_session(token, || {
                let mut clocks = self.surface_clocks.lock().unwrap();
                // 帧率未变的面沿用原钟：帧序号与节拍相位不因内容更新而重置
                match submitted.fps {
                    Some(fps) => {
                        let clock = match clocks.get(&surface_id) {
                            Some(clock) if clock.fps == fps => *clock,
                            _ => FrameClock::new(fps, Instant::now()),
                        };
                        clocks.insert(surface_id.clone(), clock);
                    }
                    None => {
                        clocks.remove(&surface_id);
                    }
                }
                drop(clocks);
                let mut store = self.frames.lock().unwrap();
                store.frames.insert(surface_id.clone(), submitted);
                // 有绘制期等帧登记：这一帧由它取走画上，不再发通知（发一条就是一次整窗重建）
                let claimed = store.waiting.contains_key(&surface_id);
                drop(store);
                self.frame_signal.notify_all();
                self.submits.fetch_add(1, Ordering::Relaxed);
                Ok(claimed)
            })
            .map_err(RpcError::failed)?;
        self.resize_acked(&surface_id);
        if !claimed {
            let _ = self.events.send(PluginEvent::ViewSubmitted { id: self.id.clone() });
        }
        Ok(())
    }

    /// ABI 回调送来的帧：复制已在宿主库完成，这里只做协议投影与校验。
    pub(crate) fn accept_abi_frame(self: &Arc<Self>, token: u64, surface: &str, frame: atelyx_plugin_host::Frame) {
        if token != self.run_token.load(Ordering::SeqCst)
            || !self.session_active.load(Ordering::SeqCst)
        {
            return;
        }
        if let Err(error) = self.accept_frame(token, surface.to_string(), abi::frame_from_host(frame)) {
            self.log_host_note(&format!("丢弃插件帧：{error}"));
        }
    }

    pub(crate) fn request_redraw_abi(&self, token: u64) {
        if token != self.run_token.load(Ordering::SeqCst) {
            return;
        }
        let _ = self.events.send(PluginEvent::ViewSubmitted { id: self.id.clone() });
    }

    pub(crate) fn log_abi(&self, token: u64, level: u32, message: &str) {
        // 与进程插件同一口径：换代后的迟到输出一律丢弃
        if token != self.run_token.load(Ordering::SeqCst) {
            return;
        }
        let prefix = if level == atelyx_plugin_abi::LOG_ERROR { "[插件错误]" } else { "[插件]" };
        self.log.line(&format!("{prefix} {message}"));
    }

    pub(crate) fn subscribe_abi(&self, token: u64, name: &str) {
        let result = self.with_session(token, || {
            self.on_subscribe(&json!({ "name": name })).map_err(|error| error.to_string())
        });
        if let Err(reason) = result {
            self.log_host_note(&format!("订阅 {name} 失败：{reason}"));
        }
    }

    /// 插件 → 宿主的通用调用：落点与进程插件完全相同（权限 / 服务目录 / 壳回填）。
    pub(crate) fn handle_abi_call(self: &Arc<Self>, token: u64, call: HostCall) {
        if token != self.run_token.load(Ordering::SeqCst)
            || !self.session_active.load(Ordering::SeqCst)
        {
            call.reply(false, "插件会话已结束");
            return;
        }
        let params = if call.params.trim().is_empty() {
            Value::Null
        } else {
            match serde_json::from_str(&call.params) {
                Ok(params) => params,
                Err(error) => {
                    call.reply(false, &format!("params 不是合法 JSON：{error}"));
                    return;
                }
            }
        };
        let id = call.request_id;
        let method = call.method.clone();
        if !self.abi.register_call(call) {
            return;
        }
        let handling = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.handle_request(
                token,
                Request { jsonrpc: Envelope::JSONRPC, id, method, params },
            );
        }));
        if handling.is_err() {
            self.log_host_note("宿主处理插件调用时 panic，已按失败回执");
            self.respond(id, None, Some(RpcError::failed("宿主内部错误")));
        }
    }

    /// 插件回报一次服务调用；登记时分配的 ABI 序号换回运行时 callId。
    pub(crate) fn reply_abi_service(&self, token: u64, call_id: u64, ok: bool, payload: &str) {
        if token != self.run_token.load(Ordering::SeqCst) {
            return;
        }
        let Some(call_id) = self.abi.finish_service_call(call_id) else {
            self.log_host_note(&format!("服务回执已收场（序号 {call_id}）"));
            return;
        };
        let (result, error) = if ok {
            if payload.trim().is_empty() {
                (Some(Value::Null), None)
            } else {
                match serde_json::from_str::<Value>(payload) {
                    Ok(value) => (Some(value), None),
                    Err(error) => (None, Some(format!("回执不是合法 JSON：{error}"))),
                }
            }
        } else {
            (None, Some(payload.to_string()))
        };
        let Ok(runtime) = self.runtime_upgrade() else { return };
        if let Err(error) = runtime.resolve_service_call(
            &self.id,
            ResultParams { call_id, result, error },
        ) {
            self.log_host_note(&format!("送达服务回执失败：{error}"));
        }
    }

    /// 让服务调用在调用方侧以可读失败结束（插件已停 / 转投时实例已收场）。
    fn fail_service_call(&self, call_id: &str, error: &str) {
        let Ok(runtime) = self.runtime_upgrade() else { return };
        if let Err(error) = runtime.resolve_service_call(
            &self.id,
            ResultParams {
                call_id: call_id.to_string(),
                result: None,
                error: Some(error.to_string()),
            },
        ) {
            self.log_host_note(&format!("结束服务调用失败：{error}"));
        }
    }

    /// 帧超出当前槽容量时换一份更大的区域；容量已够则原样返回（幂等）。
    fn on_frame_region(&self, token: u64, params: &Value) -> Result<Value, RpcError> {
        let p: frame::FrameRegionRequest = parse(params)?;
        let info = self
            .with_session(token, || self.frame_region.grow(p.min_bytes))
            .map_err(RpcError::failed)?;
        serde_json::to_value(info).map_err(|e| RpcError::failed(format!("区域描述序列化失败：{e}")))
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


    fn on_unregister(self: &Arc<Self>, token: u64, params: &Value) -> Result<Value, RpcError> {
        let p: UnregisterParams = parse(params)?;
        self.with_session(token, || {
            let mut contributions = self.contributions.lock().unwrap();
            let before = contributions.len();
            contributions.retain(|entry| entry.id != p.id);
            if contributions.len() == before {
                return Err(format!("未注册的贡献 id：{}", p.id));
            }
            {
                let mut store = self.frames.lock().unwrap();
                store.frames.remove(&p.id);
                store.waiting.remove(&p.id);
            }
            self.surface_clocks.lock().unwrap().remove(&p.id);
            self.frame_signal.notify_all();
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

    /// 转投一条同空间对等插件的消息（`host:collab.message`）。通道名由发送方填写；
    /// 这是宿主下发，权限只做公示不拦截，故不记入调用审计。
    pub(crate) fn notify_collab_message(&self, params: &method::CollabMessageParams) {
        self.notify(method::collab::HOST_MESSAGE, params);
    }

    pub(crate) fn notify_frame_input(self: &Arc<Self>, surface_id: &str, input: &frame::FrameInput) {
        let frame::FrameInput::Resize { width, height } = input else {
            // 其它输入前先把积压的最新尺寸发出去：插件按新尺寸命中，不会拿旧尺寸处理新坐标
            self.flush_pending_resize(surface_id);
            self.send_frame_input(surface_id, input);
            return;
        };
        self.on_resize(surface_id, *width, *height);
    }

    /// 把待发尺寸立即发出（不再等新帧回执或兜底定时器）。
    fn flush_pending_resize(self: &Arc<Self>, surface_id: &str) {
        let mut gates = self.resize_gates.lock().unwrap();
        let Some(gate) = gates.get_mut(surface_id) else {
            return;
        };
        if gate.pending.is_some() {
            self.dispatch_resize(surface_id, gate);
        }
    }

    fn send_frame_input(&self, surface_id: &str, input: &frame::FrameInput) {
        if self.is_abi() {
            self.abi.send_input(surface_id, input);
            return;
        }
        let params = frame::FrameInputParams {
            surface_id: surface_id.to_string(),
            input: input.clone(),
        };
        self.notify(method::frame::HOST_INPUT, &params);
    }

    /// 尺寸变更合流：只保留最新一笔，在途回执回来之前不再逐条下发（拖动中的中间尺寸没有意义）。
    fn on_resize(self: &Arc<Self>, surface_id: &str, width: f32, height: f32) {
        let mut gates = self.resize_gates.lock().unwrap();
        let gate = gates.entry(surface_id.to_string()).or_default();
        gate.pending = Some((width, height));
        if gate.in_flight.is_none() {
            self.dispatch_resize(surface_id, gate);
        }
    }

    /// 发最新待发尺寸并挂兜底：新帧回执到来就提前放行下一笔，回执不来则到点重发。
    fn dispatch_resize(self: &Arc<Self>, surface_id: &str, gate: &mut ResizeGate) {
        let Some((width, height)) = gate.pending.take() else {
            return;
        };
        gate.in_flight = Some(Instant::now());
        let start_timer = !gate.timer_active;
        gate.timer_active = true;
        self.send_frame_input(surface_id, &frame::FrameInput::Resize { width, height });
        if start_timer {
            self.spawn_resize_timer(surface_id.to_string());
        }
    }

    /// 每个面至多一条兜底线程：有在途尺寸时每 150 ms 看一次，闸门空下来即退出。
    fn spawn_resize_timer(self: &Arc<Self>, surface_id: String) {
        let weak = Arc::downgrade(self);
        std::thread::spawn(move || loop {
            std::thread::sleep(RESIZE_FALLBACK);
            let Some(instance) = weak.upgrade() else {
                return;
            };
            if !instance.resize_fallback(&surface_id) {
                return;
            }
        });
    }

    /// 兜底一拍：同一笔在途尺寸未回帧时把最新待发尺寸补发出去。返回是否需要继续等。
    fn resize_fallback(self: &Arc<Self>, surface_id: &str) -> bool {
        let mut gates = self.resize_gates.lock().unwrap();
        let Some(gate) = gates.get_mut(surface_id) else {
            return false;
        };
        let Some(sent_at) = gate.in_flight else {
            gate.timer_active = false;
            return false;
        };
        if sent_at.elapsed() >= RESIZE_GIVE_UP {
            // 该面一直不回新帧：最后一笔尺寸照发，闸门撤掉，下次尺寸变化重新开闸
            let last = gate.pending.take();
            gates.remove(surface_id);
            drop(gates);
            if let Some((width, height)) = last {
                self.send_frame_input(surface_id, &frame::FrameInput::Resize { width, height });
            }
            return false;
        }
        if gate.pending.is_some() {
            self.dispatch_resize(surface_id, gate);
        }
        true
    }

    /// 新帧落地即是尺寸的确认：在途那笔结束，有更新的尺寸就立刻发下一笔。
    fn resize_acked(self: &Arc<Self>, surface_id: &str) {
        let mut gates = self.resize_gates.lock().unwrap();
        let Some(gate) = gates.get_mut(surface_id) else {
            return;
        };
        gate.in_flight = None;
        self.dispatch_resize(surface_id, gate);
    }

    /// 是否有面在收帧（壳据此决定要不要挂帧节拍：没有动画时不产生周期唤醒）。
    pub(crate) fn has_frame_ticks(&self) -> bool {
        !self.surface_clocks.lock().unwrap().is_empty()
    }

    /// 推一次帧节拍：到期的整面帧发 `host:frame.tick`。
    pub(crate) fn pump_frames(&self, now: Instant) {
        let due: Vec<(String, u64, u32)> = {
            let mut clocks = self.surface_clocks.lock().unwrap();
            let mut due = Vec::new();
            for (surface_id, clock) in clocks.iter_mut() {
                if let Some((frame, delta_ms)) = clock.take_due(now) {
                    due.push((surface_id.clone(), frame, delta_ms));
                }
            }
            due
        };
        for (surface_id, frame, delta_ms) in due {
            if self.is_abi() {
                self.abi.send_tick(&surface_id, frame, delta_ms);
                continue;
            }
            let params = frame::FrameTickParams { surface_id, frame, delta_ms };
            self.notify(method::frame::HOST_TICK, &params);
        }
    }

    /// 告知插件某窗口因失焦被收起（`host:window.hidden`）。
    pub(crate) fn notify_window_hidden(&self, window_id: &str) {
        let params = method::WindowIdParams { id: window_id.to_string() };
        self.notify(method::window::HOST_HIDDEN, &params);
    }

    /// 告知插件某个面已挂载（`host:surface.attached`）。
    pub(crate) fn notify_surface_attached(&self, surface_id: &str, kind: SurfaceKind) {
        if self.is_abi() {
            self.abi.attach(surface_id, surface_kind_code(kind));
            return;
        }
        let params = SurfaceAttachedParams { surface_id: surface_id.to_string(), kind };
        self.notify(method::view::HOST_ATTACHED, &params);
    }

    /// 告知插件某个面已移除（`host:surface.detached`）。
    pub(crate) fn notify_surface_detached(&self, surface_id: &str) {
        if self.is_abi() {
            self.abi.detach(surface_id);
            return;
        }
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

    /// 记录一条能力调用。声明与否都执行——记录只提供知情：设置页据此区分「声明能力」与
    /// 「实际用到但未声明」。未声明的首次出现额外广播一次（同一方法不重复打扰）。
    fn record_audit(&self, method_name: &str, params: &Value) {
        let (name, target) = audit_subject(method_name, params);
        let permission = capability::permission_for(&name);
        // 只有占权限类的方法与能力总线入口进审计：界面 / 订阅这类时序方法不触碰宿主资源
        if permission.is_none() && method_name != service::method::CALL {
            return;
        }
        let declared = permission.map_or(true, |p| self.manifest.permissions.contains(&p));
        let detail = target
            .and_then(|inner| inner.get("path"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let first_undeclared = {
            let mut audits = self.audits.lock().unwrap();
            audits.push(AuditRecord {
                method: name.clone(),
                detail: detail.clone(),
                at: now_secs(),
                declared,
            });
            if audits.len() > MAX_AUDIT {
                audits.remove(0);
            }
            if declared {
                false
            } else {
                self.undeclared_calls.fetch_add(1, Ordering::Relaxed);
                // 去重表独立于审计列表：列表按条数截断，被挤掉的旧记录不该让提示重发
                self.undeclared_methods.lock().unwrap().insert(name.clone())
            }
        };
        if first_undeclared {
            let _ = self.events.send(PluginEvent::PermissionDenied {
                id: self.id.clone(),
                method: name,
                detail,
            });
        }
    }

    /// 回执一条响应（写往插件 stdin）。
    /// 本会话的运行令牌（每次启停递增；跨会话的迟到结果据此丢弃）。
    pub(crate) fn run_token(&self) -> u64 {
        self.run_token.load(Ordering::SeqCst)
    }

    /// 只在发起时的那次会话仍有效时回执：插件重启后同 id 的旧应答不得落进新会话。
    pub(crate) fn respond_if_current(
        &self,
        token: u64,
        id: u64,
        result: Option<Value>,
        error: Option<RpcError>,
    ) {
        if self.run_token() != token || !self.session_active.load(Ordering::SeqCst) {
            return;
        }
        self.respond(id, result, error);
    }

    pub(crate) fn respond(&self, id: u64, result: Option<Value>, error: Option<RpcError>) {
        if self.is_abi() {
            let (ok, payload) = match (result, error) {
                (Some(result), None) => {
                    let payload = serde_json::to_string(&result).unwrap_or_else(|_| "null".into());
                    (true, payload)
                }
                (_, Some(error)) => (false, error.message),
                (None, None) => (true, "null".into()),
            };
            let _ = self.abi.reply(id, ok, &payload);
            return;
        }
        let (envelope, what) = match (result, error) {
            (Some(result), None) => (Envelope::success(id, result), "回执"),
            (_, Some(error)) => (Envelope::failure(id, error), "错误回执"),
            (None, None) => (Envelope::success(id, Value::Null), "回执"),
        };
        self.write_envelope(envelope, what);
    }

    /// 向插件下发一条信封：进程写 stdin（已停时静默丢弃），动态库走 typed 通知。
    /// 写失败按限频记入插件日志。
    fn write_envelope(&self, envelope: Envelope, what: &str) {
        if self.is_abi() {
            if let Envelope::Notification(notification) = envelope {
                let payload = serde_json::to_string(&notification.params)
                    .unwrap_or_else(|_| "null".into());
                self.abi.notify(&notification.method, &payload);
            }
            return;
        }
        let pid = self.pid.load(Ordering::SeqCst) as u32;
        if pid == 0 {
            return;
        }
        let mut line = envelope.to_line();
        line.push('\n');
        if let Err(e) = self.process_host.send_stdin(pid, &line) {
            self.note_write_failure(what, &e);
        }
    }

    fn note_write_failure(&self, what: &str, reason: &str) {
        let mut last = self.last_write_error.lock().unwrap();
        if last.is_some_and(|at| at.elapsed() < WRITE_ERROR_NOTE_INTERVAL) {
            return;
        }
        *last = Some(Instant::now());
        drop(last);
        self.log_host_note(&format!("{what}未送达：{reason}"));
    }
}

/// `process.spawn` 的 pid 槽位：pid 定下前到达的子进程事件先按序暂存，定下后补齐（见 `spawn_child`）。
#[derive(Default)]
struct SpawnSlot {
    pid: Option<u32>,
    pending: Vec<HostEvent>,
}

/// 走壳侧异步应答的方法（对话编排 / 剪贴板 / 仓库读写 / 表格 / 托盘 / 协作 / 滚动 / 文字量算）。
///
/// 分流判定与「不得同步分派」的防御共用这一份清单：两处各写一遍时，新增方法只改一处会
/// 落进「未知方法」——功能静默不可用，编译与测试都发现不了。
fn is_shell_method(method_name: &str) -> bool {
    matches!(
        method_name,
        method::ai::CHAT_SEND
            | method::ai::CHAT_STOP
            | method::ai::CHAT_HISTORY
            | method::ai::CHAT_RENAME
            | method::ai::CHAT_DELETE
            | method::ai::CHAT_COMPACT
            | method::ai::MODELS
            | method::ai::AGENTS
            | method::clipboard::WRITE_TEXT
            | method::clipboard::READ_TEXT
            | method::clipboard::READ_IMAGE
            | method::clipboard::WRITE_IMAGE
            | method::vault::READ_TEXT
            | method::vault::WRITE_TEXT
            | method::vault::READ_BINARY
            | method::vault::WRITE_BINARY
            | method::vault::IMPORT_FILE
            | method::vault::IMAGE_INFO
            | method::vault::LIST
            | method::vault::EXISTS
            | method::vault::REMOVE
            | method::vault::RENAME
            | method::table::SNAPSHOT
            | method::table::UPDATE_CELL
            | method::table::ADD_ROW
            | method::table::REMOVE_ROW
            | method::table::SELECT_ROW
            | method::table::SET_CELL_IMAGE
            | method::tray::SET_ITEMS
            | method::collab::PEERS
            | method::collab::SEND
            | method::collab::META_GET
            | method::collab::META_PATCH
            | method::query::MEASURE
            | method::query::THEME
            | method::editor::SELECTION
            | method::editor::REPLACE_SELECTION
            | method::history::ACTIVITY
            | method::history::DAY
            | method::history::OPEN
    )
}

/// 走壳侧界面服务的方法（接管 / 插件窗口）：落地依赖 OS 窗口副作用，只由壳执行。
fn is_ui_method(method_name: &str) -> bool {
    matches!(
        method_name,
        method::view::TAKEOVER
            | method::window::OPEN
            | method::window::CLOSE
            | method::window::UPDATE
            | method::window::SHOW
            | method::window::MINIMIZE
            | method::window::TOGGLE_MAXIMIZE
            | method::window::BEGIN_DRAG
    )
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

/// 解码剪贴板图片负载并判上限。超限即拒绝，不缩放也不截断。
fn decode_clipboard_image(base64_text: &str) -> Result<Vec<u8>, String> {
    let too_large = || {
        format!(
            "图片超过单次载荷上限（{} MiB）",
            MAX_CLIPBOARD_IMAGE_BYTES / (1024 * 1024)
        )
    };
    // base64 每 4 字符编码 3 字节：先按编码长度挡在解码分配之前
    if base64_text.len() > MAX_CLIPBOARD_IMAGE_BYTES / 3 * 4 + 4 {
        return Err(too_large());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(base64_text)
        .map_err(|e| format!("base64 解码失败：{e}"))?;
    if bytes.len() > MAX_CLIPBOARD_IMAGE_BYTES {
        return Err(too_large());
    }
    Ok(bytes)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 审计对象：`service.call` 记规范化后的完整方法名，并给出目标方法自己的入参（取路径用）。
fn audit_subject<'a>(method_name: &str, params: &'a Value) -> (String, Option<&'a Value>) {
    if method_name == service::method::CALL {
        let service_name = params.get("service").and_then(Value::as_str).unwrap_or_default();
        let target = params.get("method").and_then(Value::as_str).unwrap_or_default();
        if !service_name.is_empty() && !target.is_empty() {
            return (format!("{service_name}.{target}"), params.get("params"));
        }
    }
    (method_name.to_string(), Some(params))
}

/// 面的类型码（与 `atelyx-plugin-abi` 的 `SURFACE_*` 对齐）。
fn surface_kind_code(kind: SurfaceKind) -> u32 {
    use atelyx_plugin_abi as abi;
    match kind {
        SurfaceKind::Panel => abi::SURFACE_PANEL,
        SurfaceKind::Window => abi::SURFACE_WINDOW,
        SurfaceKind::Page => abi::SURFACE_PAGE,
        SurfaceKind::Takeover => abi::SURFACE_TAKEOVER,
        SurfaceKind::Settings => abi::SURFACE_SETTINGS,
        SurfaceKind::TableView => abi::SURFACE_TABLE_VIEW,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::test_support::TempDir;
    use atelyx_plugin_protocol::frame_codec;
    use atelyx_plugin_protocol::frame_region;
    use atelyx_plugin_protocol::manifest::{Entry, Permission};

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
        make_named_instance_with_events("com.example.demo", permissions)
    }

    /// 同 [`make_instance_with_events`]，但用给定的插件 id（跨插件路由要用两个不同 id 的实例）。
    fn make_named_instance_with_events(
        id: &str,
        permissions: Vec<Permission>,
    ) -> (
        TempDir,
        Arc<Instance>,
        tokio::sync::broadcast::Receiver<PluginEvent>,
    ) {
        let tmp = TempDir::new("plugin-instance");
        let dir = tmp.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let manifest = Manifest {
            id: id.into(),
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
            event::name::TABLE_SELECTION_CHANGED,
            event::name::AI_CHAT_CHANGED,
            event::name::VAULT_FILE_CHANGED,
            event::name::SPACE_META_CHANGED,
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
    fn clipboard_image_decode_rejects_invalid_and_oversized_payloads() {
        use base64::Engine;
        let ok = base64::engine::general_purpose::STANDARD.encode([0u8, 1, 2]);
        assert_eq!(decode_clipboard_image(&ok).unwrap(), vec![0, 1, 2]);
        let err = decode_clipboard_image("不是 base64").expect_err("非法 base64 应拒绝");
        assert!(err.contains("base64"), "{err}");
        // 编码长度超限在解码分配之前就被挡住
        let oversized = "A".repeat(MAX_CLIPBOARD_IMAGE_BYTES / 3 * 4 + 5);
        let err = decode_clipboard_image(&oversized).expect_err("超限应拒绝");
        assert!(err.contains("上限"), "{err}");
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

    /// 未声明不拦执行：调用照常落地，审计记 `declared: false`（设置页据此提示「用到但未声明」）。
    #[test]
    fn undeclared_capability_still_executes_and_is_audited() {
        let (tmp, instance) = make_instance(Vec::new());
        let file = tmp.join("note.txt").to_string_lossy().into_owned();
        instance.handle_line(&request_line(
            method::fs::WRITE_TEXT,
            &json!({ "path": file, "content": "hi" }),
        ));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hi", "未声明也照常执行");
        assert_eq!(instance.undeclared_count(), 1, "未声明要留痕");
        let audit = instance.audit();
        assert_eq!(audit[0].method, method::fs::WRITE_TEXT);
        assert_eq!(audit[0].detail, file);
        assert!(!audit[0].declared, "未声明的调用必须能被认出来");
    }

    /// 声明过的调用同样进审计，但不计入「未声明」计数，也不广播提示。
    #[test]
    fn declared_capability_is_audited_without_a_warning() {
        let (tmp, instance, mut rx) = make_instance_with_events(vec![Permission::Fs]);
        let file = tmp.join("note.txt").to_string_lossy().into_owned();
        instance.handle_line(&request_line(
            method::fs::WRITE_TEXT,
            &json!({ "path": file, "content": "hi" }),
        ));
        let audit = instance.audit();
        assert_eq!(audit[0].method, method::fs::WRITE_TEXT);
        assert!(audit[0].declared);
        assert_eq!(instance.undeclared_count(), 0);
        assert!(rx.try_recv().is_err(), "声明过的调用不产生未声明提示");
    }

    #[test]
    fn undeclared_window_call_is_audited() {
        let (_tmp, instance) = make_instance(Vec::new());
        instance.handle_line(&request_line(
            method::window::OPEN,
            &json!({ "id": "w", "title": "窗口" }),
        ));
        assert_eq!(instance.undeclared_count(), 1, "未声明 window 能力的开窗请求要留痕");
        assert_eq!(instance.audit()[0].method, method::window::OPEN);
        assert!(!instance.audit()[0].declared);
    }

    #[test]
    fn undeclared_ai_calls_are_audited() {
        let (_tmp, instance) = make_instance(Vec::new());
        for (name, params) in [
            (method::ai::CHAT_SEND, json!({ "text": "你好" })),
            (method::ai::CHAT_STOP, json!({ "sessionId": "s1" })),
            (method::ai::CHAT_HISTORY, json!({})),
        ] {
            instance.handle_line(&request_line(name, &params));
        }
        assert_eq!(instance.undeclared_count(), 3, "未声明 ai 能力的三方法都要留痕");
        for record in instance.audit() {
            assert!(record.method.starts_with("ai.chat."));
            assert!(!record.declared);
        }
    }

    /// `service.call` 的审计记规范化后的完整方法名（目标方法自己的权限类决定 `declared`）。
    #[test]
    fn service_call_is_audited_under_its_normalized_name() {
        let tmp = TempDir::new("instance-service-audit");
        let runtime = PluginRuntime::new(tmp.to_path_buf());
        let (_t, instance) = make_instance(Vec::new());
        instance.attach_runtime(&runtime);
        let file = tmp.join("note.txt").to_string_lossy().into_owned();
        std::fs::write(&file, "hello").unwrap();
        instance.handle_line(&request_line(
            service::method::CALL,
            &json!({ "service": "fs", "method": "readText", "params": { "path": file } }),
        ));
        let audit = instance.audit();
        assert_eq!(audit[0].method, "fs.readText", "总线调用按目标方法名记");
        assert_eq!(audit[0].detail, file);
        assert!(!audit[0].declared, "目标方法的权限类未声明");
    }

    /// AI 对话三方法路由到壳并可回填。
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

    /// 各能力面未声明也执行：审计留痕，且不再以 `PERMISSION_DENIED` 收场。
    #[test]
    fn undeclared_calls_of_each_face_are_audited_not_denied() {
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
            // 未声明：照常进入执行路径并留痕（响应写往不存在的插件进程，观测点在审计记录）
            let (_tmp, undeclared) = make_instance(Vec::new());
            undeclared.handle_line(&request_line(method_name, &params));
            assert_eq!(undeclared.undeclared_count(), 1, "{method_name} 未声明要留痕");
            assert!(!undeclared.audit()[0].declared);
            // 已声明：同一条执行路径，只是审计标为已声明
            let (_tmp2, declared) = make_instance(vec![permission]);
            let error = declared.dispatch(method_name, &params).expect_err("入参不成立，应报执行错误");
            assert_ne!(
                error.code,
                RpcError::PERMISSION_DENIED,
                "{method_name} 的执行路径不该出现权限拒绝"
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

    /// `service.call` 的内置路径与直接调用同一条实现；未知服务 / 未知方法都以错误原因常量收场。
    #[test]
    fn service_call_resolves_to_the_builtin_dispatch() {
        let tmp = TempDir::new("instance-service-call");
        let runtime = PluginRuntime::new(tmp.to_path_buf());
        let (_t, instance) = make_instance(Vec::new());
        instance.attach_runtime(&runtime);

        let direct = instance
            .dispatch(method::markdown::PARSE, &json!({ "content": "# A" }))
            .expect("直接调用应成功");
        // 生产路径（`handle_service_call`）就是把目录决议出的内置落点重新交给同一条分派
        let target = runtime
            .service_target(service::CallParams {
                service: "markdown".into(),
                method: "parse".into(),
                params: json!({ "content": "# A" }),
            })
            .expect("内置服务应可决议");
        let ServiceTarget::Builtin { wire, params } = target else {
            panic!("内置实现应落在本地分派");
        };
        let via = instance.dispatch(&wire, &params).expect("总线调用应成功");
        assert_eq!(direct, via, "内置路径与直接调用必须落到同一实现");

        let unknown_service = runtime
            .service_target(service::CallParams {
                service: "nope".into(),
                method: "x".into(),
                params: Value::Null,
            })
            .expect_err("未知服务应失败");
        assert!(unknown_service.message.contains(service::error::UNKNOWN_SERVICE));
        let unknown_method = runtime
            .service_target(service::CallParams {
                service: "markdown".into(),
                method: "nope".into(),
                params: Value::Null,
            })
            .expect_err("未知方法应失败");
        assert!(unknown_method.message.contains(service::error::UNKNOWN_METHOD));
        let bus = runtime
            .service_target(service::CallParams {
                service: "service".into(),
                method: "list".into(),
                params: Value::Null,
            })
            .expect_err("总线命名空间不是服务");
        assert!(bus.message.contains(service::error::UNKNOWN_SERVICE));
    }

    /// 活动提供者是外部插件时普通方法与 `service.call` 都转发；提供者下线即回落内置。
    #[tokio::test]
    async fn external_provider_takes_over_vault_routing() {
        let tmp = TempDir::new("instance-service-forward");
        let runtime = PluginRuntime::new(tmp.to_path_buf());
        let (_t, provider, _provider_events) =
            make_named_instance_with_events("com.example.provider", Vec::new());
        provider.attach_runtime(&runtime);
        provider.set_status(PluginStatus::Running);
        provider.session_active.store(true, Ordering::SeqCst);
        runtime
            .instances
            .lock()
            .unwrap()
            .insert(provider.id.clone(), provider.clone());
        runtime
            .register_service(
                &provider.id,
                service::RegisterParams {
                    service: "vault".into(),
                    version: None,
                    methods: vec!["readText".into()],
                },
            )
            .expect("登记应成功");
        let provider_of = |runtime: &PluginRuntime| {
            runtime
                .service_list()
                .services
                .iter()
                .find(|s| s.service == "vault")
                .and_then(|s| s.provider.clone())
        };
        assert_eq!(provider_of(&runtime), None, "登记不自动顶替内置");
        runtime
            .set_service_provider("vault", Some(&provider.id))
            .expect("切换应成功");
        assert_eq!(provider_of(&runtime).as_deref(), Some(provider.id.as_str()));

        let (_t2, caller, _caller_events) =
            make_named_instance_with_events("com.example.caller", Vec::new());
        caller.attach_runtime(&runtime);
        runtime
            .instances
            .lock()
            .unwrap()
            .insert(caller.id.clone(), caller.clone());

        // 普通方法走转发：在途条目按提供者记账，别家回报不得认领
        caller.handle_line(&request_line(
            method::vault::READ_TEXT,
            &json!({ "path": "notes/a.md" }),
        ));
        let call_id = wait_service_call(&runtime).await;
        assert!(
            runtime
                .resolve_service_call(
                    &caller.id,
                    service::ResultParams {
                        call_id: call_id.clone(),
                        result: Some(json!({ "content": "外部提供者应答" })),
                        error: None,
                    },
                )
                .is_err(),
            "他插件的回报不得认领本插件的调用"
        );
        // `service.call` 同样走外部提供者
        caller.handle_line(&request_line(
            service::method::CALL,
            &json!({ "service": "vault", "method": "readText", "params": { "path": "notes/b.md" } }),
        ));
        let second = wait_service_call_other_than(&runtime, &call_id).await;
        for id in [call_id, second] {
            runtime
                .resolve_service_call(
                    &provider.id,
                    service::ResultParams {
                        call_id: id,
                        result: Some(json!({ "content": "外部提供者应答" })),
                        error: None,
                    },
                )
                .expect("提供者回报应被接受");
        }

        // 提供者下线：登记摘除、活动回落内置
        provider.stop();
        assert_eq!(provider_of(&runtime), None);
    }

    /// 在途调用遇提供者停用立即以 `provider_unavailable` 收场，不等 30 秒超时。
    #[tokio::test]
    async fn in_flight_call_fails_when_provider_stops() {
        let tmp = TempDir::new("instance-service-failover");
        let runtime = PluginRuntime::new(tmp.to_path_buf());
        let (_t, provider, _events) =
            make_named_instance_with_events("com.example.provider", Vec::new());
        provider.attach_runtime(&runtime);
        provider.set_status(PluginStatus::Running);
        provider.session_active.store(true, Ordering::SeqCst);
        runtime
            .instances
            .lock()
            .unwrap()
            .insert(provider.id.clone(), provider.clone());
        runtime
            .register_service(
                &provider.id,
                service::RegisterParams {
                    service: "vault".into(),
                    version: None,
                    methods: vec!["readText".into()],
                },
            )
            .expect("登记应成功");
        runtime
            .set_service_provider("vault", Some(&provider.id))
            .expect("切换应成功");

        let (_t2, caller, _caller_events) =
            make_named_instance_with_events("com.example.caller", Vec::new());
        caller.attach_runtime(&runtime);
        runtime
            .instances
            .lock()
            .unwrap()
            .insert(caller.id.clone(), caller.clone());
        caller.handle_line(&request_line(
            method::vault::READ_TEXT,
            &json!({ "path": "notes/a.md" }),
        ));
        let call_id = wait_service_call(&runtime).await;

        provider.stop();
        assert!(
            runtime.service_calls.lock().unwrap().is_empty(),
            "提供者停用后在途调用立即收场，不留在表里等超时"
        );
        let late = runtime
            .resolve_service_call(
                &provider.id,
                service::ResultParams {
                    call_id,
                    result: Some(json!({ "content": "迟到" })),
                    error: None,
                },
            )
            .expect_err("收场后的回报应被丢弃");
        assert!(late.contains("已收场"), "应可读说明迟到：{late}");
    }

    /// 分组方法名（`ai.chat.send`）经能力总线调用与直接调用落到同一实现。
    #[test]
    fn grouped_ai_methods_resolve_through_the_bus() {
        let tmp = TempDir::new("instance-service-grouped");
        let runtime = PluginRuntime::new(tmp.to_path_buf());
        let target = runtime
            .service_target(service::CallParams {
                service: "ai".into(),
                method: "chat.send".into(),
                params: json!({ "sessionId": "s1", "text": "hi" }),
            })
            .expect("分组方法应可决议");
        let ServiceTarget::Builtin { wire, .. } = target else {
            panic!("出厂活动提供者是内置实现");
        };
        assert_eq!(wire, method::ai::CHAT_SEND, "总线调用规范化回直接调用的 wire 名");
        // 工具名册与结果回报是宿主记账，不在能力目录里，也经不了总线调用
        let tool_result = runtime
            .service_target(service::CallParams {
                service: "ai".into(),
                method: "toolResult".into(),
                params: Value::Null,
            })
            .expect_err("回报方法不是可调用能力");
        assert!(tool_result.message.contains(service::error::UNKNOWN_METHOD));
    }

    /// `ai` 被外部提供者顶替后，工具名册与回报仍由宿主处理，不被提供者截走。
    #[test]
    fn host_owned_ai_methods_are_never_forwarded() {
        let tmp = TempDir::new("instance-service-host-owned");
        let runtime = PluginRuntime::new(tmp.to_path_buf());
        runtime
            .register_service(
                "com.example.provider",
                service::RegisterParams {
                    service: "ai".into(),
                    version: None,
                    methods: vec!["chat.send".into(), "toolResult".into(), "registerTool".into()],
                },
            )
            .expect("登记应成功");
        runtime
            .set_service_provider("ai", Some("com.example.provider"))
            .expect("切换应成功");
        assert_eq!(
            runtime.forward_to("ai", "chat.send"),
            Forward::Plugin("com.example.provider".into()),
            "对话能力可以被外部提供者接管"
        );
        for (service_name, method_name) in [
            ("ai", "toolResult"),
            ("ai", "registerTool"),
            ("ai", "unregisterTool"),
        ] {
            assert_eq!(
                runtime.forward_to(service_name, method_name),
                Forward::Local,
                "{service_name}.{method_name} 是宿主保留方法"
            );
        }
    }

    /// 归属插件停用后，直接调它的方法（不经 `service.call`）也不能落到本地实现。
    #[test]
    fn a_disabled_owner_blocks_direct_service_methods() {
        let tmp = TempDir::new("instance-service-blocked");
        let runtime = PluginRuntime::new(tmp.to_path_buf());
        runtime.set_builtin_owner_enabled("atelyx.notes", false);
        assert_eq!(runtime.forward_to("markdown", "parse"), Forward::Unavailable);
        assert!(runtime
            .service_target(service::CallParams {
                service: "markdown".into(),
                method: "parse".into(),
                params: Value::Null,
            })
            .is_err_and(|error| error.message.contains(service::error::UNKNOWN_SERVICE)));
        runtime.set_builtin_owner_enabled("atelyx.notes", true);
        assert_eq!(runtime.forward_to("markdown", "parse"), Forward::Local, "重新启用即回到内置分派");
    }

    /// 等在途服务调用落表（转发任务被调度后才登记）。
    async fn wait_service_call(runtime: &PluginRuntime) -> String {
        for _ in 0..100 {
            if let Some(id) = runtime.service_calls.lock().unwrap().keys().next() {
                return id.clone();
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("服务调用未在预期时间内登记");
    }

    /// 等一个不同于 `existing` 的在途服务调用（两笔调用同时在途时用）。
    async fn wait_service_call_other_than(runtime: &PluginRuntime, existing: &str) -> String {
        for _ in 0..100 {
            let found = runtime
                .service_calls
                .lock()
                .unwrap()
                .keys()
                .find(|id| id.as_str() != existing)
                .cloned();
            if let Some(id) = found {
                return id;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("第二笔服务调用未在预期时间内登记");
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
    fn window_actions_route_to_the_shell() {
        for name in [
            method::window::OPEN,
            method::window::CLOSE,
            method::window::UPDATE,
            method::window::SHOW,
            method::window::MINIMIZE,
            method::window::TOGGLE_MAXIMIZE,
            method::window::BEGIN_DRAG,
        ] {
            assert!(is_ui_method(name), "{name} 必须走壳侧界面分派");
            assert!(!is_shell_method(name), "{name} 不得落进壳侧异步服务分派");
        }
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

    /// 会话收尾清空整面帧并释放共享内存（停用 / 崩溃 / 卸载后不得留下可渲染的旧帧）。
    #[test]
    fn teardown_drops_frames_and_the_region() {
        let (_tmp, instance) = make_instance(Vec::new());
        let info = instance.frame_region.ensure().unwrap();
        let frame = frame::SurfaceFrame { fps: Some(60), ..frame::SurfaceFrame::default() };
        let file = std::fs::OpenOptions::new().read(true).write(true).open(&info.path).unwrap();
        let mut map = unsafe {
            memmap2::MmapOptions::new().len(info.byte_len()).map_mut(&file).unwrap()
        };
        let payload = frame_codec::encode(&frame);
        frame_region::write_slot(&mut map, &info, 0, 2, &payload).unwrap();
        drop(map);
        instance
            .dispatch(
                method::frame::SUBMIT_SHARED,
                &json!({
                    "surfaceId": "main",
                    "generation": info.generation,
                    "slot": 0,
                    "seq": 2,
                    "len": payload.len(),
                }),
            )
            .unwrap();
        assert!(instance.surface_frame("main").is_some());
        assert!(instance.has_frame_ticks());

        instance.release_resources();
        assert!(instance.surface_frame("main").is_none());
        assert!(!instance.has_frame_ticks());
        assert!(!std::path::Path::new(&info.path).exists(), "区域文件随会话释放");
    }

    /// 绘制期登记的等帧会取走退回的帧：登记期间的提交不发通知，超时后到达的帧照常通知。
    #[test]
    fn claimed_frame_is_not_broadcast_and_a_timeout_falls_back_to_notice() {
        let (_tmp, instance, mut events) = make_instance_with_events(Vec::new());
        let info = instance.frame_region.ensure().unwrap();
        let file = std::fs::OpenOptions::new().read(true).write(true).open(&info.path).unwrap();
        let mut map = unsafe {
            memmap2::MmapOptions::new().len(info.byte_len()).map_mut(&file).unwrap()
        };
        let submit = |map: &mut memmap2::MmapMut, slot: u32, seq: u64| {
            let frame = frame::SurfaceFrame::default();
            let payload = frame_codec::encode(&frame);
            frame_region::write_slot(map, &info, slot, seq, &payload).unwrap();
            instance
                .dispatch(
                    method::frame::SUBMIT_SHARED,
                    &json!({
                        "surfaceId": "main",
                        "generation": info.generation,
                        "slot": slot,
                        "seq": seq,
                        "len": payload.len(),
                    }),
                )
                .unwrap();
        };

        // 没人等帧：提交照常通知壳
        submit(&mut map, 0, 2);
        let first = instance.surface_frame("main").unwrap();
        assert!(matches!(
            events.try_recv().unwrap(),
            PluginEvent::ViewSubmitted { .. }
        ));

        // 登记等帧期间的提交由等待方取走，不再通知
        instance.begin_frame_wait("main", first.clone());
        submit(&mut map, 1, 2);
        assert!(events.try_recv().is_err(), "登记期间的帧不得再发通知");
        let second = instance.finish_frame_wait("main", Duration::from_millis(200)).unwrap();
        assert!(!Arc::ptr_eq(&first, &second));
        assert!(instance.take_observed_pending(), "取走即标记待补投影");
        assert!(!instance.take_observed_pending());

        // 等帧超时会撤掉登记：之后退回的帧仍要通知
        instance.begin_frame_wait("main", second);
        assert!(instance.finish_frame_wait("main", Duration::from_millis(1)).is_none());
        submit(&mut map, 0, 4);
        assert!(
            matches!(events.try_recv().unwrap(), PluginEvent::ViewSubmitted { .. }),
            "超时后到达的帧照常通知"
        );
    }

    /// 尺寸突发只保留最新一笔在途：回执放行下一笔，空闲后兜底线程退出。
    #[test]
    fn resize_burst_keeps_one_in_flight_and_the_latest_pending() {
        let (_tmp, instance) = make_instance(Vec::new());
        instance.on_resize("panel", 100.0, 100.0);
        instance.on_resize("panel", 110.0, 120.0);
        instance.on_resize("panel", 130.0, 140.0);
        {
            let gates = instance.resize_gates.lock().unwrap();
            let gate = gates.get("panel").unwrap();
            assert!(gate.in_flight.is_some(), "首笔立即发出");
            assert_eq!(gate.pending, Some((130.0, 140.0)), "拖动中的中间尺寸被最新一笔覆盖");
            assert!(gate.timer_active, "每个面至多一条兜底线程");
        }

        instance.resize_acked("panel");
        {
            let gates = instance.resize_gates.lock().unwrap();
            let gate = gates.get("panel").unwrap();
            assert!(gate.in_flight.is_some(), "回帧即放行最新一笔");
            assert!(gate.pending.is_none());
        }

        instance.resize_acked("panel");
        assert!(!instance.resize_fallback("panel"), "没有在途尺寸即停兜底");
        assert!(
            !instance.resize_gates.lock().unwrap().get("panel").unwrap().timer_active,
            "空闲后定时器退出，下次尺寸变化重新起线程"
        );
    }

    /// 超过放弃时限仍无新帧：最后一笔尺寸照发并撤闸，下次尺寸变化重新开闸。
    #[test]
    fn resize_gate_gives_up_after_the_deadline() {
        let (_tmp, instance) = make_instance(Vec::new());
        instance.on_resize("panel", 100.0, 100.0);
        {
            let mut gates = instance.resize_gates.lock().unwrap();
            let gate = gates.get_mut("panel").unwrap();
            gate.in_flight = Some(Instant::now() - RESIZE_GIVE_UP - Duration::from_millis(1));
            gate.pending = Some((120.0, 130.0));
        }
        assert!(!instance.resize_fallback("panel"), "撤闸后兜底线程退出");
        assert!(
            !instance.resize_gates.lock().unwrap().contains_key("panel"),
            "闸门撤掉，下次尺寸变化重建"
        );

        instance.on_resize("panel", 140.0, 150.0);
        assert!(instance.resize_gates.lock().unwrap().get("panel").unwrap().in_flight.is_some());
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
