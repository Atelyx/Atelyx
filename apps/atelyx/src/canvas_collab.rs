//! 画布协作在场：本机选中 / 对话卡锁声明 / 流式卡经通用瞬时通道上行，远端条目解析为
//! 画布绘制与锁判定视图。载荷只带节点 id 与身份色，不含卡内容。

use std::collections::HashMap;
use std::time::Instant;

use atelyx_core::presence::{PresenceChannel, PresenceState};
use serde_json::{json, Value};

use crate::ui::palette as pal;

pub use atelyx_core::presence::Identity;

/// 宿主内置通道名（插件通道为 `<插件id>:<频道>`，两套命名空间互不覆盖）。
pub const CHANNEL: &str = "host.canvas";

/// 远端协作者在当前画布上的在场条目（身份色畸形回落灰）。
#[derive(Clone, PartialEq)]
pub struct RemotePeer {
    /// 上报该条目的连接 id（锁主判定的声明归属）。
    pub peer_id: String,
    pub name: String,
    pub color: iced::Color,
    pub color_hex: String,
    /// 对端当前选中的卡节点 id（原序）。
    pub selection: Vec<String>,
    /// 对端的对话卡锁声明（nodeId → 占锁时刻毫秒）。
    pub locked: Vec<(String, i64)>,
    /// 对端流式生成中的对话卡节点 id。
    pub streaming: Vec<String>,
}

impl RemotePeer {
    pub fn locks(&self, node_id: &str) -> Option<i64> {
        self.locked.iter().find(|(id, _)| id == node_id).map(|(_, since)| *since)
    }

    pub fn is_streaming(&self, node_id: &str) -> bool {
        self.streaming.iter().any(|id| id == node_id)
    }
}

/// 本端在场内容（选中 / 锁 / 流式 + 身份），等于最后一次上行的状态。
#[derive(Clone, PartialEq)]
pub struct CanvasPresence {
    file: String,
    identity: (String, String),
    selection: Vec<String>,
    locked: Vec<(String, i64)>,
    streaming: Vec<String>,
}

impl PresenceState for CanvasPresence {
    type Remote = RemotePeer;

    fn file(&self) -> &str { &self.file }

    fn encode(&self) -> Value {
        let locked: Vec<Value> = self
            .locked
            .iter()
            .map(|(id, since)| json!({ "id": id, "since": since }))
            .collect();
        json!({
            "file": self.file,
            "name": self.identity.0,
            "color": self.identity.1,
            "selection": self.selection,
            "locked": locked,
            "streaming": self.streaming,
        })
    }

    fn decode(state: &Value, peer_id: &str) -> Option<Self::Remote> {
        let (color, color_hex) = parsed_color(state);
        Some(RemotePeer {
            peer_id: peer_id.to_owned(),
            name: state.get("name").and_then(Value::as_str).unwrap_or_default().to_owned(),
            color,
            color_hex,
            selection: string_list(state.get("selection")),
            locked: lock_list(state.get("locked")),
            streaming: string_list(state.get("streaming")),
        })
    }
}

/// 身份色：缺失 / 畸形回落灰（同时保留协议原串，名签取色用）。
fn parsed_color(state: &Value) -> (iced::Color, String) {
    match state.get("color").and_then(Value::as_str) {
        Some(raw) => match crate::layout_view::parse_hex_color(raw) {
            Some(color) => (color, raw.to_string()),
            None => (pal::FALLBACK_COLLAB, pal::FALLBACK_COLLAB_COLOR.to_string()),
        },
        None => (pal::FALLBACK_COLLAB, pal::FALLBACK_COLLAB_COLOR.to_string()),
    }
}

fn string_list(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default()
}

fn lock_list(v: Option<&Value>) -> Vec<(String, i64)> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|e| Some((e.get("id")?.as_str()?.to_owned(), e.get("since")?.as_i64()?)))
                .collect()
        })
        .unwrap_or_default()
}

pub struct CanvasCollab {
    presence: PresenceChannel<CanvasPresence>,
}

impl CanvasCollab {
    pub fn new() -> Self { Self { presence: PresenceChannel::new() } }

    pub fn file(&self) -> Option<&str> { self.presence.file() }
    pub fn remote(&self) -> &[RemotePeer] { self.presence.remote() }

    /// 本端在场内容与已上行状态是否一致（高频路径的无分配判等）。
    /// `locked` / `streaming` 传会话锁表与卡运行态的活引用，不要求顺序一致。
    pub fn matches_presence(
        &self,
        identity: (&str, &str),
        file: &str,
        selection: &[String],
        locked: &HashMap<String, i64>,
        streaming_ids: &[&str],
    ) -> bool {
        let Some(sent) = self.presence.sent() else { return false };
        self.presence.file() == Some(file)
            && sent.identity.0 == identity.0
            && sent.identity.1 == identity.1
            && sent.selection.as_slice() == selection
            && sent.locked.len() == locked.len()
            && sent.locked.iter().all(|(id, since)| locked.get(id) == Some(since))
            && sent.streaming.len() == streaming_ids.len()
            && sent.streaming.iter().all(|id| streaming_ids.contains(&id.as_str()))
    }

