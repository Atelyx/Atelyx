//! 画布命令：撤销 / 重做、选择、复制粘贴、新建卡与分组、分组改名与收起、引用连线。

use atelyx_format::canvas::geometry::{bbox_of_rects, display_size, group_collapsed};
use atelyx_format::canvas::{CanvasFileEdge, CanvasFileNode};
use serde_json::Value;

use crate::model::{GROUP_PAD, PASTE_SPOT_STEP, Session};

/// 新建卡（标题 / 尺寸 / 落点由调用方给出），返回新卡 id。
pub fn create_card(
    session: &mut Session,
    node_type: &str,
    x: f64,
    y: f64,
    data: Value,
    width: Option<f64>,
    height: Option<f64>,
) -> String {
    let id = uuid();
    session.push_undo();
    session.data.nodes.push(CanvasFileNode {
        id: id.clone(),
        node_type: node_type.to_string(),
        x: x.round(),
        y: y.round(),
        width,
        height,
        data,
    });
    session.view.selection = vec![id.clone()];
    session.touch();
    id
}

pub fn create_conversation(session: &mut Session, x: f64, y: f64) -> String {
    create_card(
        session,
        "conversation",
        x,
        y,
        Value::Object(Default::default()),
        None,
        None,
    )
}

pub fn create_text(session: &mut Session, x: f64, y: f64) -> String {
    create_card(session, "text", x, y, crate::model::text_data(), None, None)
}

/// 由选中卡（不含分组）的包围盒外扩出分组，返回新组 id。
pub fn create_group_from_selection(session: &mut Session) -> Option<String> {
    let members: Vec<&CanvasFileNode> = session
        .data
        .nodes
        .iter()
        .filter(|node| node.node_type != "group")
        .filter(|node| session.view.selection.iter().any(|id| id == &node.id))
        .collect();
    if members.is_empty() {
        return None;
    }
    let rects: Vec<(f64, f64, f64, f64)> = members
        .iter()
        .map(|node| {
            let (w, h) = display_size(node);
            (node.x, node.y, w, h)
        })
        .collect();
    let (x, y, w, h) = bbox_of_rects(&rects);
    let id = uuid();
    session.push_undo();
    session.data.nodes.push(CanvasFileNode {
        id: id.clone(),
        node_type: "group".to_string(),
        x: x - GROUP_PAD,
        y: y - GROUP_PAD,
        width: Some(w + GROUP_PAD * 2.0),
        height: Some(h + GROUP_PAD * 2.0),
        data: serde_json::json!({ "color": next_group_color(session).to_string() }),
    });
    session.view.selection = vec![id.clone()];
    session.touch();
    Some(id)
}

pub fn create_group_at(session: &mut Session, x: f64, y: f64) -> String {
    let (w, h) = crate::model::new_card_size("group");
    create_card(
        session,
        "group",
        x.round(),
        y.round(),
        serde_json::json!({ "color": next_group_color(session).to_string() }),
        Some(w),
        Some(h),
    )
}

fn next_group_color(session: &Session) -> usize {
    session
        .data
        .nodes
        .iter()
        .filter(|node| node.node_type == "group")
        .count()
        % 6
        + 1
}

pub fn set_group_color(session: &mut Session, id: &str, index: usize) {
    let next = index.to_string();
    let current = session
        .node(id)
        .and_then(|node| node.data.get("color"))
        .and_then(Value::as_str);
    if current == Some(next.as_str()) || session.node(id).is_none() {
        return;
    }
    session.push_undo();
    if let Some(node) = session.node_mut(id) {
        node.data["color"] = Value::String(next);
        session.touch();
    }
}

pub fn toggle_collapse(session: &mut Session, id: &str) {
    if session.node(id).is_none() {
        return;
    }
    session.push_undo();
    let Some(node) = session.node_mut(id) else {
        return;
    };
    if group_collapsed(node) {
        if let Some(object) = node.data.as_object_mut() {
            object.remove("collapsed");
        }
    } else if let Some(object) = node.data.as_object_mut() {
        object.insert("collapsed".to_string(), Value::Bool(true));
    }
    session.touch();
    let nodes = session.data.nodes.clone();
    session
        .view
        .selection
        .retain(|selected| !crate::model::hidden_of(&nodes, selected));
}

pub fn rename_commit(session: &mut Session, id: &str, name: &str) {
    let name = name.trim();
    let current = session
        .node(id)
        .and_then(|node| node.data.get("label"))
        .and_then(Value::as_str);
    let next = (!name.is_empty()).then_some(name);
    if current != next {
        session.push_undo();
        if let Some(node) = session.node_mut(id) {
            if name.is_empty() {
                if let Some(object) = node.data.as_object_mut() {
                    object.remove("label");
                }
            } else {
                node.data["label"] = Value::String(name.to_string());
            }
            session.touch();
        }
    }
    session.view.group_rename = None;
}

