//! 表格服务面的纯映射：将宿主表格真源转成协议的只读快照形态。
//! 只做形状转换，不读写磁盘——写入路径由壳的表格会话与补丁 / 保存链承担。

use atelyx_plugin_protocol::method::{TableField, TableRow, TableSnapshot};

use crate::vault::{TableField as SourceField, TableFile, TableRow as SourceRow};

/// 构建某表格的只读快照：字段只带 id / 名称 / 类型 / 选项，行只带 id / 值集。
pub fn snapshot(path: &str, file: &TableFile) -> TableSnapshot {
    TableSnapshot {
        open: true,
        path: Some(path.to_string()),
        fields: file.fields.iter().map(field).collect(),
        rows: file.rows.iter().map(row).collect(),
    }
}

fn field(source: &SourceField) -> TableField {
    TableField {
        id: source.id.clone(),
        name: source.name.clone(),
        field_type: source.field_type.clone(),
        options: source.options.clone().unwrap_or_default(),
    }
}

fn row(source: &SourceRow) -> TableRow {
    TableRow { id: source.id.clone(), values: source.values.clone() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> TableFile {
        TableFile {
            schema: "atelyx-table/v1".into(),
            id: "t1".into(),
            title: "表".into(),
            fields: vec![SourceField {
                id: "f1".into(),
                name: "名称".into(),
                field_type: "singleSelect".into(),
                options: Some(vec!["甲".into(), "乙".into()]),
                width: Some(180.0),
                calc_type: None,
            }],
            rows: vec![SourceRow {
                id: "r1".into(),
                values: serde_json::json!({ "f1": "甲" }).as_object().unwrap().clone(),
                height: Some(40.0),
                styles: None,
            }],
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn snapshot_maps_fields_and_rows() {
        let snapshot = snapshot("a.atb", &table());
        assert!(snapshot.open);
        assert_eq!(snapshot.path.as_deref(), Some("a.atb"));
        assert_eq!(snapshot.fields.len(), 1);
        assert_eq!(snapshot.fields[0].field_type, "singleSelect");
        assert_eq!(snapshot.fields[0].options, vec!["甲", "乙"]);
        assert_eq!(snapshot.rows[0].id, "r1");
        assert_eq!(snapshot.rows[0].values["f1"], "甲");
        // 视图专属字段（列宽 / 行高）不进快照
        let wire = serde_json::to_value(&snapshot).unwrap();
        assert!(wire["fields"][0].get("width").is_none());
        assert!(wire["rows"][0].get("height").is_none());
    }

    #[test]
    fn snapshot_carries_empty_options_for_plain_fields() {
        let mut file = table();
        file.fields[0].options = None;
        let snapshot = snapshot("a.atb", &file);
        assert!(snapshot.fields[0].options.is_empty());
    }
}
