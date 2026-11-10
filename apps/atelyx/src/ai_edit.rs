//! AI 编辑出口：工具线程投递编辑请求，壳 update 内串行执行（与用户输入同一条线程），
//! 写入复用保存链并记历史；失败以可读原因回执模型，不静默。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use iced::Subscription;
use tokio::sync::{broadcast, oneshot};

use atelyx_core::ai::edit_host::{
    AiEditHost, CanvasOp, DocKind, EditActor, EditFuture, NoteOp, TableOp,
};
use atelyx_core::history::{HistoryAction, HistoryEditor, HistoryKind, RecordOpts};
use atelyx_core::space_client::{HistoryAuthorReq, HistoryRecordReq};

use crate::space_actions;
use crate::ui::notify::Level as NoticeLevel;
use crate::{App, Message, RepoKind};

/// 请求回执超时：壳在跑但长时间未处理时给模型可读失败（不无限等待）。
const REPLY_TIMEOUT: Duration = Duration::from_secs(120);
/// 唤醒通道容量（同一时刻在途请求数远小于此）。
const WAKE_CAP: usize = 64;

/// 一次编辑的操作体（笔记 / 表格 / 画布共用一条请求通道）。
pub enum EditOp {
    Note(NoteOp),
    Table(Vec<TableOp>),
    Canvas(Vec<CanvasOp>),
}

/// 一次待执行的编辑请求：请求体与回执句柄只在总线持有，Message 只带 id。
pub struct EditRequest {
    pub kind: DocKind,
    pub path: String,
    pub op: EditOp,
    /// 空间笔记：工具线程先取回的服务端正文（None = 本地或取不到）。
    pub server_text: Option<String>,
    pub actor: EditActor,
    reply: oneshot::Sender<Result<String, String>>,
}

struct Bus {
    pending: Mutex<VecDeque<(u64, EditRequest)>>,
    wake: broadcast::Sender<u64>,
    next_id: Mutex<u64>,
}

static BUS: OnceLock<Arc<Bus>> = OnceLock::new();

fn bus() -> &'static Arc<Bus> {
    BUS.get_or_init(|| {
        let (wake, _) = broadcast::channel(WAKE_CAP);
        Arc::new(Bus {
            pending: Mutex::new(VecDeque::new()),
            wake,
            next_id: Mutex::new(1),
        })
    })
}

/// 订阅：壳 update 收到 `Message::AiEdit(id)` 后执行对应请求。
pub fn subscription() -> Subscription<Message> {
    Subscription::run_with((), |_| wake_stream())
}

