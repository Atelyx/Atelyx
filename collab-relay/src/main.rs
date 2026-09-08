//! Atelyx 局域网协作中转（presence + 表格/画布补丁 + 笔记 CRDT relay）。
//!
//! 无状态 WebSocket hub：客户端按仓库 id（vaultId）分房间，转发 presence（在线用户 + 打开文件 +
//! 选中状态）与内容补丁（`table-patch` 表格 / `canvas-patch` 画布，实时协作内容通道）。
//! 纯转发不持久化——断线即消失，30s 无消息心跳超时踢出。无鉴权（局域网信任）：同一局域网内
//! 任何客户端可加入房间；单端口，部署 = 服务器 `git clone && docker compose up -d`。
//!
//! 日志：tracing 结构化输出（stderr / `docker logs`）。级别由 `RUST_LOG` 控制（默认 info；
//! `RUST_LOG=debug` 看逐消息转发明细），`LOG_FORMAT=json` 切 JSON 行输出便于采集。
//! 隐私红线：转发内容（patch / Yjs payload / selection）一律只记字节数不记内容；
//! 身份元数据（昵称/设备名/仓库 id/文件相对路径/应用版本号）在局域网信任语境下可入日志。
//!
//! 协议（JSON over WS，字段 camelCase）：
//! - C→S `hello`：`{ type, vaultId, nickname, color, deviceName, version? }`（首条必发；
//!   version = 本端应用版本号，协作房间展示各成员版本，旧客户端可缺省）
//! - C→S `presence`：`{ type, file?, selection?, view?, openFiles?, lockedNodes?, streamingNodeIds? }`
//!   （选中变化节流后发；openFiles/lockedNodes/streamingNodeIds 不透明透传，供协作房间/画布锁/生成灯）
//! - C→S `table-patch`：`{ type, file, patch }`（表格增量补丁广播；patch 不透明透传，
//!   客户端按 file 匹配只应用当前打开的表格）
//! - C→S `canvas-patch`：`{ type, file, patch }`（画布增量补丁广播；patch 不透明透传，
//!   客户端按 file 匹配只应用当前打开的画布）
//! - C→S `ping`（保活）/ `bye`（离开）
//! - S→C `hello-ack`：`{ type, peerId }`（分配的本连接 id，先于 peers 帧——客户端据此把自己过滤出列表）
//! - S→C `peers`：`{ type, peers: [{ peerId, nickname, color, deviceName, version?, presence? }] }`
//!   （房间成员变化时全量推送；version = 该成员应用版本号；presence 字段 = `{ file?, selection?, view?, openFiles?, lockedNodes?, streamingNodeIds? }`）
//! - S→C `presence`：`{ type, peerId, presence }`（他人 presence 转发，不含自己）
//! - S→C `table-patch`：`{ type, peerId, file, patch }`（他人补丁转发，不含自己）
//! - S→C `canvas-patch`：`{ type, peerId, file, patch }`（他人补丁转发，不含自己）
//! - S→C `error`：`{ type, message }`

use std::collections::HashMap;
use std::io::IsTerminal;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, State};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tracing::{debug, info, trace, warn};
use tracing_subscriber::EnvFilter;

/// 心跳超时：期间无任何消息（含 ping）即断开。
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(30);

static NEXT_PEER_ID: AtomicU64 = AtomicU64::new(1);

type Rooms = HashMap<String, Room>;
/// 房间 = 同一 vaultId 的在线连接；每连接一个 broadcast 通道（转发出站消息）。
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

/// 全局房间表（vaultId → 房间）；hub 为 axum State。
#[derive(Clone)]
struct Hub(Arc<Mutex<Rooms>>);

