//! 插件文档会话的壳侧实现：读盘 / 建立会话 / 落盘状态记账 / 保存 / 改名。
//! 正文与协作都在内容所有者插件；本模块只做会话寻址、磁盘基线校验与原子写。

use iced::Task;
use serde_json::json;

use atelyx_core::plugin_runtime::{
    method, ContentPropsReadParams, ContentPropsResult, ContentPropsWriteParams, DocCloseParams,
    DocOpenParams, DocPropsWriteParams, DocRenameParams, DocSaveParams, DocSaveResult,
    DocStateParams, DocTargetParams, DocChangedPayload,
};

use crate::settings_view::plugins::{DocCommitDone, PluginsMsg};
use crate::{vault, App, Message, RepoKind};

pub use atelyx_core::doc_session::DocOpenGuard;
use atelyx_core::doc_session::{
    discard_line, DocId, DocSession, DocSessions, DocSpec, DocStateUpdate, OpenTicket,
};
use atelyx_core::doc_type::DocTypeRoute;
use atelyx_core::layout::AppUiState;

/// 打开集合（持久化意图）与会话表（运行时事实）的同步登记：宿主打开动作只经此处增。
/// 同一路径重复打开复用原会话并把该文档置热（同类型旧会话随之转冷）。
pub(crate) fn register_open(
    ui: &mut AppUiState,
    sessions: &mut DocSessions,
    rel: &str,
    route: &DocTypeRoute,
    editable: bool,
) -> DocId {
    let title = rel.rsplit('/').next().unwrap_or(rel).to_string();
    let spec = DocSpec {
        path: rel.to_string(),
        doc_type: route.id.clone(),
        plugin_id: route.owner_plugin.clone(),
        surface: route.views.first().map(|view| view.view_id.clone()).unwrap_or_default(),
        title,
        editable,
    };
    ui.remember_open_doc(rel, route.id.as_str());
    let id = sessions.open_or_reuse(spec);
    sessions.set_hot(&id);
    id
}

/// 打开集合与会话表同步移除（调用方必须先 flush 再关闭文档）。
pub(crate) fn unregister_open(ui: &mut AppUiState, sessions: &mut DocSessions, rel: &str) {
    ui.forget_open_doc(rel);
    if let Some(id) = sessions.by_path(rel).map(|session| session.id.clone()) {
        sessions.close(&id);
    }
}

/// 读盘完成回填：发起时登记的凭据 + 读盘结果（凭据由 open 原样带回）。
#[derive(Debug, Clone)]
pub struct OpenReadResult {
    ticket: OpenTicket,
    result: Result<String, String>,
    can_write: bool,
    doc_id: DocId,
    open_seq: u64,
    created: bool,
}

/// 校验会话身份：文档存在且 open_seq 仍是当前序号；旧序号的调用一律拒绝。
fn session_for(app: &App, doc_id: &str, open_seq: u64) -> Option<DocId> {
    let id = DocId::new(doc_id);
    let session = app.doc_sessions.session(&id)?;
    (session.open_seq == open_seq).then_some(id)
}

/// 磁盘基线哈希（口径见 atelyx_core::doc_session::content_rev）。
pub(crate) use atelyx_core::doc_session::content_rev as sha256_hex;

/// 广播某个文档会话的状态事实（会话表即真源；正文归内容所有者插件）。
pub fn publish_state(app: &App, id: &DocId) {
    let Some(session) = app.doc_sessions.session(id) else { return };
    app.plugins.on_doc_changed(DocChangedPayload {
        path: session.path.clone(),
        doc_id: Some(session.id.as_str().to_string()),
        open_seq: Some(session.open_seq),
        dirty: session.dirty,
        save_error: session.save_error.clone(),
    });
}

