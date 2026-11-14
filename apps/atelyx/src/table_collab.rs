//! 表格协作在场：本机选区经通用瞬时通道上行，远端选区解析为网格绘制视图。
//!
//! 通道载荷只带稳定 id（行 / 字段 id）与身份色，不含单元格内容。

use std::time::Instant;

use atelyx_core::presence::{PresenceChannel, PresenceState};
use atelyx_core::table_ops::{self, TableSelection};
use serde_json::{json, Value};

use crate::ui::palette as pal;

pub use atelyx_core::presence::Identity;

/// 宿主内置通道名（插件通道为 `<插件id>:<频道>`，两套命名空间互不覆盖）。
pub const CHANNEL: &str = "host.table";

/// 远端协作者在当前表格上的选区（身份色同时保留协议原串与解析值，畸形色两者都回落灰）。
#[derive(Clone, PartialEq)]
pub struct RemoteSelection {
    pub name: String,
    pub color: iced::Color,
    pub color_hex: String,
    pub selection: TableSelection,
}

/// 本端在场状态：身份与选区都参与变化检测（改名 / 换色也要让对端看到）。
#[derive(Clone, PartialEq)]
pub struct TablePresence {
    file: String,
    identity: (String, String),
    selection: Option<TableSelection>,
}

impl PresenceState for TablePresence {
    type Remote = RemoteSelection;

    fn file(&self) -> &str { &self.file }

    fn encode(&self) -> Value {
        let selection = self.selection.as_ref().map(table_ops::selection_to_json);
        json!({
            "file": self.file,
            "name": self.identity.0,
            "color": self.identity.1,
            "selection": selection.unwrap_or(Value::Null),
        })
    }

    fn decode(state: &Value, _peer_id: &str) -> Option<Self::Remote> {
        let selection = table_ops::selection_from_json(state.get("selection")?)?;
        let (color, color_hex) = match state.get("color").and_then(Value::as_str) {
            Some(raw) => match crate::layout_view::parse_hex_color(raw) {
                Some(color) => (color, raw.to_string()),
                None => (pal::FALLBACK_COLLAB, pal::FALLBACK_COLLAB_COLOR.to_string()),
            },
            None => (pal::FALLBACK_COLLAB, pal::FALLBACK_COLLAB_COLOR.to_string()),
        };
        let name = state.get("name").and_then(Value::as_str).unwrap_or_default().to_owned();
        Some(RemoteSelection { name, color, color_hex, selection })
    }
}

pub struct TableCollab {
    presence: PresenceChannel<TablePresence>,
}

impl TableCollab {
    pub fn new() -> Self { Self { presence: PresenceChannel::new() } }

    pub fn file(&self) -> Option<&str> { self.presence.file() }
    pub fn remote(&self) -> &[RemoteSelection] { self.presence.remote() }
    pub fn has_pending(&self) -> bool { self.presence.has_pending() }
    pub fn has_peers(&self) -> bool { self.presence.has_peers() }
    pub fn sent_selection(&self) -> Option<&TableSelection> {
        self.presence.sent().and_then(|s| s.selection.as_ref())
    }
    pub fn sent_identity(&self) -> Option<(&str, &str)> {
        self.presence.sent().map(|s| (s.identity.0.as_str(), s.identity.1.as_str()))
    }

    /// 选区 / 身份变化上报（切换文件由通道内部转入场；无变化 = None）。
    pub fn update(
        &mut self,
        now: Instant,
        identity: &Identity<'_>,
        file: &str,
        selection: Option<&TableSelection>,
    ) -> Option<Value> {
        let state = TablePresence {
            file: file.to_owned(),
            identity: identity.pair(),
            selection: selection.cloned(),
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
    pub fn refresh(&mut self, now: Instant) { self.presence.refresh(now); }
    pub fn drop_peer(&mut self, peer_id: &str) -> bool { self.presence.drop_peer(peer_id) }
    pub fn clear_remotes(&mut self) { self.presence.clear_remotes() }
}

impl Default for TableCollab { fn default() -> Self { Self::new() } }

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn identity() -> Identity<'static> {
        Identity { name: "爱丽丝", color: "#e0533d" }
    }

    #[test]
    fn 上报文件与选区_身份变化也上行() {
        let mut collab = TableCollab::new();
        let now = Instant::now();
        let sel = TableSelection::Cell { row_id: "r1".into(), field_id: "f1".into() };
        let first = collab.update(now, &identity(), "a.atb", Some(&sel)).expect("首次应产帧");
        assert_eq!(first["state"]["file"], "a.atb");
        assert_eq!(first["state"]["selection"]["kind"], "cell");
        assert_eq!(collab.sent_selection(), Some(&sel));
        assert_eq!(collab.sent_identity(), Some(("爱丽丝", "#e0533d")));
        assert!(collab.update(now, &identity(), "a.atb", Some(&sel)).is_none());

        let renamed = Identity { name: "爱丽丝2", color: "#e0533d" };
        let frame = collab
            .update(now + Duration::from_millis(60), &renamed, "a.atb", Some(&sel))
            .expect("改名应对端可见");
        assert_eq!(frame["state"]["name"], "爱丽丝2");
    }

    #[test]
    fn 远端只收同文件且有选区_颜色回落() {
        let mut collab = TableCollab::new();
        let now = Instant::now();
        collab.update(now, &identity(), "a.atb", None).expect("进入");
        let payload = |clock: u64, state: Value| json!({ "clientId": 9, "clock": clock, "state": state });
        collab.apply("p1", &payload(1, json!({ "file": "b.atb", "selection": { "kind": "all" } })), now);
        assert!(collab.remote().is_empty(), "别的文件不画");

        collab.apply("p1", &payload(2, json!({ "file": "a.atb", "selection": Value::Null })), now);
        assert!(collab.remote().is_empty(), "无选区不画");

        let bad_color = json!({ "file": "a.atb", "name": "鲍勃", "color": "not-a-color",
                                "selection": { "kind": "column", "fieldId": "f2" } });
        collab.apply("p1", &payload(3, bad_color), now + Duration::from_millis(60));
        let remote = collab.remote();
        assert_eq!(remote.len(), 1, "颜色畸形只回落颜色，条目仍可见");
        assert_eq!(remote[0].name, "鲍勃");
        assert_eq!(remote[0].color, pal::FALLBACK_COLLAB);
        assert_eq!(remote[0].color_hex, pal::FALLBACK_COLLAB_COLOR);
        assert_eq!(remote[0].selection, TableSelection::Column("f2".into()));
    }

    #[test]
    fn 超时剔除_重连重发() {
        let mut collab = TableCollab::new();
        let now = Instant::now();
        collab.update(now, &identity(), "a.atb", None).expect("进入");
        let payload = json!({ "clientId": 9, "clock": 1,
                              "state": { "file": "a.atb", "name": "鲍勃", "selection": { "kind": "all" } } });
        collab.apply("p1", &payload, now);
        assert_eq!(collab.remote().len(), 1);
        collab.refresh(now + Duration::from_secs(31));
        assert!(collab.remote().is_empty(), "30s 无更新应剔除");

        let resent = collab.resend(now).expect("在场应可重发");
        assert_eq!(resent["state"]["file"], "a.atb");
    }
}
