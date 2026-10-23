//! 表格协作在场：本机选区经通用瞬时通道上行，远端选区解析为网格绘制视图。
//!
//! 通道载荷只带稳定 id（行 / 字段 id）与身份色，不含单元格内容；对端行被删后
//! 选区自然失配（`table_ops::region_of` 返回 None），不画即可，无需额外同步。

use std::time::Instant;

use atelyx_core::ephemeral::EphemeralChannel;
use atelyx_core::table_ops::{self, TableSelection};
use serde_json::{json, Value};

use crate::ui::palette as pal;

/// 宿主内置通道名（插件通道为 `<插件id>:<频道>`，两套命名空间互不覆盖）。
pub const CHANNEL: &str = "host.table";

/// 本端身份（名签与成员色由壳提供；通道层不感知）。
pub struct Identity<'a> {
    pub name: &'a str,
    pub color: &'a str,
}

/// 远端协作者在当前表格上的选区（只收同文件条目；身份色同时保留协议原串与解析值，
/// 畸形色两者都回落灰）。
pub struct RemoteSelection {
    pub name: String,
    pub color: iced::Color,
    pub color_hex: String,
    pub selection: TableSelection,
}

pub struct TableCollab {
    channel: EphemeralChannel,
    /// 本端已挂的文件（None = 不在表格）。
    file: Option<String>,
    /// 已写入通道状态的选区（变化检测基准，含被限频压下待补发的那份）。
    sent_selection: Option<TableSelection>,
    /// 已写入通道状态的身份（昵称 / 成员色）：设置里改名或换色也要让对端看到。
    sent_identity: Option<(String, String)>,
    /// 当前文件可绘制的远端选区（事件与 tick 驱动刷新，绘制只读）。
    remote: Vec<RemoteSelection>,
}

impl TableCollab {
    pub fn new() -> Self {
        TableCollab {
            channel: EphemeralChannel::new(),
            file: None,
            sent_selection: None,
            sent_identity: None,
            remote: Vec::new(),
        }
    }

    /// 本端挂着的文件（None = 未在场）。
    pub fn file(&self) -> Option<&str> {
        self.file.as_deref()
    }

    /// 已提交到通道状态的选区与身份（调用方据此跳过无变化的同步）。
    pub fn sent_selection(&self) -> Option<&TableSelection> {
        self.sent_selection.as_ref()
    }

    pub fn sent_identity(&self) -> Option<(&str, &str)> {
        self.sent_identity
            .as_ref()
            .map(|(name, color)| (name.as_str(), color.as_str()))
    }

    pub fn remote(&self) -> &[RemoteSelection] {
        &self.remote
    }

    /// 进入 / 切换文件：立即构造本端状态（选区为空也上报，供对端显示「在此表」）。
    pub fn enter(
        &mut self,
        now: Instant,
        identity: &Identity<'_>,
        file: &str,
        selection: Option<&TableSelection>,
    ) -> Option<Value> {
        self.file = Some(file.to_owned());
        self.sent_selection = selection.cloned();
        self.sent_identity = Some(identity_pair(identity));
        // 换文件后旧文件的远端条目不再属于本视图
        self.remote.clear();
        self.channel
            .set_local_state(now, state_json(identity, file, selection))
    }

    /// 选区变化上报（同文件且选区未变 = None；限频压下的变化由 [`flush`](Self::flush) 补发）。
    pub fn update(
        &mut self,
        now: Instant,
        identity: &Identity<'_>,
        file: &str,
        selection: Option<&TableSelection>,
    ) -> Option<Value> {
        if self.file.as_deref() != Some(file) {
            return self.enter(now, identity, file, selection);
        }
        if self.sent_selection.as_ref() == selection
            && self.sent_identity.as_ref() == Some(&identity_pair(identity))
        {
            return None;
        }
        self.sent_selection = selection.cloned();
        self.sent_identity = Some(identity_pair(identity));
        self.channel
            .set_local_state(now, state_json(identity, file, selection))
    }

