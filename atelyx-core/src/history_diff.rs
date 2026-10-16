//! 历史快照差异（笔记/表格/画布）：两版快照的展示用差异行与记录用摘要（纯函数，无 I/O）。
//!
//! 解析失败按空处理：差异列表为空，摘要按「新建」或空串降级（历史为尽力而为展示，坏快照不中断弹层）。

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::vault::{CanvasFile, TableField, TableFile, TableRow};

// 各类别差异行展示上限（超出截断并追加「还有 N …」）：字段 / 行 / 单元格 / 节点 / 连线 / 消息。
const FIELD_LINES_CAP: usize = 5;
const ROW_LINES_CAP: usize = 5;
const CELL_LINES_CAP: usize = 8;
// 笔记行级 diff 的 LCS 单元数上限：超限降级为「中间段整段替换」，防长文逐行比对卡顿。
const NOTE_LCS_CELL_LIMIT: usize = 1_000_000;

/// 差异行类型（决定展示色；Same 仅用于笔记预览的全量行序列）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffLineKind {
    Same,
    Add,
    Remove,
    Change,
}

/// 一条展示用差异行（Same = 未被改动的行）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub text: String,
}

/// 单元格变更（field = 列名；row = 该行在当前版本里的位置，1 起）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellChange {
    pub field: String,
    pub row: usize,
    pub from: String,
    pub to: String,
}

/// 表格差异项：普通行（字段/行增删、顺序调整、截断提示）或单元格变更。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TableDiffItem {
    Line(DiffLine),
    Cell(CellChange),
}

fn line(kind: DiffLineKind, text: String) -> DiffLine {
    DiffLine { kind, text }
}

fn parse_table(json: &str) -> Option<TableFile> {
    serde_json::from_str(json).ok()
}

// ===== 表格 =====

/// 表格两版对比的中间数据：展示行与摘要计数共用同一份对比，避免两处口径漂移。
struct TableDelta {
    added_fields: Vec<String>,
    removed_fields: Vec<String>,
    renamed_fields: Vec<(String, String)>,
    added_rows: Vec<String>,
    removed_rows: Vec<String>,
    cells: Vec<CellChange>,
    field_order_changed: bool,
    row_order_changed: bool,
}

/// 表格两版快照的差异项：字段增删改名 / 行增删 / 单元格改动（`字段 · 第 N 行：旧 → 新`）/
/// 列行顺序调整（各类超限截断）。
pub fn table_diff(prev_json: &str, cur_json: &str) -> Vec<TableDiffItem> {
    let (Some(prev), Some(cur)) = (parse_table(prev_json), parse_table(cur_json)) else {
        return Vec::new();
    };
    let delta = table_delta(&prev, &cur);

    let mut fields = Vec::new();
    for name in &delta.added_fields {
        fields.push(line(DiffLineKind::Add, format!("新增字段「{name}」")));
    }
    for name in &delta.removed_fields {
        fields.push(line(DiffLineKind::Remove, format!("删除字段「{name}」")));
    }
    for (from, to) in &delta.renamed_fields {
        fields.push(line(DiffLineKind::Change, format!("字段「{from}」改名为「{to}」")));
    }
    let mut rows = Vec::new();
    for r in &delta.added_rows {
        rows.push(line(DiffLineKind::Add, format!("新增{r}")));
    }
    for r in &delta.removed_rows {
        rows.push(line(DiffLineKind::Remove, format!("删除{r}")));
    }
    let mut head = Vec::new();
    cap_lines(fields, FIELD_LINES_CAP, "个字段", &mut head);
    cap_lines(rows, ROW_LINES_CAP, "行", &mut head);
    let mut out: Vec<TableDiffItem> = head.into_iter().map(TableDiffItem::Line).collect();

    if delta.cells.len() > CELL_LINES_CAP {
        out.extend(delta.cells.iter().take(CELL_LINES_CAP).cloned().map(TableDiffItem::Cell));
        out.push(TableDiffItem::Line(line(
            DiffLineKind::Change,
            format!("还有 {} 个单元格", delta.cells.len() - CELL_LINES_CAP),
        )));
    } else {
        out.extend(delta.cells.iter().cloned().map(TableDiffItem::Cell));
    }

    if delta.field_order_changed {
        out.push(TableDiffItem::Line(line(DiffLineKind::Change, "调整列顺序".into())));
    }
    if delta.row_order_changed {
        out.push(TableDiffItem::Line(line(DiffLineKind::Change, "调整行顺序".into())));
    }
    out
}

