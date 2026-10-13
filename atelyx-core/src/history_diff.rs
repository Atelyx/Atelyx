//! 历史快照差异（表格/画布）：两版快照 JSON 的展示用差异行（纯函数，无 I/O）。
//!
//! 解析失败返回空（历史为尽力而为展示，与 load_history 同口径）。

use std::collections::HashMap;

use serde_json::Value;

use crate::vault::{CanvasFile, TableFile};

// 各类别差异行展示上限（超出截断并追加「还有 N 行」）：字段 / 行 / 单元格与消息。
const FIELD_LINES_CAP: usize = 5;
const ROW_LINES_CAP: usize = 5;
const CELL_LINES_CAP: usize = 8;

/// 表格两版快照的差异行：字段增删改名 / 行增删 / 单元格 from→to（各类别超限截断）。
pub fn table_diff(prev_json: &str, cur_json: &str) -> Vec<String> {
    let (Ok(prev), Ok(cur)) = (
        serde_json::from_str::<TableFile>(prev_json),
        serde_json::from_str::<TableFile>(cur_json),
    ) else {
        return Vec::new();
    };
    let mut out = Vec::new();

    let prev_fields: HashMap<&str, &crate::vault::TableField> =
        prev.fields.iter().map(|f| (f.id.as_str(), f)).collect();
    let mut fields = Vec::new();
    for f in &cur.fields {
        match prev_fields.get(f.id.as_str()) {
            None => fields.push(format!("新增字段「{}」", f.name)),
            Some(p) if p.name != f.name => {
                fields.push(format!("字段「{}」改名为「{}」", p.name, f.name))
            }
            Some(_) => {}
        }
    }
    for p in &prev.fields {
        if !cur.fields.iter().any(|f| f.id == p.id) {
            fields.push(format!("删除字段「{}」", p.name));
        }
    }
    cap_lines(fields, FIELD_LINES_CAP, &mut out);

    let cur_field_names: HashMap<&str, &str> =
        cur.fields.iter().map(|f| (f.id.as_str(), f.name.as_str())).collect();
    let prev_rows: HashMap<&str, &crate::vault::TableRow> =
        prev.rows.iter().map(|r| (r.id.as_str(), r)).collect();
    let mut rows = Vec::new();
    for r in &cur.rows {
        if !prev_rows.contains_key(r.id.as_str()) {
            rows.push(format!("新增行「{}」", row_label(r, &cur_field_names)));
        }
    }
    for p in &prev.rows {
        if !cur.rows.iter().any(|r| r.id == p.id) {
            rows.push(format!("删除行「{}」", row_label(p, &cur_field_names)));
        }
    }
    cap_lines(rows, ROW_LINES_CAP, &mut out);

    let mut cells = Vec::new();
    for r in &cur.rows {
        let Some(p) = prev_rows.get(r.id.as_str()) else { continue };
        for f in &cur.fields {
            let (a, b) = (p.values.get(&f.id), r.values.get(&f.id));
            if !crate::table_ops::cell_value_equal(a, b) {
                cells.push(format!(
                    "「{}」·{}：{} → {}",
                    row_label(r, &cur_field_names),
                    f.name,
                    cell_display(a),
                    cell_display(b),
                ));
            }
        }
    }
    cap_lines(cells, CELL_LINES_CAP, &mut out);
    out
}

