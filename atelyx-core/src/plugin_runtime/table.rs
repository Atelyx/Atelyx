//! 表格服务面的纯映射：将宿主表格真源转成协议的只读快照形态。
//! 只做形状转换，不读写磁盘——写入路径由壳的表格会话与补丁 / 保存链承担。

use atelyx_plugin_protocol::method::{
    TableField, TableRemoteSelection, TableRow, TableSelection, TableSnapshot,
};

use crate::table_ops::TableSelection as SourceSelection;
use crate::vault::{TableField as SourceField, TableFile, TableRow as SourceRow};

/// 远端协作者选区的宿主输入形态（色值为身份色原串）。
pub struct RemoteSelection<'a> {
    pub name: &'a str,
    pub color: &'a str,
    pub selection: &'a SourceSelection,
}

/// 构建某表格的只读快照：稳定 id 供插件跨改名引用，字段带 id / 名称 / 类型 / 选项 / 列宽，
/// 行带 id / 值集 / 单元格样式，另附本端与远端协作者的当前选区
/// （快照要能还原「用户看到的样子」，不只是数据本身）。
pub fn snapshot(
    path: &str,
    file: &TableFile,
    selection: Option<&SourceSelection>,
    remotes: &[RemoteSelection<'_>],
) -> TableSnapshot {
    TableSnapshot {
        open: true,
        id: Some(file.id.clone()),
        path: Some(path.to_string()),
        fields: file.fields.iter().map(field).collect(),
        rows: file.rows.iter().map(row).collect(),
        selection: selection.map(selection_of),
        remote_selections: remotes
            .iter()
            .map(|remote| TableRemoteSelection {
                name: remote.name.to_string(),
                color: remote.color.to_string(),
                selection: selection_of(remote.selection),
            })
            .collect(),
    }
}

fn field(source: &SourceField) -> TableField {
    TableField {
        id: source.id.clone(),
        name: source.name.clone(),
        field_type: source.field_type.clone(),
        options: source.options.clone().unwrap_or_default(),
        width: source.width,
    }
}

fn row(source: &SourceRow) -> TableRow {
    TableRow {
        id: source.id.clone(),
        values: source.values.clone(),
        styles: source.styles.clone(),
    }
}

/// 选区形态一一对应（宿主会话的选区就是协议的五形态）。
pub fn selection_of(source: &SourceSelection) -> TableSelection {
    match source {
        SourceSelection::Cell { row_id, field_id } => TableSelection::Cell {
            row_id: row_id.clone(),
            field_id: field_id.clone(),
        },
        SourceSelection::Range { anchor_row_id, anchor_field_id, row_id, field_id } => {
            TableSelection::Range {
                anchor_row_id: anchor_row_id.clone(),
                anchor_field_id: anchor_field_id.clone(),
                row_id: row_id.clone(),
                field_id: field_id.clone(),
            }
        }
        SourceSelection::Row(row_id) => TableSelection::Row { row_id: row_id.clone() },
        SourceSelection::Column(field_id) => TableSelection::Column { field_id: field_id.clone() },
        SourceSelection::All => TableSelection::All,
    }
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
                styles: Some(
                    serde_json::json!({ "f1": { "bold": true } })
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            }],
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn snapshot_maps_fields_rows_and_view_properties() {
        let selection = SourceSelection::Row("r1".into());
        let snapshot = snapshot("a.atb", &table(), Some(&selection), &[]);
        assert!(snapshot.open);
        assert_eq!(snapshot.id.as_deref(), Some("t1"));
        assert_eq!(snapshot.path.as_deref(), Some("a.atb"));
        assert_eq!(snapshot.fields.len(), 1);
        assert_eq!(snapshot.fields[0].field_type, "singleSelect");
        assert_eq!(snapshot.fields[0].options, vec!["甲", "乙"]);
        assert_eq!(snapshot.fields[0].width, Some(180.0));
        assert_eq!(snapshot.rows[0].id, "r1");
        assert_eq!(snapshot.rows[0].values["f1"], "甲");
        assert_eq!(snapshot.rows[0].styles.as_ref().unwrap()["f1"]["bold"], true);
        assert_eq!(snapshot.selection, Some(TableSelection::Row { row_id: "r1".into() }));
        // 行高是宿主布局参数，不进快照
        let wire = serde_json::to_value(&snapshot).unwrap();
        assert!(wire["rows"][0].get("height").is_none());
    }

    #[test]
    fn snapshot_without_selection_stays_empty() {
        let snapshot = snapshot("a.atb", &table(), None, &[]);
        assert_eq!(snapshot.selection, None);
        let wire = serde_json::to_value(&snapshot).unwrap();
        assert!(wire.get("selection").is_none(), "无选区时字段缺省");
        assert!(wire.get("remoteSelections").is_none(), "无人协同时远端选区缺省");
    }

    #[test]
    fn snapshot_maps_remote_selections() {
        let remote = SourceSelection::Cell { row_id: "r1".into(), field_id: "f1".into() };
        let remotes = [RemoteSelection { name: "鲍勃", color: "#3d7be0", selection: &remote }];
        let snapshot = snapshot("a.atb", &table(), None, &remotes);
        assert_eq!(
            snapshot.remote_selections,
            vec![TableRemoteSelection {
                name: "鲍勃".into(),
                color: "#3d7be0".into(),
                selection: TableSelection::Cell { row_id: "r1".into(), field_id: "f1".into() },
            }]
        );
    }

    #[test]
    fn selection_shapes_are_mapped_one_to_one() {
        let cases = [
            (
                SourceSelection::Cell { row_id: "r1".into(), field_id: "f1".into() },
                TableSelection::Cell { row_id: "r1".into(), field_id: "f1".into() },
            ),
            (
                SourceSelection::Range {
                    anchor_row_id: "r1".into(),
                    anchor_field_id: "f1".into(),
                    row_id: "r2".into(),
                    field_id: "f2".into(),
                },
                TableSelection::Range {
                    anchor_row_id: "r1".into(),
                    anchor_field_id: "f1".into(),
                    row_id: "r2".into(),
                    field_id: "f2".into(),
                },
            ),
            (SourceSelection::Column("f1".into()), TableSelection::Column { field_id: "f1".into() }),
            (SourceSelection::All, TableSelection::All),
        ];
        for (source, expected) in cases {
            assert_eq!(selection_of(&source), expected);
        }
    }

    #[test]
    fn snapshot_carries_empty_options_for_plain_fields() {
        let mut file = table();
        file.fields[0].options = None;
        let snapshot = snapshot("a.atb", &file, None, &[]);
        assert!(snapshot.fields[0].options.is_empty());
    }
}
