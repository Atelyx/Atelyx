//! 画布协作在场：本机选中 / 对话卡锁声明 / 流式卡经通用瞬时通道上行，远端条目解析为
//! 画布绘制与锁判定视图。
//!
//! 通道载荷只带节点 id 与身份色，不含卡内容；对端删除节点后选中 / 锁声明自然失配
//! （按现存节点过滤），不画即可，无需额外同步。

use std::collections::HashMap;
use std::time::Instant;

use atelyx_core::ephemeral::EphemeralChannel;
use serde_json::{json, Value};

use crate::ui::palette as pal;

/// 宿主内置通道名（插件通道为 `<插件id>:<频道>`，两套命名空间互不覆盖）。
pub const CHANNEL: &str = "host.canvas";

/// 本端身份（名签与成员色由壳提供；通道层不感知）。
pub struct Identity<'a> {
    pub name: &'a str,
    pub color: &'a str,
}

/// 远端协作者在当前画布上的在场条目（只收同文件条目；身份色畸形回落灰）。
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

/// 本端已写入通道状态的在场内容（变化检测基准，含被限频压下待补发的那份）。
#[derive(Clone, PartialEq)]
pub struct SentState {
    pub selection: Vec<String>,
    pub locked: Vec<(String, i64)>,
    pub streaming: Vec<String>,
    pub identity: (String, String),
}

pub struct CanvasCollab {
    channel: EphemeralChannel,
    /// 本端已挂的文件（None = 不在画布）。
    file: Option<String>,
    sent: Option<SentState>,
    /// 当前文件可绘制的远端条目（事件与 tick 驱动刷新，绘制只读）。
    remote: Vec<RemotePeer>,
}

impl CanvasCollab {
    pub fn new() -> Self {
        CanvasCollab { channel: EphemeralChannel::new(), file: None, sent: None, remote: Vec::new() }
    }

    /// 本端挂着的文件（None = 未在场）。
    pub fn file(&self) -> Option<&str> {
        self.file.as_deref()
    }

