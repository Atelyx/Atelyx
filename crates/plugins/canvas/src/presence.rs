//! 画布协作在场：本端选中 / 流式 / 锁状态上行与对端状态维护。走宿主通用 `collab` 通道，
//! 插件不连协作协议；身份随消息带出，离场帧立即摘除，过期由 TTL 兜底。

use std::collections::HashMap;
use std::time::Instant;

use atelyx_plugin_protocol::method::{CollabMessageParams, SpaceSelfIdentity};
use atelyx_plugin_sdk::Context;

use crate::PluginState;
use crate::model::{RemotePeer, Session};

/// 协作通道名（宿主按插件 id 加命名空间前缀）。
pub const CHANNEL: &str = "canvas-presence";
/// 对端状态过期（超过该时长没再收到即剔除）。
const PEER_TTL_MS: u128 = 30_000;
/// 本端状态保活间隔：空闲端不重发就会被对端按 TTL 剔除。
const KEEPALIVE_MS: u128 = 15_000;
/// 状态变化的最小上行间隔：高频输入时沿用上一次已发的状态。
const MIN_SEND_MS: u128 = 250;

/// 在场缓存：上一次上行的载荷与时刻。
#[derive(Default)]
pub struct Presence {
    sent: Option<serde_json::Value>,
    sent_at: Option<Instant>,
    /// 被最小间隔挡下 / 上行未受理的新状态：到点后由下一 tick 补发，避免停在旧状态。
    pending: Option<serde_json::Value>,
    /// 未受理载荷的下一次重试时刻：按保活节奏退避，未连接期间不逐拍空转。
    retry_at: Option<Instant>,
}

impl Presence {
    /// 载荷与上次相同则交给保活；不同则受最小上行间隔限制。
    pub fn should_send(&mut self, payload: &serde_json::Value) -> bool {
        match (&self.sent, self.sent_at) {
            (Some(sent), _) if sent == payload => {
                self.pending = None;
                self.retry_at = None;
                false
            }
            (Some(_), Some(at)) if at.elapsed().as_millis() < MIN_SEND_MS => {
                self.pending = Some(payload.clone());
                false
            }
            _ => {
                self.pending = None;
                self.retry_at = None;
                true
            }
        }
    }

    /// 到点取待发载荷：被上行间隔压下的新状态按间隔补发；未受理退回的按保活节奏重试。
    /// 不在这里消费——上行受理由 `note_sent` 清掉，失败由 `defer` 重新排期。
    pub fn take_pending(&mut self) -> Option<serde_json::Value> {
        let payload = self.pending.as_ref()?;
        if let Some(at) = self.retry_at {
            if Instant::now() < at {
                return None;
            }
            return Some(payload.clone());
        }
        let at = self.sent_at?;
        (at.elapsed().as_millis() >= MIN_SEND_MS).then(|| payload.clone())
    }

    /// 记录一次已受理的本端状态上行。待发载荷与这版相同才算送达清掉（更新的待发保留）。
    pub fn note_sent(&mut self, payload: serde_json::Value) {
        self.sent = Some(payload.clone());
        self.sent_at = Some(Instant::now());
        if self.pending.as_ref() == Some(&payload) {
            self.pending = None;
        }
        self.retry_at = None;
    }

    /// 上行未受理（未连接）：载荷退回待发并按保活节奏排期重试；已有待发载荷时保留它。
    pub fn defer(&mut self, payload: serde_json::Value) {
        if self.pending.is_none() {
            self.pending = Some(payload);
        }
        self.retry_at = Some(Instant::now()
            + std::time::Duration::from_millis(u64::try_from(KEEPALIVE_MS).unwrap_or(u64::MAX)));
    }

    /// 到期保活：返回需要重发的本端状态（未到间隔 = None）。
    /// 不在这里刷新时刻：上行由调用方受理后才记「已发」，未连接期间的保活下一拍重试。
    pub fn keepalive(&mut self) -> Option<serde_json::Value> {
        let at = self.sent_at?;
        if at.elapsed().as_millis() < KEEPALIVE_MS {
            return None;
        }
        self.sent.clone()
    }

    /// 清掉本端记录（离场 / 切仓库后重新计起）。
    pub fn reset(&mut self) {
        self.sent = None;
        self.sent_at = None;
        self.pending = None;
        self.retry_at = None;
    }
}

/// 本端状态载荷（`locks` = 本端持有的卡编辑锁与取得时刻）。
pub fn payload(session: &Session, identity: Option<&SpaceSelfIdentity>) -> serde_json::Value {
    let locks: serde_json::Map<String, serde_json::Value> = session
        .view
        .locks
        .iter()
        .map(|(id, since)| (id.clone(), serde_json::json!(since)))
        .collect();
    let name = identity
        .map(|identity| identity.name.trim())
        .filter(|name| !name.is_empty())
        .unwrap_or("用户");
    let color = identity
        .map(|identity| identity.color.trim())
        .filter(|color| !color.is_empty());
    serde_json::json!({
        "path": session.file,
        "name": name,
        "color": color,
        "selected": session.view.selection,
        "streaming": session
            .view
            .card_chat
            .iter()
            .filter(|(_, chat)| chat.streaming)
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>(),
        "locks": locks,
        "present": true,
    })
}

