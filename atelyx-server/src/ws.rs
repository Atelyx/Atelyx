//! WS 共享房间机制（presence / 补丁 / 笔记同步 / 插件消息转发），
//! 空间频道 `/ws/space` 的底层：鉴权由 space_ws 完成后，入房与转发全走本模块。
//!
//! 协议（JSON over WS，字段 camelCase）：
//! - C→S `hello`：`{ type, spaceId, token, nickname, color, deviceName, version? }`（首条必发）
//! - C→S `presence`：`{ type, file?, selection?, view?, openFiles?, lockedNodes?, streamingNodeIds?, editingNotes? }`
//! - C→S `table-patch` / `canvas-patch`：`{ type, file, patch }`（不透明透传）
//! - C→S `note-sync` / `note-aware`：`{ type, file, payload }`（base64 载荷不透明透传）
//! - C→S `plugin-msg`：`{ type, channel, payload, targetPeerId? }`（广播/定向单播）
//! - C→S `plugin-replay`：`{ type, after }`（可靠补投：回放 seq > after 的缓存广播帧）
//! - C→S `ping`（保活，回 `pong` 广播）/ `bye`（离开）
//! - C→S 二进制帧：插件消息二进制载荷直传（布局见 `parse_plugin_binary`），与 `plugin-msg` 同语义
//! - S→C `hello-ack`：`{ type, peerId, pluginSeq }`（先于 peers 帧——客户端据此把自己过滤出列表；
//!   pluginSeq = 房间插件帧序号头，客户端据此判定序号空间是否重置）
//! - S→C `peers` / `presence` / 各转发帧（不含自己；`plugin-msg` 广播帧带房间级序号 `seq`）/
//!   `meta-changed`（团队 meta 落地广播，含写入者，只带 `key` 不带值）/ `resync`（慢消费者重新握手）/ `error`
//!
//! 插件消息可靠有序：广播帧按房间级单调 seq 分配并进环形缓存（条数/字节双上限，超限逐出最旧），
//! 重连后按 `plugin-replay` 的 after 回放缺帧（回放帧与直播帧同通道保序）；定向单播为尽力而为，
//! 不占序号不入缓存。房间清空时缓存随之移除。
//!
//! 日志红线：转发内容（patch / Yjs payload / selection）只记字节数不记内容。

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, watch};
use futures_util::{SinkExt, StreamExt};
use tracing::{debug, info, trace, warn};

/// 心跳超时：期间无任何消息（含 ping）即断开。
pub(crate) const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(30);

/// resync 下发最小间隔：慢消费者会连续落后，逐次下发会让客户端反复全量重握手（大帧反过来加重积压）。
const RESYNC_MIN_INTERVAL: Duration = Duration::from_secs(5);

static NEXT_PEER_ID: AtomicU64 = AtomicU64::new(1);

/// 全局房间表（房间 id → 房间）；hub 为 axum State。
#[derive(Clone, Default)]
pub struct Hub(Arc<Mutex<HashMap<String, Room>>>);

impl Hub {
    /// 全部房间的连接总数（管理台运行状态展示；每在线端一条连接）。
    pub(crate) fn total_connections(&self) -> usize {
        self.0.lock().unwrap().values().map(|room| room.peers.len()).sum()
    }

    /// 断开属于给定设备会话的实时连接（吊销会话/重置密码/移出空间后调用）：
    /// `room_id` 有值 = 只踢该房间（移出空间只断本空间连接，同会话在其他空间的连接不受影响）；
    /// 对命中连接置踢信号与原因，由连接自身投递失效帧并走离场收尾。返回命中连接数。
    pub(crate) fn kick_sessions(
        &self,
        room_id: Option<&str>,
        session_ids: &[String],
        reason: KickReason,
    ) -> usize {
        let mut kicked = 0;
        let rooms = self.0.lock().unwrap();
        for (id, room) in rooms.iter() {
            if let Some(want) = room_id {
                if id != want {
                    continue;
                }
            }
            for peer in room.peers.values() {
                if session_ids.contains(&peer.session_id) {
                    let _ = peer.kick_tx.send(Some(reason));
                    kicked += 1;
                }
            }
        }
        kicked
    }
}

/// 踢连接原因：决定下发给被踢端的失效帧文案——会话失效与被移出空间不是同一件事，
/// 用同一句话会让被移除者以为自己的登录出了问题。
#[derive(Clone, Copy)]
pub(crate) enum KickReason {
    SessionRevoked,
    SpaceMemberRemoved,
}

impl KickReason {
    fn message(self) -> &'static str {
        match self {
            KickReason::SessionRevoked => "登录状态已失效，连接已断开",
            KickReason::SpaceMemberRemoved => "你已不在该空间，连接已断开",
        }
    }
}

/// 下行帧（文本 = JSON 协议帧；二进制 = 插件消息二进制载荷直传）。
#[derive(Clone)]
pub(crate) enum WireFrame {
    Text(Arc<String>),
    Binary(Arc<Vec<u8>>),
}

impl WireFrame {
    fn len(&self) -> usize {
        match self {
            WireFrame::Text(s) => s.len(),
            WireFrame::Binary(b) => b.len(),
        }
    }
}

/// WireFrame → axum WS 消息（发送任务与兜底帧共用）。
fn wire_to_message(frame: &WireFrame) -> Message {
    match frame {
        WireFrame::Text(s) => Message::text((**s).clone()),
        WireFrame::Binary(b) => Message::Binary(b.as_ref().clone().into()),
    }
}

/// 房间 = 在线连接 + 插件广播帧缓存（可靠补投）。
#[derive(Default)]
struct Room {
    peers: HashMap<u64, PeerEntry>,
    /// 房间级插件帧序号（单调递增；广播帧分配并缓存，单播帧不占序号不入缓存）。
    plugin_seq: u64,
    /// 近期广播插件帧缓存（可靠补投）：条数/字节双上限，超限逐出最旧。
    plugin_cache: VecDeque<CachedPluginFrame>,
    plugin_cache_bytes: usize,
}