    /// 已提交到通道状态的在场内容与身份（调用方据此跳过无变化的同步）。
    pub fn sent(&self) -> Option<&SentState> {
        self.sent.as_ref()
    }

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
        let Some(sent) = &self.sent else { return false };
        if self.file.as_deref() != Some(file)
            || sent.identity.0 != identity.0
            || sent.identity.1 != identity.1
            || sent.selection.as_slice() != selection
        {
            return false;
        }
        if sent.locked.len() != locked.len()
            || sent
                .locked
                .iter()
                .any(|(id, since)| locked.get(id) != Some(since))
        {
            return false;
        }
        sent.streaming.len() == streaming_ids.len()
            && sent.streaming.iter().all(|id| streaming_ids.contains(&id.as_str()))
    }

    pub fn remote(&self) -> &[RemotePeer] {
        &self.remote
    }

    /// 进入 / 切换文件：立即构造本端状态（选区为空也上报，供对端显示「在此画布」）。
    pub fn enter(
        &mut self,
        now: Instant,
        identity: &Identity<'_>,
        file: &str,
        selection: Vec<String>,
        locked: Vec<(String, i64)>,
        streaming: Vec<String>,
    ) -> Option<Value> {
        self.file = Some(file.to_owned());
        self.sent = Some(SentState {
            selection: selection.clone(),
            locked: locked.clone(),
            streaming: streaming.clone(),
            identity: identity_pair(identity),
        });
        // 换文件后旧文件的远端条目不再属于本视图
        self.remote.clear();
        self.channel
            .set_local_state(now, state_json(identity, file, &selection, &locked, &streaming))
    }

    /// 在场变化上报（同文件且内容未变 = None；限频压下的变化由 [`flush`](Self::flush) 补发）。
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
        if self.file.as_deref() != Some(file) {
            return self.enter(now, identity, file, selection, locked, streaming);
        }
        let next = SentState {
            selection,
            locked,
            streaming,
            identity: identity_pair(identity),
        };
        if self.sent.as_ref() == Some(&next) {
            return None;
        }
        self.sent = Some(next.clone());
        self.channel.set_local_state(
            now,
            state_json(identity, file, &next.selection, &next.locked, &next.streaming),
        )
    }

    /// 离场（关闭画布 / 离开空间）：产出 null 条目并清本端记录。
    pub fn leave(&mut self, now: Instant) -> Option<Value> {
        self.file = None;
        self.sent = None;
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

    /// 通道里是否有任何远端条目：有对端才需要周期拍做剔除与保活。
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
                let name = state
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let (color, color_hex) = match state.get("color").and_then(Value::as_str) {
                    Some(raw) => match crate::layout_view::parse_hex_color(raw) {
                        Some(color) => (color, raw.to_string()),
                        None => (pal::FALLBACK_COLLAB, pal::FALLBACK_COLLAB_COLOR.to_string()),
                    },
                    None => (pal::FALLBACK_COLLAB, pal::FALLBACK_COLLAB_COLOR.to_string()),
                };
                Some(RemotePeer {
                    peer_id: entry.peer_id.clone(),
                    name,
                    color,
                    color_hex,
                    selection: string_list(state.get("selection")),
                    locked: lock_list(state.get("locked")),
                    streaming: string_list(state.get("streaming")),
                })
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

impl Default for CanvasCollab {
    fn default() -> Self {
        Self::new()
    }
}

fn identity_pair(identity: &Identity<'_>) -> (String, String) {
    (identity.name.to_owned(), identity.color.to_owned())
}

fn state_json(
    identity: &Identity<'_>,
    file: &str,
    selection: &[String],
    locked: &[(String, i64)],
    streaming: &[String],
) -> Value {
    json!({
        "file": file,
        "name": identity.name,
        "color": identity.color,
        "selection": selection,
        "locked": locked
            .iter()
            .map(|(id, since)| json!({ "id": id, "since": since }))
            .collect::<Vec<_>>(),
        "streaming": streaming,
    })
}

fn string_list(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn lock_list(v: Option<&Value>) -> Vec<(String, i64)> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|e| {
                    let id = e.get("id").and_then(Value::as_str)?.to_owned();
                    let since = e.get("since").and_then(Value::as_i64)?;
                    Some((id, since))
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn identity() -> Identity<'static> {
        Identity { name: "爱丽丝", color: "#e0533d" }
    }

    fn remote_payload(
        client_id: u64,
        clock: u64,
        file: &str,
        selection: Value,
        locked: Value,
        streaming: Value,
    ) -> Value {
        json!({
            "clientId": client_id,
            "clock": clock,
            "state": {
                "file": file, "name": "鲍勃", "color": "#3d7be0",
                "selection": selection, "locked": locked, "streaming": streaming,
            },
        })
    }

    #[test]
    fn 进入后上报含全部在场内容_同内容不再上行() {
        let mut collab = CanvasCollab::new();
        let now = Instant::now();
        let first = collab
            .enter(now, &identity(), "a.atlx", vec!["n1".into()], vec![("n2".into(), 5)], vec!["n2".into()])
            .expect("首次应产帧");
        assert_eq!(first["state"]["file"], "a.atlx");
        assert_eq!(first["state"]["selection"], json!(["n1"]));
        assert_eq!(first["state"]["locked"], json!([{ "id": "n2", "since": 5 }]));
        assert_eq!(first["state"]["streaming"], json!(["n2"]));
        assert_eq!(collab.file(), Some("a.atlx"));
        assert!(collab
            .update(now, &identity(), "a.atlx", vec!["n1".into()], vec![("n2".into(), 5)], vec!["n2".into()])
            .is_none());
    }

    #[test]
    fn 锁与流式变化要上行_选区清空也要上行() {
        let mut collab = CanvasCollab::new();
        let now = Instant::now();
        collab
            .enter(now, &identity(), "a.atlx", vec!["n1".into()], Vec::new(), Vec::new())
            .expect("进入");
        let frame = collab
            .update(now + Duration::from_millis(60), &identity(), "a.atlx", Vec::new(), vec![("n1".into(), 7)], Vec::new())
            .expect("占锁应对端可见");
        assert_eq!(frame["state"]["locked"], json!([{ "id": "n1", "since": 7 }]));
        let frame = collab
            .update(now + Duration::from_millis(120), &identity(), "a.atlx", Vec::new(), Vec::new(), vec!["n1".into()])
            .expect("流式应对端可见");
        assert_eq!(frame["state"]["streaming"], json!(["n1"]));
        let frame = collab
            .update(now + Duration::from_millis(180), &identity(), "a.atlx", Vec::new(), Vec::new(), Vec::new())
            .expect("流式结束应对端可见");
        assert_eq!(frame["state"]["streaming"], json!([]));
        assert!(collab
            .update(now + Duration::from_millis(240), &identity(), "a.atlx", Vec::new(), Vec::new(), Vec::new())
            .is_none());
    }

    #[test]
    fn 换文件清远端_离场产null() {
        let mut collab = CanvasCollab::new();
        let now = Instant::now();
        collab
            .enter(now, &identity(), "a.atlx", Vec::new(), Vec::new(), Vec::new())
            .expect("进入");
        collab.apply("p1", &remote_payload(9, 1, "a.atlx", json!(["n1"]), json!([]), json!([])), now);
        assert_eq!(collab.remote().len(), 1, "同文件条目应可见");

        assert!(collab
            .enter(now, &identity(), "b.atlx", Vec::new(), Vec::new(), Vec::new())
            .is_none());
        assert!(collab.remote().is_empty(), "换文件后不保留旧画布条目");
        let swapped = collab
            .flush(now + Duration::from_millis(60))
            .expect("到期应补发新文件状态");
        assert_eq!(swapped["state"]["file"], "b.atlx");

        let leave = collab.leave(now).expect("在场应产离场帧");
        assert!(leave["state"].is_null());
        assert!(collab.leave(now).is_none());
    }

    #[test]
    fn 远端只收同文件条目_锁与流式可查询() {
        let mut collab = CanvasCollab::new();
        let now = Instant::now();
        collab
            .enter(now, &identity(), "a.atlx", Vec::new(), Vec::new(), Vec::new())
            .expect("进入");

        collab.apply("p1", &remote_payload(9, 1, "b.atlx", json!(["x"]), json!([]), json!([])), now);
        assert!(collab.remote().is_empty(), "别的文件不收");

        collab.apply(
            "p1",
            &remote_payload(9, 2, "a.atlx", json!(["n1", "n2"]), json!([{ "id": "n1", "since": 9 }]), json!(["n2"])),
            now + Duration::from_millis(60),
        );
        let remote = collab.remote();
        assert_eq!(remote.len(), 1);
        assert_eq!(remote[0].name, "鲍勃");
        assert_eq!(remote[0].selection, vec!["n1".to_string(), "n2".to_string()]);
        assert_eq!(remote[0].locks("n1"), Some(9));
        assert_eq!(remote[0].locks("n9"), None);
        assert!(remote[0].is_streaming("n2"));
        assert!(!remote[0].is_streaming("n1"));
    }

    #[test]
    fn 畸形条目降级_对端断链摘除() {
        let mut collab = CanvasCollab::new();
        let now = Instant::now();
        collab
            .enter(now, &identity(), "a.atlx", Vec::new(), Vec::new(), Vec::new())
            .expect("进入");
        let bad = json!({
            "clientId": 7, "clock": 1,
            "state": { "file": "a.atlx", "name": "鲍勃", "color": "not-a-color",
                       "selection": ["n1"], "locked": [{ "id": "n1" }, { "id": "n2", "since": "x" }], "streaming": [3] },
        });
        collab.apply("p1", &bad, now);
        let remote = collab.remote();
        assert_eq!(remote.len(), 1, "畸形字段降级，条目仍可见");
        assert_eq!(remote[0].color, pal::FALLBACK_COLLAB);
        assert!(remote[0].locked.is_empty(), "缺 since / 非数字 since 的锁声明丢弃");
        assert!(remote[0].streaming.is_empty(), "非字符串流式 id 丢弃");

        assert!(collab.drop_peer("p1"));
        assert!(collab.remote().is_empty());
    }

    #[test]
    fn 远端超时剔除_重连重发() {
        let mut collab = CanvasCollab::new();
        let now = Instant::now();
        collab
            .enter(now, &identity(), "a.atlx", Vec::new(), Vec::new(), Vec::new())
            .expect("进入");
        collab.apply("p1", &remote_payload(9, 1, "a.atlx", json!(["n1"]), json!([]), json!([])), now);
        assert_eq!(collab.remote().len(), 1);
        collab.refresh(now + Duration::from_secs(31));
        assert!(collab.remote().is_empty(), "30s 无更新应剔除");

        let resent = collab.resend(now).expect("在场应可重发");
        assert_eq!(resent["state"]["file"], "a.atlx");
    }
}
