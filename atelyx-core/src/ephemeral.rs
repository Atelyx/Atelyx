//! 协作瞬时通道：按通道名维护本端在场状态与远端条目（限频 / 保活 / 超时剔除）。
//!
//! 载荷不透明：本层只搬 `state` 值，不解析业务字段；`state` 为 null 表示该端离场。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

/// 本端状态上行的最小间隔：拖选 / 拖动等连续变化按此合并，最新一份由 [`EphemeralChannel::flush`] 补发。
const MIN_INTERVAL: Duration = Duration::from_millis(50);
/// 保活重发间隔：空闲端不重发身份帧就会被对端按 [`TIMEOUT`] 剔除（互见消失）。
const KEEPALIVE: Duration = Duration::from_secs(15);
/// 远端条目超时：期间无更新视为离线剔除。
const TIMEOUT: Duration = Duration::from_secs(30);

/// clientID 计数器：同一进程内多个通道实例的 id 不撞车。
static NEXT_CLIENT: AtomicU64 = AtomicU64::new(1);

/// 远端条目（壳层视图）。
pub struct RemoteState {
    pub client_id: u64,
    pub state: Value,
}

struct Entry {
    clock: u64,
    state: Value,
    seen: Instant,
    /// 上报该条目的连接（名册剔除该连接时一并移除，对端退出即时生效）。
    peer_id: String,
}

/// 单通道的在场状态机（通道名由调用方给定，业务字段全在 `state` 里）。
pub struct EphemeralChannel {
    client_id: u64,
    local_state: Option<Value>,
    /// 已上行的本端状态：与 `local_state` 不同 = 有被限频压下的变化待补发。
    local_sent: Option<Value>,
    clock: u64,
    sent_at: Option<Instant>,
    remotes: HashMap<u64, Entry>,
    /// 各远端 clientID 见过的最大 clock：条目移除后仍保留水位，
    /// 否则乱序到达的旧帧会把已离场的端重新插回来。
    clocks: HashMap<u64, u64>,
}

impl Default for EphemeralChannel {
    fn default() -> Self {
        Self::new()
    }
}

impl EphemeralChannel {
    pub fn new() -> Self {
        EphemeralChannel {
            client_id: new_client_id(),
            local_state: None,
            local_sent: None,
            clock: 0,
            sent_at: None,
            remotes: HashMap::new(),
            clocks: HashMap::new(),
        }
    }

    /// 本端状态：与上次相同返回 None。距上次上行不足 [`MIN_INTERVAL`] 的变化只记录不产帧，
    /// 最新一份由 [`flush`](Self::flush) 补发。
    pub fn set_local_state(&mut self, now: Instant, state: Value) -> Option<Value> {
        if self.local_state.as_ref() == Some(&state) {
            return None;
        }
        self.local_state = Some(state);
        if self
            .sent_at
            .is_some_and(|at| now.duration_since(at) < MIN_INTERVAL)
        {
            return None;
        }
        self.emit(now, self.local_state.clone()?)
    }

    /// 补发被限频压下的本端状态：无待发变化或未到间隔 = None。
    pub fn flush(&mut self, now: Instant) -> Option<Value> {
        if self.local_state.is_none() || self.local_state == self.local_sent {
            return None;
        }
        if self
            .sent_at
            .is_some_and(|at| now.duration_since(at) < MIN_INTERVAL)
        {
            return None;
        }
        self.emit(now, self.local_state.clone()?)
    }

    /// 是否有被限频压下、尚未上行的本端状态（壳据此保证补发拍不缺席）。
    pub fn has_pending(&self) -> bool {
        self.local_state.is_some() && self.local_state != self.local_sent
    }

    /// 到期保活：重发当前状态（未在场 / 未到间隔 = None）。
    pub fn keepalive_if_due(&mut self, now: Instant) -> Option<Value> {
        if !self
            .sent_at
            .is_none_or(|at| now.duration_since(at) >= KEEPALIVE)
        {
            return None;
        }
        self.resend(now)
    }

    /// 立即重发（重连后对端旧条目将在 [`TIMEOUT`] 过期，用当前状态覆盖）。
    pub fn resend(&mut self, now: Instant) -> Option<Value> {
        let state = self.local_state.clone()?;
        self.emit(now, state)
    }

    /// 离场：产出 null 条目并清本端状态（未在场 = None）。clock 继续递增，跨在场轮次不复位。
    pub fn leave(&mut self, now: Instant) -> Option<Value> {
        self.local_state.take()?;
        self.emit(now, Value::Null)
    }

