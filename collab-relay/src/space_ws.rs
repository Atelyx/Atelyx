//! 空间实时频道（`/ws/space`）：凭令牌订阅空间房间。转发语义与 `/ws` 一致；
//! 差别在入口鉴权——hello 必须携带有效令牌且调用方是该空间成员，否则 error 帧后断开。

use std::net::SocketAddr;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, State};
use axum::response::IntoResponse;
use serde::Deserialize;
use tracing::warn;

use crate::state::{token_hash, ServerState, ROLE_EDITOR, ROLE_OWNER};
use crate::ws::{run_room_connection, PeerMeta, HEARTBEAT_TIMEOUT};

pub async fn upgrade(
    ws: WebSocketUpgrade,
    State(state): State<ServerState>,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle(socket, state, remote))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SpaceHello {
    #[serde(rename = "type")]
    kind: String,
    space_id: String,
    token: String,
    #[serde(default)]
    nickname: String,
    #[serde(default)]
    color: String,
    #[serde(default)]
    device_name: String,
    #[serde(default)]
    version: Option<String>,
}

async fn handle(mut socket: WebSocket, state: ServerState, remote: SocketAddr) {
    // 首条消息必须是 hello（带超时保护，防半开连接占位）
    let hello = match tokio::time::timeout(HEARTBEAT_TIMEOUT, socket.recv()).await {
        Ok(Some(Ok(Message::Text(text)))) => match serde_json::from_str::<SpaceHello>(&text) {
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

    // 鉴权：令牌 → 会话 → 用户；成员资格 → 入房。失败 = error 帧后断开。
    let identity = state.authenticate(&token_hash(&hello.token));
    let Some(identity) = identity else {
        warn!(%remote, "空间频道：无效令牌，拒绝连接");
        let _ = socket.send(Message::text(crate::ws::peer_error("无效令牌"))).await;
        return;
    };
    let membership = state.read(|p| {
        let member = p
            .spaces
            .iter()
            .find(|s| s.id == hello.space_id)
            .and_then(|s| s.members.iter().find(|m| m.user_id == identity.user_id));
        let role_ok = member
            .map(|m| m.role == ROLE_OWNER || m.role == ROLE_EDITOR)
            .unwrap_or(false);
        let name = p
            .users
            .iter()
            .find(|u| u.id == identity.user_id)
            .map(|u| u.display_name.clone());
        (member.is_some(), role_ok, name)
    });
    let (is_member, role_ok, display_name) = membership;
    if !is_member || !role_ok {
        warn!(user_id = %identity.user_id, space_id = %hello.space_id, "空间频道：非成员，拒绝连接");
        let _ = socket.send(Message::text(crate::ws::peer_error("不是该空间成员"))).await;
        return;
    }
    let nickname = if hello.nickname.trim().is_empty() {
        display_name.unwrap_or_default()
    } else {
        hello.nickname
    };
    let room_id = format!("space:{}", hello.space_id);
    run_room_connection(
        socket,
        state.hub(),
        remote,
        room_id,
        PeerMeta {
            nickname,
            color: hello.color,
            device_name: hello.device_name,
            version: hello.version,
        },
    )
    .await;
}
