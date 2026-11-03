//! 画布实时协作纯逻辑：远端补丁双基线合入、对话卡消息合并、编辑锁锁主判定。
//! 全部无 I/O；合入语义与 `apply_canvas_patch` 的稳定 id 合并对齐。

use std::collections::HashSet;

use serde_json::Value;

use super::{apply_canvas_patch, CanvasFile, CanvasFileNode, CanvasPatch};

/// 远端补丁合入副作用（调用方据此做缓存失效 / 引用补读 / 面板跟随 / 锁与选中清理）。
#[derive(Default)]
pub struct RemoteApplyOutcome {
    /// 被整体替换或新增的对话卡节点 id（面板同会话视图跟随刷新）。
    pub upserted_conversations: Vec<String>,
    /// 合入后需要补读内容的仓库相对路径（上行卡 data.file 引用；已缓存的路径由调用方去重）。
    pub ref_paths: Vec<String>,
    /// 被远端删除的节点 id。
    pub removed_node_ids: Vec<String>,
}

/// 远端画布补丁合入内存会话与保存基线（双基线）。
///
/// - `data`：对话卡消息做保护性合并——合入前该卡消息与 `saved` 不一致（本端有流式 /
///   未落盘消息）时，以远端为基底、本端独有消息按 id 补尾、同 id 以远端为胜；一致时整体
///   取远端。
/// - `saved`：原始补丁直合（推进「已知服务端真源」）。`advance_baseline` = false（未确认
///   落地的透传帧）时跳过，基线不动。
/// - 两基线推进后，仅含远端内容的会话对 `compute_canvas_patch` 不再产生补丁（不回环）。
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
/// 同 id 以远端为胜。
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
    use crate::canvas::compute_canvas_patch;
    use serde_json::json;

    fn node(id: &str, node_type: &str, x: f64, data: Value) -> CanvasFileNode {
        CanvasFileNode { id: id.into(), node_type: node_type.into(), x, y: 0.0, width: None, height: None, data }
    }

    fn canvas(nodes: Vec<CanvasFileNode>) -> CanvasFile {
        CanvasFile {
            schema: super::super::CANVAS_SCHEMA.into(),
            id: "c1".into(),
            title: "画布".into(),
            nodes,
            edges: Vec::new(),
            created_at: 0,
            updated_at: 0,
        }
    }

    fn conv(id: &str, x: f64, messages: Value) -> CanvasFileNode {
        node(id, "conversation", x, json!({ "messages": messages }))
    }

    #[test]
    fn remote_patch_advances_both_baselines() {
        let base = canvas(vec![node("a", "text", 0.0, json!({}))]);
        let mut data = base.clone();
        let mut saved = base.clone();
        let remote = canvas(vec![node("a", "text", 100.0, json!({}))]);
        let patch = compute_canvas_patch(&base, &remote).unwrap();
        apply_remote_patch(&mut data, &mut saved, &patch, true);
        assert_eq!(data.nodes[0].x, 100.0);
        assert!(compute_canvas_patch(&saved, &data).is_none(), "远端内容不回环");
    }

    #[test]
    fn remote_patch_keeps_local_unsaved_messages() {
        let base = canvas(vec![conv("c", 0.0, json!([]))]);
        let mut data = base.clone();
        let mut saved = base.clone();
        data.nodes[0].data["messages"] = json!([{ "id": "local" }]);
        let remote = canvas(vec![conv("c", 0.0, json!([{ "id": "remote" }]))]);
        let patch = compute_canvas_patch(&base, &remote).unwrap();
        apply_remote_patch(&mut data, &mut saved, &patch, true);
        let ids: Vec<&str> = data.nodes[0].data["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["remote", "local"]);
    }

    #[test]
    fn lock_owner_is_smallest_since_then_peer_id() {
        let claims = vec![
            LockClaim { peer_id: "b".into(), since: 9 },
            LockClaim { peer_id: "a".into(), since: 9 },
            LockClaim { peer_id: "c".into(), since: 8 },
        ];
        assert_eq!(compute_lock_owner(&claims), Some("c".into()));
        let claims = vec![
            LockClaim { peer_id: "10".into(), since: 1 },
            LockClaim { peer_id: "9".into(), since: 1 },
        ];
        assert_eq!(compute_lock_owner(&claims), Some("9".into()));
    }
}