impl Default for Hub {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(HashMap::new())))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClientMsg {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    vault_id: String,
    #[serde(default)]
    nickname: String,
    #[serde(default)]
    color: String,
    #[serde(default)]
    device_name: String,
    /// 本端应用版本号（hello 携带；协作房间展示各成员版本，旧客户端可缺省）。
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    selection: Option<serde_json::Value>,
    #[serde(default)]
    view: Option<String>,
    /// 打开文件清单（协作房间面板；不透明透传，relay 不解析）。
    #[serde(default)]
    open_files: Option<serde_json::Value>,
    /// 画布对话独占锁声明（presence 跨视图保活；不透明透传）。
    #[serde(default)]
    locked_nodes: Option<serde_json::Value>,
    /// 画布正在 AI 生成的对话节点（生成灯；不透明透传）。
    #[serde(default)]
    streaming_node_ids: Option<serde_json::Value>,
    /// 表格增量补丁（`table-patch` 消息；不透明透传，relay 不解析内容）。
    #[serde(default)]
    patch: Option<serde_json::Value>,
    /// 笔记协作同步/awareness（`note-sync`/`note-aware` 消息；Yjs 二进制经 base64 包装，不透明透传）。
    #[serde(default)]
    payload: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    payload: Option<String>,
}

fn server_msg(
    kind: &'static str,
    peer_id: Option<u64>,
    peers: Option<Vec<PeerInfo>>,
    presence: Option<Presence>,
    file: Option<String>,
    patch: Option<serde_json::Value>,
    payload: Option<String>,
) -> Arc<String> {
    let json = serde_json::to_string(&ServerMsg {
        kind,
        peer_id,
        peers,
        presence,
        file,
        patch,
        payload,
    })
    .unwrap();
    Arc::new(json)
}

fn peer_error(text: &str) -> String {
    serde_json::json!({ "type": "error", "message": text }).to_string()
}

/// 向房间全员广播 peers 全量快照（成员加入/离开时调用）。
fn broadcast_peers(rooms: &Rooms, vault_id: &str) {
    let Some(room) = rooms.get(vault_id) else {
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
fn forward_to_room(rooms: &mut Rooms, vault_id: &str, sender_id: u64, payload: Arc<String>) {
    if let Some(room) = rooms.get_mut(vault_id) {
        for (id, peer) in room.iter() {
            if *id != sender_id {
                let _ = peer.tx.send(payload.clone());
            }
        }
    }
}

/// 日志初始化：RUST_LOG 控制级别（默认 info），非 TTY（Docker）自动去 ANSI 颜色，
/// `LOG_FORMAT=json` 切 JSON 行输出便于采集。
fn init_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    if std::env::var("LOG_FORMAT").as_deref() == Ok("json") {
        tracing_subscriber::fmt().json().with_env_filter(filter).init();
    } else {
        tracing_subscriber::fmt()
            .with_ansi(std::io::stderr().is_terminal())
            .with_env_filter(filter)
            .init();
    }
}

#[tokio::main]
async fn main() {
    init_logging();
    let port = std::env::var("PORT").unwrap_or_else(|_| "17701".to_string());
    let addr = format!("0.0.0.0:{port}");
    let app = Router::new().route("/ws", get(ws_handler)).with_state(Hub::default());
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| panic!("bind {addr} 失败：{e}"));
    info!(addr = %addr, "collab-relay 已启动，监听 /ws");
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .await
        .expect("服务器错误");
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(hub): State<Hub>,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
) -> impl IntoResponse {
    debug!(%remote, "ws 连接建立");
    ws.on_upgrade(move |socket| handle_socket(socket, hub, remote))
}