/// 插件帧缓存上限（条数）：补投单批 ≤ 本值 < 出站广播通道容量（256），回放不会自我拥塞。
const PLUGIN_CACHE_MAX_ENTRIES: usize = 128;
/// 插件帧缓存上限（字节）：防大载荷刷爆内存，与条数上限先到先逐出。
const PLUGIN_CACHE_MAX_BYTES: usize = 4 * 1024 * 1024;

struct CachedPluginFrame {
    seq: u64,
    frame: WireFrame,
}

impl Room {
    /// 追加缓存一条广播插件帧（超限逐出最旧，缓存保持 seq 升序）。
    fn cache_plugin_frame(&mut self, seq: u64, frame: WireFrame) {
        self.plugin_cache_bytes += frame.len();
        self.plugin_cache.push_back(CachedPluginFrame { seq, frame });
        while self.plugin_cache.len() > PLUGIN_CACHE_MAX_ENTRIES
            || self.plugin_cache_bytes > PLUGIN_CACHE_MAX_BYTES
        {
            let Some(oldest) = self.plugin_cache.pop_front() else {
                break;
            };
            self.plugin_cache_bytes -= oldest.frame.len();
        }
    }
}

struct PeerEntry {
    nickname: String,
    color: String,
    device_name: String,
    version: Option<String>,
    presence: Option<Presence>,
    tx: broadcast::Sender<WireFrame>,
    /// 所属设备会话（Hub::kick_sessions 按此匹配踢连接；不进 peers 快照广播）。
    session_id: String,
    /// 踢连接信号（Some = 原因）：会话被吊销/移出空间时由 Hub 触发。
    kick_tx: watch::Sender<Option<KickReason>>,
}

/// 单连接收发计数（离场随总结日志输出，用于定位流量异常/刷屏客户端）。
#[derive(Default)]
struct ConnStats {
    received_msgs: u64,
    received_bytes: u64,
    forwarded_msgs: u64,
    forwarded_bytes: u64,
}

