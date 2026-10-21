//! 协作空间 WebSocket 帧泵：连接生命周期、心跳保活、半开检测与断线退避重连，
//! 以及插件广播帧的序号去重与缺口补投；业务语义归壳层。
//! 帧格式 = JSON 文本帧（camelCase，`type` 分派）。

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};

use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Map, Value};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::{
    connect_async_with_config, tungstenite::Message, MaybeTlsStream, WebSocketStream,
};

use crate::net_guard;

/// 服务端空间频道路径（拼在服务地址后）。
const WS_PATH: &str = "/ws/space";
/// 事件通道容量：消费过慢时背压到读循环，服务端对落后者自行下发 resync。
const EVENT_CAPACITY: usize = 256;
/// 心跳帧（纯类型帧）。
const PING_FRAME: &str = r#"{"type":"ping"}"#;
/// 优雅断开帧（服务端收到即断开，防幽灵在线）。
const BYE_FRAME: &str = r#"{"type":"bye"}"#;
/// 单帧上行超时：发送缓冲满（半开连接）时 send 无限挂起会卡死 select 循环，
/// 超时按发送失败走重连（远大于正常发送耗时，远小于判死窗口）。
const SEND_TIMEOUT: Duration = Duration::from_secs(10);
/// 连接建立超时（TCP + TLS + WS 握手）：对端 accept 后停滞时裸 connect 永不返回，
/// 泵在 bye_pending 置位后会永久卡死无法终结。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// 插件帧缺口积压上限（条）：服务端补投缓存只留 128 条，积压逼近它就先要补投，
/// 免等缺口被新帧挤出缓存窗口。
const PLUGIN_PENDING_MAX: usize = 64;

/// 一条已建立的 WebSocket 连接。
type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// hello 首帧载荷（泵补 `type` 与 camelCase 字段命名）。手写 Debug 脱敏令牌（防 `{:?}` 日志触到明文）。
#[derive(Clone)]
pub struct HelloFrame {
    pub space_id: String,
    pub token: String,
    pub nickname: String,
    pub color: String,
    pub device_name: String,
    pub version: Option<String>,
}

impl std::fmt::Debug for HelloFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HelloFrame")
            .field("space_id", &self.space_id)
            .field("token", &"[redacted]")
            .field("nickname", &self.nickname)
            .field("color", &self.color)
            .field("device_name", &self.device_name)
            .field("version", &self.version)
            .finish()
    }
}

/// 壳 → 泵命令。
#[derive(Debug)]
pub enum SpaceCmd {
    /// 笔记协作同步（Yjs 二进制，泵内 base64 包装）。
    NoteSync { file: String, payload: Vec<u8> },
    /// 笔记协作 awareness。
    NoteAware { file: String, payload: Vec<u8> },
    /// presence 上行（JSON 对象，字段由壳组装，服务端不解析）。
    Presence { presence: Value },
    /// 插件通用消息上行（频道由壳按插件命名空间拼好，载荷服务端不解析）。
    /// `target_peer_id` 缺省 = 广播房间内其他成员；定向单播为尽力而为、不经补投。
    PluginMsg { channel: String, payload: Value, target_peer_id: Option<u64> },
    /// 优雅断开（切空间前发）。泵收此命令即终结，不再产事件。
    Bye,
}

/// 房间成员快照单条。
#[derive(Debug, Clone, PartialEq)]
pub struct PeerInfo {
    pub peer_id: String,
    pub nickname: String,
    pub color: String,
    pub device_name: String,
    pub version: Option<String>,
    pub presence: Option<Value>,
}

/// 泵 → 壳事件。
#[derive(Debug, Clone, PartialEq)]
pub enum SpaceEvent {
    /// 连接建立且 hello-ack 已收（此后命令才真正上行）。
    Ready { peer_id: String },
    Peers(Vec<PeerInfo>),
    Presence { peer_id: String, presence: Value },
    /// 载荷已从 base64 解码为原始字节。
    NoteSync { peer_id: String, file: String, payload: Vec<u8> },
    NoteAware { peer_id: String, file: String, payload: Vec<u8> },
    /// 表格/画布补丁。peer_id = Some 是他人 WS 透传（不落盘）或服务端落地广播回填的发起者
    /// peerId；None = 服务端落地广播且发起者无房间连接。`landed` = 服务端真源已含本补丁
    /// （HTTP 落地广播帧标记；WS 透传帧不带）——壳据此推进保存基线，peerId 匹配本端时跳过合入。
    TablePatch { peer_id: Option<String>, file: String, patch: Value, landed: bool },
    /// 团队元数据变更（只带键名，值由壳回读磁盘真源）。
    MetaChanged { key: String },
    Renamed { old_path: String, new_path: String },
    /// 文件结构变更（他端新建/删除文件与文件夹、复制）：客户端重拉整树刷新面板。
    FsChanged,
    /// 插件通用消息（广播帧已按房间序号去重补序，单播帧原样直达）。
    PluginMsg { peer_id: String, channel: String, payload: Value },
    /// 服务端提示本端消费过慢：需重新握手补齐状态。
    Resync,
    Error { message: String },
    /// 服务端拒绝（无效令牌 / 非成员 / 被移出空间）：泵终结且重连无意义，壳侧须停订阅
    /// 防订阅重算把已终结的泵原样重生（否则形成无退避的重连风暴）。
    Denied { message: String },
    /// 已就绪的连接断开（壳侧应重发握手态）。
    Disconnected,
    /// 每次重连尝试前发出（attempt 从 1 起）。
    Reconnecting { attempt: u32 },
}

/// 泵可调参数（测试用小数值）。默认：25s 心跳、75s 无任何入站帧判假死（= 3 个心跳周期，
/// 健康连接其间必有服务端帧）、退避 1s→15s、缓冲 1024。
#[derive(Debug, Clone, Copy)]
pub struct PumpConfig {
    /// 心跳间隔。
    pub ping_every: Duration,
    /// 无入站帧判半开的阈值。
    pub dead_after: Duration,
    /// 重连退避初值（每次失败翻倍）。
    pub backoff_initial: Duration,
    /// 重连退避上限。
    pub backoff_max: Duration,
    /// 断线期命令缓冲条数上限（满丢最旧）。
    pub cmd_buffer: usize,
    /// 插件广播帧缺口宽限：补投也补不齐时按现有帧补序投递的等待上限。
    pub plugin_gap_grace: Duration,
}

impl Default for PumpConfig {
    fn default() -> Self {
        Self {
            ping_every: Duration::from_secs(25),
            dead_after: Duration::from_secs(75),
            backoff_initial: Duration::from_secs(1),
            backoff_max: Duration::from_secs(15),
            cmd_buffer: 1024,
            plugin_gap_grace: Duration::from_secs(3),
        }
    }
}

/// 启动帧泵任务（须在 tokio 运行时内调用）：返回（命令入口，事件流）。
/// 命令入口不阻塞：断线期间到命的命令在泵内缓冲。`base_url` 形如 `http(s)://host:port`，
/// 内部转 `ws(s)://…/ws/space`。
pub fn spawn_pump(
    base_url: &str,
    hello: HelloFrame,
) -> (mpsc::UnboundedSender<SpaceCmd>, mpsc::Receiver<SpaceEvent>) {
    spawn_pump_with(base_url, hello, PumpConfig::default())
}

/// 同 [`spawn_pump`]，注入自定义参数（测试用小数值）。
pub fn spawn_pump_with(
    base_url: &str,
    hello: HelloFrame,
    config: PumpConfig,
) -> (mpsc::UnboundedSender<SpaceCmd>, mpsc::Receiver<SpaceEvent>) {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (event_tx, event_rx) = mpsc::channel(EVENT_CAPACITY);
    tokio::spawn(run_pump(
        base_url.to_string(),
        hello,
        config,
        cmd_rx,
        EventSink(event_tx),
    ));
    (cmd_tx, event_rx)
}

/// 泵事件出口；消费端丢弃接收端后 send 失败，泵据此终结（无人消费的事件不必再产）。
struct EventSink(mpsc::Sender<SpaceEvent>);

impl EventSink {
    async fn send(&self, event: SpaceEvent) -> bool {
        self.0.send(event).await.is_ok()
    }
}

