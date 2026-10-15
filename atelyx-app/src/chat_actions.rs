//! AI 对话面板编排：会话投影、run 事件泵、发送/停止/重生成/压缩/命名的编排链。
//!
//! 状态真源分两层：磁盘真源在 `core::ai::session::SessionStore`（本模块只经
//! commit/load 触达），展示投影 `ChatState.sessions` 由编排动作直接构造更新——
//! 每次变更同时送达两层，不读回。run 循环在 core（`AgentHub`），事件经通道进
//! Elm 循环（订阅流），事件按会话身份守卫后应用到投影。

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use iced::widget::text_editor;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use atelyx_core::ai::{
    agent::{self, AgentHub, RunConfig, RunEvent},
    session::{
        AppendBatch, CommitBatch, DeleteEntry, MetaPatch, SessionDef, SessionStore,
        TruncateEntry, WriteChainConfig,
    },
    stream::LlmMessage,
    tools::{SearchConfig, ToolCtx},
};
use crate::Message;
use crate::App;

// ===== 进程级共享运行时 =====

static STORE: OnceLock<Arc<SessionStore>> = OnceLock::new();
static HUB: OnceLock<Arc<AgentHub>> = OnceLock::new();
/// 事件通道槽：发送端常驻 + 接收端由订阅流一次性取走。
/// 接收端槽用 std 锁：订阅流跑在 tokio worker 内，tokio Mutex 的阻塞取用会 panic；
/// 取走仅发生一次且无竞争，std 锁即足够。
type EventSlot<T> = (
    mpsc::UnboundedSender<T>,
    std::sync::Mutex<Option<mpsc::UnboundedReceiver<T>>>,
);

static RUN_EVENTS: OnceLock<EventSlot<RunEvent>> = OnceLock::new();
static CONTROL: OnceLock<EventSlot<ChatMsg>> = OnceLock::new();

fn store() -> &'static Arc<SessionStore> {
    STORE.get_or_init(|| {
        let store = Arc::new(SessionStore::new(WriteChainConfig::default()));
        // 落盘失败观察者 → 控制通道 → Elm（写链线程回调，不得反压）
        let tx = control_channel().0.clone();
        store.install_status_hook(move |status| {
            let _ = tx.send(ChatMsg::PersistStatus(status.map(|e| (e.message, e.at))));
        });
        store.spawn_writer();
        store
    })
}

fn hub() -> &'static Arc<AgentHub> {
    HUB.get_or_init(|| Arc::new(AgentHub::new()))
}

fn run_event_channel() -> &'static EventSlot<RunEvent> {
    RUN_EVENTS.get_or_init(|| {
        let (tx, rx) = mpsc::unbounded_channel();
        (tx, std::sync::Mutex::new(Some(rx)))
    })
}

fn control_channel() -> &'static EventSlot<ChatMsg> {
    CONTROL.get_or_init(|| {
        let (tx, rx) = mpsc::unbounded_channel();
        (tx, std::sync::Mutex::new(Some(rx)))
    })
}

