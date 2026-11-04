//! 表格服务面（`table.*`）：向其它插件（第三方表格视图）提供当前表格的读取与写入。
//! 写入与插件自身编辑同一出口：同撤销栈、同保存链、同广播。

use atelyx_format::table::ops;
use atelyx_plugin_protocol::method::{
    TableAddRowResult, TableField, TableRemoteSelection, TableRow, TableSelection, TableSnapshot,
};
use atelyx_plugin_protocol::service::HostCallParams;
use atelyx_plugin_sdk::Context;

use crate::model::Session;
use crate::PluginState;

/// 服务名与方法清单（归属本插件）。
pub const SERVICE: &str = "table";
pub const METHODS: &[&str] = &[
    "snapshot",
    "updateCell",
    "addRow",
    "removeRow",
    "selectRow",
    "setCellImage",
];

/// 服务调用分发：结果或可读错误。
pub async fn handle(plugin: &std::sync::Arc<PluginState>, ctx: &Context, params: HostCallParams) {
    if params.service != SERVICE {
        let _ = ctx
            .service_error(&params.call_id, &format!("{}：本插件不提供该服务", params.service))
            .await;
        return;
    }
    let method = params.method.as_str();
    let result = match method {
        "snapshot" => plugin.with_session(|session| snapshot(plugin, session)).map(Ok).unwrap_or_else(|| {
            Ok(serde_json::to_value(TableSnapshot::default()).unwrap_or_default())
        }),
        "updateCell" => update_cell(plugin, ctx, &params.params).await,
        "addRow" => add_row(plugin, ctx).await,
        "removeRow" => remove_row(plugin, ctx, &params.params).await,
        "selectRow" => select_row(plugin, ctx, &params.params).await,
        "setCellImage" => set_cell_image(plugin, ctx, &params.params).await,
        other => Err(format!("table 没有方法 {other}")),
    };
    match result {
        Ok(value) => {
            let _ = ctx.service_result(&params.call_id, value).await;
        }
        Err(error) => {
            let _ = ctx.service_error(&params.call_id, &error).await;
        }
    }
}

/// 当前表格只读快照（未打开 = `open: false`）。
pub fn snapshot(plugin: &PluginState, session: &Session) -> serde_json::Value {
    let fields = session
        .data
        .fields
        .iter()
        .map(|field| TableField {
            id: field.id.clone(),
            name: field.name.clone(),
            field_type: field.field_type.clone(),
            options: field.options.clone().unwrap_or_default(),
            width: field.width,
        })
        .collect::<Vec<_>>();
    let rows = session
        .data
        .rows
        .iter()
        .map(|row| TableRow {
            id: row.id.clone(),
            values: row.values.clone(),
            styles: row.styles.clone(),
        })
        .collect::<Vec<_>>();
    let selection = session.view.selection.as_ref().and_then(selection_of);
    let remote_selections: Vec<TableRemoteSelection> = plugin
        .presence
        .lock()
        .map(|presence| {
            presence
                .selections(&session.data.fields, &session.data.rows)
                .into_iter()
                .map(|(name, color, selection)| TableRemoteSelection { name, color, selection })
                .collect()
        })
        .unwrap_or_default();
    serde_json::to_value(TableSnapshot {
        open: true,
        id: Some(session.data.id.clone()),
        path: Some(session.file.clone()),
        fields,
        rows,
        selection,
        remote_selections,
    })
    .unwrap_or_default()
}

/// 本地选区 → 协议选区。
pub fn selection_of(selection: &ops::TableSelection) -> Option<TableSelection> {
    Some(match selection {
        ops::TableSelection::Cell { row_id, field_id } => TableSelection::Cell {
            row_id: row_id.clone(),
            field_id: field_id.clone(),
        },
        ops::TableSelection::Range { anchor_row_id, anchor_field_id, row_id, field_id } => {
            TableSelection::Range {
                anchor_row_id: anchor_row_id.clone(),
                anchor_field_id: anchor_field_id.clone(),
                row_id: row_id.clone(),
                field_id: field_id.clone(),
            }
        }
        ops::TableSelection::Row(row_id) => TableSelection::Row { row_id: row_id.clone() },
        ops::TableSelection::Column(field_id) => {
            TableSelection::Column { field_id: field_id.clone() }
        }
        ops::TableSelection::All => TableSelection::All,
    })
}

async fn update_cell(
    plugin: &std::sync::Arc<PluginState>,
    ctx: &Context,
    params: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let params: atelyx_plugin_protocol::method::TableUpdateCellParams =
        serde_json::from_value(params.clone()).map_err(|e| format!("参数不可解析：{e}"))?;
    let (row_id, field_id) = (params.row_id, params.field_id);
    // null = 清空（不写成显式 null 值，否则行里会多一个无意义键）
    let value = (!params.value.is_null()).then_some(params.value);
    let changed = plugin.with_session_mut(|session| -> Result<bool, String> {
        if !session.data.fields.iter().any(|field| field.id == field_id) {
            return Err(format!("字段不存在：{field_id}"));
        }
        if !session.data.rows.iter().any(|row| row.id == row_id) {
            return Err(format!("行不存在：{row_id}"));
        }
        crate::input::push_undo(session);
        let row = session
            .data
            .rows
            .iter_mut()
            .find(|row| row.id == row_id)
            .expect("已确认行存在");
        let changed = ops::set_cell_value(row, &field_id, value);
        if changed {
            session.touch();
        } else {
            session.view.undo.pop();
        }
        Ok(changed)
    })
    .ok_or("未打开表格，无法写入")??;
    finish(plugin, ctx, changed).await
}