/// 跨连接持续的泵状态：重连退避与断线期命令缓冲。
struct PumpState {
    /// 连续失败计数（hello-ack 清零；Reconnecting 事件取值）。
    attempt: u32,
    /// 下次重连前的等待时长（hello-ack 清回 initial，每次失败后翻倍封顶）。
    backoff: Duration,
    /// 断线 / 握手未完成期间缓冲的命令（Ready 后按序冲刷）。
    buf: VecDeque<SpaceCmd>,
    /// 当前失败连片是否已发过 Error 事件（Ready 清零）。
    fail_reported: bool,
    /// 已按序投递到的房间插件帧序号（跨连接保持，重连据此请求补投）。
    plugin_delivered: u64,
    /// 本泵是否已就绪过：首次加入不回放加入前的缓存帧。
    plugin_joined: bool,
    /// 缺口暂存的广播帧（序号 → 事件），补齐连续段后按序投递。
    plugin_pending: BTreeMap<u64, SpaceEvent>,
    /// 占房间序号但不投递的帧：本端发出的广播帧（服务端不回发本人，凭 `plugin-msg-ack`
    /// 记账）与二进制帧（宿主插件面的载荷只有 JSON）。投递推进时按洞跳过。
    plugin_void: BTreeSet<u64>,
    /// 缺口宽限截止：到点仍补不齐即再要一轮补投，再不齐才按现有帧补序投递。
    plugin_gap_deadline: Option<tokio::time::Instant>,
    /// 当前缺口是否已要过补投：同一缺口只补一次，避免补不回来时反复空转。
    plugin_gap_replayed: bool,
}

/// 连接失败的两类原因：校验被拒产 Error 事件，网络不可达只走退避重连。
enum ConnectFail {
    Rejected(String),
    /// 原因随事件可见（失败连片首尝），不逐次刷屏。
    Unreachable(String),
}

/// 会话结束原因。
enum SessionEnd {
    /// 收到 Bye：泵终结。
    Bye,
    /// 命令源关闭 / 事件流消费端已离场：泵终结。
    Ended,
    /// 服务端拒绝（无效令牌 / 非成员 / 被移出空间）：重连无意义，泵终结。
    Denied,
    /// 连接断开走重连；was_ready = 是否完成过握手（决定是否发 Disconnected）。
    Closed { was_ready: bool },
}

/// 文本帧处理结果。
enum FrameFlow {
    Continue,
    /// hello-ack：进入就绪态，缓冲待冲刷；`replay_after` = 待补投的房间插件帧序号（重连才有）。
    Ready { replay_after: Option<u64> },
    /// 服务端拒绝：泵终结不重连（重新进入空间时由壳层重建）。
    Fatal,
    /// 事件流消费端已离场：泵立即终结（任何路径上「无人消费即终结」都必须成立）。
    Abort,
}

async fn run_pump(
    base_url: String,
    hello: HelloFrame,
    config: PumpConfig,
    mut cmd_rx: mpsc::UnboundedReceiver<SpaceCmd>,
    events: EventSink,
) {
    let hello_text = hello_frame_text(&hello);
    let mut state = PumpState {
        attempt: 0,
        backoff: config.backoff_initial,
        buf: VecDeque::new(),
        fail_reported: false,
        plugin_delivered: 0,
        plugin_joined: false,
        plugin_pending: BTreeMap::new(),
        plugin_void: BTreeSet::new(),
        plugin_gap_deadline: None,
        plugin_gap_replayed: false,
    };
    let mut bye_pending = false;
    loop {
        // 重连退避等待（首次连接不等待、不发 Reconnecting）；期间命令照常缓冲
        if state.attempt > 0 {
            let deadline = tokio::time::Instant::now() + state.backoff;
            loop {
                tokio::select! {
                    _ = tokio::time::sleep_until(deadline) => break,
                    cmd = cmd_rx.recv() => match cmd {
                        None => return,
                        Some(SpaceCmd::Bye) => {
                            bye_pending = true;
                            break;
                        }
                        Some(cmd) => buffer_cmd(&mut state.buf, cmd, config.cmd_buffer),
                    },
                }
            }
            state.backoff = (state.backoff * 2).min(config.backoff_max);
            if bye_pending {
                bye_once(&base_url, &hello_text).await;
                return;
            }
            if !events
                .send(SpaceEvent::Reconnecting { attempt: state.attempt })
                .await
            {
                return;
            }
        }
        // 连接尝试（校验 + 握手）；期间命令照常缓冲，Bye 搭这条在途连接发送
        let mut connect = Box::pin(establish(&base_url));
        let mut established = None;
        let mut rejected = None;
        let mut unreachable = None;
        loop {
            tokio::select! {
                result = connect.as_mut() => {
                    match result {
                        Ok(ws) => established = Some(ws),
                        Err(ConnectFail::Rejected(message)) => rejected = Some(message),
                        Err(ConnectFail::Unreachable(reason)) => unreachable = Some(reason),
                    }
                    break;
                }
                cmd = cmd_rx.recv(), if !bye_pending => match cmd {
                    None => return,
                    Some(SpaceCmd::Bye) => bye_pending = true,
                    Some(cmd) => buffer_cmd(&mut state.buf, cmd, config.cmd_buffer),
                },
            }
        }
        if bye_pending {
            if let Some(mut ws) = established {
                send_bye_frames(&mut ws, &hello_text).await;
            }
            return;
        }
        match established {
            Some(ws) => {
                match run_session(ws, &hello_text, &config, &mut cmd_rx, &events, &mut state).await {
                    SessionEnd::Bye | SessionEnd::Ended | SessionEnd::Denied => return,
                    SessionEnd::Closed { was_ready } => {
                        if was_ready && !events.send(SpaceEvent::Disconnected).await {
                            return;
                        }
                        state.attempt += 1;
                    }
                }
            }
            None => {
                state.attempt += 1;
                if let Some(message) = rejected {
                    // 地址校验失败（可能瞬态，如 DNS 故障）：原因可见后照常退避重试
                    if !events.send(SpaceEvent::Error { message }).await {
                        return;
                    }
                    state.fail_reported = true;
                } else if let Some(reason) = unreachable {
                    // 网络不可达只走退避重连，但失败连片首尝把原因带出（配错端口/路由缺失可定位）
                    if !state.fail_reported {
                        state.fail_reported = true;
                        if !events
                            .send(SpaceEvent::Error {
                                message: format!("连接失败：{reason}"),
                            })
                            .await
                        {
                            return;
                        }
                    }
                }
            }
        }
    }
}

/// 地址校验（DNS 解析逐 IP 过策略，阻塞解析放独立线程）→ WS 握手。
async fn establish(base_url: &str) -> Result<WsStream, ConnectFail> {
    let Some((ws_url, http_url)) = ws_targets(base_url) else {
        return Err(ConnectFail::Rejected(
            "仅支持 http/https 服务地址".to_string(),
        ));
    };
    let check = tokio::task::spawn_blocking(move || net_guard::ensure_ws_target(&http_url));
    match check.await {
        Ok(Ok(())) => {}
        Ok(Err(message)) => return Err(ConnectFail::Rejected(message)),
        Err(e) => return Err(ConnectFail::Rejected(format!("地址校验任务失败：{e}"))),
    }
    // 禁用 Nagle：补丁/presence 走小帧低延迟，等 ACK 攒包只会平添百毫秒级抖动。
    // 注意：此处由 tungstenite 自行解析连接，与上方校验用的解析是两次独立 DNS 查询，
    // 存在校验后 DNS 漂移的理论窗口（web.rs 通道无此窗口，其校验即连接 IP）；
    // 服务地址为用户自配，接受此残余风险
    // 连接建立限时：对端 accept 后停滞（裸 connect 会无限挂起，bye_pending 时泵永久卡死）
    match tokio::time::timeout(CONNECT_TIMEOUT, connect_async_with_config(ws_url.as_str(), None, true)).await {
        Ok(Ok((ws, _))) => Ok(ws),
        Ok(Err(e)) => Err(ConnectFail::Unreachable(e.to_string())),
        Err(_) => Err(ConnectFail::Unreachable(format!(
            "连接超时（{}s 无响应）",
            CONNECT_TIMEOUT.as_secs()
        ))),
    }
}

/// 服务地址 → (WS URL, HTTP URL)：WS 面向连接，HTTP 形态交给 net_guard 校验。
/// 接受 http/https/ws/wss；地址部分原样保留、去除尾随 `/`。
fn ws_targets(base_url: &str) -> Option<(String, String)> {
    let (scheme, rest) = base_url.split_once("://")?;
    let (ws_scheme, http_scheme) = match scheme.to_ascii_lowercase().as_str() {
        "http" | "ws" => ("ws", "http"),
        "https" | "wss" => ("wss", "https"),
        _ => return None,
    };
    let rest = rest.trim_end_matches('/');
    if rest.is_empty() {
        return None;
    }
    Some((
        format!("{ws_scheme}://{rest}{WS_PATH}"),
        format!("{http_scheme}://{rest}"),
    ))
}

/// 未连接时收到 Bye：尝试一条新连接发送 hello+bye，失败即结束（不重试）。
async fn bye_once(base_url: &str, hello_text: &str) {
    if let Ok(mut ws) = establish(base_url).await {
        send_bye_frames(&mut ws, hello_text).await;
    }
}