    /// 合入远端帧：更高 clock 覆盖、同 clock 视为保活重发照常刷新、更小 clock 丢弃；
    /// `state` 为 null 移除该端（同样受 clock 门槛约束：旧离场帧不得删掉新状态）。
    /// 缺 `clientId` / `clock` / `state` 字段视为畸形载荷整体丢弃。本端 clientID 跳过（回声）。
    /// 返回远端集合是否变化（壳据此重绘）。
    pub fn apply_remote(&mut self, peer_id: &str, payload: &Value, now: Instant) -> bool {
        let Some(client_id) = payload.get("clientId").and_then(Value::as_u64) else {
            return false;
        };
        if client_id == self.client_id {
            return false;
        }
        let Some(clock) = payload.get("clock").and_then(Value::as_u64) else {
            return false;
        };
        let Some(state) = payload.get("state").cloned() else {
            return false;
        };
        if self.clocks.get(&client_id).is_some_and(|seen| clock < *seen) {
            return false;
        }
        self.clocks.insert(client_id, clock);
        if state.is_null() {
            return self.remotes.remove(&client_id).is_some();
        }
        match self.remotes.get_mut(&client_id) {
            Some(entry) => {
                entry.clock = clock;
                entry.state = state;
                entry.seen = now;
                entry.peer_id = peer_id.to_owned();
                true
            }
            None => {
                self.remotes.insert(
                    client_id,
                    Entry { clock, state, seen: now, peer_id: peer_id.to_owned() },
                );
                true
            }
        }
    }

    /// 远端条目（按 clientID 排序）；超时未刷新的先剔除。
    pub fn remotes(&mut self, now: Instant) -> Vec<RemoteState> {
        self.remotes.retain(|_, entry| now.duration_since(entry.seen) <= TIMEOUT);
        let mut list: Vec<(u64, RemoteState)> = self
            .remotes
            .iter()
            .map(|(id, entry)| {
                (*id, RemoteState { client_id: *id, state: entry.state.clone() })
            })
            .collect();
        list.sort_by_key(|(id, _)| *id);
        list.into_iter().map(|(_, remote)| remote).collect()
    }

    /// 对端连接消失：移除其上报过的全部条目。返回是否有条目被移除。
    pub fn drop_peer(&mut self, peer_id: &str) -> bool {
        let before = self.remotes.len();
        self.remotes.retain(|_, entry| entry.peer_id != peer_id);
        self.remotes.len() != before
    }

    /// 离开房间 / 断链：清远端条目（本端状态保留，重连后由 [`resend`](Self::resend) 重发）。
    /// clock 水位一并保留：重连后迟到的旧帧不得把已离场的端插回来。
    pub fn clear_remotes(&mut self) {
        self.remotes.clear();
    }

    /// 是否有远端条目（壳据此决定要不要为保活/剔除挂周期拍）。
    pub fn has_remotes(&self) -> bool {
        !self.remotes.is_empty()
    }

    fn emit(&mut self, now: Instant, state: Value) -> Option<Value> {
        self.clock += 1;
        self.sent_at = Some(now);
        self.local_sent = Some(state.clone());
        Some(json!({
            "clientId": self.client_id,
            "clock": self.clock,
            "state": state,
        }))
    }
}

