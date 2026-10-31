//! 面板状态：会话清单、当前会话消息、输入草稿 / 引用 / 附件与选择器。
//! 真源全在宿主（`chat.session.*` / `ai.chat.*`），这里只持展示所需的副本与交互态。

use std::collections::HashSet;
use std::sync::Arc;

use serde_json::Value;

use atelyx_plugin_protocol::chat::SessionRecord;
use atelyx_plugin_protocol::method::{AiAgentsResult, AiModelsResult, ChatHistoryResult};

/// 面板贡献 id（同时是整面帧的 surfaceId）。
pub const PANEL: &str = "aichat";
/// 消息流滚动区 id。
pub const FLOW: &str = "flow";
/// 输入框 id。
pub const DRAFT: &str = "draft";
/// # 候选浮层最多列出的条数。
const PICKER_MAX: usize = 8;
/// 会话标题缺省文案。
pub const UNTITLED: &str = "未命名对话";

#[derive(Clone, Debug)]
pub struct SessionRow {
    pub id: String,
    pub title: Option<String>,
    pub updated_at: i64,
    pub count: u64,
}

impl SessionRow {
    pub fn title(&self) -> &str {
        self.title.as_deref().filter(|t| !t.is_empty()).unwrap_or(UNTITLED)
    }
}

#[derive(Clone, Debug)]
pub struct AgentRow {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct ModelRow {
    pub provider_id: String,
    pub provider_name: String,
    pub model: String,
    pub label: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefKind {
    Note,
    Table,
    Canvas,
    Dir,
}

impl RefKind {
    pub fn key(self) -> &'static str {
        match self {
            RefKind::Note => "note",
            RefKind::Table => "table",
            RefKind::Canvas => "canvas",
            RefKind::Dir => "dir",
        }
    }

    pub fn prefix(self) -> &'static str {
        match self {
            RefKind::Note => "笔记",
            RefKind::Table => "表格",
            RefKind::Canvas => "画布",
            RefKind::Dir => "文件夹",
        }
    }
}

#[derive(Clone, Debug)]
pub struct RefEntry {
    pub path: String,
    pub label: String,
    pub kind: RefKind,
}

#[derive(Clone, Debug)]
pub struct FileEntry {
    pub path: String,
    pub is_dir: bool,
}

#[derive(Clone, Debug)]
pub struct PendingImage {
    pub name: String,
    pub source: String,
    pub staged: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Candidate {
    pub path: String,
    pub is_dir: bool,
    pub name: String,
    pub sub: String,
}

#[derive(Clone, Debug)]
pub struct Picker {
    /// `#` 在草稿里的字节位置。
    pub hash: usize,
    /// 触发候选时的光标位置（查询串的右端）。
    pub caret: usize,
    pub candidates: Vec<Candidate>,
    /// 键盘巡航项。
    pub selected: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Menu {
    Agent,
    /// 模型菜单根：模型 / 推理等级两行。
    ModelRoot,
    /// 模型清单。
    ModelList,
    Effort,
    Plus,
}

/// 面板消息流状态。
#[derive(Clone)]
pub struct Model {
    pub ready: bool,
    pub error: Option<String>,
    pub persist_error: Option<String>,
    pub sessions: Vec<SessionRow>,
    pub active: Option<String>,
    pub active_title: Option<String>,
    pub active_agent_id: Option<String>,
    /// 共享只读快照：渲染时克隆 Model 不复制整段对话。
    pub messages: Arc<Vec<Value>>,
    pub compaction: Option<Value>,
    pub streaming: HashSet<String>,
    pub compacting: Option<String>,
    pub renaming: HashSet<String>,
    pub sidebar_open: bool,
    pub search: String,
    pub delete_confirm: Option<String>,
    /// 已展开删除操作的会话行（悬停时点「更多」打开）。
    pub session_menu: Option<String>,
    pub expanded: HashSet<String>,
    /// 消息流是否停在底部（上翻后出现「回到底部」）。
    pub follow_bottom: bool,
    pub rollback_confirm: Option<String>,
    pub draft: String,
    pub refs: Vec<RefEntry>,
    pub files: Vec<FileEntry>,
    pub files_ready: bool,
    pub images: Vec<PendingImage>,
    pub picker: Option<Picker>,
    pub agents: Vec<AgentRow>,
    pub models: Vec<ModelRow>,
    /// 面板级覆盖（与宿主会话真源同源持久化）。
    pub agent_override: Option<String>,
    pub model_pick: Option<(String, String)>,
    pub effort: Option<String>,
    pub menu: Option<Menu>,
}

impl Default for Model {
    fn default() -> Self {
        Self::new()
    }
}

impl Model {
    pub fn new() -> Self {
        Self {
            ready: false,
            error: None,
            persist_error: None,
            sessions: Vec::new(),
            active: None,
            active_title: None,
            active_agent_id: None,
            messages: Arc::new(Vec::new()),
            compaction: None,
            streaming: HashSet::new(),
            compacting: None,
            renaming: HashSet::new(),
            sidebar_open: false,
            search: String::new(),
            delete_confirm: None,
            session_menu: None,
            expanded: HashSet::new(),
            follow_bottom: true,
            rollback_confirm: None,
            draft: String::new(),
            refs: Vec::new(),
            files: Vec::new(),
            files_ready: false,
            images: Vec::new(),
            picker: None,
            agents: Vec::new(),
            models: Vec::new(),
            agent_override: None,
            model_pick: None,
            effort: None,
            menu: None,
        }
    }

