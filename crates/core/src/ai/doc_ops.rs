//! AI 表格 / 画布操作：工具层操作（`TableOp` / `CanvasOp`）应用到领域模型（纯函数，无 I/O）。
//! 一次调用 = 一次编辑 = 一个补丁：操作按稳定 id 定位，整批成功才由调用方提交。

use serde_json::{json, Value};

use crate::ai::edit_host::{CanvasOp, TableOp};
use crate::table_ops;
use crate::vault::{CanvasFile, CanvasFileEdge, CanvasFileNode, TableField, TableFile, TableRow};

/// 新卡自动落位：网格取整与卡间距与画布手动建卡同口径。
const SNAP_GRID: f64 = 16.0;
const CARD_GAP: f64 = 40.0;
/// 现有卡未写 width 时的显示宽（估算右缘用；与画布缺省卡宽一致）。
const DEFAULT_CARD_W: f64 = 360.0;
/// 文本卡标题取正文首行的字符上限（与画布自动落卡一致）。
const TITLE_MAX_CHARS: usize = 16;

/// 应用一批表格操作，返回受影响的行 id（新增 / 修改 / 删除各记一次）。
/// 任一项失败即返回可读错误；调用方在克隆上应用，失败丢弃（不产生部分写入）。
pub fn apply_table_ops(table: &mut TableFile, ops: &[TableOp]) -> Result<Vec<String>, String> {
    if ops.is_empty() {
        return Err("ops 不能为空".to_string());
    }
    let mut touched: Vec<String> = Vec::new();
    for (i, op) in ops.iter().enumerate() {
        let at = i + 1;
        match op {
            TableOp::SetCell { row_id, field_id, value } => {
                let field = table
                    .fields
                    .iter()
                    .find(|f| &f.id == field_id)
                    .ok_or_else(|| format!("第 {at} 项操作：字段 id 不存在：{field_id}（先用 table_read 查看字段）"))?;
                let next = coerce_cell_value(field, value).map_err(|e| format!("第 {at} 项操作：{e}"))?;
                let row = table
                    .rows
                    .iter_mut()
                    .find(|r| &r.id == row_id)
                    .ok_or_else(|| format!("第 {at} 项操作：行 id 不存在：{row_id}（先用 table_read 查看行）"))?;
                table_ops::set_cell_value(row, field_id, next);
                push_once(&mut touched, row_id);
            }
            TableOp::AddRow { cells } => {
                let mut row = TableRow {
                    id: nanoid::nanoid!(),
                    values: Default::default(),
                    height: None,
                    styles: None,
                    ai: None,
                };
                for (field_id, value) in cells {
                    let field = table
                        .fields
                        .iter()
                        .find(|f| &f.id == field_id)
                        .ok_or_else(|| format!("第 {at} 项操作：字段 id 不存在：{field_id}"))?;
                    let next = coerce_cell_value(field, value).map_err(|e| format!("第 {at} 项操作：{e}"))?;
                    if let Some(v) = next {
                        row.values.insert(field_id.clone(), v);
                    }
                }
                touched.push(row.id.clone());
                table.rows.push(row);
            }
            TableOp::DeleteRows { row_ids } => {
                if row_ids.is_empty() {
                    return Err(format!("第 {at} 项操作：rowIds 不能为空"));
                }
                // 与 set_cell 同口径：任一 id 不存在即整批报错（不静默漏删）
                let missing: Vec<&str> = row_ids
                    .iter()
                    .filter(|id| !table.rows.iter().any(|r| &r.id == *id))
                    .map(String::as_str)
                    .collect();
                if !missing.is_empty() {
                    return Err(format!(
                        "第 {at} 项操作：行 id 不存在：{}（先用 table_read 查看行）",
                        missing.join("、")
                    ));
                }
                table.rows.retain(|r| !row_ids.contains(&r.id));
                for id in row_ids {
                    push_once(&mut touched, id);
                }
            }
        }
    }
    Ok(touched)
}

