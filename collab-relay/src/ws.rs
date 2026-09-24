//! WS 共享房间机制（presence / 补丁 / 笔记同步 / 插件消息转发），
//! 空间频道 `/ws/space` 的底层：鉴权由 space_ws 完成后，入房与转发全走本模块。
//!
//! 协议（JSON over WS，字段 camelCase）：
//! - C→S `hello`：`{ type, spaceId, token, nickname, color, deviceName, version? }`（首条必发）
//! - C→S `presence`：`{ type, file?, selection?, view?, openFiles?, lockedNodes?, streamingNodeIds?, editingNotes? }`
//! - C→S `table-patch` / `canvas-patch`：`{ type, file, patch }`（不透明透传）
//! - C→S `note-sync` / `note-aware`：`{ type, file, payload }`（base64 载荷不透明透传）
//! - C→S `plugin-msg`：`{ type, channel, payload, targetPeerId? }`（广播/定向单播）
//! - C→S `ping`（保活，回 `pong` 广播）/ `bye`（离开）
//! - S→C `hello-ack`：`{ type, peerId }`（先于 peers 帧——客户端据此把自己过滤出列表）
//! - S→C `peers` / `presence` / 各转发帧（不含自己）/ `resync`（慢消费者重新握手）/ `error`
//!
//! 日志红线：转发内容（patch / Yjs payload / selection）只记字节数不记内容。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
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

/// 房间 = 同一房间 id 的在线连接；每连接一个 broadcast 通道（转发出站消息）。
type Room = HashMap<u64, PeerEntry>;

struct PeerEntry {
    nickname: String,
    color: String,
    device_name: String,
    version: Option<String>,
    presence: Option<Presence>,
    tx: broadcast::Sender<Arc<String>>,
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
}

fn server_msg(
    kind: &'static str,
    peer_id: Option<u64>,
    peers: Option<Vec<PeerInfo>>,
    presence: Option<Presence>,
    file: Option<String>,
    patch: Option<serde_json::Value>,
    payload: Option<serde_json::Value>,
) -> Arc<String> {
    let json = serde_json::to_string(&ServerMsg {
        kind,
        peer_id,
        peers,
        presence,
        file,
        patch,
        channel: None,
        payload,
    })
    .unwrap();
    Arc::new(json)
}

/// 构造插件消息转发帧（转发帧不携带定向目标——接收方无需知道是否定向）。
fn server_plugin_msg(peer_id: u64, channel: String, payload: serde_json::Value) -> Arc<String> {
    let json = serde_json::to_string(&ServerMsg {
        kind: "plugin-msg",
        peer_id: Some(peer_id),
        peers: None,
        presence: None,
        file: None,
        patch: None,
        channel: Some(channel),
        payload: Some(payload),
    })
    .unwrap();
    Arc::new(json)
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
    for p in room.values() {
        let _ = p.tx.send(payload.clone());
    }
}

