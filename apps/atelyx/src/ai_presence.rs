//! AI 协作者在场：本端 AI 编辑状态经 host.ai 瞬时通道上行，远端解析为工具条胶囊。
//! 通道载荷对服务端不透明（与插件通道同构），AI 不进 peers 名册、不建独立连接。

use std::time::Instant;

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
}

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
            channel: EphemeralChannel::new(),
            view: String::new(),
            file: None,
            agent: None,
            summary: None,
            editing: false,
        }
    }

    pub fn is_editing(&self) -> bool {
        self.editing
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

    /// 编辑结束：上报 idle + lastEdit（60s 内的对端展示「刚刚修改」）。
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