/// 上行本端状态：载荷相同即跳过，变化受最小间隔限制。
/// 只有宿主确认入队（`queued=true`）才记「已发」；未连接退回待发下一拍重试。
pub async fn send(plugin: &PluginState, ctx: &Context) {
    let Some(value) = plugin.with_session(|session| payload(session, plugin.identity().as_ref())) else {
        return;
    };
    if !plugin
        .with_presence_mut(|presence| presence.should_send(&value))
        .unwrap_or(false)
    {
        return;
    }
    match ctx
        .collab_send_ephemeral(CHANNEL, value.clone(), None)
        .await
    {
        Ok(result) if result.queued => {
            plugin.with_presence_mut(|presence| presence.note_sent(value));
        }
        Ok(_) => {
            plugin.with_presence_mut(|presence| presence.defer(value));
        }
        Err(error) => ctx.log_error(&format!("发送画布在场失败：{error}")),
    }
}

/// 到期保活：重发上一次载荷，并补发被最小间隔挡下的新状态。
pub async fn keepalive(plugin: &PluginState, ctx: &Context) {
    let payload = plugin
        .with_presence_mut(|presence| presence.keepalive().or_else(|| presence.take_pending()))
        .flatten();
    let Some(payload) = payload else {
        return;
    };
    match ctx.collab_send_ephemeral(CHANNEL, payload.clone(), None).await {
        Ok(result) if result.queued => {
            plugin.with_presence_mut(|presence| presence.note_sent(payload));
        }
        Ok(_) => {
            plugin.with_presence_mut(|presence| presence.defer(payload));
        }
        Err(error) => ctx.log_error(&format!("保活画布在场失败：{error}")),
    }
}

/// 离开画布：发离场帧让对端立即摘除本端，并清掉本端缓存。
pub async fn leave(plugin: &PluginState, ctx: &Context, path: &str) {
    let value = serde_json::json!({ "path": path, "present": false });
    if let Err(error) = ctx.collab_send_ephemeral(CHANNEL, value, None).await {
        ctx.log_error(&format!("发送画布离场失败：{error}"));
    }
    plugin.with_presence_mut(Presence::reset);
}