/// 把落盘状态写回会话表并广播（open_seq 不符即丢弃：迟到的旧回执不得覆盖新状态）。
pub(crate) fn report_state(
    app: &mut App,
    id: &DocId,
    open_seq: u64,
    dirty: bool,
    error: Option<String>,
) {
    let updated = app.doc_sessions.update_state(
        id,
        open_seq,
        DocStateUpdate { dirty, save_error: error, title: None },
    );
    if updated {
        publish_state(app, id);
    }
}

/// 打开一个文档会话：本地读盘 / 空间走服务端内容面，成功后建立会话并回执正文。
/// 每次实际读盘都向守卫登记凭据，回填时按凭据判定是否仍属于当前打开动作。
pub fn open(
    app: &mut App,
    request_id: u64,
    plugin_id: &str,
    params: DocOpenParams,
) -> Task<Message> {
    let rel = match vault::validate_plugin_path(&params.doc_id) {
        Ok(rel) => rel,
        Err(e) => {
            app.plugins.resolve_shell_call(request_id, Err(e));
            return Task::none();
        }
    };
    if session_for(app, &rel, params.open_seq).is_none() {
        app.plugins.resolve_shell_call(
            request_id,
            Err("文档会话不存在或打开序号已过期，请重新打开".into()),
        );
        return Task::none();
    }
    let created = app.doc_open.as_ref().is_some_and(|open| open.created);
    // 换篇前必须没有未落盘输入：正常打开走 gate，这里是插件直接调用的兜底。
    if app.doc_sessions.by_path(&rel).is_some_and(|session| session.dirty) {
        app.plugins
            .resolve_shell_call(request_id, Err("当前文档有未保存修改，请先保存".into()));
        return Task::none();
    }
    let gen = app.epoch();
    match app.repo_kind.clone() {
        RepoKind::Local => {
            let root = match app.vault.root() {
                Ok(root) => root,
                Err(e) => {
                    app.plugins.resolve_shell_call(request_id, Err(e));
                    return Task::none();
                }
            };
            let path = rel.clone();
            let ticket = app.doc_open_guard.start(plugin_id, request_id, gen, &rel);
            Task::perform(
                async move {
                    tokio::task::spawn_blocking(move || vault::read_note(&root, &path))
                        .await
                        .map_err(|e| e.to_string())?
                },
                move |result| {
                    Message::Plugins(PluginsMsg::DocOpenDone(OpenReadResult {
                        ticket,
                        result,
                        can_write: true,
                        doc_id: DocId::new(&rel),
                        open_seq: params.open_seq,
                        created,
                    }))
                },
            )
        }
        RepoKind::Space(_) => {
            let Some(session) = crate::space_actions::session_of(app) else {
                app.plugins.resolve_shell_call(
                    request_id,
                    Err("协作依赖协作空间：当前不是协作空间".into()),
                );
                return Task::none();
            };
            let can_write = session.role.can_write();
            let (client, space_id) = (session.client, session.space_id);
            let path = rel.clone();
            let ticket = app.doc_open_guard.start(plugin_id, request_id, gen, &rel);
            Task::perform(
                async move {
                    client
                        .get_file_text(&space_id, &path)
                        .await
                        .map_err(|e| e.to_string())
                },
                move |result| {
                    Message::Plugins(PluginsMsg::DocOpenDone(OpenReadResult {
                        ticket,
                        result,
                        can_write,
                        doc_id: DocId::new(&rel),
                        open_seq: params.open_seq,
                        created,
                    }))
                },
            )
        }
    }
}