    /// 离场（关闭表格 / 离开空间）：产出 null 条目并清本端记录。
    pub fn leave(&mut self, now: Instant) -> Option<Value> {
        self.file = None;
        self.sent_selection = None;
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

    /// 通道里是否有任何远端条目（含未给选区的）：有对端才需要周期拍做剔除与保活。
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

    /// 重新解析远端条目（超时剔除也在这里发生）：只保留与当前文件匹配、且带选区的条目。
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
                let selection = table_ops::selection_from_json(state.get("selection")?)?;
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
                Some(RemoteSelection { name, color, color_hex, selection })
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

impl Default for TableCollab {
    fn default() -> Self {
        Self::new()
    }
}

fn identity_pair(identity: &Identity<'_>) -> (String, String) {
    (identity.name.to_owned(), identity.color.to_owned())
}

fn state_json(identity: &Identity<'_>, file: &str, selection: Option<&TableSelection>) -> Value {
    json!({
        "file": file,
        "name": identity.name,
        "color": identity.color,
        "selection": selection
            .map(table_ops::selection_to_json)
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
            "state": { "file": file, "name": "鲍勃", "color": "#3d7be0", "selection": selection },
        })
    }

    #[test]
    fn 进入后上报含文件与选区_同选区不再上行() {
        let mut collab = TableCollab::new();
        let now = Instant::now();
        let sel = TableSelection::Cell { row_id: "r1".into(), field_id: "f1".into() };
        let first = collab
            .enter(now, &identity(), "a.atb", Some(&sel))
            .expect("首次应产帧");
        assert_eq!(first["state"]["file"], "a.atb");
        assert_eq!(first["state"]["selection"]["kind"], "cell");
        assert_eq!(collab.file(), Some("a.atb"));
        assert!(collab.update(now, &identity(), "a.atb", Some(&sel)).is_none());
    }

    #[test]
    fn 身份变化也要上行_同身份同选区不再上行() {
        let mut collab = TableCollab::new();
        let now = Instant::now();
        let sel = TableSelection::Cell { row_id: "r1".into(), field_id: "f1".into() };
        collab.enter(now, &identity(), "a.atb", Some(&sel)).expect("进入");
        assert!(collab.update(now, &identity(), "a.atb", Some(&sel)).is_none());
        let renamed = Identity { name: "爱丽丝2", color: "#e0533d" };
        let frame = collab
            .update(now + Duration::from_millis(60), &renamed, "a.atb", Some(&sel))
            .expect("改名应对端可见");
        assert_eq!(frame["state"]["name"], "爱丽丝2");
        assert!(collab
            .update(now + Duration::from_millis(120), &renamed, "a.atb", Some(&sel))
            .is_none());
    }

    #[test]
    fn 换文件清远端_离场产null() {
        let mut collab = TableCollab::new();
        let now = Instant::now();
        collab.enter(now, &identity(), "a.atb", None).expect("进入");
        collab.apply(
            "p1",
            &remote_payload(9, 1, "a.atb", json!({ "kind": "row", "rowId": "r1" })),
            now,
        );
        assert_eq!(collab.remote().len(), 1, "同文件选区应可绘制");

        // 换文件落在限频窗口内：只记录，最新一份由 flush 补发
        assert!(collab.enter(now, &identity(), "b.atb", None).is_none());
        assert!(collab.remote().is_empty(), "换文件后不保留旧表选区");
        let swapped = collab
            .flush(now + Duration::from_millis(60))
            .expect("到期应补发新文件状态");
        assert_eq!(swapped["state"]["file"], "b.atb");

        let leave = collab.leave(now).expect("在场应产离场帧");
        assert!(leave["state"].is_null());
        assert!(collab.leave(now).is_none());
    }

    #[test]
    fn 远端只收同文件且有选区的条目() {
        let mut collab = TableCollab::new();
        let now = Instant::now();
        collab.enter(now, &identity(), "a.atb", None).expect("进入");

        collab.apply("p1", &remote_payload(9, 1, "b.atb", json!({ "kind": "all" })), now);
        assert!(collab.remote().is_empty(), "别的文件不画");

        collab.apply(
            "p1",
            &remote_payload(9, 2, "a.atb", Value::Null),
            now + Duration::from_millis(60),
        );
        assert!(collab.remote().is_empty(), "无选区不画");

        collab.apply(
            "p1",
            &remote_payload(9, 3, "a.atb", json!({ "kind": "column", "fieldId": "f2" })),
            now + Duration::from_millis(120),
        );
        let remote = collab.remote();
        assert_eq!(remote.len(), 1);
        assert_eq!(remote[0].name, "鲍勃");
        assert_eq!(remote[0].color_hex, "#3d7be0");
        assert_eq!(remote[0].selection, TableSelection::Column("f2".into()));
    }

    #[test]
    fn 远端颜色畸形回落灰_对端断链摘除() {
        let mut collab = TableCollab::new();
        let now = Instant::now();
        collab.enter(now, &identity(), "a.atb", None).expect("进入");
        let bad_color = json!({
            "clientId": 7, "clock": 1,
            "state": { "file": "a.atb", "name": "鲍勃", "color": "not-a-color",
                       "selection": { "kind": "cell", "rowId": "r1", "fieldId": "f1" } },
        });
        collab.apply("p1", &bad_color, now);
        assert_eq!(collab.remote().len(), 1, "颜色畸形只回落颜色，条目仍可见");
        assert_eq!(collab.remote()[0].color, pal::FALLBACK_COLLAB);
        assert_eq!(collab.remote()[0].color_hex, pal::FALLBACK_COLLAB_COLOR);

        assert!(collab.drop_peer("p1"));
        assert!(collab.remote().is_empty());
    }

    #[test]
    fn 远端超时剔除_重连重发身份() {
        let mut collab = TableCollab::new();
        let now = Instant::now();
        collab.enter(now, &identity(), "a.atb", None).expect("进入");
        collab.apply(
            "p1",
            &remote_payload(9, 1, "a.atb", json!({ "kind": "all" })),
            now,
        );
        assert_eq!(collab.remote().len(), 1);
        collab.refresh(now + Duration::from_secs(31));
        assert!(collab.remote().is_empty(), "30s 无更新应剔除");

        let resent = collab.resend(now).expect("在场应可重发");
        assert_eq!(resent["state"]["file"], "a.atb");
    }
}