/// 收到一条对端状态：离场帧摘除该对端，其余按路径匹配后整条替换。
pub fn apply(plugin: &PluginState, params: CollabMessageParams) {
    // 本端上行的回声不是对端：把它记成 peer 会让自己显示成「被占用」
    let self_echo = plugin
        .with_session(|session| session.view.self_id.as_deref() == Some(params.peer_id.as_str()))
        .unwrap_or(false);
    if self_echo {
        return;
    }
    let Some(payload) = params.payload.as_object() else {
        return;
    };
    let Some(path) = payload.get("path").and_then(serde_json::Value::as_str) else {
        return;
    };
    let present = payload
        .get("present")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    let selected = string_array(payload.get("selected"));
    let streaming = string_array(payload.get("streaming"));
    let mut locks = HashMap::new();
    if let Some(map) = payload.get("locks").and_then(serde_json::Value::as_object) {
        for (id, since) in map {
            if let Some(ms) = since.as_i64() {
                locks.insert(id.clone(), ms);
            }
        }
    }
    // 身份色以宿主名册为准（跨插件一致）；旧对端未带名册色时再回落到帧内色 / 稳定色
    let color = params
        .peer_color
        .as_deref()
        .and_then(parse_hex)
        .or_else(|| {
            payload
                .get("color")
                .and_then(serde_json::Value::as_str)
                .and_then(parse_hex)
        })
        .unwrap_or_else(|| fallback_color(&params.peer_id));
    let name = params
        .peer_name
        .clone()
        .filter(|name| !name.is_empty())
        .or_else(|| {
            payload
                .get("name")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default();
    plugin.with_session_mut(|session| {
        // 对端可能开着另一张画布：状态按 peer 保留，绘制期再按当前路径过滤
        if !present {
            session
                .view
                .remote
                .retain(|peer| peer.peer_id != params.peer_id);
            return;
        }
        let peer = RemotePeer {
            peer_id: params.peer_id.clone(),
            path: path.to_string(),
            name,
            color,
            selected,
            streaming,
            locks,
            at: Instant::now(),
        };
        if let Some(slot) = session
            .view
            .remote
            .iter_mut()
            .find(|p| p.peer_id == params.peer_id)
        {
            *slot = peer;
        } else {
            session.view.remote.push(peer);
        }
    });
}

/// 名册变化：摘除不在名册内的对端（TTL 仍兜底），返回是否有条目被移除。
pub fn retain_roster(plugin: &PluginState, known: &[String]) -> bool {
    plugin
        .with_session_mut(|session| {
            let before = session.view.remote.len();
            session
                .view
                .remote
                .retain(|peer| known.iter().any(|id| id == &peer.peer_id));
            before != session.view.remote.len()
        })
        .unwrap_or(false)
}

/// 剔除过期对端（返回是否有条目被移除）。
pub fn prune(plugin: &PluginState) -> bool {
    plugin
        .with_session_mut(|session| {
            let before = session.view.remote.len();
            session
                .view
                .remote
                .retain(|peer| peer.at.elapsed().as_millis() < PEER_TTL_MS);
            before != session.view.remote.len()
        })
        .unwrap_or(false)
}

/// 清空对端（切仓库 / 断连）。
pub fn clear(plugin: &PluginState) {
    plugin.with_session_mut(|session| session.view.remote.clear());
}

fn string_array(value: Option<&serde_json::Value>) -> Vec<String> {
    value
        .and_then(serde_json::Value::as_array)
        .map(|array| {
            array
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn parse_hex(raw: &str) -> Option<(u8, u8, u8)> {
    let hex = raw.strip_prefix('#')?;
    if hex.len() < 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some((r, g, b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Session;
    use atelyx_format::canvas::{CANVAS_SCHEMA, CanvasFile};

    fn plugin_on(file: &str) -> std::sync::Arc<PluginState> {
        let plugin = PluginState::new();
        let session = Session::new(
            file.into(),
            CanvasFile {
                schema: CANVAS_SCHEMA.into(),
                id: "c".into(),
                title: "a".into(),
                nodes: Vec::new(),
                edges: Vec::new(),
                created_at: 0,
                updated_at: 0,
            },
            1,
        );
        *plugin.session.lock().unwrap() = Some(session);
        plugin
    }

    fn message(peer: &str, path: &str) -> CollabMessageParams {
        CollabMessageParams {
            channel: CHANNEL.into(),
            payload: serde_json::json!({ "path": path, "present": true }),
            peer_id: peer.into(),
            peer_name: None,
            peer_color: None,
        }
    }

    /// 本端上行的回声不能记成对端，否则自己的锁会被渲染成「被他人占用」。
    #[test]
    fn apply_skips_self_echo() {
        let plugin = plugin_on("a.atlx");
        plugin.with_session_mut(|session| session.view.self_id = Some("me".into()));
        apply(&plugin, message("me", "a.atlx"));
        assert!(plugin.with_session(|session| session.view.remote.is_empty()).unwrap());

        apply(&plugin, message("p1", "a.atlx"));
        assert_eq!(
            plugin.with_session(|session| session.view.remote.len()).unwrap(),
            1
        );
    }

    /// 对端开着别的画布也要保留状态，绘制期再按当前路径过滤。
    #[test]
    fn apply_keeps_peer_from_other_canvas() {
        let plugin = plugin_on("a.atlx");
        apply(&plugin, message("p1", "b.atlx"));
        let path = plugin
            .with_session(|session| session.view.remote[0].path.clone())
            .unwrap();
        assert_eq!(path, "b.atlx");
    }

    /// 名册色优先于帧内自报色，同一成员在各插件里由宿主统一取色。
    #[test]
    fn apply_prefers_roster_color() {
        let plugin = plugin_on("a.atlx");
        let mut params = message("p1", "a.atlx");
        params.peer_color = Some("#123456".into());
        params.payload["color"] = serde_json::json!("#abcdef");
        apply(&plugin, params);
        let color = plugin
            .with_session(|session| session.view.remote[0].color)
            .unwrap();
        assert_eq!(color, parse_hex("#123456").unwrap());
    }

    /// 断链 / 离线后名册为空，远端条目随事件立即摘除，不等 TTL。
    #[test]
    fn retain_roster_clears_disconnected_peers() {
        let plugin = plugin_on("a.atlx");
        apply(&plugin, message("p1", "a.atlx"));
        assert!(retain_roster(&plugin, &[]));
        assert!(plugin.with_session(|session| session.view.remote.is_empty()).unwrap());

        apply(&plugin, message("p1", "a.atlx"));
        assert!(!retain_roster(&plugin, &["p1".to_string()]));
        assert_eq!(
            plugin.with_session(|session| session.view.remote.len()).unwrap(),
            1
        );
    }
}

/// 对端未带身份色时按 peer id 取一个稳定色。
fn fallback_color(peer_id: &str) -> (u8, u8, u8) {
    const COLORS: [(u8, u8, u8); 6] = [
        (0x61, 0xaf, 0xef),
        (0xef, 0x8f, 0x61),
        (0x8f, 0xc9, 0x77),
        (0xc9, 0x7f, 0xd0),
        (0xef, 0xc9, 0x61),
        (0x77, 0xc9, 0xc9),
    ];
    let hash = peer_id.bytes().fold(0usize, |acc, byte| {
        acc.wrapping_mul(31).wrapping_add(byte as usize)
    });
    COLORS[hash % COLORS.len()]
}