/// 表格两版快照的记录用摘要（相对上一版）：实体计数而非文本行数；无上一版 = 新建统计。
pub fn summarize_table(prev_json: &str, cur_json: &str) -> String {
    let Some(cur) = parse_table(cur_json) else { return String::new() };
    let Some(prev) = parse_table(prev_json) else {
        return format!("新建 · {} 字段 · {} 行", cur.fields.len(), cur.rows.len());
    };
    let delta = table_delta(&prev, &cur);
    let mut parts: Vec<String> = Vec::new();
    if !delta.added_rows.is_empty() {
        parts.push(format!("新增 {} 行", delta.added_rows.len()));
    }
    if !delta.removed_rows.is_empty() {
        parts.push(format!("删除 {} 行", delta.removed_rows.len()));
    }
    if !delta.cells.is_empty() {
        parts.push(format!("修改 {} 个单元格", delta.cells.len()));
    }
    if !delta.added_fields.is_empty() {
        parts.push(format!("新增字段 {}", name_list(&delta.added_fields)));
    }
    if !delta.removed_fields.is_empty() {
        parts.push(format!("删除字段 {}", name_list(&delta.removed_fields)));
    }
    if !delta.renamed_fields.is_empty() {
        let renamed: Vec<String> = delta
            .renamed_fields
            .iter()
            .map(|(from, to)| format!("「{from}」→「{to}」"))
            .collect();
        parts.push(format!("字段改名 {}", renamed.join("、")));
    }
    if delta.field_order_changed {
        parts.push("调整列顺序".into());
    }
    if delta.row_order_changed {
        parts.push("调整行顺序".into());
    }
    if parts.is_empty() {
        "未改动".into()
    } else {
        parts.join(" · ")
    }
}

fn table_delta(prev: &TableFile, cur: &TableFile) -> TableDelta {
    let prev_fields: HashMap<&str, &TableField> =
        prev.fields.iter().map(|f| (f.id.as_str(), f)).collect();
    let mut added_fields = Vec::new();
    let mut renamed_fields = Vec::new();
    for f in &cur.fields {
        match prev_fields.get(f.id.as_str()) {
            None => added_fields.push(f.name.clone()),
            Some(p) if p.name != f.name => renamed_fields.push((p.name.clone(), f.name.clone())),
            Some(_) => {}
        }
    }
    let removed_fields: Vec<String> = prev
        .fields
        .iter()
        .filter(|p| !cur.fields.iter().any(|f| f.id == p.id))
        .map(|p| p.name.clone())
        .collect();

    let prev_rows: HashMap<&str, &TableRow> =
        prev.rows.iter().map(|r| (r.id.as_str(), r)).collect();
    let cur_row_index: HashMap<&str, usize> =
        cur.rows.iter().enumerate().map(|(i, r)| (r.id.as_str(), i)).collect();
    let added_rows: Vec<String> = cur
        .rows
        .iter()
        .enumerate()
        .filter(|(_, r)| !prev_rows.contains_key(r.id.as_str()))
        .map(|(i, r)| row_ref(r, &cur.fields, i))
        .collect();
    let removed_rows: Vec<String> = prev
        .rows
        .iter()
        .enumerate()
        .filter(|(_, p)| !cur.rows.iter().any(|r| r.id == p.id))
        .map(|(i, p)| row_ref(p, &prev.fields, i))
        .collect();

    let mut cells = Vec::new();
    for r in &cur.rows {
        let Some(p) = prev_rows.get(r.id.as_str()) else { continue };
        let row = cur_row_index[r.id.as_str()] + 1;
        for f in &cur.fields {
            let (a, b) = (p.values.get(&f.id), r.values.get(&f.id));
            if !crate::table_ops::cell_value_equal(a, b) {
                cells.push(CellChange {
                    field: f.name.clone(),
                    row,
                    from: cell_display(a),
                    to: cell_display(b),
                });
            }
        }
    }

    let cur_field_ids: Vec<&str> = cur.fields.iter().map(|f| f.id.as_str()).collect();
    let prev_field_ids: Vec<&str> = prev.fields.iter().map(|f| f.id.as_str()).collect();
    let cur_row_ids: Vec<&str> = cur.rows.iter().map(|r| r.id.as_str()).collect();
    let prev_row_ids: Vec<&str> = prev.rows.iter().map(|r| r.id.as_str()).collect();
    TableDelta {
        added_fields,
        removed_fields,
        renamed_fields,
        added_rows,
        removed_rows,
        cells,
        field_order_changed: order_changed(&cur_field_ids, &prev_field_ids),
        row_order_changed: order_changed(&cur_row_ids, &prev_row_ids),
    }
}

