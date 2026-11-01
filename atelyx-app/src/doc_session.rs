//! 插件文档会话的壳侧实现：读盘 / 建立会话 / 编辑上报 / 保存 / 在场 / 改名。
//! 正文真源与保存链仍在 `App`（文档内容的唯一出口），本模块只做服务请求的适配。

use iced::Task;
use serde_json::json;

use atelyx_core::plugin_runtime::{
    DocAwarenessParams, DocEditParams, DocOpenParams, DocRenameParams, DocSaveParams,
};

use atelyx_core::plugin_runtime::{
    DocChangedPayload, DocPresencePayload, DocRemoteCursor,
};

use crate::settings_view::plugins::PluginsMsg;
use crate::{vault, App, Message, RepoKind, SaveReason};

/// 把当前文档的正文 / 落盘状态广播给订阅插件（`text` 只在宿主改写正文时给出）。
pub fn publish_state(app: &App, text: Option<String>) {
    let Some(path) = app.ui.last_note_file.clone() else { return };
    app.plugins.on_doc_changed(DocChangedPayload {
        path,
        text,
        dirty: app.doc_dirty,
        save_error: app.doc_save_error.clone(),
    });
}

/// 把当前文档的远端协作者光标广播给订阅插件（未打开文档 / 无协作者 = 空清单）。
/// 光标偏移与 `doc_text` 同一坐标系，插件据此在原位画他人光标。
pub fn publish_presence(app: &App) {
    let path = app.ui.last_note_file.clone();
    let cursors = app
        .remote_cursors
        .iter()
        .map(|cursor| DocRemoteCursor {
            name: cursor.name.clone(),
            color: cursor.color.clone(),
            anchor: cursor.anchor,
            head: cursor.head,
        })
        .collect();
    app.plugins.on_doc_presence(DocPresencePayload { path, cursors });
}

/// 打开一个文档会话：本地读盘 / 空间走服务端内容面，成功后建立会话并回执正文。
pub fn open(app: &App, request_id: u64, params: DocOpenParams) -> Task<Message> {
    let rel = match vault::validate_plugin_path(&params.path) {
        Ok(rel) => rel,
        Err(e) => {
            app.plugins.resolve_shell_call(request_id, Err(e));
            return Task::none();
        }
    };
    // 同一篇已由插件承载：直接回内存真源（插件重启 / 重发 doc.open 不得用旧盘文覆盖未落盘输入）。
    if app.doc_active && app.ui.last_note_file.as_deref() == Some(rel.as_str()) {
        let result = json!({
            "path": rel,
            "text": app.doc_text.clone(),
            "canWrite": can_write(app),
            "dirty": app.doc_dirty,
            "saveError": app.doc_save_error.clone(),
        });
        app.plugins.resolve_shell_call(request_id, Ok(result));
        return Task::none();
    }
    // 换篇前必须没有未落盘输入：正常打开走 gate，这里是插件直接调用的兜底。
    if app.ui.last_note_file.is_some() && app.doc_dirty {
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
            let read = Task::perform(
                async move {
                    tokio::task::spawn_blocking(move || vault::read_note(&root, &path))
                        .await
                        .map_err(|e| e.to_string())?
                },
                move |result| {
                    Message::Plugins(PluginsMsg::DocOpenDone {
                        request_id,
                        gen,
                        path: rel,
                        result,
                        can_write: true,
                    })
                },
            );
            read
        }
        RepoKind::Space(_) => {
            let Some(session) = crate::space_actions::session_of(app) else {
                app.plugins
                    .resolve_shell_call(request_id, Err("协作依赖协作空间：当前不是协作空间".into()));
                return Task::none();
            };
            let can_write = session.role.can_write();
            let (client, space_id) = (session.client, session.space_id);
            let path = rel.clone();
            let read = Task::perform(
                async move {
                    client
                        .get_file_text(&space_id, &path)
                        .await
                        .map_err(|e| e.to_string())
                },
                move |result| {
                    Message::Plugins(PluginsMsg::DocOpenDone {
                        request_id,
                        gen,
                        path: rel,
                        result,
                        can_write,
                    })
                },
            );
            read
        }
    }
}

/// 读盘完成：世代守卫后建立会话并回执正文。
pub fn open_done(
    app: &mut App,
    request_id: u64,
    gen: u64,
    path: String,
    result: Result<String, String>,
    can_write: bool,
) -> Task<Message> {
    if gen != app.epoch() {
        app.plugins
            .resolve_shell_call(request_id, Err("仓库已切换，请重新打开".into()));
        return Task::none();
    }
    // 读盘在途期间插件可能已对旧篇提交按键（脏态出现即拒绝），否则旧篇输入被本篇正文覆盖。
    if result.is_ok() && app.doc_dirty {
        app.plugins.resolve_shell_call(
            request_id,
            Err("当前文档有未保存修改，请先保存".into()),
        );
        return Task::none();
    }
    match result {
        Ok(text) => {
            app.attach_doc_session(&path, text.clone());
            crate::recent_view::record(app, &path, crate::recent_view::RecentKind::Note);
            let value = json!({
                "path": path,
                "text": text,
                "canWrite": can_write,
                "dirty": false,
            });
            app.plugins.resolve_shell_call(request_id, Ok(value));
        }
        Err(e) => {
            app.plugins.resolve_shell_call(request_id, Err(e));
        }
    }
    Task::none()
}

