//! AI 表格 / 画布编辑出口：补丁按稳定 id 合并，打开会话先合入再走保存链。
//! 历史一律 editor=agent 且不参与连续编辑合并；打开会话合入后撤销栈整体作废（同远端补丁落地）。

use tokio::sync::oneshot;

use atelyx_core::ai::doc_edit::{self, PlanError};
use atelyx_core::ai::edit_host::{CanvasOp, EditActor, TableOp};
use atelyx_core::history::{HistoryAction, HistoryEditor, HistoryKind, RecordOpts};
use atelyx_core::space_client::{HistoryAuthorReq, HistoryRecordReq, SpaceError};
use atelyx_core::vault::{CanvasFile, CanvasPatch, TableFile};

use crate::canvas_view::SaveReason as CanvasSaveReason;
use crate::table_view::SaveReason as TableSaveReason;
use crate::{App, Message, RepoKind};

/// 一次补丁落地后的我方落点：磁盘真源（本地）或补丁端点返回值（空间）。
pub fn apply_table(
    app: &mut App,
    path: String,
    ops: Vec<TableOp>,
    actor: EditActor,
    reply: oneshot::Sender<Result<String, String>>,
) -> iced::Task<Message> {
    match &app.repo_kind {
        RepoKind::Local => apply_local_table(app, path, ops, actor, reply),
        RepoKind::Space(_) => apply_space_table(app, path, ops, actor, reply),
    }
}

/// 应用一次画布补丁（打开会话先合入；本地写盘 / 空间补丁端点 + 历史）。
pub fn apply_canvas(
    app: &mut App,
    path: String,
    ops: Vec<CanvasOp>,
    actor: EditActor,
    reply: oneshot::Sender<Result<String, String>>,
) -> iced::Task<Message> {
    match &app.repo_kind {
        RepoKind::Local => apply_local_canvas(app, path, ops, actor, reply),
        RepoKind::Space(_) => apply_space_canvas(app, path, ops, actor, reply),
    }
}

// ===== 本地仓库 =====

/// 本地表格：操作作用在用户眼前的模型（打开会话含未保存编辑）；补丁按 id 合入磁盘真源后写盘。
fn apply_local_table(
    app: &mut App,
    path: String,
    ops: Vec<TableOp>,
    actor: EditActor,
    reply: oneshot::Sender<Result<String, String>>,
) -> iced::Task<Message> {
    let gen = app.epoch();
    let Ok(root) = app.vault.root() else {
        return fail(app, reply, "仓库未打开".to_string());
    };
    let title = doc_edit::file_label(&path);
    let disk = match doc_edit::read_local_or_new::<TableFile>(&app.vault, &root, &path, &title) {
        Ok(model) => model,
        Err(e) => return fail(app, reply, format!("表格不存在或不可读：{path}（{e}）")),
    };
    let open = app.table.as_ref().filter(|s| s.file == path);
    // 打开会话：补丁以保存基线为源（用户未保存编辑随本次落盘一并上行）；未打开：以磁盘真源为源
    let baseline = open.map(|s| s.saved.clone()).unwrap_or_else(|| disk.clone());
    let base = open.map(|s| s.data.clone()).unwrap_or_else(|| disk.clone());
    let updated_at = crate::now_secs() as i64;
    let planned = match doc_edit::plan(&baseline, &base, &ops, &title, updated_at) {
        Ok(planned) => planned,
        Err(PlanError::Unchanged) => {
            let _ = reply.send(Ok(format!("表格未变化：{path}（操作与现值相同）")));
            return iced::Task::none();
        }
        Err(PlanError::Ops(e)) => return fail(app, reply, e),
    };
    // 打开会话先合入：用户未保存编辑保留，随后整份落盘（与既有保存链同口径）
    merge_table_into_session(app, &path, &planned.after, updated_at);
    let ok_text = format!("已编辑 {path}（{} 项操作：{}）", ops.len(), planned.summary);
    let queue = app.vault.writes.clone();
    let state = app.vault.clone();
    let author = actor.author.clone();
    let editor = editor_of(&actor);
    crate::ai_edit::begin_presence_for(app, "table", &path, &actor, &ok_text);
    let rel = path.clone();
    let frozen = planned.after.clone();
    let frozen_msg = planned.after.clone();
    let patch = planned.patch.clone();
    let expected_id = planned.after.id.clone();
    let reply_text = ok_text.clone();
    iced::Task::perform(
        async move {
            let outcome = async {
                let written = doc_edit::commit_local::<TableFile>(
                    &queue,
                    &root,
                    &rel,
                    &expected_id,
                    &frozen,
                    &patch,
                    &title,
                    updated_at,
                )
                .await?;
                let file = atelyx_core::vault::safe_join(&root, &rel, false)?;
                atelyx_core::vault::cache_put_table(&state, &file, &rel, &written);
                let content = base_json(&written);
                record_table_version(root.clone(), rel.clone(), content, author, editor).await;
                Ok::<TableFile, String>(written)
            }
            .await;
            let _ = reply.send(match &outcome {
                Ok(_) => Ok(reply_text),
                Err(e) => Err(format!("写入失败：{e}")),
            });
            let (result, body) = match outcome {
                Ok(written) => (Ok(()), written),
                Err(e) => (Err(e), frozen_msg),
            };
            Message::Table(crate::table_view::TableMsg::TableSaved(
                gen,
                rel,
                TableSaveReason::AutoSave,
                result,
                body,
            ))
        },
        |m| m,
    )
    .chain(iced::Task::done(Message::AiEditDone(gen, path, String::new(), None)))
}