    pub fn clear_messages(&mut self) {
        Arc::make_mut(&mut self.messages).clear();
    }

    /// 容器切换：旧会话投影、草稿与选择器全部作废（面板覆盖随新容器重新装载）。
    pub fn reset(&mut self) {
        self.ready = false;
        self.error = None;
        self.persist_error = None;
        self.sessions.clear();
        self.active = None;
        self.active_title = None;
        self.active_agent_id = None;
        self.messages = Arc::new(Vec::new());
        self.compaction = None;
        self.streaming.clear();
        self.compacting = None;
        self.renaming.clear();
        self.delete_confirm = None;
        self.session_menu = None;
        self.expanded.clear();
        self.follow_bottom = true;
        self.rollback_confirm = None;
        self.picker = None;
        self.draft.clear();
        self.refs.clear();
        self.images.clear();
        self.files.clear();
        self.files_ready = false;
        self.agent_override = None;
        self.model_pick = None;
        self.effort = None;
        self.menu = None;
    }

    pub fn apply_list(&mut self, list: ChatHistoryResult) {
        self.sessions = list
            .sessions
            .into_iter()
            .map(|session| SessionRow {
                title: session.title,
                updated_at: session.updated_at,
                count: session.message_count,
                id: session.id,
            })
            .collect();
        self.sort_sessions();
        self.ready = true;
    }

    pub fn apply_agents(&mut self, agents: AiAgentsResult) {
        self.agents = agents
            .agents
            .into_iter()
            .map(|agent| AgentRow { id: agent.id, name: agent.name })
            .collect();
        if self.agent_override.is_none() {
            self.agent_override = agents.current;
        }
    }

    pub fn apply_models(&mut self, models: AiModelsResult) {
        self.models = models
            .providers
            .into_iter()
            .flat_map(|provider| {
                let provider_id = provider.id.clone();
                let provider_name = provider.name.clone();
                provider.models.into_iter().map(move |model| ModelRow {
                    provider_id: provider_id.clone(),
                    provider_name: provider_name.clone(),
                    label: model.nickname.clone().unwrap_or_else(|| model.id.clone()),
                    model: model.id,
                })
            })
            .collect();
        if self.model_pick.is_none() {
            self.model_pick = models
                .current
                .map(|pick| (pick.provider_id, pick.model));
        }
    }