    /// 选中 / 锁 / 流式变化上报（切换文件由通道内部转入场；无变化 = None）。
    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &mut self,
        now: Instant,
        identity: &Identity<'_>,
        file: &str,
        selection: Vec<String>,
        locked: Vec<(String, i64)>,
        streaming: Vec<String>,
    ) -> Option<Value> {
        let state = CanvasPresence {
            file: file.to_owned(),
            identity: identity.pair(),
            selection,
            locked,
            streaming,
        };
        self.presence.update(now, state)
    }

    pub fn leave(&mut self, now: Instant) -> Option<Value> { self.presence.leave(now) }
    pub fn flush(&mut self, now: Instant) -> Option<Value> { self.presence.flush(now) }
    pub fn resend(&mut self, now: Instant) -> Option<Value> { self.presence.resend(now) }
    pub fn keepalive_if_due(&mut self, now: Instant) -> Option<Value> {
        self.presence.keepalive_if_due(now)
    }
    pub fn apply(&mut self, peer_id: &str, payload: &Value, now: Instant) -> bool {
        self.presence.apply(peer_id, payload, now)
    }
    /// 返回远端条目是否变化（调用方据此作废世界层缓存）。
    pub fn refresh(&mut self, now: Instant) -> bool { self.presence.refresh(now) }
    pub fn drop_peer(&mut self, peer_id: &str) -> bool { self.presence.drop_peer(peer_id) }
    pub fn clear_remotes(&mut self) { self.presence.clear_remotes() }
}

impl Default for CanvasCollab { fn default() -> Self { Self::new() } }

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn identity() -> Identity<'static> {
        Identity { name: "爱丽丝", color: "#e0533d" }
    }

    fn remote_payload(client_id: u64, clock: u64, state: Value) -> Value {
        json!({ "clientId": client_id, "clock": clock, "state": state })
    }

    #[test]
    fn 进入后上报全部在场内容_同内容不再上行() {
        let mut collab = CanvasCollab::new();
        let now = Instant::now();
        let first = collab
            .update(now, &identity(), "a.atlx", vec!["n1".into()], vec![("n2".into(), 5)], vec!["n2".into()])
            .expect("首次应产帧");
        assert_eq!(first["state"]["file"], "a.atlx");
        assert_eq!(first["state"]["selection"], json!(["n1"]));
        assert_eq!(first["state"]["locked"], json!([{ "id": "n2", "since": 5 }]));
        assert_eq!(first["state"]["streaming"], json!(["n2"]));
        assert!(collab
            .update(now, &identity(), "a.atlx", vec!["n1".into()], vec![("n2".into(), 5)], vec!["n2".into()])
            .is_none());
        assert!(collab.matches_presence(
            ("爱丽丝", "#e0533d"),
            "a.atlx",
            &["n1".to_owned()],
            &HashMap::from([("n2".to_owned(), 5)]),
            &["n2"],
        ));
    }

    #[test]
    fn 远端只收同文件条目_畸形降级() {
        let mut collab = CanvasCollab::new();
        let now = Instant::now();
        collab
            .update(now, &identity(), "a.atlx", Vec::new(), Vec::new(), Vec::new())
            .expect("进入");
        collab.apply("p1", &remote_payload(9, 1, json!({ "file": "b.atlx", "selection": ["x"] })), now);
        assert!(collab.remote().is_empty(), "别的文件不收");

        let bad = json!({
            "file": "a.atlx", "name": "鲍勃", "color": "not-a-color",
            "selection": ["n1"], "locked": [{ "id": "n1" }, { "id": "n2", "since": 9 }], "streaming": [3],
        });
        collab.apply("p1", &remote_payload(9, 2, bad), now + Duration::from_millis(60));
        let remote = collab.remote();
        assert_eq!(remote.len(), 1, "畸形字段降级，条目仍可见");
        assert_eq!(remote[0].color, pal::FALLBACK_COLLAB);
        assert_eq!(remote[0].selection, vec!["n1".to_string()]);
        assert_eq!(remote[0].locks("n1"), None, "缺 since 的锁声明丢弃");
        assert_eq!(remote[0].locks("n2"), Some(9));
        assert!(!remote[0].is_streaming("n1"), "非字符串流式 id 丢弃");
    }

    #[test]
    fn 超时剔除报告变化_重连重发() {
        let mut collab = CanvasCollab::new();
        let now = Instant::now();
        collab
            .update(now, &identity(), "a.atlx", Vec::new(), Vec::new(), Vec::new())
            .expect("进入");
        let payload = remote_payload(9, 1, json!({ "file": "a.atlx", "selection": ["n1"] }));
        collab.apply("p1", &payload, now);
        assert_eq!(collab.remote().len(), 1);
        assert!(!collab.refresh(now + Duration::from_secs(1)), "在场未变不报告变化");
        assert!(collab.refresh(now + Duration::from_secs(31)), "超时剔除应报告变化");
        assert!(collab.remote().is_empty(), "30s 无更新应剔除");

        let resent = collab.resend(now).expect("在场应可重发");
        assert_eq!(resent["state"]["file"], "a.atlx");
    }
}
