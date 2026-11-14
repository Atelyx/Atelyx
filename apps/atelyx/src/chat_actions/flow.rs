//! 对话编排骨架：面板与画布卡共用的发送 / 重生成 / 回滚 / 压缩流程与存储抽象。
//! 逐字相同的步骤在此收口；只读门、忙碌判定、refs 形状、注入块、last 判定落在两侧实现。

use super::*;
use crate::canvas_chat::SendOrigin;
use atelyx_core::ai::chat_send::RefsShape;

/// 附件落盘回执后的续跑包（面板 / 画布卡共用）。
pub(crate) struct SendReady {
    pub config: RunConfig,
    pub display: String,
    pub refs: Vec<ai_refs::RefEntry>,
    /// 发送前托盘图片（回执失败时归还；面板恒空）。
    pub images: Vec<PendingImage>,
    pub attachments: Vec<Value>,
    pub origin: SendOrigin,
}

/// 骨架里的动作分支（各侧可点条件不同，原样保留在实现里）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChatAction {
    Regenerate,
    Rollback,
    Compact,
}

/// 压缩前置检查顺序：面板先解析配置（提示更具体），画布卡先判消息条数与锚点。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompactOrder {
    ConfigFirst,
    MessagesFirst,
}

/// 一份对话真源的存储视图。
pub(crate) trait ChatStore {
    /// 会话身份（run 事件 / 压缩回执路由）。
    fn session_id(&self) -> String;
    /// 显示投影的消息集合（守卫与压缩入参）。
    fn messages(&self, app: &App) -> Vec<Value>;
    /// 落盘真源的消息集合（发送前历史与截断后回读）。
    fn history(&self, app: &App) -> Vec<Value>;
    /// 追加一条消息；失败原因原样进提示。
    fn append(&self, app: &mut App, message: Value) -> Result<(), String>;
    /// 截断到 history 条数：面板只提交条数（保留磁盘正文），画布卡整段替换（落重建后的正文）。
    fn truncate_to(&self, app: &mut App, history: &mut Vec<Value>) -> Result<(), String>;
    /// 写压缩检查点（面板提交会话注解，画布卡写节点数据）。
    fn write_compaction(&self, app: &mut App, record: Value) -> Result<(), String>;
    fn compaction(&self, app: &App) -> Option<Value>;
    fn streaming(&self, app: &App) -> bool;
    /// 动作闸门（忙碌 / 只读 / 压缩在途的组合各侧不同）。
    fn blocked(&self, app: &App, action: ChatAction) -> bool;
    fn run_config(
        &self,
        app: &App,
        space_prompt: Option<String>,
        for_run: bool,
    ) -> Result<RunConfig, String>;
    /// 消息 refs 字段形状。
    fn ref_shape(&self) -> RefsShape;
    /// 引用命中后的本侧注入块 + 需挂为图片附件的文件。
    fn injection(&self, _app: &App, _hits: &[ai_refs::RefHit]) -> (String, Vec<String>) {
        (String::new(), Vec::new())
    }
    /// 引用落盘后的关系维护（画布卡建数据流边）。
    fn link_refs(&self, _app: &mut App, _refs: &[ai_refs::RefEntry]) {}
    /// 发送目标仍存在（画布卡在图片落盘期间被删 = 早退）。
    fn target_ready(&self, _app: &App) -> bool {
        true
    }
    /// 早退归还输入面（图片托盘 + 引用）。
    fn restore_inputs(
        &self,
        app: &mut App,
        images: Vec<PendingImage>,
        refs: Vec<ai_refs::RefEntry>,
        origin: SendOrigin,
    );
    fn set_error(&self, app: &mut App, error: String);
    /// 追加失败的本侧提示。
    fn append_failed(&self, app: &mut App, error: String);
    /// 忙碌提示文案（两边不同）。
    fn busy_notice(&self) -> &'static str;
    /// 追加成功后的本侧收尾（清草稿 / 置流式 / 刷新文档 / 插件事件）。
    fn after_append(&self, app: &mut App, origin: SendOrigin);
    /// 重生成前重建一条 user 的注入块（面板只拼路径块，画布卡叠加卡内容）。
    fn rebuild_user_content(&self, app: &App, message: &mut Value);
    /// 空间模式提示词预取：返回任务 = 已发起，回执后续跑同一入口。
    fn prefetch_regenerate(
        &self,
        _app: &mut App,
        _history: Vec<Value>,
    ) -> Option<iced::Task<Message>> {
        None
    }
    /// 截断 / 压缩写入后的本侧刷新。
    fn refresh(&self, app: &mut App, action: ChatAction);
    fn start_run(&self, app: &App, config: RunConfig, history: Vec<Value>) -> iced::Task<Message>;
    /// run 之后的本侧收尾（滚底 / 新图片取句柄）。
    fn tail(&self, _app: &App, _image_files: &[String]) -> iced::Task<Message> {
        iced::Task::none()
    }
    fn compact_order(&self) -> CompactOrder {
        CompactOrder::MessagesFirst
    }
    /// 压缩前锚点校验（画布卡要求末条消息有 id）。
    fn compact_precheck(&self, _app: &App, _messages: &[Value]) -> Result<(), String> {
        Ok(())
    }
    /// 压缩在途标记（面板存会话 id，画布卡存请求时刻锚点）。
    fn mark_compacting(&self, _app: &mut App, _messages: &[Value]) {}
    /// 压缩回执清标记并给出锚点（面板取回执时刻末条，画布卡用请求时刻锚点）。
    fn clear_compacting(&self, app: &mut App) -> Option<(String, usize)>;
    /// 锚点缺失时的提示（面板静默跳过）。
    fn missing_anchor_error(&self) -> Option<&'static str> {
        None
    }
}