async fn handle_socket(socket: WebSocket, hub: Hub, remote: SocketAddr) {
    let (mut sink, mut stream) = socket.split();
    let started_at = std::time::Instant::now();

    // 首条消息必须是 hello（带超时保护，防半开连接占位）
    let hello = match tokio::time::timeout(HEARTBEAT_TIMEOUT, stream.next()).await {
        Ok(Some(Ok(Message::Text(text)))) => match serde_json::from_str::<ClientMsg>(&text) {
            Ok(m) if m.kind == "hello" => m,
            _ => {
                warn!(%remote, "首条消息非 hello，拒绝连接");
                let _ = sink.send(Message::Text(peer_error("首条消息须为 hello").into())).await;
                return;
            }
        },
        Ok(Some(Ok(_))) => {
            warn!(%remote, "首条消息非文本帧，拒绝连接");
            return;
        }
        Ok(Some(Err(_))) => {
            warn!(%remote, "首条消息协议错误，断开");
            return;
        }
        Ok(None) => {
            debug!(%remote, "hello 前连接关闭");
            return;
        }
        Err(_) => {
            warn!(%remote, "hello 超时（30s 内无首条消息），断开");
            return;
        }
    };

    let peer_id = NEXT_PEER_ID.fetch_add(1, Ordering::Relaxed);
    let mut stats = ConnStats::default();
    let (btx, _) = broadcast::channel::<Arc<String>>(256);
    // 后台转发必须先就位（订阅 receiver），再入房广播——否则本连接的首次 peers 帧
    // （含自己）在 send_task 启动前被 send 丢弃（broadcast 无 receiver 时 send 直接 Err）
    let mut btx_rx = btx.subscribe();
    let send_task = tokio::spawn(async move {
        loop {
            match btx_rx.recv().await {
                Ok(payload) => {
                    if sink.send(Message::Text((*payload).clone().into())).await.is_err() {
                        debug!(peer_id, "发送失败，客户端断开");
                        break;
                    }
                }
                // 消费过慢被广播层裁剪（lagged）：跳过该帧继续，防发送任务误退出
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    debug!(peer_id, "发送队列过慢，跳过被裁剪帧");
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    // 先告知本连接自己的 peerId（客户端据此把自己过滤出 peers），再广播全量快照——
    // 顺序颠倒会让客户端收到含自己的 peers 帧时还无法识别自己（一帧闪现）
    let _ = btx.send(server_msg("hello-ack", Some(peer_id), None, None, None, None, None));
    let vault_id = hello.vault_id.clone();
    let nickname = hello.nickname.clone();
    let device_name = hello.device_name.clone();
    let version = hello.version.clone();
    {
        let mut rooms = hub.0.lock().unwrap();
        let room = rooms.entry(vault_id.clone()).or_default();
        room.insert(
            peer_id,
            PeerEntry {
                nickname: hello.nickname,
                color: hello.color,
                device_name: hello.device_name,
                version: hello.version,
                presence: None,
                tx: btx.clone(),
            },
        );
        info!(
            peer_id,
            vault_id = %vault_id,
            nickname = %nickname,
            device_name = %device_name,
            version = version.as_deref().unwrap_or(""),
            %remote,
            room_size = room.len(),
            "协作者加入房间",
        );
        broadcast_peers(&rooms, &vault_id);
    }

    // 消息循环：30s 无消息（心跳超时）断开
    loop {
        let recv = tokio::time::timeout(HEARTBEAT_TIMEOUT, stream.next()).await;
        match recv {
            Err(_) => {
                warn!(peer_id, vault_id = %vault_id, "心跳超时（30s 无消息），断开");
                break;
            }
            Ok(None) => {
                debug!(peer_id, vault_id = %vault_id, "连接关闭（对端断开）");
                break;
            }
            Ok(Some(Err(_))) => {
                warn!(peer_id, vault_id = %vault_id, "WebSocket 协议错误，断开");
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
                        };
                        // presence 高频（选中节流后仍密集）：debug 只记文件/视图与清单数量，
                        // 不记 selection 内容（内容可能含用户文本/图片选区）
                        debug!(
                            peer_id,
                            vault_id = %vault_id,
                            file = presence.file.as_deref().unwrap_or(""),
                            view = presence.view.as_deref().unwrap_or(""),
                            open_files = presence
                                .open_files
                                .as_ref()
                                .and_then(|v| v.as_array())
                                .map_or(0, |a| a.len()),
                            locked_nodes = presence
                                .locked_nodes
                                .as_ref()
                                .and_then(|v| v.as_array())
                                .map_or(0, |a| a.len()),
                            streaming_nodes = presence
                                .streaming_node_ids
                                .as_ref()
                                .and_then(|v| v.as_array())
                                .map_or(0, |a| a.len()),
                            "presence 转发",
                        );
                        let mut rooms = hub.0.lock().unwrap();
                        if let Some(room) = rooms.get_mut(&vault_id) {
                            if let Some(peer) = room.get_mut(&peer_id) {
                                peer.presence = Some(presence.clone());
                            }
                            let payload =
                                server_msg("presence", Some(peer_id), None, Some(presence), None, None, None);
                            stats.forwarded_msgs += 1;
                            stats.forwarded_bytes += payload.len() as u64;
                            forward_to_room(&mut rooms, &vault_id, peer_id, payload);
                        }
                    }
                    // 表格/画布内容补丁：同构透传（不存储、不解析内容，原样转发房间内其他成员，
                    // 客户端按 file 匹配只应用当前打开的文件）
                    "table-patch" | "canvas-patch" => {
                        if let (Some(file), Some(patch)) = (msg.file, msg.patch) {
                            let kind: &'static str = if msg.kind.as_str() == "table-patch" {
                                "table-patch"
                            } else {
                                "canvas-patch"
                            };
                            debug!(peer_id, vault_id = %vault_id, kind, file = %file, "内容补丁转发");
                            let mut rooms = hub.0.lock().unwrap();
                            let payload = server_msg(
                                kind,
                                Some(peer_id),
                                None,
                                None,
                                Some(file),
                                Some(patch),
                                None,
                            );
                            stats.forwarded_msgs += 1;
                            stats.forwarded_bytes += payload.len() as u64;
                            forward_to_room(&mut rooms, &vault_id, peer_id, payload);
                        }
                    }
                    // 笔记协作同步 / awareness：不透明透传（base64 载荷），原样转发房间内其他成员
                    // （客户端按 file 匹配只合入当前打开的笔记）；保持 relay 无状态纯转发
                    "note-sync" | "note-aware" => {
                        if let (Some(file), Some(payload)) = (msg.file, msg.payload) {
                            let kind: &'static str =
                                if msg.kind.as_str() == "note-sync" { "note-sync" } else { "note-aware" };
                            debug!(
                                peer_id,
                                vault_id = %vault_id,
                                kind,
                                file = %file,
                                bytes = payload.len(),
                                "笔记同步转发",
                            );
                            let mut rooms = hub.0.lock().unwrap();
                            let relayed = server_msg(
                                kind,
                                Some(peer_id),
                                None,
                                None,
                                Some(file),
                                None,
                                Some(payload),
                            );
                            stats.forwarded_msgs += 1;
                            stats.forwarded_bytes += relayed.len() as u64;
                            forward_to_room(&mut rooms, &vault_id, peer_id, relayed);
                        }
                    }
                    "bye" => {
                        info!(peer_id, vault_id = %vault_id, "协作者离开（bye）");
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
        if let Some(room) = rooms.get_mut(&vault_id) {
            room.remove(&peer_id);
            if room.is_empty() {
                rooms.remove(&vault_id);
            } else {
                broadcast_peers(&rooms, &vault_id);
            }
        }
    }
    info!(
        peer_id,
        vault_id = %vault_id,
        duration_ms = started_at.elapsed().as_millis() as u64,
        received_msgs = stats.received_msgs,
        received_bytes = stats.received_bytes,
        forwarded_msgs = stats.forwarded_msgs,
        forwarded_bytes = stats.forwarded_bytes,
        "协作者连接结束",
    );
    send_task.abort();
}
