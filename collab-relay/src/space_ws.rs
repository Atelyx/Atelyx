//! 空间实时频道（`/ws/space`）：凭令牌订阅空间房间，底层复用 ws.rs 的共享房间机制
//! （入房 / presence / 补丁 / 笔记同步 / 插件消息转发 / 心跳 / resync）。
//! 本模块只负责入口鉴权——hello 必须携带有效令牌且调用方是该空间成员
//! （viewer 可入房只读接收，写权限由各写端点闸门负责），否则 error 帧后断开。

use std::net::SocketAddr;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, State};
use axum::response::IntoResponse;
use serde::Deserialize;
use tracing::warn;

use crate::state::{token_hash, ServerState};
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

    // 鉴权：令牌 → 会话 → 用户；成员资格 → 入房（viewer 亦可，只读接收）。失败 = error 帧后断开。
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
        let name = p
            .users
            .iter()
            .find(|u| u.id == identity.user_id)
            .map(|u| u.display_name.clone());
        (member.is_some(), name)
    });
    let (is_member, display_name) = membership;
    if !is_member {
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
            session_id: identity.session_id,
        },
        // 入房后回查：会话在鉴权与入房之间可能已被吊销（如管理台刚执行重置密码/吊销）
        |session_id: &str| state.read(|p| p.sessions.iter().any(|s| s.id == session_id)),
    )
    .await;
}
