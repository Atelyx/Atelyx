//! 文档会话缓冲：热文档唯一，旧会话按 `(docId, open_seq)` 留冷缓冲。
//! 只负责身份与保存状态，正文与视图态仍在 `NoteModel`；迟到回执按身份丢弃。

use std::collections::HashMap;

use atelyx_format::note::edit::{merge_before_write, MergeOutcome};
use atelyx_plugin_protocol::method::DocSaveResult;

/// 目录前缀跟随：路径等于 `old` 或位于 `old/` 之下时给出新路径，其余返回 `None`。
pub fn follow_rename(path: &str, old: &str, new: &str) -> Option<String> {
    if path == old {
        return Some(new.to_string());
    }
    let rest = path.strip_prefix(old)?.strip_prefix('/')?;
    Some(format!("{new}/{rest}"))
}

/// 文档会话身份：`open_seq` 每次打开推进，旧序号的回执一律作废。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DocKey {
    pub doc_id: String,
    pub open_seq: u64,
}

/// 一次保存请求的冻结正文与发起时基线。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveRequest {
    pub text: String,
    pub expected_rev: String,
}

/// 保存回执应用到状态后的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SaveOutcome {
    /// 落盘成功且之后没有新输入。
    Clean,
    /// 落盘成功，但期间有新输入：仍需下一轮保存。
    Dirty,
    /// 磁盘内容被外部改动：返回合流后的正文，调用方替换在屏正文后重试。
    Merged { text: String },
    /// 磁盘改动与本端改动无法安全合并：两侧内容都保留，调用方不得自动选边覆盖磁盘。
    Conflict,
    /// 落盘失败：正文保持脏，错误可见。
    Failed(String),
    /// 回执不属于当前在途请求（迟到 / 已切换会话）。
    Stale,
}

/// 一个文档的保存状态：基线、冻结在途请求与共同祖先。
#[derive(Clone, Debug, Default)]
pub struct DocState {
    key: Option<DocKey>,
    /// 磁盘基线：打开时为读盘哈希，每次成功保存后换新哈希。
    expected_rev: String,
    /// 上次成功保存 / 打开时的正文，作为冲突合流的共同祖先。
    saved_text: String,
    saving: Option<SaveRequest>,
}

impl DocState {
    /// 打开成功后建立状态：正文与基线都取宿主回执（宿主是唯一读盘方）。
    pub fn open(&mut self, key: DocKey, text: &str, disk_rev: String) {
        self.key = Some(key);
        self.expected_rev = disk_rev;
        self.saved_text = text.to_string();
        self.saving = None;
    }

    /// 关闭当前文档：清身份与在途请求，之后再来的回执按迟到丢弃。
    pub fn close(&mut self) {
        self.key = None;
        self.expected_rev.clear();
        self.saved_text.clear();
        self.saving = None;
    }

    pub fn key(&self) -> Option<&DocKey> {
        self.key.as_ref()
    }

    pub fn is_current(&self, doc_id: &str, open_seq: u64) -> bool {
        matches!(&self.key, Some(key) if key.doc_id == doc_id && key.open_seq == open_seq)
    }

    /// 整篇换文（历史回滚）：新正文即落盘基线，下一次冲突合流以它为共同祖先，
    /// 否则回滚掉的内容会在合流时被当作本端改动带回来。
    pub fn adopt_text(&mut self, text: &str) {
        self.saved_text = text.to_string();
    }

    /// 宿主改名：会话 id 随文件路径换新，序号、基线与在途请求都不动。
    pub fn rename_doc_id(&mut self, doc_id: &str) {
        if let Some(key) = self.key.as_mut() {
            key.doc_id = doc_id.to_string();
        }
    }

    /// 冻结一次保存：已有在途请求时返回 `None`（调用方不得并发写同一文档）。
    pub fn begin_save(&mut self, text: &str) -> Option<SaveRequest> {
        if self.key.is_none() || self.saving.is_some() {
            return None;
        }
        let request = SaveRequest {
            text: text.to_string(),
            expected_rev: self.expected_rev.clone(),
        };
        self.saving = Some(request.clone());
        Some(request)
    }

    /// 撤回在途请求：宿主调用本身失败（插件停用 / 会话关闭）时不能留下永久在途，
    /// 否则后续 flush 会误判成「正在保存」而跳过。
    pub fn abort_save(&mut self, request: &SaveRequest) {
        if self.saving.as_ref() == Some(request) {
            self.saving = None;
        }
    }