async fn add_row(plugin: &std::sync::Arc<PluginState>, ctx: &Context) -> Result<serde_json::Value, String> {
    let row_id = plugin
        .with_session_mut(|session| {
            let row_id = nanoid::nanoid!();
            crate::input::push_undo(session);
            ops::add_row(&mut session.data, row_id.clone());
            session.touch();
            row_id
        })
        .ok_or("未打开表格，无法新增行")?;
    finish(plugin, ctx, true).await?;
    serde_json::to_value(TableAddRowResult { row_id }).map_err(|e| e.to_string())
}

async fn remove_row(
    plugin: &std::sync::Arc<PluginState>,
    ctx: &Context,
    params: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let params: atelyx_plugin_protocol::method::TableRowIdParams =
        serde_json::from_value(params.clone()).map_err(|e| format!("参数不可解析：{e}"))?;
    let row_id = params.row_id;
    let changed = plugin
        .with_session_mut(|session| -> Result<bool, String> {
            if !session.data.rows.iter().any(|row| row.id == row_id) {
                return Err(format!("行不存在：{row_id}"));
            }
            crate::input::push_undo(session);
            ops::remove_rows(&mut session.data, &[row_id]);
            session.touch();
            crate::input::clear_invalid_selection(session);
            session.rebuild_layout();
            crate::geom::clamp_scroll(session);
            Ok(true)
        })
        .ok_or("未打开表格，无法删除行")??;
    finish(plugin, ctx, changed).await
}

async fn select_row(
    plugin: &std::sync::Arc<PluginState>,
    ctx: &Context,
    params: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let params: atelyx_plugin_protocol::method::TableRowIdParams =
        serde_json::from_value(params.clone()).map_err(|e| format!("参数不可解析：{e}"))?;
    let row_id = params.row_id;
    plugin
        .with_session_mut(|session| -> Result<(), String> {
            let Some(index) = session.data.rows.iter().position(|row| row.id == row_id) else {
                return Err(format!("行不存在：{row_id}"));
            };
            session.view.selection = Some(ops::TableSelection::Row(row_id));
            crate::input::scroll_into_view(session, index, 0);
            Ok(())
        })
        .ok_or("未打开表格，无法选中行")??;
    crate::session::broadcast_selection(plugin, ctx).await;
    plugin.render(ctx).await;
    plugin.publish_header(ctx).await;
    Ok(serde_json::json!({}))
}

async fn set_cell_image(
    plugin: &std::sync::Arc<PluginState>,
    ctx: &Context,
    params: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let params: atelyx_plugin_protocol::method::TableSetCellImageParams =
        serde_json::from_value(params.clone()).map_err(|e| format!("参数不可解析：{e}"))?;
    let (row_id, field_id) = (params.row_id, params.field_id);
    let images = params.images;
    let display = params.display;
    let changed = plugin.with_session_mut(|session| -> Result<bool, String> {
        let Some(field) = session.data.fields.iter().find(|field| field.id == field_id) else {
            return Err(format!("字段不存在：{field_id}"));
        };
        if field.field_type != "image" {
            return Err(format!("字段不是图片类型：{field_id}"));
        }
        if !session.data.rows.iter().any(|row| row.id == row_id) {
            return Err(format!("行不存在：{row_id}"));
        }
        let mut value = serde_json::json!({ "images": images });
        if let Some(display) = display {
            value["display"] = serde_json::Value::String(display);
        }
        crate::input::push_undo(session);
        let changed = session
            .data
            .rows
            .iter_mut()
            .find(|row| row.id == row_id)
            .is_some_and(|row| ops::set_cell_value(row, &field_id, Some(value)));
        if changed {
            session.touch();
        } else {
            session.view.undo.pop();
        }
        Ok(changed)
    })
    .ok_or("未打开表格，无法写入")??;
    finish(plugin, ctx, changed).await
}

/// 写入结算：有改动即排保存、重绘并广播（第三方视图据此重取快照）。
async fn finish(
    plugin: &std::sync::Arc<PluginState>,
    ctx: &Context,
    changed: bool,
) -> Result<serde_json::Value, String> {
    if changed {
        crate::session::notify_changed(plugin, ctx).await;
        crate::session::broadcast_selection(plugin, ctx).await;
        // 与用户编辑同一出口：写后同样排防抖保存
        plugin.schedule_save(ctx);
        plugin.render(ctx).await;
        plugin.publish_header(ctx).await;
    }
    Ok(serde_json::json!({}))
}