    pub fn apply_overrides(
        &mut self,
        model: Option<Value>,
        effort: Option<String>,
        agent: Option<String>,
    ) {
        if let Some(value) = model {
            self.model_pick = Some((
                value.get("providerId").and_then(Value::as_str).unwrap_or_default().to_string(),
                value.get("model").and_then(Value::as_str).unwrap_or_default().to_string(),
            ));
        }
        self.effort = effort;
        self.agent_override = agent;
    }

    pub fn set_persist_error(&mut self, message: Option<String>) {
        self.persist_error = message;
    }

    pub fn sort_sessions(&mut self) {
        self.sessions.sort_by_key(|session| -session.updated_at);
    }

    pub fn visible_sessions(&self) -> Vec<&SessionRow> {
        let query = self.search.trim().to_lowercase();
        self.sessions
            .iter()
            .filter(|session| query.is_empty() || session.title().to_lowercase().contains(&query))
            .collect()
    }

    pub fn session(&self, id: &str) -> Option<&SessionRow> {
        self.sessions.iter().find(|session| session.id == id)
    }

    pub fn apply_session(&mut self, record: SessionRecord) {
        if self.active.as_deref() != Some(record.id.as_str()) {
            return;
        }
        self.active_title = record.title.clone();
        self.active_agent_id = record.agent_id.clone();
        self.compaction = record.compaction;
        self.messages = Arc::new(record.messages);
        if let Some(row) = self.sessions.iter_mut().find(|row| row.id == record.id) {
            row.title = record.title;
            row.count = self.messages.len() as u64;
            row.updated_at = record.updated_at;
        }
    }

    pub fn upsert_message(&mut self, session_id: &str, message_id: &str, message: Value) {
        if self.active.as_deref() == Some(session_id) {
            let messages = Arc::make_mut(&mut self.messages);
            if let Some(slot) = messages.iter_mut().find(|m| message_id_of(m) == message_id) {
                *slot = message;
            } else {
                messages.push(message);
            }
        }
    }

    pub fn remove_message(&mut self, session_id: &str, message_id: &str) {
        if self.active.as_deref() == Some(session_id) {
            Arc::make_mut(&mut self.messages).retain(|m| message_id_of(m) != message_id);
        }
    }

    pub fn set_title(&mut self, session_id: &str, title: Option<String>) {
        if let Some(row) = self.sessions.iter_mut().find(|row| row.id == session_id) {
            row.title = title.clone();
        }
        if self.active.as_deref() == Some(session_id) {
            self.active_title = title;
        }
    }

    pub fn remove_session(&mut self, session_id: &str) {
        self.sessions.retain(|row| row.id != session_id);
        self.streaming.remove(session_id);
        self.renaming.remove(session_id);
        if self.active.as_deref() == Some(session_id) {
            self.active = None;
            self.active_title = None;
            self.active_agent_id = None;
            self.messages = Arc::new(Vec::new());
            self.compaction = None;
            self.rollback_confirm = None;
        }
    }

    pub fn active_streaming(&self) -> bool {
        self.active.as_deref().is_some_and(|id| self.streaming.contains(id))
    }

    pub fn active_compacting(&self) -> bool {
        self.active
            .as_deref()
            .is_some_and(|id| self.compacting.as_deref() == Some(id))
    }

    pub fn active_renaming(&self) -> bool {
        self.active.as_deref().is_some_and(|id| self.renaming.contains(id))
    }

    pub fn title(&self) -> String {
        self.active
            .as_deref()
            .and_then(|id| self.session(id))
            .map(|row| row.title().to_string())
            .or_else(|| self.active_title.clone().filter(|t| !t.is_empty()))
            .unwrap_or_else(|| "新对话".to_string())
    }

    /// 当前生效 Agent：会话记录优先，其次面板覆盖。
    pub fn active_agent(&self) -> Option<String> {
        self.active_agent_id.clone().or_else(|| self.agent_override.clone())
    }