/// 读盘完成：守卫校验归属后写入会话基线并回执正文；被取代 / 跨世代的结果直接丢弃并留日志。
pub fn open_done(app: &mut App, done: OpenReadResult) -> Task<Message> {
    let OpenReadResult { ticket, result, can_write, doc_id, open_seq, created } = done;
    if let Err(reason) = app.doc_open_guard.accept(&ticket, app.epoch()) {
        let line = discard_line(ticket.path(), reason);
        if !app.plugins.log_to_plugin(ticket.plugin_id(), &line) {
            eprintln!("{line}");
        }
        app.plugins
            .resolve_shell_call(ticket.request_id(), Err(reason.text().into()));
        return Task::none();
    }
    let request_id = ticket.request_id();
    let path = ticket.path().to_string();
    // 启动恢复只把文件挂回已有视图：最近打开清单与磁盘一致，不重排、不写盘。
    let restored = app.doc_open.as_ref().is_some_and(|open| {
        open.mode == crate::plugin_actions::OpenMode::Restore && open.path == path
    });
    match result {
        Ok(text) => {
            let disk_rev = sha256_hex(text.as_bytes());
            // 读盘成功即新基线：会话按新序号记干净（打开前已由脏拦截保证没有未落盘输入）
            app.doc_sessions.update_state(
                &doc_id,
                open_seq,
                DocStateUpdate { dirty: false, save_error: None, title: None },
            );
            publish_state(app, &doc_id);
            if !restored {
                crate::recent_files::record(
                    app,
                    &path,
                    atelyx_core::plugin_runtime::RecentKind::Note,
                );
            }
            let value = json!({
                "docId": path.clone(),
                "openSeq": open_seq,
                "path": path,
                "text": text,
                "canWrite": can_write,
                "diskRev": disk_rev,
            });
            app.plugins.resolve_shell_call(request_id, Ok(value));
        }
        Err(e) => {
            // 读盘失败只撤销本次新建的登记；已存在的会话保留。
            if created {
                app.forget_open_doc(&path);
            }
            app.plugins.resolve_shell_call(request_id, Err(e));
        }
    }
    // 恢复读盘成功或失败都算这份请求收尾（正文仍由插件面异步呈现）
    if restored {
        app.startup_restore_doc_done()
    } else {
        Task::none()
    }
}

pub(crate) fn can_write(app: &App) -> bool {
    match app.repo_kind {
        RepoKind::Local => true,
        RepoKind::Space(_) => crate::space_actions::session_of(app)
            .is_some_and(|session| session.role.can_write()),
    }
}

/// 内容所有者上报一次会话状态：宿主只记脏与保存错误，并据此排自动保存。
pub fn state(app: &mut App, request_id: u64, params: DocStateParams) -> Task<Message> {
    if session_for(app, &params.doc_id, params.open_seq).is_none() {
        app.plugins
            .resolve_shell_call(request_id, Err("插件上报的文档会话与当前会话不一致".into()));
        return Task::none();
    }
    let id = DocId::new(&params.doc_id);
    let failed = params.save_error.is_some();
    report_state(app, &id, params.open_seq, params.dirty, params.save_error);
    // 脏且无保存错误才排自动保存：防抖到点由宿主向所有者发 panel:flush，插件内部提交保存。
    // 失败后不再无退避地重试（每 500ms 一轮读写只会反复失败），重试交给下一次输入或手动保存
    if params.dirty && !failed {
        app.autosave_deadline = Some(std::time::Instant::now() + crate::AUTOSAVE_DEBOUNCE);
    }
    app.plugins.resolve_shell_call(request_id, Ok(json!({})));
    app.chain_autosave_timer(Task::none())
}