/// 优雅断开仍走完整握手（服务端首帧必须是 hello），随后发 bye。
async fn send_bye_frames(ws: &mut WsStream, hello_text: &str) {
    let _ = ws.send(Message::text(hello_text)).await;
    let _ = ws.send(Message::text(BYE_FRAME)).await;
}

/// 一条连接的会话循环：hello 首发 → 帧分发 / 命令上行 / 心跳与半开检测。
/// 命令在 hello-ack 前入缓冲（与断线期同一队列），Ready 后按序冲刷。
async fn run_session(
    mut ws: WsStream,
    hello_text: &str,
    config: &PumpConfig,
    cmd_rx: &mut mpsc::UnboundedReceiver<SpaceCmd>,
    events: &EventSink,
    state: &mut PumpState,
) -> SessionEnd {
    if send_with_timeout(&mut ws, Message::text(hello_text)).await.is_err() {
        return SessionEnd::Closed { was_ready: false };
    }
    let mut ready = false;
    let mut last_inbound = Instant::now();
    // 首个心跳延后一个完整周期：不在握手窗口发 ping，避免插进 hello 与业务帧之间
    let mut ping = tokio::time::interval_at(
        tokio::time::Instant::now() + config.ping_every,
        config.ping_every,
    );
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        // 每轮重取值：帧处理里可能新开或撤掉缺口宽限
        let plugin_gap_deadline = state.plugin_gap_deadline;
        tokio::select! {
            cmd = cmd_rx.recv() => match cmd {
                None => return SessionEnd::Ended,
                Some(SpaceCmd::Bye) => {
                    let _ = send_with_timeout(&mut ws, Message::text(BYE_FRAME)).await;
                    close_quiet(&mut ws).await;
                    return SessionEnd::Bye;
                }
                Some(cmd) => {
                    if ready {
                        if !send_cmd(&mut ws, cmd).await {
                            close_quiet(&mut ws).await;
                            return SessionEnd::Closed { was_ready: true };
                        }
                    } else {
                        buffer_cmd(&mut state.buf, cmd, config.cmd_buffer);
                    }
                }
            },
            _ = ping.tick() => {
                if send_with_timeout(&mut ws, Message::text(PING_FRAME)).await.is_err() {
                    close_quiet(&mut ws).await;
                    return SessionEnd::Closed { was_ready: ready };
                }
                // 半开检测：dead_after 内无任何入站帧判连接假死（服务端 TCP 未断但不回帧），
                // 主动断开走重连；服务端每 ping 必回 pong，健康连接不会误判
                if last_inbound.elapsed() > config.dead_after {
                    close_quiet(&mut ws).await;
                    return SessionEnd::Closed { was_ready: ready };
                }
            }
            // 缺口宽限到点仍未补齐：先就地在位补投一次，再补不齐才按现有帧补序投递
            _ = gap_wait(plugin_gap_deadline) => {
                if !ready {
                    // 断线残留的宽限：等本次握手重计，不在就绪前投递暂存帧
                    state.plugin_gap_deadline = None;
                } else if !state.plugin_gap_replayed {
                    state.plugin_gap_replayed = true;
                    state.plugin_gap_deadline =
                        Some(tokio::time::Instant::now() + config.plugin_gap_grace);
                    let text = plugin_replay_frame_text(state.plugin_delivered);
                    if send_with_timeout(&mut ws, Message::text(text)).await.is_err() {
                        close_quiet(&mut ws).await;
                        return SessionEnd::Closed { was_ready: true };
                    }
                } else if let FrameFlow::Abort = flush_plugin_gap(state, events).await {
                    close_quiet(&mut ws).await;
                    return SessionEnd::Ended;
                }
            }
            frame = ws.next() => {
                let msg = match frame {
                    Some(Ok(msg)) => msg,
                    Some(Err(_)) | None => {
                        // 对端已 RST/EOF，无 close 握手可言
                        return SessionEnd::Closed { was_ready: ready };
                    }
                };
                last_inbound = Instant::now();
                let text = match msg {
                    Message::Text(text) => text,
                    Message::Binary(data) => {
                        // 二进制插件帧占房间序号但不进插件面（插件载荷只有 JSON）：
                        // 只推进序号，免后续帧等一个永远投不出来的洞
                        if ready {
                            if let Some(seq) =
                                binary_plugin_seq(&data).filter(|seq| *seq > state.plugin_delivered)
                            {
                                state.plugin_void.insert(seq);
                                if let FrameFlow::Abort =
                                    flush_plugin_contiguous(state, events).await
                                {
                                    close_quiet(&mut ws).await;
                                    return SessionEnd::Ended;
                                }
                            }
                        }
                        continue;
                    }
                    _ => continue,
                };
                match handle_text_frame(text.as_str(), ready, &config, state, events).await {
                    FrameFlow::Continue => {}
                    FrameFlow::Ready { replay_after } => {
                        ready = true;
                        // 重连成功：清退避与失败连片标记，下次断线从头计
                        state.attempt = 0;
                        state.backoff = config.backoff_initial;
                        state.fail_reported = false;
                        while let Some(cmd) = state.buf.pop_front() {
                            if !send_cmd(&mut ws, cmd).await {
                                close_quiet(&mut ws).await;
                                return SessionEnd::Closed { was_ready: true };
                            }
                        }
                        // 补投请求经本连接上行，回放帧与直播帧同出站通道保序；
                        // 缺口暂存已按就绪时刻重计宽限，等补投期间不会被提前弃等
                        if let Some(after) = replay_after {
                            let text = plugin_replay_frame_text(after);
                            if send_with_timeout(&mut ws, Message::text(text)).await.is_err() {
                                close_quiet(&mut ws).await;
                                return SessionEnd::Closed { was_ready: true };
                            }
                        }
                    }
                    FrameFlow::Fatal => {
                        close_quiet(&mut ws).await;
                        return SessionEnd::Denied;
                    }
                    FrameFlow::Abort => {
                        close_quiet(&mut ws).await;
                        return SessionEnd::Ended;
                    }
                }
            }
        }
    }
}

/// 上行帧带超时：TCP 发送缓冲满（半开连接）时 `send` 会无限挂起并卡死 select 循环，
/// ping 停发、下行停轮、判死失效——超时按发送失败处理走重连。
async fn send_with_timeout(ws: &mut WsStream, msg: Message) -> Result<(), ()> {
    match tokio::time::timeout(SEND_TIMEOUT, ws.send(msg)).await {
        Ok(Ok(())) => Ok(()),
        _ => Err(()),
    }
}

/// 主动断开前发 close 帧：直接 drop 只关 TCP（未读下行帧会让内核改发 RST），
/// 服务端把「未走 close 握手的断开」统一报协议错误，掩盖真实断开原因。
async fn close_quiet(ws: &mut WsStream) {
    let _ = send_with_timeout(ws, Message::Close(None)).await;
}

