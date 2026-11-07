//! 会话装配与本地保存链：当前表格取自宿主打开文件集合；保存走「读盘 → 基线补丁合并 →
//! 写盘队列」，与宿主编辑同一套落盘语义（只认磁盘事实，不覆盖外部改动未触及的实体）。

use atelyx_format::table::{self, TableFile};
use atelyx_plugin_protocol::event;
use atelyx_plugin_protocol::method::HistoryRecordParams;
use atelyx_plugin_sdk::Context;

use crate::model::Session;
use crate::PluginState;

/// 宿主当前打开的表格路径（打开文件集合里唯一的 `.atb`）。
async fn open_table_path(ctx: &Context) -> Option<String> {
    let files = ctx.open_files().await.ok()?;
    files.into_iter().find(|path| path.ends_with(".atb"))
}

/// 当前会话的表格路径。
pub fn current_file(plugin: &PluginState) -> Option<String> {
    plugin
        .session
        .lock()
        .ok()
        .and_then(|session| session.as_ref().map(|session| session.file.clone()))
}

/// 按宿主打开的文件集合载入 / 关闭表格会话。
pub async fn reload_from_open_files(plugin: &PluginState, ctx: &Context) {
    let target = open_table_path(ctx).await;
    let previous = current_file(plugin);
    if previous == target {
        return;
    }
    if let Some(old) = previous.as_deref() {
        crate::presence::publish_leave(plugin, ctx, old).await;
    }
    let Some(path) = target else {
        if let Ok(mut session) = plugin.session.lock() {
            *session = None;
        }
        if let Some(old) = previous.as_deref() {
            emit_changed(ctx, old).await;
        }
        broadcast_selection(plugin, ctx).await;
        return;
    };
    // 改名（本端补丁回执 / 空间远端改标题）只换路径，不重载：脏会话里的编辑与撤销栈
    // 仍属于同一实体，清会话会把未落盘内容丢掉
    let dirty_id = plugin
        .with_session(|session| session.dirty.then(|| session.data.id.clone()))
        .flatten();
    let text = match ctx.vault_read_text(&path).await {
        Ok(text) => text,
        Err(error) => {
            if let Some(id) = dirty_id.as_deref() {
                let migrated = plugin
                    .with_session_mut(|session| {
                        if session.data.id != id {
                            return false;
                        }
                        session.file = path.clone();
                        session.data.title = title_of(&path);
                        session.saved.title = session.data.title.clone();
                        true
                    })
                    .unwrap_or(false);
                if migrated {
                    broadcast_selection(plugin, ctx).await;
                    emit_changed(ctx, &path).await;
                    return;
                }
            }
            ctx.log_error(&format!("读取表格失败（{path}）：{error}"));
            let _ = ctx
                .notify(
                    "打开表格失败",
                    Some(&error.to_string()),
                    atelyx_plugin_protocol::method::NotificationLevel::Error,
                )
                .await;
            if let Ok(mut session) = plugin.session.lock() {
                *session = None;
            }
            return;
        }
    };
    let mut data: TableFile = match serde_json::from_str(&text) {
        Ok(data) => data,
        Err(error) => {
            ctx.log_error(&format!("表格解析失败（{path}）：{error}"));
            let _ = ctx
                .notify(
                    "打开表格失败",
                    Some(&error.to_string()),
                    atelyx_plugin_protocol::method::NotificationLevel::Error,
                )
                .await;
            if let Ok(mut session) = plugin.session.lock() {
                *session = None;
            }
            return;
        }
    };
    for row in &mut data.rows {
        atelyx_format::table::ops::normalize_table_row(row);
    }
    // 内嵌标题以文件名为真源（改名后旧标题不可信）
    data.title = title_of(&path);
    let migrated = dirty_id.as_deref().is_some_and(|id| {
        plugin
            .with_session_mut(|session| {
                if session.data.id != id || data.id != id {
                    return false;
                }
                session.file = path.clone();
                session.data.title = data.title.clone();
                session.saved.title = data.title.clone();
                true
            })
            .unwrap_or(false)
    });
    if migrated {
        broadcast_selection(plugin, ctx).await;
        emit_changed(ctx, &path).await;
        return;
    }
    // 确实换了另一张表：先关旧会话，载入失败时界面不能继续显示上一张表，更不能按旧路径写回
    if let Ok(mut session) = plugin.session.lock() {
        *session = None;
    }
    let mut session = Session::new(path, data);
    if let Ok(size) = plugin.surface_size.lock() {
        session.view.canvas_size = *size;
    }
    if let Ok(mut slot) = plugin.session.lock() {
        *slot = Some(session);
    }
    broadcast_selection(plugin, ctx).await;
    notify_changed(plugin, ctx).await;
}