/// 把当前文档改名为新的主名：同目录、保留扩展名，引用同步与历史随宿主既有改名链。
/// 有未落盘修改时拒绝（插件先保存再重试），改名落地后插件经 vault:file.changed 重开本篇。
pub fn rename(app: &mut App, request_id: u64, params: DocRenameParams) -> Task<Message> {
    let Some(open) = app.doc_open.as_ref().map(|open| open.path.clone()) else {
        app.plugins
            .resolve_shell_call(request_id, Err("当前没有打开的文档".into()));
        return Task::none();
    };
    // 同类文档可多会话并存：只认这条路径登记的最新会话，别把别的文档的脏状态当成本篇的
    let latest = app.doc_sessions.by_path(&open).map(|session| session.open_seq);
    if latest != app.doc_open.as_ref().map(|open| open.open_seq) {
        app.plugins
            .resolve_shell_call(request_id, Err("文档会话已更换，请重新保存".into()));
        return Task::none();
    }
    if open != params.path {
        app.plugins
            .resolve_shell_call(request_id, Err("插件改名的文档路径与当前会话不一致".into()));
        return Task::none();
    }
    if !can_write(app) {
        app.plugins
            .resolve_shell_call(request_id, Err("该空间为只读身份，无法改名".into()));
        return Task::none();
    }
    if app.doc_sessions.by_path(&open).is_some_and(|session| session.dirty) {
        app.plugins
            .resolve_shell_call(request_id, Err("文档有未保存修改，请先保存".into()));
        return Task::none();
    }
    let from = params.path.clone();
    let Some(to) = renamed_path(&from, &params.name) else {
        app.plugins
            .resolve_shell_call(request_id, Err("文件名不能为空".into()));
        return Task::none();
    };
    if to == from {
        app.plugins.resolve_shell_call(request_id, Ok(json!({})));
        return Task::none();
    }
    crate::settings_view::plugins::execute_vault_rename_allow_open(app, request_id, from, to)
}

/// 改名目标路径：与文件树改名同一口径（剥手打后缀、净化非法字符、保留原名扩展名）。
fn renamed_path(from: &str, input: &str) -> Option<String> {
    let old_name = from.rsplit('/').next().unwrap_or(from);
    let new_name = vault::rename_file_name(input, old_name)?;
    let dir = from.rsplit_once('/').map(|(dir, _)| format!("{dir}/")).unwrap_or_default();
    Some(format!("{dir}{new_name}"))
}

/// 写盘 / 基线校验完成：按回执三态改写会话的脏与错误并回执，正文差异仍以插件随后的上报为准。
pub fn commit_done(app: &mut App, done: DocCommitDone) -> Task<Message> {
    if app.epoch() != done.gen {
        app.plugins
            .resolve_shell_call(done.request_id, Err("仓库已切换，写入结果作废".into()));
        return Task::none();
    }
    let Some(id) = session_for(app, &done.doc_id, done.open_seq) else {
        app.plugins
            .resolve_shell_call(done.request_id, Err("文档会话已关闭或打开序号已过期".into()));
        return Task::none();
    };
    if let Some(error) = &done.history_error {
        app.notify(crate::ui::notify::Level::Warning, error.clone());
    }
    let value = serde_json::to_value(&done.result).unwrap_or_default();
    let (dirty, error) = match &done.result {
        DocSaveResult::Ok { .. } => (false, None),
        DocSaveResult::Conflict { .. } => (true, None),
        DocSaveResult::Err { message } => (true, Some(message.clone())),
    };
    app.doc_sessions.update_state(
        &id,
        done.open_seq,
        DocStateUpdate { dirty, save_error: error, title: None },
    );
    publish_state(app, &id);
    // 回滚的成功提示挂在发起回滚那篇文档的落盘回执上：别的文档保存成功不算回滚完成
    if app.history_restore_doc.as_ref() == Some(&id) {
        app.settle_history_restore(&id, matches!(&done.result, DocSaveResult::Ok { .. }));
    }
    app.plugins.resolve_shell_call(done.request_id, Ok(value));
    Task::none()
}

