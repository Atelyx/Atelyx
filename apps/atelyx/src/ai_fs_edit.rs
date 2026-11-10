//! AI 文件系统操作出口：改名 / 移动 / 删除由壳执行——先落盘未保存内容，再动磁盘，
//! 并复用文件树改名的跟随语义收尾打开中的编辑面与引用同步。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use tokio::sync::oneshot;

use atelyx_core::ai::edit_host::FsOp;

use crate::space_actions;
use crate::ui::notify::Level as NoticeLevel;
use crate::{AiFsDone, App, Message, RepoKind};

/// 在途回执表：请求体（含 oneshot 发送端）只在表里，消息只带 id。
fn replies() -> &'static Mutex<HashMap<u64, oneshot::Sender<Result<String, String>>>> {
    static REPLIES: OnceLock<Mutex<HashMap<u64, oneshot::Sender<Result<String, String>>>>> =
        OnceLock::new();
    REPLIES.get_or_init(|| Mutex::new(HashMap::new()))
}

static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// 登记回执，返回请求 id。
fn register_reply(tx: oneshot::Sender<Result<String, String>>) -> u64 {
    let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    replies().lock().unwrap_or_else(|e| e.into_inner()).insert(id, tx);
    id
}

/// 交出结果并摘除登记（接收端已放弃等待 = 模型已收到超时，结果丢弃）。
/// 被更晚的宿主操作取代时由 gate 回填可读错误，工具线程不悬挂。
pub(crate) fn settle(id: u64, result: Result<String, String>) {
    let Some(tx) = replies().lock().unwrap_or_else(|e| e.into_inner()).remove(&id) else {
        return;
    };
    let _ = tx.send(result);
}

/// 执行一次文件系统操作（壳 update 内调用）：脏面先 flush 放行，再动磁盘。
pub fn apply(
    app: &mut App,
    op: FsOp,
    reply: oneshot::Sender<Result<String, String>>,
) -> iced::Task<Message> {
    let id = register_reply(reply);
    // 已有 flush 动作在途：不排队（同一时刻只有一个放行动作，排队会与它的回执抢放行）
    if app.flush_pending.is_some() {
        settle(id, Err("宿主保存进行中，请稍后重试".to_string()));
        return iced::Task::none();
    }
    app.gate(crate::PendingAfterFlush::AiFs { op, id })
}

/// flush 放行后的执行体：改磁盘 + 同步引用 + 收尾打开中的面。
pub(crate) fn execute(app: &mut App, op: FsOp, id: u64) -> iced::Task<Message> {
    match op {
        FsOp::Move { from, to } => move_entry(app, from, to, id),
        FsOp::Delete { path, force } => delete_entry(app, path, force, id),
    }
}

