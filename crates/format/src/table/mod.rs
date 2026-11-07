//! 表格格式语义：`.atb` 模型、按 id 的增量补丁合并、图片附件命名。零 I/O，宿主与插件共用一份。

pub mod ops;

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

/// `.atb` 表格文件 schema 标识。
pub const TABLE_SCHEMA: &str = "atelyx-table/v1";

/// `.atb` 表格文件（schema `atelyx-table/v1`）。
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TableFile {
    #[serde(default)]
    pub schema: String,
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub fields: Vec<TableField>,
    #[serde(default)]
    pub rows: Vec<TableRow>,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TableField {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub field_type: String,
    /// singleSelect 的选项列表（其他类型无此字段）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
    /// 用户拖拽调整后的列宽（px；缺省 = 按字段名自适应）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
    /// 状态栏列自动计算类型（sum/avg/max/min/count；缺省 = 无计算）。
    /// 必须在此显式声明，否则 serde 反序列化会丢弃调用方传来的该字段，写盘丢失。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calc_type: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct TableRow {
    pub id: String,
    /// 单元格值按字段 id 存（缺 key = 空单元格）。
    #[serde(default)]
    pub values: serde_json::Map<String, serde_json::Value>,
    /// 用户拖拽调整后的行高（px；缺省 = 按内容自然撑开）。
    /// 必须在此显式声明，否则 serde 反序列化会丢弃调用方传来的该字段，写盘丢失。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<f64>,
    /// 单元格显示样式按字段 id 存（与值正交：字体/粗体/字色/底色等，缺 key = 默认样式）。
    /// 必须在此显式声明，否则反序列化丢弃、增量补丁整行替换后写盘丢失样式。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub styles: Option<serde_json::Map<String, serde_json::Value>>,
}

/// 表格增量保存补丁（对应 `.atb` schema）。
#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TablePatch {
    /// 表格 id（防串文件守卫）。
    pub id: String,
    /// 标题（线形状保留：合并端按此应用标题）。文件名是唯一真源，内容保存不携带——
    /// 改名只走重命名端点，否则陈旧标题会把文件改回旧名。
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub upsert_fields: Vec<TableField>,
    #[serde(default)]
    pub removed_field_ids: Vec<String>,
    #[serde(default)]
    pub upsert_rows: Vec<TableRow>,
    #[serde(default)]
    pub removed_row_ids: Vec<String>,
    /// 字段 id 全序（与上次落盘序列不同时携带；排序是数组属性，id 合并无法表达，须显式重排）。
    #[serde(default)]
    pub field_order: Option<Vec<String>>,
    /// 行 id 全序（同上）。
    #[serde(default)]
    pub row_order: Option<Vec<String>>,
}

/// 按 id 顺序重排数组（稳定排序）：order 未出现的 id（同批删除/对端新增）保持相对顺序置于末尾。
/// 用于补丁合并后应用 field_order/row_order——拖拽排序/复制行/左右插列的顺序变化落盘。
pub fn reorder_by<T>(items: &mut [T], order: &[String], key: impl Fn(&T) -> &str) {
    if items.len() <= 1 {
        return;
    }
    let rank: HashMap<&str, usize> = order
        .iter()
        .enumerate()
        .map(|(i, id)| (id.as_str(), i))
        .collect();
    items.sort_by_key(|item| rank.get(key(item)).copied().unwrap_or(usize::MAX));
}

/// 计算表格增量补丁：与落盘基线按值 diff，只含变化/新增/删除的字段与行；数组顺序是数组属性、
/// 值 diff 看不见，id 序列不同即视为排序变更并经 field_order/row_order 显式携带。
/// 无任何变化返回 None（调用方跳过写盘）。title 不随补丁携带（文件改名链负责磁盘标题）。
pub fn compute_table_patch(baseline: &TableFile, current: &TableFile) -> Option<TablePatch> {
    let last_fields: HashMap<&str, &TableField> =
        baseline.fields.iter().map(|f| (f.id.as_str(), f)).collect();
    let last_rows: HashMap<&str, &TableRow> =
        baseline.rows.iter().map(|r| (r.id.as_str(), r)).collect();
    let upsert_fields: Vec<TableField> = current
        .fields
        .iter()
        .filter(|f| last_fields.get(f.id.as_str()).copied() != Some(*f))
        .cloned()
        .collect();
    let current_field_ids: HashSet<&str> =
        current.fields.iter().map(|f| f.id.as_str()).collect();
    let removed_field_ids: Vec<String> = baseline
        .fields
        .iter()
        .filter(|f| !current_field_ids.contains(f.id.as_str()))
        .map(|f| f.id.clone())
        .collect();
    let upsert_rows: Vec<TableRow> = current
        .rows
        .iter()
        .filter(|r| last_rows.get(r.id.as_str()).copied() != Some(*r))
        .cloned()
        .collect();
    let current_row_ids: HashSet<&str> = current.rows.iter().map(|r| r.id.as_str()).collect();
    let removed_row_ids: Vec<String> = baseline
        .rows
        .iter()
        .filter(|r| !current_row_ids.contains(r.id.as_str()))
        .map(|r| r.id.clone())
        .collect();
    let field_order_changed = !same_id_sequence(&baseline.fields, &current.fields);
    let row_order_changed = !same_id_sequence(&baseline.rows, &current.rows);
    if upsert_fields.is_empty()
        && upsert_rows.is_empty()
        && removed_field_ids.is_empty()
        && removed_row_ids.is_empty()
        && !field_order_changed
        && !row_order_changed
    {
        return None;
    }
    Some(TablePatch {
        id: current.id.clone(),
        // 标题随文件名，不由内容补丁携带（见字段注释）
        title: None,
        upsert_fields,
        removed_field_ids,
        upsert_rows,
        removed_row_ids,
        field_order: field_order_changed.then(|| current.fields.iter().map(|f| f.id.clone()).collect()),
        row_order: row_order_changed.then(|| current.rows.iter().map(|r| r.id.clone()).collect()),
    })
}

