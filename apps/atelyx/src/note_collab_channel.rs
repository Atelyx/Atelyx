//! 笔记协作在场：光标 / 选区经通用瞬时通道上行并解析为编辑面光标视图（载荷不含正文，按 `file` 过滤）。

use std::time::Instant;

use atelyx_core::presence::{PresenceChannel, PresenceState};
use serde_json::{json, Value};

use crate::ui::palette as pal;
pub use atelyx_core::presence::Identity;

/// 宿主内置通道名（插件通道为 `<插件id>:<频道>`，两套命名空间互不覆盖）。
pub const CHANNEL: &str = "host.note-aware";

/// 远端光标：选区为全文 UTF-16 偏移（无选区 = 只上报身份，渲染层不画光标）。
#[derive(Clone, PartialEq)]
pub struct RemoteCaret {
    pub name: String,
    pub color: String,
    pub anchor: Option<u32>,
    pub head: Option<u32>,
}

/// 本端在场状态：身份与选区都参与变化检测（改名 / 换色也要让对端看到）。
#[derive(Clone, PartialEq)]
pub struct NotePresence {
    file: String,
    identity: (String, String),
    selection: Option<(u32, u32)>,
}

impl NotePresence {
    fn new(identity: &Identity<'_>, file: &str, selection: Option<(u32, u32)>) -> Self {
        NotePresence { file: file.to_owned(), identity: identity.pair(), selection }
    }
}

impl PresenceState for NotePresence {
    type Remote = RemoteCaret;

    fn file(&self) -> &str { &self.file }

    fn encode(&self) -> Value {
        let selection = self.selection.map(|(anchor, head)| json!({ "anchor": anchor, "head": head }));
        json!({
            "file": self.file,
            "user": { "name": self.identity.0, "color": self.identity.1 },
            "selection": selection,
        })
    }

    fn decode(state: &Value, _peer_id: &str) -> Option<Self::Remote> {
        let user = state.get("user");
        let text = |key: &str| user.and_then(|u| u.get(key)).and_then(Value::as_str);
        let unit = |key: &str| {
            state.get("selection").and_then(|s| s.get(key)).and_then(Value::as_u64)
                .and_then(|v| u32::try_from(v).ok())
        };
        Some(RemoteCaret {
            name: text("name").unwrap_or_default().to_owned(),
            color: text("color").filter(|c| !c.is_empty())
                .unwrap_or(pal::FALLBACK_COLLAB_COLOR).to_owned(),
            anchor: unit("anchor"),
            head: unit("head"),
        })
    }
}

pub struct NoteCollabChannel {
    presence: PresenceChannel<NotePresence>,
}

impl NoteCollabChannel {
    pub fn new() -> Self { Self { presence: PresenceChannel::new() } }

    pub fn file(&self) -> Option<&str> { self.presence.file() }
    pub fn remote(&self) -> &[RemoteCaret] { self.presence.remote() }
    pub fn has_pending(&self) -> bool { self.presence.has_pending() }
    pub fn has_peers(&self) -> bool { self.presence.has_peers() }

    /// 选区 / 身份变化上报（切换文件由通道内部转入场；无变化 = None）。
    pub fn update(
        &mut self,
        now: Instant,
        identity: &Identity<'_>,
        file: &str,
        selection: Option<(u32, u32)>,
    ) -> Option<Value> {
        self.presence.update(now, NotePresence::new(identity, file, selection))
    }

    /// 文件换路跟随（改名广播）：挂靠路径改写并重发，选区偏移与路径无关继续有效。
    pub fn relocate(
        &mut self,
        now: Instant,
        identity: &Identity<'_>,
        old: &str,
        new: &str,
    ) -> Option<Value> {
        self.presence.relocate(now, old, new, |sent| NotePresence {
            selection: sent.selection,
            ..NotePresence::new(identity, new, None)
        })
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

impl Default for NoteCollabChannel { fn default() -> Self { Self::new() } }

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn identity() -> Identity<'static> {
        Identity { name: "爱丽丝", color: "#e0533d" }
    }

    #[test]
    fn 上报文件身份与选区_身份变化也上行() {
        let mut ch = NoteCollabChannel::new();
        let now = Instant::now();
        let first = ch.update(now, &identity(), "a.md", Some((3, 7))).expect("首次应产帧");
        assert_eq!(first["state"]["file"], "a.md");
        assert_eq!(first["state"]["user"]["name"], "爱丽丝");
        assert_eq!(first["state"]["selection"]["anchor"], 3);
        assert!(ch.update(now, &identity(), "a.md", Some((3, 7))).is_none());

        let renamed = Identity { name: "爱丽丝2", color: "#e0533d" };
        let frame = ch
            .update(now + Duration::from_millis(60), &renamed, "a.md", Some((3, 7)))
            .expect("改名应对端可见");
        assert_eq!(frame["state"]["user"]["name"], "爱丽丝2");
    }

    #[test]
    fn 改名跟随_挂靠路径改写并重发() {
        let mut ch = NoteCollabChannel::new();
        let now = Instant::now();
        ch.update(now, &identity(), "notes/a.md", Some((3, 7))).expect("进入");
        let moved = ch
            .relocate(now + Duration::from_millis(60), &identity(), "notes/a.md", "笔记/a.md")
            .expect("改名应重发在场状态");
        assert_eq!(moved["state"]["file"], "笔记/a.md");
        assert_eq!(moved["state"]["selection"]["anchor"], 3, "选区偏移与路径无关");
        assert!(ch
            .relocate(now + Duration::from_millis(120), &identity(), "other.md", "x.md")
            .is_none());
        assert_eq!(ch.file(), Some("笔记/a.md"));
    }

    #[test]
    fn 远端光标只收同文件_u32偏移与颜色回落() {
        let mut ch = NoteCollabChannel::new();
        let now = Instant::now();
        ch.update(now, &identity(), "a.md", None).expect("进入");
        let payload = json!({
            "clientId": 9, "clock": 1,
            "state": { "file": "b.md", "user": { "name": "鲍勃", "color": "#3d7be0" },
                       "selection": { "anchor": 1, "head": 2 } },
        });
        ch.apply("p1", &payload, now);
        assert!(ch.remote().is_empty(), "别的文件不画");

        let long = json!({
            "clientId": 9, "clock": 2,
            "state": { "file": "a.md", "user": { "name": "鲍勃" },
                       "selection": { "anchor": 70_000, "head": 70_010 } },
        });
        ch.apply("p1", &long, now + Duration::from_millis(60));
        let remote = ch.remote();
        assert_eq!(remote.len(), 1);
        assert_eq!(remote[0].color, pal::FALLBACK_COLLAB_COLOR);
        assert_eq!(remote[0].anchor, Some(70_000));
        assert_eq!(remote[0].head, Some(70_010));
    }
}