/// 选区变更的唯一出口：宿主事件与协作在场一起发（上行节流在 presence 内）。
pub async fn broadcast_selection(plugin: &PluginState, ctx: &Context) {
    crate::presence::publish_selection(plugin, ctx).await;
    notify_selection(plugin, ctx).await;
}

/// 宿主历史回滚交回的快照：整体替换数据并置脏（撤销可回退这一步）。
pub fn restore_snapshot(plugin: &PluginState, path: &str, content: &str) -> bool {
    let Ok(mut slot) = plugin.session.lock() else { return false };
    let Some(session) = slot.as_mut() else { return false };
    if session.file != path {
        return false;
    }
    let Ok(mut data) = serde_json::from_str::<TableFile>(content) else { return false };
    for row in &mut data.rows {
        atelyx_format::table::ops::normalize_table_row(row);
    }
    data.title = session.data.title.clone();
    session.view.undo.push((session.data.fields.clone(), session.data.rows.clone()));
    if session.view.undo.len() > crate::model::UNDO_DEPTH {
        session.view.undo.remove(0);
    }
    session.data = data;
    session.view.redo.clear();
    session.view.editing = None;
    session.view.selection = None;
    session.touch();
    // 回滚后的行 / 列集合变了：滚动位收回新几何的合法区间
    crate::geom::clamp_scroll(session);
    true
}

/// 远端补丁合入结果：是否改动数据；在途草稿绑定的行 / 字段被远端删除而丢弃。
#[derive(Clone, Copy, Debug, Default)]
pub struct RemotePatchOutcome {
    pub applied: bool,
    pub draft_lost: bool,
}

/// 空间远端补丁合入：作废撤销栈（栈内快照是合入前的本端全量数据）。
/// 草稿按稳定 id 跟随实体；实体被远端删除时丢弃草稿并由调用方提示，不能静默写进别的格。
pub fn apply_remote_patch(
    plugin: &PluginState,
    path: &str,
    patch: &serde_json::Value,
    landed: bool,
) -> RemotePatchOutcome {
    let Ok(mut slot) = plugin.session.lock() else { return RemotePatchOutcome::default() };
    let Some(session) = slot.as_mut() else { return RemotePatchOutcome::default() };
    if session.file != path {
        return RemotePatchOutcome::default();
    }
    let Ok(patch) = serde_json::from_value::<table::TablePatch>(patch.clone()) else {
        return RemotePatchOutcome::default();
    };
    if patch.id != session.data.id {
        return RemotePatchOutcome::default();
    }
    // 回声补丁（同 id 同内容）不触发布局重建与重绘：按 id 差量比对，空补丁直接丢弃
    if !patch_changes(&session.data, &patch) {
        return RemotePatchOutcome::default();
    }
    let editing = session
        .view
        .editing
        .as_ref()
        .map(|edit| (edit.row_id.clone(), edit.field_id.clone()));
    table::apply_table_patch(&mut session.data, &patch);
    session.version += 1;
    let draft_lost = editing.is_some_and(|(row_id, field_id)| {
        let gone = !session.data.rows.iter().any(|row| row.id == row_id)
            || !session.data.fields.iter().any(|field| field.id == field_id);
        if gone {
            session.view.editing = None;
            session.view.edit_session = None;
            session.view.editor_drag = false;
        }
        gone
    });
    session.rebuild_layout();
    crate::input::clear_invalid_selection(session);
    // 行 / 列集合变了：滚动位收回新几何的合法区间
    crate::geom::clamp_scroll(session);
    session.view.undo.clear();
    session.view.redo.clear();
    if landed && !session.dirty {
        session.saved = session.data.clone();
    }
    RemotePatchOutcome { applied: true, draft_lost }
}

