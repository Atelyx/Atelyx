//! 画布格式语义：`.atlx` 模型、按 id 的增量补丁合并、远端合入。零 I/O，宿主与插件共用一份。

pub mod collab;
pub mod geometry;

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

/// `.atlx` 画布文件 schema 标识。
pub const CANVAS_SCHEMA: &str = "atelyx-canvas/v1";

/// `.atlx` 画布文件。`data` 用 JSON 值承载卡内容，格式层不解释业务字段。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CanvasFile {
    #[serde(default)]
    pub schema: String,
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub nodes: Vec<CanvasFileNode>,
    #[serde(default)]
    pub edges: Vec<CanvasFileEdge>,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CanvasFileNode {
    pub id: String,
    #[serde(rename = "type")]
    pub node_type: String,
    pub x: f64,
    pub y: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<f64>,
    pub data: serde_json::Value,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CanvasFileEdge {
    pub id: String,
    pub source: String,
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_handle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_handle: Option<String>,
    /// false = 关联边（无消费语义）；缺省 = 数据流边。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directed: Option<bool>,
    /// 关联边的箭头模式（"none" | "single" | "double"，仅 directed: false 生效；缺省 = 无向）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_mode: Option<String>,
    #[serde(default)]
    pub created_at: i64,
}

/// 画布增量保存补丁：只含变化/新增/删除的实体，按稳定 id 合并——
/// removed 幂等（缺 id 不报错），upsert 覆盖同 id 或追加。
#[derive(Serialize, Deserialize, Default, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CanvasPatch {
    /// 画布 id（防串文件守卫）。
    pub id: String,
    /// 标题（线形状保留：合并端按此应用标题）。文件名是唯一真源，内容保存不携带——
    /// 改名只走重命名端点，否则陈旧标题会把文件改回旧名。
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub upsert_nodes: Vec<CanvasFileNode>,
    #[serde(default)]
    pub removed_node_ids: Vec<String>,
    #[serde(default)]
    pub upsert_edges: Vec<CanvasFileEdge>,
    #[serde(default)]
    pub removed_edge_ids: Vec<String>,
}

/// 计算画布增量补丁：与落盘基线按值 diff，只含变化/新增/删除的卡与连线。
/// 卡坐标/尺寸/data 任一变化即整卡 upsert（补丁粒度 = 整卡）；数组顺序无业务语义。
/// 无任何变化返回 None（调用方跳过写盘）。title 变化不随补丁携带（文件名是唯一真源）。
pub fn compute_canvas_patch(baseline: &CanvasFile, current: &CanvasFile) -> Option<CanvasPatch> {
    let last_nodes: HashMap<&str, &CanvasFileNode> =
        baseline.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let last_edges: HashMap<&str, &CanvasFileEdge> =
        baseline.edges.iter().map(|e| (e.id.as_str(), e)).collect();
    let upsert_nodes: Vec<CanvasFileNode> = current
        .nodes
        .iter()
        .filter(|n| last_nodes.get(n.id.as_str()).copied() != Some(*n))
        .cloned()
        .collect();
    let current_node_ids: HashSet<&str> = current.nodes.iter().map(|n| n.id.as_str()).collect();
    let removed_node_ids: Vec<String> = baseline
        .nodes
        .iter()
        .filter(|n| !current_node_ids.contains(n.id.as_str()))
        .map(|n| n.id.clone())
        .collect();
    let upsert_edges: Vec<CanvasFileEdge> = current
        .edges
        .iter()
        .filter(|e| last_edges.get(e.id.as_str()).copied() != Some(*e))
        .cloned()
        .collect();
    let current_edge_ids: HashSet<&str> = current.edges.iter().map(|e| e.id.as_str()).collect();
    let removed_edge_ids: Vec<String> = baseline
        .edges
        .iter()
        .filter(|e| !current_edge_ids.contains(e.id.as_str()))
        .map(|e| e.id.clone())
        .collect();
    if upsert_nodes.is_empty()
        && removed_node_ids.is_empty()
        && upsert_edges.is_empty()
        && removed_edge_ids.is_empty()
    {
        return None;
    }
    Some(CanvasPatch {
        id: current.id.clone(),
        title: None,
        upsert_nodes,
        removed_node_ids,
        upsert_edges,
        removed_edge_ids,
    })
}

/// 按稳定 id 合并画布补丁到全量文件（removed 幂等；upsert 覆盖同 id 或追加）。
/// 调用方须先校验 `patch.id` 与画布 id 一致。
pub fn apply_canvas_patch(canvas: &mut CanvasFile, patch: &CanvasPatch) {
    if let Some(title) = &patch.title {
        canvas.title = title.clone();
    }
    let removed_nodes: HashSet<&String> = patch.removed_node_ids.iter().collect();
    canvas.nodes.retain(|n| !removed_nodes.contains(&n.id));
    for n in &patch.upsert_nodes {
        match canvas.nodes.iter_mut().find(|x| x.id == n.id) {
            Some(existing) => *existing = n.clone(),
            None => canvas.nodes.push(n.clone()),
        }
    }
    let removed_edges: HashSet<&String> = patch.removed_edge_ids.iter().collect();
    canvas.edges.retain(|e| !removed_edges.contains(&e.id));
    for e in &patch.upsert_edges {
        match canvas.edges.iter_mut().find(|x| x.id == e.id) {
            Some(existing) => *existing = e.clone(),
            None => canvas.edges.push(e.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(id: &str, x: f64) -> CanvasFileNode {
        CanvasFileNode {
            id: id.into(),
            node_type: "text".into(),
            x,
            y: 0.0,
            width: None,
            height: None,
            data: json!({}),
        }
    }

    fn canvas(nodes: Vec<CanvasFileNode>) -> CanvasFile {
        CanvasFile {
            schema: CANVAS_SCHEMA.into(),
            id: "c1".into(),
            title: "画布".into(),
            nodes,
            edges: vec![],
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn patch_carries_changed_and_removed_nodes() {
        let base = canvas(vec![node("a", 0.0), node("b", 10.0)]);
        let next = canvas(vec![node("a", 5.0), node("c", 20.0)]);
        let patch = compute_canvas_patch(&base, &next).expect("有变化");
        assert_eq!(patch.upsert_nodes.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(), ["a", "c"]);
        assert_eq!(patch.removed_node_ids, ["b"]);
        assert!(patch.title.is_none());
    }

    #[test]
    fn identical_canvas_has_no_patch() {
        let base = canvas(vec![node("a", 0.0)]);
        assert!(compute_canvas_patch(&base, &base).is_none());
    }

    #[test]
    fn apply_patch_is_idempotent_and_ordered() {
        let mut target = canvas(vec![node("a", 0.0)]);
        let patch = CanvasPatch {
            id: "c1".into(),
            title: None,
            upsert_nodes: vec![node("a", 9.0), node("b", 4.0)],
            removed_node_ids: vec!["missing".into()],
            upsert_edges: vec![],
            removed_edge_ids: vec!["missing".into()],
        };
        apply_canvas_patch(&mut target, &patch);
        apply_canvas_patch(&mut target, &patch);
        assert_eq!(target.nodes.len(), 2);
        assert_eq!(target.nodes[0].x, 9.0);
        assert_eq!(target.nodes[1].id, "b");
    }
}
