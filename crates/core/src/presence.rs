//! 协作在场通道：按文件过滤、限频 / 保活 / 超时剔除与远端条目投影。
//!
//! 状态码器由壳提供：本端状态 ⇄ 协议载荷、远端载荷 → 渲染条目；本层只管簿记。

use std::time::Instant;

use serde_json::Value;

use crate::ephemeral::EphemeralChannel;

/// 本端身份（名签与成员色；通道层不感知来源）。
pub struct Identity<'a> {
    pub name: &'a str,
    pub color: &'a str,
}

impl Identity<'_> {
    /// 身份对（昵称, 成员色）：壳内状态与变化检测直接持有。
    pub fn pair(&self) -> (String, String) {
        (self.name.to_owned(), self.color.to_owned())
    }
}

/// 一类协作面的在场状态码器。
pub trait PresenceState: Clone + PartialEq {
    /// 远端投影：各面的渲染结构不同，由壳实现。
    type Remote: Clone + PartialEq;

    /// 本端挂着的文件，须与 [`encode`](Self::encode) 产出的 `file` 一致。
    fn file(&self) -> &str;

    /// 本端状态 → 上行载荷（须含 `file` 字段，远端按它过滤）。
    fn encode(&self) -> Value;

    /// 远端载荷 → 渲染条目；`file` 已由通道过滤，畸形条目返回 None。
    fn decode(state: &Value, peer_id: &str) -> Option<Self::Remote>;
}

/// 单通道的在场状态机：本端状态 + 当前文件的远端投影。
pub struct PresenceChannel<S: PresenceState> {
    channel: EphemeralChannel,
    /// 本端挂着的文件（None = 未在场）。
    file: Option<String>,
    /// 已写入通道状态的本端状态（含被限频压下待补发的那份）。
    sent: Option<S>,
    /// 当前文件可绘制的远端条目（事件与 tick 驱动刷新，绘制只读）。
    remote: Vec<S::Remote>,
}

impl<S: PresenceState> Default for PresenceChannel<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: PresenceState> PresenceChannel<S> {
    pub fn new() -> Self {
        PresenceChannel {
            channel: EphemeralChannel::new(),
            file: None,
            sent: None,
            remote: Vec::new(),
        }
    }

    /// 本端挂着的文件（None = 未在场）。
    pub fn file(&self) -> Option<&str> {
        self.file.as_deref()
    }

    /// 已提交到通道状态的本端状态（调用方据此跳过无变化的同步）。
    pub fn sent(&self) -> Option<&S> {
        self.sent.as_ref()
    }

    /// 当前文件的远端投影（绘制只读）。
    pub fn remote(&self) -> &[S::Remote] {
        &self.remote
    }

    /// 进入 / 切换文件：立即上报本端状态（空选区也上报，供对端显示「在此文件」）。
    /// 旧文件的远端投影清空，同文件条目由 [`refresh`](Self::refresh) 重建。
    pub fn enter(&mut self, now: Instant, state: S) -> Option<Value> {
        self.file = Some(state.file().to_owned());
        self.sent = Some(state.clone());
        self.remote.clear();
        self.channel.set_local_state(now, state.encode())
    }

    /// 在场变化上报（同文件且状态未变 = None；限频压下的变化由 [`flush`](Self::flush) 补发）。
    pub fn update(&mut self, now: Instant, state: S) -> Option<Value> {
        if self.file.as_deref() != Some(state.file()) {
            return self.enter(now, state);
        }
        if self.sent.as_ref() == Some(&state) {
            return None;
        }
        self.sent = Some(state.clone());
        self.channel.set_local_state(now, state.encode())
    }

    /// 离场：产出 null 条目并清本端状态。
    pub fn leave(&mut self, now: Instant) -> Option<Value> {
        self.file = None;
        self.sent = None;
        self.remote.clear();
        self.channel.leave(now)
    }

