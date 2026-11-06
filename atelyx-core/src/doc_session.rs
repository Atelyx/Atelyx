//! 通用文档会话表：谁打开了哪份文件、归属哪个文档类型与插件、脏 / 错误 / 标题与热态。
//! 正文缓冲、合并与落盘都在内容所有者插件，本表只记状态；不变量：会话存在 ⇔ `docs` 有条目，
//! 每（插件, 文档类型）至多一个热文档，每次打开推进 `open_seq`，旧序号的回执不得覆盖新状态。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::doc_type::DocTypeId;
use crate::write_queue::WriteQueue;

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

/// 正文基线的完整 SHA-256（64 位小写十六进制）：基线只认实际字节，
/// 用时间戳 / 修改标记推断会把「内容没变」误判成冲突或反过来漏掉冲突。
pub fn content_rev(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

/// 带基线的落盘结果：`Written` 回执新基线，`Conflict` 交回磁盘当前内容（不写盘）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocWrite {
    Written { new_rev: String },
    Conflict { disk_rev: String, disk_content: String },
}

/// 在写盘队列的同一执行序位内完成「读盘 → 比基线 → 原子写」：先读后写的间隙里
/// 外部编辑器的写入会被静默覆盖，所以比较与写入不能分两次排队。
/// 文件被外部删除按空基线比对（缺失即内容为空），由冲突分支把事实交回调用方。
pub async fn write_at_baseline(
    queue: &WriteQueue,
    path: std::path::PathBuf,
    content: String,
    expected_rev: &str,
) -> Result<DocWrite, String> {
    let expected_rev = expected_rev.to_string();
    queue
        .bulk(move || {
            let disk_text = match std::fs::read_to_string(&path) {
                Ok(text) => text,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(error) => return Err(format!("写前读盘失败：{error}")),
            };
            let disk_rev = content_rev(disk_text.as_bytes());
            if disk_rev != expected_rev {
                return Ok(DocWrite::Conflict { disk_rev, disk_content: disk_text });
            }
            crate::vault::atomic_write(&path, &content)?;
            Ok(DocWrite::Written { new_rev: content_rev(content.as_bytes()) })
        })
        .await
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
    /// 建立 / 复用本次会话时的打开序号；旧序号的回执会被拒绝。
    pub open_seq: u64,
    /// 状态版本：每次打开 / 插件上报都推进。宿主发起的落盘回执按它判定是否已被更新的上报取代。
    pub state_rev: u64,
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
/// `title` 为 `None` 表示不变。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocStateUpdate {
    pub dirty: bool,
    pub save_error: Option<String>,
    pub title: Option<String>,
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
    /// 建立或复用会话：同一路径复用原条目并推进打开序号，脏与错误原样保留。
    /// 调用方负责在换文档前 flush：保留未落盘状态，重开时的脏拦截才拦得住。本方法不读盘、不写盘。
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
                session.state_rev = session.state_rev.saturating_add(1);
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
                        open_seq,
                        state_rev: 0,
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

    /// 某文档类型的当前热文档；同一类型至多一个热会话，故不必先知道归属插件。
    pub fn hot_of_type(&self, doc_type: &DocTypeId) -> Option<&DocSession> {
        let (_, id) = self.hot.iter().find(|((_, kind), _)| kind == doc_type)?;
        self.docs.get(id)
    }

    /// 该文档类型的全部会话：换文件前按它清掉同类型的旧会话，不区分冷热。
    pub fn of_type(&self, doc_type: &DocTypeId) -> impl Iterator<Item = &DocSession> + '_ {
        let doc_type = doc_type.clone();
        self.docs
            .values()
            .filter(move |session| session.doc_type == doc_type)
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
        session.state_rev = session.state_rev.saturating_add(1);
        true
    }

    /// 会话当前状态版本：宿主在发起落盘 / 读盘时记下，回执结算时用 [`Self::update_state_if_rev`]。
    pub fn state_rev(&self, id: &DocId) -> Option<u64> {
        self.docs.get(id).map(|session| session.state_rev)
    }

    /// 按状态版本结算宿主动作：期间有更新的插件上报（`state_rev` 已推进）即拒绝，
    /// 迟到的旧结论不得覆盖新状态。
    pub fn update_state_if_rev(
        &mut self,
        id: &DocId,
        open_seq: u64,
        expected_rev: u64,
        state: DocStateUpdate,
    ) -> bool {
        let Some(session) = self.docs.get(id) else { return false };
        if session.open_seq != open_seq || session.state_rev != expected_rev {
            return false;
        }
        self.update_state(id, open_seq, state)
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

    /// 有未落盘修改的会话（按路径序；调用方据 `plugin_id` 找到内容所有者请它落盘）。
    pub fn dirty(&self) -> impl Iterator<Item = &DocSession> {
        self.docs.values().filter(|session| session.dirty)
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
        DocStateUpdate { dirty, save_error: error.map(Into::into), title: None }
    }

    /// 基线写把「读—比—写」放在同一执行序位：基线不符不写盘，磁盘内容原样交回。
    #[tokio::test]
    async fn baseline_write_conflicts_without_touching_disk() {
        let tmp = crate::vault::test_support::TempDir::new("doc-baseline");
        let path = tmp.join("a.md");
        std::fs::write(&path, "disk").unwrap();
        let queue = WriteQueue::new();
        let rev = content_rev(b"disk");
        let written = write_at_baseline(&queue, path.clone(), "next".into(), &rev)
            .await
            .expect("写盘不应失败");
        assert_eq!(written, DocWrite::Written { new_rev: content_rev(b"next") });
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "next");
        // 外部改动后再用旧基线提交：不写盘，交回磁盘现状
        std::fs::write(&path, "external").unwrap();
        let stale = write_at_baseline(&queue, path.clone(), "ours".into(), &rev)
            .await
            .expect("冲突不是调用失败");
        assert_eq!(
            stale,
            DocWrite::Conflict {
                disk_rev: content_rev(b"external"),
                disk_content: "external".into()
            }
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "external", "冲突不得覆盖磁盘");
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
    }

    #[test]
    fn reuse_keeps_unsaved_state() {
        let mut sessions = DocSessions::default();
        let id = sessions.open_or_reuse(spec("a.md", "note"));
        assert!(sessions.update_state(
            &id,
            sessions.session(&id).unwrap().open_seq,
            state(true, Some("磁盘被外部修改")),
        ));
        // 重开同路径不得抹掉未落盘状态：脏拦截读的就是这份
        sessions.open_or_reuse(spec("a.md", "note"));
        let session = sessions.session(&id).unwrap();
        assert!(session.dirty);
        assert_eq!(session.save_error.as_deref(), Some("磁盘被外部修改"));
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
    fn of_type_lists_every_session_of_the_type() {
        let mut sessions = DocSessions::default();
        let a = sessions.open_or_reuse(spec("a.md", "note"));
        let b = sessions.open_or_reuse(spec("b.md", "note"));
        sessions.open_or_reuse(spec("c.atb", "table"));
        assert!(sessions.set_hot(&a));
        // 冷会话与热会话同等计入：换文件时要清掉同类型的全部旧会话
        let mut paths: Vec<&str> = sessions
            .of_type(&DocTypeId::new("note"))
            .map(|session| session.path.as_str())
            .collect();
        paths.sort_unstable();
        assert_eq!(paths, vec!["a.md", "b.md"]);
        assert_eq!(sessions.of_type(&DocTypeId::new("note")).count(), 2);
        assert!(sessions.of_type(&DocTypeId::new("board")).next().is_none());
        assert!(!sessions.session(&b).unwrap().dirty);
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

    /// 宿主动作的落盘回执按状态版本结算：期间插件又上报过（版本推进）即不得回写旧结论。
    #[test]
    fn guarded_settlement_respects_newer_plugin_state() {
        let mut sessions = DocSessions::default();
        let id = sessions.open_or_reuse(spec("a.md", "note"));
        let seq = sessions.session(&id).unwrap().open_seq;
        let rev = sessions.state_rev(&id).unwrap();
        // 发起落盘后插件又上报了脏：宿主回执不得把这份新状态盖成干净
        assert!(sessions.update_state(&id, seq, state(true, None)));
        assert!(!sessions.update_state_if_rev(&id, seq, rev, state(false, None)));
        assert!(sessions.session(&id).unwrap().dirty);
        // 没有更新的上报时按回执结算
        let rev = sessions.state_rev(&id).unwrap();
        assert!(sessions.update_state_if_rev(&id, seq, rev, state(false, None)));
        assert!(!sessions.session(&id).unwrap().dirty);
        // 打开新序号同样让旧版本失效
        let stale = sessions.state_rev(&id).unwrap();
        sessions.open_or_reuse(spec("a.md", "note"));
        let seq = sessions.session(&id).unwrap().open_seq;
        assert!(!sessions.update_state_if_rev(&id, seq, stale, state(true, None)));
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
