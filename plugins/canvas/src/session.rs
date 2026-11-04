//! 画布会话装配与保存链：当前画布取自宿主打开文件集合；保存走「读盘 → 基线补丁合并 →
//! 写盘队列」，空间模式交宿主按 `canvas` 文件族仲裁。

use atelyx_format::canvas::{
    CANVAS_SCHEMA, CanvasFile, CanvasPatch, apply_canvas_patch, compute_canvas_patch,
};
use atelyx_plugin_protocol::method::{
    CanvasContentParams, HistoryRecordParams, NotificationLevel, SpaceFilePatchParams,
};
use atelyx_plugin_sdk::Context;

use crate::PluginState;
use crate::model::{RefContent, SaveReason, Session};

/// 当前会话的画布路径。
pub fn current_file(plugin: &PluginState) -> Option<String> {
    plugin
        .session
        .lock()
        .ok()
        .and_then(|slot| slot.as_ref().map(|s| s.file.clone()))
}

async fn open_canvas_path(ctx: &Context) -> Option<String> {
    let files = ctx.open_files().await.ok()?;
    files.into_iter().find(|path| path.ends_with(".atlx"))
}

/// 按宿主打开的文件集合载入 / 关闭画布会话。
pub async fn reload_from_open_files(plugin: &PluginState, ctx: &Context) {
    let target = open_canvas_path(ctx).await;
    let previous = current_file(plugin);
    if previous == target {
        return;
    }
    let Some(path) = target else {
        if let Ok(mut slot) = plugin.session.lock() {
            *slot = None;
        }
        if let Some(old) = previous.as_deref() {
            emit_changed(plugin, ctx, old).await;
        }
        return;
    };
    // 改名只换路径不重载：脏会话的未落盘内容与撤销栈属于同一实体
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
                        session.data.title = crate::model::file_title(&path);
                        session.saved.title = session.data.title.clone();
                        true
                    })
                    .unwrap_or(false);
                if migrated {
                    emit_changed(plugin, ctx, &path).await;
                    return;
                }
            }
            fail_open(ctx, &path, &error.to_string()).await;
            if let Ok(mut slot) = plugin.session.lock() {
                *slot = None;
            }
            return;
        }
    };
    let mut data: CanvasFile = match serde_json::from_str(&text) {
        Ok(data) => data,
        Err(error) => {
            fail_open(ctx, &path, &error.to_string()).await;
            if let Ok(mut slot) = plugin.session.lock() {
                *slot = None;
            }
            return;
        }
    };
    if data.schema != CANVAS_SCHEMA {
        fail_open(ctx, &path, &format!("画布 schema 不匹配：{}", data.schema)).await;
        if let Ok(mut slot) = plugin.session.lock() {
            *slot = None;
        }
        return;
    }
    // 内嵌标题以文件名为真源
    data.title = crate::model::file_title(&path);
    // 同一画布换路径（改名 / 移动）：只换路径与标题，保留撤销栈 / 选区 / 缩放
    let migrated = plugin
        .with_session_mut(|session| {
            if session.data.id != data.id {
                return false;
            }
            session.file = path.clone();
            session.data.title = data.title.clone();
            session.saved.title = data.title.clone();
            session.epoch += 1;
            true
        })
        .unwrap_or(false);
    if migrated {
        emit_changed(plugin, ctx, &path).await;
        return;
    }
    let mut session = Session::new(path.clone(), data, 1);
    session.view.self_id = ctx.collab_peers().await.ok().and_then(|peers| peers.self_id);
    session.request_fit();
    crate::chat::sync_card_chats(&mut session);
    crate::session::load_refs(&mut session, ctx).await;
    if let Ok(mut slot) = plugin.session.lock() {
        *slot = Some(session);
    }
    emit_changed(plugin, ctx, &path).await;
}

async fn fail_open(ctx: &Context, path: &str, error: &str) {
    ctx.log_error(&format!("读取画布失败（{path}）：{error}"));
    let _ = ctx
        .notify("打开画布失败", Some(error), NotificationLevel::Error)
        .await;
}

/// 预读文本 / 图片 / 表格卡引用（与打开画布同一读取口径）。
pub async fn load_refs(session: &mut Session, ctx: &Context) {
    for node in &session.data.nodes {
        let Some(path) = node.data.get("file").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if session.view.refs.contains_key(path) {
            continue;
        }
        let content = read_ref(ctx, path).await;
        session.view.refs.insert(path.to_string(), content);
    }
}