/// id 序列是否逐位一致（长度不同即不等）：顺序是数组属性，须显式比对。
fn same_id_sequence<T: Id>(a: &[T], b: &[T]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.id() == y.id())
}

/// 表格实体的稳定 id 抽象（补丁 diff 的 id 序列比较共用）。
trait Id {
    fn id(&self) -> &str;
}

impl Id for TableField {
    fn id(&self) -> &str {
        &self.id
    }
}

impl Id for TableRow {
    fn id(&self) -> &str {
        &self.id
    }
}

/// 按稳定 id 合并表格补丁到全量文件（removed 幂等；upsert 覆盖同 id 或追加；
/// field_order/row_order 显式重排——order 未出现的实体保持相对顺序置尾）。
/// 与磁盘真源合并（而非整文件覆写）：外部改动期间未触及的字段/行不被在途保存冲掉。
pub fn apply_table_patch(table: &mut TableFile, patch: &TablePatch) {
    if let Some(title) = &patch.title {
        table.title = title.clone();
    }
    let removed_fields: HashSet<&String> = patch.removed_field_ids.iter().collect();
    table.fields.retain(|f| !removed_fields.contains(&f.id));
    for f in &patch.upsert_fields {
        match table.fields.iter_mut().find(|x| x.id == f.id) {
            Some(existing) => *existing = f.clone(),
            None => table.fields.push(f.clone()),
        }
    }
    let removed_rows: HashSet<&String> = patch.removed_row_ids.iter().collect();
    table.rows.retain(|r| !removed_rows.contains(&r.id));
    for r in &patch.upsert_rows {
        match table.rows.iter_mut().find(|x| x.id == r.id) {
            Some(existing) => *existing = r.clone(),
            None => table.rows.push(r.clone()),
        }
    }
    if let Some(order) = &patch.field_order {
        reorder_by(&mut table.fields, order, |f| f.id.as_str());
    }
    if let Some(order) = &patch.row_order {
        reorder_by(&mut table.rows, order, |r| r.id.as_str());
    }
}

/// 表格图片附件相对路径：每次导入新文件名 `img-<nanoid>.<ext>`（删除后重导不覆盖旧文件、
/// 不撞显示缓存）。表格 id 直接进路径，只接受应用生成器的字母表（字母/数字/`-`/`_`），
/// 拒绝路径语义与空串。
pub fn table_image_rel_path(table_id: &str, file_name: &str) -> Result<String, String> {
    let valid = !table_id.is_empty()
        && table_id.len() <= 128
        && table_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !valid {
        return Err(format!("非法的表格 id：{table_id}"));
    }
    let ext = image_ext_of(file_name).ok_or_else(|| format!("非图片文件：{file_name}"))?;
    let name = format!("img-{}", nanoid::nanoid!());
    Ok(format!(".atelyx/temp/tables/{table_id}/{name}.{ext}"))
}

/// 图片文件名 → 规范扩展名（白名单外拒绝；对话附件托盘与表格导入共用）。
pub fn image_ext_of(file_name: &str) -> Option<&'static str> {
    let ext = std::path::Path::new(file_name).extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some("png"),
        "jpg" | "jpeg" => Some("jpg"),
        "webp" => Some("webp"),
        "gif" => Some("gif"),
        _ => None,
    }
}

#[cfg(test)]
mod patch_tests {
    use super::*;

    fn field(id: &str, name: &str, field_type: &str) -> TableField {
        TableField { id: id.into(), name: name.into(), field_type: field_type.into(), options: None, width: None, calc_type: None }
    }

    fn row(id: &str, values: Vec<(&str, serde_json::Value)>) -> TableRow {
        TableRow {
            id: id.into(),
            values: values.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
            height: None,
            styles: None,
        }
    }