/// 文本帧 → 事件：字段抽取与 base64 解码，业务匹配归壳层。
/// 握手完成前的业务帧忽略（服务端先发 hello-ack 再 peers）；载荷解码失败丢帧。
async fn handle_text_frame(
    text: &str,
    ready: bool,
    config: &PumpConfig,
    state: &mut PumpState,
    events: &EventSink,
) -> FrameFlow {
    let Ok(frame) = serde_json::from_str::<Value>(text) else {
        return FrameFlow::Continue;
    };
    let Some(kind) = frame.get("type").and_then(Value::as_str) else {
        return FrameFlow::Continue;
    };
    match kind {
        "hello-ack" => {
            let peer_id = frame
                .get("peerId")
                .and_then(peer_id_string)
                .unwrap_or_default();
            // 序号头 = 服务端当前已分配的插件帧序号。首次加入从序号头续上（之前的帧发生在
            // 加入前）；小于已投递序号 = 房间重建、序号空间重置，归零重来
            if let Some(header) = frame.get("pluginSeq").and_then(Value::as_u64) {
                if !state.plugin_joined {
                    state.plugin_delivered = state.plugin_delivered.max(header);
                } else if header < state.plugin_delivered {
                    state.plugin_delivered = 0;
                    state.plugin_void.clear();
                }
                state.plugin_pending.retain(|seq, _| *seq <= header);
                state.plugin_void.retain(|seq| *seq > state.plugin_delivered);
                if state.plugin_pending.is_empty() {
                    state.plugin_gap_deadline = None;
                }
            }
            // 重连才请求补投：首次加入不回放加入前的缓存帧
            let replay_after = state.plugin_joined.then_some(state.plugin_delivered);
            state.plugin_joined = true;
            // 重连自带一次补投：断线前的暂存帧由它补齐；无暂存帧时下一个缺口仍可再请求一次
            state.plugin_gap_replayed = replay_after.is_some() && !state.plugin_pending.is_empty();
            if !state.plugin_pending.is_empty() {
                state.plugin_gap_deadline =
                    Some(tokio::time::Instant::now() + config.plugin_gap_grace);
            }
            if events.send(SpaceEvent::Ready { peer_id }).await {
                FrameFlow::Ready { replay_after }
            } else {
                FrameFlow::Abort
            }
        }
        "error" => {
            // 服务端拒绝（会话吊销 / 移出空间 / 无效令牌 / 非成员）：重连无意义，泵终结
            let message = str_field(&frame, "message").unwrap_or_default();
            if events.send(SpaceEvent::Denied { message }).await {
                FrameFlow::Fatal
            } else {
                FrameFlow::Abort
            }
        }
        "pong" => FrameFlow::Continue,
        "fs-changed" => deliver(events.send(SpaceEvent::FsChanged).await),
        _ if !ready => FrameFlow::Continue,
        "peers" => {
            let peers = frame
                .get("peers")
                .and_then(Value::as_array)
                .map(|list| list.iter().map(parse_peer).collect())
                .unwrap_or_default();
            deliver(events.send(SpaceEvent::Peers(peers)).await)
        }
        "presence" => {
            let (Some(peer_id), Some(presence)) = (
                frame.get("peerId").and_then(peer_id_string),
                frame.get("presence").cloned().filter(|p| !p.is_null()),
            ) else {
                return FrameFlow::Continue;
            };
            deliver(events.send(SpaceEvent::Presence { peer_id, presence }).await)
        }
        "note-sync" | "note-aware" => {
            let (Some(peer_id), Some(file), Some(payload)) = (
                frame.get("peerId").and_then(peer_id_string),
                str_field(&frame, "file"),
                decode_payload(&frame),
            ) else {
                return FrameFlow::Continue;
            };
            let event = if kind == "note-sync" {
                SpaceEvent::NoteSync { peer_id, file, payload }
            } else {
                SpaceEvent::NoteAware { peer_id, file, payload }
            };
            deliver(events.send(event).await)
        }
        "table-patch" => {
            let Some(file) = str_field(&frame, "file") else {
                return FrameFlow::Continue;
            };
            let peer_id = frame.get("peerId").and_then(peer_id_string);
            let patch = frame.get("patch").cloned().unwrap_or(Value::Null);
            let landed = frame.get("landed").and_then(Value::as_bool).unwrap_or(false);
            deliver(events.send(SpaceEvent::TablePatch { peer_id, file, patch, landed }).await)
        }
        "meta-changed" => {
            let Some(key) = str_field(&frame, "key") else {
                return FrameFlow::Continue;
            };
            deliver(events.send(SpaceEvent::MetaChanged { key }).await)
        }
        "renamed" => {
            let Some(payload) = frame.get("payload") else {
                return FrameFlow::Continue;
            };
            let (Some(old_path), Some(new_path)) =
                (str_field(payload, "oldPath"), str_field(payload, "newPath"))
            else {
                return FrameFlow::Continue;
            };
            deliver(events.send(SpaceEvent::Renamed { old_path, new_path }).await)
        }
        "resync" => deliver(events.send(SpaceEvent::Resync).await),
        "plugin-msg" => {
            let (Some(peer_id), Some(channel)) = (
                frame.get("peerId").and_then(peer_id_string),
                str_field(&frame, "channel"),
            ) else {
                return FrameFlow::Continue;
            };
            let payload = frame.get("payload").cloned().unwrap_or(Value::Null);
            let event = SpaceEvent::PluginMsg { peer_id, channel, payload };
            match frame.get("seq").and_then(Value::as_u64).filter(|seq| *seq > 0) {
                // 广播帧：按房间序号去重补序（重连期缺帧由补投送来）
                Some(seq) => deliver_plugin_broadcast(state, seq, event, config, events).await,
                // 单播帧：不占序号、不入补投缓存，原样直达
                None => deliver(events.send(event).await),
            }
        }
        // 自己发出的广播帧只收回执（服务端不回发本人）：记入跳过集推进序号
        "plugin-msg-ack" => {
            let Some(seq) = frame.get("seq").and_then(Value::as_u64).filter(|seq| *seq > 0) else {
                return FrameFlow::Continue;
            };
            if seq > state.plugin_delivered {
                state.plugin_void.insert(seq);
            }
            flush_plugin_contiguous(state, events).await
        }
        // canvas-patch / 未知类型：泵不承载
        _ => FrameFlow::Continue,
    }
}

/// 事件投递成功 → 继续；消费端已离场 → 泵立即终结。
fn deliver(sent: bool) -> FrameFlow {
    if sent {
        FrameFlow::Continue
    } else {
        FrameFlow::Abort
    }
}

/// 广播插件帧按房间序号投递：重复丢弃、连续直达、缺口暂存等补投。
/// 积压超过 [`PLUGIN_PENDING_MAX`] 时把宽限压到最短（立即请求补投）；仍补不齐才按现有帧补序投递。
async fn deliver_plugin_broadcast(
    state: &mut PumpState,
    seq: u64,
    event: SpaceEvent,
    config: &PumpConfig,
    events: &EventSink,
) -> FrameFlow {
    if seq <= state.plugin_delivered {
        return FrameFlow::Continue;
    }
    if seq > state.plugin_delivered + 1 {
        state.plugin_pending.insert(seq, event);
        if state.plugin_pending.len() >= PLUGIN_PENDING_MAX {
            // 立即要补投：宽限到点由会话循环发请求，此处只把等待压到最短
            state.plugin_gap_deadline = Some(tokio::time::Instant::now());
        } else {
            state
                .plugin_gap_deadline
                .get_or_insert_with(|| tokio::time::Instant::now() + config.plugin_gap_grace);
        }
        return FrameFlow::Continue;
    }
    state.plugin_delivered = seq;
    if !events.send(event).await {
        return FrameFlow::Abort;
    }
    flush_plugin_contiguous(state, events).await
}

/// 投递暂存里接着当前序号的连续帧（本端自己的帧序号只推进不投递）；暂存清空即撤掉宽限。
async fn flush_plugin_contiguous(state: &mut PumpState, events: &EventSink) -> FrameFlow {
    loop {
        let next = state.plugin_delivered + 1;
        if state.plugin_void.remove(&next) {
            state.plugin_delivered = next;
            continue;
        }
        let Some(event) = state.plugin_pending.remove(&next) else {
            break;
        };
        state.plugin_delivered = next;
        if !events.send(event).await {
            return FrameFlow::Abort;
        }
    }
    if state.plugin_pending.is_empty() {
        state.plugin_gap_deadline = None;
        state.plugin_gap_replayed = false;
    }
    FrameFlow::Continue
}

/// 弃等缺口：按现有序号升序投递暂存帧（跳过的序号补不回来，只能以现有帧续上）。
async fn flush_plugin_gap(state: &mut PumpState, events: &EventSink) -> FrameFlow {
    state.plugin_gap_deadline = None;
    state.plugin_gap_replayed = false;
    for (seq, event) in std::mem::take(&mut state.plugin_pending) {
        state.plugin_delivered = seq;
        if !events.send(event).await {
            return FrameFlow::Abort;
        }
    }
    // 已越过游标的跳过集条目不会再命中，只留游标之后的（自己的帧可能随后到达）
    state.plugin_void.retain(|seq| *seq > state.plugin_delivered);
    FrameFlow::Continue
}