/// 本地画布：口径同 apply_local_table（画布额外收敛视图态与引用补读）。
fn apply_local_canvas(
    app: &mut App,
    path: String,
    ops: Vec<CanvasOp>,
    actor: EditActor,
    reply: oneshot::Sender<Result<String, String>>,
) -> iced::Task<Message> {
    let gen = app.epoch();
    let Ok(root) = app.vault.root() else {
        return fail(app, reply, "仓库未打开".to_string());
    };
    let title = doc_edit::file_label(&path);
    let disk = match doc_edit::read_local_or_new::<CanvasFile>(&app.vault, &root, &path, &title) {
        Ok(model) => model,
        Err(e) => return fail(app, reply, format!("画布不存在或不可读：{path}（{e}）")),
    };
    let open = app.canvas.as_ref().filter(|s| s.file == path);
    let baseline = open.map(|s| s.saved.clone()).unwrap_or_else(|| disk.clone());
    let base = open.map(|s| s.data.clone()).unwrap_or_else(|| disk.clone());
    let updated_at = crate::now_secs() as i64;
    let planned = match doc_edit::plan(&baseline, &base, &ops, &title, updated_at) {
        Ok(planned) => planned,
        Err(PlanError::Unchanged) => {
            let _ = reply.send(Ok(format!("画布未变化：{path}（操作与现值相同）")));
            return iced::Task::none();
        }
        Err(PlanError::Ops(e)) => return fail(app, reply, e),
    };
    let merge = merge_canvas_into_session(app, &path, &planned.after, &planned.patch, updated_at);
    let ok_text = format!(
        "已编辑 {path}（{} 项操作：{}）；实体：{}",
        ops.len(),
        planned.summary,
        touched_ids(&planned.patch).join("、")
    );
    let queue = app.vault.writes.clone();
    let state = app.vault.clone();
    let author = actor.author.clone();
    let editor = editor_of(&actor);
    crate::ai_edit::begin_presence_for(app, "canvas", &path, &actor, &ok_text);
    let rel = path.clone();
    let frozen = planned.after.clone();
    let frozen_msg = planned.after.clone();
    let patch = planned.patch.clone();
    let expected_id = planned.after.id.clone();
    let reply_text = ok_text.clone();
    merge.chain(
        iced::Task::perform(
            async move {
                let outcome = async {
                    let written = doc_edit::commit_local::<CanvasFile>(
                        &queue,
                        &root,
                        &rel,
                        &expected_id,
                        &frozen,
                        &patch,
                        &title,
                        updated_at,
                    )
                    .await?;
                    let file = atelyx_core::vault::safe_join(&root, &rel, false)?;
                    atelyx_core::vault::cache_put_canvas(&state, &file, &rel, &written);
                    let content = base_json(&written);
                    record_canvas_version(root.clone(), rel.clone(), content, author, editor).await;
                    Ok::<CanvasFile, String>(written)
                }
                .await;
                let _ = reply.send(match &outcome {
                    Ok(_) => Ok(reply_text),
                    Err(e) => Err(format!("写入失败：{e}")),
                });
                let (result, body) = match outcome {
                    Ok(written) => (Ok(()), written),
                    Err(e) => (Err(e), frozen_msg),
                };
                Message::Canvas(crate::canvas_view::CanvasMsg::Saved(
                    gen,
                    rel,
                    CanvasSaveReason::AutoSave,
                    result,
                    body,
                ))
            },
            |m| m,
        ),
    )
    .chain(iced::Task::done(Message::AiEditDone(gen, path, String::new(), None)))
}

