//! 表格协作在场：本端选区上行与对端选区维护。走宿主通用 `collab` 通道（同插件同空间互通），
//! 插件不连协作协议；对端身份色随消息带出，过期即剔除。

use std::collections::HashMap;
use std::time::Instant;

use atelyx_format::table::ops;
use atelyx_plugin_sdk::Context;

use crate::model::{Rect, Session};
use crate::PluginState;

/// 协作通道名（宿主按插件 id 加命名空间前缀）。
const CHANNEL: &str = "table-presence";
/// 对端选区过期（超过该时长没再收到即剔除；与宿主瞬时通道同档）。
const PEER_TTL_MS: u128 = 30_000;
/// 本端状态保活间隔：空闲端不重发就会被对端按 TTL 剔除。
const KEEPALIVE_MS: u128 = 15_000;
/// 本端选区上行最小间隔：连续拖选合并成一次，只发最后一版。
const SEND_INTERVAL_MS: u128 = 250;

/// 一位对端的选区。
#[derive(Clone)]
pub struct RemotePeer {
    pub name: String,
    pub color: [u8; 4],
    pub selection: ops::TableSelection,
    pub at: Instant,
}

/// 对端选区（描边矩形 + 身份色 + 昵称；昵称空 = 不画名签）。
#[derive(Clone, Debug)]
pub struct RemoteCursor {
    pub rect: Rect,
    pub color: [u8; 4],
    pub name: String,
}

/// 协作在场缓存。
#[derive(Default)]
pub struct Presence {
    peers: HashMap<String, RemotePeer>,
    /// 最近一次上行的本端状态与时刻（保活重发用）。
    sent: Option<serde_json::Value>,
    sent_at: Option<Instant>,
    /// 被最小上行间隔挡下的新状态：到点后由下一拍补发，避免停在旧状态。
    pending: Option<serde_json::Value>,
}

impl Presence {
    /// 记录一次已受理的本端状态上行。
    pub fn note_sent(&mut self, payload: serde_json::Value) {
        self.sent = Some(payload);
        self.sent_at = Some(Instant::now());
    }

    /// 到期保活：返回需要重发的本端状态（未到间隔 = None）。
    pub fn keepalive(&mut self) -> Option<serde_json::Value> {
        let at = self.sent_at?;
        if at.elapsed().as_millis() < KEEPALIVE_MS {
            return None;
        }
        self.sent_at = Some(Instant::now());
        self.sent.clone()
    }

    /// 载荷与上次相同则交给保活；不同则受最小上行间隔限制。
    pub fn should_send(&mut self, payload: &serde_json::Value) -> bool {
        match (&self.sent, self.sent_at) {
            (Some(sent), _) if sent == payload => {
                self.pending = None;
                false
            }
            (Some(_), Some(at)) if at.elapsed().as_millis() < SEND_INTERVAL_MS => {
                self.pending = Some(payload.clone());
                false
            }
            _ => {
                self.pending = None;
                true
            }
        }
    }

    /// 取被最小间隔挡下的新状态（未到间隔或无待发 = None）。
    pub fn take_pending(&mut self) -> Option<serde_json::Value> {
        let at = self.sent_at?;
        if at.elapsed().as_millis() < SEND_INTERVAL_MS {
            return None;
        }
        self.pending.take()
    }

    /// 离场：清本端与对端记录（本端状态由调用方补发空载荷）。
    pub fn leave(&mut self) {
        self.sent = None;
        self.sent_at = None;
        self.pending = None;
        self.peers.clear();
    }

    /// 收到一条对端选区（`selection` 缺省 = 该对端收起选区；非当前文件的条目忽略）。
    pub fn apply(
        &mut self,
        peer_id: &str,
        name: Option<&str>,
        peer_color: Option<&str>,
        payload: &serde_json::Value,
        file: &str,
    ) {
        if payload.get("path").and_then(|path| path.as_str()) != Some(file) {
            self.peers.remove(peer_id);
            return;
        }
        let Some(selection) = payload.get("selection").and_then(parse_selection) else {
            self.peers.remove(peer_id);
            return;
        };
        // 身份色以宿主名册为准（跨插件一致）；旧对端未带名册色时再回落到帧内色 / 稳定色
        let color = peer_color
            .and_then(crate::text::parse_hex)
            .or_else(|| {
                payload
                    .get("color")
                    .and_then(|color| color.as_str())
                    .and_then(crate::text::parse_hex)
            })
            .unwrap_or_else(|| fallback_color(peer_id));
        let name = name
            .map(str::to_string)
            .or_else(|| payload.get("name").and_then(|name| name.as_str()).map(str::to_string))
            .unwrap_or_default();
        self.peers.insert(
            peer_id.to_string(),
            RemotePeer { name, color, selection, at: Instant::now() },
        );
    }

