//! 插件文档会话的壳侧实现：读盘 / 建立会话 / 编辑上报 / 保存 / 在场 / 改名。
//! 正文真源与保存链仍在 `App`（文档内容的唯一出口），本模块只做服务请求的适配。

use iced::Task;
use serde_json::json;

use atelyx_core::plugin_runtime::{
    DocAwarenessParams, DocEditParams, DocOpenParams, DocPropsResult, DocPropsWriteParams,
    DocRenameParams, DocSaveParams,
};

use atelyx_core::plugin_runtime::{
    DocChangedPayload, DocPresencePayload, DocRemoteCursor,
};

use crate::settings_view::plugins::PluginsMsg;
use crate::{vault, App, Message, RepoKind, SaveReason};

/// 一次读盘回填的归属凭据：身份（插件 id + 请求 id）与序号（本会话内的打开次序），
/// 连同发起时的世代与路径。守卫只认与当前登记完全一致的凭据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenTicket {
    plugin_id: String,
    request_id: u64,
    seq: u64,
    gen: u64,
    path: String,
}

/// 读盘完成回填：发起时登记的凭据 + 读盘结果（凭据由 `open` 原样带回）。
#[derive(Debug, Clone)]
pub struct OpenReadResult {
    ticket: OpenTicket,
    result: Result<String, String>,
    can_write: bool,
}

/// 读盘结果被守卫丢弃的原因（决定回执文案与日志说明）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscardReason {
    /// 打开动作已被新的打开 / 关闭取代（凭据不再指向当前登记）。
    Superseded,
    /// 读盘跨过了仓库切换。
    VaultSwitched,
}

impl DiscardReason {
    fn text(self) -> &'static str {
        match self {
            Self::Superseded => "文档打开已被新的请求取代",
            Self::VaultSwitched => "仓库已切换，请重新打开",
        }
    }
}

/// 在途读盘的归属守卫：只有最后一次登记、且身份 / 序号 / 世代都对得上的结果可以建立会话。
/// 打开新文件、关闭实例、插件重发 `doc.open` 都推进序号，旧结果据此丢弃。
#[derive(Debug, Default)]
pub struct DocOpenGuard {
    seq: u64,
    current: Option<OpenTicket>,
}

impl DocOpenGuard {
    /// 登记一次新的读盘并返回它的凭据；更早登记的读盘结果全部作废。
    pub(crate) fn start(
        &mut self,
        plugin_id: &str,
        request_id: u64,
        gen: u64,
        path: &str,
    ) -> OpenTicket {
        self.seq = self.seq.saturating_add(1);
        let ticket = OpenTicket {
            plugin_id: plugin_id.to_string(),
            request_id,
            seq: self.seq,
            gen,
            path: path.to_string(),
        };
        self.current = Some(ticket.clone());
        ticket
    }

    /// 实例被换下 / 关闭：在途读盘结果不得再回填。
    pub(crate) fn invalidate(&mut self) {
        self.seq = self.seq.saturating_add(1);
        self.current = None;
    }

    /// 校验回填归属：接受即消耗本次打开（一次打开只建立一次会话）。
    fn accept(&mut self, ticket: &OpenTicket, epoch: u64) -> Result<(), DiscardReason> {
        if ticket.gen != epoch {
            return Err(DiscardReason::VaultSwitched);
        }
        let matched = self.current.as_ref().is_some_and(|current| current == ticket);
        if !matched {
            return Err(DiscardReason::Superseded);
        }
        self.current = None;
        Ok(())
    }
}

/// 被丢弃读盘的日志文案：含被丢弃的路径与原因，便于在插件日志里定位。
fn discard_line(path: &str, reason: DiscardReason) -> String {
    format!("文档读盘结果被丢弃：{path}（{}）", reason.text())
}

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
/// 每次实际读盘都向守卫登记凭据，回填时按凭据判定是否仍属于当前打开动作。
pub fn open(
    app: &mut App,
    request_id: u64,
    plugin_id: &str,
    params: DocOpenParams,
) -> Task<Message> {
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
            let ticket = app.doc_open_guard.start(plugin_id, request_id, gen, &rel);
            let read = Task::perform(
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
                    }))
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
            let ticket = app.doc_open_guard.start(plugin_id, request_id, gen, &rel);
            let read = Task::perform(
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
                    }))
                },
            );
            read
        }
    }
}