/// 补丁是否真会改动数据（按 id 比对内容；顺序变更按 id 序列判定）。
/// 字段必须覆盖 apply 会写到的每一处，否则该字段的远端更新会被当成回声丢掉。
fn patch_changes(data: &TableFile, patch: &table::TablePatch) -> bool {
    if patch.title.as_ref().is_some_and(|title| *title != data.title) {
        return true;
    }
    let field = |id: &str| data.fields.iter().find(|field| field.id == id);
    let row = |id: &str| data.rows.iter().find(|row| row.id == id);
    patch.upsert_fields.iter().any(|next| field(&next.id) != Some(next))
        || patch.upsert_rows.iter().any(|next| row(&next.id) != Some(next))
        || patch.removed_field_ids.iter().any(|id| field(id).is_some())
        || patch.removed_row_ids.iter().any(|id| row(id).is_some())
        || patch.field_order.as_ref().is_some_and(|order| {
            order.len() != data.fields.len()
                || order.iter().zip(&data.fields).any(|(id, field)| *id != field.id)
        })
        || patch.row_order.as_ref().is_some_and(|order| {
            order.len() != data.rows.len()
                || order.iter().zip(&data.rows).any(|(id, row)| *id != row.id)
        })
}

/// 文件名主名（去扩展名）：内嵌标题的唯一真源。
pub fn title_of(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(name).to_string()
}

/// 保存触发原因：自动保存不打断在途编辑；手动保存与 flush 先提交草稿。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveReason {
    Auto,
    Manual,
    Flush,
}

/// 保存：空间模式交服务端补丁仲裁；本地模式读盘合并后写盘并记历史。
/// 返回本次冻结快照是否已是当前数据（false = 写盘期间又有编辑，调用方保持脏）。
pub async fn save(
    plugin: &PluginState,
    ctx: &Context,
    reason: SaveReason,
) -> Result<bool, String> {
    if reason != SaveReason::Auto {
        commit_draft(plugin);
    }
    if space_container(ctx).await? {
        return save_space(plugin, ctx).await;
    }
    let (file, epoch, baseline, mut frozen) = {
        let Ok(slot) = plugin.session.lock() else {
            return Err("会话状态不可用".to_string());
        };
        let Some(session) = slot.as_ref() else {
            return Ok(true);
        };
        if !session.dirty {
            return Ok(true);
        }
        (session.file.clone(), session.epoch, session.saved.clone(), session.data.clone())
    };
    frozen.title = title_of(&file);
    let mut target = match ctx.vault_read_text(&file).await {
        Ok(text) => match serde_json::from_str::<TableFile>(&text) {
            // 磁盘仍是同一实体：把本端改动按实体合并上去
            Ok(mut disk) if disk.id == frozen.id => {
                if let Some(patch) = table::compute_table_patch(&baseline, &frozen) {
                    table::apply_table_patch(&mut disk, &patch);
                }
                disk
            }
            // 盘上换了个文件（被替换 / 删了重建）：以本端全量为准
            Ok(_) => frozen.clone(),
            Err(error) => {
                // 读到盘上内容但解析失败：以本端全量覆盖，失败原因必须留痕（不静默覆盖）
                ctx.log_error(&format!("读表格磁盘内容失败，以本端为准覆盖（{file}）：{error}"));
                frozen.clone()
            }
        },
        // 读盘失败（瞬时 IO / 权限）：留痕后以本端为准，错误本体是唯一定位线索
        Err(error) => {
            ctx.log_error(&format!("读表格磁盘失败，以本端为准覆盖（{file}）：{error}"));
            frozen.clone()
        }
    };
    target.title = title_of(&file);
    target.updated_at = now_secs();
    // 读盘到写盘之间切了仓库 / 换了文件：这次快照已不属于当前会话，不能写进新容器
    let still_current = plugin
        .with_session(|session| session.epoch == epoch && session.file == file)
        .unwrap_or(false);
    if !still_current {
        return Ok(true);
    }
    let json = serde_json::to_string_pretty(&target).map_err(|e| e.to_string())?;
    ctx.vault_write_text(&file, &json).await.map_err(|e| e.to_string())?;
    let stable = settle(plugin, &file, epoch, frozen.clone());
    if stable {
        let content = serde_json::to_string(&frozen).map_err(|e| e.to_string())?;
        let params = HistoryRecordParams {
            kind: "table".to_string(),
            file: file.clone(),
            content,
            action: Some("edit".to_string()),
            coalesce_ms: Some(60_000),
            note: None,
        };
        if let Err(error) = ctx.history_record(&params).await {
            ctx.log_error(&format!("记录表格历史失败：{error}"));
            let _ = ctx
                .notify(
                    "历史版本记录失败",
                    Some(&error.to_string()),
                    atelyx_plugin_protocol::method::NotificationLevel::Error,
                )
                .await;
        }
    }
    Ok(stable)
}