/// 删除选中：对端锁定的对话卡跳过；返回被删节点 id（调用方据它停 run）。
pub fn delete_selection(session: &mut Session) -> Vec<String> {
    let locked: Vec<String> = session
        .view
        .selection
        .iter()
        .filter(|id| session.view.locked_by_peer(&session.file, id).is_some())
        .cloned()
        .collect();
    let ids: Vec<String> = session
        .view
        .selection
        .iter()
        .filter(|id| !locked.contains(id))
        .cloned()
        .collect();
    if ids.is_empty() {
        return Vec::new();
    }
    session.push_undo();
    session.data.nodes.retain(|node| !ids.contains(&node.id));
    session
        .data
        .edges
        .retain(|edge| !ids.contains(&edge.source) && !ids.contains(&edge.target));
    session.view.selection.clear();
    session.view.hover = None;
    for id in &ids {
        session.view.card_chat.remove(id);
        session.view.locks.remove(id);
    }
    session.touch();
    ids
}

/// 复制：只带节点（分组不复制），序列化为带标记的 JSON。
pub fn copy_selection(session: &Session) -> Option<String> {
    let cards: Vec<&CanvasFileNode> = session
        .data
        .nodes
        .iter()
        .filter(|node| node.node_type != "group")
        .filter(|node| session.view.selection.iter().any(|id| id == &node.id))
        .collect();
    if cards.is_empty() {
        return None;
    }
    serde_json::to_string(&serde_json::json!({ "atelyxCanvas": 1, "cards": cards })).ok()
}

/// 粘贴落点对齐网格（世界坐标）。
const PASTE_SNAP: f64 = 16.0;

/// 粘贴：节点 id 与对话消息 id 重生成，整组按包围盒对到视口中心，重叠时逐行避让。
/// 任一卡解析失败即整次放弃，避免落半份内容。
pub fn paste(session: &mut Session, text: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return false;
    };
    if value.get("atelyxCanvas").and_then(Value::as_u64) != Some(1) {
        return false;
    }
    let Some(cards) = value.get("cards").and_then(Value::as_array) else {
        return false;
    };
    let mut nodes: Vec<CanvasFileNode> = Vec::new();
    for card in cards {
        let Ok(mut node) = serde_json::from_value::<CanvasFileNode>(card.clone()) else {
            return false;
        };
        node.id = uuid();
        if node.node_type == "conversation" {
            if let Some(messages) = node.data.get_mut("messages").and_then(Value::as_array_mut) {
                for message in messages {
                    if let Some(object) = message.as_object_mut() {
                        object.insert("id".to_string(), Value::String(uuid()));
                    }
                }
            }
        }
        nodes.push(node);
    }
    if nodes.is_empty() {
        return false;
    }
    let rects: Vec<(f64, f64, f64, f64)> = nodes
        .iter()
        .map(|node| {
            let (w, h) = display_size(node);
            (node.x, node.y, w, h)
        })
        .collect();
    let (bx, by, bw, bh) = bbox_of_rects(&rects);
    let (vw, vh) = session.view.viewport;
    let center = session.view.world(vw / 2.0, vh / 2.0);
    let tx = ((center.0 - bw / 2.0) / PASTE_SNAP).round() * PASTE_SNAP;
    let mut ty = ((center.1 - bh / 2.0) / PASTE_SNAP).round() * PASTE_SNAP;
    while session.data.nodes.iter().any(|node| {
        let (w, h) = display_size(node);
        tx < node.x + w && tx + bw > node.x && ty < node.y + h && ty + bh > node.y
    }) {
        ty += PASTE_SPOT_STEP;
    }
    let (dx, dy) = (tx - bx, ty - by);
    session.push_undo();
    let mut ids = Vec::new();
    for mut node in nodes {
        node.x += dx;
        node.y += dy;
        ids.push(node.id.clone());
        session.data.nodes.push(node);
    }
    session.view.selection = ids;
    session.touch();
    true
}

/// 引用连线：源卡与对话卡之间只保留一条数据流边。
pub fn ensure_edge(session: &mut Session, source: &str, conversation: &str) {
    if session.data.edges.iter().any(|edge| {
        edge.source == source && edge.target == conversation && edge.directed != Some(false)
    }) {
        return;
    }
    session.data.edges.push(CanvasFileEdge {
        id: uuid(),
        source: source.to_string(),
        target: conversation.to_string(),
        source_handle: None,
        target_handle: None,
        directed: None,
        link_mode: None,
        created_at: crate::now_secs(),
    });
}