async fn read_ref(ctx: &Context, path: &str) -> RefContent {
    let name = path.rsplit('/').next().unwrap_or(path);
    let ext = name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "md" => match ctx.vault_read_text(path).await {
            Ok(text) => RefContent::Text(text),
            Err(_) => RefContent::Missing,
        },
        "atb" => match ctx.vault_read_text(path).await {
            Ok(text) => match serde_json::from_str::<atelyx_format::table::TableFile>(&text) {
                Ok(table) => RefContent::Table {
                    summary: format!("{} 字段 · {} 行", table.fields.len(), table.rows.len()),
                    body: atelyx_format::table::ops::table_to_text(&table),
                },
                Err(_) => RefContent::Missing,
            },
            Err(_) => RefContent::Missing,
        },
        _ if matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp" | "gif") => {
            match ctx.vault_read_binary(path).await {
                Ok(bytes) => match image::load_from_memory(&bytes) {
                    Ok(_) => RefContent::Image,
                    Err(_) => RefContent::Binary,
                },
                Err(_) => RefContent::Missing,
            }
        }
        _ => match ctx.vault_read_text(path).await {
            Ok(text) => RefContent::Text(text),
            Err(_) => RefContent::Missing,
        },
    }
}

/// 为一批新引用补读内容（落卡后调用；已读过的跳过）。
pub async fn load_refs_for(plugin: &PluginState, ctx: &Context, paths: Vec<String>) {
    for path in paths {
        let exists = plugin
            .with_session(|session| session.view.refs.contains_key(&path))
            .unwrap_or(false);
        if exists {
            continue;
        }
        let content = read_ref(ctx, &path).await;
        plugin.with_session_mut(|session| {
            session.view.refs.insert(path.clone(), content);
        });
    }
}

/// 历史回滚 / 空间装载下发的整文件快照：整体替换内容、清空撤销栈，基线保持原值
/// （下次保存的补丁把快照写出去）。
pub fn restore_snapshot(plugin: &PluginState, payload: &serde_json::Value) -> bool {
    let Ok(params) = serde_json::from_value::<CanvasContentParams>(payload.clone()) else {
        return false;
    };
    let Some(path) = params.path.clone().or_else(|| current_file(plugin)) else {
        return false;
    };
    let Ok(mut data) = serde_json::from_str::<CanvasFile>(&params.content) else {
        return false;
    };
    if data.schema != CANVAS_SCHEMA {
        return false;
    }
    data.title = crate::model::file_title(&path);
    plugin
        .with_session_mut(|session| {
            if session.file != path {
                return false;
            }
            session.data = data;
            session.touch();
            session.suppress_history = true;
            session.epoch += 1;
            session.view.undo.clear();
            session.view.redo.clear();
            crate::chat::sync_card_chats(session);
            let live: Vec<String> = session
                .data
                .nodes
                .iter()
                .map(|node| node.id.clone())
                .collect();
            session.view.selection.retain(|id| live.contains(id));
            session.view.locks.retain(|id, _| live.contains(id));
            true
        })
        .unwrap_or(false)
}

/// 合入一条远端补丁并同步会话视图；脏标记只由本地编辑置位，透传帧不改它。
fn merge_remote(
    session: &mut Session,
    patch: &CanvasPatch,
    landed: bool,
) -> Option<atelyx_format::canvas::collab::RemoteApplyOutcome> {
    if patch.id != session.data.id {
        return None;
    }
    let outcome = atelyx_format::canvas::collab::apply_remote_patch(
        &mut session.data,
        &mut session.saved,
        patch,
        landed,
    );
    session.version += 1;
    session.view.undo.clear();
    session.view.redo.clear();
    let live: Vec<String> = session
        .data
        .nodes
        .iter()
        .map(|node| node.id.clone())
        .collect();
    session.view.selection.retain(|id| live.contains(id));
    session.view.locks.retain(|id, _| live.contains(id));
    if session.view.hover.as_ref().is_some_and(|id| !live.contains(id)) {
        session.view.hover = None;
    }
    if session
        .view
        .group_rename
        .as_ref()
        .is_some_and(|(id, _)| !live.contains(id))
    {
        session.view.group_rename = None;
    }
    crate::chat::sync_card_chats(session);
    Some(outcome)
}