/// 当前墙钟秒（表格 `updatedAt` 与宿主同一口径）。
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

/// 通知宿主「当前表格内容已结算」：第三方表格视图据此重取快照。
pub async fn notify_changed(plugin: &PluginState, ctx: &Context) {
    let Some(path) = current_file(plugin) else { return };
    emit_changed(ctx, &path).await;
}

/// 按显式路径广播变更：关闭表格后当前文件已不在，订阅方仍要知道旧表已失效。
async fn emit_changed(ctx: &Context, path: &str) {
    let payload = serde_json::json!({ "path": path });
    if let Err(error) = ctx.event_emit(event::name::TABLE_CHANGED, payload).await {
        ctx.log_error(&format!("发布表格变更失败：{error}"));
    }
}

/// 选区快照（宿主与第三方表格视图据此对齐选中格，含其他协作者的选区）。
pub fn selection_payload(plugin: &crate::PluginState) -> Option<serde_json::Value> {
    plugin.with_session(|session| {
        let selection = session
            .view
            .selection
            .as_ref()
            .and_then(|selection| serde_json::to_value(to_wire(selection)).ok());
        let remote_selections = plugin
            .presence
            .lock()
            .map(|presence| {
                presence.remote_selections(&session.data.fields, &session.data.rows)
            })
            .unwrap_or_default();
        serde_json::json!({
            "path": session.file,
            "selection": selection,
            "remoteSelections": remote_selections,
        })
    })
}

/// 本地选区 → 协议选区（字段 / 行 id 原样，宿主不解释）。
fn to_wire(selection: &atelyx_format::table::ops::TableSelection) -> serde_json::Value {
    use atelyx_format::table::ops::TableSelection;
    match selection {
        TableSelection::Cell { row_id, field_id } => {
            serde_json::json!({ "kind": "cell", "rowId": row_id, "fieldId": field_id })
        }
        TableSelection::Range { anchor_row_id, anchor_field_id, row_id, field_id } => {
            serde_json::json!({
                "kind": "range",
                "anchorRowId": anchor_row_id,
                "anchorFieldId": anchor_field_id,
                "rowId": row_id,
                "fieldId": field_id
            })
        }
        TableSelection::Row(row_id) => serde_json::json!({ "kind": "row", "rowId": row_id }),
        TableSelection::Column(field_id) => {
            serde_json::json!({ "kind": "column", "fieldId": field_id })
        }
        TableSelection::All => serde_json::json!({ "kind": "all" }),
    }
}

/// 只把选区变化发给宿主（内容不变、只有选区移动也发）；外部一律经 [`broadcast_selection`]，
/// 避免漏掉协作上行。
async fn notify_selection(plugin: &crate::PluginState, ctx: &Context) {
    // 无打开表格也要发一次空载荷：订阅方据此知道上一张表的选区已失效
    let payload = selection_payload(plugin).unwrap_or_else(|| serde_json::json!({}));
    if let Err(error) = ctx
        .event_emit(event::name::TABLE_SELECTION_CHANGED, payload)
        .await
    {
        ctx.log_error(&format!("发布表格选区失败：{error}"));
    }
}