/// 订阅流取走 run 事件接收端（每进程一次；订阅重建不重复取）。
pub fn take_run_event_receiver() -> Option<mpsc::UnboundedReceiver<RunEvent>> {
    run_event_channel().1.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// 订阅流取走控制事件接收端（persistError 等）。
pub fn take_control_receiver() -> Option<mpsc::UnboundedReceiver<ChatMsg>> {
    control_channel().1.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// run 事件订阅流（core run 循环 → Elm）。
pub fn run_events_subscription() -> iced::Subscription<Message> {
    iced::Subscription::run_with(String::from("chat-run-events"), |_: &String| {
        iced::stream::channel(64, async move |mut output| {
            if let Some(mut rx) = take_run_event_receiver() {
                while let Some(event) = rx.recv().await {
                    let sent = iced::futures::SinkExt::send(&mut output, Message::Chat(ChatMsg::Run(event))).await;
                    if sent.is_err() {
                        break;
                    }
                }
            }
        })
    })
}

/// 控制事件订阅流（写链 persistError 观察者 → Elm）。
pub fn control_subscription() -> iced::Subscription<Message> {
    iced::Subscription::run_with(String::from("chat-control-events"), |_: &String| {
        iced::stream::channel(16, async move |mut output| {
            if let Some(mut rx) = take_control_receiver() {
                while let Some(msg) = rx.recv().await {
                    let sent = iced::futures::SinkExt::send(&mut output, Message::Chat(msg)).await;
                    if sent.is_err() {
                        break;
                    }
                }
            }
        })
    })
}

// ===== 面板状态 =====

/// 面板状态（跨窗口共享一份；会话真源在 store）。
#[derive(Default)]
pub struct ChatState {
    /// 已装载的仓库根（None = 未进仓 / 未装载）。
    pub loaded_root: Option<PathBuf>,
    /// 会话出线投影（store snapshot 快照；显示真相）。
    pub sessions: Vec<Value>,
    /// 消息正文的 markdown 解析缓存（键 = 源文本哈希；视图按哈希取用）。
    pub docs: std::collections::HashMap<u64, atelyx_core::markdown::MarkdownDocument>,
    /// 当前会话（None = 新对话态，首条消息发出时创建会话）。
    pub active: Option<String>,
    /// 输入草稿（多行）。
    pub input: text_editor::Content,
    /// 新对话待固化 Agent（发送首条消息时落会话 agentId）。
    pub draft_agent_id: Option<String>,
    /// 活跃 run 的会话集合。
    pub streaming: HashSet<String>,
    /// 压缩中的会话。
    pub compacting: Option<String>,
    /// 请求级错误（顶部行展示；X 清除）。
    pub error: Option<String>,
    /// 落盘失败（写链观察者回填；恢复自动清除）。
    pub persist_error: Option<(String, i64)>,
    pub sidebar_open: bool,
    pub sidebar_search: String,
    /// 展开行内菜单的会话（重命名 / 删除）。
    pub session_menu: Option<String>,
    /// 删除二段确认（会话 id；与 rollback 同交互模式）。
    pub delete_confirm: Option<String>,
    /// 重新命名请求在途的会话集合（多会话并行，按会话防重）。
    pub renaming: HashSet<String>,
    /// Agent 选择浮层开启态。
    pub agent_menu_open: bool,
    /// 投影刷新节流（流式增量事件按间隔取样，避免每 token 全量重建）。
    pub last_refresh: Option<std::time::Instant>,
    /// 回滚二段确认（messageId）。
    pub rollback_confirm: Option<String>,
}

impl ChatState {
    pub fn new() -> Self {
        Self {
            sidebar_open: true,
            ..Default::default()
        }
    }

    /// 当前会话的 agentId（会话值优先，缺省回落新对话草稿）。
    pub fn active_agent_id(&self) -> Option<String> {
        if let Some(id) = &self.active {
            if let Some(s) = self.sessions.iter().find(|s| s["id"] == json!(id)) {
                if let Some(agent) = s.get("agentId").and_then(Value::as_str) {
                    return Some(agent.to_string());
                }
            }
        }
        self.draft_agent_id.clone()
    }

    fn session_mut(&mut self, id: &str) -> Option<&mut Value> {
        self.sessions.iter_mut().find(|s| s["id"] == json!(id))
    }

    /// 从 store 拉取单会话最新投影（run 事件 / 提交后调用），并预解析消息正文。
    fn refresh_session(&mut self, session_id: &str) {
        let store = store();
        let messages = store.session_messages(session_id);
        match messages {
            Some(messages) => {
                for m in &messages {
                    let sources = [
                        m.get("displayContent").and_then(Value::as_str),
                        m.get("content").and_then(Value::as_str),
                    ];
                    for src in sources.into_iter().flatten() {
                        if src.is_empty() {
                            continue;
                        }
                        let key = source_hash(src);
                        self.docs
                            .entry(key)
                            .or_insert_with(|| crate::markdown_view::parse_document(src));
                    }
                    if let Some(steps) = m.get("steps").and_then(Value::as_array) {
                        for st in steps {
                            if st.get("kind").and_then(Value::as_str) == Some("text") {
                                if let Some(text) = st.get("text").and_then(Value::as_str) {
                                    if !text.is_empty() {
                                        let key = source_hash(text);
                                        self.docs.entry(key).or_insert_with(|| {
                                            crate::markdown_view::parse_document(text)
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
                let wire = session_wire(session_id, messages);
                match self.session_mut(session_id) {
                    Some(slot) => *slot = wire,
                    None => self.sessions.push(wire),
                }
            }
            None => {
                self.sessions.retain(|s| s["id"] != json!(session_id));
                if self.active.as_deref() == Some(session_id) {
                    self.active = None;
                }
            }
        }
    }
}

fn session_wire(session_id: &str, messages: Vec<Value>) -> Value {
    let mut wire = store()
        .session_meta(session_id)
        .unwrap_or_else(|| {
            json!({ "id": session_id, "file": "", "createdAt": 0, "updatedAt": 0 })
        });
    wire["messages"] = Value::Array(messages);
    wire
}

fn latest_session_id(sessions: &[Value]) -> Option<String> {
    sessions
        .iter()
        .max_by_key(|s| s.get("updatedAt").and_then(Value::as_i64).unwrap_or(0))
        .and_then(|s| s.get("id").and_then(Value::as_str))
        .map(str::to_string)
}

// ===== 消息 =====

#[derive(Debug, Clone)]
pub enum ChatMsg {
    /// 落盘失败状态（写链观察者；None = 恢复）。
    PersistStatus(Option<(String, i64)>),
    SessionPicked(String),
    NewSession,
    DeleteSession(String),
    ToggleSidebar,
    SidebarSearch(String),
    SessionMenu(Option<String>),
    /// Agent 选择浮层（选择固化到当前会话 / 新对话草稿）。
    AgentMenu(bool),
    AgentPick(Option<String>),
    InputAction(text_editor::Action),
    Send,
    Stop,
    Regenerate,
    /// 回到此处（二段确认：首次按下进入确认态，再按执行）。
    Rollback(String),
    RollbackCancel,
    Compact,
    Rename,
    /// run 循环事件（core → Elm）。
    Run(RunEvent),
    /// 辅助调用收尾（压缩 / 手动命名完成；携带结果）。
    AidDone(AidKind, String, Option<String>),
}

/// 辅助调用类别（不算 run）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AidKind {
    Compact,
    ManualRename,
    AutoRename,
}

// ===== 仓库装载 =====

/// 进仓 / 切仓调用：停全部在途 run（在途提交会随换根失效）、装载磁盘真源并刷新投影。
pub fn load_vault(app: &mut App, root: &std::path::Path) {
    hub().stop_all();
    match store().load(root) {
        Ok(_) => {
            app.chat.loaded_root = Some(root.to_path_buf());
            let snap = store().snapshot();
            app.chat.sessions = snap.sessions;
            app.chat.persist_error = snap.persist_error.map(|e| (e.message, e.at));
            let current = app.chat.active.clone();
            app.chat.active = current
                .filter(|id| app.chat.sessions.iter().any(|s| s["id"] == json!(id)))
                .or_else(|| latest_session_id(&app.chat.sessions));
            app.chat.error = None;
            app.chat.streaming.clear();
            app.chat.compacting = None;
            app.chat.docs.clear();
            app.chat.delete_confirm = None;
            app.chat.rollback_confirm = None;
        }
        Err(e) => {
            app.chat.error = Some(format!("会话装载失败：{e}"));
        }
    }
}

/// 空间仓库脱离：本地会话真源不可达，清投影防跨仓读写。
pub fn detach_vault(app: &mut App) {
    hub().stop_all();
    app.chat.loaded_root = None;
    app.chat.sessions.clear();
    app.chat.active = None;
    app.chat.streaming.clear();
    app.chat.compacting = None;
    app.chat.docs.clear();
    app.chat.delete_confirm = None;
    app.chat.rollback_confirm = None;
}

// ===== 配置解析 =====

/// 解析 run 配置（供应商路由 / 模型 / Agent 名册）。配置缺失 = Err（面板提示）。
fn resolve_run_config(app: &App, agent_id: Option<&str>) -> Result<RunConfig, String> {
    let Some(model) = app.repo_config.model.clone() else {
        return Err("未配置默认模型（设置 → AI 设置 → 模型服务）".to_string());
    };
    let provider_id = app.repo_config.model_provider_id.clone().unwrap_or_default();
    let Some(provider) = app
        .repo_config
        .providers
        .as_ref()
        .and_then(|ps| ps.iter().find(|p| p.id == provider_id))
    else {
        return Err("默认模型的供应商已失效，请重新选择".to_string());
    };
    let api_key = match &provider.api_key {
        Some(k) if !k.is_empty() => k.clone(),
        _ => {
            let scope = key_scope(app);
            crate::credentials::load_provider_key(&scope, &provider.id)
                .map_err(|_| "读取密钥链失败".to_string())?
                .ok_or_else(|| "API key 未配置或密钥链不可用".to_string())?
        }
    };
    // Agent 名册：缺省预置「对话」（无提示词 + 只读工具）；指定 id 缺失时同样降级
    let agent = app.agents.iter().find(|a| Some(a.id.as_str()) == agent_id);
    let system_prompt = agent
        .and_then(|a| a.system_prompt_file.clone())
        .map(|file| {
            let root = app.vault_root.clone().unwrap_or_default();
            std::fs::read_to_string(root.join(&file)).unwrap_or_default()
        })
        .filter(|s| !s.trim().is_empty());
    let tool_ids = agent.map(|a| a.tools.clone()).unwrap_or_else(|| {
        ["web_search", "web_fetch", "read_file", "glob", "grep", "list_dir", "read_history"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    });
    Ok(RunConfig {
        base_url: provider.base_url.clone(),
        api_key,
        model,
        reasoning_effort: None,
        system_prompt,
        tool_ids,
        search: search_config(app),
        exclude: app.repo_config.exclude_folders.clone().unwrap_or_default(),
    })
}

fn search_config(app: &App) -> SearchConfig {
    let search = app.repo_config.search.clone().unwrap_or_default();
    let tavily_key = if search.provider == "tavily" {
        crate::credentials::load_provider_key(&key_scope(app), "tavily").ok().flatten()
    } else {
        None
    };
    SearchConfig {
        provider: if search.provider.is_empty() { None } else { Some(search.provider.clone()) },
        tavily_key,
        searxng_url: Some(search.searxng_url.clone()).filter(|s| !s.is_empty()),
    }
}

fn key_scope(app: &App) -> String {
    app.vault_root
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn tool_ctx(app: &App) -> ToolCtx {
    ToolCtx {
        root: app.vault_root.clone().unwrap_or_default(),
        queue: app.vault.writes.clone(),
        search: search_config(app),
        exclude: app.repo_config.exclude_folders.clone().unwrap_or_default(),
    }
}

// ===== 更新循环 =====

pub fn update(app: &mut App, msg: ChatMsg) -> iced::Task<Message> {
    match msg {
        ChatMsg::PersistStatus(status) => {
            app.chat.persist_error = status;
            iced::Task::none()
        }
        ChatMsg::ToggleSidebar => {
            app.chat.sidebar_open = !app.chat.sidebar_open;
            iced::Task::none()
        }
        ChatMsg::SidebarSearch(text) => {
            app.chat.sidebar_search = text;
            iced::Task::none()
        }
        ChatMsg::SessionMenu(menu) => {
            app.chat.session_menu = menu;
            iced::Task::none()
        }
        ChatMsg::AgentMenu(open) => {
            app.chat.agent_menu_open = open;
            iced::Task::none()
        }
        ChatMsg::AgentPick(id) => {
            app.chat.agent_menu_open = false;
            match app.chat.active.clone() {
                Some(session_id) => {
                    if let Some(s) = app.chat.session_mut(&session_id) {
                        s["agentId"] = id.clone().map(|v| serde_json::json!(v)).unwrap_or(Value::Null);
                    }
                    let _ = store().commit(CommitBatch {
                        metas: vec![MetaPatch {
                            id: session_id,
                            title: None,
                            agent_id: Some(id),
                            compaction: None,
                            updated_at: None,
                        }],
                        ..Default::default()
                    });
                }
                None => app.chat.draft_agent_id = id,
            }
            iced::Task::none()
        }
        ChatMsg::SessionPicked(id) => {
            app.chat.active = Some(id);
            app.chat.rollback_confirm = None;
            app.chat.error = None;
            iced::Task::none()
        }
        ChatMsg::NewSession => {
            app.chat.active = None;
            app.chat.draft_agent_id = None;
            app.chat.error = None;
            app.chat.rollback_confirm = None;
            iced::Task::none()
        }
        ChatMsg::DeleteSession(id) => {
            if app.chat.delete_confirm.as_deref() != Some(id.as_str()) {
                app.chat.delete_confirm = Some(id);
                return iced::Task::none();
            }
            app.chat.delete_confirm = None;
            app.chat.session_menu = None;
            let _ = store().commit(CommitBatch {
                deletes: vec![DeleteEntry {
                    session_id: id.clone(),
                    remove_files: true,
                }],
                ..Default::default()
            });
            app.chat.sessions.retain(|s| s["id"] != json!(id));
            if app.chat.active.as_deref() == Some(id.as_str()) {
                app.chat.active = latest_session_id(&app.chat.sessions);
            }
            iced::Task::none()
        }
        ChatMsg::InputAction(action) => {
            app.chat.input.perform(action);
            iced::Task::none()
        }
        ChatMsg::Stop => {
            if let Some(id) = app.chat.active.clone() {
                hub().stop(&id);
            }
            iced::Task::none()
        }
        ChatMsg::Send => send(app),
        ChatMsg::Regenerate => regenerate(app),
        ChatMsg::Rollback(message_id) => {
            if app.chat.rollback_confirm.as_deref() == Some(message_id.as_str()) {
                app.chat.rollback_confirm = None;
                rollback(app, &message_id)
            } else {
                app.chat.rollback_confirm = Some(message_id);
                iced::Task::none()
            }
        }
        ChatMsg::RollbackCancel => {
            app.chat.rollback_confirm = None;
            iced::Task::none()
        }
        ChatMsg::Compact => compact(app),
        ChatMsg::Rename => manual_rename(app),
        ChatMsg::Run(event) => on_run_event(app, event),
        ChatMsg::AidDone(kind, session_id, result) => {
            match kind {
                AidKind::Compact => {
                    app.chat.compacting = None;
                    if let Some(summary) = result {
                        if !summary.trim().is_empty() {
                            let anchor = app
                                .chat
                                .session_mut(&session_id)
                                .and_then(|s| {
                                    s.get_mut("messages")
                                        .and_then(Value::as_array_mut)
                                        .and_then(|ms| ms.last().cloned())
                                })
                                .and_then(|m| m.get("id").and_then(Value::as_str).map(str::to_string));
                            if let Some(anchor) = anchor {
                                let count = app
                                    .chat
                                    .session_mut(&session_id)
                                    .and_then(|s| s.get("messages").and_then(Value::as_array))
                                    .map(|ms| ms.len())
                                    .unwrap_or(0);
                                let _ = store().commit(CommitBatch {
                                    metas: vec![MetaPatch {
                                        id: session_id.clone(),
                                        title: None,
                                        agent_id: None,
                                        compaction: Some(json!({
                                            "summary": summary,
                                            "upToMessageId": anchor,
                                            "messageCount": count,
                                            "createdAt": now_ms(),
                                        })),
                                        updated_at: None,
                                    }],
                                    ..Default::default()
                                });
                            }
                        } else {
                            app.chat.error = Some("压缩失败：摘要为空".to_string());
                        }
                    } else {
                        app.chat.error = Some("压缩失败".to_string());
                    }
                }
                AidKind::ManualRename => {
                    app.chat.renaming.remove(&session_id);
                    match result {
                        Some(title) => {
                            let _ = store().commit(CommitBatch {
                                metas: vec![MetaPatch {
                                    id: session_id.clone(),
                                    title: Some(title),
                                    agent_id: None,
                                    compaction: None,
                                    updated_at: None,
                                }],
                                ..Default::default()
                            });
                        }
                        None => app.chat.error = Some("命名失败".to_string()),
                    }
                }
                AidKind::AutoRename => {
                    app.chat.renaming.remove(&session_id);
                    if let Some(title) = result {
                        let _ = store().commit(CommitBatch {
                            metas: vec![MetaPatch {
                                id: session_id.clone(),
                                title: Some(title),
                                agent_id: None,
                                compaction: None,
                                updated_at: None,
                            }],
                            ..Default::default()
                        });
                    }
                }
            }
            app.chat.refresh_session(&session_id);
            iced::Task::none()
        }
    }
}

/// 源文本哈希（markdown 解析缓存键；冲突仅回落纯文本渲染）。
pub(crate) fn source_hash(s: &str) -> u64 {
    use std::hash::Hash;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    std::hash::Hasher::finish(&h)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn uuid_v4() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn send(app: &mut App) -> iced::Task<Message> {
    let text = app.chat.input.text().trim().to_string();
    if text.is_empty() || app.chat.compacting.is_some() {
        return iced::Task::none();
    }
    if let Err(e) = check_ready(app) {
        app.chat.error = Some(e);
        return iced::Task::none();
    }
    let config = match resolve_run_config(app, app.chat.active_agent_id().as_deref()) {
        Ok(c) => c,
        Err(e) => {
            app.chat.error = Some(e);
            return iced::Task::none();
        }
    };
    // 会话就位：新对话态首条消息创建会话
    let session_id = match app.chat.active.clone() {
        Some(id) => id,
        None => {
            let id = uuid_v4();
            if store()
                .commit(CommitBatch {
                    created: vec![SessionDef {
                        id: id.clone(),
                        file: atelyx_core::ai::session::chat_message_file(&id),
                        title: None,
                        agent_id: app.chat.draft_agent_id.clone(),
                        compaction: None,
                        created_at: now_ms(),
                        updated_at: now_ms(),
                        messages: Vec::new(),
                    }],
                    ..Default::default()
                })
                .is_err()
            {
                app.chat.error = Some("会话创建失败".to_string());
                return iced::Task::none();
            }
            app.chat.active = Some(id.clone());
            app.chat.draft_agent_id = None;
            id
        }
    };
    if app.chat.streaming.contains(&session_id) || hub().is_running(&session_id) {
        return iced::Task::none(); // run 不变量：同会话单 run（以 hub 注册表为准）
    }
    // 历史快照先于追加（user 消息由 run 入参携带）
    let mut history = store().session_messages(&session_id).unwrap_or_default();
    let user_message = json!({
        "id": uuid_v4(),
        "role": "user",
        "content": text.clone(),
        "displayContent": text,
        "createdAt": now_ms(),
    });
    if let Err(e) = store().commit(CommitBatch {
        appends: vec![AppendBatch {
            session_id: session_id.clone(),
            messages: vec![user_message.clone()],
        }],
        ..Default::default()
    }) {
        app.chat.error = Some(format!("消息写入失败：{e}"));
        return iced::Task::none();
    }
    app.chat.input = text_editor::Content::new();
    app.chat.refresh_session(&session_id);
    let compaction = app
        .chat
        .session_mut(&session_id)
        .and_then(|s| s.get("compaction").cloned())
        .filter(|c| !c.is_null());
    history.push(user_message);
    start_run(config, session_id, compaction, history, app)
}

/// 发送前置检查（供应商 / 模型 / 密钥 / 仓库就位）。
fn check_ready(app: &App) -> Result<(), String> {
    if app.vault_root.is_none() {
        return Err("未打开仓库".to_string());
    }
    if app.chat.loaded_root.is_none() {
        return Err("会话仓库未就绪".to_string());
    }
    Ok(())
}

fn start_run(
    config: RunConfig,
    session_id: String,
    compaction: Option<Value>,
    history: Vec<Value>,
    app: &App,
) -> iced::Task<Message> {
    let ctx = tool_ctx(app);
    let store = Arc::clone(store());
    let hub = Arc::clone(hub());
    let events = run_event_channel().0.clone();
    iced::Task::future(async move {
        if let Err(e) = hub
            .start(store, events, session_id.clone(), compaction, config, history, ctx)
            .await
        {
            let _ = run_event_channel().0.send(RunEvent::Failed {
                session_id,
                message_id: String::new(),
                error: e,
            });
        }
        Message::Noop
    })
}

fn regenerate(app: &mut App) -> iced::Task<Message> {
    let Some(session_id) = app.chat.active.clone() else {
        return iced::Task::none();
    };
    if app.chat.streaming.contains(&session_id) || app.chat.compacting.is_some() {
        return iced::Task::none();
    }
    let Some(session) = app.chat.sessions.iter().find(|s| s["id"] == json!(session_id)) else {
        return iced::Task::none();
    };
    let messages = session
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    // 找最后一条 user：重生成 = 截断到它（含）并以 displayContent 重发
    let Some(last_user_idx) = messages.iter().rposition(|m| m["role"] == json!("user")) else {
        return iced::Task::none();
    };
    let config = match resolve_run_config(app, session.get("agentId").and_then(Value::as_str)) {
        Ok(c) => c,
        Err(e) => {
            app.chat.error = Some(e);
            return iced::Task::none();
        }
    };
    if let Err(e) = store().commit(CommitBatch {
        truncations: vec![TruncateEntry {
            session_id: session_id.clone(),
            keep_count: last_user_idx + 1,
        }],
        ..Default::default()
    }) {
        app.chat.error = Some(format!("回退失败：{e}"));
        return iced::Task::none();
    }
    let history = store().session_messages(&session_id).unwrap_or_default();
    let compaction = session.get("compaction").cloned().filter(|c| !c.is_null());
    app.chat.refresh_session(&session_id);
    start_run(config, session_id, compaction, history, app)
}

fn rollback(app: &mut App, message_id: &str) -> iced::Task<Message> {
    let Some(session_id) = app.chat.active.clone() else {
        return iced::Task::none();
    };
    if app.chat.streaming.contains(&session_id) {
        return iced::Task::none();
    }
    let Some(session) = app.chat.sessions.iter().find(|s| s["id"] == json!(session_id)) else {
        return iced::Task::none();
    };
    let messages = session
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let Some(idx) = messages
        .iter()
        .position(|m| m.get("id").and_then(Value::as_str) == Some(message_id))
    else {
        return iced::Task::none();
    };
    if idx + 1 >= messages.len() {
        return iced::Task::none(); // 最后一条无可回滚
    }
    if let Err(e) = store().commit(CommitBatch {
        truncations: vec![TruncateEntry {
            session_id: session_id.clone(),
            keep_count: idx + 1,
        }],
        ..Default::default()
    }) {
        app.chat.error = Some(format!("回滚失败：{e}"));
        return iced::Task::none();
    }
    app.chat.refresh_session(&session_id);
    iced::Task::none()
}

/// 手动压缩：全部消息折进检查点（≥2 条），摘要写会话 compaction 注解（非破坏）。
fn compact(app: &mut App) -> iced::Task<Message> {
    let Some(session_id) = app.chat.active.clone() else {
        return iced::Task::none();
    };
    if app.chat.streaming.contains(&session_id) || app.chat.compacting.is_some() {
        return iced::Task::none();
    }
    let config = match resolve_run_config(app, app.chat.active_agent_id().as_deref()) {
        Ok(c) => c,
        Err(e) => {
            app.chat.error = Some(e);
            return iced::Task::none();
        }
    };
    let Some(session) = app.chat.sessions.iter().find(|s| s["id"] == json!(session_id)) else {
        return iced::Task::none();
    };
    let messages = session
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if messages.len() < 2 {
        app.chat.error = Some("对话内容太少，无需压缩".to_string());
        return iced::Task::none();
    }
    let compaction = session.get("compaction").cloned().filter(|c| !c.is_null());
    let mut transcript = String::new();
    for m in agent::history_to_llm_messages(&messages, compaction.as_ref()) {
        match m {
            LlmMessage::User { parts } => {
                transcript.push_str(&format!("用户：{}\n\n", parts.join("\n")));
            }
            LlmMessage::Assistant { content, tool_calls } => {
                let body = match content {
                    Some(c) if !c.is_empty() => c,
                    _ if !tool_calls.is_empty() => format!(
                        "（调用工具 {}）",
                        tool_calls.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join("、")
                    ),
                    _ => String::new(),
                };
                transcript.push_str(&format!("助手：{body}\n\n"));
            }
            LlmMessage::Tool { content, .. } => {
                transcript.push_str(&format!("工具结果：{}\n\n", content));
            }
            LlmMessage::System { .. } => {}
        }
    }
    app.chat.compacting = Some(session_id.clone());
    iced::Task::future(async move {
        let client = agent::aid_client(&config.base_url);
        let summary = agent::compact_summary(
            &client,
            &config.base_url,
            &config.api_key,
            &config.model,
            &transcript,
        )
        .await;
        Message::Chat(ChatMsg::AidDone(AidKind::Compact, session_id, summary))
    })
}

fn manual_rename(app: &mut App) -> iced::Task<Message> {
    let Some(session_id) = app.chat.active.clone() else {
        return iced::Task::none();
    };
    if app.chat.renaming.contains(&session_id) || app.chat.streaming.contains(&session_id) {
        return iced::Task::none();
    }
    let config = match resolve_run_config(app, app.chat.active_agent_id().as_deref()) {
        Ok(c) => c,
        Err(e) => {
            app.chat.error = Some(e);
            return iced::Task::none();
        }
    };
    let Some(session) = app.chat.sessions.iter().find(|s| s["id"] == json!(session_id)) else {
        return iced::Task::none();
    };
    let messages = session
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let excerpt = transcript_excerpt(&messages);
    app.chat.renaming.insert(session_id.clone());
    iced::Task::future(async move {
        let client = agent::aid_client(&config.base_url);
        let title = agent::auto_title(
            &client,
            &config.base_url,
            &config.api_key,
            &config.model,
            &excerpt,
        )
        .await;
        Message::Chat(ChatMsg::AidDone(AidKind::ManualRename, session_id, title))
    })
}

fn transcript_excerpt(messages: &[Value]) -> String {
    messages
        .iter()
        .filter_map(|m| {
            let role = m.get("role").and_then(Value::as_str)?;
            let content = m.get("content").and_then(Value::as_str)?;
            if content.is_empty() {
                return None;
            }
            Some(format!("{role}: {content}"))
        })
        .collect::<Vec<_>>()
        .join("\n\n")
        .chars()
        .take(2000)
        .collect()
}

// ===== run 事件应用 =====

fn on_run_event(app: &mut App, event: RunEvent) -> iced::Task<Message> {
    let session_id = event_session(&event).to_string();
    let forced = !matches!(
        event,
        RunEvent::TextDelta { .. } | RunEvent::ReasoningDelta { .. } | RunEvent::ToolStart { .. }
    );
    let due = app
        .chat
        .last_refresh
        .map(|t| t.elapsed() >= std::time::Duration::from_millis(150))
        .unwrap_or(true);
    match &event {
        RunEvent::Started { .. } => {
            app.chat.streaming.insert(session_id.clone());
            app.chat.error = None;
            app.chat.refresh_session(&session_id);
        }
        RunEvent::TextDelta { .. } | RunEvent::ReasoningDelta { .. } | RunEvent::ToolStart { .. } => {
            if due {
                app.chat.last_refresh = Some(std::time::Instant::now());
                app.chat.refresh_session(&session_id);
            }
        }
        RunEvent::ToolEnd { .. } => {
            app.chat.refresh_session(&session_id);
        }
        RunEvent::Finished { .. } => {
            app.chat.streaming.remove(&session_id);
            app.chat.last_refresh = None;
            app.chat.refresh_session(&session_id);
            // 话题自动命名：新会话无标题且命名模式开启时触发（独立轻量调用，不算 run）
            if let Some(task) = maybe_auto_name(app, &session_id) {
                return task;
            }
        }
        RunEvent::Failed { error, .. } => {
            app.chat.streaming.remove(&session_id);
            if !error.is_empty() {
                app.chat.error = Some(error.clone());
            }
            app.chat.refresh_session(&session_id);
        }
    }
    let _ = forced;
    iced::Task::none()
}

fn event_session(event: &RunEvent) -> &str {
    match event {
        RunEvent::Started { session_id, .. }
        | RunEvent::TextDelta { session_id, .. }
        | RunEvent::ReasoningDelta { session_id, .. }
        | RunEvent::ToolStart { session_id, .. }
        | RunEvent::ToolEnd { session_id, .. }
        | RunEvent::Finished { session_id, .. }
        | RunEvent::Failed { session_id, .. } => session_id,
    }
}

/// 新会话终局后自动命名（AI 设置三选；写前二次校验标题仍为空）。
fn maybe_auto_name(app: &mut App, session_id: &str) -> Option<iced::Task<Message>> {
    use crate::settings_view::ai::NamingMode;
    if app.chat.renaming.contains(session_id)
        || crate::settings_view::ai::naming_mode(app) == NamingMode::Off
    {
        return None;
    }
    let session = app.chat.sessions.iter().find(|s| s["id"] == json!(session_id))?;
    if session.get("title").and_then(Value::as_str).is_some() {
        return None;
    }
    let messages = session.get("messages").and_then(Value::as_array)?.clone();
    if !messages.iter().any(|m| m["role"] == json!("user"))
        || !messages.iter().any(|m| m["role"] == json!("assistant"))
    {
        return None;
    }
    let agent_id = session.get("agentId").and_then(Value::as_str);
    let default = resolve_run_config(app, agent_id).ok()?;
    // 命名路由：设置指定模型 > 默认模型（供应商失效不跨家替换）
    let (base_url, api_key, model) = match crate::settings_view::ai::naming_mode(app) {
        NamingMode::Custom => {
            let pick = app.repo_config.auto_naming_model.clone()?;
            let provider = app
                .repo_config
                .providers
                .as_ref()?
                .iter()
                .find(|p| p.id == pick.provider_id)?;
            let api_key = match &provider.api_key {
                Some(k) if !k.is_empty() => k.clone(),
                _ => crate::credentials::load_provider_key(&key_scope(app), &provider.id)
                    .ok()
                    .flatten()
                    .filter(|k| !k.is_empty())?,
            };
            (provider.base_url.clone(), api_key, pick.model)
        }
        _ => (default.base_url.clone(), default.api_key.clone(), default.model.clone()),
    };
    let excerpt = transcript_excerpt(&messages);
    app.chat.renaming.insert(session_id.to_string());
    let sid = session_id.to_string();
    Some(iced::Task::future(async move {
        let client = agent::aid_client(&base_url);
        let title = agent::auto_title(&client, &base_url, &api_key, &model, &excerpt).await;
        Message::Chat(ChatMsg::AidDone(AidKind::AutoRename, sid, title))
    }))
}