/// 附件落盘回执后的发送收尾：闸门复检 → 注入块 → user 消息落真源 → 启动 run。
pub(crate) fn finish_send<S: ChatStore>(
    app: &mut App,
    store: &S,
    send: SendReady,
) -> iced::Task<Message> {
    let SendReady { config, display, refs, images, attachments, origin } = send;
    if store.streaming(app) {
        store.restore_inputs(app, images, refs, origin);
        app.notify(crate::NoticeLevel::Warning, store.busy_notice());
        return iced::Task::none();
    }
    if !store.target_ready(app) {
        store.restore_inputs(app, images, refs, origin);
        return iced::Task::none();
    }
    let hits = ai_refs::scan_hits(&display, &refs);
    let path_block = ai_refs::reference_path_block(&hits);
    let (card_block, image_files) = store.injection(app, &hits);
    let content = chat_send::message_content(&path_block, &card_block, &display);
    let refs_json = chat_send::refs_json(&hits, &refs, store.ref_shape());
    let attachments = attach_images(attachments, &image_files);
    let image_paths = image_files_of(&attachments);
    store.link_refs(app, &refs);
    let message = chat_send::user_message(
        uuid_v4(),
        content,
        display,
        refs_json,
        attachments,
        now_ms(),
    );
    let mut history = store.history(app);
    if let Err(e) = store.append(app, message.clone()) {
        store.restore_inputs(app, images, refs, origin);
        store.append_failed(app, format!("消息写入失败：{e}"));
        return iced::Task::none();
    }
    history.push(message);
    store.after_append(app, origin);
    store.start_run(app, config, history).chain(store.tail(app, &image_paths))
}

/// 重生成：截断到最后一条 user（含），按当下引用重建注入块后重发。
///
/// resumed：空间提示词回执带回的待跑历史（None = 由真源现取）。
pub(crate) fn regenerate<S: ChatStore>(
    app: &mut App,
    store: &S,
    space_prompt: Option<String>,
    resumed: Option<Vec<Value>>,
) -> iced::Task<Message> {
    if store.blocked(app, ChatAction::Regenerate) {
        return iced::Task::none();
    }
    let messages = store.messages(app);
    let Some(last_user) = chat_send::last_user_index(&messages) else {
        return iced::Task::none();
    };
    let resuming = resumed.is_some();
    let mut history = resumed.unwrap_or_else(|| messages[..=last_user].to_vec());
    if let Some(last) = history.last_mut() {
        store.rebuild_user_content(app, last);
    }
    if !resuming {
        if let Some(task) = store.prefetch_regenerate(app, history.clone()) {
            return task;
        }
    }
    let config = match store.run_config(app, space_prompt, true) {
        Ok(config) => config,
        Err(e) => {
            store.set_error(app, e);
            return iced::Task::none();
        }
    };
    if let Err(e) = store.truncate_to(app, &mut history) {
        store.set_error(app, format!("回退失败：{e}"));
        return iced::Task::none();
    }
    // 面板按落盘真源回读（正文是磁盘事实），回读后再重拼注入块
    if let Some(last) = history.last_mut() {
        store.rebuild_user_content(app, last);
    }
    store.refresh(app, ChatAction::Regenerate);
    store.start_run(app, config, history)
}