/// 合入打开中的表格会话：会话模型即操作后的模型（含用户未保存编辑），撤销栈整体作废。
fn merge_table_into_session(app: &mut App, path: &str, after: &TableFile, updated_at: i64) {
    let Some(s) = app.table.as_mut().filter(|s| s.file == path) else {
        return;
    };
    s.data = after.clone();
    s.data.updated_at = updated_at;
    s.version += 1;
    s.dirty = true;
    s.save_error = None;
    s.rebuild_layout();
    // 合入后撤销栈内是合入前的全量快照：按 Ctrl+Z 会把刚落盘内容回滚并随下次保存广播（内容丢失）
    s.view.undo.clear();
    s.view.redo.clear();
    // 打开中的表格内容变更属宿主发起的编辑结算：与人工保存同口径通知插件
    app.plugins.on_table_changed(Some(path));
}

/// 补丁合入打开中的画布会话并收敛视图态（选中 / 悬停 / 改名目标 / 独占锁），返回引用补读任务。
fn merge_canvas_into_session(
    app: &mut App,
    path: &str,
    after: &CanvasFile,
    patch: &CanvasPatch,
    updated_at: i64,
) -> iced::Task<Message> {
    let mut ref_paths: Vec<String> = patch
        .upsert_nodes
        .iter()
        .filter_map(|n| {
            n.data
                .get("file")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .collect();
    ref_paths.sort();
    ref_paths.dedup();
    let text_changed = patch.upsert_nodes.iter().any(|n| n.node_type == "text");
    {
        let Some(s) = app.canvas.as_mut().filter(|s| s.file == path) else {
            return iced::Task::none();
        };
        s.data = after.clone();
        s.data.updated_at = updated_at;
        s.version += 1;
        s.dirty = true;
        s.save_error = None;
        s.view.content_cache.clear();
        s.view.undo.clear();
        s.view.redo.clear();
        let nodes = &s.data.nodes;
        s.view.selection.retain(|id| nodes.iter().any(|n| &n.id == id));
        if s.view.hover.as_ref().is_some_and(|id| !nodes.iter().any(|n| &n.id == id)) {
            s.view.hover = None;
        }
        if s
            .view
            .group_rename
            .as_ref()
            .is_some_and(|(id, _)| !nodes.iter().any(|n| &n.id == id))
        {
            s.view.group_rename = None;
        }
        for id in &patch.removed_node_ids {
            s.view.locks.remove(id);
        }
    }
    crate::canvas_chat::sync_card_chats(app);
    if text_changed {
        if let Some(s) = app.canvas.as_mut() {
            crate::canvas_actions::rebuild_text_docs(s);
        }
    }
    crate::canvas_actions::load_refs(app, ref_paths)
}

// ===== 协作空间 =====

/// 空间表格：打开会话按「保存基线 → 合入后会话模型」算一次补丁（用户未保存编辑一并上行），
/// 未打开则读服务端真源后算补丁；落盘走补丁端点（服务端按 id 仲裁并广播）。
fn apply_space_table(
    app: &mut App,
    path: String,
    ops: Vec<TableOp>,
    actor: EditActor,
    reply: oneshot::Sender<Result<String, String>>,
) -> iced::Task<Message> {
    let gen = app.epoch();
    let RepoKind::Space(session) = &app.repo_kind else {
        return fail(app, reply, "仓库类型不匹配".to_string());
    };
    if !session.role.can_write() {
        return fail(app, reply, "该空间为只读身份，无法编辑".to_string());
    }
    let client = session.client.clone();
    let space_id = session.space_id.clone();
    let open = app.table.as_ref().filter(|s| s.file == path);
    let Some(s) = open else {
        return apply_space_table_from_server(
            app, gen, path, ops, actor, client, space_id, reply,
        );
    };
    let baseline = s.saved.clone();
    let base = s.data.clone();
    let title = doc_edit::file_label(&path);
    let updated_at = crate::now_secs() as i64;
    let planned = match doc_edit::plan(&baseline, &base, &ops, &title, updated_at) {
        Ok(planned) => planned,
        Err(PlanError::Unchanged) => {
            let _ = reply.send(Ok(format!("表格未变化：{path}（操作与现值相同）")));
            return iced::Task::none();
        }
        Err(PlanError::Ops(e)) => return fail(app, reply, e),
    };
    let patch = planned.patch;
    let after = planned.after;
    let summary = planned.summary;
    merge_table_into_session(app, &path, &after, updated_at);
    let ok_text = format!("已编辑 {path}（{} 项操作：{summary}）", ops.len());
    let content = base_json(&after);
    let author = actor.author.clone();
    let editor = editor_of(&actor);
    crate::ai_edit::begin_presence_for(app, "table", &path, &actor, &ok_text);
    let rel = path.clone();
    let reply_text = ok_text.clone();
    iced::Task::perform(
        async move {
            let outcome: Result<String, String> = async {
                let new_file = match client.table_patch(&space_id, &rel, &patch).await {
                    Ok(ack) => ack.file,
                    Err(SpaceError::BadRequest(m)) => {
                        return Err(format!("表格已被他人修改，请刷新后重试（{m}）"));
                    }
                    Err(e) => return Err(e.to_string()),
                };
                space_history_record(
                    client.clone(),
                    space_id.clone(),
                    "table".to_string(),
                    rel.clone(),
                    content,
                    summary,
                    author,
                    editor,
                )
                .await;
                Ok(new_file)
            }
            .await;
            let _ = reply.send(match &outcome {
                Ok(_) => Ok(reply_text),
                Err(e) => Err(format!("写入失败：{e}")),
            });
            let (new_file, result) = match &outcome {
                Ok(file) => (file.clone(), Ok(file.clone())),
                Err(e) => (rel.clone(), Err(e.clone())),
            };
            Message::Space(crate::SpaceMsg::TablePatchDone {
                gen,
                file: rel,
                new_file,
                reason: TableSaveReason::AutoSave,
                result,
                frozen: after,
            })
        },
        |m| m,
    )
    .chain(iced::Task::done(Message::AiEditDone(gen, path, String::new(), None)))
}

/// 空间表格（未打开）：先取服务端真源，应用操作后按一次补丁上行。
#[allow(clippy::too_many_arguments)]
fn apply_space_table_from_server(
    app: &mut App,
    gen: u64,
    path: String,
    ops: Vec<TableOp>,
    actor: EditActor,
    client: std::sync::Arc<atelyx_core::space_client::SpaceClient>,
    space_id: String,
    reply: oneshot::Sender<Result<String, String>>,
) -> iced::Task<Message> {
    let author = actor.author.clone();
    let editor = editor_of(&actor);
    crate::ai_edit::begin_presence_for(app, "table", &path, &actor, "编辑表格");
    let rel = path.clone();
    iced::Task::perform(
        async move {
            let outcome: Result<(String, TableFile, String), String> = async {
                let title = doc_edit::file_label(&rel);
                // 服务端还没有这个文件 = 本次创建：先落空模型，再按补丁上行
                let base = match doc_edit::read_space::<TableFile>(&client, &space_id, &rel).await? {
                    Some(model) => model,
                    None => doc_edit::create_space::<TableFile>(&client, &space_id, &rel, &title).await?,
                };
                let updated_at = crate::now_secs() as i64;
                let planned = match doc_edit::plan(&base, &base, &ops, &title, updated_at) {
                    Ok(planned) => planned,
                    Err(PlanError::Unchanged) => {
                        return Ok((rel.clone(), base, "未产生补丁".to_string()));
                    }
                    Err(PlanError::Ops(e)) => return Err(e),
                };
                let new_file = doc_edit::commit_space::<TableFile>(
                    &client, &space_id, &rel, &planned.patch,
                )
                .await?;
                space_history_record(
                    client.clone(),
                    space_id.clone(),
                    "table".to_string(),
                    rel.clone(),
                    base_json(&planned.after),
                    planned.summary.clone(),
                    author,
                    editor,
                )
                .await;
                Ok((new_file, planned.after, planned.summary))
            }
            .await;
            let receipt = match &outcome {
                Ok((new_file, _, summary)) => {
                    format!("已编辑 {new_file}（{} 项操作：{summary}）", ops.len())
                }
                Err(e) => e.clone(),
            };
            let _ = reply.send(match &outcome {
                Ok(_) => Ok(receipt),
                Err(e) => Err(format!("写入失败：{e}")),
            });
            let (new_file, result, frozen) = match outcome {
                Ok((new_file, after, _)) => (new_file.clone(), Ok(new_file), after),
                Err(e) => (
                    rel.clone(),
                    Err(e),
                    TableFile {
                        schema: String::new(),
                        id: String::new(),
                        title: String::new(),
                        fields: vec![],
                        rows: vec![],
                        created_at: 0,
                        updated_at: 0,
                    },
                ),
            };
            Message::Space(crate::SpaceMsg::TablePatchDone {
                gen,
                file: rel,
                new_file,
                reason: TableSaveReason::AutoSave,
                result,
                frozen,
            })
        },
        |m| m,
    )
    .chain(iced::Task::done(Message::AiEditDone(gen, path, String::new(), None)))
}

/// 空间画布：口径同 apply_space_table。
fn apply_space_canvas(
    app: &mut App,
    path: String,
    ops: Vec<CanvasOp>,
    actor: EditActor,
    reply: oneshot::Sender<Result<String, String>>,
) -> iced::Task<Message> {
    let gen = app.epoch();
    let RepoKind::Space(session) = &app.repo_kind else {
        return fail(app, reply, "仓库类型不匹配".to_string());
    };
    if !session.role.can_write() {
        return fail(app, reply, "该空间为只读身份，无法编辑".to_string());
    }
    let client = session.client.clone();
    let space_id = session.space_id.clone();
    let open = app.canvas.as_ref().filter(|s| s.file == path);
    let Some(s) = open else {
        return apply_space_canvas_from_server(
            app, gen, path, ops, actor, client, space_id, reply,
        );
    };
    let baseline = s.saved.clone();
    let base = s.data.clone();
    let title = doc_edit::file_label(&path);
    let updated_at = crate::now_secs() as i64;
    let planned = match doc_edit::plan(&baseline, &base, &ops, &title, updated_at) {
        Ok(planned) => planned,
        Err(PlanError::Unchanged) => {
            let _ = reply.send(Ok(format!("画布未变化：{path}（操作与现值相同）")));
            return iced::Task::none();
        }
        Err(PlanError::Ops(e)) => return fail(app, reply, e),
    };
    let patch = planned.patch;
    let after = planned.after;
    let summary = planned.summary;
    let ok_text = format!(
        "已编辑 {path}（{} 项操作：{summary}）；实体：{}",
        ops.len(),
        touched_ids(&patch).join("、")
    );
    let merge = merge_canvas_into_session(app, &path, &after, &patch, after.updated_at);
    let content = base_json(&after);
    let author = actor.author.clone();
    let editor = editor_of(&actor);
    crate::ai_edit::begin_presence_for(app, "canvas", &path, &actor, &ok_text);
    let rel = path.clone();
    let reply_text = ok_text.clone();
    merge.chain(
        iced::Task::perform(
            async move {
                let outcome: Result<String, String> = async {
                    let new_file = match client.canvas_patch(&space_id, &rel, &patch).await {
                        Ok(ack) => ack.file,
                        Err(SpaceError::BadRequest(m)) => {
                            return Err(format!("画布已被他人修改，请刷新后重试（{m}）"));
                        }
                        Err(e) => return Err(e.to_string()),
                    };
                    space_history_record(
                        client.clone(),
                        space_id.clone(),
                        "canvas".to_string(),
                        rel.clone(),
                        content,
                        summary,
                        author,
                        editor,
                    )
                    .await;
                    Ok(new_file)
                }
                .await;
                let _ = reply.send(match &outcome {
                    Ok(_) => Ok(reply_text),
                    Err(e) => Err(format!("写入失败：{e}")),
                });
                let (new_file, result) = match &outcome {
                    Ok(file) => (file.clone(), Ok(file.clone())),
                    Err(e) => (rel.clone(), Err(e.clone())),
                };
                Message::Space(crate::SpaceMsg::CanvasPatchDone {
                    gen,
                    file: rel,
                    new_file,
                    reason: CanvasSaveReason::AutoSave,
                    result,
                    frozen: after,
                })
            },
            |m| m,
        ),
    )
    .chain(iced::Task::done(Message::AiEditDone(gen, path, String::new(), None)))
}

/// 空间画布（未打开）：先取服务端真源，应用操作后按一次补丁上行。
#[allow(clippy::too_many_arguments)]
fn apply_space_canvas_from_server(
    app: &mut App,
    gen: u64,
    path: String,
    ops: Vec<CanvasOp>,
    actor: EditActor,
    client: std::sync::Arc<atelyx_core::space_client::SpaceClient>,
    space_id: String,
    reply: oneshot::Sender<Result<String, String>>,
) -> iced::Task<Message> {
    let author = actor.author.clone();
    let editor = editor_of(&actor);
    crate::ai_edit::begin_presence_for(app, "canvas", &path, &actor, "编辑画布");
    let rel = path.clone();
    iced::Task::perform(
        async move {
            let outcome: Result<(String, CanvasFile, String), String> = async {
                let title = doc_edit::file_label(&rel);
                // 服务端还没有这个文件 = 本次创建：先落空模型，再按补丁上行
                let base = match doc_edit::read_space::<CanvasFile>(&client, &space_id, &rel).await? {
                    Some(model) => model,
                    None => doc_edit::create_space::<CanvasFile>(&client, &space_id, &rel, &title).await?,
                };
                let updated_at = crate::now_secs() as i64;
                let planned = match doc_edit::plan(&base, &base, &ops, &title, updated_at) {
                    Ok(planned) => planned,
                    Err(PlanError::Unchanged) => {
                        return Ok((rel.clone(), base, "未产生补丁".to_string()));
                    }
                    Err(PlanError::Ops(e)) => return Err(e),
                };
                let new_file = doc_edit::commit_space::<CanvasFile>(
                    &client, &space_id, &rel, &planned.patch,
                )
                .await?;
                space_history_record(
                    client.clone(),
                    space_id.clone(),
                    "canvas".to_string(),
                    rel.clone(),
                    base_json(&planned.after),
                    planned.summary.clone(),
                    author,
                    editor,
                )
                .await;
                Ok((new_file, planned.after, planned.summary))
            }
            .await;
            let receipt = match &outcome {
                Ok((new_file, _, summary)) => {
                    format!("已编辑 {new_file}（{} 项操作：{summary}）", ops.len())
                }
                Err(e) => e.clone(),
            };
            let _ = reply.send(match &outcome {
                Ok(_) => Ok(receipt),
                Err(e) => Err(format!("写入失败：{e}")),
            });
            let (new_file, result, frozen) = match outcome {
                Ok((new_file, after, _)) => (new_file.clone(), Ok(new_file), after),
                Err(e) => (
                    rel.clone(),
                    Err(e),
                    CanvasFile {
                        schema: String::new(),
                        id: String::new(),
                        title: String::new(),
                        nodes: vec![],
                        edges: vec![],
                        created_at: 0,
                        updated_at: 0,
                    },
                ),
            };
            Message::Space(crate::SpaceMsg::CanvasPatchDone {
                gen,
                file: rel,
                new_file,
                reason: CanvasSaveReason::AutoSave,
                result,
                frozen,
            })
        },
        |m| m,
    )
    .chain(iced::Task::done(Message::AiEditDone(gen, path, String::new(), None)))
}

// ===== 共用小件 =====

/// 可读失败回执：提示层可见 + 工具回执 Err（不静默）。
fn fail(
    app: &mut App,
    reply: oneshot::Sender<Result<String, String>>,
    reason: String,
) -> iced::Task<Message> {
    app.notify(crate::NoticeLevel::Error, format!("AI 修改未落盘：{reason}"));
    let _ = reply.send(Err(reason));
    iced::Task::none()
}

/// 本地表格历史版本（editor = Agent、不参与连续编辑合并；尽力而为，失败不阻塞正文）。
async fn record_table_version(
    root: std::path::PathBuf,
    rel: String,
    content: String,
    author: atelyx_core::history::HistoryAuthor,
    editor: Option<HistoryEditor>,
) {
    let _ = tokio::task::spawn_blocking(move || {
        atelyx_core::history::record_version(
            &root,
            HistoryKind::Table,
            &rel,
            &RecordOpts {
                content,
                action: HistoryAction::Edit,
                author,
                coalesce_edit_ms: None,
                max_versions: 0,
                note: None,
                editor,
            },
        )
    })
    .await;
}

/// 本地画布历史版本（口径同 record_table_version）。
async fn record_canvas_version(
    root: std::path::PathBuf,
    rel: String,
    content: String,
    author: atelyx_core::history::HistoryAuthor,
    editor: Option<HistoryEditor>,
) {
    let _ = tokio::task::spawn_blocking(move || {
        atelyx_core::history::record_version(
            &root,
            HistoryKind::Canvas,
            &rel,
            &RecordOpts {
                content,
                action: HistoryAction::Edit,
                author,
                coalesce_edit_ms: None,
                max_versions: 0,
                note: None,
                editor,
            },
        )
    })
    .await;
}

/// 空间历史版本（editor = Agent、不参与合并；失败不阻塞正文）。
async fn space_history_record(
    client: std::sync::Arc<atelyx_core::space_client::SpaceClient>,
    space_id: String,
    kind: String,
    file: String,
    content: String,
    summary: String,
    author: atelyx_core::history::HistoryAuthor,
    editor: Option<HistoryEditor>,
) {
    let _ = client
        .history_record(
            &space_id,
            &HistoryRecordReq {
                kind,
                file,
                content,
                action: "edit".to_string(),
                author: HistoryAuthorReq {
                    id: author.id,
                    name: author.name,
                    device: author.device,
                },
                summary: Some(summary),
                note: None,
                editor,
                coalesce_edit_ms: None,
                max_versions: None,
                byte_budget: Some(atelyx_core::history::HISTORY_BYTE_BUDGET),
            },
        )
        .await;
}

/// AI 版本的历史来源标记（agent 名进展示，id 供归因）。
fn editor_of(actor: &EditActor) -> Option<HistoryEditor> {
    Some(HistoryEditor {
        agent_id: actor.agent.agent_id.clone(),
        agent_name: actor.agent.agent_name.clone(),
    })
}

fn base_json<T: serde::Serialize>(model: &T) -> String {
    serde_json::to_string(model).unwrap_or_default()
}


/// 画布补丁受影响实体（新增 / 修改节点与连线，按补丁顺序）：
/// 从合入后模型里取本次新增的稳定 id，供回执告诉模型后续操作可用的 id。
fn touched_ids(patch: &CanvasPatch) -> Vec<String> {
    let mut ids: Vec<String> = patch.upsert_nodes.iter().map(|n| n.id.clone()).collect();
    ids.extend(patch.upsert_edges.iter().map(|e| e.id.clone()));
    ids.extend(patch.removed_node_ids.iter().cloned());
    ids.dedup();
    ids
}