    /// 应用保存回执。`current` 是此刻在屏正文（可能已在写盘期间被继续编辑）。
    pub fn finish(&mut self, request: &SaveRequest, current: &str, result: DocSaveResult) -> SaveOutcome {
        if self.saving.as_ref() != Some(request) {
            return SaveOutcome::Stale;
        }
        self.saving = None;
        match result {
            DocSaveResult::Ok { new_rev } => {
                self.expected_rev = new_rev;
                self.saved_text = request.text.clone();
                if current == request.text {
                    SaveOutcome::Clean
                } else {
                    SaveOutcome::Dirty
                }
            }
            DocSaveResult::Conflict { disk_rev, disk_content } => {
                // 先按上次成功保存的正文为共同祖先合流，再用磁盘新基线重试
                match merge_before_write(Some(&self.saved_text), current, &disk_content) {
                    MergeOutcome::Merged(text) => {
                        self.expected_rev = disk_rev;
                        self.saved_text = disk_content;
                        SaveOutcome::Merged { text }
                    }
                    // 合并不了就不推进基线：按本端写入会覆盖磁盘新内容，只能交回冲突
                    MergeOutcome::Conflict => SaveOutcome::Conflict,
                }
            }
            DocSaveResult::Err { message } => SaveOutcome::Failed(message),
        }
    }
}

/// 冷文档快照：切走时冻结正文与保存状态；关闭 / flush 按身份找到它再提交。
#[derive(Clone, Debug, Default)]
pub struct ColdDoc {
    pub text: String,
    pub dirty: bool,
    pub state: DocState,
}

/// 热文档唯一，冷文档按身份留存；迟到身份不在热位即丢弃。
#[derive(Default)]
pub struct DocBuffers {
    hot: Option<DocKey>,
    cold: HashMap<DocKey, ColdDoc>,
}

impl DocBuffers {
    pub fn hot(&self) -> Option<&DocKey> {
        self.hot.as_ref()
    }

    /// 换仓库：全部会话身份作废，旧文档的迟到回执不得再落进新仓库。
    pub fn clear(&mut self) {
        self.hot = None;
        self.cold.clear();
    }

    pub fn is_current(&self, doc_id: &str, open_seq: u64) -> bool {
        matches!(&self.hot, Some(key) if key.doc_id == doc_id && key.open_seq == open_seq)
    }

    /// 切到新文档：旧热文档按身份移入冷缓冲，新热文档从冷缓冲摘出。
    /// 旧快照必须落冷——它已不是热文档，丢弃会让切走的正文失去落盘目标。
    pub fn activate(&mut self, key: DocKey, previous: Option<(DocKey, ColdDoc)>) {
        if let Some((old, snapshot)) = previous {
            self.cold.insert(old, snapshot);
        }
        self.hot = Some(key.clone());
        self.cold.remove(&key);
    }

    /// 关文档：移除身份，冷热都不再接受它的回执。
    pub fn forget(&mut self, key: &DocKey) {
        if self.hot.as_ref() == Some(key) {
            self.hot = None;
        }
        self.cold.remove(key);
    }

    /// 冷文档出现在作用域时取出快照（flush / 关闭按身份提交）。
    pub fn cold(&self, key: &DocKey) -> Option<&ColdDoc> {
        self.cold.get(key)
    }

    /// 冷文档提交后回写；只更新已登记的冷会话，热会话与已 forget 的身份一律丢弃
    /// （保存回写不得把已关闭的文档插回缓冲）。
    pub fn store_cold(&mut self, key: DocKey, doc: ColdDoc) {
        if self.hot.as_ref() == Some(&key) {
            return;
        }
        if let Some(slot) = self.cold.get_mut(&key) {
            *slot = doc;
        }
    }

    /// 清掉同一文档的旧序号冷缓冲（新序号已接任，旧回执没有意义）。
    pub fn prune(&mut self, doc_id: &str) {
        self.cold.retain(|key, _| key.doc_id != doc_id);
    }

