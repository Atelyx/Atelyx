//! AI 文档（表格 / 画布）保存出口：读基线 → 计划 → 合入会话 → 提交 → 记史 → 回执 → 消息。
//! 文档类型只提供壳侧差异面（会话读取 / 合入 / 回执载荷），其余步骤只此一份。

use atelyx_core::ai::doc_edit::{self, DocModel, PlanError};
use atelyx_core::ai::edit_host::EditActor;
use atelyx_core::history::{HistoryEditor, HistoryKind, RecordOpts};
use atelyx_core::space_client::HistoryRecordReq;
use atelyx_core::vault::{CanvasFile, CanvasPatch, TableFile};

use crate::ai_edit::Reply;
use crate::{App, Message, RepoKind};

/// 落盘回执的冻结快照：保存链据此结算脏态与基线，与发起时正文同源。
#[derive(Clone, Debug)]
pub(crate) enum Frozen {
    Table(TableFile),
    Canvas(CanvasFile),
}

/// 一次文档落盘的回执：落点 + 冻结快照 + 结果。
#[derive(Clone, Debug)]
pub(crate) struct DocSaved {
    pub gen: u64,
    /// true = 空间补丁端点，false = 本地写盘。
    pub space: bool,
    /// 发起时路径。
    pub file: String,
    /// 落地后路径（空间按标题改名；本地恒等于 `file`）。
    pub new_file: String,
    pub result: Result<(), String>,
    /// true = 脏拦截发起的放行保存：失败要回填挂起操作，成功要续跑放行链。
    pub flush: bool,
    pub frozen: Frozen,
}

/// 文档类型在壳侧的差异面：打开会话的读取、合入与回执载荷。
pub(crate) struct DocHost<M: DocModel> {
    /// 打开中的会话（上次落盘基线, 当前模型）；未打开返回 None。
    session: fn(&App, &str) -> Option<(M, M)>,
    /// 合入打开中的会话（未打开无副作用），返回会话收敛的后续任务。
    merge: fn(&mut App, &str, &M, &M::Patch, i64) -> iced::Task<Message>,
    /// 回执载荷。
    frozen: fn(M) -> Frozen,
}

pub(crate) const TABLE: DocHost<TableFile> = DocHost {
    session: table_session,
    merge: merge_table_into_session,
    frozen: Frozen::Table,
};

pub(crate) const CANVAS: DocHost<CanvasFile> = DocHost {
    session: canvas_session,
    merge: merge_canvas_into_session,
    frozen: Frozen::Canvas,
};

/// 一次 AI 文档编辑的落地：本地写盘与空间补丁端点两个分支只此一处。
pub(crate) fn apply<M: DocModel>(
    host: &'static DocHost<M>,
    app: &mut App,
    path: String,
    ops: M::Ops,
    actor: EditActor,
    reply: Reply,
) -> iced::Task<Message> {
    match &app.repo_kind {
        RepoKind::Local => apply_local(app, host, path, ops, actor, reply),
        RepoKind::Space(_) => apply_space(app, host, path, ops, actor, reply),
    }
}

/// 落盘回执结算：按落点与文档类型交对应保存链（脏态 / 基线 / 历史 / 提示口径不变）。
pub(crate) fn on_saved(app: &mut App, saved: DocSaved) -> iced::Task<Message> {
    let DocSaved { gen, space, file, new_file, result, flush, frozen } = saved;
    match (space, frozen) {
        (false, Frozen::Table(frozen)) => {
            let task = crate::table_actions::on_saved(app, gen, file, flush, result, frozen);
            // 与表格消息入口同口径：选区广播、AI 自动队列续跑、可见区图片补拉
            crate::space_actions::table_collab_sync(app);
            task.chain(crate::table_ai::drain_auto(app))
                .chain(crate::table_actions::ensure_image_loads(app).unwrap_or_default())
        }
        (false, Frozen::Canvas(frozen)) => {
            crate::canvas_chat::sync_card_chats(app);
            let task = crate::canvas_actions::on_saved(app, gen, file, flush, result, frozen);
            app.schedule_canvas_save();
            crate::space_actions::canvas_collab_sync(app);
            task
        }
        (true, Frozen::Table(frozen)) => {
            let landed = new_file.clone();
            crate::space_actions::on_table_patch_done(
                app,
                gen,
                file,
                new_file,
                flush,
                result.map(|()| landed),
                frozen,
            )
        }
        (true, Frozen::Canvas(frozen)) => {
            let landed = new_file.clone();
            crate::space_actions::on_canvas_patch_done(
                app,
                gen,
                file,
                new_file,
                flush,
                result.map(|()| landed),
                frozen,
            )
        }
    }
}