    pub fn agent_label(&self) -> String {
        match self.active_agent() {
            Some(id) => self
                .agents
                .iter()
                .find(|agent| agent.id == id)
                .map(|agent| agent.name.clone())
                .unwrap_or(id),
            None => "对话".to_string(),
        }
    }

    pub fn model_label(&self) -> String {
        match &self.model_pick {
            Some((provider, model)) => self
                .models
                .iter()
                .find(|row| &row.provider_id == provider && &row.model == model)
                .map(|row| row.label.clone())
                .unwrap_or_else(|| model.clone()),
            None => "选择模型".to_string(),
        }
    }

    pub fn effort_label(&self) -> &'static str {
        match self.effort.as_deref() {
            Some("off") => "关闭",
            Some("low") => "低",
            Some("medium") => "中",
            Some("high") => "高",
            _ => "默认",
        }
    }

    pub fn push_image(&mut self, name: String, source: String) {
        self.images.push(PendingImage { name, source, staged: None });
    }

    pub fn clear_input(&mut self) {
        self.draft.clear();
        self.refs.clear();
        self.images.clear();
        self.picker = None;
        self.menu = None;
    }

    /// 草稿 / 光标 → # 候选：`#` 后到光标之间不含空白即视为查询串。
    pub fn refresh_picker(&mut self, caret: usize) {
        let caret = caret.min(self.draft.len());
        self.picker = None;
        if !self.files_ready {
            return;
        }
        let Some(hash) = self.draft[..caret].rfind('#') else { return };
        let preceded_ok = self.draft[..hash]
            .chars()
            .next_back()
            .is_none_or(|c| c.is_whitespace() || "（([{，。、；：,.".contains(c));
        if !preceded_ok {
            return;
        }
        let query = &self.draft[hash + 1..caret];
        if query.chars().any(|c| c.is_whitespace() || c == '#' || c == '：' || c == ':') {
            return;
        }
        let needle = query.to_lowercase();
        let mut hits: Vec<&FileEntry> = self
            .files
            .iter()
            .filter(|file| file.path.to_lowercase().contains(&needle))
            .collect();
        hits.sort_by_key(|file| {
            let name = file_name(&file.path).to_lowercase();
            (!name.starts_with(&needle), !name.contains(&needle), file.path.len())
        });
        let candidates = hits
            .into_iter()
            .take(PICKER_MAX)
            .map(|file| Candidate {
                path: file.path.clone(),
                is_dir: file.is_dir,
                name: file_name(&file.path).to_string(),
                sub: file
                    .path
                    .rsplit_once('/')
                    .map(|(dir, _)| dir.to_string())
                    .unwrap_or_default(),
            })
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return;
        }
        self.picker = Some(Picker { hash, caret, candidates, selected: 0 });
    }

    /// 键盘巡航候选（↑↓ 环绕）。
    pub fn move_picker(&mut self, delta: isize) {
        let Some(picker) = self.picker.as_mut() else { return };
        let len = picker.candidates.len();
        if len == 0 {
            return;
        }
        let current = picker.selected.min(len - 1) as isize;
        picker.selected = (current + delta).rem_euclid(len as isize) as usize;
    }

    /// 当前候选下标（无候选 = None）。
    pub fn picker_selected(&self) -> Option<usize> {
        self.picker.as_ref().map(|picker| picker.selected)
    }