/// 顺序变化判定：两侧 id 集合相同但排列不同；增删导致的序列差异不算「调整顺序」（避免与增删重复报）。
fn order_changed(cur_ids: &[&str], prev_ids: &[&str]) -> bool {
    if cur_ids.len() != prev_ids.len() {
        return false;
    }
    let cur_set: HashSet<&str> = cur_ids.iter().copied().collect();
    let prev_set: HashSet<&str> = prev_ids.iter().copied().collect();
    cur_set == prev_set && cur_ids != prev_ids
}

/// 名称名册：「a」、「b」。
fn name_list(names: &[String]) -> String {
    names.iter().map(|n| format!("「{n}」")).collect::<Vec<_>>().join("、")
}

// ===== 画布 =====

/// 画布两版对比的中间数据（展示行与摘要计数共用）。
struct CanvasDelta {
    added_nodes: Vec<String>,
    removed_nodes: Vec<String>,
    modified_nodes: Vec<String>,
    added_edges: Vec<String>,
    removed_edges: Vec<String>,
    modified_edges: Vec<String>,
    added_messages: Vec<String>,
    removed_messages: Vec<String>,
    modified_messages: Vec<String>,
}

/// 画布两版快照的差异行：节点/连线/消息按 id 的增删改（连线两端与消息标签取自同侧快照）。
pub fn canvas_diff(prev_json: &str, cur_json: &str) -> Vec<DiffLine> {
    let (Ok(prev), Ok(cur)) = (
        serde_json::from_str::<CanvasFile>(prev_json),
        serde_json::from_str::<CanvasFile>(cur_json),
    ) else {
        return Vec::new();
    };
    let delta = canvas_delta(&prev, &cur, &messages_of(prev_json), &messages_of(cur_json));
    let mut out = Vec::new();

    let mut nodes = Vec::new();
    for label in &delta.added_nodes {
        nodes.push(line(DiffLineKind::Add, format!("新增节点「{label}」")));
    }
    for label in &delta.modified_nodes {
        nodes.push(line(DiffLineKind::Change, format!("节点「{label}」已修改")));
    }
    for label in &delta.removed_nodes {
        nodes.push(line(DiffLineKind::Remove, format!("删除节点「{label}」")));
    }
    cap_lines(nodes, CELL_LINES_CAP, "个节点", &mut out);

    let mut edges = Vec::new();
    for ends in &delta.added_edges {
        edges.push(line(DiffLineKind::Add, format!("新增连线「{ends}」")));
    }
    for ends in &delta.modified_edges {
        edges.push(line(DiffLineKind::Change, format!("连线「{ends}」已修改")));
    }
    for ends in &delta.removed_edges {
        edges.push(line(DiffLineKind::Remove, format!("删除连线「{ends}」")));
    }
    cap_lines(edges, CELL_LINES_CAP, "条连线", &mut out);

    let mut messages = Vec::new();
    for label in &delta.added_messages {
        messages.push(line(DiffLineKind::Add, format!("新增消息「{label}」")));
    }
    for label in &delta.modified_messages {
        messages.push(line(DiffLineKind::Change, format!("消息「{label}」已修改")));
    }
    for label in &delta.removed_messages {
        messages.push(line(DiffLineKind::Remove, format!("删除消息「{label}」")));
    }
    cap_lines(messages, CELL_LINES_CAP, "条消息", &mut out);
    out
}