    /// 剔除过期对端（返回是否有条目被移除）。
    pub fn prune(&mut self) -> bool {
        let now = Instant::now();
        let before = self.peers.len();
        self.peers
            .retain(|_, peer| now.duration_since(peer.at).as_millis() < PEER_TTL_MS);
        before != self.peers.len()
    }

    /// 对端选区矩形、身份色与昵称（按当前布局换算）。
    pub fn rects(&self, session: &Session) -> Vec<RemoteCursor> {
        let Some(geo) = crate::geom::geo(
            &session.view.layout,
            session.view.scroll_x,
            session.view.scroll_y,
            session.view.zoom,
            session.view.canvas_size,
        ) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for peer in self.peers.values() {
            let Some(region) =
                ops::region_of(Some(&peer.selection), &session.data.fields, &session.data.rows)
            else {
                continue;
            };
            let first = geo.cell_rect(region.row_start, region.col_start);
            let last = geo.cell_rect(region.row_end, region.col_end);
            out.push(RemoteCursor {
                rect: Rect::new(
                    first.x,
                    first.y,
                    (last.right() - first.x).max(0.0),
                    (last.bottom() - first.y).max(0.0),
                ),
                color: peer.color,
                name: peer.name.clone(),
            });
        }
        out
    }

    /// 对端选区列表（服务面快照用）：昵称 / 身份色 / 选区。
    pub fn selections(
        &self,
        fields: &[atelyx_format::table::TableField],
        rows: &[atelyx_format::table::TableRow],
    ) -> Vec<(String, String, atelyx_plugin_protocol::method::TableSelection)> {
        self.peers
            .values()
            .filter(|peer| ops::region_of(Some(&peer.selection), fields, rows).is_some())
            .filter_map(|peer| {
                Some((
                    peer.name.clone(),
                    crate::text::to_hex(peer.color),
                    crate::service::selection_of(&peer.selection)?,
                ))
            })
            .collect()
    }

    /// 选区事件载荷里的远端条目（按当前字段 / 行过滤失效 id）。
    pub fn remote_selections(
        &self,
        fields: &[atelyx_format::table::TableField],
        rows: &[atelyx_format::table::TableRow],
    ) -> Vec<atelyx_plugin_protocol::method::TableRemoteSelection> {
        self.selections(fields, rows)
            .into_iter()
            .map(|(name, color, selection)| {
                atelyx_plugin_protocol::method::TableRemoteSelection { name, color, selection }
            })
            .collect()
    }

    /// 当前协作者（昵称 + 身份色）。
    pub fn collaborators(&self) -> Vec<(String, String)> {
        self.peers
            .values()
            .filter(|peer| !peer.name.is_empty())
            .map(|peer| (peer.name.clone(), crate::text::to_hex(peer.color)))
            .collect()
    }

}

/// 对端未带身份色时按 peer id 取一个稳定色：同一条通道里不同协作者不再全是同一种蓝。
fn fallback_color(peer_id: &str) -> [u8; 4] {
    const COLORS: [[u8; 4]; 6] = [
        [0x61, 0xaf, 0xef, 0xff],
        [0xef, 0x8f, 0x61, 0xff],
        [0x8f, 0xc9, 0x77, 0xff],
        [0xc9, 0x7f, 0xd0, 0xff],
        [0xef, 0xc9, 0x61, 0xff],
        [0x77, 0xc9, 0xc9, 0xff],
    ];
    let hash = peer_id.bytes().fold(0usize, |acc, byte| acc.wrapping_mul(31).wrapping_add(byte as usize));
    COLORS[hash % COLORS.len()]
}

/// 发本端选区（空间模式下才有意义；未连接时宿主回 `queued=false`，只记本地状态）。
/// 载荷未变直接跳过，变化受最小上行间隔限制（间隔内的连续变更只发最后一版）。
pub async fn publish_selection(plugin: &PluginState, ctx: &Context) {
    let Some(payload) = plugin.with_session(|session| {
        let selection = session.view.selection.as_ref().and_then(selection_json);
        serde_json::json!({
            "path": session.file,
            "selection": selection,
        })
    }) else {
        return;
    };
    let due = plugin
        .presence
        .lock()
        .map(|mut presence| presence.should_send(&payload))
        .unwrap_or(false);
    if !due {
        return;
    }
    send(plugin, ctx, payload).await;
}

/// 上行一条本端状态：受理后记为已发，失败留日志（保活会在下一拍重发上一次状态）。
async fn send(plugin: &PluginState, ctx: &Context, payload: serde_json::Value) {
    match ctx.collab_send_ephemeral(CHANNEL, payload.clone(), None).await {
        Ok(_) => {
            if let Ok(mut presence) = plugin.presence.lock() {
                presence.note_sent(payload);
            }
        }
        Err(error) => ctx.log_error(&format!("发送表格在场失败：{error}")),
    }
}