fn wake_stream() -> impl iced::futures::Stream<Item = Message> + use<> {
    iced::stream::channel(16, async move |mut output| {
        let mut wake = bus().wake.subscribe();
        loop {
            match wake.recv().await {
                Ok(id) => {
                    if iced::futures::SinkExt::send(&mut output, Message::AiEdit(id)).await.is_err() {
                        return;
                    }
                }
                // 落后丢帧不致命：请求仍在 pending 里，由后续帧或回执超时收场
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    })
}

/// 取出一个待执行请求（update 内按 id 调用；找不到 = 已作废）。
pub fn take(id: u64) -> Option<EditRequest> {
    let mut pending = bus().pending.lock().unwrap_or_else(|e| e.into_inner());
    let pos = pending.iter().position(|(rid, _)| *rid == id)?;
    pending.remove(pos).map(|(_, req)| req)
}

/// 工具线程侧的宿主出口：投递请求并等待壳回执。
pub struct ShellEditHost {
    /// 空间后端（None = 本地仓库）：笔记写入前先取服务端正文作为 AI 操作目标。
    space: Option<(Arc<atelyx_core::space_client::SpaceClient>, String)>,
}

impl ShellEditHost {
    pub fn new(app: &App) -> Self {
        let space = match &app.repo_kind {
            RepoKind::Space(session) => Some((session.client.clone(), session.space_id.clone())),
            RepoKind::Local => None,
        };
        Self { space }
    }

    fn submit(
        &self,
        kind: DocKind,
        path: String,
        op: EditOp,
        server_text: Option<String>,
        actor: &EditActor,
    ) -> EditFuture<'_> {
        let actor = actor.clone();
        Box::pin(async move {
            let id = {
                let mut next = bus().next_id.lock().unwrap_or_else(|e| e.into_inner());
                let id = *next;
                *next += 1;
                id
            };
            let (reply, rx) = oneshot::channel();
            {
                let mut pending = bus().pending.lock().unwrap_or_else(|e| e.into_inner());
                pending.push_back((
                    id,
                    EditRequest { kind, path, op, server_text, actor, reply },
                ));
            }
            if bus().wake.send(id).is_err() {
                let _ = take(id);
                return Err("AI 编辑不可用：界面通道未就绪".to_string());
            }
            match tokio::time::timeout(REPLY_TIMEOUT, rx).await {
                Ok(Ok(result)) => result,
                Ok(Err(_)) => Err("AI 编辑请求已被丢弃（界面未回执）".to_string()),
                Err(_) => Err("AI 编辑超时：界面长时间未处理".to_string()),
            }
        })
    }
}

impl AiEditHost for ShellEditHost {
    fn apply_note(&self, path: &str, op: NoteOp, actor: &EditActor) -> EditFuture<'_> {
        // 空间笔记：先取服务端正文（网络在工具线程内完成，壳 update 不做网络等待）
        let space = self.space.clone();
        let path = path.to_string();
        let actor = actor.clone();
        Box::pin(async move {
            let server_text = match &space {
                Some((client, space_id)) => match client.get_file_text(space_id, &path).await {
                    Ok(text) => Some(text),
                    Err(atelyx_core::space_client::SpaceError::NotFound) => Some(String::new()),
                    Err(e) => return Err(format!("读取服务端文件失败：{e}")),
                },
                None => None,
            };
            ShellEditHost { space: None }
                .submit(DocKind::Note, path, EditOp::Note(op), server_text, &actor)
                .await
        })
    }

    fn apply_table(&self, path: &str, ops: Vec<TableOp>, actor: &EditActor) -> EditFuture<'_> {
        let path = path.to_string();
        let actor = actor.clone();
        Box::pin(async move {
            ShellEditHost { space: None }
                .submit(DocKind::Table, path, EditOp::Table(ops), None, &actor)
                .await
        })
    }

    fn apply_canvas(&self, path: &str, ops: Vec<CanvasOp>, actor: &EditActor) -> EditFuture<'_> {
        let path = path.to_string();
        let actor = actor.clone();
        Box::pin(async move {
            ShellEditHost { space: None }
                .submit(DocKind::Canvas, path, EditOp::Canvas(ops), None, &actor)
                .await
        })
    }
}

/// 执行一次待处理编辑（update 内调用）：打开且脏走三方合流，冲突拒绝写入。
pub fn apply(app: &mut App, id: u64) -> iced::Task<Message> {
    let Some(req) = take(id) else { return iced::Task::none() };
    let EditRequest { kind, path, op, server_text, actor, reply } = req;
    match (kind, op) {
        (DocKind::Note, EditOp::Note(op)) => apply_note(app, path, op, server_text, actor, reply),
        (DocKind::Table, EditOp::Table(ops)) => {
            crate::ai_doc_edit::apply_table(app, path, ops, actor, reply)
        }
        (DocKind::Canvas, EditOp::Canvas(ops)) => {
            crate::ai_doc_edit::apply_canvas(app, path, ops, actor, reply)
        }
        _ => {
            let _ = reply.send(Err("AI 编辑请求与文档类型不匹配".to_string()));
            iced::Task::none()
        }
    }
}

fn apply_note(
    app: &mut App,
    path: String,
    op: NoteOp,
    server_text: Option<String>,
    actor: EditActor,
    reply: oneshot::Sender<Result<String, String>>,
) -> iced::Task<Message> {
    let open = app.ui.last_note_file.as_deref() == Some(path.as_str());
    match &app.repo_kind {
        RepoKind::Local => apply_local_note(app, path, op, open, actor, reply),
        RepoKind::Space(_) => apply_space_note(app, path, op, server_text, open, actor, reply),
    }
}