/// 画布两版快照的记录用摘要（相对上一版）：节点/连线/消息计数；无上一版 = 新建统计。
pub fn summarize_canvas(prev_json: &str, cur_json: &str) -> String {
    let Ok(cur) = serde_json::from_str::<CanvasFile>(cur_json) else { return String::new() };
    let Ok(prev) = serde_json::from_str::<CanvasFile>(prev_json) else {
        return format!("新建 · {} 节点 · {} 连线", cur.nodes.len(), cur.edges.len());
    };
    let delta = canvas_delta(&prev, &cur, &messages_of(prev_json), &messages_of(cur_json));
    let mut parts: Vec<String> = Vec::new();
    for (n, label) in [
        (delta.added_nodes.len(), "新增 {} 节点"),
        (delta.removed_nodes.len(), "删除 {} 节点"),
        (delta.modified_nodes.len(), "修改 {} 节点"),
        (delta.added_edges.len(), "新增 {} 连线"),
        (delta.removed_edges.len(), "删除 {} 连线"),
        // 仅改端点或仅改消息内容的版本同样算改动：漏计会把有改动的版本标成「未改动」
        (delta.modified_edges.len(), "修改 {} 连线"),
        (delta.modified_messages.len(), "修改 {} 消息"),
    ] {
        if n > 0 {
            parts.push(label.replace("{}", &n.to_string()));
        }
    }
    let msg_delta = delta.added_messages.len() as i64 - delta.removed_messages.len() as i64;
    if msg_delta > 0 {
        parts.push(format!("对话消息 +{msg_delta}"));
    } else if msg_delta < 0 {
        parts.push(format!("对话消息 {msg_delta}"));
    }
    if parts.is_empty() {
        "未改动".into()
    } else {
        parts.join(" · ")
    }
}

