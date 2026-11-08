//! 画布实时协作纯逻辑：远端补丁双基线合入、对话卡消息合并、编辑锁锁主判定。
//!
//! 全部无 I/O；合入语义与 `vault::apply_canvas_patch` 的稳定 id 合并对齐。

use std::collections::HashSet;

use serde_json::Value;

use crate::vault::{apply_canvas_patch, CanvasFile, CanvasFileNode, CanvasPatch};

/// 远端补丁合入副作用（壳据此做缓存失效 / 引用补读 / 面板跟随 / 锁与选中清理）。
#[derive(Default)]
pub struct RemoteApplyOutcome {
    /// 被整体替换或新增的对话卡节点 id（面板同会话视图跟随刷新）。
    pub upserted_conversations: Vec<String>,
    /// 合入后需要补读内容的仓库相对路径（上行卡 data.file 引用；已缓存的路径由壳去重决策）。
    pub ref_paths: Vec<String>,
    /// 被远端删除的节点 id。
    pub removed_node_ids: Vec<String>,
}

/// 远端画布补丁合入内存会话与保存基线（双基线）。
///
/// - `data`：对话卡消息做保护性合并——合入前该卡消息与 `saved` 不一致（本端有流式 /
///   未落盘消息）时，以远端为基底、本端独有消息按 id 补尾、同 id 以远端为胜；一致时整体
///   取远端（远端的消息增删照单全收，不复活本端已落盘删除的消息）。
/// - `saved`：原始补丁直合（推进「已知服务端真源」）。`advance_baseline` = false（未确认
///   落地的透传帧）时跳过，基线不动。
/// - 两基线推进后，仅含远端内容的会话对 `compute_canvas_patch` 不再产生补丁（不回环），
///   本端未落盘编辑的补丁保持只含本端增量（不膨胀）。
///
/// 调用方先守卫补丁 id 与画布 id 一致；`data` 与 `saved` 必须来自同一会话。
pub fn apply_remote_patch(
    data: &mut CanvasFile,
    saved: &mut CanvasFile,
    patch: &CanvasPatch,
    advance_baseline: bool,
) -> RemoteApplyOutcome {
    let mut outcome = RemoteApplyOutcome {
        removed_node_ids: patch.removed_node_ids.clone(),
        ..RemoteApplyOutcome::default()
    };
    // 本端未落盘消息保护名单：合入前该卡 data 消息与基线不一致的对话卡
    let mut unsaved_messages: Vec<(String, Vec<Value>)> = Vec::new();
    for n in &patch.upsert_nodes {
        if n.node_type != "conversation" {
            continue;
        }
        outcome.upserted_conversations.push(n.id.clone());
        let Some(existing) = data.nodes.iter().find(|x| x.id == n.id) else {
            continue;
        };
        let remote_msgs = node_messages(saved_node(saved, &n.id));
        if node_messages(Some(existing)) != remote_msgs {
            unsaved_messages.push((n.id.clone(), node_messages(Some(existing)).to_vec()));
        }
    }
    if advance_baseline {
        apply_canvas_patch(saved, patch);
    }
    apply_canvas_patch(data, patch);
    for (id, local) in unsaved_messages {
        let Some(node) = data.nodes.iter_mut().find(|x| x.id == id) else { continue };
        let remote = node_messages(Some(node)).to_vec();
        set_node_messages(node, merge_messages(&remote, &local));
    }
    for n in &patch.upsert_nodes {
        if let Some(file) = n.data.get("file").and_then(Value::as_str) {
            outcome.ref_paths.push(file.to_string());
        }
    }
    outcome
}

fn saved_node<'a>(saved: &'a CanvasFile, id: &str) -> Option<&'a CanvasFileNode> {
    saved.nodes.iter().find(|x| x.id == id)
}