/// 已验证入房者的展示身份（nickname 空缺由调用方落到合理缺省）。
pub struct PeerMeta {
    pub nickname: String,
    pub color: String,
    pub device_name: String,
    pub version: Option<String>,
    /// 所属设备会话（吊销会话时按此踢连接）。
    pub session_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ClientMsg {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub selection: Option<serde_json::Value>,
    #[serde(default)]
    pub view: Option<String>,
    /// 打开文件清单（协作房间面板；不透明透传，服务端不解析）。
    #[serde(default)]
    pub open_files: Option<serde_json::Value>,
    /// 画布对话独占锁声明（presence 跨视图保活；不透明透传）。
    #[serde(default)]
    pub locked_nodes: Option<serde_json::Value>,
    /// 画布正在 AI 生成的对话节点（生成灯；不透明透传）。
    #[serde(default)]
    pub streaming_node_ids: Option<serde_json::Value>,
    /// 本端已打开编辑面的笔记（跨视图互见；不透明透传）。
    #[serde(default)]
    pub editing_notes: Option<serde_json::Value>,
    /// 表格/画布增量补丁（不透明透传，服务端不解析内容）。
    #[serde(default)]
    pub patch: Option<serde_json::Value>,
    /// 笔记协作同步/awareness（Yjs 二进制经 base64 包装）或插件消息载荷。不透明透传。
    #[serde(default)]
    pub payload: Option<serde_json::Value>,
    /// 插件消息频道名（不解析，按频道转发）。
    #[serde(default)]
    pub channel: Option<String>,
    /// 插件消息定向目标（单播：只转发给该 peer；缺省 = 广播房间内其他成员）。
    #[serde(default)]
    pub target_peer_id: Option<u64>,
    /// 可靠补投请求（`plugin-replay`）：客户端已收到的房间级插件帧序号，回放 seq > after 的缓存帧。
    #[serde(default)]
    pub after: Option<u64>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Presence {
    file: Option<String>,
    selection: Option<serde_json::Value>,
    view: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    open_files: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    locked_nodes: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    streaming_node_ids: Option<serde_json::Value>,
    /// 本端已打开编辑面的笔记（跨视图互见：画布节点上编辑笔记时聚焦文件仍是画布）。
    #[serde(skip_serializing_if = "Option::is_none")]
    editing_notes: Option<serde_json::Value>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PeerInfo {
    peer_id: u64,
    nickname: String,
    color: String,
    device_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    presence: Option<Presence>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ServerMsg {
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    peer_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    peers: Option<Vec<PeerInfo>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    presence: Option<Presence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    patch: Option<serde_json::Value>,
    /// 插件消息频道名（`plugin-msg` 转发帧携带）。
    #[serde(skip_serializing_if = "Option::is_none")]
    channel: Option<String>,
    /// 载荷（`note-sync`/`note-aware` 为 base64 字符串，`plugin-msg` 为任意 JSON）。
    #[serde(skip_serializing_if = "Option::is_none")]
    payload: Option<serde_json::Value>,
    /// 插件广播帧的房间级序号（可靠补投对账；单播帧不携带）。
    #[serde(skip_serializing_if = "Option::is_none")]
    seq: Option<u64>,
    /// 房间插件帧序号头（`hello-ack` 携带；客户端据此判定序号空间是否重置）。
    #[serde(skip_serializing_if = "Option::is_none")]
    plugin_seq: Option<u64>,
    /// 元数据键名（`meta-changed` 落地广播帧携带；只广播键名，值由客户端回读磁盘真源）。
    #[serde(skip_serializing_if = "Option::is_none")]
    key: Option<String>,
}

fn server_msg(
    kind: &'static str,
    peer_id: Option<u64>,
    peers: Option<Vec<PeerInfo>>,
    presence: Option<Presence>,
    file: Option<String>,
    patch: Option<serde_json::Value>,
    payload: Option<serde_json::Value>,
) -> WireFrame {
    WireFrame::Text(server_text_msg(ServerMsg {
        kind,
        peer_id,
        peers,
        presence,
        file,
        patch,
        channel: None,
        payload,
        seq: None,
        plugin_seq: None,
        key: None,
    }))
}

fn server_text_msg(msg: ServerMsg) -> Arc<String> {
    Arc::new(serde_json::to_string(&msg).unwrap())
}

/// 构造 hello-ack（分配 peerId + 房间插件序号头；客户端据此过滤自己并判定序号空间）。
fn server_hello_ack(peer_id: u64, plugin_seq: u64) -> WireFrame {
    WireFrame::Text(server_text_msg(ServerMsg {
        kind: "hello-ack",
        peer_id: Some(peer_id),
        peers: None,
        presence: None,
        file: None,
        patch: None,
        channel: None,
        payload: None,
        seq: None,
        plugin_seq: Some(plugin_seq),
        key: None,
    }))
}

/// 构造插件消息转发帧（转发帧不携带定向目标——接收方无需知道是否定向）；
/// seq = 房间级序号（广播帧携带，单播帧 None）。
fn server_plugin_msg(
    peer_id: u64,
    channel: String,
    payload: serde_json::Value,
    seq: Option<u64>,
) -> WireFrame {
    WireFrame::Text(server_text_msg(ServerMsg {
        kind: "plugin-msg",
        peer_id: Some(peer_id),
        peers: None,
        presence: None,
        file: None,
        patch: None,
        channel: Some(channel),
        payload: Some(payload),
        seq,
        plugin_seq: None,
        key: None,
    }))
}

pub(crate) fn peer_error(text: &str) -> String {
    serde_json::json!({ "type": "error", "message": text }).to_string()
}

/// 向房间全员广播 peers 全量快照（成员加入/离开时调用）。
fn broadcast_peers(rooms: &HashMap<String, Room>, room_id: &str) {
    let Some(room) = rooms.get(room_id) else {
        return;
    };
    let peers: Vec<PeerInfo> = room
        .peers
        .iter()
        .map(|(id, p)| PeerInfo {
            peer_id: *id,
            nickname: p.nickname.clone(),
            color: p.color.clone(),
            device_name: p.device_name.clone(),
            version: p.version.clone(),
            presence: p.presence.clone(),
        })
        .collect();
    let payload = server_msg("peers", None, Some(peers), None, None, None, None);
    for p in room.peers.values() {
        let _ = p.tx.send(payload.clone());
    }
}

/// 把已构建的转发帧送房间内除发送者外的全部成员（presence/补丁/笔记同步共用转发循环）。
fn forward_to_room(
    rooms: &mut HashMap<String, Room>,
    room_id: &str,
    sender_id: u64,
    payload: WireFrame,
) {
    if let Some(room) = rooms.get_mut(room_id) {
        for (id, peer) in room.peers.iter() {
            if *id != sender_id {
                let _ = peer.tx.send(payload.clone());
            }
        }
    }
}

/// 是否允许下发 resync：首次（`last` 为 None）允许，其后需过最小间隔。
fn resync_due(last: Option<Instant>, now: Instant) -> bool {
    match last {
        None => true,
        Some(at) => now.saturating_duration_since(at) >= RESYNC_MIN_INTERVAL,
    }
}

/// 服务端主动向房间广播补丁帧（HTTP 补丁端点落地成功后调用）。发给房间内全部成员的
/// WS 连接，含发起写入者自己——发起者经 HTTP 保存、无转发帧可回声，收到的是落地广播帧；
/// 帧无 peerId，客户端按远端补丁同一路径幂等应用。房间为空或个别投递失败只记日志：
/// 真源已落盘，广播失败不回滚落地。
pub(crate) fn broadcast_patch(hub: &Hub, room_id: &str, kind: &'static str, file: &str, patch: serde_json::Value) {
    let payload = server_msg(kind, None, None, None, Some(file.to_string()), Some(patch), None);
    let rooms = hub.0.lock().unwrap();
    if let Some(room) = rooms.get(room_id) {
        for peer in room.peers.values() {
            if peer.tx.send(payload.clone()).is_err() {
                debug!(room = %room_id, kind = %kind, "补丁帧投递失败（接收端已关闭）");
            }
        }
    }
}

/// 服务端主动向房间广播改名帧（rename 端点落地成功后调用）。发给房间内全部成员，含发起者
/// 自己——发起者已本地跟随，重复跟随为 no-op。客户端据此把打开中的文件即时切到新路径，
/// 不再依赖滞后的文件树刷新（滞后感知会误判删除并静默关闭打开的文件）。房间为空或个别
/// 投递失败只记日志：真源已落盘，广播失败不回滚落地。
pub(crate) fn broadcast_rename(hub: &Hub, room_id: &str, old_path: &str, new_path: &str) {
    let payload = server_msg(
        "renamed",
        None,
        None,
        None,
        None,
        None,
        Some(serde_json::json!({ "oldPath": old_path, "newPath": new_path })),
    );
    let rooms = hub.0.lock().unwrap();
    if let Some(room) = rooms.get(room_id) {
        for peer in room.peers.values() {
            if peer.tx.send(payload.clone()).is_err() {
                debug!(room = %room_id, "改名帧投递失败（接收端已关闭）");
            }
        }
    }
}

/// 服务端主动向房间广播元数据变更帧（团队层 meta 写/删端点落地成功后调用）。发给房间内全部成员，
/// 含发起写入者自己——发起者经 HTTP 保存、无转发帧可回声，收到的是落地广播帧；帧只带键名不带值，
/// 客户端据此回读磁盘真源。房间为空或个别投递失败只记日志：真源已落盘，广播失败不回滚落地。
pub(crate) fn broadcast_meta_changed(hub: &Hub, room_id: &str, key: &str) {
    let payload = WireFrame::Text(server_text_msg(ServerMsg {
        kind: "meta-changed",
        peer_id: None,
        peers: None,
        presence: None,
        file: None,
        patch: None,
        channel: None,
        payload: None,
        seq: None,
        plugin_seq: None,
        key: Some(key.to_string()),
    }));
    let rooms = hub.0.lock().unwrap();
    if let Some(room) = rooms.get(room_id) {
        for peer in room.peers.values() {
            if peer.tx.send(payload.clone()).is_err() {
                debug!(room = %room_id, key = %key, "meta 变更帧投递失败（接收端已关闭）");
            }
        }
    }
}

// ===== 插件消息二进制帧编解码（与前端 framePump.ts 逐字节同构） =====
// 布局（小端）：[0]=kind(1) [1]=flags(bit0=有 targetPeerId) [2..3]=channel 字节长
// [channel utf8] [8B seq] [8B targetPeerId（flags.bit0 时）] [8B senderPeerId] [payload 原样字节]。

const PLUGIN_BINARY_KIND: u8 = 1;
const PLUGIN_BINARY_HAS_TARGET: u8 = 0b1;
/// 帧头：kind(1) + flags(1) + channel 字节长(2)。
const PLUGIN_BINARY_HEADER: usize = 4;
/// channel 之后的固定尾段：seq(8) + senderPeerId(8)。
const PLUGIN_BINARY_TAIL: usize = 8 + 8;

/// 解析后的二进制插件帧。
struct BinaryPluginFrame {
    channel: String,
    payload: Vec<u8>,
    seq: u64,
    target_peer_id: Option<u64>,
    sender_peer_id: u64,
}

/// 解析二进制插件帧（kind 不符 / channel 非 UTF-8 / 长度不足返回 None，调用方忽略）。
fn parse_plugin_binary(data: &[u8]) -> Option<BinaryPluginFrame> {
    if data.len() < PLUGIN_BINARY_HEADER || data[0] != PLUGIN_BINARY_KIND {
        return None;
    }
    let name_len = u16::from_le_bytes([data[2], data[3]]) as usize;
    if PLUGIN_BINARY_HEADER + name_len + 8 > data.len() {
        return None;
    }
    let channel =
        String::from_utf8(data[PLUGIN_BINARY_HEADER..PLUGIN_BINARY_HEADER + name_len].to_vec())
            .ok()?;
    let mut offset = PLUGIN_BINARY_HEADER + name_len;
    let read_u64 = |off: usize| -> Option<u64> {
        data.get(off..off + 8)?.try_into().ok().map(u64::from_le_bytes)
    };
    let seq = read_u64(offset)?;
    offset += 8;
    let mut target_peer_id = None;
    if data[1] & PLUGIN_BINARY_HAS_TARGET != 0 {
        target_peer_id = Some(read_u64(offset)?);
        offset += 8;
    }
    let sender_peer_id = read_u64(offset)?;
    offset += 8;
    Some(BinaryPluginFrame {
        channel,
        payload: data[offset..].to_vec(),
        seq,
        target_peer_id,
        sender_peer_id,
    })
}

/// 构建下行二进制插件帧（seq/sender 由服务端回填；转发帧不携带定向目标）。
fn build_plugin_binary(frame: &BinaryPluginFrame) -> Vec<u8> {
    let name = frame.channel.as_bytes();
    let mut out = Vec::with_capacity(
        PLUGIN_BINARY_HEADER + name.len() + PLUGIN_BINARY_TAIL + frame.payload.len(),
    );
    out.push(PLUGIN_BINARY_KIND);
    out.push(0); // 下行不带定向目标
    out.extend_from_slice(&(name.len() as u16).to_le_bytes());
    out.extend_from_slice(name);
    out.extend_from_slice(&frame.seq.to_le_bytes());
    out.extend_from_slice(&frame.sender_peer_id.to_le_bytes());
    out.extend_from_slice(&frame.payload);
    out
}

/// 已验证的连接进入房间：hello-ack → 入房广播 → 会话有效性回查 → 消息循环 → 离场收尾。
/// 鉴权由调用方在进入本函数之前完成；`session_alive` 回查设备会话是否仍有效
/// （鉴权与入房之间会话可能已被吊销，kick_sessions 触及不到尚未入房的连接）。
pub(crate) async fn run_room_connection(
    socket: WebSocket,
    hub: Hub,
    remote: SocketAddr,
    room_id: String,
    meta: PeerMeta,
    session_alive: impl Fn(&str) -> bool,
) {
    let (mut sink, mut stream) = socket.split();
    let started_at = Instant::now();
    // 踢连接信号（会话被吊销/移出空间时由 Hub::kick_sessions 置原因）
    let (kick_tx, mut kick_rx) = watch::channel::<Option<KickReason>>(None);
    let kick_frame = |reason: KickReason| {
        WireFrame::Text(Arc::new(
            serde_json::json!({ "type": "error", "message": reason.message() }).to_string(),
        ))
    };

    let peer_id = NEXT_PEER_ID.fetch_add(1, Ordering::Relaxed);
    let mut stats = ConnStats::default();
    let (btx, _) = broadcast::channel::<WireFrame>(256);
    // 后台转发必须先就位（订阅 receiver），再入房广播——否则本连接的首次 peers 帧
    // （含自己）在 send_task 启动前被 send 丢弃（broadcast 无 receiver 时 send 直接 Err）
    let mut btx_rx = btx.subscribe();
    let send_task = tokio::spawn(async move {
        let mut last_resync: Option<Instant> = None;
        loop {
            match btx_rx.recv().await {
                Ok(frame) => {
                    if sink.send(wire_to_message(&frame)).await.is_err() {
                        debug!(peer_id, "发送失败，客户端断开");
                        break;
                    }
                }
                // 消费过慢被广播层裁剪（lagged）：被裁的帧收不回来，下发 resync 让客户端重新握手
                // （笔记域重发 syncStep1 索取对端全量状态），否则内容静默分歧；
                // 按最小间隔下发，避免持续落后时反复触发全量重握手（其大帧反过来加重积压）
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    debug!(peer_id, dropped = n, "发送队列过慢，跳过被裁剪帧");
                    if resync_due(last_resync, Instant::now()) {
                        last_resync = Some(Instant::now());
                        warn!(peer_id, "下发 resync：客户端需重新握手补齐被裁剪的状态");
                        let resync = server_msg("resync", None, None, None, None, None, None);
                        if sink.send(wire_to_message(&resync)).await.is_err() {
                            debug!(peer_id, "发送失败，客户端断开");
                            break;
                        }
                    }
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    let nickname = meta.nickname.clone();
    let device_name = meta.device_name.clone();
    let version = meta.version.clone();
    let session_id = meta.session_id.clone();
    {
        // 入房 + hello-ack 同锁内完成：hello-ack 先于 peers 帧（客户端据此把自己过滤出列表），
        // 并带房间插件序号头（客户端据此判定序号空间是否重置）
        let mut rooms = hub.0.lock().unwrap();
        let room = rooms.entry(room_id.clone()).or_default();
        room.peers.insert(
            peer_id,
            PeerEntry {
                nickname: meta.nickname,
                color: meta.color,
                device_name: meta.device_name,
                version: meta.version,
                presence: None,
                tx: btx.clone(),
                session_id: meta.session_id,
                kick_tx,
            },
        );
        let _ = btx.send(server_hello_ack(peer_id, room.plugin_seq));
        info!(
            peer_id,
            room = %room_id,
            nickname = %nickname,
            device_name = %device_name,
            version = version.as_deref().unwrap_or(""),
            %remote,
            room_size = room.peers.len(),
            "协作者加入房间",
        );
        broadcast_peers(&rooms, &room_id);
    }
    // 入房后回查会话有效性：鉴权与入房之间会话可能已被吊销（kick_sessions 只触及已入房
    // 连接），此处回查补上这一窗口；入房之后的吊销由踢信号（watch）覆盖，两段合起来无空档
    let mut kicked = !session_alive(&session_id);
    if kicked {
        let _ = btx.send(kick_frame(KickReason::SessionRevoked));
    }

    // 消息循环：30s 无消息（心跳超时）断开；会话被吊销时投递失效帧后断开
    while !kicked {
        let recv = tokio::select! {
            r = tokio::time::timeout(HEARTBEAT_TIMEOUT, stream.next()) => r,
            _ = kick_rx.changed() => {
                if let Some(reason) = *kick_rx.borrow_and_update() {
                    // 失效帧经自身广播通道投递（发送任务负责写 socket），随后走离场收尾
                    let _ = btx.send(kick_frame(reason));
                    kicked = true;
                    break;
                }
                // 非踢信号（防御性：发送方只写 Some），继续等消息
                continue;
            }
        };
        match recv {
            Err(_) => {
                warn!(peer_id, room = %room_id, "心跳超时（30s 无消息），断开");
                break;
            }
            Ok(None) => {
                debug!(peer_id, room = %room_id, "连接关闭（对端断开）");
                break;
            }
            Ok(Some(Err(_))) => {
                warn!(peer_id, room = %room_id, "WebSocket 协议错误，断开");
                break;
            }
            Ok(Some(Ok(Message::Text(text)))) => {
                stats.received_msgs += 1;
                stats.received_bytes += text.len() as u64;
                let Ok(msg) = serde_json::from_str::<ClientMsg>(&text) else {
                    // 只记字节数不记原文——原文可能含用户文本
                    warn!(peer_id, bytes = text.len(), "消息解析失败，忽略");
                    continue;
                };
                match msg.kind.as_str() {
                    "presence" => {
                        let presence = Presence {
                            file: msg.file,
                            selection: msg.selection,
                            view: msg.view,
                            open_files: msg.open_files,
                            locked_nodes: msg.locked_nodes,
                            streaming_node_ids: msg.streaming_node_ids,
                            editing_notes: msg.editing_notes,
                        };
                        // presence 高频（选中节流后仍密集）：debug 只记文件/视图与清单数量，
                        // 不记 selection 内容（内容可能含用户文本/图片选区）
                        debug!(
                            peer_id,
                            room = %room_id,
                            file = presence.file.as_deref().unwrap_or(""),
                            view = presence.view.as_deref().unwrap_or(""),
                            open_files = presence.open_files.as_ref().and_then(|v| v.as_array()).map_or(0, |a| a.len()),
                            locked_nodes = presence.locked_nodes.as_ref().and_then(|v| v.as_array()).map_or(0, |a| a.len()),
                            streaming_nodes = presence.streaming_node_ids.as_ref().and_then(|v| v.as_array()).map_or(0, |a| a.len()),
                            editing_notes = presence.editing_notes.as_ref().and_then(|v| v.as_array()).map_or(0, |a| a.len()),
                            "presence 转发",
                        );
                        let mut rooms = hub.0.lock().unwrap();
                        if let Some(room) = rooms.get_mut(&room_id) {
                            if let Some(peer) = room.peers.get_mut(&peer_id) {
                                peer.presence = Some(presence.clone());
                            }
                            let payload =
                                server_msg("presence", Some(peer_id), None, Some(presence), None, None, None);
                            stats.forwarded_msgs += 1;
                            stats.forwarded_bytes += payload.len() as u64;
                            forward_to_room(&mut rooms, &room_id, peer_id, payload);
                        }
                    }
                    // 表格/画布内容补丁：同构透传（不存储、不解析内容，原样转发房间内其他成员，
                    // 客户端按 file 匹配只应用当前打开的文件）
                    "table-patch" | "canvas-patch" => {
                        if let (Some(file), Some(patch)) = (msg.file, msg.patch) {
                            debug!(peer_id, room = %room_id, kind = %msg.kind, file = %file, "内容补丁转发");
                            let mut rooms = hub.0.lock().unwrap();
                            let payload = server_msg(
                                if msg.kind == "table-patch" { "table-patch" } else { "canvas-patch" },
                                Some(peer_id),
                                None,
                                None,
                                Some(file),
                                Some(patch),
                                None,
                            );
                            stats.forwarded_msgs += 1;
                            stats.forwarded_bytes += payload.len() as u64;
                            forward_to_room(&mut rooms, &room_id, peer_id, payload);
                        }
                    }
                    // 笔记协作同步 / awareness：不透明透传（base64 载荷），原样转发房间内其他成员
                    // （客户端按 file 匹配只合入当前打开的笔记）
                    "note-sync" | "note-aware" => {
                        if let (Some(file), Some(payload)) = (
                            msg.file,
                            msg.payload.and_then(|p| p.as_str().map(str::to_string)),
                        ) {
                            debug!(peer_id, room = %room_id, kind = %msg.kind, file = %file, bytes = payload.len(), "笔记同步转发");
                            let mut rooms = hub.0.lock().unwrap();
                            let relayed = server_msg(
                                if msg.kind == "note-sync" { "note-sync" } else { "note-aware" },
                                Some(peer_id),
                                None,
                                None,
                                Some(file),
                                None,
                                Some(serde_json::Value::String(payload)),
                            );
                            stats.forwarded_msgs += 1;
                            stats.forwarded_bytes += relayed.len() as u64;
                            forward_to_room(&mut rooms, &room_id, peer_id, relayed);
                        }
                    }
                    // 插件通用消息：channel + payload 任意 JSON 不透明透传（不解析内容）。
                    // 广播帧分配房间级 seq 并入缓存（可靠补投，同锁内「缓存+转发」保证序）；
                    // 带 targetPeerId 的单播为尽力而为（不占序号不入缓存），目标不在线或指向自己
                    // 即丢弃，与广播「不含自己」语义一致。日志只记频道/字节数/目标，不记载荷内容。
                    "plugin-msg" => {
                        if let (Some(channel), Some(payload)) = (msg.channel, msg.payload) {
                            // 空频道与二进制路径同口径拒绝（正常客户端经宿主命名空间不会发出）
                            if channel.is_empty() {
                                continue;
                            }
                            debug!(
                                peer_id,
                                room = %room_id,
                                channel = %channel,
                                target_peer = ?msg.target_peer_id,
                                bytes = text.len(),
                                "插件消息转发",
                            );
                            stats.forwarded_msgs += 1;
                            let mut rooms = hub.0.lock().unwrap();
                            match msg.target_peer_id {
                                Some(target) => {
                                    if target != peer_id {
                                        if let Some(room) = rooms.get(&room_id) {
                                            if let Some(peer) = room.peers.get(&target) {
                                                let frame =
                                                    server_plugin_msg(peer_id, channel, payload, None);
                                                stats.forwarded_bytes += frame.len() as u64;
                                                let _ = peer.tx.send(frame);
                                            }
                                        }
                                    }
                                }
                                None => {
                                    if let Some(room) = rooms.get_mut(&room_id) {
                                        room.plugin_seq += 1;
                                        let seq = room.plugin_seq;
                                        let frame =
                                            server_plugin_msg(peer_id, channel, payload, Some(seq));
                                        stats.forwarded_bytes += frame.len() as u64;
                                        room.cache_plugin_frame(seq, frame.clone());
                                        for (id, peer) in room.peers.iter() {
                                            if *id != peer_id {
                                                let _ = peer.tx.send(frame.clone());
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    // 可靠补投：回放 seq > after 的缓存广播帧（经本连接出站通道，与直播帧保序）。
                    // 回放上限 = 缓存条数上限 < 通道容量，回放本身不会触发 Lagged。
                    "plugin-replay" => {
                        if let Some(after) = msg.after {
                            let mut rooms = hub.0.lock().unwrap();
                            if let Some(room) = rooms.get_mut(&room_id) {
                                let mut replayed = 0usize;
                                for cached in &room.plugin_cache {
                                    if cached.seq > after {
                                        let _ = btx.send(cached.frame.clone());
                                        replayed += 1;
                                    }
                                }
                                debug!(peer_id, room = %room_id, after, replayed, "插件帧补投");
                            }
                        }
                    }
                    "bye" => {
                        info!(peer_id, room = %room_id, "协作者离开（bye）");
                        break;
                    }
                    // 心跳回执：回 pong 广播（健康连接每 ≤25s 有人 ping，全员 lastMessageAt 刷新，
                    // 前端据此 75s 静默即判半开假死主动重连；单人房间亦收到自己的 pong，无空转误判）
                    "ping" => {
                        trace!(peer_id, "ping → pong");
                        let _ = btx.send(server_msg("pong", None, None, None, None, None, None));
                    }
                    other => {
                        debug!(peer_id, kind = %other, "未知消息类型，忽略");
                        continue;
                    }
                }
            }
            Ok(Some(Ok(Message::Binary(data)))) => {
                stats.received_msgs += 1;
                stats.received_bytes += data.len() as u64;
                // 二进制帧 = 插件消息二进制载荷直传：广播帧分配房间级 seq 并入缓存（可靠补投），
                // 单播帧尽力而为；畸形/未知帧与 JSON 未知类型同口径忽略（只记字节数）
                let Some(mut frame) = parse_plugin_binary(&data) else {
                    debug!(peer_id, bytes = data.len(), "忽略无法解析的二进制帧");
                    continue;
                };
                if frame.channel.is_empty() {
                    continue;
                }
                debug!(
                    peer_id,
                    room = %room_id,
                    channel = %frame.channel,
                    target_peer = ?frame.target_peer_id,
                    bytes = data.len(),
                    "插件二进制消息转发",
                );
                stats.forwarded_msgs += 1;
                frame.sender_peer_id = peer_id;
                let mut rooms = hub.0.lock().unwrap();
                match frame.target_peer_id {
                    Some(target) => {
                        if target != peer_id {
                            if let Some(room) = rooms.get(&room_id) {
                                if let Some(peer) = room.peers.get(&target) {
                                    frame.seq = 0;
                                    let wire = WireFrame::Binary(Arc::new(build_plugin_binary(&frame)));
                                    stats.forwarded_bytes += wire.len() as u64;
                                    let _ = peer.tx.send(wire);
                                }
                            }
                        }
                    }
                    None => {
                        if let Some(room) = rooms.get_mut(&room_id) {
                            room.plugin_seq += 1;
                            frame.seq = room.plugin_seq;
                            let wire = WireFrame::Binary(Arc::new(build_plugin_binary(&frame)));
                            stats.forwarded_bytes += wire.len() as u64;
                            room.cache_plugin_frame(frame.seq, wire.clone());
                            for (id, peer) in room.peers.iter() {
                                if *id != peer_id {
                                    let _ = peer.tx.send(wire.clone());
                                }
                            }
                        }
                    }
                }
            }
            Ok(Some(Ok(_))) => {
                debug!(peer_id, "忽略非文本帧");
                continue;
            }
        }
    }

    // 离开：移出房间 + 广播更新后的 peers（房间空则整体移除，插件帧缓存随之丢弃）
    {
        let mut rooms = hub.0.lock().unwrap();
        if let Some(room) = rooms.get_mut(&room_id) {
            room.peers.remove(&peer_id);
            if room.peers.is_empty() {
                rooms.remove(&room_id);
            } else {
                broadcast_peers(&rooms, &room_id);
            }
        }
    }
    info!(
        peer_id,
        room = %room_id,
        kicked,
        duration_ms = started_at.elapsed().as_millis() as u64,
        received_msgs = stats.received_msgs,
        received_bytes = stats.received_bytes,
        forwarded_msgs = stats.forwarded_msgs,
        forwarded_bytes = stats.forwarded_bytes,
        "协作者连接结束",
    );
    // 先丢弃本端发送者：发送任务排空已入队帧（含失效帧）后收到 Closed 自行结束。
    // 限时兜底——对端异常卡住写侧时不拖住连接收尾（超时即分离任务，socket 关闭）
    drop(btx);
    let _ = tokio::time::timeout(Duration::from_secs(2), send_task).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试辅助：按客户端上行格式编码二进制插件帧（seq/sender 置 0，可选定向目标）。
    fn encode_client_plugin_binary(channel: &str, payload: &[u8], target: Option<u64>) -> Vec<u8> {
        let name = channel.as_bytes();
        let mut out =
            Vec::with_capacity(PLUGIN_BINARY_HEADER + name.len() + PLUGIN_BINARY_TAIL + payload.len());
        out.push(PLUGIN_BINARY_KIND);
        out.push(if target.is_some() { PLUGIN_BINARY_HAS_TARGET } else { 0 });
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(name);
        out.extend_from_slice(&0u64.to_le_bytes()); // seq：服务端分配
        if let Some(t) = target {
            out.extend_from_slice(&t.to_le_bytes());
        }
        out.extend_from_slice(&0u64.to_le_bytes()); // senderPeerId：服务端回填
        out.extend_from_slice(payload);
        out
    }

    /// resync 是纯类型帧：客户端只据 `type` 重新握手，帧内不带任何多余字段。
    #[test]
    fn resync_frame_carries_type_only() {
        let frame = server_msg("resync", None, None, None, None, None, None);
        let WireFrame::Text(frame) = &frame else {
            panic!("resync 帧应为文本帧");
        };
        let value: serde_json::Value = serde_json::from_str(frame).expect("resync 帧须为合法 JSON");
        assert_eq!(value["type"], "resync");
        assert_eq!(value.as_object().map(|o| o.len()), Some(1));
    }

    /// resync 下发节流：首次放行，间隔内拒绝，超过间隔再放行（防慢消费者自激）。
    #[test]
    fn resync_due_is_throttled() {
        let now = Instant::now();
        assert!(resync_due(None, now), "首次应放行");
        assert!(!resync_due(Some(now), now), "同一时刻应被节流");
        assert!(
            !resync_due(Some(now), now + RESYNC_MIN_INTERVAL - Duration::from_millis(1)),
            "未到间隔应被节流"
        );
        assert!(resync_due(Some(now), now + RESYNC_MIN_INTERVAL), "到达最小间隔应放行");
    }

    /// 可选字段的 skip_serializing_if 语义：给了就序列化，没给就不出现（笔记同步帧仍带 file/payload）。
    #[test]
    fn optional_fields_skip_when_absent() {
        let note = server_msg(
            "note-sync",
            Some(7),
            None,
            None,
            Some("notes/a.md".to_string()),
            None,
            Some(serde_json::Value::String("AAA=".to_string())),
        );
        let WireFrame::Text(note) = &note else {
            panic!("note 帧应为文本帧");
        };
        let value: serde_json::Value = serde_json::from_str(note).expect("note 帧须为合法 JSON");
        assert_eq!(value["type"], "note-sync");
        assert_eq!(value["peerId"], 7);
        assert_eq!(value["file"], "notes/a.md");
        assert_eq!(value["payload"], "AAA=");
        assert!(value.get("peers").is_none());
        assert!(value.get("presence").is_none());
        assert!(value.get("patch").is_none());
        assert!(value.get("channel").is_none());

        let ack = server_msg("hello-ack", Some(3), None, None, None, None, None);
        let WireFrame::Text(ack) = &ack else {
            panic!("hello-ack 帧应为文本帧");
        };
        let value: serde_json::Value = serde_json::from_str(ack).expect("hello-ack 须为合法 JSON");
        assert_eq!(value["type"], "hello-ack");
        assert_eq!(value["peerId"], 3);
        assert!(value.get("file").is_none());
        assert!(value.get("payload").is_none());
    }

    /// 插件消息转发帧：channel + 任意 JSON payload 平铺携带；广播帧带房间级 seq，
    /// 单播帧不带 seq；转发帧不携带定向目标（接收方无需知道是否定向），
    /// 也不带 peers/presence/file 等无关字段。
    #[test]
    fn plugin_msg_frame_carries_channel_and_payload() {
        let frame = server_plugin_msg(7, "comfyui.remote".to_string(), serde_json::json!({ "cmd": "start" }), Some(42));
        let WireFrame::Text(text) = &frame else {
            panic!("插件 JSON 帧应为文本帧");
        };
        let value: serde_json::Value = serde_json::from_str(text).expect("plugin-msg 帧须为合法 JSON");
        assert_eq!(value["type"], "plugin-msg");
        assert_eq!(value["peerId"], 7);
        assert_eq!(value["channel"], "comfyui.remote");
        assert_eq!(value["payload"], serde_json::json!({ "cmd": "start" }));
        assert_eq!(value["seq"], 42);
        assert!(value.get("targetPeerId").is_none());
        assert!(value.get("peers").is_none());
        assert!(value.get("presence").is_none());
        assert!(value.get("file").is_none());

        let unicast = server_plugin_msg(7, "comfyui.remote".to_string(), serde_json::json!(1), None);
        let WireFrame::Text(text) = &unicast else {
            panic!("插件 JSON 帧应为文本帧");
        };
        let value: serde_json::Value = serde_json::from_str(text).unwrap();
        assert!(value.get("seq").is_none(), "单播帧不携带房间级 seq");
    }

    /// 二进制插件帧编解码往返：channel/payload/seq/sender/target 原样还原；载荷不经任何转码。
    #[test]
    fn plugin_binary_frame_roundtrip() {
        let payload: Vec<u8> = (0u8..=255).cycle().take(1024).collect();
        let encoded = encode_client_plugin_binary("com.x.whiteboard:wb", &payload, Some(9));
        let parsed = parse_plugin_binary(&encoded).expect("合法帧应可解析");
        assert_eq!(parsed.channel, "com.x.whiteboard:wb");
        assert_eq!(parsed.payload, payload);
        assert_eq!(parsed.target_peer_id, Some(9));
        assert_eq!(parsed.seq, 0, "客户端发送帧 seq 置 0（服务端分配）");
        assert_eq!(parsed.sender_peer_id, 0);

        // 下行帧：seq/sender 回填，不带定向目标
        let down = BinaryPluginFrame {
            channel: parsed.channel.clone(),
            payload: parsed.payload.clone(),
            seq: 5,
            target_peer_id: None,
            sender_peer_id: 12,
        };
        let encoded = build_plugin_binary(&down);
        let parsed = parse_plugin_binary(&encoded).expect("下行帧应可解析");
        assert_eq!(parsed.channel, "com.x.whiteboard:wb");
        assert_eq!(parsed.payload, payload);
        assert_eq!(parsed.seq, 5);
        assert_eq!(parsed.sender_peer_id, 12);
        assert_eq!(parsed.target_peer_id, None);
    }

    /// 畸形二进制帧恒拒（不 panic）：截断/未知 kind/坏 channel 编码返回 None。
    #[test]
    fn plugin_binary_frame_rejects_malformed() {
        let full = encode_client_plugin_binary("ch", &[1, 2, 3], None);
        assert!(parse_plugin_binary(&full).is_some());
        for cut in [0, 1, 10, PLUGIN_BINARY_HEADER - 1, PLUGIN_BINARY_HEADER + 1] {
            assert!(
                parse_plugin_binary(&full[..cut.min(full.len())]).is_none(),
                "截断到 {cut} 字节应拒绝"
            );
        }
        let mut bad_kind = full.clone();
        bad_kind[0] = 9;
        assert!(parse_plugin_binary(&bad_kind).is_none(), "未知 kind 应拒绝");
        let mut bad_utf8 = full.clone();
        bad_utf8[4] = 0xff;
        assert!(parse_plugin_binary(&bad_utf8).is_none(), "坏 channel 编码应拒绝");
    }

    /// 插件帧缓存逐出：条数超限逐出最旧，seq 升序保持。
    #[test]
    fn plugin_cache_evicts_oldest() {
        let mut room = Room::default();
        for seq in 1..=(PLUGIN_CACHE_MAX_ENTRIES as u64 + 10) {
            room.cache_plugin_frame(seq, server_msg("plugin-msg", None, None, None, None, None, None));
        }
        assert_eq!(room.plugin_cache.len(), PLUGIN_CACHE_MAX_ENTRIES);
        let seqs: Vec<u64> = room.plugin_cache.iter().map(|c| c.seq).collect();
        let mut sorted = seqs.clone();
        sorted.sort_unstable();
        assert_eq!(seqs, sorted, "缓存保持 seq 升序");
        assert_eq!(seqs[0], 11, "最旧的 1..10 已逐出");
    }

    /// meta 变更广播帧：只带键名（值由客户端回读磁盘真源），不带 peerId 与其他字段。
    #[test]
    fn meta_changed_frame_carries_key_only() {
        let json = serde_json::to_string(&ServerMsg {
            kind: "meta-changed",
            peer_id: None,
            peers: None,
            presence: None,
            file: None,
            patch: None,
            channel: None,
            payload: None,
            seq: None,
            plugin_seq: None,
            key: Some("calendar".to_string()),
        })
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).expect("meta-changed 帧须为合法 JSON");
        assert_eq!(value["type"], "meta-changed");
        assert_eq!(value["key"], "calendar");
        assert_eq!(value.as_object().map(|o| o.len()), Some(2));
    }

    /// 广播按房间投递给房间内全部成员；无关房间与空房间不投递、不报错。
    #[test]
    fn broadcast_meta_changed_reaches_room_members() {
        let hub = Hub::default();
        let make_peer = |tx: broadcast::Sender<WireFrame>, session: &str| PeerEntry {
            nickname: session.to_string(),
            color: String::new(),
            device_name: String::new(),
            version: None,
            presence: None,
            tx,
            session_id: session.to_string(),
            kick_tx: watch::channel(None).0,
        };
        let (tx_a, mut rx_a) = broadcast::channel::<WireFrame>(16);
        let (tx_b, mut rx_b) = broadcast::channel::<WireFrame>(16);
        hub.0.lock().unwrap().insert(
            "space:s1".to_string(),
            Room {
                peers: HashMap::from([(1u64, make_peer(tx_a, "s-a")), (2u64, make_peer(tx_b, "s-b"))]),
                ..Room::default()
            },
        );
        hub.0
            .lock()
            .unwrap()
            .insert("space:s2".to_string(), Room::default());

        broadcast_meta_changed(&hub, "space:s1", "calendar");

        let frame_a = rx_a.try_recv().expect("房间内成员 a 应收到广播帧");
        let frame_b = rx_b.try_recv().expect("房间内成员 b 应收到广播帧");
        let WireFrame::Text(text_a) = &frame_a else {
            panic!("meta 帧应为文本帧");
        };
        let value: serde_json::Value = serde_json::from_str(text_a).unwrap();
        assert_eq!(value["type"], "meta-changed");
        assert_eq!(value["key"], "calendar");
        match (&frame_a, &frame_b) {
            (WireFrame::Text(a), WireFrame::Text(b)) => assert_eq!(a, b, "同一房间收到的是同一帧"),
            _ => panic!("meta 帧应为文本帧"),
        }
        assert!(rx_a.try_recv().is_err(), "不应有额外帧");
    }
}