    /// 补发被限频压下的本端状态（无待发变化或未到间隔 = None）。
    pub fn flush(&mut self, now: Instant) -> Option<Value> {
        self.channel.flush(now)
    }

    /// 到期保活重发（未在场 / 未到间隔 = None）。
    pub fn keepalive_if_due(&mut self, now: Instant) -> Option<Value> {
        self.channel.keepalive_if_due(now)
    }

    /// 立即重发当前状态（重连后覆盖对端旧条目）。
    pub fn resend(&mut self, now: Instant) -> Option<Value> {
        self.channel.resend(now)
    }

    /// 是否有被限频压下、尚未上行的本端状态。
    pub fn has_pending(&self) -> bool {
        self.channel.has_pending()
    }

    /// 通道里是否有任何远端条目：有对端才需要周期拍做剔除与保活。
    pub fn has_peers(&self) -> bool {
        self.channel.has_remotes()
    }

    /// 合入他人帧并刷新投影；返回帧是否被通道接受（对端投影可能未变）。
    pub fn apply(&mut self, peer_id: &str, payload: &Value, now: Instant) -> bool {
        if !self.channel.apply_remote(peer_id, payload, now) {
            return false;
        }
        self.refresh(now);
        true
    }

    /// 重新解析远端条目（超时剔除也在这里发生）：只保留本端文件匹配的条目。
    /// 返回投影是否变化（壳据此作废缓存）。
    pub fn refresh(&mut self, now: Instant) -> bool {
        let Some(file) = self.file.clone() else {
            let changed = !self.remote.is_empty();
            self.remote.clear();
            return changed;
        };
        let next: Vec<S::Remote> = self
            .channel
            .remotes(now)
            .into_iter()
            .filter(|entry| entry.state.get("file").and_then(Value::as_str) == Some(file.as_str()))
            .filter_map(|entry| S::decode(&entry.state, &entry.peer_id))
            .collect();
        let changed = next != self.remote;
        self.remote = next;
        changed
    }

    /// 挂靠文件改名：本端挂在 `old` 时改写为 `new` 并重发。`state` 按旧状态构造新状态，
    /// 与路径无关的内容（选区偏移等）由调用方保留。
    pub fn relocate(
        &mut self,
        now: Instant,
        old: &str,
        new: &str,
        state: impl FnOnce(&S) -> S,
    ) -> Option<Value> {
        if self.file.as_deref() != Some(old) {
            return None;
        }
        let next = state(self.sent.as_ref()?);
        self.sent = Some(next.clone());
        self.file = Some(new.to_owned());
        self.refresh(now);
        self.channel.set_local_state(now, next.encode())
    }

    /// 对端连接消失：摘除其条目并刷新。返回是否有条目被移除。
    pub fn drop_peer(&mut self, peer_id: &str) -> bool {
        if !self.channel.drop_peer(peer_id) {
            return false;
        }
        self.refresh(Instant::now());
        true
    }