/// 回滚到某条消息（含）：截断其后全部消息。
pub(crate) fn rollback<S: ChatStore>(
    app: &mut App,
    store: &S,
    message_id: &str,
) -> iced::Task<Message> {
    if store.blocked(app, ChatAction::Rollback) {
        return iced::Task::none();
    }
    let messages = store.messages(app);
    let Some(keep) = chat_send::rollback_keep_count(&messages, message_id) else {
        return iced::Task::none();
    };
    let mut history = messages[..keep].to_vec();
    if let Err(e) = store.truncate_to(app, &mut history) {
        store.set_error(app, format!("回滚失败：{e}"));
        return iced::Task::none();
    }
    store.refresh(app, ChatAction::Rollback);
    iced::Task::none()
}

/// 手动压缩：全部消息折进检查点（≥2 条），摘要写本侧压缩注解（非破坏）。
pub(crate) fn compact<S: ChatStore>(app: &mut App, store: &S) -> iced::Task<Message> {
    if store.blocked(app, ChatAction::Compact) {
        return iced::Task::none();
    }
    let mut config = None;
    if store.compact_order() == CompactOrder::ConfigFirst {
        match store.run_config(app, None, false) {
            Ok(c) => config = Some(c),
            Err(e) => {
                store.set_error(app, e);
                return iced::Task::none();
            }
        }
    }
    let messages = store.messages(app);
    if messages.len() < 2 {
        store.set_error(app, "对话内容太少，无需压缩".to_string());
        return iced::Task::none();
    }
    if let Err(e) = store.compact_precheck(app, &messages) {
        store.set_error(app, e);
        return iced::Task::none();
    }
    let config = match config {
        Some(config) => config,
        None => match store.run_config(app, None, false) {
            Ok(config) => config,
            Err(e) => {
                store.set_error(app, e);
                return iced::Task::none();
            }
        },
    };
    let compaction = store.compaction(app);
    store.mark_compacting(app, &messages);
    compact_task(store.session_id(), config, &messages, compaction)
}

/// 压缩回执：清在途标记 → 空摘要 / 缺锚点提示 → 写检查点。
pub(crate) fn compact_done<S: ChatStore>(
    app: &mut App,
    store: &S,
    result: Option<String>,
) {
    let anchor = store.clear_compacting(app);
    let summary_empty = result.as_ref().is_some_and(|s| s.trim().is_empty());
    let Some(summary) = result.filter(|s| !s.trim().is_empty()) else {
        let message = if summary_empty { "压缩失败：摘要为空" } else { "压缩失败" };
        store.set_error(app, message.to_string());
        return;
    };
    let Some((up_to, count)) = anchor else {
        if let Some(message) = store.missing_anchor_error() {
            store.set_error(app, message.to_string());
        }
        return;
    };
    let record = chat_send::compaction_record(summary, &up_to, count, now_ms());
    if let Err(e) = store.write_compaction(app, record) {
        store.set_error(app, format!("压缩摘要写入失败：{e}"));
    }
}

/// 卡侧图片引用并入附件（同文件只挂一次，mime 按扩展名）。
fn attach_images(mut attachments: Vec<Value>, image_files: &[String]) -> Vec<Value> {
    for file in image_files {
        if attachments
            .iter()
            .any(|a| a.get("file").and_then(Value::as_str) == Some(file.as_str()))
        {
            continue;
        }
        let ext = file
            .rsplit_once('.')
            .map(|(_, e)| e.to_ascii_lowercase())
            .unwrap_or_default();
        attachments.push(json!({
            "kind": "image",
            "mime": image_mime(&ext),
            "file": file,
        }));
    }
    attachments
}

/// 消息附件里的图片路径（新发送的图片立刻取句柄用）。
fn image_files_of(attachments: &[Value]) -> Vec<String> {
    attachments
        .iter()
        .filter(|a| a.get("kind").and_then(Value::as_str) == Some("image"))
        .filter_map(|a| a.get("file").and_then(Value::as_str).map(str::to_string))
        .collect()
}