/// 把已构建的转发帧送房间内除发送者外的全部成员（presence/补丁/笔记同步共用转发循环）。
fn forward_to_room(rooms: &mut HashMap<String, Room>, room_id: &str, sender_id: u64, payload: Arc<String>) {
    if let Some(room) = rooms.get_mut(room_id) {
        for (id, peer) in room.iter() {
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
        for peer in room.values() {
            if peer.tx.send(payload.clone()).is_err() {
                debug!(room = %room_id, kind = %kind, "补丁帧投递失败（接收端已关闭）");
            }
        }
    }
}

/// 已验证的连接进入房间：hello-ack → 入房广播 → 消息循环 → 离场收尾。
/// 鉴权由调用方在进入本函数之前完成。
pub(crate) async fn run_room_connection(
    socket: WebSocket,
    hub: Hub,
    remote: SocketAddr,
    room_id: String,
    meta: PeerMeta,
) {
    let (mut sink, mut stream) = socket.split();
    let started_at = Instant::now();

    let peer_id = NEXT_PEER_ID.fetch_add(1, Ordering::Relaxed);
    let mut stats = ConnStats::default();
    let (btx, _) = broadcast::channel::<Arc<String>>(256);
    // 后台转发必须先就位（订阅 receiver），再入房广播——否则本连接的首次 peers 帧
    // （含自己）在 send_task 启动前被 send 丢弃（broadcast 无 receiver 时 send 直接 Err）
    let mut btx_rx = btx.subscribe();
    let send_task = tokio::spawn(async move {
        let mut last_resync: Option<Instant> = None;
        loop {
            match btx_rx.recv().await {
                Ok(payload) => {
                    if sink.send(Message::text((*payload).clone())).await.is_err() {
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
                        if sink.send(Message::text((*resync).clone())).await.is_err() {
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
    // 先告知本连接自己的 peerId（客户端据此把自己过滤出 peers），再广播全量快照——
    // 顺序颠倒会让客户端收到含自己的 peers 帧时还无法识别自己（一帧闪现）
    let _ = btx.send(server_msg("hello-ack", Some(peer_id), None, None, None, None, None));
    let nickname = meta.nickname.clone();
    let device_name = meta.device_name.clone();
    let version = meta.version.clone();
    {
        let mut rooms = hub.0.lock().unwrap();
        let room = rooms.entry(room_id.clone()).or_default();
        room.insert(
            peer_id,
            PeerEntry {
                nickname: meta.nickname,
                color: meta.color,
                device_name: meta.device_name,
                version: meta.version,
                presence: None,
                tx: btx.clone(),
            },
        );
        info!(
            peer_id,
            room = %room_id,
            nickname = %nickname,
            device_name = %device_name,
            version = version.as_deref().unwrap_or(""),
            %remote,
            room_size = room.len(),
            "协作者加入房间",
        );
        broadcast_peers(&rooms, &room_id);
    }

    // 消息循环：30s 无消息（心跳超时）断开
    loop {
        let recv = tokio::time::timeout(HEARTBEAT_TIMEOUT, stream.next()).await;
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
                            if let Some(peer) = room.get_mut(&peer_id) {
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
                    // 插件通用消息：channel + payload 任意 JSON 不透明透传（不解析内容），
                    // 缺省广播房间内其他成员；带 targetPeerId 时单播只发该 peer（不在线或指向自己
                    // 即丢弃，与广播「不含自己」语义一致）。日志只记频道/字节数/目标，不记载荷内容。
                    "plugin-msg" => {
                        if let (Some(channel), Some(payload)) = (msg.channel, msg.payload) {
                            let frame = server_plugin_msg(peer_id, channel.clone(), payload);
                            debug!(
                                peer_id,
                                room = %room_id,
                                channel = %channel,
                                target_peer = ?msg.target_peer_id,
                                bytes = frame.len(),
                                "插件消息转发",
                            );
                            stats.forwarded_msgs += 1;
                            stats.forwarded_bytes += frame.len() as u64;
                            let mut rooms = hub.0.lock().unwrap();
                            if let Some(target) = msg.target_peer_id {
                                if target != peer_id {
                                    if let Some(room) = rooms.get(&room_id) {
                                        if let Some(peer) = room.get(&target) {
                                            let _ = peer.tx.send(frame);
                                        }
                                    }
                                }
                            } else {
                                forward_to_room(&mut rooms, &room_id, peer_id, frame);
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
            Ok(Some(Ok(_))) => {
                debug!(peer_id, "忽略二进制/关闭帧");
                continue;
            }
        }
    }

    // 离开：移出房间 + 广播更新后的 peers（房间空则整体移除）
    {
        let mut rooms = hub.0.lock().unwrap();
        if let Some(room) = rooms.get_mut(&room_id) {
            room.remove(&peer_id);
            if room.is_empty() {
                rooms.remove(&room_id);
            } else {
                broadcast_peers(&rooms, &room_id);
            }
        }
    }
    info!(
        peer_id,
        room = %room_id,
        duration_ms = started_at.elapsed().as_millis() as u64,
        received_msgs = stats.received_msgs,
        received_bytes = stats.received_bytes,
        forwarded_msgs = stats.forwarded_msgs,
        forwarded_bytes = stats.forwarded_bytes,
        "协作者连接结束",
    );
    send_task.abort();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// resync 是纯类型帧：客户端只据 `type` 重新握手，帧内不带任何多余字段。
    #[test]
    fn resync_frame_carries_type_only() {
        let frame = server_msg("resync", None, None, None, None, None, None);
        let value: serde_json::Value = serde_json::from_str(&frame).expect("resync 帧须为合法 JSON");
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
        let value: serde_json::Value = serde_json::from_str(&note).expect("note 帧须为合法 JSON");
        assert_eq!(value["type"], "note-sync");
        assert_eq!(value["peerId"], 7);
        assert_eq!(value["file"], "notes/a.md");
        assert_eq!(value["payload"], "AAA=");
        assert!(value.get("peers").is_none());
        assert!(value.get("presence").is_none());
        assert!(value.get("patch").is_none());
        assert!(value.get("channel").is_none());

        let ack = server_msg("hello-ack", Some(3), None, None, None, None, None);
        let value: serde_json::Value = serde_json::from_str(&ack).expect("hello-ack 须为合法 JSON");
        assert_eq!(value["type"], "hello-ack");
        assert_eq!(value["peerId"], 3);
        assert!(value.get("file").is_none());
        assert!(value.get("payload").is_none());
    }

    /// 插件消息转发帧：channel + 任意 JSON payload 平铺携带；转发帧不携带定向目标
    /// （接收方无需知道是否定向），也不带 peers/presence/file 等无关字段。
    #[test]
    fn plugin_msg_frame_carries_channel_and_payload() {
        let frame = server_plugin_msg(7, "comfyui.remote".to_string(), serde_json::json!({ "cmd": "start" }));
        let value: serde_json::Value = serde_json::from_str(&frame).expect("plugin-msg 帧须为合法 JSON");
        assert_eq!(value["type"], "plugin-msg");
        assert_eq!(value["peerId"], 7);
        assert_eq!(value["channel"], "comfyui.remote");
        assert_eq!(value["payload"], serde_json::json!({ "cmd": "start" }));
        assert!(value.get("targetPeerId").is_none());
        assert!(value.get("peers").is_none());
        assert!(value.get("presence").is_none());
        assert!(value.get("file").is_none());
    }
}