/// 内容所有者提交完整正文：写前即时读盘比对 expected_rev，冲突把磁盘原文原样交回插件。
pub fn save(app: &mut App, request_id: u64, params: DocSaveParams) -> Task<Message> {
    if session_for(app, &params.doc_id, params.open_seq).is_none() {
        app.plugins
            .resolve_shell_call(request_id, Err("插件保存的文档会话与当前会话不一致".into()));
        return Task::none();
    }
    if !can_write(app) {
        app.plugins
            .resolve_shell_call(request_id, Err("该空间为只读身份，无法保存文档".into()));
        return Task::none();
    }
    if let Err(e) = params.validate() {
        app.plugins.resolve_shell_call(request_id, Err(e));
        return Task::none();
    }
    if matches!(app.repo_kind, RepoKind::Space(_)) {
        return crate::space_actions::space_commit_doc(app, request_id, params);
    }
    let Ok(root) = app.vault.root() else {
        app.plugins.resolve_shell_call(request_id, Err("仓库未打开".into()));
        return Task::none();
    };
    let gen = app.epoch();
    let queue = app.vault.writes.clone();
    let doc_id = params.doc_id.clone();
    let open_seq = params.open_seq;
    let expected_rev = params.expected_rev;
    let text = params.text;
    let async_doc_id = doc_id.clone();
    Task::perform(
        async move {
            let path = match vault::safe_join(&root, &async_doc_id, false) {
                Ok(path) => path,
                Err(e) => return DocSaveResult::Err { message: e },
            };
            let disk_path = path.clone();
            let disk = tokio::task::spawn_blocking(move || std::fs::read_to_string(disk_path)).await;
            let disk_text = match disk {
                Ok(Ok(text)) => text,
                Ok(Err(e)) => {
                    return DocSaveResult::Err { message: format!("写前读盘失败：{e}") }
                }
                Err(e) => {
                    return DocSaveResult::Err { message: format!("写前读盘任务失败：{e}") }
                }
            };
            let disk_rev = sha256_hex(disk_text.as_bytes());
            if disk_rev != expected_rev {
                return DocSaveResult::Conflict { disk_rev, disk_content: disk_text };
            }
            if let Err(e) = queue.write(path, text.clone()).await {
                return DocSaveResult::Err { message: e };
            }
            DocSaveResult::Ok { new_rev: sha256_hex(text.as_bytes()) }
        },
        move |result| {
            Message::Plugins(PluginsMsg::DocCommitDone(DocCommitDone {
                request_id,
                doc_id,
                open_seq,
                gen,
                result,
                history_error: None,
            }))
        },
    )
}