/// 空间模式保存：基线 + 冻结快照交宿主 `space.file.patch`，回执带服务端最终路径
/// （补丁带标题时服务端按标题改名），随后把基线推进到冻结快照。
async fn save_space(plugin: &PluginState, ctx: &Context) -> Result<bool, String> {
    let (file, epoch, baseline, mut frozen) = {
        let Ok(slot) = plugin.session.lock() else {
            return Err("会话状态不可用".to_string());
        };
        let Some(session) = slot.as_ref() else {
            return Ok(true);
        };
        if !session.dirty {
            return Ok(true);
        }
        (session.file.clone(), session.epoch, session.saved.clone(), session.data.clone())
    };
    let params = atelyx_plugin_protocol::method::SpaceFilePatchParams {
        path: file.clone(),
        kind: "table".to_string(),
        baseline: serde_json::to_string(&baseline).map_err(|e| e.to_string())?,
        frozen: serde_json::to_string(&frozen).map_err(|e| e.to_string())?,
    };
    let result = ctx
        .space_file_patch(&params)
        .await
        .map_err(|e| e.to_string())?;
    let new_path = if result.path.is_empty() { file } else { result.path };
    frozen.title = title_of(&new_path);
    let mut stable = false;
    if let Ok(mut slot) = plugin.session.lock() {
        if let Some(session) = slot.as_mut().filter(|session| session.epoch == epoch) {
            if session.file != new_path {
                session.file = new_path.clone();
            }
            // 路径可能没变而标题变了（服务端按标题改名）：标题始终跟随冻结快照
            session.data.title = frozen.title.clone();
            // 稳定判定只看正文（标题随路径变化不算编辑）
            stable = session.data.fields == frozen.fields && session.data.rows == frozen.rows;
            if stable {
                session.saved = frozen.clone();
                session.dirty = false;
                session.save_error = None;
            } else {
                session.save_error = None;
            }
        }
    }
    Ok(stable)
}

/// 当前容器是否协作空间（`space.room` 的 `kind`）。容器类型无法确认时返回错误：
/// 此时按本地写会把服务端内容落进用户仓库，宁可保存失败让用户重试。
async fn space_container(ctx: &Context) -> Result<bool, String> {
    let value = ctx
        .call_raw(atelyx_plugin_protocol::method::space::ROOM, serde_json::Value::Null)
        .await
        .map_err(|e| format!("无法确认容器类型：{e}"))?;
    Ok(value
        .get("kind")
        .and_then(|kind| kind.as_str())
        .is_some_and(|kind| kind == "space"))
}

/// 保存结算：**只有会话仍是发起保存的那一份、且正文仍是这份快照时**才推进基线并清脏。
/// 写在途期间用户继续编辑（data 已变）时保留脏，否则那次编辑会被这次结算吞掉。
fn settle(plugin: &PluginState, file: &str, epoch: u64, frozen: table::TableFile) -> bool {
    let Ok(mut slot) = plugin.session.lock() else { return false };
    let Some(session) = slot.as_mut().filter(|session| session.epoch == epoch) else {
        return false;
    };
    if session.file != file {
        return false;
    }
    let unchanged = session.data.fields == frozen.fields && session.data.rows == frozen.rows;
    session.save_error = None;
    if unchanged {
        session.data.title = frozen.title.clone();
        session.saved = frozen;
        session.dirty = false;
    }
    unchanged
}

/// 保存前提交在途单元格草稿：草稿也是未落盘内容，不能随保存被跳过。
fn commit_draft(plugin: &PluginState) {
    let Ok(mut slot) = plugin.session.lock() else { return };
    if let Some(session) = slot.as_mut() {
        if session.view.editing.is_some() {
            crate::edit::commit(session);
        }
    }
}

#[cfg(test)]
mod tests {
    use atelyx_format::table::ops;

    use super::*;
    use crate::model::Size;

    fn table(rows: usize) -> TableFile {
        let rows: Vec<serde_json::Value> = (0..rows)
            .map(|index| serde_json::json!({ "id": format!("r{index}"), "values": { "f": "x" } }))
            .collect();
        serde_json::from_value(serde_json::json!({
            "schema": "atelyx-table/v1",
            "id": "t",
            "title": "T",
            "fields": [{ "id": "f", "name": "A", "type": "text" }],
            "rows": rows,
        }))
        .expect("表格夹具")
    }