/// 回执消息（盒装：载荷含跟随语义所需的全部字段）。
#[allow(clippy::too_many_arguments)]
fn done(
    id: u64,
    gen: u64,
    dir: String,
    from: String,
    to: String,
    rewritten: Vec<String>,
    deleted: Option<(String, bool)>,
    result: Result<(), String>,
) -> Message {
    Message::AiFsDone(Box::new(AiFsDone { id, gen, dir, from, to, rewritten, deleted, result }))
}
/// 改名 / 移动：本地经引用同步事务，空间走服务端 rename + 引用改写；成功后按 Renamed 跟随语义
/// 收尾打开中的编辑面（与文件树改名同一链），失败只回执不动状态。
fn move_entry(app: &mut App, from: String, to: String, id: u64) -> iced::Task<Message> {
    if from == to {
        settle(id, Err("源与目标相同，无需移动".to_string()));
        return iced::Task::none();
    }
    let gen = app.epoch();
    let parent = parent_of(&from);
    match &app.repo_kind {
        RepoKind::Local => {
            let Ok(root) = app.vault.root() else {
                settle(id, Err("仓库未打开".to_string()));
                return iced::Task::none();
            };
            let Ok(exclude) = app.vault.exclude_folders() else {
                settle(id, Err("仓库配置读取失败".to_string()));
                return iced::Task::none();
            };
            let is_dir = atelyx_core::vault::safe_join(&root, &from, false)
                .map(|path| path.is_dir())
                .unwrap_or(false);
            let from_task = from.clone();
            let to_task = to.clone();
            iced::Task::perform(
                async move {
                    if is_dir {
                        atelyx_core::vault_refs::rename_folder_synced(
                            &root, &exclude, &from_task, &to_task,
                        )
                    } else {
                        atelyx_core::vault_refs::rename_note_synced(
                            &root, &exclude, &from_task, &to_task,
                        )
                    }
                },
                move |result| match result {
                    Ok(sync) => done(
                        id,
                        gen,
                        parent.clone(),
                        from.clone(),
                        to.clone(),
                        sync.rewritten,
                        None,
                        Ok(()),
                    ),
                    Err(e) => done(id, gen, String::new(), from, to, Vec::new(), None, Err(e)),
                },
            )
        }
        RepoKind::Space(session) => {
            if !session.role.can_write() {
                settle(id, Err("该空间为只读身份，无法编辑".to_string()));
                return iced::Task::none();
            }
            let client = session.client.clone();
            let space_id = session.space_id.clone();
            let from_task = from.clone();
            let to_task = to.clone();
            iced::Task::perform(
                async move {
                    client
                        .rename_file(&space_id, &from_task, &to_task)
                        .await
                        .map_err(|e| e.to_string())?;
                    // 树现拉：引用改写要基于最新全库清单（未就绪时不得静默零执行）
                    let tree = client.tree(&space_id).await.map_err(|e| e.to_string())?;
                    Ok::<Vec<String>, String>(
                        space_actions::rewrite_links_after_rename(
                            &client, &space_id, &tree, &from_task, &to_task,
                        )
                        .await,
                    )
                },
                move |result| {
                    let rewritten = result.as_ref().map(|r| r.clone()).unwrap_or_default();
                    done(
                        id,
                        gen,
                        parent.clone(),
                        from.clone(),
                        to.clone(),
                        rewritten,
                        None,
                        result.map(|_| ()),
                    )
                },
            )
        }
    }
}
/// 删除：本地按磁盘事实分派文件 / 目录（目录非空需 force），空间按树节点类型判定；
/// 成功后按 Deleted 跟随语义收尾打开中的面。
fn delete_entry(app: &mut App, path: String, force: bool, id: u64) -> iced::Task<Message> {
    let gen = app.epoch();
    let parent = parent_of(&path);
    match &app.repo_kind {
        RepoKind::Local => {
            let Ok(root) = app.vault.root() else {
                settle(id, Err("仓库未打开".to_string()));
                return iced::Task::none();
            };
            let path_task = path.clone();
            iced::Task::perform(
                async move {
                    // 附件目录需在文件仍在盘上时收集（删掉后读不到表 id）
                    let root_for_scan = root.clone();
                    let rel_for_scan = path_task.clone();
                    let attachment_dirs = tokio::task::spawn_blocking(move || {
                        atelyx_core::vault::table_attachment_dirs(
                            &root_for_scan,
                            &rel_for_scan,
                            false,
                        )
                    })
                    .await
                    .unwrap_or_default();
                    let abs = atelyx_core::vault::safe_join(&root, &path_task, false)?;
                    let removed = if abs.is_dir() {
                        let ack = atelyx_core::vault::delete_folder(&root, &path_task, force)?;
                        if !ack.deleted {
                            return Err(format!("目录非空（force 可强制删除）：{path_task}"));
                        }
                        (path_task, true)
                    } else if abs.is_file() {
                        atelyx_core::vault::delete_vault_file(&root, &path_task)
                            .map(|_| (path_task, false))?
                    } else {
                        return Err(format!("路径不存在：{path_task}"));
                    };
                    atelyx_core::vault::purge_table_attachments(attachment_dirs);
                    Ok::<(String, bool), String>(removed)
                },
                move |result| {
                    let deleted = result.as_ref().ok().cloned();
                    done(
                        id,
                        gen,
                        parent.clone(),
                        String::new(),
                        String::new(),
                        Vec::new(),
                        deleted,
                        result.map(|_| ()),
                    )
                },
            )
        }
        RepoKind::Space(session) => {
            if !session.role.can_write() {
                settle(id, Err("该空间为只读身份，无法编辑".to_string()));
                return iced::Task::none();
            }
            let client = session.client.clone();
            let space_id = session.space_id.clone();
            let path_task = path.clone();
            iced::Task::perform(
                async move {
                    let tree = client.tree(&space_id).await.map_err(|e| e.to_string())?;
                    let is_dir = space_actions::tree_node_is_dir(&tree, &path_task)
                        .ok_or_else(|| format!("路径不存在：{path_task}"))?;
                    if is_dir {
                        let ack = client
                            .delete_folder(&space_id, &path_task, force)
                            .await
                            .map_err(|e| e.to_string())?;
                        if !ack.deleted {
                            return Err(format!("目录非空（force 可强制删除）：{path_task}"));
                        }
                        Ok((path_task, true))
                    } else {
                        client
                            .delete_file(&space_id, &path_task)
                            .await
                            .map(|_| (path_task, false))
                            .map_err(|e| e.to_string())
                    }
                },
                move |result| {
                    let deleted = result.as_ref().ok().cloned();
                    done(
                        id,
                        gen,
                        parent.clone(),
                        String::new(),
                        String::new(),
                        Vec::new(),
                        deleted,
                        result.map(|_| ()),
                    )
                },
            )
        }
    }
}