/// 离开当前表格：先发一条无选区的同路径载荷，对端据此立刻收起本端（不必等 TTL）。
pub async fn publish_leave(plugin: &PluginState, ctx: &Context, path: &str) {
    let payload = serde_json::json!({ "path": path, "selection": serde_json::Value::Null });
    if let Err(error) = ctx.collab_send_ephemeral(CHANNEL, payload, None).await {
        ctx.log_error(&format!("发送表格离场失败：{error}"));
    }
    if let Ok(mut presence) = plugin.presence.lock() {
        presence.leave();
    }
}

/// 帧拍推进：剔除过期对端；到点保活重发被间隔挡下的选区（返回是否需要重绘）。
pub async fn tick(plugin: &PluginState, ctx: &Context) -> bool {
    let pruned = plugin
        .presence
        .lock()
        .map(|mut presence| presence.prune())
        .unwrap_or(false);
    // 节流窗口内排队的选区在下一拍补发，不依赖下一次输入；与保活同拍只发一条
    let due = plugin
        .presence
        .lock()
        .ok()
        .and_then(|mut presence| presence.keepalive().or_else(|| presence.take_pending()));
    if let Some(payload) = due {
        send(plugin, ctx, payload).await;
    }
    pruned
}

/// 选区 → 线形状（与宿主既有在场载荷同形态）。
fn selection_json(selection: &ops::TableSelection) -> Option<serde_json::Value> {
    Some(match selection {
        ops::TableSelection::Cell { row_id, field_id } => {
            serde_json::json!({ "kind": "cell", "rowId": row_id, "fieldId": field_id })
        }
        ops::TableSelection::Range { anchor_row_id, anchor_field_id, row_id, field_id } => {
            serde_json::json!({
                "kind": "range",
                "anchorRowId": anchor_row_id,
                "anchorFieldId": anchor_field_id,
                "rowId": row_id,
                "fieldId": field_id
            })
        }
        ops::TableSelection::Row(row_id) => serde_json::json!({ "kind": "row", "rowId": row_id }),
        ops::TableSelection::Column(field_id) => {
            serde_json::json!({ "kind": "column", "fieldId": field_id })
        }
        ops::TableSelection::All => serde_json::json!({ "kind": "all" }),
    })
}

/// 收端：解析对端选区（形态不符 = 无选区）。
fn parse_selection(value: &serde_json::Value) -> Option<ops::TableSelection> {
    let kind = value.get("kind")?.as_str()?;
    let text = |key: &str| value.get(key).and_then(|v| v.as_str()).map(str::to_string);
    Some(match kind {
        "cell" => ops::TableSelection::Cell { row_id: text("rowId")?, field_id: text("fieldId")? },
        "range" => ops::TableSelection::Range {
            anchor_row_id: text("anchorRowId")?,
            anchor_field_id: text("anchorFieldId")?,
            row_id: text("rowId")?,
            field_id: text("fieldId")?,
        },
        "row" => ops::TableSelection::Row(text("rowId")?),
        "column" => ops::TableSelection::Column(text("fieldId")?),
        "all" => ops::TableSelection::All,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;

    use super::Presence;

    fn payload(row: &str) -> serde_json::Value {
        json!({ "path": "t.atb", "selection": { "kind": "cell", "rowId": row, "fieldId": "f" } })
    }

    impl Presence {
        /// 回拨最近上行时刻：用例以固定时刻验间隔边界，免得真实等待在负载下越过窗口。
        fn backdate_send(&mut self, ago: Duration) {
            if let Some(at) = self.sent_at {
                self.sent_at = Some(at.checked_sub(ago).unwrap_or(at));
            }
        }
    }

    #[test]
    fn local_updates_are_throttled_and_keep_latest() {
        let mut presence = Presence::default();
        let first = payload("r1");
        let latest = payload("r2");
        assert!(presence.should_send(&first), "首条上行不受间隔限制");
        presence.note_sent(first);
        assert!(!presence.should_send(&latest), "窗口内只记待发");
        presence.backdate_send(Duration::from_millis(200));
        assert!(presence.take_pending().is_none(), "未满最小间隔不补发");
        presence.backdate_send(Duration::from_millis(100));
        assert_eq!(presence.take_pending(), Some(latest), "窗口过后补发最后一版");
    }

    /// 同一状态不产生新上行，已排队的待发一并作废。
    #[test]
    fn identical_payload_is_not_resent() {
        let mut presence = Presence::default();
        let state = payload("r1");
        assert!(presence.should_send(&state));
        presence.note_sent(state.clone());
        assert!(!presence.should_send(&state));
        assert!(presence.pending.is_none());
    }

    /// 名册色优先于帧内自报色，同一成员在画布 / 表格里取同一身份色。
    #[test]
    fn roster_color_wins_over_payload_color() {
        let mut presence = Presence::default();
        presence.apply("p1", Some("林"), Some("#123456"), &payload("r1"), "t.atb");
        let peer = presence.peers.get("p1").unwrap();
        assert_eq!(peer.color, [0x12, 0x34, 0x56, 0xff]);
    }
}