fn canvas_delta(prev: &CanvasFile, cur: &CanvasFile, prev_msgs: &[Value], cur_msgs: &[Value]) -> CanvasDelta {
    let label_of = |nodes: &[crate::vault::CanvasFileNode], id: &str| {
        nodes.iter().find(|n| n.id == id).map(node_label).unwrap_or_else(|| "未知节点".to_string())
    };
    let prev_nodes: HashMap<&str, &crate::vault::CanvasFileNode> =
        prev.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut added_nodes = Vec::new();
    let mut modified_nodes = Vec::new();
    for n in &cur.nodes {
        match prev_nodes.get(n.id.as_str()) {
            None => added_nodes.push(node_label(n)),
            // 节点改 = 类型/尺寸/数据变化；坐标平移不算（布局移动非内容变化）
            Some(p)
                if p.node_type != n.node_type
                    || p.width != n.width
                    || p.height != n.height
                    || p.data != n.data =>
            {
                modified_nodes.push(node_label(n))
            }
            Some(_) => {}
        }
    }
    let removed_nodes: Vec<String> = prev
        .nodes
        .iter()
        .filter(|p| !cur.nodes.iter().any(|n| n.id == p.id))
        .map(node_label)
        .collect();

    let ends = |nodes: &[crate::vault::CanvasFileNode], e: &crate::vault::CanvasFileEdge| {
        format!("{} → {}", label_of(nodes, &e.source), label_of(nodes, &e.target))
    };
    let mut added_edges = Vec::new();
    let mut modified_edges = Vec::new();
    for e in &cur.edges {
        match prev.edges.iter().find(|p| p.id == e.id) {
            None => added_edges.push(ends(&cur.nodes, e)),
            Some(p) if p.source != e.source || p.target != e.target => {
                modified_edges.push(ends(&prev.nodes, p))
            }
            Some(_) => {}
        }
    }
    let removed_edges: Vec<String> = prev
        .edges
        .iter()
        .filter(|p| !cur.edges.iter().any(|e| e.id == p.id))
        .map(|p| ends(&prev.nodes, p))
        .collect();

    let label = |m: &Value| {
        m.get("text")
            .or_else(|| m.get("content"))
            .and_then(Value::as_str)
            .or_else(|| m.get("id").and_then(Value::as_str))
            .unwrap_or("消息")
            .to_string()
    };
    let msg_id = |m: &Value| m.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
    let mut added_messages = Vec::new();
    let mut modified_messages = Vec::new();
    for m in cur_msgs {
        let id = msg_id(m);
        match prev_msgs.iter().find(|p| msg_id(p) == id) {
            None => added_messages.push(label(m)),
            Some(p) if p != m => modified_messages.push(label(m)),
            Some(_) => {}
        }
    }
    let removed_messages: Vec<String> = prev_msgs
        .iter()
        .filter(|p| !cur_msgs.iter().any(|m| msg_id(m) == msg_id(p)))
        .map(label)
        .collect();

    CanvasDelta {
        added_nodes,
        removed_nodes,
        modified_nodes,
        added_edges,
        removed_edges,
        modified_edges,
        added_messages,
        removed_messages,
        modified_messages,
    }
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

// ===== 笔记 =====

/// 笔记两版正文的行级差异（全量行序列）：含当前版本全部行与被删行（插入在删除位置），
/// 供预览按行着色。公共前后缀先裁掉，中段按 LCS 对齐；中段规模超限时降级为整段替换。
pub fn note_diff(prev: &str, cur: &str) -> Vec<DiffLine> {
    // 空正文 = 零行（而非「一个空行」）：否则「空 → 有内容」会多出一条删除空行
    let a: Vec<&str> = if prev.is_empty() { Vec::new() } else { prev.split('\n').collect() };
    let b: Vec<&str> = if cur.is_empty() { Vec::new() } else { cur.split('\n').collect() };
    let (mut pre, mut suf) = (0, 0);
    while pre < a.len() && pre < b.len() && a[pre] == b[pre] {
        pre += 1;
    }
    while suf < a.len() - pre && suf < b.len() - pre && a[a.len() - 1 - suf] == b[b.len() - 1 - suf] {
        suf += 1;
    }
    let mid_a = &a[pre..a.len() - suf];
    let mid_b = &b[pre..b.len() - suf];

    let mut out = Vec::new();
    for text in &a[..pre] {
        out.push(line(DiffLineKind::Same, (*text).to_string()));
    }
    if mid_a.len().saturating_mul(mid_b.len()) > NOTE_LCS_CELL_LIMIT {
        // 降级：改动段过大，整段按「先删后增」呈现（不逐行对齐，避免长文比对开销）
        for text in mid_a {
            out.push(line(DiffLineKind::Remove, (*text).to_string()));
        }
        for text in mid_b {
            out.push(line(DiffLineKind::Add, (*text).to_string()));
        }
    } else {
        out.extend(lcs_diff(mid_a, mid_b));
    }
    for text in &a[a.len() - suf..] {
        out.push(line(DiffLineKind::Same, (*text).to_string()));
    }
    out
}

/// 中段行级 LCS 对齐（等长前缀删除行优先，与常见 diff 的「先删后增」一致）。
fn lcs_diff(a: &[&str], b: &[&str]) -> Vec<DiffLine> {
    let (n, m) = (a.len(), b.len());
    let width = m + 1;
    let mut dp = vec![0u32; (n + 1) * width];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i * width + j] = if a[i] == b[j] {
                dp[(i + 1) * width + j + 1] + 1
            } else {
                dp[(i + 1) * width + j].max(dp[i * width + j + 1])
            };
        }
    }
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if a[i] == b[j] {
            out.push(line(DiffLineKind::Same, a[i].to_string()));
            i += 1;
            j += 1;
        } else if dp[(i + 1) * width + j] >= dp[i * width + j + 1] {
            out.push(line(DiffLineKind::Remove, a[i].to_string()));
            i += 1;
        } else {
            out.push(line(DiffLineKind::Add, b[j].to_string()));
            j += 1;
        }
    }
    while i < n {
        out.push(line(DiffLineKind::Remove, a[i].to_string()));
        i += 1;
    }
    while j < m {
        out.push(line(DiffLineKind::Add, b[j].to_string()));
        j += 1;
    }
    out
}

// ===== 展示辅助 =====

/// 差异行截断：超上限截断并追加「还有 N …」（单位区分条数口径，避免多处截断提示同形）。
fn cap_lines(lines: Vec<DiffLine>, cap: usize, unit: &str, out: &mut Vec<DiffLine>) {
    if lines.len() > cap {
        let extra = lines.len() - cap;
        out.extend(lines.into_iter().take(cap));
        out.push(line(DiffLineKind::Change, format!("还有 {extra} {unit}")));
    } else {
        out.extend(lines);
    }
}

/// 行引用文本：有标签用「标签」，无标签回落「第 N 行」（N = 该行在所属版本里的位置）。
fn row_ref(row: &TableRow, fields: &[TableField], index: usize) -> String {
    match row_label(row, fields) {
        Some(label) => format!("行「{label}」"),
        None => format!("第 {} 行", index + 1),
    }
}