/// 失败收场：界面提示 + 工具回执 Err（不静默）。
pub(crate) fn fail(app: &mut App, reply: Reply, reason: String) -> iced::Task<Message> {
    app.notify(crate::NoticeLevel::Error, format!("AI 修改未落盘：{reason}"));
    reply.err(reason);
    iced::Task::none()
}

/// 本地：打开会话含未保存编辑时以保存基线为源，补丁按 id 合入磁盘真源后写盘。
fn apply_local<M: DocModel>(
    app: &mut App,
    host: &'static DocHost<M>,
    path: String,
    ops: M::Ops,
    actor: EditActor,
    reply: Reply,
) -> iced::Task<Message> {
    let gen = app.epoch();
    let Ok(root) = app.vault.root() else {
        return fail(app, reply, "仓库未打开".to_string());
    };
    let title = atelyx_core::vault::file_label(&path);
    let disk = match doc_edit::read_local_or_new::<M>(&app.vault, &root, &path, &title) {
        Ok(model) => model,
        Err(e) => return fail(app, reply, doc_edit::unreadable_receipt::<M>(&path, &e)),
    };
    // 打开会话：补丁以保存基线为源（用户未保存编辑随本次落盘一并写入）；未打开：以磁盘真源为源
    let (baseline, base) = match (host.session)(app, &path) {
        Some((saved, data)) => (saved, data),
        None => (disk.clone(), disk),
    };
    let updated_at = crate::now_secs() as i64;
    let planned = match doc_edit::plan::<M>(&baseline, &base, &ops, &title, updated_at) {
        Ok(planned) => planned,
        Err(PlanError::Unchanged) => {
            reply.ok(doc_edit::unchanged_receipt::<M>(&path));
            return iced::Task::none();
        }
        Err(PlanError::Ops(e)) => return fail(app, reply, e),
    };
    let ok_text =
        doc_edit::edited_receipt::<M>(&path, M::ops_count(&ops), &planned.summary, &planned.patch);
    // 打开会话先合入：用户未保存编辑保留，随后整份落盘（与既有保存链同口径）
    let merge = (host.merge)(app, &path, &planned.after, &planned.patch, updated_at);
    let author = actor.author.clone();
    let editor = crate::ai_edit::editor_of(&actor);
    crate::ai_edit::begin_presence_for(app, M::space_kind(), &path, &actor, &ok_text);
    let queue = app.vault.writes.clone();
    let state = app.vault.clone();
    let rel = path;
    let expected_id = planned.after.id().to_string();
    let frozen = planned.after.clone();
    let frozen_msg = planned.after;
    let patch = planned.patch;
    merge
        .chain(iced::Task::perform(
            async move {
                let outcome = async {
                    let written = doc_edit::commit_local::<M>(
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
                    M::write_cache(&state, &file, &rel, &written);
                    record_local_version(
                        root.clone(),
                        M::kind(),
                        rel.clone(),
                        base_json(&written),
                        author,
                        editor,
                    )
                    .await;
                    Ok::<M, String>(written)
                }
                .await;
                let saved = match outcome {
                    Ok(written) => {
                        reply.ok(ok_text);
                        DocSaved {
                            gen,
                            space: false,
                            file: rel.clone(),
                            new_file: rel,
                            result: Ok(()),
                            // AI 编辑不经脏拦截放行，不存在 Flush 收尾
                            flush: false,
                            frozen: (host.frozen)(written),
                        }
                    }
                    Err(e) => {
                        reply.err(doc_edit::write_failed(&e));
                        DocSaved {
                            gen,
                            space: false,
                            file: rel.clone(),
                            new_file: rel,
                            result: Err(e),
                            flush: false,
                            frozen: (host.frozen)(frozen_msg),
                        }
                    }
                };
                Message::DocSaved(Box::new(saved))
            },
            |m| m,
        ))
        .chain(iced::Task::done(Message::AiDocDone(gen)))
}

/// 空间已打开：补丁从保存基线算起（用户未保存编辑一并上行），服务端按稳定 id 仲裁。
fn apply_space<M: DocModel>(
    app: &mut App,
    host: &'static DocHost<M>,
    path: String,
    ops: M::Ops,
    actor: EditActor,
    reply: Reply,
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
    let Some((baseline, base)) = (host.session)(app, &path) else {
        return apply_space_from_server(app, gen, host, path, ops, actor, client, space_id, reply);
    };
    let title = atelyx_core::vault::file_label(&path);
    let updated_at = crate::now_secs() as i64;
    let planned = match doc_edit::plan::<M>(&baseline, &base, &ops, &title, updated_at) {
        Ok(planned) => planned,
        Err(PlanError::Unchanged) => {
            reply.ok(doc_edit::unchanged_receipt::<M>(&path));
            return iced::Task::none();
        }
        Err(PlanError::Ops(e)) => return fail(app, reply, e),
    };
    let ok_text =
        doc_edit::edited_receipt::<M>(&path, M::ops_count(&ops), &planned.summary, &planned.patch);
    let patch = planned.patch;
    let after = planned.after;
    let summary = planned.summary;
    let merge = (host.merge)(app, &path, &after, &patch, updated_at);
    let content = base_json(&after);
    let author = actor.author.clone();
    let editor = crate::ai_edit::editor_of(&actor);
    crate::ai_edit::begin_presence_for(app, M::space_kind(), &path, &actor, &ok_text);
    let rel = path;
    let reply_text = ok_text;
    merge
        .chain(iced::Task::perform(
            async move {
                let outcome: Result<String, String> = async {
                    let new_file = M::submit_patch(&client, &space_id, &rel, &patch)
                        .await
                        .map_err(|e| doc_edit::patch_error(e, M::label()))?;
                    space_history_record(
                        client.clone(),
                        space_id,
                        M::space_kind(),
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
                let saved = match outcome {
                    Ok(new_file) => {
                        reply.ok(reply_text);
                        DocSaved {
                            gen,
                            space: true,
                            file: rel.clone(),
                            new_file,
                            result: Ok(()),
                            flush: false,
                            frozen: (host.frozen)(after),
                        }
                    }
                    Err(e) => {
                        reply.err(doc_edit::write_failed(&e));
                        DocSaved {
                            gen,
                            space: true,
                            file: rel.clone(),
                            new_file: rel,
                            result: Err(e),
                            flush: false,
                            frozen: (host.frozen)(after),
                        }
                    }
                };
                Message::DocSaved(Box::new(saved))
            },
            |m| m,
        ))
        .chain(iced::Task::done(Message::AiDocDone(gen)))
}

/// 空间未打开：读服务端真源后算补丁（文件不存在时只新建，不覆盖他人内容）。
#[allow(clippy::too_many_arguments)]
fn apply_space_from_server<M: DocModel>(
    app: &mut App,
    gen: u64,
    host: &'static DocHost<M>,
    path: String,
    ops: M::Ops,
    actor: EditActor,
    client: std::sync::Arc<atelyx_core::space_client::SpaceClient>,
    space_id: String,
    reply: Reply,
) -> iced::Task<Message> {
    let author = actor.author.clone();
    let editor = crate::ai_edit::editor_of(&actor);
    let fallback = format!("编辑{}", M::label());
    crate::ai_edit::begin_presence_for(app, M::space_kind(), &path, &actor, &fallback);
    let rel = path;
    iced::Task::perform(
        async move {
            let outcome = async {
                let title = atelyx_core::vault::file_label(&rel);
                let updated_at = crate::now_secs() as i64;
                let Some(outcome) = doc_edit::apply_space::<M>(
                    &client,
                    &space_id,
                    &rel,
                    &title,
                    updated_at,
                    &ops,
                    M::label(),
                )
                .await?
                else {
                    return Ok::<Option<_>, String>(None);
                };
                space_history_record(
                    client.clone(),
                    space_id.clone(),
                    M::space_kind(),
                    outcome.file.clone(),
                    base_json(&outcome.after),
                    outcome.summary.clone(),
                    author,
                    editor,
                )
                .await;
                Ok(Some(outcome))
            }
            .await;
            match outcome {
                Ok(Some(outcome)) => {
                    reply.ok(doc_edit::edited_receipt::<M>(
                        &outcome.file,
                        M::ops_count(&ops),
                        &outcome.summary,
                        &outcome.patch,
                    ));
                    Message::DocSaved(Box::new(DocSaved {
                        gen,
                        space: true,
                        file: rel,
                        new_file: outcome.file,
                        result: Ok(()),
                        flush: false,
                        frozen: (host.frozen)(outcome.after),
                    }))
                }
                // 未落盘：提前收尾（提示 + 重取当前面），不按成功结算；保存链回执只用于已落盘的成功路径
                Ok(None) => {
                    reply.ok(doc_edit::unchanged_receipt::<M>(&rel));
                    Message::AiDocDone(gen)
                }
                Err(e) => {
                    reply.err(doc_edit::write_failed(&e));
                    Message::AiDocFailed(gen, e)
                }
            }
        },
        |m| m,
    )
    .chain(iced::Task::done(Message::AiDocDone(gen)))
}

fn table_session(app: &App, path: &str) -> Option<(TableFile, TableFile)> {
    let s = app.table.as_ref().filter(|s| s.file == path)?;
    Some((s.saved.clone(), s.data.clone()))
}

fn canvas_session(app: &App, path: &str) -> Option<(CanvasFile, CanvasFile)> {
    let s = app.canvas.as_ref().filter(|s| s.file == path)?;
    Some((s.saved.clone(), s.data.clone()))
}

/// 合入打开中的表格会话：会话模型即操作后的模型（含用户未保存编辑），撤销栈整体作废。
fn merge_table_into_session(
    app: &mut App,
    path: &str,
    after: &TableFile,
    _patch: &atelyx_core::vault::TablePatch,
    updated_at: i64,
) -> iced::Task<Message> {
    {
        let Some(s) = app.table.as_mut().filter(|s| s.file == path) else {
            return iced::Task::none();
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
    }
    // 合入即排自动保存：写盘失败后由防抖重试，不留「脏但无重试拍」的悬空会话
    app.autosave_deadline = Some(std::time::Instant::now() + crate::AUTOSAVE_DEBOUNCE);
    // 打开中的表格内容变更属宿主发起的编辑结算：与人工保存同口径通知插件
    app.plugins.on_table_changed(Some(path));
    iced::Task::none()
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
    // 合入即排自动保存：写盘失败后由防抖重试，不留「脏但无重试拍」的悬空会话
    app.schedule_canvas_save();
    crate::canvas_chat::sync_card_chats(app);
    if text_changed {
        if let Some(s) = app.canvas.as_mut() {
            crate::canvas_actions::rebuild_text_docs(s);
        }
    }
    crate::canvas_actions::load_refs(app, ref_paths)
}

/// 本地历史版本（editor = Agent、不参与连续编辑合并；尽力而为，失败不阻塞正文）。
async fn record_local_version(
    root: std::path::PathBuf,
    kind: HistoryKind,
    rel: String,
    content: String,
    author: atelyx_core::history::HistoryAuthor,
    editor: Option<HistoryEditor>,
) {
    let _ = tokio::task::spawn_blocking(move || {
        atelyx_core::history::record_version(
            &root,
            kind,
            &rel,
            &RecordOpts::agent_edit(content, author, editor),
        )
    })
    .await;
}

/// 空间历史版本（editor = Agent、不参与合并；失败不阻塞正文）。
async fn space_history_record(
    client: std::sync::Arc<atelyx_core::space_client::SpaceClient>,
    space_id: String,
    kind: &str,
    file: String,
    content: String,
    summary: String,
    author: atelyx_core::history::HistoryAuthor,
    editor: Option<HistoryEditor>,
) {
    let _ = client
        .history_record(
            &space_id,
            &HistoryRecordReq::agent_edit(kind, file, content, &author, Some(summary), editor),
        )
        .await;
}

fn base_json<T: serde::Serialize>(model: &T) -> String {
    serde_json::to_string(model).unwrap_or_default()
}