/// 从连接点拖到对话卡：把来源卡引用写进草稿并登记引用，返回插入后的光标位置。
/// 同一来源卡已登记时只收起候选浮层、返回原光标；卡或对话卡不存在返回 None。
pub fn append_card_ref(session: &mut Session, source: &str, conversation: &str) -> Option<usize> {
    let target = session.node(source).cloned()?;
    let (path, kind) = card_ref_identity(&target);
    let title = crate::chat::card_ref_title(&target);
    let caret = {
        let chat = session.view.card_chat.get_mut(conversation)?;
        if chat
            .mention_refs
            .iter()
            .any(|entry| entry.node_id.as_deref() == Some(source))
        {
            chat.mention_picker = None;
            return Some(chat.caret);
        }
        let taken: Vec<(&str, &str)> = chat
            .mention_refs
            .iter()
            .map(|entry| (entry.path.as_str(), entry.label.as_str()))
            .collect();
        let label = unique_card_label(&title, &taken);
        let tag = format!("#{}：{} ", kind.prefix(), label);
        let at = chat
            .mention_picker
            .as_ref()
            .map_or(chat.caret.min(chat.draft.len()), |p| p.at);
        let caret = insert_tag(&mut chat.draft, at, chat.caret, &tag);
        chat.caret = caret;
        chat.mention_refs.push(crate::refs::RefEntry {
            path,
            label,
            kind,
            node_id: Some(source.to_string()),
        });
        chat.mention_picker = None;
        caret
    };
    ensure_edge(session, source, conversation);
    Some(caret)
}

/// 来源卡引用的路径身份与引用种类（笔记卡 = 路径 + Note；其余 = Card，纯画布卡以节点 id 作路径）。
fn card_ref_identity(node: &CanvasFileNode) -> (String, crate::refs::RefKind) {
    match (
        node.node_type.as_str(),
        node.data.get("file").and_then(Value::as_str),
    ) {
        ("text", Some(file)) => (file.to_string(), crate::refs::RefKind::Note),
        (_, Some(file)) => (file.to_string(), crate::refs::RefKind::Card),
        (_, None) => (node.id.clone(), crate::refs::RefKind::Card),
    }
}

fn unique_card_label(title: &str, taken: &[(&str, &str)]) -> String {
    if !taken.iter().any(|(_, label)| *label == title) {
        return title.to_string();
    }
    for n in 2u32.. {
        let candidate = format!("{title} ({n})");
        if !taken.iter().any(|(_, label)| *label == candidate) {
            return candidate;
        }
    }
    unreachable!("标签序号递增必收敛")
}

/// 用标签替换 `[at, max(caret, at+1))` 区间（吃掉触发的 `#`），返回插入后的光标位置。
pub(crate) fn insert_tag(draft: &mut String, at: usize, caret: usize, tag: &str) -> usize {
    let at = at.min(draft.len());
    let to = caret.max(at + 1).min(draft.len());
    let mut next = String::with_capacity(draft.len() - (to - at) + tag.len());
    next.push_str(&draft[..at]);
    next.push_str(tag);
    next.push_str(&draft[to..]);
    *draft = next;
    at + tag.len()
}

fn uuid() -> String {
    crate::uuid()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn card(id: &str, x: f64, y: f64, w: f64, h: f64) -> CanvasFileNode {
        CanvasFileNode {
            id: id.into(),
            node_type: "text".into(),
            x,
            y,
            width: Some(w),
            height: Some(h),
            data: json!({}),
        }
    }

    fn session_with(nodes: Vec<CanvasFileNode>) -> Session {
        Session::new(
            "a.atlx".into(),
            atelyx_format::canvas::CanvasFile {
                schema: atelyx_format::canvas::CANVAS_SCHEMA.into(),
                id: "c".into(),
                title: "a".into(),
                nodes,
                edges: Vec::new(),
                created_at: 0,
                updated_at: 0,
            },
            1,
        )
    }

    /// 包围盒是 (x, y, w, h)：建组矩形 = 成员范围外扩 GROUP_PAD，宽高不能算成右边界。
    #[test]
    fn group_frame_wraps_selection_bbox() {
        let mut session = session_with(vec![
            card("a", 100.0, 200.0, 200.0, 100.0),
            card("b", 400.0, 300.0, 200.0, 100.0),
        ]);
        session.view.selection = vec!["a".into(), "b".into()];
        let id = create_group_from_selection(&mut session).unwrap();
        let group = session.node(&id).unwrap();
        assert_eq!(group.x, 100.0 - GROUP_PAD);
        assert_eq!(group.y, 200.0 - GROUP_PAD);
        assert_eq!(group.width, Some(500.0 + GROUP_PAD * 2.0));
        assert_eq!(group.height, Some(200.0 + GROUP_PAD * 2.0));
    }
}