/// 空间远端补丁合入（双基线推进）。
pub async fn apply_remote(
    plugin: &PluginState,
    ctx: &Context,
    payload: &serde_json::Value,
) -> bool {
    let Ok(params) = serde_json::from_value::<atelyx_plugin_protocol::method::SpaceFilePatchPayload>(
        payload.clone(),
    ) else {
        return false;
    };
    if params.kind != "canvas" {
        return false;
    }
    let Ok(patch) = serde_json::from_value::<atelyx_format::canvas::CanvasPatch>(params.patch)
    else {
        return false;
    };
    let applied = plugin
        .with_session_mut(|session| merge_remote(session, &patch, params.landed))
        .flatten();
    if let Some(outcome) = applied {
        let paths: Vec<String> = plugin
            .with_session(|session| {
                let mut paths: Vec<String> = Vec::new();
                for path in &outcome.ref_paths {
                    if !session.view.refs.contains_key(path) && !paths.contains(path) {
                        paths.push(path.clone());
                    }
                }
                paths
            })
            .unwrap_or_default();
        for path in paths {
            let content = read_ref(ctx, &path).await;
            plugin.with_session_mut(|session| {
                session.view.refs.insert(path.clone(), content);
            });
        }
        true
    } else {
        false
    }
}

/// 保存（`AutoSave` / `Manual` / `Flush`）：`Ok(false)` = 写盘期间又有编辑未收敛。
pub async fn save(
    plugin: &PluginState,
    ctx: &Context,
    _reason: SaveReason,
) -> Result<bool, String> {
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
        (
            session.file.clone(),
            session.epoch,
            session.saved.clone(),
            session.data.clone(),
        )
    };
    frozen.title = crate::model::file_title(&file);
    // 读盘失败 / 内容不可解析时以本端全量写，并留下可定位原因（不静默覆盖）
    let disk = match ctx.vault_read_text(&file).await {
        Ok(text) => match serde_json::from_str::<CanvasFile>(&text) {
            Ok(canvas) if canvas.schema == CANVAS_SCHEMA => Some(canvas),
            Ok(canvas) => {
                ctx.log_error(&format!("画布 schema 不匹配（{file}）：{}", canvas.schema));
                None
            }
            Err(error) => {
                ctx.log_error(&format!("解析画布失败（{file}）：{error}"));
                None
            }
        },
        Err(error) => {
            ctx.log_error(&format!("读取画布失败（{file}），以本端内容写入：{error}"));
            None
        }
    };
    // 与磁盘真源合并：同一画布才应用本端补丁，外部换了文件则整份写本端
    let mut target = match disk {
        Some(mut disk) if disk.id == frozen.id => {
            if let Some(patch) = compute_canvas_patch(&baseline, &frozen) {
                apply_canvas_patch(&mut disk, &patch);
            }
            disk
        }
        _ => frozen.clone(),
    };
    target.title = frozen.title.clone();
    target.updated_at = crate::now_secs();
    let still_current = plugin
        .with_session(|session| session.epoch == epoch && session.file == file)
        .unwrap_or(false);
    if !still_current {
        return Ok(true);
    }
    let json = serde_json::to_string_pretty(&target).map_err(|error| error.to_string())?;
    ctx.vault_write_text(&file, &json)
        .await
        .map_err(|error| error.to_string())?;
    let stable = plugin
        .with_session_mut(|session| {
            if session.epoch != epoch || session.file != file {
                return None;
            }
            let stable = session.data.nodes == frozen.nodes && session.data.edges == frozen.edges;
            if stable {
                session.data.updated_at = target.updated_at;
                session.saved = session.data.clone();
                session.dirty = false;
                session.save_error = None;
                // 回滚由宿主记 restore 版本：这一笔落盘不再额外记 edit，避免同一内容两条版本
                return Some(std::mem::take(&mut session.suppress_history));
            }
            None
        })
        .flatten();
    if let Some(suppress) = stable {
        crate::service::emit_changed(plugin, ctx).await;
        if !suppress {
            record_history(ctx, &file, &frozen).await;
        }
    }
    Ok(stable.is_some())
}

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
        (
            session.file.clone(),
            session.epoch,
            session.saved.clone(),
            session.data.clone(),
        )
    };
    let params = SpaceFilePatchParams {
        path: file.clone(),
        kind: "canvas".to_string(),
        baseline: serde_json::to_string(&baseline).map_err(|error| error.to_string())?,
        frozen: serde_json::to_string(&frozen).map_err(|error| error.to_string())?,
    };
    let result = ctx
        .space_file_patch(&params)
        .await
        .map_err(|error| error.to_string())?;
    let new_path = if result.path.is_empty() {
        file
    } else {
        result.path
    };
    frozen.title = crate::model::file_title(&new_path);
    let stable = plugin
        .with_session_mut(|session| {
            if session.epoch != epoch {
                return false;
            }
            session.file = new_path;
            session.data.title = frozen.title.clone();
            let stable = session.data.nodes == frozen.nodes && session.data.edges == frozen.edges;
            if stable {
                session.saved = frozen;
                session.dirty = false;
                session.save_error = None;
            }
            stable
        })
        .unwrap_or(false);
    if stable {
        crate::service::emit_changed(plugin, ctx).await;
    }
    Ok(stable)
}