/// 缺口宽限等待；无缺口时永不就绪。
async fn gap_wait(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

/// 命令编码上行（false = 写失败，连接已死）。Bye 由会话循环拦截，不经此路径。
async fn send_cmd(ws: &mut WsStream, cmd: SpaceCmd) -> bool {
    let text = match cmd {
        SpaceCmd::NoteSync { file, payload } => note_frame_text("note-sync", &file, &payload),
        SpaceCmd::NoteAware { file, payload } => note_frame_text("note-aware", &file, &payload),
        SpaceCmd::Presence { presence } => presence_frame_text(presence),
        SpaceCmd::PluginMsg { channel, payload, target_peer_id } => {
            plugin_frame_text(&channel, payload, target_peer_id)
        }
        SpaceCmd::Bye => return true,
    };
    send_with_timeout(ws, Message::text(text)).await.is_ok()
}

/// 笔记协作帧：Yjs 二进制经标准 base64 包装，服务端不透明转发。
fn note_frame_text(kind: &str, file: &str, payload: &[u8]) -> String {
    json!({
        "type": kind,
        "file": file,
        "payload": base64::engine::general_purpose::STANDARD.encode(payload),
    })
    .to_string()
}

/// 二进制插件帧的房间序号。线格式：kind(1) flags(1) 频道字节长(2) 频道 seq(8) sender(8)
/// [target(8)] 载荷；不是本类帧或长度不足即 None。
fn binary_plugin_seq(data: &[u8]) -> Option<u64> {
    const KIND: u8 = 1;
    const HEADER: usize = 4;
    if data.len() < HEADER + 8 || data[0] != KIND {
        return None;
    }
    let channel_len = u16::from_le_bytes([data[2], data[3]]) as usize;
    let offset = HEADER + channel_len;
    data.get(offset..offset + 8)?
        .try_into()
        .ok()
        .map(u64::from_le_bytes)
}

/// 插件广播帧补投请求：服务端按房间序号补发 `seq > after` 的缓存帧（缓存窗口外的补不回）。
fn plugin_replay_frame_text(after: u64) -> String {
    json!({ "type": "plugin-replay", "after": after }).to_string()
}

/// 插件消息上行：频道 + 任意 JSON 载荷透传；定向单播带数值 `targetPeerId`。
fn plugin_frame_text(channel: &str, payload: Value, target_peer_id: Option<u64>) -> String {
    let mut obj = Map::new();
    obj.insert("type".to_string(), Value::from("plugin-msg"));
    obj.insert("channel".to_string(), Value::from(channel));
    obj.insert("payload".to_string(), payload);
    if let Some(target) = target_peer_id {
        obj.insert("targetPeerId".to_string(), Value::from(target));
    }
    Value::Object(obj).to_string()
}

/// presence 上行：壳组装的 JSON 对象打上 `type` 原样透传（非对象则只发 type）。
/// 先合并字段再打 `type`：载荷里的同名键不得覆盖帧类型。
fn presence_frame_text(presence: Value) -> String {
    let mut obj = match presence {
        Value::Object(fields) => fields,
        _ => Map::new(),
    };
    obj.insert("type".to_string(), Value::from("presence"));
    Value::Object(obj).to_string()
}

/// hello 首帧（camelCase；version 缺省不下发）。
fn hello_frame_text(hello: &HelloFrame) -> String {
    let mut obj = Map::new();
    obj.insert("type".to_string(), Value::from("hello"));
    obj.insert("spaceId".to_string(), Value::from(hello.space_id.as_str()));
    obj.insert("token".to_string(), Value::from(hello.token.as_str()));
    obj.insert("nickname".to_string(), Value::from(hello.nickname.as_str()));
    obj.insert("color".to_string(), Value::from(hello.color.as_str()));
    obj.insert(
        "deviceName".to_string(),
        Value::from(hello.device_name.as_str()),
    );
    if let Some(version) = &hello.version {
        obj.insert("version".to_string(), Value::from(version.as_str()));
    }
    Value::Object(obj).to_string()
}

/// 命令入缓冲；容量满丢最旧（断线期的旧命令已无现场价值，保留最新）。
/// NoteSync 是因果链增量，丢弃会产生文档缺口——满时优先腾出可覆盖类命令
/// （presence / awareness 新盖旧），无可腾才按序丢最旧。
fn buffer_cmd(buf: &mut VecDeque<SpaceCmd>, cmd: SpaceCmd, cap: usize) {
    if buf.len() >= cap {
        let evictable = buf
            .iter()
            .position(|c| matches!(c, SpaceCmd::Presence { .. } | SpaceCmd::NoteAware { .. }));
        match evictable {
            Some(i) => {
                buf.remove(i);
            }
            None => {
                buf.pop_front();
            }
        }
    }
    buf.push_back(cmd);
}

/// peerId 还原为字符串（服务端为数值；字符串形态同收）。
fn peer_id_string(value: &Value) -> Option<String> {
    if let Some(n) = value.as_u64() {
        Some(n.to_string())
    } else {
        value.as_str().map(str::to_string)
    }
}

/// 字符串字段提取（缺字段 → None）。
fn str_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

/// note-sync/note-aware 载荷解码（base64 → 原始字节；缺失或非法 → None，丢帧）。
fn decode_payload(frame: &Value) -> Option<Vec<u8>> {
    let raw = frame.get("payload").and_then(Value::as_str)?;
    base64::engine::general_purpose::STANDARD.decode(raw).ok()
}

/// peers 帧单条成员还原（缺省字段落空值）。
fn parse_peer(entry: &Value) -> PeerInfo {
    PeerInfo {
        peer_id: entry
            .get("peerId")
            .and_then(peer_id_string)
            .unwrap_or_default(),
        nickname: str_field(entry, "nickname").unwrap_or_default(),
        color: str_field(entry, "color").unwrap_or_default(),
        device_name: str_field(entry, "deviceName").unwrap_or_default(),
        version: str_field(entry, "version"),
        presence: entry.get("presence").cloned().filter(|p| !p.is_null()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use serde_json::json;
    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_async;

    /// 假服务端一侧的 WebSocket 连接。
    type ServerWs = WebSocketStream<tokio::net::TcpStream>;

    /// 毫秒级参数：心跳 40ms / 半开 100ms / 退避 20ms 起（测试要快）。
    fn test_config() -> PumpConfig {
        PumpConfig {
            ping_every: Duration::from_millis(40),
            dead_after: Duration::from_millis(100),
            backoff_initial: Duration::from_millis(20),
            backoff_max: Duration::from_millis(100),
            cmd_buffer: 8,
            plugin_gap_grace: Duration::from_millis(80),
        }
    }

    fn test_hello() -> HelloFrame {
        HelloFrame {
            space_id: "s1".to_string(),
            token: "tok-1".to_string(),
            nickname: "小明".to_string(),
            color: "#3b82f6".to_string(),
            device_name: "dev-a".to_string(),
            version: Some("0.1.0".to_string()),
        }
    }

    /// 限时取事件（测试卡死兜底）。
    async fn recv_event(events: &mut mpsc::Receiver<SpaceEvent>) -> SpaceEvent {
        tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("等待事件超时")
            .expect("事件流已结束")
    }

    /// 读一条文本帧 → JSON（跳过非文本帧与心跳帧，与服务端 pong 语义一致）。
    async fn read_json(ws: &mut ServerWs) -> Value {
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
                .await
                .expect("等待帧超时")
                .expect("连接断开")
                .expect("帧读取错误");
            if let Message::Text(text) = msg {
                let value: Value = serde_json::from_str(text.as_str()).unwrap();
                if value["type"] == "ping" {
                    continue;
                }
                return value;
            }
        }
    }

    async fn send_json(ws: &mut ServerWs, frame: Value) {
        ws.send(Message::text(frame.to_string())).await.unwrap();
    }

    /// 接入并升级为 WS（禁用 Nagle：小帧攒包会把下行扣到延迟 ACK 之后，干扰时序断言）。
    async fn accept_ws(stream: tokio::net::TcpStream) -> ServerWs {
        stream.set_nodelay(true).unwrap();
        accept_async(stream).await.unwrap()
    }

    /// 起一个只服务一条连接的假服务端：标准握手后执行 `after_ready`。
    /// 返回 (服务地址, 任务句柄)——句柄须 await 以确保服务端断言执行。
    async fn spawn_handshake_server<F, Fut>(after_ready: F) -> (String, tokio::task::JoinHandle<()>)
    where
        F: FnOnce(ServerWs) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let ws = accept_ws(stream).await;
            let ws = handshake(ws).await;
            after_ready(ws).await;
        });
        (format!("http://{addr}"), task)
    }

    /// 标准握手：读 hello（校验字段映射）、回 hello-ack(peerId=1)。
    async fn handshake(mut ws: ServerWs) -> ServerWs {
        let hello = read_json(&mut ws).await;
        assert_eq!(hello["type"], "hello");
        assert_eq!(hello["spaceId"], "s1");
        assert_eq!(hello["token"], "tok-1");
        assert_eq!(hello["nickname"], "小明");
        assert_eq!(hello["color"], "#3b82f6");
        assert_eq!(hello["deviceName"], "dev-a");
        assert_eq!(hello["version"], "0.1.0");
        send_json(&mut ws, json!({"type": "hello-ack", "peerId": 1})).await;
        ws
    }

    /// 带房间插件序号头的握手（序号空间重置/补投断言用）。
    async fn handshake_with_plugin_seq(mut ws: ServerWs, plugin_seq: u64) -> ServerWs {
        let hello = read_json(&mut ws).await;
        assert_eq!(hello["type"], "hello");
        send_json(
            &mut ws,
            json!({"type": "hello-ack", "peerId": 1, "pluginSeq": plugin_seq}),
        )
        .await;
        ws
    }

    /// 地址换算：http/ws 归一 ws、https/wss 归一 wss，非法 scheme 与空地址拒绝。
    #[test]
    fn ws_targets_converts_schemes() {
        let (ws, http) = ws_targets("http://192.168.1.5:11224").unwrap();
        assert_eq!(ws, "ws://192.168.1.5:11224/ws/space");
        assert_eq!(http, "http://192.168.1.5:11224");
        let (ws, http) = ws_targets("https://collab.example.com/").unwrap();
        assert_eq!(ws, "wss://collab.example.com/ws/space");
        assert_eq!(http, "https://collab.example.com");
        let (ws, http) = ws_targets("ws://127.0.0.1:11224").unwrap();
        assert_eq!(ws, "ws://127.0.0.1:11224/ws/space");
        assert_eq!(http, "http://127.0.0.1:11224");
        assert!(ws_targets("ftp://x").is_none());
        assert!(ws_targets("http://").is_none());
    }

    /// 握手：hello 字段上行还原（见 handshake）、hello-ack → Ready、peers → 成员快照字段还原。
    #[tokio::test]
    async fn handshake_yields_ready_and_peers() {
        let (base, server) = spawn_handshake_server(|mut ws| async move {
            send_json(&mut ws, json!({"type": "peers", "peers": [
                {"peerId": 1, "nickname": "小明", "color": "#3b82f6", "deviceName": "dev-a", "version": "0.1.0"},
                {"peerId": 2, "nickname": "小红", "color": "#f97316", "deviceName": "dev-b", "presence": {"view": "table"}}
            ]}))
            .await;
        })
        .await;
        let (_cmd_tx, mut events) = spawn_pump_with(&base, test_hello(), test_config());
        assert_eq!(
            recv_event(&mut events).await,
            SpaceEvent::Ready { peer_id: "1".to_string() }
        );
        assert_eq!(
            recv_event(&mut events).await,
            SpaceEvent::Peers(vec![
                PeerInfo {
                    peer_id: "1".to_string(),
                    nickname: "小明".to_string(),
                    color: "#3b82f6".to_string(),
                    device_name: "dev-a".to_string(),
                    version: Some("0.1.0".to_string()),
                    presence: None,
                },
                PeerInfo {
                    peer_id: "2".to_string(),
                    nickname: "小红".to_string(),
                    color: "#f97316".to_string(),
                    device_name: "dev-b".to_string(),
                    version: None,
                    presence: Some(json!({"view": "table"})),
                },
            ])
        );
        server.await.unwrap();
    }

    /// 命令上行：NoteSync/NoteAware → base64 包装帧，服务端解码一致。
    #[tokio::test]
    async fn note_cmds_uplink_with_base64_payload() {
        let payload = vec![0u8, 1, 2, 250, 255];
        let server_payload = payload.clone();
        let (base, server) = spawn_handshake_server(move |mut ws| async move {
            let frame = read_json(&mut ws).await;
            assert_eq!(frame["type"], "note-sync");
            assert_eq!(frame["file"], "notes/a.md");
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(frame["payload"].as_str().unwrap())
                .unwrap();
            assert_eq!(decoded, server_payload);
            let frame = read_json(&mut ws).await;
            assert_eq!(frame["type"], "note-aware");
            assert_eq!(frame["file"], "notes/a.md");
        })
        .await;
        let (cmd_tx, mut events) = spawn_pump_with(&base, test_hello(), test_config());
        assert_eq!(
            recv_event(&mut events).await,
            SpaceEvent::Ready { peer_id: "1".to_string() }
        );
        cmd_tx
            .send(SpaceCmd::NoteSync { file: "notes/a.md".to_string(), payload })
            .unwrap();
        cmd_tx
            .send(SpaceCmd::NoteAware {
                file: "notes/a.md".to_string(),
                payload: b"aw".to_vec(),
            })
            .unwrap();
        server.await.unwrap();
    }

    /// 下行帧映射：presence / WS 透传与服务端落地（带 landed）的 table-patch / meta-changed / renamed / resync。
    #[tokio::test]
    async fn downstream_frames_map_to_events() {
        let (base, server) = spawn_handshake_server(|mut ws| async move {
            send_json(&mut ws, json!({"type": "presence", "peerId": 2, "presence": {"view": "table", "file": "t.atb"}})).await;
            send_json(&mut ws, json!({"type": "table-patch", "peerId": 2, "file": "t.atb", "patch": {"ops": 1}})).await;
            send_json(&mut ws, json!({"type": "table-patch", "peerId": 1, "file": "t.atb", "patch": {"ops": 2}, "landed": true})).await;
            send_json(&mut ws, json!({"type": "meta-changed", "key": "calendar"})).await;
            send_json(&mut ws, json!({"type": "renamed", "payload": {"oldPath": "a.md", "newPath": "b/a.md"}})).await;
            send_json(&mut ws, json!({"type": "resync"})).await;
            // 挂住连接，让测试从容收完事件（未收完即断会引入多余事件）
            tokio::time::sleep(Duration::from_millis(300)).await;
        })
        .await;
        let (_cmd_tx, mut events) = spawn_pump_with(&base, test_hello(), test_config());
        assert!(matches!(recv_event(&mut events).await, SpaceEvent::Ready { .. }));
        assert_eq!(
            recv_event(&mut events).await,
            SpaceEvent::Presence {
                peer_id: "2".to_string(),
                presence: json!({"view": "table", "file": "t.atb"}),
            }
        );
        assert_eq!(
            recv_event(&mut events).await,
            SpaceEvent::TablePatch {
                peer_id: Some("2".to_string()),
                file: "t.atb".to_string(),
                patch: json!({"ops": 1}),
                landed: false,
            }
        );
        assert_eq!(
            recv_event(&mut events).await,
            SpaceEvent::TablePatch {
                peer_id: Some("1".to_string()),
                file: "t.atb".to_string(),
                patch: json!({"ops": 2}),
                landed: true,
            }
        );
        assert_eq!(
            recv_event(&mut events).await,
            SpaceEvent::MetaChanged { key: "calendar".to_string() }
        );
        assert_eq!(
            recv_event(&mut events).await,
            SpaceEvent::Renamed {
                old_path: "a.md".to_string(),
                new_path: "b/a.md".to_string(),
            }
        );
        assert_eq!(recv_event(&mut events).await, SpaceEvent::Resync);
        server.await.unwrap();
    }

    /// 心跳：50ms 间隔下 180ms 内至少收到 2 条 ping（首个心跳延后一个完整周期）。
    #[tokio::test]
    async fn heartbeat_pings_keep_flow() {
        let (count_tx, count_rx) = tokio::sync::oneshot::channel();
        let (base, server) = spawn_handshake_server(move |mut ws| async move {
            let mut pings = 0u32;
            let deadline = tokio::time::Instant::now() + Duration::from_millis(180);
            loop {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() {
                    break;
                }
                match tokio::time::timeout(remaining, ws.next()).await {
                    Ok(Some(Ok(Message::Text(text)))) => {
                        if let Ok(v) = serde_json::from_str::<Value>(text.as_str()) {
                            if v["type"] == "ping" {
                                pings += 1;
                            }
                        }
                    }
                    _ => break,
                }
            }
            let _ = count_tx.send(pings);
        })
        .await;
        let config = PumpConfig {
            ping_every: Duration::from_millis(50),
            ..test_config()
        };
        let (_cmd_tx, _events) = spawn_pump_with(&base, test_hello(), config);
        assert!(count_rx.await.unwrap() >= 2);
        server.await.unwrap();
    }

    /// 半开检测：服务端握手后完全静默，dead_after 内客户端主动断开并重连握手成功。
    #[tokio::test]
    async fn silent_server_triggers_half_open_reconnect() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            // 第 1 条连接：握手后不发任何帧（连 hello-ack 也不回），等客户端半开判死
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws1 = accept_ws(stream).await;
            let hello = read_json(&mut ws1).await;
            assert_eq!(hello["type"], "hello");
            // 第 2 条连接：客户端重连后正常握手
            let (stream, _) = listener.accept().await.unwrap();
            let ws2 = handshake(accept_ws(stream).await).await;
            let _ = ws2;
        });
        let (_cmd_tx, mut events) =
            spawn_pump_with(&format!("http://{addr}"), test_hello(), test_config());
        assert_eq!(
            recv_event(&mut events).await,
            SpaceEvent::Reconnecting { attempt: 1 }
        );
        assert_eq!(
            recv_event(&mut events).await,
            SpaceEvent::Ready { peer_id: "1".to_string() }
        );
    }

    /// Bye：服务端收到 bye 帧，事件流随后结束。
    #[tokio::test]
    async fn bye_closes_event_stream() {
        let (base, server) = spawn_handshake_server(|mut ws| async move {
            let frame = read_json(&mut ws).await;
            assert_eq!(frame["type"], "bye");
        })
        .await;
        let (cmd_tx, mut events) = spawn_pump_with(&base, test_hello(), test_config());
        assert_eq!(
            recv_event(&mut events).await,
            SpaceEvent::Ready { peer_id: "1".to_string() }
        );
        cmd_tx.send(SpaceCmd::Bye).unwrap();
        drop(cmd_tx);
        let done = tokio::time::timeout(Duration::from_secs(2), events.recv()).await;
        assert!(matches!(done, Ok(None)), "Bye 后事件流应结束：{done:?}");
        server.await.unwrap();
    }

    /// 服务端拒绝：error 帧转 Denied 事件且泵终结（事件流结束，不再重连）。
    #[tokio::test]
    async fn server_error_denies_and_ends_stream() {
        let (base, server) = spawn_handshake_server(|mut ws| async move {
            send_json(&mut ws, json!({"type": "error", "message": "被移出空间"})).await;
            // 挂住连接等客户端收尾，防任务提前结束引入多余事件
            let _ = tokio::time::timeout(Duration::from_secs(2), ws.next()).await;
        })
        .await;
        let (_cmd_tx, mut events) = spawn_pump_with(&base, test_hello(), test_config());
        assert_eq!(
            recv_event(&mut events).await,
            SpaceEvent::Ready { peer_id: "1".to_string() }
        );
        assert_eq!(
            recv_event(&mut events).await,
            SpaceEvent::Denied { message: "被移出空间".to_string() }
        );
        let done = tokio::time::timeout(Duration::from_secs(2), events.recv()).await;
        assert!(matches!(done, Ok(None)), "Denied 后事件流应结束：{done:?}");
        server.await.unwrap();
    }

    /// 断线缓冲：连接断开期间的命令在重连 Ready 后按序冲刷，载荷原样还原。
    #[tokio::test]
    async fn commands_buffer_across_disconnect() {
        let (kill_tx, kill_rx) = tokio::sync::oneshot::channel::<()>();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let payload_a = vec![1u8, 2, 3];
        let payload_b = vec![9u8, 8, 7];
        let (server_a, server_b) = (payload_a.clone(), payload_b.clone());
        let server = tokio::spawn(async move {
            // 连接 1：握手后等测试信号，杀连接制造断线
            let (stream, _) = listener.accept().await.unwrap();
            let ws1 = handshake(accept_ws(stream).await).await;
            kill_rx.await.ok();
            drop(ws1);
            // 连接 2：重连握手后按序收到缓冲的两条 note-sync
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws2 = handshake(accept_ws(stream).await).await;
            for (frame, file, payload) in [
                (read_json(&mut ws2).await, "notes/a.md", server_a),
                (read_json(&mut ws2).await, "notes/b.md", server_b),
            ] {
                assert_eq!(frame["type"], "note-sync");
                assert_eq!(frame["file"], file);
                let decoded = base64::engine::general_purpose::STANDARD
                    .decode(frame["payload"].as_str().unwrap())
                    .unwrap();
                assert_eq!(decoded, payload);
            }
        });
        let (cmd_tx, mut events) =
            spawn_pump_with(&format!("http://{addr}"), test_hello(), test_config());
        assert_eq!(
            recv_event(&mut events).await,
            SpaceEvent::Ready { peer_id: "1".to_string() }
        );
        kill_tx.send(()).ok();
        assert_eq!(recv_event(&mut events).await, SpaceEvent::Disconnected);
        // 断线期间发送：应进缓冲，重连后按序冲刷
        cmd_tx
            .send(SpaceCmd::NoteSync { file: "notes/a.md".to_string(), payload: payload_a })
            .unwrap();
        cmd_tx
            .send(SpaceCmd::NoteSync { file: "notes/b.md".to_string(), payload: payload_b })
            .unwrap();
        assert_eq!(
            recv_event(&mut events).await,
            SpaceEvent::Reconnecting { attempt: 1 }
        );
        assert_eq!(
            recv_event(&mut events).await,
            SpaceEvent::Ready { peer_id: "1".to_string() }
        );
        server.await.unwrap();
    }

    fn plugin_event(peer_id: &str, payload: Value) -> SpaceEvent {
        SpaceEvent::PluginMsg {
            peer_id: peer_id.to_string(),
            channel: "chat".to_string(),
            payload,
        }
    }

    /// 插件消息：连续广播按序直达、重复序号丢弃、单播无序号原样直达；
    /// 上行编码带 channel / payload 与数值 targetPeerId。
    #[tokio::test]
    async fn plugin_messages_flow_without_duplicates() {
        let (base, server) = spawn_handshake_server(|mut ws| async move {
            send_json(&mut ws, json!({"type": "plugin-msg", "peerId": 2, "channel": "chat", "payload": {"n": 1}, "seq": 1})).await;
            send_json(&mut ws, json!({"type": "plugin-msg", "peerId": 2, "channel": "chat", "payload": {"n": 2}, "seq": 2})).await;
            send_json(&mut ws, json!({"type": "plugin-msg", "peerId": 2, "channel": "chat", "payload": {"n": 2}, "seq": 2})).await;
            send_json(&mut ws, json!({"type": "plugin-msg", "peerId": 3, "channel": "chat", "payload": {"n": 3}})).await;
            let frame = read_json(&mut ws).await;
            assert_eq!(frame, json!({"type": "plugin-msg", "channel": "chat", "payload": {"m": 1}}));
            let frame = read_json(&mut ws).await;
            assert_eq!(
                frame,
                json!({"type": "plugin-msg", "channel": "chat", "payload": {"m": 2}, "targetPeerId": 3})
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        })
        .await;
        let (cmd_tx, mut events) = spawn_pump_with(&base, test_hello(), test_config());
        assert!(matches!(recv_event(&mut events).await, SpaceEvent::Ready { .. }));
        cmd_tx
            .send(SpaceCmd::PluginMsg {
                channel: "chat".to_string(),
                payload: json!({"m": 1}),
                target_peer_id: None,
            })
            .unwrap();
        cmd_tx
            .send(SpaceCmd::PluginMsg {
                channel: "chat".to_string(),
                payload: json!({"m": 2}),
                target_peer_id: Some(3),
            })
            .unwrap();
        assert_eq!(recv_event(&mut events).await, plugin_event("2", json!({"n": 1})));
        assert_eq!(recv_event(&mut events).await, plugin_event("2", json!({"n": 2})));
        assert_eq!(recv_event(&mut events).await, plugin_event("3", json!({"n": 3})));
        server.await.unwrap();
    }

    /// 自己发出的广播帧：服务端只回 `plugin-msg-ack`，客户端凭它推进序号，
    /// 后续帧不因这处序号洞等补投宽限。
    #[tokio::test]
    async fn own_broadcast_ack_advances_sequence() {
        let (base, server) = spawn_handshake_server(|mut ws| async move {
            send_json(&mut ws, json!({"type": "plugin-msg-ack", "seq": 1})).await;
            send_json(&mut ws, json!({"type": "plugin-msg", "peerId": 2, "channel": "chat", "payload": {"n": 2}, "seq": 2})).await;
            tokio::time::sleep(Duration::from_millis(100)).await;
        })
        .await;
        // 宽限放到 5s：回执没生效时这里会卡到 recv_event 的 2s 超时
        let config = PumpConfig {
            plugin_gap_grace: Duration::from_secs(5),
            ..test_config()
        };
        let (_cmd_tx, mut events) = spawn_pump_with(&base, test_hello(), config);
        assert!(matches!(recv_event(&mut events).await, SpaceEvent::Ready { .. }));
        assert_eq!(recv_event(&mut events).await, plugin_event("2", json!({"n": 2})));
        server.await.unwrap();
    }

    /// 二进制插件帧占房间序号：宿主跳过它的载荷但推进序号，后续帧照常直达。
    #[tokio::test]
    async fn binary_plugin_frame_advances_sequence() {
        let (base, server) = spawn_handshake_server(|mut ws| async move {
            let channel = b"chat";
            let mut binary = vec![1u8, 0];
            binary.extend_from_slice(&(channel.len() as u16).to_le_bytes());
            binary.extend_from_slice(channel);
            binary.extend_from_slice(&1u64.to_le_bytes());
            binary.extend_from_slice(&2u64.to_le_bytes());
            binary.extend_from_slice(b"raw");
            let _ = ws.send(Message::Binary(binary.into())).await;
            send_json(&mut ws, json!({"type": "plugin-msg", "peerId": 2, "channel": "chat", "payload": {"n": 2}, "seq": 2})).await;
            tokio::time::sleep(Duration::from_millis(100)).await;
        })
        .await;
        let config = PumpConfig {
            plugin_gap_grace: Duration::from_secs(5),
            ..test_config()
        };
        let (_cmd_tx, mut events) = spawn_pump_with(&base, test_hello(), config);
        assert!(matches!(recv_event(&mut events).await, SpaceEvent::Ready { .. }));
        assert_eq!(recv_event(&mut events).await, plugin_event("2", json!({"n": 2})));
        server.await.unwrap();
    }

    /// 重连补投：首次加入不回放缓存帧，重连按已投递序号请求补投；
    /// 补投前先到的直播帧按序号暂存，缺口补齐后按序投递。
    #[tokio::test]
    async fn reconnect_replays_missed_plugin_frames_in_order() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            // 连接 1：发一条广播帧后断开
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws1 = handshake(accept_ws(stream).await).await;
            send_json(&mut ws1, json!({"type": "plugin-msg", "peerId": 2, "channel": "chat", "payload": {"n": 1}, "seq": 1})).await;
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(ws1);
            // 连接 2：序号头正常时从已投递序号之后补投
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws2 = handshake_with_plugin_seq(accept_ws(stream).await, 1).await;
            let replay = read_json(&mut ws2).await;
            assert_eq!(replay, json!({"type": "plugin-replay", "after": 1}));
            // 直播帧（seq 3）先于补投帧（seq 2）到达：客户端须按 2、3 投递
            send_json(&mut ws2, json!({"type": "plugin-msg", "peerId": 2, "channel": "chat", "payload": {"n": 3}, "seq": 3})).await;
            send_json(&mut ws2, json!({"type": "plugin-msg", "peerId": 2, "channel": "chat", "payload": {"n": 2}, "seq": 2})).await;
            tokio::time::sleep(Duration::from_millis(150)).await;
        });
        let (_cmd_tx, mut events) =
            spawn_pump_with(&format!("http://{addr}"), test_hello(), test_config());
        assert!(matches!(recv_event(&mut events).await, SpaceEvent::Ready { .. }));
        assert_eq!(recv_event(&mut events).await, plugin_event("2", json!({"n": 1})));
        assert_eq!(recv_event(&mut events).await, SpaceEvent::Disconnected);
        assert_eq!(recv_event(&mut events).await, SpaceEvent::Reconnecting { attempt: 1 });
        assert!(matches!(recv_event(&mut events).await, SpaceEvent::Ready { .. }));
        assert_eq!(recv_event(&mut events).await, plugin_event("2", json!({"n": 2})));
        assert_eq!(recv_event(&mut events).await, plugin_event("2", json!({"n": 3})));
        server.await.unwrap();
    }

    /// 缺口宽限：缺号超出补投窗口时不会永久卡住，宽限到点按现有帧补序投递；
    /// 之后迟到的缺号帧按重复丢弃，后续帧照常续上。
    #[tokio::test]
    async fn plugin_gap_flushes_after_grace() {
        let (base, server) = spawn_handshake_server(|mut ws| async move {
            send_json(&mut ws, json!({"type": "plugin-msg", "peerId": 2, "channel": "chat", "payload": {"n": 1}, "seq": 1})).await;
            send_json(&mut ws, json!({"type": "plugin-msg", "peerId": 2, "channel": "chat", "payload": {"n": 3}, "seq": 3})).await;
            // 超过宽限（测试 80ms）后再送缺号帧，验证弃等后按重复丢弃
            tokio::time::sleep(Duration::from_millis(200)).await;
            send_json(&mut ws, json!({"type": "plugin-msg", "peerId": 2, "channel": "chat", "payload": {"n": 2}, "seq": 2})).await;
            send_json(&mut ws, json!({"type": "plugin-msg", "peerId": 2, "channel": "chat", "payload": {"n": 4}, "seq": 4})).await;
            tokio::time::sleep(Duration::from_millis(100)).await;
        })
        .await;
        // 服务端静默 200ms 等宽限到点：放宽半开阈值，避免静默期间被判假死重连
        let config = PumpConfig {
            dead_after: Duration::from_millis(400),
            ..test_config()
        };
        let (_cmd_tx, mut events) = spawn_pump_with(&base, test_hello(), config);
        assert!(matches!(recv_event(&mut events).await, SpaceEvent::Ready { .. }));
        assert_eq!(recv_event(&mut events).await, plugin_event("2", json!({"n": 1})));
        assert_eq!(recv_event(&mut events).await, plugin_event("2", json!({"n": 3})));
        assert_eq!(recv_event(&mut events).await, plugin_event("2", json!({"n": 4})));
        server.await.unwrap();
    }

    /// 序号空间重置：hello-ack 序号头小于已投递序号（房间重建）即归零，
    /// 按新空间从零补投，新空间的同号帧不被旧游标当作重复。
    #[tokio::test]
    async fn plugin_seq_space_reset_restarts_ordering() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws1 = handshake(accept_ws(stream).await).await;
            send_json(&mut ws1, json!({"type": "plugin-msg", "peerId": 2, "channel": "chat", "payload": {"n": 1}, "seq": 1})).await;
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(ws1);
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws2 = handshake_with_plugin_seq(accept_ws(stream).await, 0).await;
            let replay = read_json(&mut ws2).await;
            assert_eq!(replay, json!({"type": "plugin-replay", "after": 0}));
            send_json(&mut ws2, json!({"type": "plugin-msg", "peerId": 3, "channel": "chat", "payload": {"n": "new"}, "seq": 1})).await;
            tokio::time::sleep(Duration::from_millis(100)).await;
        });
        let (_cmd_tx, mut events) =
            spawn_pump_with(&format!("http://{addr}"), test_hello(), test_config());
        assert!(matches!(recv_event(&mut events).await, SpaceEvent::Ready { .. }));
        assert_eq!(recv_event(&mut events).await, plugin_event("2", json!({"n": 1})));
        assert_eq!(recv_event(&mut events).await, SpaceEvent::Disconnected);
        assert_eq!(recv_event(&mut events).await, SpaceEvent::Reconnecting { attempt: 1 });
        assert!(matches!(recv_event(&mut events).await, SpaceEvent::Ready { .. }));
        assert_eq!(recv_event(&mut events).await, plugin_event("3", json!({"n": "new"})));
        server.await.unwrap();
    }

    /// 首次加入活跃房间：序号头之前的帧发生在加入前，不补投、不等宽限，直播帧直接续上。
    #[tokio::test]
    async fn first_join_starts_after_plugin_seq_header() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = handshake_with_plugin_seq(accept_ws(stream).await, 5).await;
            send_json(&mut ws, json!({"type": "plugin-msg", "peerId": 2, "channel": "chat", "payload": {"n": 6}, "seq": 6})).await;
            tokio::time::sleep(Duration::from_millis(100)).await;
        });
        // 宽限放到 5s：游标没对齐时这里会卡到 recv_event 的 2s 超时
        let config = PumpConfig {
            plugin_gap_grace: Duration::from_secs(5),
            ..test_config()
        };
        let (_cmd_tx, mut events) =
            spawn_pump_with(&format!("http://{addr}"), test_hello(), config);
        assert!(matches!(recv_event(&mut events).await, SpaceEvent::Ready { .. }));
        assert_eq!(recv_event(&mut events).await, plugin_event("2", json!({"n": 6})));
        server.await.unwrap();
    }

    /// 重连后的新缺口仍会请求补投：重连自带的那次补投只覆盖断线前的缓存。
    #[tokio::test]
    async fn reconnect_requests_replay_for_new_gap() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws1 = handshake(accept_ws(stream).await).await;
            send_json(&mut ws1, json!({"type": "plugin-msg", "peerId": 2, "channel": "chat", "payload": {"n": 1}, "seq": 1})).await;
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(ws1);
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws2 = handshake_with_plugin_seq(accept_ws(stream).await, 1).await;
            assert_eq!(read_json(&mut ws2).await, json!({"type": "plugin-replay", "after": 1}));
            // 新连接上出现缺口：宽限到点须再请求一次补投，而不是直接弃等
            send_json(&mut ws2, json!({"type": "plugin-msg", "peerId": 2, "channel": "chat", "payload": {"n": 3}, "seq": 3})).await;
            assert_eq!(read_json(&mut ws2).await, json!({"type": "plugin-replay", "after": 1}));
            send_json(&mut ws2, json!({"type": "plugin-msg", "peerId": 2, "channel": "chat", "payload": {"n": 2}, "seq": 2})).await;
            tokio::time::sleep(Duration::from_millis(100)).await;
        });
        let (_cmd_tx, mut events) =
            spawn_pump_with(&format!("http://{addr}"), test_hello(), test_config());
        assert!(matches!(recv_event(&mut events).await, SpaceEvent::Ready { .. }));
        assert_eq!(recv_event(&mut events).await, plugin_event("2", json!({"n": 1})));
        assert_eq!(recv_event(&mut events).await, SpaceEvent::Disconnected);
        assert_eq!(recv_event(&mut events).await, SpaceEvent::Reconnecting { attempt: 1 });
        assert!(matches!(recv_event(&mut events).await, SpaceEvent::Ready { .. }));
        assert_eq!(recv_event(&mut events).await, plugin_event("2", json!({"n": 2})));
        assert_eq!(recv_event(&mut events).await, plugin_event("2", json!({"n": 3})));
        server.await.unwrap();
    }
}