    /// 历史回滚换了行 / 列集合：滚动位要收回新几何，选区随旧行失效一并清掉。
    #[test]
    fn restore_clamps_scroll_and_clears_selection() {
        let plugin = PluginState::new();
        let mut session = Session::new("t.atb".into(), table(40));
        session.view.canvas_size = Size { width: 200.0, height: 120.0 };
        session.view.scroll_x = 5_000.0;
        session.view.scroll_y = 5_000.0;
        session.view.selection = Some(ops::TableSelection::Cell {
            row_id: "r39".into(),
            field_id: "f".into(),
        });
        *plugin.session.lock().expect("会话锁") = Some(session);

        let content = serde_json::to_string(&table(2)).expect("快照序列化");
        assert!(restore_snapshot(&plugin, "t.atb", &content), "同路径快照应被接受");

        let (selection, scroll_x, scroll_y, max_x, max_y) = plugin
            .with_session(|session| {
                let geo = crate::geom::geo(
                    &session.view.layout,
                    session.view.scroll_x,
                    session.view.scroll_y,
                    session.view.zoom,
                    session.view.canvas_size,
                )
                .expect("回滚后应重建布局");
                (
                    session.view.selection.is_none(),
                    session.view.scroll_x,
                    session.view.scroll_y,
                    geo.max_scroll_x(),
                    geo.max_scroll_y(),
                )
            })
            .expect("会话仍在");
        assert!(selection, "回滚清掉对旧行的选区");
        assert_eq!(scroll_x, max_x);
        assert_eq!(scroll_y, max_y);
    }

    /// 编辑在途 + 远端在顶部插行：草稿绑定行 id，提交落到原行而不是旧行索引。
    #[test]
    fn draft_commit_follows_row_id_after_remote_insert() {
        let plugin = PluginState::new();
        let mut session = Session::new("t.atb".into(), table(2));
        crate::edit::begin(&mut session, 1, 0, Some("改".into()));
        *plugin.session.lock().expect("会话锁") = Some(session);

        let patch = serde_json::json!({
            "id": "t",
            "upsertRows": [{ "id": "r9", "values": { "f": "远端" } }],
            "rowOrder": ["r9", "r0", "r1"],
        });
        let outcome = apply_remote_patch(&plugin, "t.atb", &patch, false);
        assert!(outcome.applied);
        assert!(!outcome.draft_lost, "被编辑行仍在，草稿应保留");

        plugin
            .with_session_mut(|session| crate::edit::commit(session))
            .expect("会话仍在");
        let values = plugin
            .with_session(|session| {
                session.data.rows.iter()
                    .find(|row| row.id == "r1")
                    .and_then(|row| row.values.get("f").cloned())
            })
            .expect("会话仍在");
        assert_eq!(values, Some(serde_json::json!("改")), "草稿写回原行");
        assert!(
            plugin
                .with_session(|session| session.data.rows[1].values.get("f") == Some(&serde_json::json!("x")))
                .expect("会话仍在"),
            "插行后的旧索引行未被写错"
        );
    }

    /// 编辑在途 + 远端删掉被编辑行：草稿丢弃并标记，不能静默写进相邻格。
    #[test]
    fn draft_dropped_when_remote_removes_edited_row() {
        let plugin = PluginState::new();
        let mut session = Session::new("t.atb".into(), table(2));
        crate::edit::begin(&mut session, 1, 0, Some("改".into()));
        *plugin.session.lock().expect("会话锁") = Some(session);

        let patch = serde_json::json!({
            "id": "t",
            "removedRowIds": ["r1"],
            "rowOrder": ["r0"],
        });
        let outcome = apply_remote_patch(&plugin, "t.atb", &patch, false);
        assert!(outcome.applied);
        assert!(outcome.draft_lost, "被编辑行已删除，草稿应丢弃");

        let (editing, wrote) = plugin
            .with_session(|session| {
                (
                    session.view.editing.is_none(),
                    session.data.rows.iter().any(|row| row.values.get("f") == Some(&serde_json::json!("改"))),
                )
            })
            .expect("会话仍在");
        assert!(editing, "编辑态随目标消失而结束");
        assert!(!wrote, "草稿未写进任何现存行");
    }
}