/// 读盘完成：守卫校验归属后建立会话并回执正文；被取代 / 跨世代的读盘结果直接丢弃并留日志。
pub fn open_done(app: &mut App, done: OpenReadResult) -> Task<Message> {
    let OpenReadResult { ticket, result, can_write } = done;
    if let Err(reason) = app.doc_open_guard.accept(&ticket, app.epoch()) {
        let line = discard_line(&ticket.path, reason);
        if !app.plugins.log_to_plugin(&ticket.plugin_id, &line) {
            eprintln!("{line}");
        }
        app.plugins
            .resolve_shell_call(ticket.request_id, Err(reason.text().into()));
        return Task::none();
    }
    let request_id = ticket.request_id;
    let path = ticket.path;
    // 读盘在途期间插件可能已对旧篇提交按键（脏态出现即拒绝），否则旧篇输入被本篇正文覆盖。
    if result.is_ok() && app.doc_dirty {
        app.plugins.resolve_shell_call(
            request_id,
            Err("当前文档有未保存修改，请先保存".into()),
        );
        return Task::none();
    }
    // 启动恢复只把文件挂回已有视图：最近打开清单与磁盘一致，不重排、不写盘。
    let restored = app.doc_open.as_ref().is_some_and(|open| {
        open.mode == crate::plugin_actions::OpenMode::Restore && open.path == path
    });
    match result {
        Ok(text) => {
            app.attach_doc_session(&path, text.clone());
            if !restored {
                crate::recent_files::record(
                    app,
                    &path,
                    atelyx_core::plugin_runtime::RecentKind::Note,
                );
            }
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
        app.clear_doc_state();
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

    /// 交错：A 读盘在途 → 用户切到 B（实例换下 + B 登记）→ A 结果晚到。
    /// A 的凭据不再指向当前登记，必须丢弃；B 的凭据仍可回填（A 不得覆盖 B 的会话）。
    #[test]
    fn stale_read_of_previous_file_is_discarded() {
        let mut guard = DocOpenGuard::default();
        let a = guard.start("notes", 11, 1, "a.md");
        guard.invalidate();
        let b = guard.start("notes", 12, 1, "b.md");

        assert_eq!(
            guard.accept(&a, 1),
            Err(DiscardReason::Superseded),
            "切到 B 之后 A 的读盘不得再建立会话"
        );
        assert!(guard.accept(&b, 1).is_ok(), "B 的读盘仍归属当前打开");
        let line = discard_line("a.md", DiscardReason::Superseded);
        assert!(line.contains("a.md") && line.contains("取代"), "日志含被丢弃的路径与原因：{line}");
    }

    /// 正常链路：登记后原样回填即建立会话，且同一凭据只能回填一次。
    #[test]
    fn matching_read_is_accepted_once() {
        let mut guard = DocOpenGuard::default();
        let ticket = guard.start("notes", 7, 3, "a.md");
        assert!(guard.accept(&ticket, 3).is_ok());
        assert_eq!(
            guard.accept(&ticket, 3),
            Err(DiscardReason::Superseded),
            "同一凭据的重复回填不得二次挂载"
        );
    }

    /// 交错：读盘在途期间切换仓库（世代前进），旧世代的结果必须丢弃。
    #[test]
    fn vault_switch_discards_inflight_read() {
        let mut guard = DocOpenGuard::default();
        let ticket = guard.start("notes", 9, 4, "a.md");
        assert_eq!(guard.accept(&ticket, 5), Err(DiscardReason::VaultSwitched));
        let line = discard_line("a.md", DiscardReason::VaultSwitched);
        assert!(line.contains("a.md") && line.contains("仓库已切换"), "日志含路径与原因：{line}");
    }

    /// 交错：启动恢复打开 A（在途）→ 用户主动打开同一篇 A（实例换下）。
    /// 恢复那笔读盘必须丢弃，只有用户这次打开可以建立会话。
    #[test]
    fn restore_read_is_discarded_when_user_reopens_same_file() {
        let mut guard = DocOpenGuard::default();
        let restore = guard.start("notes", 21, 1, "a.md");
        guard.invalidate();
        let reveal = guard.start("notes", 22, 1, "a.md");

        assert_eq!(guard.accept(&restore, 1), Err(DiscardReason::Superseded));
        assert!(guard.accept(&reveal, 1).is_ok());
    }

    /// 凭据身份不同（插件 / 请求 id）时即使路径与序号巧合也不得回填。
    #[test]
    fn identity_mismatch_is_discarded() {
        let mut guard = DocOpenGuard::default();
        let ticket = guard.start("notes", 31, 2, "a.md");
        let mut other = ticket.clone();
        other.request_id = 32;
        assert_eq!(guard.accept(&other, 2), Err(DiscardReason::Superseded));
    }

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

/// 读当前文档的属性区：无文档 / 无属性块时给出空块，正文明细不在回执里。
pub fn props_read(app: &App, request_id: u64) -> Task<Message> {
    let path = app.ui.last_note_file.clone().unwrap_or_default();
    let editable = !path.is_empty() && can_write(app);
    if path.is_empty() {
        return crate::settings_view::plugins::resolve_vault(
            app,
            request_id,
            Ok(serde_json::to_value(DocPropsResult {
                path: String::new(),
                frontmatter: String::new(),
                present: false,
                parsed: false,
                editable: false,
            })
            .unwrap_or_default()),
        );
    }
    let parsed = atelyx_core::frontmatter::parse(&app.doc_text);
    let closed = atelyx_core::frontmatter::block_end(&app.doc_text).is_some();
    let result = DocPropsResult {
        path,
        frontmatter: if closed { parsed.fm_prefix } else { String::new() },
        present: app.doc_text.starts_with("---"),
        parsed: parsed.ok && closed,
        editable,
    };
    let value = serde_json::to_value(result).unwrap_or_else(|_| json!({}));
    crate::settings_view::plugins::resolve_vault(app, request_id, Ok(value))
}

/// 用新的 frontmatter 块替换当前文档的属性区；正文与在途编辑保持不动。
pub fn props_write(
    app: &mut App,
    request_id: u64,
    params: DocPropsWriteParams,
) -> Task<Message> {
    if app.ui.last_note_file.is_none() {
        return crate::settings_view::plugins::resolve_vault(
            app,
            request_id,
            Err("没有打开的文档".to_string()),
        );
    }
    if !can_write(app) {
        return crate::settings_view::plugins::resolve_vault(
            app,
            request_id,
            Err("当前文档不可写".to_string()),
        );
    }
    let block = params.frontmatter;
    let data = if block.trim().is_empty() {
        Vec::new()
    } else {
        let parsed = atelyx_core::frontmatter::parse(&block);
        if !parsed.ok || atelyx_core::frontmatter::block_end(&block).is_none() {
            return crate::settings_view::plugins::resolve_vault(
                app,
                request_id,
                Err("属性块无法解析".to_string()),
            );
        }
        parsed.data.as_map().map(<[(String, atelyx_core::frontmatter::Yaml)]>::to_vec).unwrap_or_default()
    };
    apply_props(app, data);
    crate::settings_view::plugins::resolve_vault(app, request_id, Ok(json!({})))
}

/// 用新的属性数据重写属性区；正文与在途编辑保持不动。
pub fn apply_props(app: &mut App, data: Vec<(String, atelyx_core::frontmatter::Yaml)>) {
    use atelyx_core::frontmatter::{stringify, Yaml};
    let body = crate::strip_frontmatter(&app.doc_text).to_string();
    let previous = std::mem::replace(&mut app.doc_text, stringify(&Yaml::Map(data), &body));
    // 宿主改了正文：插件下一笔提交仍以它上次给的全文为祖先，不能把这次属性修改覆盖掉
    if app.doc_base.is_none() {
        app.doc_base = Some(previous);
    }
    app.doc_dirty = true;
    app.doc_save_error = None;
    app.autosave_deadline = Some(std::time::Instant::now() + crate::AUTOSAVE_DEBOUNCE);
    crate::space_actions::collab_after_edit(app);
    publish_state(app, Some(app.doc_text.clone()));
}