/// 工具值 → 磁盘值形态（null / 空文本 = 清空）；按字段类型校验，越界给可读原因。
fn coerce_cell_value(field: &TableField, value: &Value) -> Result<Option<Value>, String> {
    // AI 字段先拒：null 也属写入（清空），不能走「null = 清空」的通用捷径
    if field.field_type == "ai" {
        return Err(format!(
            "字段「{}」是 AI 字段，不能直接写入；请改字段设置后重新生成",
            field.name
        ));
    }
    if value.is_null() {
        return Ok(None);
    }
    match field.field_type.as_str() {
        "image" => Err(format!("字段「{}」是图片字段，工具不能写入图片", field.name)),
        "number" | "duration" => {
            let number = match value {
                Value::Number(_) => value.as_f64(),
                Value::String(s) => s.trim().parse::<f64>().ok(),
                _ => None,
            }
            .filter(|n| n.is_finite());
            number
                .map(|n| Some(table_ops::number_value(n)))
                .ok_or_else(|| format!("字段「{}」需要数字", field.name))
        }
        "singleSelect" => {
            let option = value
                .as_str()
                .ok_or_else(|| format!("字段「{}」需要文本选项", field.name))?;
            let known = field
                .options
                .as_ref()
                .is_some_and(|options| options.iter().any(|o| o == option));
            if !known {
                let options = field.options.as_deref().unwrap_or(&[]).join("、");
                return Err(format!("字段「{}」的可选值：{options}（要给新选项请先在表格里添加）", field.name));
            }
            Ok(Some(Value::String(option.to_string())))
        }
        _ => {
            let text = value
                .as_str()
                .ok_or_else(|| format!("字段「{}」需要文本", field.name))?;
            Ok((!text.is_empty()).then(|| Value::String(text.to_string())))
        }
    }
}

/// 应用一批画布操作，返回新增 / 修改的节点与连线 id。
/// 任一项失败即返回可读错误；调用方在克隆上应用，失败丢弃（不产生部分写入）。
pub fn apply_canvas_ops(canvas: &mut CanvasFile, ops: &[CanvasOp]) -> Result<Vec<String>, String> {
    if ops.is_empty() {
        return Err("ops 不能为空".to_string());
    }
    let mut touched: Vec<String> = Vec::new();
    for (i, op) in ops.iter().enumerate() {
        let at = i + 1;
        match op {
            CanvasOp::AddTextCard { text, x, y } => {
                let (x, y) = place(canvas, *x, *y);
                let node = CanvasFileNode {
                    id: nanoid::nanoid!(),
                    node_type: "text".to_string(),
                    x,
                    y,
                    width: None,
                    height: None,
                    data: json!({ "title": title_of_text(text), "bodyMd": text }),
                };
                touched.push(node.id.clone());
                canvas.nodes.push(node);
            }
            CanvasOp::AddNoteCard { path, x, y } => {
                if !path.to_ascii_lowercase().ends_with(".md") {
                    return Err(format!("第 {at} 项操作：笔记卡需要 .md 文件路径：{path}"));
                }
                let (x, y) = place(canvas, *x, *y);
                let node = CanvasFileNode {
                    id: nanoid::nanoid!(),
                    node_type: "text".to_string(),
                    x,
                    y,
                    width: None,
                    height: None,
                    data: json!({ "title": file_stem(path), "file": path }),
                };
                touched.push(node.id.clone());
                canvas.nodes.push(node);
            }
            CanvasOp::Connect { from, to, kind } => {
                for id in [from, to] {
                    ensure_node(canvas, id).map_err(|e| format!("第 {at} 项操作：{e}"))?;
                }
                let (directed, link_mode) = match kind.as_deref() {
                    None | Some("data") => (None, None),
                    Some("link") => (Some(false), None),
                    Some(other) => {
                        return Err(format!("第 {at} 项操作：未知连线类型 {other}（data / link）"));
                    }
                };
                // 同端点连线已存在 = 幂等跳过（不叠重复边）
                if canvas.edges.iter().any(|e| &e.source == from && &e.target == to) {
                    continue;
                }
                let edge = CanvasFileEdge {
                    id: nanoid::nanoid!(),
                    source: from.clone(),
                    target: to.clone(),
                    source_handle: None,
                    target_handle: None,
                    directed,
                    link_mode,
                    created_at: now_secs(),
                };
                touched.push(edge.id.clone());
                canvas.edges.push(edge);
            }
            CanvasOp::UpdateText { node_id, text } => {
                let node = canvas
                    .nodes
                    .iter_mut()
                    .find(|n| &n.id == node_id)
                    .ok_or_else(|| format!("第 {at} 项操作：节点 id 不存在：{node_id}"))?;
                if node.node_type != "text" {
                    return Err(format!("第 {at} 项操作：节点 {node_id} 不是文本卡（类型 {}）", node.node_type));
                }
                if node.data.get("file").is_some() {
                    return Err(format!("第 {at} 项操作：节点 {node_id} 是笔记引用卡，正文请用笔记工具改"));
                }
                if let Some(obj) = node.data.as_object_mut() {
                    obj.insert("bodyMd".to_string(), Value::String(text.clone()));
                    obj.insert("title".to_string(), Value::String(title_of_text(text)));
                }
                push_once(&mut touched, node_id);
            }
            CanvasOp::DeleteNode { node_id } => {
                let before = canvas.nodes.len();
                canvas.nodes.retain(|n| &n.id != node_id);
                if canvas.nodes.len() == before {
                    return Err(format!("第 {at} 项操作：节点 id 不存在：{node_id}"));
                }
                canvas.edges.retain(|e| &e.source != node_id && &e.target != node_id);
                push_once(&mut touched, node_id);
            }
        }
    }
    Ok(touched)
}