/// 井号触发段的解析结果（同一草稿与浮层 → 同一份判定）。
pub(crate) struct MentionSync {
    at: usize,
    query: String,
    same_trigger: bool,
    stale: bool,
}

impl MentionSync {
    /// 光标前没有触发段 = None（调用方收起浮层）。
    pub(crate) fn of(
        draft: &chat_input::Draft,
        picker: Option<&MentionPicker>,
    ) -> Option<Self> {
        let at = draft.active_mention_at()?;
        let caret = draft.caret.min(draft.text.len());
        let query = draft.text[at + 1..caret].to_string();
        let same_trigger = picker.is_some_and(|p| p.at == at);
        let stale = picker.is_some_and(|p| p.query != query);
        Some(Self { at, query, same_trigger, stale })
    }

    /// 触发段与查询都没变 = 不必重算候选。
    pub(crate) fn unchanged(&self) -> bool {
        self.same_trigger && !self.stale
    }

    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    /// 应用候选：新触发段 = 新建浮层；查询变化 = 就地换候选；候选空 = 收起。
    pub(crate) fn apply(self, picker: &mut Option<MentionPicker>, candidates: Vec<MentionTarget>) {
        if !self.same_trigger {
            *picker = (!candidates.is_empty()).then_some(MentionPicker {
                at: self.at,
                query: self.query,
                selected: 0,
                candidates,
            });
        } else if self.stale {
            if let Some(p) = picker.as_mut() {
                p.query = self.query;
                p.candidates = candidates;
                p.selected = 0;
            }
        }
        if picker.as_ref().is_some_and(|p| p.candidates.is_empty()) {
            *picker = None;
        }
    }
}

/// 文件引用落地：标签文本替换触发段并登记（同路径只记一条）。
///
/// 返回 false = 扩展名不可引用（调用方各自决定浮层去留）。
pub(crate) fn commit_vault_ref(
    draft: &mut chat_input::Draft,
    refs: &mut Vec<ai_refs::RefEntry>,
    rel: String,
    is_dir: bool,
    at: Option<usize>,
) -> bool {
    let kind = if is_dir {
        ai_refs::RefKind::Dir
    } else {
        match ai_refs::kind_of_file(&rel) {
            Some(kind) => kind,
            None => return false,
        }
    };
    let at = at.unwrap_or(draft.caret);
    let taken: Vec<(&str, &str)> = refs
        .iter()
        .map(|r| (r.path.as_str(), r.label.as_str()))
        .collect();
    let label = ai_refs::make_label(&rel, &taken);
    let tag = chat_send::mention_tag(kind, &label);
    let (text, caret) = chat_send::insert_mention(&draft.text, draft.caret, at, &tag);
    draft.apply_edited(text, caret, caret);
    if !refs.iter().any(|r| r.path == rel) {
        refs.push(ai_refs::RefEntry { path: rel, label, kind, node_id: None });
    }
    true
}

fn projection_messages(app: &App, session_id: &str) -> Vec<Value> {
    app.chat
        .sessions
        .iter()
        .find(|s| s["id"] == json!(session_id))
        .and_then(|s| s.get("messages").and_then(Value::as_array))
        .cloned()
        .unwrap_or_default()
}

/// 面板会话：投影读、store 提交、插件事件与顶部错误行。
pub(crate) struct PanelStore {
    session_id: String,
}

impl PanelStore {
    pub(crate) fn new(session_id: String) -> Self {
        Self { session_id }
    }
}

impl ChatStore for PanelStore {
    fn session_id(&self) -> String {
        self.session_id.clone()
    }

    fn messages(&self, app: &App) -> Vec<Value> {
        projection_messages(app, &self.session_id)
    }

    fn history(&self, _app: &App) -> Vec<Value> {
        store().session_messages(&self.session_id).unwrap_or_default()
    }

    fn append(&self, _app: &mut App, message: Value) -> Result<(), String> {
        store()
            .commit(CommitBatch {
                appends: vec![AppendBatch {
                    session_id: self.session_id.clone(),
                    messages: vec![message],
                }],
                ..Default::default()
            })
            .map_err(|e| e.to_string())
    }