fn node_messages(node: Option<&CanvasFileNode>) -> &[Value] {
    node.and_then(|n| n.data.get("messages"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn set_node_messages(node: &mut CanvasFileNode, messages: Vec<Value>) {
    if let Some(obj) = node.data.as_object_mut() {
        obj.insert("messages".to_string(), Value::Array(messages));
    }
}

/// 对话卡消息合并：远端为基底，本端独有消息（对端尚未见到的流式 / 未落盘消息）按原序补尾；
/// 同 id 以远端为胜。锁模型下对端不并发写同一张卡的消息，合并只为兜底对端搬动本端
/// 正在生成的卡时陈旧节点快照不冲掉进行中内容。
fn merge_messages(remote: &[Value], local: &[Value]) -> Vec<Value> {
    let mut merged = remote.to_vec();
    let remote_ids: HashSet<&str> = remote
        .iter()
        .filter_map(|m| m.get("id").and_then(Value::as_str))
        .collect();
    for m in local {
        let Some(id) = m.get("id").and_then(Value::as_str) else { continue };
        if !remote_ids.contains(id) {
            merged.push(m.clone());
        }
    }
    merged
}

/// 单节点独占编辑锁的锁主判定：`since` 最小者持有；同 `since` 按 peerId 取小（数字 id 按
/// 数值比，非数字按字典序，数字恒先于非数字）。全部对端对同一批声明算出同一锁主。
pub fn compute_lock_owner(claims: &[LockClaim]) -> Option<String> {
    let mut best: Option<&LockClaim> = None;
    for c in claims {
        let take = match best {
            None => true,
            Some(b) => (c.since, peer_rank(&c.peer_id)) < (b.since, peer_rank(&b.peer_id)),
        };
        if take {
            best = Some(c);
        }
    }
    best.map(|c| c.peer_id.clone())
}

#[derive(Debug, Clone, PartialEq)]
pub struct LockClaim {
    pub peer_id: String,
    /// 占锁时刻（毫秒时间戳；本端已有锁不刷新）。
    pub since: i64,
}

/// peerId 排序键：数字 id 按数值排（恒先于非数字），非数字按字典序。
fn peer_rank(peer_id: &str) -> (u8, u64, &str) {
    match peer_id.parse::<u64>() {
        Ok(n) => (0, n, ""),
        Err(_) => (1, 0, peer_id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::{compute_canvas_patch, CanvasFile, CanvasFileNode, CanvasFileEdge};
    use serde_json::json;

    fn node(id: &str, node_type: &str, x: f64, data: Value) -> CanvasFileNode {
        CanvasFileNode { id: id.into(), node_type: node_type.into(), x, y: 0.0, width: None, height: None, data }
    }

    fn canvas(nodes: Vec<CanvasFileNode>) -> CanvasFile {
        CanvasFile {
            schema: "atelyx-canvas/v1".into(),
            id: "c1".into(),
            title: "画布".into(),
            nodes,
            edges: Vec::<CanvasFileEdge>::new(),
            created_at: 0,
            updated_at: 0,
        }
    }

    fn conv(id: &str, x: f64, messages: Value) -> CanvasFileNode {
        node(id, "conversation", x, json!({ "title": "对话", "messages": messages }))
    }

    /// 回环复现：干净会话合入远端补丁后，两基线一致，不再产生补丁（不广播）。
    #[test]
    fn remote_apply_does_not_rebroadcast() {
        let base = canvas(vec![node("a", "text", 0.0, json!({"bodyMd": "一"}))]);
        let mut data = base.clone();
        let mut saved = base.clone();
        let mut remote = base.clone();
        remote.nodes[0].x = 100.0;
        let patch = compute_canvas_patch(&base, &remote).unwrap();
        apply_remote_patch(&mut data, &mut saved, &patch, true);
        assert!(compute_canvas_patch(&saved, &data).is_none());
        assert_eq!(data.nodes[0].x, 100.0);
    }

    /// 膨胀复现：本端脏（编辑卡 B）期间合入远端对卡 A 的改动，下一次补丁只含本端增量。
    #[test]
    fn remote_apply_keeps_local_edits_incremental() {
        let base = canvas(vec![
            node("a", "text", 0.0, json!({"bodyMd": "一"})),
            node("b", "text", 0.0, json!({"bodyMd": "二"})),
        ]);
        let mut saved = base.clone();
        let mut data = base.clone();
        data.nodes[1].x = 50.0; // 本端未落盘编辑
        let mut remote = base.clone();
        remote.nodes[0].x = 100.0;
        let patch = compute_canvas_patch(&base, &remote).unwrap();
        apply_remote_patch(&mut data, &mut saved, &patch, true);
        let next = compute_canvas_patch(&saved, &data).unwrap();
        assert_eq!(next.upsert_nodes.len(), 1);
        assert_eq!(next.upsert_nodes[0].id, "b");
        assert!(next.removed_node_ids.is_empty());
    }

    /// 流式保护：本端对话卡有未落盘消息（流式中），远端陈旧快照搬动该卡——
    /// 本端独有消息保留在尾部，坐标更新照常生效。
    #[test]
    fn streaming_messages_survive_peer_move() {
        let msgs = |ids: &[&str]| Value::Array(ids.iter().map(|i| json!({"id": i, "role": "user"})).collect());
        let base = canvas(vec![conv("c", 0.0, msgs(&["m1", "m2"]))]);
        let mut saved = base.clone();
        let mut data = base.clone();
        data.nodes[0] = conv("c", 0.0, msgs(&["m1", "m2", "m3", "m4"]));
        let mut remote = base.clone();
        remote.nodes[0] = conv("c", 80.0, msgs(&["m1", "m2"]));
        let patch = compute_canvas_patch(&base, &remote).unwrap();
        let outcome = apply_remote_patch(&mut data, &mut saved, &patch, true);
        assert_eq!(outcome.upserted_conversations, vec!["c".to_string()]);
        assert_eq!(node_messages(Some(&data.nodes[0])), node_messages(Some(&conv("c", 0.0, msgs(&["m1", "m2", "m3", "m4"])))));
        assert_eq!(data.nodes[0].x, 80.0);
        // 本端独有消息尚未上行：下一轮补丁把它们带给服务端
        let next = compute_canvas_patch(&saved, &data).unwrap();
        assert_eq!(next.upsert_nodes.len(), 1);
    }

    /// 消息已同步（本端无未落盘变更）时整体取远端：远端删除 m2、新增 m9 照单全收，
    /// 已落盘删除的消息不复活，基线推进后不再产生补丁。
    #[test]
    fn synced_card_takes_remote_messages_wholesale() {
        let msgs = |ids: &[&str]| Value::Array(ids.iter().map(|i| json!({"id": i, "role": "user"})).collect());
        let base = canvas(vec![conv("c", 0.0, msgs(&["m1", "m2"]))]);
        let mut data = base.clone();
        let mut saved = base.clone();
        let mut remote = base.clone();
        remote.nodes[0] = conv("c", 0.0, msgs(&["m1", "m9"]));
        let patch = compute_canvas_patch(&base, &remote).unwrap();
        apply_remote_patch(&mut data, &mut saved, &patch, true);
        assert_eq!(node_messages(Some(&data.nodes[0])), node_messages(Some(&conv("c", 0.0, msgs(&["m1", "m9"])))));
        assert!(compute_canvas_patch(&saved, &data).is_none());
    }

    /// 同 id 消息以远端为胜；本端独有消息补尾。
    #[test]
    fn merge_remote_wins_same_id_and_appends_local_extras() {
        let remote = vec![json!({"id": "m1", "text": "远端"}), json!({"id": "m2", "text": "远端二"})];
        let local = vec![
            json!({"id": "m1", "text": "本端"}),
            json!({"id": "m0", "text": "本端独有"}),
            json!({"id": "m2", "text": "本端二"}),
        ];
        let merged = merge_messages(&remote, &local);
        assert_eq!(
            merged,
            vec![
                json!({"id": "m1", "text": "远端"}),
                json!({"id": "m2", "text": "远端二"}),
                json!({"id": "m0", "text": "本端独有"}),
            ]
        );
    }

    /// 未确认落地（advance_baseline = false）：data 合入、基线不动（下轮补丁重含远端内容）。
    #[test]
    fn relay_frame_without_landing_keeps_baseline() {
        let base = canvas(vec![node("a", "text", 0.0, json!({"bodyMd": "一"}))]);
        let mut data = base.clone();
        let mut saved = base.clone();
        let mut remote = base.clone();
        remote.nodes[0].x = 100.0;
        let patch = compute_canvas_patch(&base, &remote).unwrap();
        apply_remote_patch(&mut data, &mut saved, &patch, false);
        assert_eq!(data.nodes[0].x, 100.0);
        assert_eq!(saved.nodes[0].x, 0.0);
    }

    /// 引用路径与删除清单随结果返回，供壳补读内容与清理锁 / 选中。
    #[test]
    fn outcome_lists_ref_paths_and_removals() {
        let base = canvas(vec![node("a", "text", 0.0, json!({"title": "笔记卡", "file": "notes/a.md"}))]);
        let mut data = base.clone();
        let mut saved = base.clone();
        let mut remote = base.clone();
        remote.nodes[0].x = 10.0;
        remote.nodes.push(node("m", "media", 0.0, json!({"file": "assets/pic.png"})));
        let patch = compute_canvas_patch(&base, &remote).unwrap();
        let outcome = apply_remote_patch(&mut data, &mut saved, &patch, true);
        assert!(outcome.ref_paths.contains(&"notes/a.md".to_string()));
        assert!(outcome.ref_paths.contains(&"assets/pic.png".to_string()));
    }

    #[test]
    fn lock_owner_picks_min_since_and_tiebreaks_peer_id() {
        assert!(compute_lock_owner(&[]).is_none());
        let claims = |pairs: &[(&str, i64)]| -> Vec<LockClaim> {
            pairs.iter().map(|(p, s)| LockClaim { peer_id: p.to_string(), since: *s }).collect()
        };
        assert_eq!(
            compute_lock_owner(&claims(&[("2", 200), ("1", 100), ("3", 300)])),
            Some("1".to_string())
        );
        // 同 since：数字 id 按数值取小（"10" < "9" 按数值不按字典序）
        assert_eq!(
            compute_lock_owner(&claims(&[("10", 100), ("9", 100)])),
            Some("9".to_string())
        );
        // 非数字 id 按字典序，且恒后于数字 id
        assert_eq!(
            compute_lock_owner(&claims(&[("abc", 100), ("3", 100)])),
            Some("3".to_string())
        );
        assert_eq!(
            compute_lock_owner(&claims(&[("abc", 100), ("abd", 100)])),
            Some("abc".to_string())
        );
    }
}
