//! 笔记协作在场：本机光标 / 选区经通用瞬时通道上行，远端状态解析为编辑面光标视图。
//!
//! 通道载荷只带全文 UTF-16 偏移与身份色，不含正文；对端打开别的文件时按 `file` 过滤，
//! 换文件靠 state 变化自然生效，无需先发离场帧。

use std::time::Instant;

use atelyx_core::ephemeral::EphemeralChannel;
use serde_json::{json, Value};

use crate::ui::palette as pal;

/// 宿主内置通道名（插件通道为 `<插件id>:<频道>`，两套命名空间互不覆盖）。
pub const CHANNEL: &str = "host.note-aware";

/// 本端身份（名签与成员色由壳提供；通道层不感知）。
pub struct Identity<'a> {
    pub name: &'a str,
    pub color: &'a str,
}

/// 远端光标：选区为全文 UTF-16 偏移（无选区 = 只上报身份，渲染层不画光标）。
pub struct RemoteCaret {
    pub name: String,
    pub color: String,
    pub anchor: Option<u32>,
    pub head: Option<u32>,
}

pub struct NoteCollabChannel {
    channel: EphemeralChannel,
    /// 本端挂着的文件（None = 不在笔记）。
    file: Option<String>,
    /// 已写入通道状态的选区（变化检测基准，含被限频压下待补发的那份）。
    sent: Option<(u32, u32)>,
    /// 已写入通道状态的身份（昵称 / 成员色）：设置里改名或换色也要让对端看到。
    sent_identity: Option<(String, String)>,
    /// 当前文件可绘制的远端光标（事件与 tick 驱动刷新，绘制只读）。
    remote: Vec<RemoteCaret>,
}

impl NoteCollabChannel {
    pub fn new() -> Self {
        NoteCollabChannel {
            channel: EphemeralChannel::new(),
            file: None,
            sent: None,
            sent_identity: None,
            remote: Vec::new(),
        }
    }

    /// 本端挂着的文件（None = 未在场）。
    pub fn file(&self) -> Option<&str> {
        self.file.as_deref()
    }

    pub fn remote(&self) -> &[RemoteCaret] {
        &self.remote
    }

    /// 进入 / 切换文件：立即构造本端状态（无选区也上报，供对端显示「在此文件」）。
    pub fn enter(
        &mut self,
        now: Instant,
        identity: &Identity<'_>,
        file: &str,
        selection: Option<(u32, u32)>,
    ) -> Option<Value> {
        self.file = Some(file.to_owned());
        self.sent = selection;
        self.sent_identity = Some(identity_pair(identity));
        // 换文件后旧文件的远端条目不属本视图（对端条目仍在通道里，按 file 过滤）
        self.refresh(now);
        self.channel
            .set_local_state(now, state_json(identity, file, selection))
    }

    /// 选区变化上报（同文件且选区未变 = None；限频压下的变化由 [`flush`](Self::flush) 补发）。
    pub fn update(
        &mut self,
        now: Instant,
        identity: &Identity<'_>,
        file: &str,
        selection: Option<(u32, u32)>,
    ) -> Option<Value> {
        if self.file.as_deref() != Some(file) {
            return self.enter(now, identity, file, selection);
        }
        if self.sent == selection && self.sent_identity.as_ref() == Some(&identity_pair(identity)) {
            return None;
        }
        self.sent = selection;
        self.sent_identity = Some(identity_pair(identity));
        self.channel
            .set_local_state(now, state_json(identity, file, selection))
    }

    /// 离场（关闭笔记 / 离开空间）：产出 null 条目并清本端记录。
    pub fn leave(&mut self, now: Instant) -> Option<Value> {
        self.file = None;
        self.sent = None;
        self.sent_identity = None;
        self.remote.clear();
        self.channel.leave(now)
    }

    pub fn flush(&mut self, now: Instant) -> Option<Value> {
        self.channel.flush(now)
    }

    pub fn keepalive_if_due(&mut self, now: Instant) -> Option<Value> {
        self.channel.keepalive_if_due(now)
    }

    pub fn resend(&mut self, now: Instant) -> Option<Value> {
        self.channel.resend(now)
    }

    pub fn has_pending(&self) -> bool {
        self.channel.has_pending()
    }

    /// 通道里是否有任何远端条目（含无选区的）：有对端才需要周期拍做剔除与保活。
    pub fn has_peers(&self) -> bool {
        self.channel.has_remotes()
    }

    /// 合入他人帧并刷新视图；返回远端集合是否变化。
    pub fn apply(&mut self, peer_id: &str, payload: &Value, now: Instant) -> bool {
        if !self.channel.apply_remote(peer_id, payload, now) {
            return false;
        }
        self.refresh(now);
        true
    }