    /// 点选候选：把 `#查询` 换成完整标签，返回新草稿与光标位置。
    pub fn pick_ref(&mut self, index: usize) -> Option<(String, usize)> {
        let picker = self.picker.take()?;
        let candidate = picker.candidates.get(index)?.clone();
        let kind = if candidate.is_dir {
            RefKind::Dir
        } else {
            kind_of_file(&candidate.path)?
        };
        let existing = self
            .refs
            .iter()
            .find(|entry| entry.path == candidate.path)
            .map(|entry| entry.label.clone());
        let label = match existing {
            Some(label) => label,
            None => {
                let taken: Vec<String> =
                    self.refs.iter().map(|entry| entry.label.clone()).collect();
                let label = make_label(&candidate.path, &taken);
                self.refs.push(RefEntry { path: candidate.path, label: label.clone(), kind });
                label
            }
        };
        let tag = format!("#{}：{} ", kind.prefix(), label);
        let mut next = String::with_capacity(self.draft.len() + tag.len());
        next.push_str(&self.draft[..picker.hash]);
        next.push_str(&tag);
        next.push_str(&self.draft[picker.caret.min(self.draft.len())..]);
        let caret_after = picker.hash + tag.len();
        Some((next, caret_after))
    }

    /// 在光标处插入 `#` 并唤起候选（工具栏入口）。
    pub fn insert_hash(&mut self, caret: usize) -> usize {
        let caret = caret.min(self.draft.len());
        self.draft.insert(caret, '#');
        caret + 1
    }

    /// 重生成：最后一条 user 消息的下标（截断到此之前）。
    pub fn regenerate_keep(&self, session_id: &str) -> Option<(usize, Value)> {
        if self.active.as_deref() != Some(session_id) {
            return None;
        }
        let index = self.messages.iter().rposition(|m| role_of(m) == "user")?;
        Some((index, self.messages[index].clone()))
    }

    /// 回到此处：保留到该消息（含）。
    pub fn rollback_keep(&self, message_id: &str) -> Option<usize> {
        self.messages
            .iter()
            .position(|m| message_id_of(m) == message_id)
            .map(|index| index + 1)
    }

    pub fn message_by_id(&self, message_id: &str) -> Option<&Value> {
        self.messages.iter().find(|m| message_id_of(m) == message_id)
    }

    pub fn is_last(&self, message_id: &str) -> bool {
        self.messages.last().is_some_and(|m| message_id_of(m) == message_id)
    }
}

pub fn message_id_of(message: &Value) -> &str {
    message.get("id").and_then(Value::as_str).unwrap_or_default()
}

pub fn role_of(message: &Value) -> &str {
    message.get("role").and_then(Value::as_str).unwrap_or_default()
}

pub fn display_of(message: &Value) -> &str {
    message
        .get("displayContent")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .or_else(|| message.get("content").and_then(Value::as_str))
        .unwrap_or_default()
}

pub fn content_of(message: &Value) -> &str {
    message.get("content").and_then(Value::as_str).unwrap_or_default()
}

/// 文件 / 目录引用的类型（只认笔记 / 表格 / 画布，其余不可引用）。
pub fn kind_of_file(path: &str) -> Option<RefKind> {
    let name = file_name(path);
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    match ext.as_str() {
        "md" => Some(RefKind::Note),
        "atb" => Some(RefKind::Table),
        "atlx" => Some(RefKind::Canvas),
        _ => None,
    }
}

fn file_name(path: &str) -> &str {
    path.trim_end_matches('/').rsplit('/').next().unwrap_or(path)
}

/// 标签生成：文件名 → 逐级补父目录消歧 → 完整路径加序号；超长按 24 字符截断。
fn make_label(path: &str, taken: &[String]) -> String {
    let segs: Vec<&str> = path.trim_end_matches('/').split('/').collect();
    let candidates: Vec<String> = (0..segs.len()).rev().map(|from| segs[from..].join("/")).collect();
    let conflict = |candidate: &str| taken.iter().any(|label| label == candidate);
    for candidate in &candidates {
        let truncated = truncate_tail(candidate, 24);
        if !conflict(&truncated) {
            return truncated;
        }
    }
    let base = candidates.last().cloned().unwrap_or_else(|| path.to_string());
    for seq in 2u32.. {
        let candidate = format!("{base} ({seq})");
        if !conflict(&candidate) {
            return candidate;
        }
    }
    unreachable!("标签冲突集合有限，序号递增必收敛")
}