async fn record_history(ctx: &Context, file: &str, frozen: &CanvasFile) {
    let Ok(content) = serde_json::to_string(frozen) else {
        return;
    };
    let params = HistoryRecordParams {
        kind: "canvas".to_string(),
        file: file.to_string(),
        content,
        action: Some("edit".to_string()),
        coalesce_ms: Some(60_000),
        note: None,
    };
    if let Err(error) = ctx.history_record(&params).await {
        ctx.log_error(&format!("记录画布历史失败（{file}）：{error}"));
        let _ = ctx
            .notify(
                "历史版本记录失败",
                Some(&error.to_string()),
                NotificationLevel::Warning,
            )
            .await;
    }
}

/// 当前容器是否协作空间（无法确认时拒绝保存，避免把服务端内容写进本地仓库）。
async fn space_container(ctx: &Context) -> Result<bool, String> {
    let value = ctx
        .call_raw(
            atelyx_plugin_protocol::method::space::ROOM,
            serde_json::Value::Null,
        )
        .await
        .map_err(|error| format!("无法确认容器类型：{error}"))?;
    Ok(value.get("kind").and_then(serde_json::Value::as_str) == Some("space"))
}

pub async fn notify_changed(plugin: &PluginState, ctx: &Context) {
    crate::service::emit_changed(plugin, ctx).await;
}

async fn emit_changed(plugin: &PluginState, ctx: &Context, _path: &str) {
    crate::service::emit_changed(plugin, ctx).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn canvas(node_ids: &[&str]) -> CanvasFile {
        CanvasFile {
            schema: CANVAS_SCHEMA.into(),
            id: "c".into(),
            title: "a".into(),
            nodes: node_ids
                .iter()
                .map(|id| atelyx_format::canvas::CanvasFileNode {
                    id: (*id).into(),
                    node_type: "text".into(),
                    x: 0.0,
                    y: 0.0,
                    width: None,
                    height: None,
                    data: json!({}),
                })
                .collect(),
            edges: Vec::new(),
            created_at: 0,
            updated_at: 0,
        }
    }

    /// 整文件快照只换内容不换基线：否则后续保存按旧基线算出的补丁为空，回滚不会落盘。
    #[test]
    fn restore_snapshot_keeps_baseline_for_next_save() {
        let plugin = PluginState::new();
        *plugin.session.lock().unwrap() = Some(Session::new("a.atlx".into(), canvas(&["a"]), 1));

        let snapshot = canvas(&["a", "b"]);
        let payload = json!({
            "path": "a.atlx",
            "content": serde_json::to_string(&snapshot).unwrap(),
        });
        assert!(restore_snapshot(&plugin, &payload));

        plugin.with_session(|session| {
            assert_eq!(session.data.nodes.len(), 2);
            assert_eq!(session.saved.nodes.len(), 1);
            assert!(session.dirty);
        });
    }

    /// 远端透传帧只改内存数据，不置脏：否则会把对端内容当成本地改动回写。
    #[test]
    fn remote_patch_keeps_dirty_flag_local() {
        let plugin = PluginState::new();
        *plugin.session.lock().unwrap() = Some(Session::new("a.atlx".into(), canvas(&["a"]), 1));

        plugin.with_session_mut(|session| {
            let mut extra = session.data.nodes[0].clone();
            extra.id = "b".into();
            let patch = CanvasPatch {
                id: "c".into(),
                title: None,
                upsert_nodes: vec![extra],
                removed_node_ids: Vec::new(),
                upsert_edges: Vec::new(),
                removed_edge_ids: Vec::new(),
            };
            assert!(merge_remote(session, &patch, false).is_some());
            assert_eq!(session.data.nodes.len(), 2, "透传帧应合入内存");
            assert!(!session.dirty, "透传帧不置脏");

            session.dirty = true;
            assert!(merge_remote(session, &patch, true).is_some());
            assert!(session.dirty, "本地脏标记不被远端覆盖");
        });
    }
}