    /// 断开 / 离开空间：清远端条目（本端状态保留，重连后由 [`resend`](Self::resend) 重发）。
    pub fn clear_remotes(&mut self) {
        self.channel.clear_remotes();
        self.remote.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use serde_json::json;

    /// 测试码器：文件 + 光标偏移 + 身份，形状与笔记通道一致。
    #[derive(Clone, PartialEq)]
    struct CursorState {
        file: String,
        caret: Option<u32>,
        identity: (String, String),
    }

    impl CursorState {
        fn new(file: &str, caret: Option<u32>) -> Self {
            CursorState {
                file: file.to_owned(),
                caret,
                identity: ("爱丽丝".to_owned(), "#e0533d".to_owned()),
            }
        }
    }

    #[derive(Clone, PartialEq, Debug)]
    struct RemoteCursor {
        peer_id: String,
        name: String,
        caret: Option<u32>,
    }

    impl PresenceState for CursorState {
        type Remote = RemoteCursor;

        fn file(&self) -> &str {
            &self.file
        }

        fn encode(&self) -> Value {
            json!({
                "file": self.file,
                "user": { "name": self.identity.0, "color": self.identity.1 },
                "caret": self.caret,
            })
        }

        fn decode(state: &Value, peer_id: &str) -> Option<Self::Remote> {
            let caret = state
                .get("caret")
                .and_then(Value::as_u64)
                .and_then(|v| u32::try_from(v).ok());
            Some(RemoteCursor {
                peer_id: peer_id.to_owned(),
                name: state
                    .get("user")
                    .and_then(|u| u.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                caret,
            })
        }
    }

    fn remote(client_id: u64, clock: u64, file: &str, caret: Option<u32>) -> Value {
        json!({
            "clientId": client_id,
            "clock": clock,
            "state": { "file": file, "user": { "name": "鲍勃", "color": "#3d7be0" }, "caret": caret },
        })
    }

    #[test]
    fn 换文件后旧文件帧被过滤() {
        let mut ch = PresenceChannel::new();
        let now = Instant::now();
        ch.enter(now, CursorState::new("a.md", None)).expect("首次应产帧");
        ch.apply("p1", &remote(9, 1, "a.md", Some(3)), now);
        assert_eq!(ch.remote().len(), 1, "同文件条目应可见");

        // 换文件落在限频窗口内：只记录不产帧，最新一份由 flush 补发
        assert!(ch.enter(now, CursorState::new("b.md", None)).is_none());
        assert!(ch.remote().is_empty(), "换文件清旧投影");
        ch.apply("p1", &remote(9, 2, "a.md", Some(4)), now + Duration::from_millis(60));
        assert!(ch.remote().is_empty(), "旧文件帧被过滤");
        assert!(!ch.refresh(now + Duration::from_millis(60)), "投影未变不报告变化");
        assert!(ch.has_peers(), "旧文件条目仍在通道里参与保活与剔除");

        ch.apply("p2", &remote(10, 1, "b.md", Some(5)), now + Duration::from_millis(120));
        let remote = ch.remote();
        assert_eq!(remote.len(), 1);
        assert_eq!(remote[0].peer_id, "p2");
        assert_eq!(remote[0].caret, Some(5));
    }

    #[test]
    fn 同载荷不产帧_变化才上行() {
        let mut ch = PresenceChannel::new();
        let now = Instant::now();
        let first = ch.enter(now, CursorState::new("a.md", Some(3))).expect("首次应产帧");
        assert_eq!(first["state"]["file"], "a.md");
        assert_eq!(first["state"]["caret"], 3);
        assert!(ch.update(now, CursorState::new("a.md", Some(3))).is_none());
        assert!(!ch.has_pending());

        let moved = ch
            .update(now + Duration::from_millis(60), CursorState::new("a.md", Some(7)))
            .expect("光标变化应产帧");
        assert_eq!(moved["state"]["caret"], 7);
        assert_eq!(ch.sent().map(|s| s.caret), Some(Some(7)));
    }

    #[test]
    fn 改名跟随_路径改写且选区保留() {
        let mut ch = PresenceChannel::new();
        let now = Instant::now();
        ch.enter(now, CursorState::new("notes/a.md", Some(3))).expect("进入");
        ch.apply("p1", &remote(9, 1, "notes/a.md", Some(1)), now);
        assert_eq!(ch.remote().len(), 1);

        let moved = ch
            .relocate(now + Duration::from_millis(60), "notes/a.md", "笔记/a.md", |sent| {
                CursorState { file: "笔记/a.md".to_owned(), ..sent.clone() }
            })
            .expect("改名应重发在场状态");
        assert_eq!(moved["state"]["file"], "笔记/a.md");
        assert_eq!(moved["state"]["caret"], 3, "选区偏移与路径无关");
        assert_eq!(ch.file(), Some("笔记/a.md"));
        assert!(ch.remote().is_empty(), "旧路径条目不属新视图");

        // 非本文件不改写
        assert!(ch
            .relocate(now + Duration::from_millis(120), "other.md", "x.md", |s| s.clone())
            .is_none());
        assert_eq!(ch.file(), Some("笔记/a.md"));
    }

    #[test]
    fn 限频窗口只记最新_flush到期补发() {
        let mut ch = PresenceChannel::new();
        let now = Instant::now();
        ch.enter(now, CursorState::new("a.md", Some(1))).expect("首帧");
        assert!(ch.update(now, CursorState::new("a.md", Some(2))).is_none(), "窗口内只记录");
        assert!(ch.has_pending());
        assert!(ch.flush(now + Duration::from_millis(10)).is_none(), "未到间隔不补发");
        let flushed = ch.flush(now + Duration::from_millis(50)).expect("到间隔补发最新一份");
        assert_eq!(flushed["state"]["caret"], 2);
        assert!(!ch.has_pending());
        assert!(ch.flush(now + Duration::from_secs(1)).is_none(), "无待发变化");
    }

    #[test]
    fn 保活按间隔重发() {
        let mut ch = PresenceChannel::new();
        let now = Instant::now();
        ch.enter(now, CursorState::new("a.md", Some(1))).expect("首帧");
        assert!(ch.keepalive_if_due(now + Duration::from_secs(14)).is_none());
        let keep = ch.keepalive_if_due(now + Duration::from_secs(15)).expect("到期应重发");
        assert_eq!(keep["state"]["caret"], 1);
        assert!(ch.keepalive_if_due(now + Duration::from_secs(16)).is_none(), "刚重发过");
    }

    #[test]
    fn 远端只收同文件条目_超时消失() {
        let mut ch = PresenceChannel::new();
        let now = Instant::now();
        ch.enter(now, CursorState::new("a.md", None)).expect("进入");
        ch.apply("other", &remote(9, 1, "b.md", Some(1)), now);
        assert!(ch.remote().is_empty(), "别的文件不画");
        assert!(ch.has_peers(), "条目仍在通道里参与保活与剔除");

        ch.apply("p1", &remote(10, 1, "a.md", Some(2)), now);
        assert_eq!(ch.remote().len(), 1);
        ch.apply("p1", &remote(10, 2, "a.md", Some(4)), now + Duration::from_millis(60));
        assert_eq!(ch.remote()[0].caret, Some(4));

        // 恰好到期的一刻仍算在线（闭区间），超过才剔除
        let at_expiry = now + Duration::from_millis(60) + Duration::from_secs(30);
        assert!(!ch.refresh(at_expiry), "未变不报告变化");
        assert!(ch.refresh(at_expiry + Duration::from_millis(1)), "超时剔除应报告变化");
        assert!(ch.remote().is_empty(), "30s 无更新应剔除");

        let resent = ch.resend(now).expect("在场应可重发");
        assert_eq!(resent["state"]["file"], "a.md");
    }

    #[test]
    fn 连接消失摘除条目_断链清远端不动本端() {
        let mut ch = PresenceChannel::new();
        let now = Instant::now();
        ch.enter(now, CursorState::new("a.md", Some(1))).expect("进入");
        ch.apply("p1", &remote(9, 1, "a.md", None), now);
        assert!(ch.drop_peer("p1"));
        assert!(!ch.drop_peer("p1"));
        assert!(ch.remote().is_empty());

        ch.apply("p1", &remote(9, 2, "a.md", None), now);
        assert_eq!(ch.remote().len(), 1);
        ch.clear_remotes();
        assert!(ch.remote().is_empty());
        assert!(!ch.has_peers());
        assert_eq!(ch.file(), Some("a.md"), "本端状态保留");
        assert!(ch.resend(now).is_some(), "重连应可重发本端状态");
    }
}
