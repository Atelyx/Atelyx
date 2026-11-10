//! AI 协作者在场：本端 AI 编辑状态经 host.ai 瞬时通道上行，远端解析为工具条胶囊。
//! 通道载荷对服务端不透明（与插件通道同构），AI 不进 peers 名册、不建独立连接。

use std::time::{Duration, Instant};

use atelyx_core::ai::edit_host::{AiIdentity, EditActor};
use atelyx_core::ephemeral::EphemeralChannel;
use serde_json::{json, Value};

/// 宿主内置通道名（与插件通道命名空间互不覆盖）。
pub const CHANNEL: &str = "host.ai";

/// 远端 AI 条目（工具条胶囊视图）。
#[derive(Clone)]
pub struct RemoteAi {
    pub agent_name: String,
    pub operator_name: String,
    pub view: String,
    pub file: Option<String>,
    /// 正在编辑该文件（false = 刚刚修改过）。
    pub editing: bool,
    /// 最近一次编辑时刻（毫秒；无 = 0）。
    pub last_edit_ts: i64,
    /// 最近一次编辑摘要（悬停展示；无 = 空）。
    pub last_edit_summary: Option<String>,
}

/// 远端「刚刚修改」的展示窗口：编辑结束后条目要在对端留这么久（协作者胶囊同档）。
/// 通道超时即保活上限，所以取展示窗口本身，而不是通道缺省的 30 秒。
const IDLE_WINDOW: Duration = Duration::from_secs(60);

/// 本端 AI 在场状态机（瞬时通道封装）。
pub struct AiPresence {
    channel: EphemeralChannel,
    view: String,
    file: Option<String>,
    agent: Option<AiIdentity>,
    summary: Option<String>,
    editing: bool,
}

impl Default for AiPresence {
    fn default() -> Self {
        Self::new()
    }
}

impl AiPresence {
    pub fn new() -> Self {
        AiPresence {
            // 编辑结束后的「刚刚修改」要在对端留满展示窗口：通道超时按窗口取；
            // 空闲态不保活，条目到期由对端剔除
            channel: EphemeralChannel::with_timeout(IDLE_WINDOW),
            view: String::new(),
            file: None,
            agent: None,
            summary: None,
            editing: false,
        }
    }

    /// 本端是否处于编辑态（壳据此挂周期拍做保活）。
    pub fn is_editing(&self) -> bool {
        self.editing
    }

    /// 限频压下的本端状态补发（壳 tick 调用）。
    pub fn flush(&mut self, now: Instant) -> Option<Value> {
        self.channel.flush(now)
    }

    /// 到期保活：只在编辑态续期（编辑久于保活间隔时对端不能按超时剔除）。
    /// 空闲态的「刚刚修改」不续期——续期会让胶囊在对端永不消失，展示窗口由通道超时收口。
    pub fn keepalive(&mut self, now: Instant) -> Option<Value> {
        if !self.editing {
            return None;
        }
        self.channel.keepalive_if_due(now)
    }

    /// 重连后重发当前状态（对端旧条目会在超时后消失，重发即恢复）。
    pub fn resend(&mut self, now: Instant) -> Option<Value> {
        self.channel.resend(now)
    }

    /// 是否有被限频压下的状态待补发（壳据此挂周期拍）。
    pub fn has_pending(&self) -> bool {
        self.channel.has_pending()
    }

    /// 开始编辑：上报 editing 状态（同状态重复调用不产帧）。
    pub fn begin(
        &mut self,
        now: Instant,
        view: &str,
        file: &str,
        actor: &EditActor,
        summary: &str,
        operator: &(String, String),
    ) -> Option<Value> {
        self.view = view.to_string();
        self.file = Some(file.to_string());
        self.agent = Some(actor.agent.clone());
        self.summary = Some(summary.to_string());
        self.editing = true;
        let state = self.state_json(operator, None);
        self.channel.set_local_state(now, state)
    }

    /// 编辑结束：上报 idle + lastEdit（对端在条目存活期内展示「刚刚修改」，超时后自然消失）。
    pub fn idle(&mut self, now: Instant, operator: &(String, String)) -> Option<Value> {
        if !self.editing {
            return None;
        }
        self.editing = false;
        let ts = now_ms();
        let summary = self.summary.clone();
        let state = self.state_json(operator, Some((ts, summary)));
        self.channel.set_local_state(now, state)
    }

    /// 离场：发 null 条目并清本端状态。
    pub fn leave(&mut self, now: Instant) -> Option<Value> {
        self.editing = false;
        self.channel.leave(now)
    }

    /// 合入远端帧；返回远端集合是否变化。
    pub fn apply(&mut self, peer_id: &str, payload: &Value, now: Instant) -> bool {
        self.channel.apply_remote(peer_id, payload, now)
    }