    /// 宿主改名：热位与冷缓冲的会话 id 随路径换新（序号与保存状态跟着走）。
    pub fn rename_path(&mut self, old: &str, new: &str) {
        if let Some(hot) = self.hot.as_mut() {
            if let Some(next) = follow_rename(&hot.doc_id, old, new) {
                hot.doc_id = next;
            }
        }
        let moved: Vec<(DocKey, ColdDoc)> = self.cold.drain().collect();
        for (key, mut doc) in moved {
            let doc_id = follow_rename(&key.doc_id, old, new).unwrap_or(key.doc_id);
            doc.state.rename_doc_id(&doc_id);
            self.cold.insert(DocKey { doc_id, open_seq: key.open_seq }, doc);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(doc_id: &str, open_seq: u64) -> DocKey {
        DocKey { doc_id: doc_id.to_string(), open_seq }
    }

    fn opened(text: &str) -> DocState {
        let mut state = DocState::default();
        state.open(key("a.md", 1), text, "rev-a".to_string());
        state
    }

    /// 写盘在途 + 继续输入：回执只能结算它冻结的那次正文，期间新输入保持脏。
    #[test]
    fn in_flight_write_keeps_newer_text_dirty() {
        let mut state = opened("base\n");
        let request = state.begin_save("base\nfirst\n").expect("开始保存");
        // 写盘在途，用户继续输入
        let current = "base\nfirst\nsecond\n";
        let outcome = state.finish(
            &request,
            current,
            DocSaveResult::Ok { new_rev: "rev-b".to_string() },
        );
        assert_eq!(outcome, SaveOutcome::Dirty);
        assert_eq!(state.expected_rev, "rev-b");
        assert_eq!(state.saved_text, "base\nfirst\n");
        // 同一份回执再次到达（重复投递）不得再结算
        assert_eq!(
            state.finish(&request, current, DocSaveResult::Ok { new_rev: "rev-c".into() }),
            SaveOutcome::Stale
        );
    }

    /// 写盘在途 + 外部修改 + 继续输入：磁盘内容合流回插件，重试基线换成磁盘新哈希。
    #[test]
    fn in_flight_write_merges_external_change() {
        let mut state = opened("a\nb\nc\n");
        let request = state.begin_save("a\nb\nc\nd\n").expect("开始保存");
        let current = "a\nb\nc\nd\ne\n";
        let outcome = state.finish(
            &request,
            current,
            DocSaveResult::Conflict {
                disk_rev: "rev-external".to_string(),
                disk_content: "a\nB\nc\n".to_string(),
            },
        );
        let SaveOutcome::Merged { text } = outcome else {
            panic!("外部修改应合流，得到 {outcome:?}");
        };
        assert!(text.contains('B'), "外部修改保留：{text:?}");
        assert!(text.contains('d'), "本端修改保留：{text:?}");
        assert!(text.contains('e'), "写盘期间的新输入保留：{text:?}");
        assert_eq!(state.expected_rev, "rev-external");
        assert_eq!(state.saved_text, "a\nB\nc\n");
    }

    /// A 的迟到结果晚于切到 B：热位已换，A 的回执按身份丢弃。
    #[test]
    fn late_result_after_switch_is_discarded() {
        let mut buffers = DocBuffers::default();
        buffers.activate(key("a.md", 1), None);
        assert!(buffers.is_current("a.md", 1));
        // 切到 B：A 移入冷缓冲
        let a_snapshot = ColdDoc { text: "a body".to_string(), dirty: true, ..ColdDoc::default() };
        buffers.activate(key("b.md", 2), Some((key("a.md", 1), a_snapshot)));
        assert!(!buffers.is_current("a.md", 1), "A 不再是热文档");
        assert!(buffers.is_current("b.md", 2));
        // A 的迟到保存/打开回执不得应用到 B
        assert!(buffers.hot().is_some_and(|hot| hot.doc_id == "b.md"));
    }

    /// 改名换路：热位与冷缓冲的会话 id 都跟到新路径，序号与在途请求不动。
    #[test]
    fn rename_path_moves_hot_and_cold_identities() {
        let mut buffers = DocBuffers::default();
        buffers.activate(key("notes/a.md", 3), None);
        let mut state = DocState::default();
        state.open(key("notes/a.md", 3), "a", "rev-a".to_string());
        let cold = ColdDoc { text: "a".to_string(), dirty: true, state };
        buffers.activate(key("notes/b.md", 4), Some((key("notes/a.md", 3), cold)));
        buffers.rename_path("notes", "archive/notes");
        assert!(buffers.is_current("archive/notes/b.md", 4));
        let moved = buffers
            .cold(&key("archive/notes/a.md", 3))
            .expect("冷缓冲随目录换路");
        assert!(moved.state.is_current("archive/notes/a.md", 3), "冷缓冲内部身份同样换路");
    }

    /// 保存回写只更新已登记的冷会话：关闭后迟到的回写不得把正文插回缓冲。
    #[test]
    fn store_cold_does_not_resurrect_forgotten_session() {
        let mut buffers = DocBuffers::default();
        buffers.activate(key("a.md", 1), None);
        buffers.activate(key("b.md", 2), Some((key("a.md", 1), ColdDoc::default())));
        buffers.forget(&key("a.md", 1));
        let late = ColdDoc { text: "late".to_string(), dirty: true, ..ColdDoc::default() };
        buffers.store_cold(key("a.md", 1), late);
        assert!(buffers.cold(&key("a.md", 1)).is_none(), "已 forget 的会话不得被回写插回");
    }

    #[test]
    fn follow_rename_matches_exact_path_and_folder_prefix() {
        assert_eq!(follow_rename("a.md", "a.md", "b.md").as_deref(), Some("b.md"));
        assert_eq!(follow_rename("dir/a.md", "dir", "x").as_deref(), Some("x/a.md"));
        assert_eq!(follow_rename("dir2/a.md", "dir", "x"), None);
    }

    /// 保存失败不得静默：错误留在状态里，正文继续保持脏。
    #[test]
    fn failed_write_surfaces_error() {
        let mut state = opened("base");
        let request = state.begin_save("base changed").expect("开始保存");
        let outcome = state.finish(
            &request,
            "base changed",
            DocSaveResult::Err { message: "磁盘不可写".to_string() },
        );
        assert_eq!(outcome, SaveOutcome::Failed("磁盘不可写".to_string()));
        assert!(state.begin_save("base changed").is_some(), "失败后仍可重试");
    }
}