/// 本地笔记：基线 = 打开且脏时的上次落盘正文，否则磁盘事实。
fn apply_local_note(
    app: &mut App,
    path: String,
    op: NoteOp,
    open: bool,
    actor: EditActor,
    reply: oneshot::Sender<Result<String, String>>,
) -> iced::Task<Message> {
    let Ok(root) = app.vault.root() else {
        let _ = reply.send(Err("仓库未打开".to_string()));
        return iced::Task::none();
    };
    let disk = match atelyx_core::vault::safe_join(&root, &path, false) {
        Ok(file) => std::fs::read_to_string(&file).unwrap_or_default(),
        Err(e) => {
            let _ = reply.send(Err(e));
            return iced::Task::none();
        }
    };
    let base = if open && app.dirty {
        // 草稿的祖先必须是上次落盘正文；磁盘已变说明外部也改过，无法安全对齐
        if disk != app.note_base {
            let _ = reply.send(Err(format!(
                "文件已被外部修改，且你有未保存的改动；请先保存或重新打开 {path} 后再让 AI 修改"
            )));
            return iced::Task::none();
        }
        app.note_base.clone()
    } else {
        disk
    };
    let (ai_text, replaced) = match atelyx_core::ai::edit_host::apply_note_op(&base, &op) {
        Ok(value) => value,
        Err(e) => {
            let _ = reply.send(Err(e));
            return iced::Task::none();
        }
    };
    let merged = if open && app.dirty {
        match atelyx_core::ai::edit_host::merge_note_edit(&base, &app.note_text, &ai_text) {
            Ok(text) => text,
            Err(e) => {
                let _ = reply.send(Err(format!("{e}：{path}")));
                return iced::Task::none();
            }
        }
    } else {
        ai_text
    };
    if open && merged != app.note_text {
        space_actions::apply_remote_text(app, merged.clone());
    }
    if open {
        app.note_base = merged.clone();
        app.dirty = false;
        app.save_error = None;
    }
    let editor = editor_of(&actor);
    let author = actor.author.clone();
    let queue = app.vault.writes.clone();
    let rel = path.clone();
    let content = merged.clone();
    let ok_text = success_text(&op, &path, replaced);
    let gen = app.epoch();
    let root_for_task = root.clone();
    iced::Task::perform(
        async move {
            let outcome = async {
                let file = atelyx_core::vault::safe_join(&root_for_task, &rel, false)?;
                queue.write(file, content.clone()).await?;
                let (root, rel2, body) = (root_for_task.clone(), rel.clone(), content.clone());
                // 历史尽力而为：失败不阻塞正文（与人工保存同口径）
                let _ = tokio::task::spawn_blocking(move || {
                    atelyx_core::history::record_version(
                        &root,
                        HistoryKind::Note,
                        &rel2,
                        &RecordOpts {
                            content: body,
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
                Ok::<(), String>(())
            }
            .await;
            let _ = reply.send(match &outcome {
                Ok(()) => Ok(ok_text),
                Err(e) => Err(format!("写入失败：{e}")),
            });
            Message::AiEditDone(gen, rel, content, outcome.err())
        },
        |msg| msg,
    )
}

/// 空间笔记：AI 结果整文上行，冲突按现有保存链口径（本地保留未写、工具报错）。
fn apply_space_note(
    app: &mut App,
    path: String,
    op: NoteOp,
    server_text: Option<String>,
    open: bool,
    actor: EditActor,
    reply: oneshot::Sender<Result<String, String>>,
) -> iced::Task<Message> {
    let RepoKind::Space(session) = &app.repo_kind else {
        let _ = reply.send(Err("仓库类型不匹配".to_string()));
        return iced::Task::none();
    };
    let client = session.client.clone();
    let space_id = session.space_id.clone();
    let base = app.space_note_flushed.clone().unwrap_or_default();
    let source = server_text.unwrap_or_else(|| base.clone());
    let (ai_text, replaced) = match atelyx_core::ai::edit_host::apply_note_op(&source, &op) {
        Ok(value) => value,
        Err(e) => {
            let _ = reply.send(Err(e));
            return iced::Task::none();
        }
    };
    let merged = if open && app.dirty {
        match atelyx_core::ai::edit_host::merge_note_edit(&base, &app.note_text, &ai_text) {
            Ok(text) => text,
            Err(e) => {
                let _ = reply.send(Err(format!("{e}：{path}")));
                return iced::Task::none();
            }
        }
    } else {
        ai_text
    };
    if open {
        if merged != app.note_text {
            space_actions::apply_remote_text(app, merged.clone());
        }
        // 与人工输入同一条路：协作文档增量上行，打开者实时看到 AI 修改
        space_actions::collab_after_edit(app);
        app.dirty = false;
        app.save_error = None;
    }
    let summary = atelyx_core::history::version_summary(HistoryKind::Note, &source, &merged);
    let editor = editor_of(&actor);
    let author = actor.author.clone();
    let rel = path.clone();
    let content = merged.clone();
    let ok_text = success_text(&op, &path, replaced);
    begin_presence(app, &path, &actor, &ok_text);
    let gen = app.epoch();
    iced::Task::perform(
        async move {
            let outcome = async {
                client
                    .put_file_text(&space_id, &rel, &content)
                    .await
                    .map_err(|e| e.to_string())?;
                // 历史尽力而为：失败不阻塞正文
                let _ = client
                    .history_record(
                        &space_id,
                        &HistoryRecordReq {
                            kind: "note".into(),
                            file: rel.clone(),
                            content: content.clone(),
                            action: "edit".into(),
                            author: HistoryAuthorReq {
                                id: author.id.clone(),
                                name: author.name.clone(),
                                device: author.device.clone(),
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
                Ok::<(), String>(())
            }
            .await;
            let _ = reply.send(match &outcome {
                Ok(()) => Ok(ok_text),
                Err(e) => Err(format!("写入失败：{e}")),
            });
            Message::AiEditDone(gen, rel, content, outcome.err())
        },
        |msg| msg,
    )
}

/// AI 开始编辑：空间模式下上报在场状态（编辑中 + 操作者 + Agent），本地无协作通道。
fn begin_presence(app: &mut App, path: &str, actor: &EditActor, summary: &str) {
    if !matches!(app.repo_kind, RepoKind::Space(_)) {
        return;
    }
    let identity = crate::space_actions::local_identity(app);
    let presence = app
        .ai_presence
        .get_or_insert_with(crate::ai_presence::AiPresence::new);
    if let Some(payload) = presence.begin(
        std::time::Instant::now(),
        "note",
        path,
        actor,
        summary,
        &identity,
    ) {
        crate::space_actions::send_ai_presence(app, payload);
    }
}

/// AI 编辑结束：上报 idle + lastEdit（失败同样收场，不留下「编辑中」幽灵条目）。
fn end_presence(app: &mut App) {
    if !matches!(app.repo_kind, RepoKind::Space(_)) {
        return;
    }
    let identity = crate::space_actions::local_identity(app);
    let Some(presence) = app.ai_presence.as_mut() else {
        return;
    };
    if let Some(payload) = presence.idle(std::time::Instant::now(), &identity) {
        crate::space_actions::send_ai_presence(app, payload);
    }
}

/// AI 版本的历史来源标记（agent 名进展示，id 供归因）。
fn editor_of(actor: &EditActor) -> Option<HistoryEditor> {
    Some(HistoryEditor {
        agent_id: actor.agent.agent_id.clone(),
        agent_name: actor.agent.agent_name.clone(),
    })
}

/// 工具回执文本：按操作类型给模型可读结果。
fn success_text(op: &NoteOp, path: &str, replaced: usize) -> String {
    match op {
        NoteOp::Write(_) => format!("已写入 {path}"),
        NoteOp::Append(_) => format!("已追加到 {path}"),
        NoteOp::Replace { .. } => format!("已编辑 {path}（替换 {replaced} 处）"),
    }
}

/// 编辑落盘回执：成功推进空间落盘基线并刷新历史列表；失败恢复脏标记（防抖保存兜底）。
pub fn on_done(
    app: &mut App,
    gen: u64,
    path: String,
    content: String,
    err: Option<String>,
) -> iced::Task<Message> {
    if gen != app.epoch() {
        return iced::Task::none();
    }
    end_presence(app);
    match err {
        None => {
            if matches!(app.repo_kind, RepoKind::Space(_))
                && app.ui.last_note_file.as_deref() == Some(path.as_str())
            {
                app.space_note_flushed = Some(content);
            }
            app.reload_history()
        }
        Some(e) => {
            app.save_error = Some(e.clone());
            app.dirty = true;
            app.notify(NoticeLevel::Error, format!("AI 修改未落盘：{e}"));
            iced::Task::none()
        }
    }
}