    fn truncate_to(&self, _app: &mut App, history: &mut Vec<Value>) -> Result<(), String> {
        store()
            .commit(CommitBatch {
                truncations: vec![TruncateEntry {
                    session_id: self.session_id.clone(),
                    keep_count: history.len(),
                }],
                ..Default::default()
            })
            .map_err(|e| e.to_string())?;
        *history = store().session_messages(&self.session_id).unwrap_or_default();
        Ok(())
    }

    fn write_compaction(&self, app: &mut App, record: Value) -> Result<(), String> {
        store()
            .commit(CommitBatch {
                metas: vec![MetaPatch {
                    id: self.session_id.clone(),
                    title: None,
                    agent_id: None,
                    compaction: Some(record),
                    updated_at: None,
                }],
                ..Default::default()
            })
            .map_err(|e| e.to_string())?;
        app.plugins
            .on_chat_changed(&self.session_id, ChatChangeKind::Messages);
        Ok(())
    }

    fn compaction(&self, app: &App) -> Option<Value> {
        app.chat
            .sessions
            .iter()
            .find(|s| s["id"] == json!(self.session_id))
            .and_then(|s| s.get("compaction").cloned())
            .filter(|c| !c.is_null())
    }

    fn streaming(&self, app: &App) -> bool {
        // 投影流式态之外再看 hub 注册表：发送收尾复检以 run 真源为准
        app.chat.streaming.contains(&self.session_id) || hub().is_running(&self.session_id)
    }

    fn blocked(&self, app: &App, action: ChatAction) -> bool {
        // 闸门只看投影流式态（与 hub 的复检只在发送收尾做）
        let streaming = app.chat.streaming.contains(&self.session_id);
        match action {
            // 回滚只看 run 在途（压缩在途不影响回滚）
            ChatAction::Rollback => streaming,
            ChatAction::Regenerate | ChatAction::Compact => {
                streaming || app.chat.compacting.is_some()
            }
        }
    }

    fn run_config(
        &self,
        app: &App,
        space_prompt: Option<String>,
        for_run: bool,
    ) -> Result<RunConfig, String> {
        resolve_run_config(app, app.chat.active_agent_id().as_deref(), space_prompt, for_run, None)
    }

    fn ref_shape(&self) -> RefsShape {
        RefsShape::File
    }

    fn restore_inputs(
        &self,
        app: &mut App,
        _images: Vec<PendingImage>,
        refs: Vec<ai_refs::RefEntry>,
        _origin: SendOrigin,
    ) {
        app.chat.mention_refs = refs;
    }

    fn set_error(&self, app: &mut App, error: String) {
        app.chat.error = Some(error);
    }

    fn append_failed(&self, app: &mut App, error: String) {
        app.chat.error = Some(error);
    }

    fn busy_notice(&self) -> &'static str {
        "该会话已有对话进行中，消息未发送"
    }

    fn after_append(&self, app: &mut App, _origin: SendOrigin) {
        app.chat.input.clear();
        app.chat.mention_picker = None;
        app.chat.refresh_session(&self.session_id);
        app.plugins
            .on_chat_changed(&self.session_id, ChatChangeKind::Messages);
        app.chat.follow_bottom = true;
    }

    fn rebuild_user_content(&self, _app: &App, message: &mut Value) {
        rebuild_user_injection(message);
    }

    fn refresh(&self, app: &mut App, _action: ChatAction) {
        app.chat.refresh_session(&self.session_id);
        app.plugins
            .on_chat_changed(&self.session_id, ChatChangeKind::Messages);
    }

    fn start_run(&self, app: &App, config: RunConfig, history: Vec<Value>) -> iced::Task<Message> {
        let compaction = self.compaction(app);
        super::start_run(config, self.session_id.clone(), compaction, history, app)
    }

    fn tail(&self, _app: &App, _image_files: &[String]) -> iced::Task<Message> {
        scroll_flow_bottom()
    }

    fn compact_order(&self) -> CompactOrder {
        CompactOrder::ConfigFirst
    }

    fn mark_compacting(&self, app: &mut App, _messages: &[Value]) {
        app.chat.compacting = Some(self.session_id.clone());
    }

    fn clear_compacting(&self, app: &mut App) -> Option<(String, usize)> {
        app.chat.compacting = None;
        chat_send::compaction_anchor(&projection_messages(app, &self.session_id))
    }
}