    fn table(id: &str, fields: Vec<TableField>, rows: Vec<TableRow>) -> TableFile {
        TableFile {
            schema: TABLE_SCHEMA.into(),
            id: id.into(),
            title: "表格".into(),
            fields,
            rows,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn cell(v: &str) -> serde_json::Value {
        serde_json::json!(v)
    }

    #[test]
    fn no_change_yields_no_patch() {
        let base = table("t", vec![field("f1", "名称", "text")], vec![row("r1", vec![("f1", cell("a"))])]);
        assert!(compute_table_patch(&base, &base).is_none());
    }

    #[test]
    fn single_cell_edit_produces_one_row_upsert() {
        let base = table("t", vec![field("f1", "名称", "text")], vec![
            row("r1", vec![("f1", cell("a"))]),
            row("r2", vec![("f1", cell("b"))]),
        ]);
        let mut next = base.clone();
        next.rows[0].values.insert("f1".into(), cell("x"));
        let patch = compute_table_patch(&base, &next).expect("patch");
        assert_eq!(patch.upsert_rows.len(), 1);
        assert_eq!(patch.upsert_rows[0].id, "r1");
        assert!(patch.upsert_fields.is_empty());
        assert!(patch.removed_row_ids.is_empty());
        assert!(patch.field_order.is_none());
        // 同一实体 id 顺序未变
        assert!(patch.row_order.is_none());
    }

    #[test]
    fn order_change_carries_order_vectors() {
        let base = table("t", vec![field("f1", "a", "text"), field("f2", "b", "text")], vec![row("r1", vec![])]);
        let mut next = base.clone();
        next.fields.reverse();
        let patch = compute_table_patch(&base, &next).expect("patch");
        assert!(patch.upsert_fields.is_empty(), "值未变不应产生 upsert");
        assert_eq!(patch.field_order, Some(vec!["f2".into(), "f1".into()]));
    }

    #[test]
    fn add_remove_field_and_row() {
        let base = table("t", vec![field("f1", "a", "text")], vec![row("r1", vec![])]);
        let mut next = base.clone();
        next.fields.push(field("f2", "b", "number"));
        next.rows.push(row("r2", vec![]));
        next.rows.remove(0);
        let patch = compute_table_patch(&base, &next).expect("patch");
        assert_eq!(patch.upsert_fields.len(), 1);
        assert_eq!(patch.removed_row_ids, vec!["r1"]);
        assert_eq!(patch.upsert_rows.len(), 1);
    }

    #[test]
    fn apply_patch_merges_onto_disk_truth() {
        // 磁盘文件含外部新增行（基线里没有）：补丁合并不得冲掉它
        let base = table("t", vec![field("f1", "a", "text")], vec![row("r1", vec![("f1", cell("a"))])]);
        let mut next = base.clone();
        next.rows[0].values.insert("f1".into(), cell("x"));
        let patch = compute_table_patch(&base, &next).expect("patch");

        let mut disk = base.clone();
        disk.rows.push(row("r-external", vec![("f1", cell("外部"))]));
        apply_table_patch(&mut disk, &patch);
        assert_eq!(disk.rows[0].values["f1"], cell("x"));
        assert_eq!(disk.rows[1].id, "r-external");
    }

    #[test]
    fn apply_patch_reorders_and_preserves_unknown_ids_at_end() {
        let mut disk = table("t", vec![field("f1", "a", "text"), field("f2", "b", "text")], vec![
            row("r1", vec![]),
            row("r2", vec![]),
        ]);
        disk.rows.push(row("r-new", vec![]));
        let patch = TablePatch {
            id: "t".into(),
            title: None,
            upsert_fields: vec![],
            removed_field_ids: vec![],
            upsert_rows: vec![],
            removed_row_ids: vec![],
            field_order: None,
            row_order: Some(vec!["r2".into(), "r1".into()]),
        };
        apply_table_patch(&mut disk, &patch);
        let ids: Vec<&str> = disk.rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["r2", "r1", "r-new"]);
    }

    #[test]
    fn patch_survives_json_roundtrip() {
        // TablePatch 线形状即契约（camelCase + type 字段名）
        let base = table("t", vec![field("f1", "a", "singleSelect")], vec![row("r1", vec![("f1", cell("甲"))])]);
        let mut next = base.clone();
        next.fields[0].field_type = "text".into();
        next.fields[0].options = Some(vec!["甲".into()]);
        let patch = compute_table_patch(&base, &next).expect("patch");
        let json = serde_json::to_string(&patch).expect("serialize");
        assert!(json.contains("\"type\""));
        let back: TablePatch = serde_json::from_str(&json).expect("deserialize");
        let mut disk = base.clone();
        apply_table_patch(&mut disk, &back);
        assert_eq!(disk.fields, next.fields);
    }
}