fn truncate_tail(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(3)).collect();
    out.push_str("...");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn picker_model() -> Model {
        let mut model = Model::new();
        model.files_ready = true;
        model.files = vec![
            FileEntry { path: "笔记/方案.md".into(), is_dir: false },
            FileEntry { path: "数据/看板.atb".into(), is_dir: false },
            FileEntry { path: "画布/流程.atlx".into(), is_dir: false },
            FileEntry { path: "附件".into(), is_dir: true },
        ];
        model
    }

    #[test]
    fn hash_query_opens_picker_and_pick_replaces_the_tag() {
        let mut model = picker_model();
        model.draft = "看看 #方案".into();
        model.refresh_picker(model.draft.len());
        let picker = model.picker.as_ref().expect("应出现候选");
        assert_eq!(picker.candidates.len(), 1);
        let (next, caret) = model.pick_ref(0).expect("应能选中");
        assert_eq!(next, "看看 #笔记：方案.md ");
        assert_eq!(caret, next.len());
        assert_eq!(model.refs.len(), 1);
        assert!(model.refs[0].label.ends_with("方案.md"));
    }

    #[test]
    fn whitespace_or_foreign_tag_closes_picker() {
        let mut model = picker_model();
        model.draft = "#方案 后续".into();
        model.refresh_picker(model.draft.len());
        assert!(model.picker.is_none(), "空格后的内容不再是查询串");
        model.draft = "前 #笔记：已有".into();
        model.refresh_picker(model.draft.len());
        assert!(model.picker.is_none(), "已是完整标签时不再弹候选");
    }

    #[test]
    fn labels_disambiguate_same_file_names() {
        let taken = vec!["a.md".to_string()];
        assert_eq!(make_label("dir/a.md", &taken), "dir/a.md");
        assert_eq!(make_label("x/y/a.md", &taken), "y/a.md");
    }

    #[test]
    fn picker_selection_wraps_both_directions() {
        let mut model = picker_model();
        model.draft = "#".into();
        model.refresh_picker(model.draft.len());
        assert_eq!(model.picker_selected(), Some(0));
        model.move_picker(1);
        assert_eq!(model.picker_selected(), Some(1));
        model.move_picker(-1);
        assert_eq!(model.picker_selected(), Some(0));
        model.move_picker(-1);
        assert_eq!(model.picker_selected(), Some(model.picker.as_ref().unwrap().candidates.len() - 1));
    }

    #[test]
    fn repicking_same_file_reuses_its_label() {
        let mut model = picker_model();
        model.draft = "#方案".into();
        model.refresh_picker(model.draft.len());
        let (first, _) = model.pick_ref(0).expect("首次选中");
        let label = model.refs[0].label.clone();
        assert!(first.contains(&label));

        // 旧标签可能仍留在草稿里：再选同一文件必须沿用旧 label，否则按标签扫描会失配
        model.draft = "再看 #方案".into();
        model.refresh_picker(model.draft.len());
        let (second, _) = model.pick_ref(0).expect("再次选中");
        assert_eq!(model.refs.len(), 1);
        assert_eq!(model.refs[0].label, label);
        assert_eq!(second, format!("再看 #笔记：{label} "));
    }

    #[test]
    fn regenerate_and_rollback_keep_offsets() {
        let mut model = Model::new();
        model.active = Some("s".into());
        model.messages = Arc::new(vec![
            json!({ "id": "u1", "role": "user", "content": "一" }),
            json!({ "id": "a1", "role": "assistant", "content": "答" }),
            json!({ "id": "u2", "role": "user", "content": "二" }),
            json!({ "id": "a2", "role": "assistant", "content": "答二" }),
        ]);
        assert_eq!(model.regenerate_keep("s").unwrap().0, 2);
        assert_eq!(model.rollback_keep("a1"), Some(2));
        assert!(model.is_last("a2"));
    }
}