/// 相对路径的父目录（根为 ""）。
fn parent_of(rel: &str) -> String {
    match rel.rfind('/') {
        Some(i) => rel[..i].to_string(),
        None => String::new(),
    }
}
/// 落盘回执：先交回工具线程（模型可见），再按跟随语义收尾打开中的面并刷新文件树。
#[allow(clippy::too_many_arguments)]
pub(crate) fn on_done(
    app: &mut App,
    id: u64,
    gen: u64,
    dir: String,
    from: String,
    to: String,
    rewritten: Vec<String>,
    deleted: Option<(String, bool)>,
    result: Result<(), String>,
) -> iced::Task<Message> {
    if gen != app.epoch() {
        settle(id, Err("仓库已切换，AI 操作已取消".to_string()));
        return iced::Task::none();
    }
    let (follow, ok_text) = match (&result, &deleted) {
        (Ok(()), Some((rel, is_dir))) => {
            let text = if *is_dir {
                format!("已删除目录 {rel}")
            } else {
                format!("已删除 {rel}")
            };
            (crate::FollowUp::Deleted { rel: rel.clone(), is_dir: *is_dir }, text)
        }
        (Ok(()), None) => (
            crate::FollowUp::Renamed {
                from: from.clone(),
                to: to.clone(),
                rewritten: rewritten.clone(),
                announce: false,
            },
            format!("已移动 {from} → {to}"),
        ),
        (Err(e), _) => {
            settle(id, Err(format!("操作失败：{e}")));
            app.notify(NoticeLevel::Error, format!("AI 文件操作失败：{e}"));
            return iced::Task::none();
        }
    };
    settle(id, Ok(ok_text));
    // 与文件树操作同一条收尾链：插件广播 → 打开中的面跟随 → 树 / 名册 / 反链刷新
    match &follow {
        crate::FollowUp::Renamed { from, to, .. } if from != to => {
            app.plugins.on_file_changed(
                to,
                atelyx_core::plugin_runtime::FileChangeKind::Renamed,
                Some(from.as_str()),
            );
        }
        crate::FollowUp::Deleted { rel, .. } => {
            app.plugins.on_file_changed(
                rel,
                atelyx_core::plugin_runtime::FileChangeKind::Deleted,
                None,
            );
        }
        _ => {}
    }
    app.apply_follow(&follow);
    let mut reload_note: Option<String> = None;
    if let crate::FollowUp::Renamed { rewritten, .. } = &follow {
        if let Some(f) = app.ui.last_note_file.clone() {
            if rewritten.contains(&f) {
                reload_note = Some(f);
            }
        }
    }
    if let crate::FollowUp::Renamed { from, to, .. } = &follow {
        app.follow_children_cache(from, to);
    }
    let mut next = if matches!(app.repo_kind, RepoKind::Space(_)) {
        space_actions::refresh_space_tree(app)
    } else {
        app.load_children(&dir)
    };
    if !matches!(app.repo_kind, RepoKind::Space(_)) {
        if let crate::FollowUp::Renamed { from, .. } = &follow {
            let from_dir = parent_of(from);
            if from_dir != dir {
                next = next.chain(app.load_children(&from_dir));
            }
        }
    }
    let mut next = next
        .chain(app.refresh_note_files())
        .chain(app.refresh_tag_vocab())
        .chain(app.refresh_backlinks());
    if matches!(follow, crate::FollowUp::Renamed { .. }) {
        next = next.chain(app.load_vault_meta());
    }
    if let Some(rel) = reload_note {
        next = next.chain(app.load_note(&rel));
    }
    next
}
