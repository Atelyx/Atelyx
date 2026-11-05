//! 通用文档会话表：谁打开了哪份文件、归属哪个文档类型与插件、脏 / 错误 / 标题与热态。
//! 与布局无关、零 I/O：正文真源、保存链与 flush 都在调用方；宿主不推断 `dirty`。
//! 不变量：会话存在 ⇔ `docs` 有条目；每（插件, 文档类型）至多一个热文档；
//! 每次打开推进 `open_seq`，带旧序号的回执不得覆盖新状态。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::doc_type::DocTypeId;

/// 文档身份 = 仓库相对路径（磁盘上按字符串写出）。
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DocId(String);

impl DocId {
    pub fn new(path: impl Into<String>) -> Self {
        Self(path.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for DocId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocSession {
    pub id: DocId,
    pub path: String,
    pub doc_type: DocTypeId,
    /// 内容真源插件（与承载视图无关）。
    pub plugin_id: String,
    /// 当前绑定的视图贡献 id。
    pub surface: String,
    pub title: String,
    pub editable: bool,
    pub dirty: bool,
    pub save_error: Option<String>,
    /// 最近一次确认落盘的版本号；由调用方在保存完成时上报。
    pub disk_rev: u64,
    /// 建立 / 复用本次会话时的打开序号；旧序号的回执会被拒绝。
    pub open_seq: u64,
}

/// 建立或复用会话所需的声明信息。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocSpec {
    pub path: String,
    pub doc_type: DocTypeId,
    pub plugin_id: String,
    pub surface: String,
    pub title: String,
    pub editable: bool,
}

/// 插件上报的会话状态：`dirty` 与 `save_error` 每次按上报值覆盖（`None` = 无错误）；
/// `title` / `disk_rev` 为 `None` 表示不变。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocStateUpdate {
    pub dirty: bool,
    pub save_error: Option<String>,
    pub title: Option<String>,
    pub disk_rev: Option<u64>,
}

/// 一次读盘回填的归属凭据：身份（插件 id + 请求 id）、序号（打开次序）、世代与路径。
/// 守卫只认与当前登记完全一致的凭据。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenTicket {
    plugin_id: String,
    request_id: u64,
    seq: u64,
    gen: u64,
    path: String,
}

impl OpenTicket {
    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    pub fn request_id(&self) -> u64 {
        self.request_id
    }

    pub fn path(&self) -> &str {
        &self.path
    }
}

/// 读盘结果被守卫丢弃的原因（决定回执文案与日志说明）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiscardReason {
    /// 打开动作已被新的打开 / 关闭取代（凭据不再指向当前登记）。
    Superseded,
    /// 读盘跨过了仓库切换。
    VaultSwitched,
}

impl DiscardReason {
    pub fn text(self) -> &'static str {
        match self {
            Self::Superseded => "文档打开已被新的请求取代",
            Self::VaultSwitched => "仓库已切换，请重新打开",
        }
    }
}

/// 在途读盘的归属守卫：只有最后一次登记、且身份 / 序号 / 世代都对得上的结果可建立会话。
/// 打开新文件、关闭实例、插件重发打开请求都推进序号，旧结果据此丢弃。
#[derive(Debug, Default)]
pub struct DocOpenGuard {
    seq: u64,
    current: Option<OpenTicket>,
}