/// 行标签：按字段顺序取第一个非空文本值。
fn row_label(row: &TableRow, fields: &[TableField]) -> Option<String> {
    fields.iter().find_map(|f| {
        row.values
            .get(&f.id)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    })
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

/// 单元格展示串（缺键/空 = 「空」；图片按张数；超长截断）。
fn cell_display(v: Option<&Value>) -> String {
    const MAX: usize = 24;
    let text = match v {
        None => "空".to_string(),
        Some(Value::String(s)) if s.trim().is_empty() => "空".to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        // 图片形态（新 `{images}` 对象 / 旧 `string[]`）：按张数展示，空图片值即空单元格
        Some(v) if crate::table_ops::is_image_value(v) => {
            let n = crate::table_ops::image_entries(v).len();
            if n > 0 { format!("图片 ×{n}") } else { "空".to_string() }
        }
        Some(other) => other.to_string(),
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
    use crate::vault::{CanvasFileEdge, CanvasFileNode, TableField, TableFile, TableRow};

    fn field(id: &str, name: &str, field_type: &str) -> TableField {
        TableField {
            id: id.into(),
            name: name.into(),
            field_type: field_type.into(),
            options: None,
            width: None,
            calc_type: None,
        }
    }

    fn row(id: &str, values: &[(&str, serde_json::Value)]) -> TableRow {
        TableRow {
            id: id.into(),
            values: values.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect(),
            height: None,
            styles: None,
        }
    }

    fn table(fields: Vec<TableField>, rows: Vec<TableRow>) -> String {
        serde_json::to_string(&TableFile {
            schema: "atelyx-table/v1".into(),
            id: "t1".into(),
            title: "表".into(),
            fields,
            rows,
            created_at: 0,
            updated_at: 0,
        })
        .unwrap()
    }

    fn canvas(nodes: Vec<CanvasFileNode>, edges: Vec<CanvasFileEdge>, messages: &str) -> String {
        let base = serde_json::to_value(CanvasFile {
            schema: "atelyx-canvas/v1".into(),
            id: "c1".into(),
            title: "画布".into(),
            nodes,
            edges,
            created_at: 0,
            updated_at: 0,
        })
        .unwrap();
        if messages.is_empty() {
            return serde_json::to_string(&base).unwrap();
        }
        let mut obj = base.as_object().unwrap().clone();
        obj.insert(
            "messages".into(),
            serde_json::from_str::<Value>(messages).unwrap(),
        );
        serde_json::to_string(&Value::Object(obj)).unwrap()
    }

    fn node(id: &str, text: &str) -> CanvasFileNode {
        CanvasFileNode {
            id: id.into(),
            node_type: "text".into(),
            x: 0.0,
            y: 0.0,
            width: None,
            height: None,
            data: serde_json::json!({ "text": text }),
        }
    }

    fn texts(items: &[TableDiffItem]) -> Vec<String> {
        items
            .iter()
            .map(|i| match i {
                TableDiffItem::Line(l) => l.text.clone(),
                TableDiffItem::Cell(c) => format!("{} · 第 {} 行：{} → {}", c.field, c.row, c.from, c.to),
            })
            .collect()
    }

    #[test]
    fn table_cell_change_reports_field_and_row_position() {
        let prev = table(
            vec![field("f1", "名称", "text"), field("f2", "备注", "text")],
            vec![
                row("r1", &[("f1", "甲".into())]),
                row("r2", &[("f1", "乙".into()), ("f2", "旧".into())]),
            ],
        );
        let cur = table(
            vec![field("f1", "名称", "text"), field("f2", "备注", "text")],
            vec![
                row("r1", &[("f1", "甲".into())]),
                row("r2", &[("f1", "乙".into()), ("f2", "新".into())]),
            ],
        );
        // 行号取当前版本实际位置（1 起），字段用列名——用户能直接定位到「哪一行哪一列」
        assert_eq!(texts(&table_diff(&prev, &cur)), vec!["备注 · 第 2 行：旧 → 新"]);
        assert_eq!(summarize_table(&prev, &cur), "修改 1 个单元格");
    }

    #[test]
    fn table_rows_and_fields_report_labels() {
        let prev = table(
            vec![field("f1", "名称", "text")],
            vec![row("r1", &[("f1", "甲".into())])],
        );
        let cur = table(
            vec![field("f1", "标题", "text"), field("f2", "备注", "text")],
            vec![
                row("r1", &[("f1", "甲".into())]),
                row("r2", &[("f1", "乙".into())]),
            ],
        );
        assert_eq!(
            texts(&table_diff(&prev, &cur)),
            vec!["新增字段「备注」", "字段「名称」改名为「标题」", "新增行「乙」"]
        );
        assert_eq!(
            summarize_table(&prev, &cur),
            "新增 1 行 · 新增字段 「备注」 · 字段改名 「名称」→「标题」"
        );
    }

    #[test]
    fn table_reorder_only_reported_when_id_set_unchanged() {
        let two_rows = |first: &str, second: &str| {
            table(
                vec![field("f1", "名称", "text")],
                vec![
                    row(first, &[("f1", first.into())]),
                    row(second, &[("f1", second.into())]),
                ],
            )
        };
        let swapped = table_diff(&two_rows("r1", "r2"), &two_rows("r2", "r1"));
        assert_eq!(texts(&swapped), vec!["调整行顺序"]);

        // 增删导致的序列差异不算「调整顺序」（避免与增删重复报）
        let grown = table_diff(&two_rows("r1", "r2"), &two_rows("r2", "r3"));
        assert!(!texts(&grown).iter().any(|t| t == "调整行顺序"));
    }

    #[test]
    fn table_diff_caps_long_lists() {
        // 10 个新字段 → 展示截断为 5 行 + 提示（摘要仍按全量计数）
        let cols: Vec<TableField> = (0..10)
            .map(|i| field(&format!("f{i}"), &format!("列{i}"), "text"))
            .collect();
        let prev = table(Vec::new(), Vec::new());
        let cur = table(cols, Vec::new());
        let items = table_diff(&prev, &cur);
        assert_eq!(items.len(), FIELD_LINES_CAP + 1);
        assert_eq!(texts(&items).last().unwrap(), "还有 5 个字段");
        assert_eq!(summarize_table(&prev, &cur), "新增字段 「列0」、「列1」、「列2」、「列3」、「列4」、「列5」、「列6」、「列7」、「列8」、「列9」");
    }

    #[test]
    fn table_summary_first_version_and_noop() {
        let prev = table(vec![field("f1", "名称", "text")], vec![row("r1", &[])]);
        let cur = table(
            vec![field("f1", "名称", "text"), field("f2", "备注", "text")],
            vec![row("r1", &[]), row("r2", &[])],
        );
        // 无上一版（首版）：新建统计
        assert_eq!(summarize_table("", &cur), "新建 · 2 字段 · 2 行");
        // 相同内容：未改动
        assert_eq!(summarize_table(&prev, &prev), "未改动");
        // 解析失败：空摘要（展示层不显示该行）
        assert_eq!(summarize_table(&prev, "not json"), "");
    }

    #[test]
    fn table_display_image_cell_by_count() {
        let prev = table(vec![field("f1", "图", "image")], vec![row("r1", &[])]);
        let cur = table(
            vec![field("f1", "图", "image")],
            vec![row("r1", &[("f1", serde_json::json!({ "images": ["a.png", "b.png"] }))])],
        );
        assert_eq!(texts(&table_diff(&prev, &cur)), vec!["图 · 第 1 行：空 → 图片 ×2"]);
    }

    #[test]
    fn note_diff_marks_added_removed_and_keeps_context() {
        let prev = "a\nb\nc";
        let cur = "a\nB\nc\nd";
        assert_eq!(
            note_diff(prev, cur),
            vec![
                line(DiffLineKind::Same, "a".into()),
                line(DiffLineKind::Remove, "b".into()),
                line(DiffLineKind::Add, "B".into()),
                line(DiffLineKind::Same, "c".into()),
                line(DiffLineKind::Add, "d".into()),
            ]
        );
        // 全量替换：旧行逐行标删、新行逐行标增（无公共前后缀）
        let replaced = note_diff("旧一\n旧二", "新一\n新二\n新三");
        assert!(replaced.iter().all(|l| l.kind != DiffLineKind::Same));
        assert_eq!(replaced.iter().filter(|l| l.kind == DiffLineKind::Remove).count(), 2);
        assert_eq!(replaced.iter().filter(|l| l.kind == DiffLineKind::Add).count(), 3);
    }

    #[test]
    fn note_diff_treats_empty_text_as_zero_lines() {
        // 空正文 = 零行：不得凭空多出一条删除空行
        let from_empty = note_diff("", "甲\n乙");
        assert_eq!(
            from_empty,
            vec![
                line(DiffLineKind::Add, "甲".into()),
                line(DiffLineKind::Add, "乙".into()),
            ]
        );
        let to_empty = note_diff("甲\n乙", "");
        assert!(to_empty.iter().all(|l| l.kind == DiffLineKind::Remove));
        assert!(note_diff("", "").is_empty());
    }

    #[test]
    fn note_diff_trims_common_prefix_and_suffix() {
        let prev = "头\n旧\n尾";
        let cur = "头\n新\n尾";
        let out = note_diff(prev, cur);
        // 公共首尾标记为 Same（未改动块合并展示的输入）
        assert_eq!(out.first().unwrap().kind, DiffLineKind::Same);
        assert_eq!(out.last().unwrap().kind, DiffLineKind::Same);
        assert_eq!(out.len(), 4);
    }

    #[test]
    fn canvas_summary_counts_nodes_edges_messages() {
        let json = canvas(
            vec![node("n1", "甲")],
            vec![CanvasFileEdge {
                id: "e1".into(),
                source: "n1".into(),
                target: "n1".into(),
                source_handle: None,
                target_handle: None,
                directed: None,
                link_mode: None,
                created_at: 0,
            }],
            r#"[{"id":"m1","text":"你好"}]"#,
        );
        assert_eq!(canvas_summary(&json), (1, 1, 1));
        assert_eq!(canvas_summary("not json"), (0, 0, 0));
    }

    #[test]
    fn canvas_diff_and_summary_track_entity_changes() {
        let prev = canvas(vec![node("n1", "甲")], Vec::new(), "");
        let cur = canvas(
            vec![node("n1", "甲改"), node("n2", "乙")],
            Vec::new(),
            r#"[{"id":"m1","text":"你好"}]"#,
        );
        assert_eq!(
            canvas_diff(&prev, &cur).iter().map(|l| l.text.clone()).collect::<Vec<_>>(),
            vec!["新增节点「乙」", "节点「甲改」已修改", "新增消息「你好」"]
        );
        assert_eq!(summarize_canvas(&prev, &cur), "新增 1 节点 · 修改 1 节点 · 对话消息 +1");
        // 无上一版：新建统计
        assert_eq!(summarize_canvas("", &cur), "新建 · 2 节点 · 0 连线");
    }

    #[test]
    fn canvas_summary_reports_edge_and_message_edits() {
        let edge = |target: &str| CanvasFileEdge {
            id: "e1".into(),
            source: "n1".into(),
            target: target.into(),
            source_handle: None,
            target_handle: None,
            directed: None,
            link_mode: None,
            created_at: 0,
        };
        let prev = canvas(
            vec![node("n1", "甲"), node("n2", "乙")],
            vec![edge("n1")],
            r#"[{"id":"m1","text":"旧话"}]"#,
        );
        let cur = canvas(
            vec![node("n1", "甲"), node("n2", "乙")],
            vec![edge("n2")],
            r#"[{"id":"m1","text":"新话"}]"#,
        );
        // 只改端点/消息内容也是改动：摘要不得写成「未改动」（与展开差异口径一致）
        assert_eq!(summarize_canvas(&prev, &cur), "修改 1 连线 · 修改 1 消息");
        assert_eq!(
            canvas_diff(&prev, &cur).iter().map(|l| l.text.clone()).collect::<Vec<_>>(),
            vec!["连线「甲 → 甲」已修改", "消息「新话」已修改"]
        );
    }

    #[test]
    fn summary_dispatches_by_kind() {
        let table_json = table(vec![field("f1", "名称", "text")], vec![row("r1", &[])]);
        assert_eq!(
            crate::history::version_summary(crate::history::HistoryKind::Table, "", &table_json),
            "新建 · 1 字段 · 1 行"
        );
        assert_eq!(
            crate::history::version_summary(crate::history::HistoryKind::Note, "a", "a\nb"),
            "+1 行"
        );
        // 内容变了但实体口径认不出（如仅改列宽）：不得写成「未改动」
        let wider = table_json.replace("\"name\":\"名称\"", "\"name\":\"名称\",\"width\":180.0");
        assert_eq!(
            crate::history::version_summary(crate::history::HistoryKind::Table, &table_json, &wider),
            "其他改动"
        );
        assert_eq!(
            crate::history::version_summary(crate::history::HistoryKind::Table, &table_json, &table_json),
            "未改动"
        );
    }
}