/// 画布两版快照的差异行：节点/连线/消息按 id 的增删改（连线两端与消息内容取自同侧快照）。
pub fn canvas_diff(prev_json: &str, cur_json: &str) -> Vec<String> {
    let (Ok(prev), Ok(cur)) = (
        serde_json::from_str::<CanvasFile>(prev_json),
        serde_json::from_str::<CanvasFile>(cur_json),
    ) else {
        return Vec::new();
    };
    let mut out = Vec::new();

    let label_of = |nodes: &[crate::vault::CanvasFileNode], id: &str| {
        nodes
            .iter()
            .find(|n| n.id == id)
            .map(|n| node_label(n))
            .unwrap_or_else(|| "未知节点".to_string())
    };
    let prev_nodes: HashMap<&str, &crate::vault::CanvasFileNode> =
        prev.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut lines = Vec::new();
    for n in &cur.nodes {
        match prev_nodes.get(n.id.as_str()) {
            None => lines.push(format!("新增节点「{}」", node_label(n))),
            // 节点改 = 类型/尺寸/数据变化；坐标平移不算（布局移动非内容变化）
            Some(p) if p.node_type != n.node_type || p.width != n.width || p.height != n.height || p.data != n.data => {
                lines.push(format!("节点「{}」已修改", node_label(n)));
            }
            Some(_) => {}
        }
    }
    for p in &prev.nodes {
        if !cur.nodes.iter().any(|n| n.id == p.id) {
            lines.push(format!("删除节点「{}」", node_label(p)));
        }
    }
    cap_lines(lines, CELL_LINES_CAP, &mut out);

    let mut lines = Vec::new();
    for e in &cur.edges {
        match prev.edges.iter().find(|p| p.id == e.id) {
            None => lines.push(format!(
                "新增连线「{} → {}」",
                label_of(&cur.nodes, &e.source),
                label_of(&cur.nodes, &e.target),
            )),
            Some(p) if p.source != e.source || p.target != e.target => {
                lines.push(format!(
                    "连线「{} → {}」已修改",
                    label_of(&prev.nodes, &p.source),
                    label_of(&prev.nodes, &p.target),
                ));
            }
            Some(_) => {}
        }
    }
    for p in &prev.edges {
        if !cur.edges.iter().any(|e| e.id == p.id) {
            lines.push(format!(
                "删除连线「{} → {}」",
                label_of(&prev.nodes, &p.source),
                label_of(&prev.nodes, &p.target),
            ));
        }
    }
    cap_lines(lines, CELL_LINES_CAP, &mut out);

    let (prev_msgs, cur_msgs) = (messages_of(prev_json), messages_of(cur_json));
    let label = |m: &Value| {
        m.get("text")
            .or_else(|| m.get("content"))
            .and_then(Value::as_str)
            .or_else(|| m.get("id").and_then(Value::as_str))
            .unwrap_or("消息")
            .to_string()
    };
    let mut lines = Vec::new();
    for m in &cur_msgs {
        let id = m.get("id").and_then(Value::as_str).unwrap_or_default();
        match prev_msgs.iter().find(|p| p.get("id").and_then(Value::as_str) == Some(id)) {
            None => lines.push(format!("新增消息「{}」", label(m))),
            Some(p) if p != m => lines.push(format!("消息「{}」已修改", label(m))),
            Some(_) => {}
        }
    }
    for p in &prev_msgs {
        let id = p.get("id").and_then(Value::as_str).unwrap_or_default();
        if !cur_msgs.iter().any(|m| m.get("id").and_then(Value::as_str) == Some(id)) {
            lines.push(format!("删除消息「{}」", label(p)));
        }
    }
    cap_lines(lines, CELL_LINES_CAP, &mut out);
    out
}

/// 画布快照的规模计数：(节点数, 连线数, 消息数)。消息存于顶层 `messages` 数组
/// （画布格式约定，带稳定 id；类型化结构未覆盖该字段，按原始 JSON 计数）。
pub fn canvas_summary(json: &str) -> (usize, usize, usize) {
    let nodes = serde_json::from_str::<CanvasFile>(json)
        .map(|c| (c.nodes.len(), c.edges.len()))
        .unwrap_or((0, 0));
    (nodes.0, nodes.1, messages_of(json).len())
}

/// 顶层 messages 数组（缺失或非数组 = 空）。
fn messages_of(json: &str) -> Vec<Value> {
    serde_json::from_str::<Value>(json)
        .ok()
        .and_then(|v| v.get("messages").and_then(Value::as_array).cloned())
        .unwrap_or_default()
}

/// 差异行截断：超上限截断并追加「还有 N 行」。
fn cap_lines(lines: Vec<String>, cap: usize, out: &mut Vec<String>) {
    if lines.len() > cap {
        out.extend(lines[..cap].iter().cloned());
        out.push(format!("还有 {} 行", lines.len() - cap));
    } else {
        out.extend(lines);
    }
}

/// 行展示标签：按字段顺序取第一个非空文本值，缺省「（无标题）」。
fn row_label(row: &crate::vault::TableRow, field_names: &HashMap<&str, &str>) -> String {
    row.values
        .iter()
        .filter(|(k, v)| v.as_str().is_some_and(|s| !s.trim().is_empty()) && field_names.contains_key(k.as_str()))
        .min_by_key(|(k, _)| field_names.get(k.as_str()).copied().unwrap_or_default())
        .and_then(|(_, v)| v.as_str())
        .unwrap_or("（无标题）")
        .to_string()
}