fn ensure_node(canvas: &CanvasFile, node_id: &str) -> Result<(), String> {
    if canvas.nodes.iter().any(|n| n.id == node_id) {
        Ok(())
    } else {
        Err(format!("节点 id 不存在：{node_id}"))
    }
}

/// 新卡落位：给定坐标按网格取整；缺省放在现有卡右侧空白处（避开内容区）。
fn place(canvas: &CanvasFile, x: Option<f64>, y: Option<f64>) -> (f64, f64) {
    match (x, y) {
        (Some(x), Some(y)) => (snap(x), snap(y)),
        _ => {
            let right = canvas
                .nodes
                .iter()
                .filter(|n| n.node_type != "group")
                .map(|n| n.x + n.width.unwrap_or(DEFAULT_CARD_W))
                .fold(None, |acc: Option<f64>, edge| Some(acc.map_or(edge, |m| m.max(edge))));
            let top = canvas
                .nodes
                .iter()
                .filter(|n| n.node_type != "group")
                .map(|n| n.y)
                .fold(None, |acc: Option<f64>, y| Some(acc.map_or(y, |m| m.min(y))));
            match (right, top) {
                (Some(right), Some(top)) => (snap(right + CARD_GAP), snap(top)),
                _ => (0.0, 0.0),
            }
        }
    }
}

fn snap(v: f64) -> f64 {
    (v / SNAP_GRID).round() * SNAP_GRID
}

/// 文本卡标题：正文首行前 N 字符（空正文回落「文本」）。
fn title_of_text(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    let title: String = line.chars().take(TITLE_MAX_CHARS).collect();
    if title.is_empty() {
        "文本".to_string()
    } else {
        title
    }
}

/// 路径主名（画布笔记卡标题口径与壳侧一致：末段去扩展名）。
fn file_stem(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(name).to_string()
}

