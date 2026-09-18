//! 无鉴权中转端点（`/ws`）：报仓库 id 入房，转发 presence / 补丁 / 笔记同步 / 插件消息。
//! 信任边界 = 局域网（不校验身份）；带鉴权的空间能力走 `/api/*` 与 `/ws/space`。

use std::net::SocketAddr;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, State};
use axum::response::IntoResponse;
use serde::Deserialize;
use tracing::warn;

use crate::state::ServerState;
use crate::ws::{run_room_connection, Hub, PeerMeta, HEARTBEAT_TIMEOUT};

pub async fn upgrade(
    ws: WebSocketUpgrade,
    State(state): State<ServerState>,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle(socket, state.hub(), remote))
}

async fn handle(mut socket: WebSocket, hub: Hub, remote: SocketAddr) {
    // 首条消息必须是 hello（带超时保护，防半开连接占位）
    let hello = match tokio::time::timeout(HEARTBEAT_TIMEOUT, socket.recv()).await {
        Ok(Some(Ok(Message::Text(text)))) => match serde_json::from_str::<LegacyHello>(&text) {
            Ok(m) if m.kind == "hello" => m,
            _ => {
                warn!(%remote, "首条消息非 hello，拒绝连接");
                let _ = socket.send(Message::text(crate::ws::peer_error("首条消息须为 hello"))).await;
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
            tracing::debug!(%remote, "hello 前连接关闭");
            return;
        }
        Err(_) => {
            warn!(%remote, "hello 超时（30s 内无首条消息），断开");
            return;
        }
    };
    run_room_connection(
        socket,
        hub,
        remote,
        hello.vault_id,
        PeerMeta {
            nickname: hello.nickname,
            color: hello.color,
            device_name: hello.device_name,
            version: hello.version,
        },
    )
    .await;
}

/// 旧协议 hello：仓库 id 即房间 id（无鉴权）。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyHello {
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
    #[serde(default)]
    version: Option<String>,
}