/// 节点展示标签：data.text / data.title / 节点类型。
fn node_label(n: &crate::vault::CanvasFileNode) -> String {
    n.data
        .get("text")
        .or_else(|| n.data.get("title"))
        .and_then(Value::as_str)
        .unwrap_or(&n.node_type)
        .to_string()
}

/// 单元格展示串（缺键/空 = 「空」；图片 = 「[图片]」；超长截断）。
fn cell_display(v: Option<&Value>) -> String {
    const MAX: usize = 24;
    let text = match v {
        None => "空".to_string(),
        Some(v) => match v {
            Value::String(s) if s.is_empty() => "空".to_string(),
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            v if v.get("images").and_then(Value::as_array).is_some_and(|a| !a.is_empty()) => {
                format!("[{} 张图片]", v["images"].as_array().unwrap().len())
            }
            other => other.to_string(),
        },
    };
    if text.chars().count() > MAX {
        let head: String = text.chars().take(MAX).collect();
        format!("{head}…")
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_json(fields: &[(&str, &str)], rows: &[(&str, Value)]) -> String {
        let fields: Vec<Value> = fields
            .iter()
            .map(|(id, name)| serde_json::json!({"id": id, "name": name, "type": "text"}))
            .collect();
        let rows: Vec<Value> = rows
            .iter()
            .map(|(id, values)| serde_json::json!({"id": id, "values": values}))
            .collect();
        serde_json::json!({
            "schema": crate::vault::TABLE_SCHEMA,
            "id": "t1",
            "title": "表",
            "fields": fields,
            "rows": rows,
        })
        .to_string()
    }

    fn canvas_json(nodes: Value, edges: Value, messages: Value) -> String {
        serde_json::json!({
            "schema": crate::vault::CANVAS_SCHEMA,
            "id": "c1",
            "title": "画布",
            "nodes": nodes,
            "edges": edges,
            "messages": messages,
        })
        .to_string()
    }

    #[test]
    fn table_diff_field_add_rename_remove() {
        let prev = table_json(&[("f1", "名称"), ("f2", "旧名")], &[("r1", serde_json::json!({}))]);
        let cur = table_json(
            &[("f1", "名称"), ("f2", "新名"), ("f3", "补充")],
            &[("r1", serde_json::json!({}))],
        );
        assert_eq!(
            table_diff(&prev, &cur),
            vec!["字段「旧名」改名为「新名」".to_string(), "新增字段「补充」".to_string()]
        );
        // 反向：删除字段 + 名称随方向回退
        assert_eq!(
            table_diff(&cur, &prev),
            vec!["字段「新名」改名为「旧名」".to_string(), "删除字段「补充」".to_string()]
        );
        // 同内容 = 无差异
        assert!(table_diff(&prev, &prev).is_empty());
    }

    #[test]
    fn table_diff_rows_and_cells() {
        let prev = table_json(
            &[("f1", "名称"), ("f2", "备注")],
            &[("r1", serde_json::json!({"f1": "甲", "f2": "旧"}))],
        );
        let cur = table_json(
            &[("f1", "名称"), ("f2", "备注")],
            &[
                ("r1", serde_json::json!({"f1": "甲", "f2": "新"})),
                ("r2", serde_json::json!({"f1": "乙"})),
            ],
        );
        // 行标签取第一个非空文本字段（名称），单元格行带 from→to
        assert_eq!(
            table_diff(&prev, &cur),
            vec![
                "新增行「乙」".to_string(),
                "「甲」·备注：旧 → 新".to_string(),
            ]
        );
        // 删除行 + 单元格变化随方向回退
        assert_eq!(
            table_diff(&cur, &prev),
            vec!["删除行「乙」".to_string(), "「甲」·备注：新 → 旧".to_string()]
        );
    }

    #[test]
    fn table_diff_caps_each_category() {
        let fields: Vec<(&str, &str)> = (0..7).map(|i| (format!("x{i}").leak() as &str, format!("字段{i}").leak() as &str)).collect();
        let rows: Vec<(&str, Value)> = (0..7)
            .map(|i| ((format!("r{i}").leak() as &str), serde_json::json!({})))
            .collect();
        let prev = table_json(&[], &[]);
        let cur = table_json(&fields, &rows);
        let diff = table_diff(&prev, &cur);
        // 字段 5 行 + 还有 2 行；行 5 行 + 还有 2 行
        assert_eq!(&diff[..5], &diff[..5]);
        assert_eq!(diff[5], "还有 2 行");
        assert_eq!(diff[6], "新增行「（无标题）」");
        assert_eq!(diff[11], "还有 2 行");
        assert_eq!(diff.len(), 12);
    }

    #[test]
    fn table_diff_cell_cap_and_image_and_empty() {
        let mut cur_values = serde_json::Map::new();
        for i in 0..10 {
            cur_values.insert(format!("f{i}"), serde_json::json!(format!("值{i}")));
        }
        let fields: Vec<(&str, &str)> = (0..10).map(|i| (format!("f{i}").leak() as &str, format!("字段{i}").leak() as &str)).collect();
        let prev = table_json(&fields, &[("r1", serde_json::json!({}))]);
        let cur = table_json(&fields, &[("r1", Value::Object(cur_values))]);
        let diff = table_diff(&prev, &cur);
        // 单元格 8 行上限 + 还有 2 行
        assert_eq!(diff.len(), 9);
        assert_eq!(diff[8], "还有 2 行");
        assert!(diff[0].ends_with("：空 → 值0"));
    }

    #[test]
    fn table_diff_unparseable_returns_empty() {
        assert!(table_diff("not json", "{}").is_empty());
        assert!(table_diff("{}", "not json").is_empty());
    }

    #[test]
    fn canvas_diff_nodes_edges() {
        let node = |id: &str, text: &str| serde_json::json!({"id": id, "type": "text", "x": 0.0, "y": 0.0, "data": {"text": text}});
        let prev = canvas_json(
            serde_json::json!([node("n1", "甲"), node("n2", "乙")]),
            serde_json::json!([{"id": "e1", "source": "n1", "target": "n2"}]),
            serde_json::json!([]),
        );
        // n1 改数据、n3 新增、e1 删除、e2 新增；坐标平移不算修改（n2 保留）
        let mut moved = node("n1", "甲改");
        moved["x"] = serde_json::json!(50.0);
        let cur = canvas_json(
            serde_json::json!([moved, node("n2", "乙"), node("n3", "丙")]),
            serde_json::json!([{"id": "e2", "source": "n1", "target": "n3"}]),
            serde_json::json!([]),
        );
        assert_eq!(
            canvas_diff(&prev, &cur),
            vec![
                "节点「甲改」已修改".to_string(),
                "新增节点「丙」".to_string(),
                "新增连线「甲改 → 丙」".to_string(),
                "删除连线「甲 → 乙」".to_string(),
            ]
        );
        // 纯坐标移动 = 无差异
        let same_but_moved = canvas_json(
            {
                let mut n = node("n1", "甲");
                n["x"] = serde_json::json!(99.0);
                serde_json::json!([n, node("n2", "乙")])
            },
            serde_json::json!([{"id": "e1", "source": "n1", "target": "n2"}]),
            serde_json::json!([]),
        );
        assert!(canvas_diff(&prev, &same_but_moved).is_empty());
    }

    #[test]
    fn canvas_diff_messages_by_id() {
        let base = serde_json::json!([]);
        let prev = canvas_json(base.clone(), serde_json::json!([]), serde_json::json!([{"id": "m1", "text": "你好"}]));
        let cur = canvas_json(
            base,
            serde_json::json!([]),
            serde_json::json!([{"id": "m1", "text": "改了"}, {"id": "m2", "text": "新消息"}]),
        );
        assert_eq!(
            canvas_diff(&prev, &cur),
            vec!["消息「改了」已修改".to_string(), "新增消息「新消息」".to_string()]
        );
    }

    #[test]
    fn canvas_summary_counts_all_three() {
        let json = canvas_json(
            serde_json::json!([
                {"id": "n1", "type": "text", "x": 0.0, "y": 0.0, "data": {}},
                {"id": "n2", "type": "text", "x": 1.0, "y": 1.0, "data": {}}
            ]),
            serde_json::json!([{"id": "e1", "source": "n1", "target": "n2"}]),
            serde_json::json!([{"id": "m1"}, {"id": "m2"}]),
        );
        assert_eq!(canvas_summary(&json), (2, 1, 2));
        assert_eq!(canvas_summary("not json"), (0, 0, 0));
        // 无 messages 键 = 0
        let bare = serde_json::json!({"schema": crate::vault::CANVAS_SCHEMA, "id": "c", "title": "t", "nodes": [], "edges": []});
        assert_eq!(canvas_summary(&bare.to_string()), (0, 0, 0));
    }
}