/// 53 位非零 clientID（与 Yjs clientID 同量级，便于将来跨端对照；同进程内不重复）。
fn new_client_id() -> u64 {
    let seq = NEXT_CLIENT.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let id = (nanos.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ seq) & ((1 << 53) - 1);
    if id == 0 {
        1
    } else {
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(clock: u64, state: Value) -> Value {
        json!({ "clientId": 9, "clock": clock, "state": state })
    }

    #[test]
    fn 首次产帧_同值不产帧_clock递增() {
        let mut ch = EphemeralChannel::new();
        let now = Instant::now();
        let first = ch.set_local_state(now, json!({ "v": 1 })).expect("首次应产帧");
        assert_eq!(first["clock"], 1);
        assert_eq!(first["state"], json!({ "v": 1 }));
        assert_eq!(first["clientId"], ch.client_id);
        assert!(ch.set_local_state(now, json!({ "v": 1 })).is_none());
        let changed = ch
            .set_local_state(now + MIN_INTERVAL, json!({ "v": 2 }))
            .expect("变化应产帧");
        assert_eq!(changed["clock"], 2);
    }

    #[test]
    fn 限频窗口只记最新_flush到期补发() {
        let mut ch = EphemeralChannel::new();
        let now = Instant::now();
        ch.set_local_state(now, json!({ "v": 1 })).expect("首帧");
        assert!(ch.set_local_state(now, json!({ "v": 2 })).is_none());
        assert!(ch.has_pending());
        assert!(ch.flush(now + Duration::from_millis(10)).is_none(), "未到间隔不补发");
        let flushed = ch
            .flush(now + MIN_INTERVAL)
            .expect("到间隔补发最新一份");
        assert_eq!(flushed["state"], json!({ "v": 2 }));
        assert!(!ch.has_pending());
        assert!(ch.flush(now + Duration::from_secs(1)).is_none(), "无待发变化");
    }

    #[test]
    fn 保活按间隔重发_离场清本端() {
        let mut ch = EphemeralChannel::new();
        let now = Instant::now();
        ch.set_local_state(now, json!({ "v": 1 })).expect("首帧");
        assert!(ch.keepalive_if_due(now + Duration::from_secs(14)).is_none());
        let keep = ch
            .keepalive_if_due(now + KEEPALIVE)
            .expect("到期应重发");
        assert_eq!(keep["state"], json!({ "v": 1 }));
        assert!(ch.local_state.is_some());

        let leave = ch.leave(now + KEEPALIVE).expect("在场应产离场帧");
        assert!(leave["state"].is_null());
        assert!(ch.local_state.is_none());
        assert!(ch.leave(now + KEEPALIVE).is_none());
        assert!(ch.keepalive_if_due(now + Duration::from_secs(60)).is_none());
    }

    #[test]
    fn 远端更高clock覆盖_同clock保活_更小丢弃_本端回声跳过() {
        let mut ch = EphemeralChannel::new();
        let now = Instant::now();
        assert!(ch.apply_remote("p1", &payload(1, json!({ "v": 1 })), now));
        assert_eq!(ch.remotes(now).len(), 1);

        assert!(ch.apply_remote("p1", &payload(2, json!({ "v": 2 })), now));
        assert_eq!(ch.remotes(now)[0].state, json!({ "v": 2 }));

        assert!(!ch.apply_remote("p1", &payload(1, json!({ "v": 1 })), now), "旧 clock 丢弃");
        assert!(ch.apply_remote("p1", &payload(2, json!({ "v": 2 })), now), "同 clock 视为保活");

        let echo = json!({ "clientId": ch.client_id, "clock": 99, "state": json!({}) });
        assert!(!ch.apply_remote("p1", &echo, now), "本端回声跳过");
    }

    #[test]
    fn 远端null移除_超时剔除_按连接摘除() {
        let mut ch = EphemeralChannel::new();
        let now = Instant::now();
        ch.apply_remote("p1", &payload(1, json!({ "v": 1 })), now);
        let second = json!({ "clientId": 10, "clock": 1, "state": json!({ "v": 1 }) });
        ch.apply_remote("p2", &second, now);
        assert_eq!(ch.remotes(now).len(), 2);

        assert!(ch.drop_peer("p1"));
        assert!(!ch.drop_peer("p1"));
        assert_eq!(ch.remotes(now).len(), 1);
        assert_eq!(ch.remotes(now)[0].client_id, 10);

        // 恰好到期的一刻仍算在线（闭区间），超过才剔除
        assert_eq!(ch.remotes(now + TIMEOUT).len(), 1);
        assert!(ch.remotes(now + TIMEOUT + Duration::from_millis(1)).is_empty());
    }

    #[test]
    fn 畸形载荷丢弃_旧离场不删新状态_离场后旧帧不复活() {
        let mut ch = EphemeralChannel::new();
        let now = Instant::now();
        // 缺 state / 缺 clock / 缺 clientId：整体丢弃，不因缺字段把对端判成离场
        assert!(!ch.apply_remote("p1", &json!({ "clientId": 9, "clock": 1 }), now));
        assert!(!ch.apply_remote("p1", &json!({ "clientId": 9, "state": {} }), now));
        assert!(ch.remotes(now).is_empty());

        assert!(ch.apply_remote("p1", &payload(5, json!({ "v": 1 })), now));
        // 旧 clock 的离场帧删不掉新状态
        assert!(!ch.apply_remote("p1", &json!({ "clientId": 9, "clock": 4, "state": null }), now));
        assert_eq!(ch.remotes(now).len(), 1);

        // 正常离场后，更旧的 state 帧不得把条目插回来（clock 水位保留）
        assert!(ch.apply_remote("p1", &json!({ "clientId": 9, "clock": 6, "state": null }), now));
        assert!(!ch.apply_remote("p1", &payload(5, json!({ "v": 1 })), now));
        assert!(!ch.has_remotes());
    }

    #[test]
    fn 复位后重发身份_远端清空不动本端() {
        let mut ch = EphemeralChannel::new();
        let now = Instant::now();
        let first = ch.set_local_state(now, json!({ "v": 1 })).expect("首帧");
        ch.apply_remote("p1", &payload(1, json!({})), now);
        ch.clear_remotes();
        assert!(ch.remotes(now).is_empty());

        let resent = ch.resend(now).expect("在场应可重发");
        assert!(resent["clock"].as_u64().unwrap() > first["clock"].as_u64().unwrap());
    }
}