impl DocOpenGuard {
    /// 登记一次新的读盘并返回凭据；更早登记的读盘结果全部作废。
    pub fn start(&mut self, plugin_id: &str, request_id: u64, gen: u64, path: &str) -> OpenTicket {
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
    pub fn invalidate(&mut self) {
        self.seq = self.seq.saturating_add(1);
        self.current = None;
    }

    /// 校验回填归属：接受即消耗本次打开（一次打开只建立一次会话）。
    pub fn accept(&mut self, ticket: &OpenTicket, epoch: u64) -> Result<(), DiscardReason> {
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
pub fn discard_line(path: &str, reason: DiscardReason) -> String {
    format!("文档读盘结果被丢弃：{path}（{}）", reason.text())
}

/// 全部打开中的文档；热态另存，不在 `DocSession` 里（布局可随时换视图而不改变会话）。
#[derive(Debug, Default)]
pub struct DocSessions {
    docs: BTreeMap<DocId, DocSession>,
    seq: u64,
    guard: DocOpenGuard,
    hot: BTreeMap<(String, DocTypeId), DocId>,
}

impl DocSessions {
    /// 建立或复用会话：同一路径复用原条目（保留脏 / 错误状态），并推进打开序号。
    /// 调用方负责在换文档前 flush；本方法不读盘、不写盘。
    pub fn open_or_reuse(&mut self, spec: DocSpec) -> DocId {
        self.seq = self.seq.saturating_add(1);
        let open_seq = self.seq;
        let id = DocId::new(&spec.path);
        match self.docs.get_mut(&id) {
            Some(session) => {
                session.doc_type = spec.doc_type;
                session.plugin_id = spec.plugin_id;
                session.surface = spec.surface;
                session.title = spec.title;
                session.editable = spec.editable;
                session.open_seq = open_seq;
            }
            None => {
                self.docs.insert(
                    id.clone(),
                    DocSession {
                        id: id.clone(),
                        path: spec.path,
                        doc_type: spec.doc_type,
                        plugin_id: spec.plugin_id,
                        surface: spec.surface,
                        title: spec.title,
                        editable: spec.editable,
                        dirty: false,
                        save_error: None,
                        disk_rev: 0,
                        open_seq,
                    },
                );
            }
        }
        id
    }

    /// 登记一次在途打开；更早的在途结果作废。
    pub fn begin_open(&mut self, plugin_id: &str, request_id: u64, gen: u64, path: &str) -> OpenTicket {
        self.guard.start(plugin_id, request_id, gen, path)
    }

    /// 回执：先校验凭据，再建立 / 复用会话。乱序回执返回错误且表不变。
    pub fn finish_open(&mut self, ticket: &OpenTicket, gen: u64, spec: DocSpec) -> Result<DocId, DiscardReason> {
        self.guard.accept(ticket, gen)?;
        Ok(self.open_or_reuse(spec))
    }

    /// 标记热文档：同一（插件, 文档类型）只保留这一个。
    pub fn set_hot(&mut self, id: &DocId) -> bool {
        let Some(session) = self.docs.get(id) else { return false };
        self.hot.insert((session.plugin_id.clone(), session.doc_type.clone()), id.clone());
        true
    }

    /// 取消热态；id 不是当前热文档时不做改动。
    pub fn set_cold(&mut self, id: &DocId) -> bool {
        let Some(session) = self.docs.get(id) else { return false };
        let key = (session.plugin_id.clone(), session.doc_type.clone());
        if self.hot.get(&key) == Some(id) {
            self.hot.remove(&key);
            return true;
        }
        false
    }

    pub fn hot_of(&self, plugin_id: &str, doc_type: &DocTypeId) -> Option<&DocSession> {
        let id = self.hot.get(&(plugin_id.to_string(), doc_type.clone()))?;
        self.docs.get(id)
    }

    /// 应用插件上报状态；`open_seq` 与条目的当前序号不符时拒绝（乱序回执不得覆盖新状态）。
    pub fn update_state(&mut self, id: &DocId, open_seq: u64, state: DocStateUpdate) -> bool {
        let Some(session) = self.docs.get_mut(id) else { return false };
        if session.open_seq != open_seq {
            return false;
        }
        session.dirty = state.dirty;
        session.save_error = state.save_error;
        if let Some(title) = state.title {
            session.title = title;
        }
        if let Some(disk_rev) = state.disk_rev {
            session.disk_rev = disk_rev;
        }
        true
    }

    /// 关闭会话。调用方必须先 flush；本方法不写盘、不推断脏状态。
    pub fn close(&mut self, id: &DocId) -> Option<DocSession> {
        let session = self.docs.remove(id)?;
        let key = (session.plugin_id.clone(), session.doc_type.clone());
        if self.hot.get(&key) == Some(id) {
            self.hot.remove(&key);
        }
        Some(session)
    }

    /// 路径改名：条目与热态一起换到新路径，脏 / 错误状态原样保留。
    /// 目标路径已有条目时以被改名的会话为准（改名覆盖的旧条目丢弃）。
    pub fn rename_path(&mut self, from: &str, to: &str) -> bool {
        let old = DocId::new(from);
        let new = DocId::new(to);
        if let Some(replaced) = self.docs.remove(&new) {
            let key = (replaced.plugin_id, replaced.doc_type);
            if self.hot.get(&key) == Some(&new) {
                self.hot.remove(&key);
            }
        }
        let Some(mut session) = self.docs.remove(&old) else { return false };
        for id in self.hot.values_mut() {
            if id == &old {
                *id = new.clone();
            }
        }
        session.id = new.clone();
        session.path = to.to_string();
        self.docs.insert(new, session);
        true
    }

    /// 清空全部会话（切仓库 / 切空间：旧路径全部失效）。序号继续推进，不复用旧号。
    pub fn clear(&mut self) {
        self.docs.clear();
        self.hot.clear();
        self.seq = self.seq.saturating_add(1);
    }

    pub fn session(&self, id: &DocId) -> Option<&DocSession> {
        self.docs.get(id)
    }

    pub fn by_path(&self, path: &str) -> Option<&DocSession> {
        self.docs.get(&DocId::new(path))
    }

    pub fn by_type<'a>(&'a self, plugin_id: &'a str, doc_type: &'a DocTypeId) -> impl Iterator<Item = &'a DocSession> + 'a {
        self.docs
            .values()
            .filter(move |session| session.plugin_id == plugin_id && &session.doc_type == doc_type)
    }

    pub fn all_open(&self) -> impl Iterator<Item = &DocSession> {
        self.docs.values()
    }

    pub fn len(&self) -> usize {
        self.docs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(path: &str, doc_type: &str) -> DocSpec {
        DocSpec {
            path: path.into(),
            doc_type: DocTypeId::new(doc_type),
            plugin_id: "atelyx.notes".into(),
            surface: "note".into(),
            title: path.rsplit('/').next().unwrap_or(path).into(),
            editable: true,
        }
    }

    fn state(dirty: bool, error: Option<&str>) -> DocStateUpdate {
        DocStateUpdate { dirty, save_error: error.map(Into::into), title: None, disk_rev: None }
    }

    #[test]
    fn opens_and_reuses_by_path() {
        let mut sessions = DocSessions::default();
        let id = sessions.open_or_reuse(spec("a.md", "note"));
        let seq = sessions.session(&id).unwrap().open_seq;
        assert_eq!(sessions.len(), 1);

        let reused = sessions.open_or_reuse(spec("a.md", "note"));
        assert_eq!(reused, id, "同一路径复用同一会话");
        assert_eq!(sessions.len(), 1);
        assert!(sessions.session(&id).unwrap().open_seq > seq, "复用推进打开序号");
        assert_eq!(sessions.by_path("a.md").unwrap().id, id);
        assert_eq!(sessions.by_type("atelyx.notes", &DocTypeId::new("note")).count(), 1);
    }

    #[test]
    fn hot_switch_on_missing_entry_leaves_state_unchanged() {
        let mut sessions = DocSessions::default();
        let a = sessions.open_or_reuse(spec("a.md", "note"));
        assert!(sessions.set_hot(&a));
        let ghost = DocId::new("gone.md");
        // 调用方 flush 失败即中止切换：表侧热态不动
        assert!(!sessions.set_hot(&ghost));
        assert!(!sessions.set_cold(&ghost));
        assert_eq!(
            sessions.hot_of("atelyx.notes", &DocTypeId::new("note")).unwrap().id,
            a
        );
    }

    #[test]
    fn hot_is_unique_per_type() {
        let mut sessions = DocSessions::default();
        let a = sessions.open_or_reuse(spec("a.md", "note"));
        let b = sessions.open_or_reuse(spec("b.md", "note"));
        assert!(sessions.set_hot(&a));
        assert!(sessions.set_hot(&b));
        assert_eq!(sessions.hot_of("atelyx.notes", &DocTypeId::new("note")).unwrap().id, b);
        assert!(sessions.set_cold(&b));
        assert!(sessions.hot_of("atelyx.notes", &DocTypeId::new("note")).is_none());

        let board = DocSpec { doc_type: DocTypeId::new("board"), surface: "board".into(), ..spec("a.board", "board") };
        let board = sessions.open_or_reuse(board);
        sessions.set_hot(&board);
        assert!(sessions.hot_of("atelyx.notes", &DocTypeId::new("board")).is_some(), "不同类型可各自有热文档");
    }

    #[test]
    fn close_drops_entry_and_hot_state() {
        let mut sessions = DocSessions::default();
        let id = sessions.open_or_reuse(spec("a.md", "note"));
        sessions.set_hot(&id);
        assert!(sessions.close(&id).is_some());
        assert!(sessions.session(&id).is_none());
        assert!(sessions.hot_of("atelyx.notes", &DocTypeId::new("note")).is_none());
        assert!(sessions.close(&id).is_none());
    }

    #[test]
    fn rename_moves_entry_and_keeps_dirty_state() {
        let mut sessions = DocSessions::default();
        let id = sessions.open_or_reuse(spec("a.md", "note"));
        let seq = sessions.session(&id).unwrap().open_seq;
        assert!(sessions.update_state(&id, seq, state(true, Some("保存失败"))));
        sessions.set_hot(&id);
        assert!(sessions.rename_path("a.md", "dir/b.md"));
        assert!(sessions.by_path("a.md").is_none());
        let moved = sessions.by_path("dir/b.md").expect("改名后按新路径可达");
        assert_eq!(moved.id, DocId::new("dir/b.md"));
        assert!(moved.dirty);
        assert_eq!(moved.save_error.as_deref(), Some("保存失败"));
        assert_eq!(sessions.hot_of("atelyx.notes", &DocTypeId::new("note")).unwrap().path, "dir/b.md");
        assert!(!sessions.rename_path("gone.md", "x.md"));
    }

    #[test]
    fn cold_session_keeps_unsaved_state() {
        let mut sessions = DocSessions::default();
        let a = sessions.open_or_reuse(spec("a.md", "note"));
        let seq = sessions.session(&a).unwrap().open_seq;
        sessions.set_hot(&a);
        assert!(sessions.update_state(&a, seq, state(true, None)));
        let b = sessions.open_or_reuse(spec("b.md", "note"));
        sessions.set_hot(&b);
        // 换热文档后旧会话转冷但脏状态必须留在表里（flush 由调用方在换之前完成）
        assert_eq!(sessions.hot_of("atelyx.notes", &DocTypeId::new("note")).unwrap().id, b);
        assert!(sessions.session(&a).unwrap().dirty);
    }

    #[test]
    fn clear_drops_all_sessions_and_hot_state() {
        let mut sessions = DocSessions::default();
        let a = sessions.open_or_reuse(spec("a.md", "note"));
        sessions.set_hot(&a);
        sessions.clear();
        assert!(sessions.is_empty());
        assert!(sessions.hot_of("atelyx.notes", &DocTypeId::new("note")).is_none());
    }

    #[test]
    fn out_of_order_receipts_are_discarded() {
        let mut sessions = DocSessions::default();
        let first = sessions.begin_open("atelyx.notes", 1, 1, "a.md");
        let second = sessions.begin_open("atelyx.notes", 2, 1, "b.md");
        assert_eq!(
            sessions.finish_open(&first, 1, spec("a.md", "note")),
            Err(DiscardReason::Superseded),
            "旧凭据不得建立会话"
        );
        assert!(sessions.is_empty(), "被丢弃的回执不得改表");
        let id = sessions.finish_open(&second, 1, spec("b.md", "note")).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions.session(&id).unwrap().path, "b.md");
    }

    #[test]
    fn stale_state_updates_are_rejected() {
        let mut sessions = DocSessions::default();
        let id = sessions.open_or_reuse(spec("a.md", "note"));
        let stale_seq = sessions.session(&id).unwrap().open_seq;
        sessions.open_or_reuse(spec("a.md", "note"));
        assert!(!sessions.update_state(&id, stale_seq, state(true, Some("保存失败"))));
        assert!(!sessions.session(&id).unwrap().dirty);
        let current_seq = sessions.session(&id).unwrap().open_seq;
        assert!(sessions.update_state(&id, current_seq, state(true, Some("保存失败"))));
        assert!(sessions.session(&id).unwrap().dirty);
    }

    #[test]
    fn guard_discards_inflight_read_of_previous_file() {
        let mut guard = DocOpenGuard::default();
        let a = guard.start("notes", 11, 1, "a.md");
        guard.invalidate();
        let b = guard.start("notes", 12, 1, "b.md");
        assert_eq!(guard.accept(&a, 1), Err(DiscardReason::Superseded));
        assert!(guard.accept(&b, 1).is_ok(), "B 的读盘仍归属当前打开");
        let line = discard_line("a.md", DiscardReason::Superseded);
        assert!(line.contains("a.md") && line.contains("取代"), "日志含路径与原因：{line}");
    }

    #[test]
    fn guard_rejects_other_ticket_and_stale_epoch() {
        let mut guard = DocOpenGuard::default();
        let ticket = guard.start("atelyx.notes", 7, 1, "a.md");
        let mut other = ticket.clone();
        other.request_id = 8;
        assert_eq!(guard.accept(&other, 1), Err(DiscardReason::Superseded));
        assert!(guard.accept(&ticket, 1).is_ok(), "同一凭据只能接受一次");
        assert_eq!(guard.accept(&ticket, 1), Err(DiscardReason::Superseded));

        let ticket = guard.start("atelyx.notes", 9, 3, "a.md");
        assert_eq!(guard.accept(&ticket, 5), Err(DiscardReason::VaultSwitched));
        assert_eq!(
            discard_line("a.md", DiscardReason::VaultSwitched),
            "文档读盘结果被丢弃：a.md（仓库已切换，请重新打开）"
        );
    }
}