/// 关闭插件文档：先回执已受理，再经 gate 落盘后结束会话；保存失败即中止，文档留着。
pub fn close(app: &mut App, request_id: u64, params: DocCloseParams) -> Task<Message> {
    let Some(id) = session_for(app, &params.doc_id, params.open_seq) else {
        app.plugins
            .resolve_shell_call(request_id, Err("文档会话不存在或打开序号已过期".into()));
        return Task::none();
    };
    // 先回执已受理：关闭要等 gate 落盘，不能让插件那次调用悬着
    app.plugins.resolve_shell_call(request_id, Ok(json!({})));
    app.close_document(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_target_strips_suffix_and_keeps_the_folder() {
        assert_eq!(
            renamed_path("notes/方案.md", "新方案.md").as_deref(),
            Some("notes/新方案.md"),
            "手打后缀不落成双后缀"
        );
        assert_eq!(
            renamed_path("notes/方案.md", "a/b:c").as_deref(),
            Some("notes/a_b_c.md"),
            "非法字符按文件树同一口径净化"
        );
        assert_eq!(renamed_path("notes/方案.md", ""), None, "空名拒绝");
        assert_eq!(renamed_path("notes/方案.md", ".md"), None, "只剩后缀 = 空主名");
    }

    /// 打开集合 = 持久化意图、会话表 = 运行时事实：只在 register / unregister 里成对增删。
    #[test]
    fn open_set_and_sessions_move_together() {
        use atelyx_core::doc_type::{DocTypeId, DocViewRef};

        let route = DocTypeRoute {
            id: DocTypeId::new("note"),
            owner_plugin: "atelyx.notes".into(),
            declaring_plugin: "atelyx.notes".into(),
            views: vec![DocViewRef { plugin_id: "atelyx.notes".into(), view_id: "note".into() }],
            extensions: vec!["md".into()],
            editable: true,
            multi_doc: false,
            label: "笔记".into(),
            icon: "file-text".into(),
            preset: true,
        };
        let mut ui = AppUiState::default();
        let mut sessions = DocSessions::default();
        let a = register_open(&mut ui, &mut sessions, "a.md", &route, true);
        let b = register_open(&mut ui, &mut sessions, "b.md", &route, true);
        assert_eq!(ui.open_docs.len(), 2);
        assert!(sessions.session(&a).is_some() && sessions.session(&b).is_some());
        assert_eq!(
            sessions.hot_of("atelyx.notes", &DocTypeId::new("note")).unwrap().id,
            b,
            "后打开的置热，旧会话转冷"
        );
        unregister_open(&mut ui, &mut sessions, "a.md");
        assert_eq!(ui.open_docs.len(), 1);
        assert!(sessions.session(&a).is_none());
        assert!(sessions.session(&b).is_some());
    }
}

/// 读文档属性：宿主只寻址会话并转发给内容所有者，不解析 frontmatter。
pub fn props_read(app: &App, request_id: u64, target: DocTargetParams) -> Task<Message> {
    let Some(session) = target_session(app, &target.doc_id) else {
        return crate::settings_view::plugins::resolve_vault(
            app,
            request_id,
            Ok(serde_json::to_value(ContentPropsResult::default()).unwrap_or_default()),
        );
    };
    let params = ContentPropsReadParams {
        doc_id: session.id.as_str().to_string(),
        open_seq: session.open_seq,
    };
    forward_content(app, request_id, &session.plugin_id, method::content::PROPS_READ, json!(params))
}

/// 用新的属性块改写属性区：转发给内容所有者，由它改自己的缓冲并提交正文。
pub fn props_write(app: &App, request_id: u64, params: DocPropsWriteParams) -> Task<Message> {
    let Some(session) = target_session(app, &params.doc_id) else {
        return crate::settings_view::plugins::resolve_vault(
            app,
            request_id,
            Err("没有打开的文档".to_string()),
        );
    };
    if !can_write(app) {
        return crate::settings_view::plugins::resolve_vault(
            app,
            request_id,
            Err("当前文档不可写".to_string()),
        );
    }
    let host_params = ContentPropsWriteParams {
        doc_id: session.id.as_str().to_string(),
        open_seq: session.open_seq,
        value: params.value,
    };
    forward_content(app, request_id, &session.plugin_id, method::content::PROPS_WRITE, json!(host_params))
}

/// 属性寻址的会话：目标为空串 = 当前打开的插件文档，否则按 docId 找会话。
fn target_session<'a>(app: &'a App, doc_id: &str) -> Option<&'a DocSession> {
    let id = if doc_id.is_empty() {
        app.doc_open.as_ref().map(|open| open.doc_id.clone())?
    } else {
        DocId::new(doc_id)
    };
    app.doc_sessions.session(&id)
}

/// 宿主 → 内容所有者的属性请求：只转发，回执原样交回属性面板。
fn forward_content(
    app: &App,
    request_id: u64,
    plugin_id: &str,
    method_name: &'static str,
    params: serde_json::Value,
) -> Task<Message> {
    let receiver = match app.plugins.call_plugin(
        plugin_id,
        method::content::SERVICE,
        method_name,
        params,
    ) {
        Ok(receiver) => receiver,
        Err(e) => {
            return crate::settings_view::plugins::resolve_vault(app, request_id, Err(e));
        }
    };
    Task::perform(
        async move {
            tokio::task::spawn_blocking(move || {
                receiver
                    .recv()
                    .unwrap_or_else(|_| Err("属性请求未收到回执".to_string()))
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()))
        },
        move |result| {
            Message::Plugins(PluginsMsg::DocPropsForwarded { request_id, result })
        },
    )
}