    /// 文件换路跟随（改名广播 / 远端 relocate）：挂靠路径改写并重发在场状态。
    /// 选区指向正文偏移、与路径无关，继续有效；对端靠 state.file 过滤自然切到新路径。
    pub fn relocate(
        &mut self,
        now: Instant,
        identity: &Identity<'_>,
        old: &str,
        new: &str,
    ) -> Option<Value> {
        if self.file.as_deref() != Some(old) {
            return None;
        }
        self.file = Some(new.to_owned());
        self.refresh(now);
        self.channel
            .set_local_state(now, state_json(identity, new, self.sent))
    }

    /// 重新解析远端条目（超时剔除也在这里发生）：只保留与当前文件匹配的条目。
    pub fn refresh(&mut self, now: Instant) {
        let Some(file) = self.file.clone() else {
            self.remote.clear();
            return;
        };
        self.remote = self
            .channel
            .remotes(now)
            .into_iter()
            .filter_map(|entry| {
                let state = &entry.state;
                if state.get("file").and_then(Value::as_str) != Some(file.as_str()) {
                    return None;
                }
                let user = state.get("user");
                let name = user
                    .and_then(|u| u.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let color = user
                    .and_then(|u| u.get("color"))
                    .and_then(Value::as_str)
                    .filter(|c| !c.is_empty())
                    .unwrap_or(pal::FALLBACK_COLLAB_COLOR)
                    .to_owned();
                let unit = |key: &str| {
                    state
                        .get("selection")
                        .and_then(|s| s.get(key))
                        .and_then(Value::as_u64)
                        .and_then(|v| u32::try_from(v).ok())
                };
                Some(RemoteCaret { name, color, anchor: unit("anchor"), head: unit("head") })
            })
            .collect();
    }

    /// 对端连接消失：摘除其条目并刷新。
    pub fn drop_peer(&mut self, peer_id: &str) -> bool {
        if !self.channel.drop_peer(peer_id) {
            return false;
        }
        self.refresh(Instant::now());
        true
    }

    /// 断开 / 离开空间：清远端（本端状态保留，重连后由 [`resend`](Self::resend) 重发）。
    pub fn clear_remotes(&mut self) {
        self.channel.clear_remotes();
        self.remote.clear();
    }
}

impl Default for NoteCollabChannel {
    fn default() -> Self {
        Self::new()
    }
}

fn identity_pair(identity: &Identity<'_>) -> (String, String) {
    (identity.name.to_owned(), identity.color.to_owned())
}

fn state_json(identity: &Identity<'_>, file: &str, selection: Option<(u32, u32)>) -> Value {
    json!({
        "file": file,
        "user": { "name": identity.name, "color": identity.color },
        "selection": selection
            .map(|(anchor, head)| json!({ "anchor": anchor, "head": head }))
            .unwrap_or(Value::Null),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn identity() -> Identity<'static> {
        Identity { name: "爱丽丝", color: "#e0533d" }
    }

    fn remote_payload(client_id: u64, clock: u64, file: &str, selection: Value) -> Value {
        json!({
            "clientId": client_id,
            "clock": clock,
            "state": { "file": file, "user": { "name": "鲍勃", "color": "#3d7be0" }, "selection": selection },
        })
    }

    #[test]
    fn 进入后上报文件与身份_同选区不再上行() {
        let mut ch = NoteCollabChannel::new();
        let now = Instant::now();
        let first = ch.enter(now, &identity(), "a.md", Some((3, 7))).expect("首次应产帧");
        assert_eq!(first["state"]["file"], "a.md");
        assert_eq!(first["state"]["user"]["name"], "爱丽丝");
        assert_eq!(first["state"]["selection"]["anchor"], 3);
        assert_eq!(ch.file(), Some("a.md"));
        assert!(ch.update(now, &identity(), "a.md", Some((3, 7))).is_none());
    }

    #[test]
    fn 身份变化也要上行_同身份同选区不再上行() {
        let mut ch = NoteCollabChannel::new();
        let now = Instant::now();
        ch.enter(now, &identity(), "a.md", Some((3, 7))).expect("进入");
        assert!(ch.update(now, &identity(), "a.md", Some((3, 7))).is_none());
        let renamed = Identity { name: "爱丽丝2", color: "#e0533d" };
        // 身份变化也产帧（限频窗口外）
        let renamed_frame = ch
            .update(now + Duration::from_millis(60), &renamed, "a.md", Some((3, 7)))
            .expect("改名应对端可见");
        assert_eq!(renamed_frame["state"]["user"]["name"], "爱丽丝2");
        assert!(ch
            .update(now + Duration::from_millis(120), &renamed, "a.md", Some((3, 7)))
            .is_none());
    }

    #[test]
    fn 改名跟随_挂靠路径改写并重发() {
        let mut ch = NoteCollabChannel::new();
        let now = Instant::now();
        ch.enter(now, &identity(), "notes/a.md", Some((3, 7))).expect("进入");
        ch.apply("p1", &remote_payload(9, 1, "notes/a.md", json!({ "anchor": 1, "head": 2 })), now);
        assert_eq!(ch.remote().len(), 1);

        let moved = ch
            .relocate(now + Duration::from_millis(60), &identity(), "notes/a.md", "笔记/a.md")
            .expect("改名应重发在场状态");
        assert_eq!(moved["state"]["file"], "笔记/a.md");
        assert_eq!(moved["state"]["selection"]["anchor"], 3, "选区偏移与路径无关");
        assert_eq!(ch.file(), Some("笔记/a.md"));
        assert!(ch.remote().is_empty(), "旧路径条目不属新视图");
        // 非本文件不改写
        assert!(ch
            .relocate(now + Duration::from_millis(120), &identity(), "other.md", "x.md")
            .is_none());
        assert_eq!(ch.file(), Some("笔记/a.md"));
    }

    #[test]
    fn 长文本选区u32不丢失() {
        let mut ch = NoteCollabChannel::new();
        let now = Instant::now();
        ch.enter(now, &identity(), "a.md", Some((70_000, 70_010))).expect("进入");
        let mut other = NoteCollabChannel::new();
        other.enter(now, &identity(), "a.md", None).expect("进入");
        let payload = json!({
            "clientId": 9, "clock": 1,
            "state": { "file": "a.md", "user": { "name": "鲍勃", "color": "#3d7be0" },
                       "selection": { "anchor": 70_000, "head": 70_010 } },
        });
        other.apply("p1", &payload, now);
        let remote = other.remote();
        assert_eq!(remote[0].anchor, Some(70_000));
        assert_eq!(remote[0].head, Some(70_010));
    }

    #[test]
    fn 换文件清旧视图_离场产null() {
        let mut ch = NoteCollabChannel::new();
        let now = Instant::now();
        ch.enter(now, &identity(), "a.md", None).expect("进入");
        ch.apply("p1", &remote_payload(9, 1, "a.md", json!({ "anchor": 1, "head": 2 })), now);
        assert_eq!(ch.remote().len(), 1);

        // 换文件落在限频窗口内：只记录，最新一份由 flush 补发
        assert!(ch.enter(now, &identity(), "b.md", None).is_none());
        assert!(ch.remote().is_empty(), "换文件后不保留旧文件光标");
        let swapped = ch.flush(now + Duration::from_millis(60)).expect("到期应补发新文件状态");
        assert_eq!(swapped["state"]["file"], "b.md");

        let leave = ch.leave(now).expect("在场应产离场帧");
        assert!(leave["state"].is_null());
        assert!(ch.leave(now).is_none());
    }

    #[test]
    fn 远端只收同文件条目_无选区仍保留身份() {
        let mut ch = NoteCollabChannel::new();
        let now = Instant::now();
        ch.enter(now, &identity(), "a.md", None).expect("进入");

        ch.apply("p1", &remote_payload(9, 1, "b.md", json!({ "anchor": 1, "head": 2 })), now);
        assert!(ch.remote().is_empty(), "别的文件不画");
        assert!(ch.has_peers(), "条目仍在通道里参与保活与剔除");

        ch.apply("p1", &remote_payload(9, 2, "a.md", Value::Null), now + Duration::from_millis(60));
        let remote = ch.remote();
        assert_eq!(remote.len(), 1);
        assert_eq!(remote[0].name, "鲍勃");
        assert_eq!(remote[0].anchor, None, "无选区 = 只有身份");
    }

    #[test]
    fn 远端颜色缺失回落_对端断链摘除_超时剔除() {
        let mut ch = NoteCollabChannel::new();
        let now = Instant::now();
        ch.enter(now, &identity(), "a.md", None).expect("进入");
        let no_color = json!({
            "clientId": 7, "clock": 1,
            "state": { "file": "a.md", "user": { "name": "鲍勃" },
                       "selection": { "anchor": 1, "head": 2 } },
        });
        ch.apply("p1", &no_color, now);
        assert_eq!(ch.remote()[0].color, pal::FALLBACK_COLLAB_COLOR);

        assert!(ch.drop_peer("p1"));
        assert!(ch.remote().is_empty());
        assert!(!ch.drop_peer("p1"));

        ch.apply("p1", &remote_payload(9, 1, "a.md", Value::Null), now);
        assert_eq!(ch.remote().len(), 1);
        ch.refresh(now + Duration::from_secs(31));
        assert!(ch.remote().is_empty(), "30s 无更新应剔除");

        let resent = ch.resend(now).expect("在场应可重发");
        assert_eq!(resent["state"]["file"], "a.md");
    }
}