pub(crate) fn can_write(app: &App) -> bool {
    match app.repo_kind {
        RepoKind::Local => true,
        RepoKind::Space(_) => crate::space_actions::session_of(app)
            .is_some_and(|session| session.role.can_write()),
    }
}

/// 提交一次编辑：路径必须与当前会话一致；内容有变化才置脏与排自动保存。
pub fn edit(app: &mut App, request_id: u64, params: DocEditParams) -> Task<Message> {
    if app.ui.last_note_file.as_deref() != Some(params.path.as_str()) {
        app.plugins
            .resolve_shell_call(request_id, Err("插件提交的文档路径与当前会话不一致".into()));
        return Task::none();
    }
    if !can_write(app) {
        app.plugins
            .resolve_shell_call(request_id, Err("该空间为只读身份，无法编辑文档".into()));
        return Task::none();
    }
    let selection = params.anchor.zip(params.head).map(|(anchor, head)| {
        (
            clamp_boundary(&params.text, anchor),
            clamp_boundary(&params.text, head),
        )
    });
    let correction = app.apply_doc_edit(params.text, selection);
    if let Some(text) = correction {
        publish_state(app, Some(text));
    }
    app.plugins.resolve_shell_call(request_id, Ok(json!({})));
    Task::none()
}

/// 把外部给的光标偏移钳到字符边界（插件传来的偏移可能落在多字节字符中间）。
pub(crate) fn clamp_boundary(text: &str, mut offset: usize) -> usize {
    offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// 把当前文档改名为新的主名：同目录、保留扩展名，引用同步与历史随宿主既有改名链。
/// 有未落盘修改时拒绝（插件先保存再重试），改名落地后插件经 `vault:file.changed` 重开本篇。
pub fn rename(app: &mut App, request_id: u64, params: DocRenameParams) -> Task<Message> {
    if app.ui.last_note_file.as_deref() != Some(params.path.as_str()) {
        app.plugins
            .resolve_shell_call(request_id, Err("插件改名的文档路径与当前会话不一致".into()));
        return Task::none();
    }
    if !can_write(app) {
        app.plugins
            .resolve_shell_call(request_id, Err("该空间为只读身份，无法改名".into()));
        return Task::none();
    }
    if app.doc_dirty {
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

/// 请求保存：先收当前正文，再走既有保存链；结果经 `doc:changed` 到达。
pub fn save(app: &mut App, request_id: u64, params: DocSaveParams) -> Task<Message> {
    if app.ui.last_note_file.as_deref() != Some(params.path.as_str()) {
        app.plugins
            .resolve_shell_call(request_id, Err("插件保存的文档路径与当前会话不一致".into()));
        return Task::none();
    }
    if !can_write(app) {
        app.plugins
            .resolve_shell_call(request_id, Err("该空间为只读身份，无法保存文档".into()));
        return Task::none();
    }
    app.plugins.resolve_shell_call(request_id, Ok(json!({})));
    let reason = if params.manual { SaveReason::Manual } else { SaveReason::AutoSave };
    app.submit_save(reason)
}

/// 关闭当前会话：只认与当前路径一致的关闭（换文件时旧实例的关闭不得摘掉新会话）。
pub fn close(app: &mut App, request_id: u64, path: &str) -> Task<Message> {
    if app.ui.last_note_file.as_deref() == Some(path) {
        app.close_doc_session();
    }
    app.plugins.resolve_shell_call(request_id, Ok(json!({})));
    Task::none()
}

/// 在场上报：选区写入会话状态并上行（仅协作空间实际发出）。
pub fn awareness(app: &mut App, request_id: u64, params: DocAwarenessParams) -> Task<Message> {
    if app.ui.last_note_file.as_deref() != Some(params.path.as_str()) {
        app.plugins
            .resolve_shell_call(request_id, Err("插件上报的文档路径与当前会话不一致".into()));
        return Task::none();
    }
    app.doc_selection = params.anchor.zip(params.head).map(|(anchor, head)| {
        (
            clamp_boundary(&app.doc_text, anchor),
            clamp_boundary(&app.doc_text, head),
        )
    });
    crate::space_actions::collab_after_caret(app);
    app.plugins.resolve_shell_call(request_id, Ok(json!({})));
    Task::none()
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
}