    /// 远端条目快照（超时条目在通道内剔除）。
    pub fn remotes(&mut self, now: Instant) -> Vec<RemoteAi> {
        self.channel
            .remotes(now)
            .into_iter()
            .filter_map(|remote| {
                let state = &remote.state;
                let agent_name = state
                    .get("agent")
                    .and_then(|a| a.get("name"))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())?;
                let editing = state.get("action").and_then(Value::as_str) == Some("editing");
                Some(RemoteAi {
                    agent_name: agent_name.to_string(),
                    operator_name: state
                        .get("operator")
                        .and_then(|o| o.get("name"))
                        .and_then(Value::as_str)
                        .unwrap_or("用户")
                        .to_string(),
                    view: state.get("view").and_then(Value::as_str).unwrap_or("").to_string(),
                    file: state.get("file").and_then(Value::as_str).map(str::to_string),
                    editing,
                    last_edit_ts: state
                        .get("lastEdit")
                        .and_then(|e| e.get("ts"))
                        .and_then(Value::as_i64)
                        .unwrap_or(0),
                    last_edit_summary: state
                        .get("lastEdit")
                        .and_then(|e| e.get("summary"))
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string),
                })
            })
            .collect()
    }

    fn state_json(
        &self,
        operator: &(String, String),
        last_edit: Option<(i64, Option<String>)>,
    ) -> Value {
        let agent = self.agent.clone().unwrap_or(AiIdentity {
            agent_id: String::new(),
            agent_name: String::new(),
        });
        json!({
            "view": self.view,
            "file": self.file,
            "action": if self.editing { "editing" } else { "idle" },
            "agent": { "id": agent.agent_id, "name": agent.agent_name },
            "operator": { "name": operator.0, "color": operator.1 },
            "lastEdit": last_edit.map(|(ts, summary)| json!({ "ts": ts, "summary": summary })),
        })
    }
}

/// 当前时刻（毫秒）。
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actor() -> EditActor {
        EditActor {
            author: atelyx_core::history::HistoryAuthor {
                id: "u".into(),
                name: "甲".into(),
                device: "d".into(),
            },
            agent: AiIdentity { agent_id: "builtin-agent".into(), agent_name: "Agent".into() },
        }
    }

    fn identity() -> (String, String) {
        ("甲".to_string(), "#3b82f6".to_string())
    }

    #[test]
    fn begin_publishes_editing_state() {
        let mut presence = AiPresence::new();
        let state = presence
            .begin(Instant::now(), "note", "a.md", &actor(), "已编辑 a.md", &identity())
            .expect("首次上报必产帧");
        assert_eq!(state["state"]["action"], "editing");
        assert_eq!(state["state"]["file"], "a.md");
        assert_eq!(state["state"]["agent"]["name"], "Agent");
        assert_eq!(state["state"]["operator"]["name"], "甲");
        assert!(presence.is_editing());
    }

    #[test]
    fn idle_state_is_flushed_after_throttle_window() {
        let mut presence = AiPresence::new();
        let now = Instant::now();
        presence
            .begin(now, "note", "a.md", &actor(), "已编辑 a.md", &identity())
            .expect("首次上报必产帧");
        // 50ms 限频窗口内：idle 被压下，标记待补发（壳 tick 到点补发）
        assert!(presence.idle(Instant::now(), &identity()).is_none());
        assert!(presence.has_pending());
        std::thread::sleep(std::time::Duration::from_millis(60));
        let state = presence.flush(Instant::now()).expect("到窗口后补发");
        assert_eq!(state["state"]["action"], "idle");
        assert!(state["state"]["lastEdit"]["ts"].as_i64().unwrap() > 0);
        assert!(!presence.has_pending());
        // 重复 idle 不产帧
        assert!(presence.idle(Instant::now(), &identity()).is_none());
    }

    #[test]
    fn remote_state_decodes_agent_and_last_edit() {
        let mut presence = AiPresence::new();
        let now = Instant::now();
        let payload = serde_json::json!({
            "clientId": 7u64,
            "clock": 1u64,
            "state": {
                "view": "note",
                "file": "a.md",
                "action": "idle",
                "agent": { "id": "builtin-agent", "name": "Agent" },
                "operator": { "name": "甲", "color": "#3b82f6" },
                "lastEdit": { "ts": 123, "summary": "已编辑 a.md" }
            }
        });
        assert!(presence.apply("peer-1", &payload, now));
        let remotes = presence.remotes(now);
        assert_eq!(remotes.len(), 1);
        assert_eq!(remotes[0].agent_name, "Agent");
        assert_eq!(remotes[0].operator_name, "甲");
        assert_eq!(remotes[0].file.as_deref(), Some("a.md"));
        assert!(!remotes[0].editing);
        assert_eq!(remotes[0].last_edit_ts, 123);
        assert_eq!(remotes[0].last_edit_summary.as_deref(), Some("已编辑 a.md"));
    }

    #[test]
    fn idle_entry_survives_the_show_window() {
        let mut presence = AiPresence::new();
        let now = Instant::now();
        let payload = serde_json::json!({
            "clientId": 7u64,
            "clock": 1u64,
            "state": {
                "view": "note",
                "file": "a.md",
                "action": "idle",
                "agent": { "id": "builtin-agent", "name": "Agent" },
                "operator": { "name": "甲", "color": "#3b82f6" },
                "lastEdit": { "ts": 123, "summary": "已编辑 a.md" }
            }
        });
        assert!(presence.apply("peer-1", &payload, now));
        // 通道缺省超时 30 秒；「刚刚修改」按展示窗口（60 秒）保留
        assert_eq!(presence.remotes(now + Duration::from_secs(45)).len(), 1);
        assert!(presence
            .remotes(now + IDLE_WINDOW + Duration::from_millis(1))
            .is_empty());
    }

    #[test]
    fn leave_publishes_null_state_once() {
        let mut presence = AiPresence::new();
        let now = Instant::now();
        presence
            .begin(now, "note", "a.md", &actor(), "x", &identity())
            .expect("首次上报必产帧");
        let left = presence.leave(now).expect("在场中离场必产帧");
        assert!(left["state"].is_null());
        assert!(presence.leave(now).is_none());
        assert!(!presence.is_editing());
    }
}