fn push_once(ids: &mut Vec<String>, id: &str) {
    if !ids.iter().any(|x| x == id) {
        ids.push(id.to_string());
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::{TABLE_SCHEMA, CANVAS_SCHEMA};

    fn table() -> TableFile {
        TableFile {
            schema: TABLE_SCHEMA.into(),
            id: "t1".into(),
            title: "任务".into(),
            fields: vec![
                TableField { id: "f1".into(), name: "名称".into(), field_type: "text".into(), options: None, width: None, calc_type: None, ai: None },
                TableField { id: "f2".into(), name: "状态".into(), field_type: "singleSelect".into(), options: Some(vec!["待办".into(), "完成".into()]), width: None, calc_type: None, ai: None },
                TableField { id: "f3".into(), name: "工时".into(), field_type: "number".into(), options: None, width: None, calc_type: None, ai: None },
            ],
            rows: vec![TableRow { id: "r1".into(), values: Default::default(), height: None, styles: None, ai: None }],
            created_at: 0,
            updated_at: 0,
        }
    }

    fn canvas() -> CanvasFile {
        CanvasFile {
            schema: CANVAS_SCHEMA.into(),
            id: "c1".into(),
            title: "画布".into(),
            nodes: vec![],
            edges: vec![],
            created_at: 0,
            updated_at: 0,
        }
    }


    #[test]
    fn ai_field_rejects_null_and_text_writes() {
        let mut t = table();
        t.fields.push(TableField {
            id: "f4".into(),
            name: "提示词".into(),
            field_type: "ai".into(),
            options: None,
            width: None,
            calc_type: None,
            ai: Some(Default::default()),
        });
        for value in [json!(null), json!("手写")] {
            let err = apply_table_ops(
                &mut t,
                &[TableOp::SetCell { row_id: "r1".into(), field_id: "f4".into(), value }],
            )
            .expect_err("AI 字段应拒绝写入");
            assert!(err.contains("AI 字段"), "{err}");
        }
        assert!(t.rows[0].values.get("f4").is_none());
    }

    #[test]
    fn table_ops_write_cells_add_and_delete_rows() {
        let mut t = table();
        let ops = vec![
            TableOp::SetCell { row_id: "r1".into(), field_id: "f1".into(), value: json!("写文档") },
            TableOp::SetCell { row_id: "r1".into(), field_id: "f3".into(), value: json!(2.5) },
            TableOp::AddRow { cells: vec![("f2".into(), json!("完成"))] },
        ];
        let touched = apply_table_ops(&mut t, &ops).unwrap();
        assert_eq!(t.rows[0].values["f1"], json!("写文档"));
        assert_eq!(t.rows[0].values["f3"], json!(2.5));
        assert_eq!(t.rows.len(), 2);
        assert_eq!(t.rows[1].values["f2"], json!("完成"));
        assert_eq!(touched, vec!["r1".to_string(), t.rows[1].id.clone()]);
        // 删除：按稳定 id，未知 id 报错
        let ops = vec![TableOp::DeleteRows { row_ids: vec![t.rows[1].id.clone()] }];
        apply_table_ops(&mut t, &ops).unwrap();
        assert_eq!(t.rows.len(), 1);
        let err = apply_table_ops(&mut t, &[TableOp::DeleteRows { row_ids: vec!["nope".into()] }]).unwrap_err();
        assert!(err.contains("不存在"), "{err}");
    }

    #[test]
    fn table_ops_reject_bad_ids_and_values() {
        let mut t = table();
        for (op, want) in [
            (TableOp::SetCell { row_id: "nope".into(), field_id: "f1".into(), value: json!("x") }, "行 id 不存在"),
            (TableOp::SetCell { row_id: "r1".into(), field_id: "nope".into(), value: json!("x") }, "字段 id 不存在"),
            (TableOp::SetCell { row_id: "r1".into(), field_id: "f3".into(), value: json!("abc") }, "需要数字"),
            (TableOp::SetCell { row_id: "r1".into(), field_id: "f2".into(), value: json!("未定义") }, "可选值"),
        ] {
            let err = apply_table_ops(&mut t, &[op]).unwrap_err();
            assert!(err.contains(want), "期望含「{want}」，实际：{err}");
        }
        // 数字文本按数字落盘；null 清空
        apply_table_ops(&mut t, &[TableOp::SetCell { row_id: "r1".into(), field_id: "f3".into(), value: json!("3") }]).unwrap();
        assert_eq!(t.rows[0].values["f3"], json!(3));
        apply_table_ops(&mut t, &[TableOp::SetCell { row_id: "r1".into(), field_id: "f3".into(), value: Value::Null }]).unwrap();
        assert!(!t.rows[0].values.contains_key("f3"));
    }

    #[test]
    fn canvas_ops_add_connect_update_and_delete() {
        let mut c = canvas();
        let ops = vec![
            CanvasOp::AddTextCard { text: "第一行\n正文".into(), x: Some(10.0), y: Some(20.0) },
            CanvasOp::AddTextCard { text: "自动落位".into(), x: None, y: None },
            CanvasOp::AddNoteCard { path: "笔记/方案.md".into(), x: None, y: None },
        ];
        let ids = apply_canvas_ops(&mut c, &ops).unwrap();
        assert_eq!(ids.len(), 3);
        assert_eq!(c.nodes[0].data["title"], json!("第一行"));
        assert_eq!(c.nodes[0].data["bodyMd"], json!("第一行\n正文"));
        assert_eq!((c.nodes[0].x, c.nodes[0].y), (16.0, 16.0), "坐标按 16px 网格取整");
        // 自动落位在现有卡右侧
        assert!(c.nodes[1].x > c.nodes[0].x);
        assert_eq!(c.nodes[2].data["file"], json!("笔记/方案.md"));
        assert_eq!(c.nodes[2].data["title"], json!("方案"));
        // 连线（数据流 / 关联）、重复连线幂等
        let (a, b) = (c.nodes[0].id.clone(), c.nodes[1].id.clone());
        let edges = apply_canvas_ops(&mut c, &[CanvasOp::Connect { from: a.clone(), to: b.clone(), kind: None }]).unwrap();
        assert_eq!(edges.len(), 1);
        apply_canvas_ops(&mut c, &[CanvasOp::Connect { from: a.clone(), to: b.clone(), kind: Some("link".into()) }]).unwrap();
        assert_eq!(c.edges.len(), 1);
        assert!(apply_canvas_ops(&mut c, &[CanvasOp::Connect { from: a.clone(), to: "nope".into(), kind: None }]).is_err());
        // 改正文（标题随正文）；引用卡拒写
        apply_canvas_ops(&mut c, &[CanvasOp::UpdateText { node_id: a.clone(), text: "改后".into() }]).unwrap();
        assert_eq!(c.nodes[0].data["bodyMd"], json!("改后"));
        let note = c.nodes[2].id.clone();
        assert!(apply_canvas_ops(&mut c, &[CanvasOp::UpdateText { node_id: note, text: "x".into() }]).is_err());
        // 删除节点连带删连线
        apply_canvas_ops(&mut c, &[CanvasOp::DeleteNode { node_id: b }]).unwrap();
        assert_eq!(c.nodes.len(), 2);
        assert!(c.edges.is_empty());
        assert!(apply_canvas_ops(&mut c, &[CanvasOp::DeleteNode { node_id: "nope".into() }]).is_err());
    }

    #[test]
    fn empty_ops_are_rejected() {
        assert!(apply_table_ops(&mut table(), &[]).is_err());
        assert!(apply_canvas_ops(&mut canvas(), &[]).is_err());
    }
}
