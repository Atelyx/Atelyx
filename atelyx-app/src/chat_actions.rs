//! AI 对话面板编排：会话投影、run 事件泵、发送/停止/重生成/压缩/命名的编排链。
//!
//! 状态真源分两层：磁盘真源在 `core::ai::session::SessionStore`（本模块只经
//! commit/load 触达），展示投影 `ChatState.sessions` 由编排动作直接构造更新——
//! 每次变更同时送达两层，不读回。run 循环在 core（`AgentHub`），事件经通道进
//! Elm 循环（订阅流），事件按会话身份守卫后应用到投影。

use std::collections::HashSet;
use std::sync::{Arc, OnceLock};


use serde_json::{json, Value};
use tokio::sync::mpsc;

use atelyx_core::ai::{
    agent::{self, AgentHub, RunConfig, RunEvent},
    refs as ai_refs,
    session::{
        AppendBatch, CommitBatch, DeleteEntry, MetaPatch, OverridesPatch, SessionDef, SessionStore,
        StoreSnapshot, TruncateEntry, WriteChainConfig,
    },
    stream::LlmMessage,
    tools::{RepoAccess, SearchConfig, ToolCtx},
};
use atelyx_core::plugin_runtime::{ChatEvent, ChatEventParams, ChatHistoryResult, ChatSessionSummary, ChatSessionWire};

use crate::chat_input;
use crate::Message;
use crate::App;
use crate::RepoKind;

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

/// 消息流滚动容器 id（视图挂载与滚动命令共用）。
pub fn chat_flow_id() -> iced::widget::Id {
    iced::widget::Id::new("chat-flow")
}

/// 消息流滚到底（跟随与回底浮钮共用；超大偏移由滚动容器钳制到末端）。
pub fn scroll_flow_bottom() -> iced::Task<Message> {
    iced::widget::operation::scroll_to(
        chat_flow_id(),
        iced::widget::scrollable::AbsoluteOffset { x: Some(0.0), y: Some(1.0e9) },
    )
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
    /// 已装载真源的身份键（本地 = 仓库根；空间 = 「服务器地址/空间 id」；None = 未装载）。
    pub session_key: Option<String>,
    /// 会话出线投影（store snapshot 快照；显示真相）。
    pub sessions: Vec<Value>,
    /// 消息正文的 markdown 解析缓存（键 = 源文本哈希；按会话刷新时收敛）。
    pub docs: std::collections::HashMap<u64, atelyx_core::markdown::MarkdownDocument>,
    /// 发送 / 重生成在等 Agent 提示词取回（空间模式：提示词文件在服务端）。
    pub pending_action: Option<PendingChatAction>,
    /// 已取回待用的 Agent 提示词正文（一次性，随本次发送消费）。
    pub pending_prompt: Option<String>,
    /// 面板级模型选择（None = 未选择，发送前检查拒绝；值形如 `{providerId, model}`）。
    pub model_override: Option<Value>,
    /// 面板级推理等级覆盖（None = 跟随默认不发送；值 = 线格式 low/medium/high）。
    pub effort: Option<String>,
    /// 消息流贴底跟随（true = 新内容自动滚到底；上翻即脱离，回底浮钮恢复）。
    pub follow_bottom: bool,
    /// 会话列表悬停行（操作钮显形用）。
    pub row_hover: Option<String>,
    /// 当前会话（None = 新对话态，首条消息发出时创建会话）。
    pub active: Option<String>,
    /// 输入草稿（自绘输入面真相：文本 / 光标 / 选区 / 组合串 / 撤销栈）。
    pub input: chat_input::Draft,
    /// 本草稿已登记的 # 引用（发送时按正文命中注入；标签删除即作废）。
    pub mention_refs: Vec<atelyx_core::ai::refs::RefEntry>,
    /// # 引用候选浮层（None = 收起）。
    pub mention_picker: Option<MentionPicker>,
    /// 同批按压落在输入面内的标志（外点失焦判定，全局按压消息消费）。
    pub press_inside: bool,
    /// 待发送图片托盘（拖入 / 粘贴 / 选择器；发送时落临时区）。
    pub pending_images: Vec<PendingImage>,
    /// 新对话待固化 Agent（发送首条消息时落会话 agentId）。
    pub draft_agent_id: Option<String>,
    /// 面板级 Agent 选择（上次选择，随面板覆盖持久化；新建会话继承）。
    pub agent_override: Option<String>,
    /// 标题重生成转圈动画节拍（UiTick 在命名在途时递增；视图按计数取旋转角）。
    pub spin_tick: u32,
    /// 活跃 run 的会话集合。
    pub streaming: HashSet<String>,
    /// 压缩中的会话。
    pub compacting: Option<String>,
    /// 请求级错误（顶部行展示；X 清除）。
    pub error: Option<String>,
    /// 展开的步骤块（键 = 「消息 id:步骤序号」；思考与工具步共用）。
    pub expanded: HashSet<String>,
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
    /// 投影刷新节流（流式增量事件按间隔取样，避免每 token 全量重建）。
    pub last_refresh: Option<std::time::Instant>,
    /// 回滚二段确认（messageId）。
    pub rollback_confirm: Option<String>,
    /// 插件发起的会话 → 发起插件 id（事件转发路由；换仓 / 脱离时清空）。
    pub plugin_sessions: std::collections::HashMap<String, String>,
    /// 插件对话事件序号（快照 rev，单调递增；插件只比较同会话内的先后）。
    pub plugin_event_seq: u64,
}

impl ChatState {
    pub fn new() -> Self {
        // 会话侧栏默认收起：面板初始为「新对话」态，历史会话由用户主动展开
        Self::default()
    }

    /// 当前生效的 Agent：活跃会话严格取会话值（存量会话未设 agentId 不被面板
    /// 上次选择接管）；无活跃会话时回落新对话草稿 → 面板上次选择。
    pub fn active_agent_id(&self) -> Option<String> {
        if let Some(id) = &self.active {
            if let Some(s) = self.sessions.iter().find(|s| s["id"] == json!(id)) {
                return s.get("agentId").and_then(Value::as_str).map(str::to_string);
            }
        }
        self.draft_agent_id
            .clone()
            .or_else(|| self.agent_override.clone())
    }

    fn session_mut(&mut self, id: &str) -> Option<&mut Value> {
        self.sessions.iter_mut().find(|s| s["id"] == json!(id))
    }

    /// 从 store 拉取单会话最新投影（run 事件 / 提交后调用），并预解析消息正文。
    /// 解析缓存按「本会话 ∪ 当前显示会话」收敛——流式期间正文每拍换哈希，只增不删会让
    /// 长对话把整段历史常驻内存；而后台会话的节流刷新不得清掉正在显示的会话缓存。
    fn refresh_session(&mut self, session_id: &str) {
        let store = store();
        let messages = store.session_messages(session_id);
        match messages {
            Some(messages) => {
                let mut used: HashSet<u64> = HashSet::new();
                for m in &messages {
                    // user 消息显示原文里的引用标签按 mention 胶囊渲染（refs 作候选传入；
                    // 缓存键并入 refs 签名，防同文不同引用串档）
                    let user_mentions = message_mentions(m);
                    let sources = [
                        m.get("displayContent").and_then(Value::as_str),
                        m.get("content").and_then(Value::as_str),
                    ];
                    for src in sources.into_iter().flatten() {
                        if src.is_empty() {
                            continue;
                        }
                        let is_user_display = m["role"] == json!("user")
                            && !user_mentions.is_empty()
                            && m.get("displayContent").and_then(Value::as_str) == Some(src);
                        let (key, parse) = if is_user_display {
                            let sig: Vec<String> =
                                user_mentions.iter().map(|mn| mn.label.clone()).collect();
                            let _ = &sig;
                            let mns = user_mentions.clone();
                            let src = src.to_string();
                            (
                                doc_key(src.as_str(), &sig),
                                Box::new(move || {
                                    crate::markdown_view::parse_document_with_mentions(&src, mns)
                                })
                                    as Box<dyn FnOnce() -> atelyx_core::markdown::MarkdownDocument>,
                            )
                        } else {
                            let src = src.to_string();
                            (
                                source_hash(src.as_str()),
                                Box::new(move || crate::markdown_view::parse_document(&src))
                                    as Box<dyn FnOnce() -> atelyx_core::markdown::MarkdownDocument>,
                            )
                        };
                        used.insert(key);
                        self.docs.entry(key).or_insert_with(parse);
                    }
                    if let Some(steps) = m.get("steps").and_then(Value::as_array) {
                        for st in steps {
                            if st.get("kind").and_then(Value::as_str) == Some("text") {
                                if let Some(text) = st.get("text").and_then(Value::as_str) {
                                    if !text.is_empty() {
                                        let key = source_hash(text);
                                        used.insert(key);
                                        self.docs.entry(key).or_insert_with(|| {
                                            crate::markdown_view::parse_document(text)
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
                if let Some(active) = self.active.clone() {
                    if active != session_id {
                        // 并行 run 的后台刷新：把显示中会话引用的哈希并入保留集
                        if let Some(active_msgs) = store.session_messages(&active) {
                            collect_doc_keys(&active_msgs, &mut used);
                        }
                    }
                }
                if self.docs.len() > used.len() {
                    self.docs.retain(|hash, _| used.contains(hash));
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
    /// Agent 选择（选择固化到当前会话 / 新对话草稿；None = 缺省「对话」）。
    AgentPick(Option<String>),
    /// 自绘输入面事件（文本突变 / 选区 / 焦点 / 发送 / 浮层转交）。
    Input(chat_input::InputEvent),
    /// # 候选浮层内点击某条候选。
    MentionPick(usize),
    /// 工具栏「添加上下文」：光标处插入 `#` 并唤起候选浮层。
    TriggerMention,
    /// 工具栏「上传附件」：系统文件选择器选图片进托盘。
    UploadAttachment,
    /// 文件选择器 / 拖入的文件路径回执（按扩展名过滤图片进托盘）。
    AttachmentsPicked(Vec<std::path::PathBuf>),
    /// 附件字节落盘回执（两段式发送第二段；Err = 附件未能落盘，图片与引用原样归还）。
    SendReady {
        payload: SendPayload,
        images: Vec<PendingImage>,
        result: Result<Vec<serde_json::Value>, String>,
    },
    /// 从托盘移除一张待发送图片（下标）。
    RemoveAttachment(usize),
    /// 外部文件拖入窗口（是否纳用由编排按当前布局是否开着对话面板判定）。
    FileDropped { window: iced::window::Id, path: std::path::PathBuf },
    Send,
    Stop,
    /// 重新生成最后一条回复（截断到最后一条 user 后重跑）。
    Regenerate,
    /// 回到此处（二段确认：首次按下进入确认态，再按执行）。
    Rollback(String),
    RollbackCancel,
    Compact,
    /// 展开 / 收起一个步骤块（思考 / 工具）。
    ToggleStep(String),
    /// 面板级模型选择（None = 清除选择）。
    ModelPicked(Option<(String, String)>),
    /// 面板级推理等级覆盖（None = 清除覆盖，跟随默认不发送）。
    EffortPicked(Option<String>),
    /// 消息流滚动位置回报（y 相对偏移；贴底 = 跟随开启）。
    FlowScrolled(f32),
    /// 会话行悬停态（操作钮显形）。
    RowHover(Option<String>),
    /// 回到底部浮钮。
    ScrollToBottom,
    /// 按压落在盖层侧栏面板内（吞掉事件，防触发底层的「点外部收起」）。
    OverlayPress,
    /// 重新生成当前会话标题（AI 命名，手动触发）。
    RegenTitle,
    /// 清除顶部行的错误提示。
    ClearError,
    /// run 循环事件（core → Elm）。
    Run(RunEvent),
    /// 会话真源装载完成（空间模式在自建线程上装载，回执经控制通道回 Elm）。
    RepoLoaded(u64, Result<(), String>),
    /// Agent 提示词取回（空间模式发送前置步骤）。
    PromptReady(u64, Result<String, String>),
    /// 退出前落盘回执。
    FlushDone(Result<(), String>),
    /// 辅助调用收尾（压缩 / 手动命名完成；携带结果）。
    AidDone(AidKind, String, Option<String>),
    /// 插件请求发起 / 续聊一轮对话（ai.chat.send 的宿主编排）。
    PluginSend {
        request_id: u64,
        plugin_id: String,
        text: String,
        session_id: Option<String>,
    },
    /// 插件请求中断一个会话的活跃 run（ai.chat.stop）。
    PluginStop { request_id: u64, session_id: String },
    /// 插件请求读会话清单或单会话消息（ai.chat.history）。
    PluginHistory {
        request_id: u64,
        session_id: Option<String>,
        limit: Option<u32>,
    },
}

/// 用户发起的对话动作（决定提示词补齐后回到哪个入口续跑）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingChatAction {
    Send,
    Regenerate,
}

/// 辅助调用类别（不算 run）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AidKind {
    Compact,
    AutoRename,
}

// ===== 仓库装载 =====

/// 进仓 / 切仓调用（本地）：装载磁盘真源并刷新投影。
pub fn load_vault(app: &mut App, root: &std::path::Path) {
    prepare_load(app);
    let ticket = store().begin_load();
    match store().load(ticket, atelyx_core::ai::session_io::local_io(root)) {
        Ok(Some(snap)) => apply_snapshot(app, snap),
        // 已被更新的装载取代：真源状态归那次装载，本次不再回填投影
        Ok(None) => {}
        Err(e) => app.chat.error = Some(format!("会话装载失败：{e}")),
    }
}

/// 进协作空间调用：装载空间真源（用户元数据：`chat/messages` 等键）。
/// 空间后端是阻塞 HTTP，装载落在自建线程上（tokio 线程上阻塞会 panic），回执经控制通道回 Elm。
pub fn load_space_sessions(app: &mut App) -> iced::Task<Message> {
    prepare_load(app);
    let Some(session) = crate::space_actions::session_of(app) else {
        app.chat.error = Some("会话装载失败：协作空间会话缺失".to_string());
        return iced::Task::none();
    };
    let Some(token) = app.space_token.clone() else {
        app.chat.error = Some("会话装载失败：协作空间未登录".to_string());
        return iced::Task::none();
    };
    let io = match atelyx_core::ai::session_io::space_io(
        &session.server_url,
        token,
        session.space_id.clone(),
    ) {
        Ok(io) => io,
        Err(e) => {
            app.chat.error = Some(format!("会话装载失败：{e}"));
            return iced::Task::none();
        }
    };
    let gen = app.epoch();
    let ticket = store().begin_load();
    let store = Arc::clone(store());
    let tx = control_channel().0.clone();
    std::thread::spawn(move || {
        let result = store.load(ticket, io).map(|_| ());
        let _ = tx.send(ChatMsg::RepoLoaded(gen, result));
    });
    iced::Task::none()
}

/// 装载前置：停全部在途 run（在途提交随换源失效）、清插件会话归属与上一容器的投影。
fn prepare_load(app: &mut App) {
    hub().stop_all();
    app.chat.plugin_sessions.clear();
    app.chat.session_key = None;
    app.chat.sessions.clear();
    app.chat.active = None;
    app.chat.streaming.clear();
    app.chat.compacting = None;
    app.chat.docs.clear();
    app.chat.delete_confirm = None;
    app.chat.rollback_confirm = None;
    app.chat.pending_action = None;
    app.chat.pending_prompt = None;
    app.chat.model_override = None;
    app.chat.effort = None;
    app.chat.agent_override = None;
}

/// 装载就绪：投影快照 + 活动会话按最近更新回落 + 历史消息解析缓存重建。
fn apply_snapshot(app: &mut App, snap: StoreSnapshot) {
    app.chat.session_key = Some(snap.session_vault_key.clone());
    app.chat.sessions = snap.sessions;
    app.chat.persist_error = snap.persist_error.map(|e| (e.message, e.at));
    app.chat.model_override = snap.model_override.filter(|v| !v.is_null());
    app.chat.effort = snap.effort_override;
    app.chat.agent_override = snap.agent_override;
    // 装载后回到「新对话」态：不自动选中最近会话、不展开会话侧栏——历史会话由用户主动选取
    app.chat.active = None;
    app.chat.sidebar_open = false;
    app.chat.error = None;
    app.chat.streaming.clear();
    app.chat.compacting = None;
    app.chat.docs.clear();
    app.chat.delete_confirm = None;
    app.chat.rollback_confirm = None;
    app.chat.follow_bottom = true;
    app.chat.row_hover = None;
    app.chat.expanded.clear();
    app.chat.session_menu = None;
}

/// 无真源（离开空间 / 未进仓）：清投影，防跨源读写。
pub fn detach_vault(app: &mut App) {
    prepare_load(app);
    // 跨源后上一容器的错误条不再适用
    app.chat.error = None;
    app.chat.persist_error = None;
}

/// 消息集引用的正文源哈希（docs 缓存保留集计算用）。
fn collect_doc_keys(messages: &[Value], keys: &mut HashSet<u64>) {
    for m in messages {
        let sources = [
            m.get("displayContent").and_then(Value::as_str),
            m.get("content").and_then(Value::as_str),
        ];
        for src in sources.into_iter().flatten() {
            if !src.is_empty() {
                keys.insert(source_hash(src));
            }
        }
        if let Some(steps) = m.get("steps").and_then(Value::as_array) {
            for st in steps {
                if st.get("kind").and_then(Value::as_str) == Some("text") {
                    if let Some(text) = st.get("text").and_then(Value::as_str) {
                        if !text.is_empty() {
                            keys.insert(source_hash(text));
                        }
                    }
                }
            }
        }
    }
}

/// 退出前落盘会话真源：写链是防抖的，未 flush 的变更会随进程结束丢失。
/// 只等一轮（成功或失败），失败走提示 + 下次启动的退避重试，退出不无限等待。
pub fn flush_sessions() -> iced::Task<Message> {
    let store = Arc::clone(store());
    iced::Task::perform(async move { store.flush().await }, |result| {
        Message::Chat(ChatMsg::FlushDone(result))
    })
}

// ===== 配置解析 =====

/// 解析 run 配置（供应商路由 / 模型 / Agent 名册）。配置缺失 = Err（面板提示）。
/// `space_prompt`：空间模式下已取回的 Agent 提示词正文（本地模式自取，传 None）。
/// `for_run`：辅助调用（压缩 / 命名）不消费提示词，传 false 跳过提示词解析，
/// 空间模式下带提示词文件的 Agent 不因此报错。
fn resolve_run_config(
    app: &App,
    agent_id: Option<&str>,
    space_prompt: Option<String>,
    for_run: bool,
) -> Result<RunConfig, String> {
    // 模型来源 = 面板选择（唯一来源；未选择由下方报错拒绝）
    let override_pick = app.chat.model_override.as_ref().and_then(|v| {
        let provider_id = v.get("providerId").and_then(Value::as_str)?;
        let model = v.get("model").and_then(Value::as_str)?;
        Some((provider_id.to_string(), model.to_string()))
    });
    // 模型来源 = 面板选择（上次选择随面板覆盖持久化；设置页不再有默认模型）
    let Some((provider_id, model)) = override_pick else {
        return Err("未选择模型：点击输入区下方的模型按钮选择".to_string());
    };
    let Some(provider) = app
        .repo_config
        .providers
        .as_ref()
        .and_then(|ps| ps.iter().find(|p| p.id == provider_id))
    else {
        return Err("所选模型的供应商已失效，请在输入区重新选择".to_string());
    };
    let api_key = match &provider.api_key {
        Some(k) if !k.is_empty() => k.clone(),
        _ => key_scope(app)
            .and_then(|scope| {
                crate::credentials::load_provider_key(&scope, &provider.id).ok().flatten()
            })
            .filter(|k| !k.is_empty())
            .ok_or_else(|| "API key 未配置或密钥链不可用".to_string())?,
    };
    // Agent 名册：缺省预置「对话」（无提示词 + 只读工具）；指定 id 缺失时同样降级
    let agent = app.agents.iter().find(|a| Some(a.id.as_str()) == agent_id);
    let system_prompt = match agent.and_then(|a| a.system_prompt_file.clone()) {
        None => None,
        // 辅助调用不消费提示词：直接跳过解析（空间下该文件需异步预取，这里没有）
        Some(file) if for_run => {
            Some(prompt_text(app, &file, space_prompt)?).filter(|s| !s.trim().is_empty())
        }
        Some(_) => None,
    };
    // 无 Agent = 内置「对话」的工具集（单一维护点在 core builtin_agents）
    let tool_ids = agent.map(|a| a.tools.clone()).unwrap_or_else(|| {
        atelyx_core::vault::builtin_agents()
            .into_iter()
            .next()
            .map(|a| a.tools)
            .unwrap_or_default()
    });
    // # 引用 / 打开文件上下文只发路径，读取引导随名册含 read_file 才追加
    // （模型没有读取能力时注入无意义）
    let system_prompt = system_prompt.map(|mut prompt| {
        if tool_ids.iter().any(|t| t == "read_file") {
            if !prompt.trim().is_empty() {
                prompt.push_str("\n\n");
            }
            prompt.push_str(ai_refs::FILE_REFERENCE_PROMPT);
        }
        prompt
    });
    Ok(RunConfig {
        base_url: provider.base_url.clone(),
        api_key,
        model,
        // 面板级推理等级覆盖（线格式解析失败 = 不发送，与未覆盖同语义）
        reasoning_effort: app
            .chat
            .effort
            .as_deref()
            .and_then(atelyx_core::ai::stream::ReasoningEffort::parse_wire),
        system_prompt,
        tool_ids,
        search: search_config(app),
        exclude: app.repo_config.exclude_folders.clone().unwrap_or_default(),
    })
}

/// Agent 提示词正文：本地经 core 出口读仓库内文件（路径越界被拒）；
/// 空间由调用方先行取回（服务端读，见 `prompt_prefetch`）。
fn prompt_text(app: &App, file: &str, space_prompt: Option<String>) -> Result<String, String> {
    match &app.repo_kind {
        RepoKind::Local => {
            let root = app
                .vault_root
                .as_ref()
                .ok_or_else(|| "仓库未打开".to_string())?;
            atelyx_core::vault::read_note(root, file)
                .map_err(|e| format!("提示词文件读取失败（{file}）：{e}"))
        }
        RepoKind::Space(_) => {
            space_prompt.ok_or_else(|| format!("提示词文件未取到（{file}）"))
        }
    }
}

/// 空间模式下 Agent 提示词文件在服务端：发送 / 重生成前异步取一次。
/// 返回 Some(task) = 已在取（回执 `ChatMsg::PromptReady` 后回到同一入口续跑）。
fn prompt_prefetch(
    app: &mut App,
    action: PendingChatAction,
) -> Option<iced::Task<Message>> {
    let RepoKind::Space(session) = &app.repo_kind else {
        return None;
    };
    let file = app
        .agents
        .iter()
        .find(|a| Some(a.id.as_str()) == app.chat.active_agent_id().as_deref())
        .and_then(|a| a.system_prompt_file.clone())?;
    let gen = app.epoch();
    let client = session.client.clone();
    let space_id = session.space_id.clone();
    app.chat.pending_action = Some(action);
    Some(iced::Task::perform(
        async move {
            client
                .get_file_text(&space_id, &file)
                .await
                .map_err(|e| e.to_string())
        },
        move |result| Message::Chat(ChatMsg::PromptReady(gen, result)),
    ))
}

fn search_config(app: &App) -> SearchConfig {
    let search = app.repo_config.search.clone().unwrap_or_default();
    let tavily_key = if search.provider == "tavily" {
        key_scope(app)
            .and_then(|scope| crate::credentials::load_provider_key(&scope, "tavily").ok().flatten())
    } else {
        None
    };
    SearchConfig {
        provider: if search.provider.is_empty() { None } else { Some(search.provider.clone()) },
        tavily_key,
        searxng_url: Some(search.searxng_url.clone()).filter(|s| !s.is_empty()),
    }
}

/// 密钥链条目 scope：与 AI 设置页同口径（本地 = 仓库根路径；空间 = 「服务器地址/空间 id」）。
/// 空间仓库当前无本地会话真源（发送被前置拦截），该口径保证未来开放时不换锁位。
fn key_scope(app: &App) -> Option<String> {
    match &app.repo_kind {
        crate::RepoKind::Local => {
            app.vault_root.as_ref().map(|r| r.to_string_lossy().into_owned())
        }
        crate::RepoKind::Space(s) => Some(format!("{}/{}", s.server_url, s.space_id)),
    }
}

/// 工具执行环境：按容器身份选仓库访问后端（本地 = 仓库根 + 写盘队列；空间 = 服务端客户端）。
fn tool_ctx(app: &App) -> ToolCtx {
    let repo = match &app.repo_kind {
        RepoKind::Local => RepoAccess::Local {
            root: app.vault_root.clone().unwrap_or_default(),
            queue: app.vault.writes.clone(),
        },
        RepoKind::Space(session) => RepoAccess::Space {
            client: session.client.clone(),
            space_id: session.space_id.clone(),
        },
    };
    ToolCtx {
        repo,
        search: search_config(app),
        exclude: app.repo_config.exclude_folders.clone().unwrap_or_default(),
    }
}

// ===== 更新循环 =====

pub fn update(app: &mut App, msg: ChatMsg) -> iced::Task<Message> {
    let task = update_inner(app, msg);
    // 附件缩略图按需载入（挂接在每次面板消息处理尾部；去重由缓存与在途集合承担）
    ensure_chat_images(app).chain(task)
}

fn update_inner(app: &mut App, msg: ChatMsg) -> iced::Task<Message> {
    match msg {
        ChatMsg::PersistStatus(status) => {
            app.chat.persist_error = status;
            iced::Task::none()
        }
        ChatMsg::ModelPicked(pick) => {
            app.dismiss_popup();
            let value = pick
                .map(|(provider_id, model)| json!({ "providerId": provider_id, "model": model }));
            if store()
                .commit(CommitBatch {
                    overrides: OverridesPatch {
                        model_override: Some(value.clone()),
                        effort_override: None,
                        agent_override: None,
                    },
                    ..Default::default()
                })
                .is_err()
            {
                app.chat.error = Some("模型选择保存失败".to_string());
                return iced::Task::none();
            }
            app.chat.model_override = value;
            iced::Task::none()
        }
        ChatMsg::EffortPicked(pick) => {
            app.dismiss_popup();
            if store()
                .commit(CommitBatch {
                    overrides: OverridesPatch {
                        model_override: None,
                        effort_override: Some(pick.clone()),
                        agent_override: None,
                    },
                    ..Default::default()
                })
                .is_err()
            {
                app.chat.error = Some("推理等级保存失败".to_string());
                return iced::Task::none();
            }
            app.chat.effort = pick;
            iced::Task::none()
        }
        ChatMsg::FlowScrolled(y) => {
            app.chat.follow_bottom = y >= 0.995;
            iced::Task::none()
        }
        ChatMsg::RowHover(id) => {
            app.chat.row_hover = id;
            iced::Task::none()
        }
        ChatMsg::OverlayPress => iced::Task::none(),
        ChatMsg::RegenTitle => regenerate_title(app),
        ChatMsg::ScrollToBottom => {
            app.chat.follow_bottom = true;
            scroll_flow_bottom()
        }
        ChatMsg::ToggleStep(key) => {
            if !app.chat.expanded.remove(&key) {
                app.chat.expanded.insert(key);
            }
            iced::Task::none()
        }
        ChatMsg::ClearError => {
            app.chat.error = None;
            app.chat.persist_error = None;
            iced::Task::none()
        }
        ChatMsg::FlushDone(result) => {
            if let Err(e) = result {
                // 退出前的最后一次落盘失败：提示可见（若窗口已关，至少留下可定位原因）
                eprintln!("会话落盘失败（退出前）：{e}");
                app.notify(crate::ui::notify::Level::Error, format!("会话落盘失败：{e}"));
            }
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
        ChatMsg::AgentPick(id) => {
            app.dismiss_popup();
            // 选择同时固化为面板覆盖（新建会话继承上次选择）
            app.chat.agent_override = id.clone();
            app.chat.draft_agent_id = id.clone();
            if let Some(session_id) = app.chat.active.clone() {
                if let Some(s) = app.chat.session_mut(&session_id) {
                    s["agentId"] = id.clone().map(|v| serde_json::json!(v)).unwrap_or(Value::Null);
                }
                if store()
                    .commit(CommitBatch {
                        metas: vec![MetaPatch {
                            id: session_id,
                            title: None,
                            agent_id: Some(id.clone()),
                            compaction: None,
                            updated_at: None,
                        }],
                        overrides: OverridesPatch {
                            agent_override: Some(id),
                            ..Default::default()
                        },
                        ..Default::default()
                    })
                    .is_err()
                {
                    app.chat.error = Some("Agent 选择保存失败".to_string());
                }
            } else if store()
                .commit(CommitBatch {
                    overrides: OverridesPatch {
                        agent_override: Some(id),
                        ..Default::default()
                    },
                    ..Default::default()
                })
                .is_err()
            {
                app.chat.error = Some("Agent 选择保存失败".to_string());
            }
            iced::Task::none()
        }
        ChatMsg::SessionPicked(id) => {
            app.chat.active = Some(id.clone());
            app.chat.rollback_confirm = None;
            app.chat.error = None;
            // 切换会话即重建该会话的解析缓存（装载后历史消息不退化纯文本）
            app.chat.refresh_session(&id);
            // 切会话直接呈现到最新消息（贴底跟随一并复位）
            app.chat.follow_bottom = true;
            scroll_flow_bottom()
        }
        ChatMsg::NewSession => {
            app.chat.active = None;
            // 新会话继承上次使用的 Agent（面板覆盖；未选过 = 内置「对话」走名册缺省）
            app.chat.draft_agent_id = app.chat.agent_override.clone();
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
            // 附件临时目录随会话清理（本地仓库自管区；空间侧附件随服务端内容）
            if matches!(app.repo_kind, RepoKind::Local) {
                if let Some(root) = app.vault_root.clone() {
                    if let Ok(dir) = atelyx_core::vault::chat_attachment_dir(&root, &id) {
                        if let Err(e) = std::fs::remove_dir_all(&dir) {
                            app.notify(
                                crate::NoticeLevel::Warning,
                                format!("会话附件临时目录清理失败：{e}"),
                            );
                        }
                    }
                }
            }
            iced::Task::none()
        }
        ChatMsg::Input(event) => on_input(app, event),
        ChatMsg::MentionPick(index) => {
            // 点选浮层行不算外点：保持输入面焦点，继续键入接在插入的标签之后
            app.chat.input.focused = true;
            app.chat.press_inside = true;
            commit_mention(app, index);
            iced::Task::none()
        }
        ChatMsg::TriggerMention => {
            trigger_mention(app);
            iced::Task::none()
        }
        ChatMsg::UploadAttachment => upload_attachment(),
        ChatMsg::AttachmentsPicked(paths) => {
            add_attachments(app, paths);
            iced::Task::none()
        }
        ChatMsg::RemoveAttachment(index) => {
            if index < app.chat.pending_images.len() {
                app.chat.pending_images.remove(index);
            }
            iced::Task::none()
        }
        ChatMsg::FileDropped { window, path } => {
            if crate::layout_view::window_has_view(app, "aichat")
                && app.main_id.is_some_and(|main| main == window)
            {
                add_attachments(app, vec![path]);
            }
            iced::Task::none()
        }
        ChatMsg::Stop => {
            if let Some(id) = app.chat.active.clone() {
                hub().stop(&id);
            }
            iced::Task::none()
        }
        ChatMsg::Send => chat_entry(app, PendingChatAction::Send),
        ChatMsg::Regenerate => chat_entry(app, PendingChatAction::Regenerate),
        ChatMsg::SendReady { payload, images, result } => match result {
            Ok(attachments) => finish_send(app, payload, attachments),
            Err(e) => {
                // materialize 失败时图片原样带回（托盘恢复原样）；引用一并归还
                app.chat.pending_images = images;
                app.chat.mention_refs = payload.refs;
                app.notify(crate::NoticeLevel::Warning, format!("图片发送失败：{e}"));
                iced::Task::none()
            }
        },
        ChatMsg::RepoLoaded(gen, result) => {
            if gen != app.epoch() {
                // 装载在途期间换了容器：旧真源快照整体丢弃
                return iced::Task::none();
            }
            match result {
                Ok(()) => apply_snapshot(app, store().snapshot()),
                Err(e) => app.chat.error = Some(format!("会话装载失败：{e}")),
            }
            iced::Task::none()
        }
        ChatMsg::PromptReady(gen, result) => {
            if gen != app.epoch() {
                app.chat.pending_action = None;
                app.chat.pending_prompt = None;
                return iced::Task::none();
            }
            let Some(action) = app.chat.pending_action.take() else {
                return iced::Task::none(); // 已被更早的回执消费（重复请求）
            };
            match result {
                Ok(text) => {
                    app.chat.pending_prompt = Some(text);
                    chat_entry(app, action)
                }
                Err(e) => {
                    app.chat.pending_prompt = None;
                    app.chat.error = Some(format!("Agent 提示词读取失败，已取消本次操作：{e}"));
                    iced::Task::none()
                }
            }
        }
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
        ChatMsg::Run(event) => on_run_event(app, event),
        ChatMsg::PluginSend { request_id, plugin_id, text, session_id } => {
            plugin_chat_send(app, request_id, &plugin_id, text, session_id)
        }
        ChatMsg::PluginStop { request_id, session_id } => {
            hub().stop(&session_id);
            app.plugins.resolve_chat_call(request_id, Ok(json!({})));
            iced::Task::none()
        }
        ChatMsg::PluginHistory { request_id, session_id, limit } => {
            let result = plugin_history(&session_id, limit);
            app.plugins.resolve_chat_call(request_id, result);
            iced::Task::none()
        }
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
                AidKind::AutoRename => {
                    app.chat.renaming.remove(&session_id);
                    if let Some(title) = result.clone() {
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
            if kind == AidKind::AutoRename {
                // 命名结果对发起会话的插件补一发 Titled（无映射则无操作）
                let title = app
                    .chat
                    .sessions
                    .iter()
                    .find(|s| s["id"] == json!(session_id))
                    .and_then(|s| s.get("title").and_then(Value::as_str).map(str::to_string));
                notify_plugin_chat(
                    app,
                    &session_id,
                    String::new(),
                    ChatEvent::Titled { title },
                );
            }
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

/// 文档缓存键（并入引用签名：同文不同引用的 mention 解析结果不同，防串档）。
pub(crate) fn doc_key(source: &str, mention_sig: &[String]) -> u64 {
    source_hash(&format!("{source}\u{1}{}", mention_sig.join("\u{1}")))
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

/// 发送 / 重生成共用入口：空间模式先取 Agent 提示词文件，取回后回到本入口执行本体。
fn chat_entry(app: &mut App, action: PendingChatAction) -> iced::Task<Message> {
    if app.chat.pending_prompt.is_none() {
        if let Some(task) = prompt_prefetch(app, action) {
            return task;
        }
    }
    let prompt = app.chat.pending_prompt.take();
    app.chat.pending_action = None;
    match action {
        PendingChatAction::Send => send_now(app, prompt),
        PendingChatAction::Regenerate => regenerate_now(app, prompt),
    }
}

fn regenerate_now(app: &mut App, space_prompt: Option<String>) -> iced::Task<Message> {
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
    let config = match resolve_run_config(app, session.get("agentId").and_then(Value::as_str), space_prompt, true) {
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
    // 末条 user 的注入块按当下状态重拼：引用块按 displayContent + refs 重扫
    //（标签删除 = 作废），打开文件块按当前窗口状态重写；块随请求内联，不改写磁盘消息。
    let mut history = history;
    if let Some(last) = history.last_mut() {
        if last["role"] == json!("user") {
            rebuild_user_injection(app, last);
        }
    }
    app.chat.refresh_session(&session_id);
    start_run(config, session_id, compaction, history, app)
}

/// 重建一条 user 消息的注入内容（重生成路径）：无 refs 的旧消息原样保留；
/// 有 refs 的按 displayContent 重扫命中，重拼引用路径块与打开文件块。
fn rebuild_user_injection(app: &App, message: &mut Value) {
    let refs = message_refs(message);
    if refs.is_empty() {
        return;
    }
    let display = message
        .get("displayContent")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| message.get("content").and_then(Value::as_str).unwrap_or(""))
        .to_string();
    let hits = ai_refs::scan_hits(&display, &refs);
    let mut content = ai_refs::reference_path_block(&hits);
    content.push_str(&display);
    if let Some(block) = ai_refs::open_files_context_block(&open_files_context(app)) {
        content.push_str("\n\n");
        content.push_str(&block);
    }
    message["content"] = json!(content);
}

/// 消息的引用标签签名（缓存键与解析候选同源：前缀：标签，与正文逐字一致）。
pub(crate) fn mention_sig(message: &Value) -> Vec<String> {
    message_mentions(message).into_iter().map(|mn| mn.label).collect()
}

/// 消息 refs → mention 候选（标签文本 = 前缀：标签，与正文中的引用标签逐字一致）。
pub(crate) fn message_mentions(message: &Value) -> Vec<atelyx_core::markdown::MentionCandidate> {
    message_refs(message)
        .iter()
        .map(|r| atelyx_core::markdown::MentionCandidate {
            key: r.path.clone(),
            label: format!("{}：{}", r.kind.prefix(), r.label),
        })
        .collect()
}

/// 消息 refs 字段 → 内存引用条目（畸形条目跳过）。
pub(crate) fn message_refs(message: &Value) -> Vec<ai_refs::RefEntry> {
    message
        .get("refs")
        .and_then(Value::as_array)
        .map(|refs| {
            refs.iter()
                .filter_map(|r| {
                    let path = r.get("path").and_then(Value::as_str)?;
                    let label = r.get("label").and_then(Value::as_str)?;
                    let kind = ai_refs::RefKind::parse_key(
                        r.get("refKind").and_then(Value::as_str).unwrap_or(""),
                    )?;
                    Some(ai_refs::RefEntry {
                        path: path.to_string(),
                        label: label.to_string(),
                        kind,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn send_now(app: &mut App, space_prompt: Option<String>) -> iced::Task<Message> {
    let display = app.chat.input.text.trim().to_string();
    let images = std::mem::take(&mut app.chat.pending_images);
    let refs = std::mem::take(&mut app.chat.mention_refs);
    // 任何早退路径都要把托盘与引用原样归还（半发状态不可接受）
    macro_rules! restore {
        () => {{
            app.chat.pending_images = images;
            app.chat.mention_refs = refs;
        }};
    }
    if (display.is_empty() && images.is_empty()) || app.chat.compacting.is_some() {
        restore!();
        return iced::Task::none();
    }
    if let Err(e) = check_ready(app) {
        app.chat.error = Some(e);
        restore!();
        return iced::Task::none();
    }
    let config = match resolve_run_config(app, app.chat.active_agent_id().as_deref(), space_prompt, true) {
        Ok(c) => c,
        Err(e) => {
            app.chat.error = Some(e);
            restore!();
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
                restore!();
                return iced::Task::none();
            }
            app.chat.active = Some(id.clone());
            app.chat.draft_agent_id = None;
            id
        }
    };
    if app.chat.streaming.contains(&session_id) || hub().is_running(&session_id) {
        restore!();
        return iced::Task::none(); // run 不变量：同会话单 run（以 hub 注册表为准）
    }
    // 附件字节先落仓库临时区（两段式：回执后落消息并启动 run；落盘失败不产生消息）
    let backend = match image_backend(app) {
        Ok(backend) => backend,
        Err(e) => {
            app.chat.error = Some(e);
            restore!();
            return iced::Task::none();
        }
    };
    let payload = SendPayload { session_id, config, display, refs };
    let sid = payload.session_id.clone();
    iced::Task::perform(
        materialize_images(backend, sid, images),
        move |result| match result {
            Ok(attachments) => Message::Chat(ChatMsg::SendReady {
                payload: payload.clone(),
                images: Vec::new(),
                result: Ok(attachments),
            }),
            Err((images, e)) => Message::Chat(ChatMsg::SendReady {
                payload: payload.clone(),
                images,
                result: Err(e),
            }),
        },
    )
}

/// 两段式发送的续跑包（附件落盘回执后消费）。
#[derive(Debug, Clone)]
pub struct SendPayload {
    pub session_id: String,
    pub config: RunConfig,
    /// 用户原始输入（trim 后；含 # 标签文本）。
    pub display: String,
    /// 本草稿登记的引用（发送时按正文命中筛选）。
    pub refs: Vec<ai_refs::RefEntry>,
}

/// 附件落盘后端（本地写盘队列 / 空间端点）。
enum ImageBackend {
    Local {
        root: std::path::PathBuf,
        queue: atelyx_core::write_queue::WriteQueue,
    },
    Space {
        client: Arc<atelyx_core::space_client::SpaceClient>,
        space_id: String,
    },
}

fn image_backend(app: &App) -> Result<ImageBackend, String> {
    match &app.repo_kind {
        RepoKind::Space(_) => {
            let session =
                crate::space_actions::session_of(app).ok_or_else(|| "协作空间未连接".to_string())?;
            if !session.role.can_write() {
                return Err("该空间为只读身份，无法发送图片".to_string());
            }
            Ok(ImageBackend::Space { client: session.client, space_id: session.space_id })
        }
        RepoKind::Local => {
            let root = app.vault_root.clone().ok_or_else(|| "仓库未打开".to_string())?;
            Ok(ImageBackend::Local { root, queue: app.vault.writes.clone() })
        }
    }
}

/// 托盘图片逐张落临时区（`img-<nanoid>.<ext>` 每次新文件名，重发不覆盖），
/// 返回消息 attachments 元数据（路径引用；字节不进消息）。
/// 任一张失败 = 已处理图片与余量整体原样带回（托盘恢复原样，不产生半发消息）。
async fn materialize_images(
    backend: ImageBackend,
    session_id: String,
    images: Vec<PendingImage>,
) -> Result<Vec<Value>, (Vec<PendingImage>, String)> {
    let mut attachments = Vec::new();
    let mut pending: std::collections::VecDeque<PendingImage> = images.into_iter().collect();
    while let Some(img) = pending.pop_front() {
        let rel = match atelyx_core::vault::chat_image_rel_path(&session_id, &img.file_name) {
            Ok(rel) => rel,
            Err(e) => return Err((pending.into_iter().collect(), e)),
        };
        let write = match &backend {
            ImageBackend::Local { root, queue } => {
                let path = match atelyx_core::vault::safe_join(root, &rel, true) {
                    Ok(path) => path,
                    Err(e) => return Err((pending.into_iter().collect(), e)),
                };
                queue.write_bytes(path, img.bytes.clone()).await
            }
            ImageBackend::Space { client, space_id } => client
                .put_file_bytes(space_id, &rel, &img.bytes)
                .await
                .map_err(|e| e.to_string()),
        };
        if let Err(e) = write {
            let mut restore: Vec<PendingImage> = pending.into_iter().collect();
            restore.insert(0, img);
            return Err((restore, format!("图片落盘失败（{rel}）：{e}")));
        }
        attachments.push(json!({ "kind": "image", "mime": img.mime, "file": rel }));
    }
    Ok(attachments)
}

/// 附件落盘回执：成功 = 注入块落消息并启动 run；失败 = 引用原样归还并通知
/// （输入草稿本就未清；落盘成功的临时文件随会话删除回收）。
fn finish_send(app: &mut App, payload: SendPayload, attachments: Vec<Value>) -> iced::Task<Message> {
    let SendPayload { session_id, config, display, refs } = payload;
    macro_rules! restore {
        () => {{
            app.chat.mention_refs = refs;
        }};
    }
    if app.chat.streaming.contains(&session_id) || hub().is_running(&session_id) {
        restore!();
        app.notify(crate::NoticeLevel::Warning, "该会话已有对话进行中，消息未发送");
        return iced::Task::none(); // run 不变量：同会话单 run
    }
    // 历史快照先于追加（user 消息由 run 入参携带；commit 后快照已含新消息，会重复）
    let mut history = store().session_messages(&session_id).unwrap_or_default();
    // 引用命中（标签被手动删掉 = 该引用作废）→ 路径块 + 打开的文件块随消息持久化
    let hits = ai_refs::scan_hits(&display, &refs);
    let mut content = ai_refs::reference_path_block(&hits);
    content.push_str(&display);
    if let Some(block) = ai_refs::open_files_context_block(&open_files_context(app)) {
        content.push_str("\n\n");
        content.push_str(&block);
    }
    let refs_json: Vec<Value> = refs
        .iter()
        .filter(|r| hits.iter().any(|h| h.path == r.path))
        .map(|r| {
            json!({ "kind": "file", "path": r.path, "label": r.label, "refKind": r.kind.key() })
        })
        .collect();
    let user_message = json!({
        "id": uuid_v4(),
        "role": "user",
        "content": content,
        "displayContent": display,
        "refs": refs_json,
        "attachments": attachments,
        "createdAt": now_ms(),
    });
    if let Err(e) = store().commit(CommitBatch {
        appends: vec![AppendBatch {
            session_id: session_id.clone(),
            messages: vec![user_message.clone()],
        }],
        ..Default::default()
    }) {
        restore!();
        app.chat.error = Some(format!("消息写入失败：{e}"));
        return iced::Task::none();
    }
    app.chat.input.clear();
    app.chat.mention_picker = None;
    app.chat.refresh_session(&session_id);
    // 发送即回底跟随（run 事件链会持续把新内容带到视野）
    app.chat.follow_bottom = true;
    let compaction = app
        .chat
        .session_mut(&session_id)
        .and_then(|s| s.get("compaction").cloned())
        .filter(|c| !c.is_null());
    history.push(user_message);
    start_run(config, session_id, compaction, history, app).chain(scroll_flow_bottom())
}

/// 当前打开的文件（布局模型笔记 / 表格 / 画布三槽位 = 全部窗口正在查看的文件）。
fn open_files_context(app: &App) -> Vec<ai_refs::OpenFileCtx> {
    let mut out: Vec<ai_refs::OpenFileCtx> = Vec::new();
    for slot in [&app.ui.last_note_file, &app.ui.last_table_file, &app.ui.last_canvas_file] {
        if let Some(path) = slot {
            if !out.iter().any(|f| &f.path == path) {
                out.push(ai_refs::OpenFileCtx { path: path.clone(), viewing: true });
            }
        }
    }
    out
}

/// 会话消息附件缩略图的按需载入：显示中会话的图片附件缺句柄且未在途/未失败时发起读盘
/// （本地读盘 / 空间端点；缓存与在途集合与笔记图片共用，键 = 仓库相对路径）。
fn ensure_chat_images(app: &mut App) -> iced::Task<Message> {
    let Some(active) = app.chat.active.clone() else {
        return iced::Task::none();
    };
    // 只读借用收集路径（避免整份消息数组深拷贝）
    let mut srcs = Vec::new();
    if let Some(messages) = app
        .chat
        .sessions
        .iter()
        .find(|s| s["id"] == json!(active))
        .and_then(|s| s.get("messages").and_then(Value::as_array))
    {
        for m in messages {
            for a in m.get("attachments").and_then(Value::as_array).into_iter().flatten() {
                if a.get("kind").and_then(Value::as_str) == Some("image") {
                    if let Some(rel) = a.get("file").and_then(Value::as_str) {
                        srcs.push(rel.to_string());
                    }
                }
            }
        }
    }
    if srcs.is_empty() {
        return iced::Task::none();
    }
    let gen = app.epoch();
    let root = app.vault.root().ok();
    let mut tasks = Vec::new();
    for src in srcs {
        if app.note_images.contains_key(&src)
            || app.note_images_failed.contains(&src)
            || !app.note_images_inflight.insert(src.clone())
        {
            continue;
        }
        let task = match (&root, matches!(app.repo_kind, RepoKind::Space(_))) {
            (None, false) => {
                app.note_images_inflight.remove(&src);
                continue;
            }
            (_, true) => crate::space_actions::space_load_image(app, gen, src.clone()),
            (Some(root), false) => {
                let root = root.clone();
                let path = src.clone();
                iced::Task::perform(
                    async move { atelyx_core::vault::read_file_bytes(&root, &path) },
                    move |r| Message::ImageLoaded(gen, src.clone(), r),
                )
            }
        };
        tasks.push(task);
    }
    iced::Task::batch(tasks)
}

// ===== 输入面与 # 引用 =====

/// # 引用候选浮层状态。
pub struct MentionPicker {
    /// 触发 `#` 的字节位置。
    pub at: usize,
    /// 触发位到光标之间的查询串。
    pub query: String,
    /// 键盘选中行。
    pub selected: usize,
    /// 过滤后的候选（仓库相对路径 + 是否目录）。
    pub candidates: Vec<(String, bool)>,
}

/// 候选条数上限（超出只显示前 N 条，继续键入收窄）。
const MENTION_CANDIDATE_CAP: usize = 50;

/// 自绘输入面事件落地：草稿突变、选区、焦点、发送与浮层转交。
fn on_input(app: &mut App, event: chat_input::InputEvent) -> iced::Task<Message> {
    match event {
        chat_input::InputEvent::Focus => {
            app.chat.input.focused = true;
            // 同批全局按压消息据此判定「按压在输入面内」
            app.chat.press_inside = true;
        }
        chat_input::InputEvent::Edited { text, caret, anchor } => {
            app.chat.input.apply_edited(text, caret, anchor);
            sync_mention_picker(app);
        }
        chat_input::InputEvent::Select { caret, anchor } => {
            app.chat.input.caret = caret;
            app.chat.input.anchor = anchor;
            sync_mention_picker(app);
        }
        chat_input::InputEvent::Preedit(preedit) => {
            app.chat.input.preedit = preedit;
        }
        chat_input::InputEvent::Send => return chat_entry(app, PendingChatAction::Send),
        chat_input::InputEvent::Undo => {
            app.chat.input.undo_step();
            sync_mention_picker(app);
        }
        chat_input::InputEvent::Redo => {
            app.chat.input.redo_step();
            sync_mention_picker(app);
        }
        chat_input::InputEvent::PickerNav(delta) => {
            if let Some(p) = &mut app.chat.mention_picker {
                let len = p.candidates.len();
                if len > 0 {
                    let next = p.selected as i32 + delta;
                    p.selected = next.rem_euclid(len as i32) as usize;
                }
            }
        }
        chat_input::InputEvent::PickerCommit => {
            let selected = app.chat.mention_picker.as_ref().map_or(0, |p| p.selected);
            commit_mention(app, selected);
        }
        chat_input::InputEvent::PickerClose => app.chat.mention_picker = None,
    }
    iced::Task::none()
}

/// 候选浮层状态机：光标前的 `#` 触发段激活 = 开启（或刷新查询）；退出触发段 = 关闭。
fn sync_mention_picker(app: &mut App) {
    let draft = &app.chat.input;
    let Some(at) = draft.active_mention_at() else {
        app.chat.mention_picker = None;
        return;
    };
    let caret = draft.caret.min(draft.text.len());
    let query = draft.text[at + 1..caret].to_string();
    // 拍下触发位与查询后再取可变借用（候选收集要读文件树缓存）
    let same_trigger = app
        .chat
        .mention_picker
        .as_ref()
        .is_some_and(|p| p.at == at);
    if !same_trigger {
        let candidates = mention_candidates(app, &query);
        app.chat.mention_picker = (!candidates.is_empty())
            .then_some(MentionPicker { at, query, selected: 0, candidates });
        return;
    }
    let stale = app
        .chat
        .mention_picker
        .as_ref()
        .is_some_and(|p| p.query != query);
    if stale {
        let candidates = mention_candidates(app, &query);
        if let Some(p) = app.chat.mention_picker.as_mut() {
            p.query = query;
            p.candidates = candidates;
            p.selected = 0;
        }
    }
    if app
        .chat
        .mention_picker
        .as_ref()
        .is_some_and(|p| p.candidates.is_empty())
    {
        app.chat.mention_picker = None;
    }
}

/// 从文件树缓存收集引用候选（md / atb / atlx 文件与目录；隐藏与排除目录不在
/// 缓存内自然缺席），按查询子串过滤（不区分大小写），文件树序（同级目录在前）。
fn mention_candidates(app: &App, query: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    collect_candidates(app, "", query, &mut out);
    out
}

fn collect_candidates(app: &App, dir: &str, query: &str, out: &mut Vec<(String, bool)>) {
    if out.len() >= MENTION_CANDIDATE_CAP {
        return;
    }
    let mut children = app.children.get(dir).cloned().unwrap_or_default();
    atelyx_core::vault::sort_children(&mut children, &app.file_sort);
    for child in children {
        let hit = query.is_empty()
            || child.rel.to_lowercase().contains(&query.to_lowercase());
        if hit {
            out.push((child.rel.clone(), child.is_dir));
            if out.len() >= MENTION_CANDIDATE_CAP {
                return;
            }
        }
        if child.is_dir {
            collect_candidates(app, &child.rel, query, out);
        }
    }
}

/// 候选落地：触发段替换为标签文本（尾随空格），路径登记进本草稿引用（同路径只记一条）。
fn commit_mention(app: &mut App, index: usize) {
    let Some(picker) = app.chat.mention_picker.as_ref() else {
        return;
    };
    let Some((rel, is_dir)) = picker.candidates.get(index).cloned() else {
        return;
    };
    let kind = if is_dir {
        ai_refs::RefKind::Dir
    } else {
        match ai_refs::kind_of_file(&rel) {
            Some(k) => k,
            None => {
                app.chat.mention_picker = None;
                return;
            }
        }
    };
    let at = picker.at;
    let taken: Vec<(&str, &str)> = app
        .chat
        .mention_refs
        .iter()
        .map(|r| (r.path.as_str(), r.label.as_str()))
        .collect();
    let label = ai_refs::make_label(&rel, &taken);
    let tag = format!("#{}：{} ", kind.prefix(), label);
    let text = app.chat.input.text.clone();
    let caret = app.chat.input.caret.min(text.len());
    let to = caret.max(at + 1).min(text.len());
    let mut next = String::with_capacity(text.len() - (to - at) + tag.len());
    next.push_str(&text[..at]);
    next.push_str(&tag);
    next.push_str(&text[to..]);
    let caret_after = at + tag.len();
    if !app.chat.mention_refs.iter().any(|r| r.path == rel) {
        app.chat.mention_refs.push(ai_refs::RefEntry { path: rel, label, kind });
    }
    app.chat.input.apply_edited(next, caret_after, caret_after);
    app.chat.mention_picker = None;
}

/// 工具栏「添加上下文」：光标处插入 `#` 并唤起候选浮层（与键入同状态）。
fn trigger_mention(app: &mut App) {
    let draft = &mut app.chat.input;
    draft.focused = true;
    let (a, b) = draft.selection();
    let mut text = String::with_capacity(draft.text.len() - (b - a) + 1);
    text.push_str(&draft.text[..a]);
    text.push('#');
    text.push_str(&draft.text[b..]);
    let caret_after = a + 1;
    draft.apply_edited(text, caret_after, caret_after);
    sync_mention_picker(app);
}

/// 工具栏「上传附件」：系统文件选择器（图片过滤），选中文件进待发送托盘。
fn upload_attachment() -> iced::Task<Message> {
    iced::Task::future(async move {
        let files = rfd::AsyncFileDialog::new()
            .add_filter("图片", &["png", "jpg", "jpeg", "webp", "gif"])
            .pick_files()
            .await;
        Message::Chat(ChatMsg::AttachmentsPicked(
            files
                .map(|fs| fs.into_iter().map(|f| f.path().to_path_buf()).collect())
                .unwrap_or_default(),
        ))
    })
}

/// 附件拖入 / 选择器回执：按扩展名过滤图片入托盘，其余提示不支持。
fn add_attachments(app: &mut App, paths: Vec<std::path::PathBuf>) {
    for path in paths {
        let Some(name) = path.file_name().and_then(|n| n.to_str()).map(str::to_string) else {
            continue;
        };
        let Some(ext) = atelyx_core::vault::image_ext_of(&name) else {
            app.notify(
                crate::NoticeLevel::Warning,
                format!("暂不支持发送该类型文件：{name}（仅图片）"),
            );
            continue;
        };
        match std::fs::read(&path) {
            Ok(bytes) => app.chat.pending_images.push(PendingImage {
                file_name: name,
                bytes,
                mime: image_mime(ext),
            }),
            Err(e) => app.notify(
                crate::NoticeLevel::Warning,
                format!("图片读取失败：{}（{e}）", path.display()),
            ),
        }
    }
}

/// 扩展名 → MIME（托盘四类图片）。
fn image_mime(ext: &str) -> String {
    match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        _ => "image/gif",
    }
    .to_string()
}

/// 待发送图片附件（托盘运行时项；发送时字节落仓库临时区，消息只持久化路径引用）。
#[derive(Debug, Clone)]
pub struct PendingImage {
    pub file_name: String,
    pub bytes: Vec<u8>,
    pub mime: String,
}

/// 发送前置检查（会话真源已装载）。
fn check_ready(app: &App) -> Result<(), String> {
    if app.chat.session_key.is_none() {
        return Err("会话未装载：请先打开仓库或进入协作空间".to_string());
    }
    if matches!(app.repo_kind, RepoKind::Local) && app.vault_root.is_none() {
        return Err("仓库未打开".to_string());
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
    let config = match resolve_run_config(app, app.chat.active_agent_id().as_deref(), None, false) {
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
            LlmMessage::User { parts, .. } => {
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
        let Some(client) = agent::aid_client(&config.base_url) else {
            return Message::Chat(ChatMsg::AidDone(AidKind::Compact, session_id, None));
        };
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
    // 贴底跟随只对当前显示的会话生效（并行 run 不打扰别的会话视图）
    let follow =
        app.chat.follow_bottom && app.chat.active.as_deref() == Some(session_id.as_str());
    let tail = |follow: bool| {
        if follow {
            scroll_flow_bottom()
        } else {
            iced::Task::none()
        }
    };
    let forced = !matches!(
        event,
        RunEvent::TextDelta { .. } | RunEvent::ReasoningDelta { .. } | RunEvent::ToolStart { .. }
    );
    let due = app
        .chat
        .last_refresh
        .map(|t| t.elapsed() >= std::time::Duration::from_millis(150))
        .unwrap_or(true);
    // 插件会话事件转发：终态强制，流式拍与面板刷新同步节流。
    // 须先于事件应用：Finished 臂可能因命名任务提前 return，后置会吞掉终态转发。
    forward_plugin_event(app, &event, forced || due);
    match &event {
        RunEvent::Started { .. } => {
            app.chat.streaming.insert(session_id.clone());
            app.chat.error = None;
            app.chat.refresh_session(&session_id);
            tail(follow)
        }
        RunEvent::TextDelta { .. } | RunEvent::ReasoningDelta { .. } | RunEvent::ToolStart { .. } => {
            if due {
                app.chat.last_refresh = Some(std::time::Instant::now());
                app.chat.refresh_session(&session_id);
            }
            tail(follow)
        }
        RunEvent::ToolEnd { .. } => {
            app.chat.refresh_session(&session_id);
            tail(follow)
        }
        RunEvent::Finished { .. } => {
            app.chat.streaming.remove(&session_id);
            app.chat.last_refresh = None;
            app.chat.refresh_session(&session_id);
            // 话题自动命名：新会话无标题且命名模式开启时触发（独立轻量调用，不算 run）
            if let Some(task) = maybe_auto_name(app, &session_id) {
                return task.chain(tail(follow));
            }
            tail(follow)
        }
        RunEvent::Failed { error, .. } => {
            app.chat.streaming.remove(&session_id);
            if !error.is_empty() {
                app.chat.error = Some(error.clone());
            }
            app.chat.refresh_session(&session_id);
            tail(follow)
        }
    }
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
    // 自动命名只补空标题；已有标题走面板的「重新生成标题」
    if session.get("title").and_then(Value::as_str).is_some() {
        return None;
    }
    let messages = session.get("messages").and_then(Value::as_array)?.clone();
    if !messages.iter().any(|m| m["role"] == json!("user"))
        || !messages.iter().any(|m| m["role"] == json!("assistant"))
    {
        return None;
    }
    spawn_title_task(app, session_id, &messages)
}

/// 命名在途的转圈动画节拍（UiTick 递增；视图按计数取旋转角）。
pub fn on_ui_tick(app: &mut App) {
    if !app.chat.renaming.is_empty() {
        app.chat.spin_tick = app.chat.spin_tick.wrapping_add(1);
    }
}

/// 手动重新生成当前会话标题（无视命名模式开关；命名路由与自动命名一致）。
fn regenerate_title(app: &mut App) -> iced::Task<Message> {
    let Some(session_id) = app.chat.active.clone() else {
        return iced::Task::none();
    };
    if app.chat.streaming.contains(&session_id) || app.chat.renaming.contains(&session_id) {
        return iced::Task::none();
    }
    let Some(session) = app.chat.sessions.iter().find(|s| s["id"] == json!(session_id)) else {
        return iced::Task::none();
    };
    let Some(messages) = session.get("messages").and_then(Value::as_array).cloned() else {
        return iced::Task::none();
    };
    if !messages.iter().any(|m| m["role"] == json!("user"))
        || !messages.iter().any(|m| m["role"] == json!("assistant"))
    {
        return iced::Task::none();
    }
    spawn_title_task(app, &session_id, &messages).unwrap_or_else(|| {
        app.chat.error = Some("标题生成不可用：未选择模型或命名模型未配置".to_string());
        iced::Task::none()
    })
}

/// 发布命名任务：命名路由 = 设置指定模型 > 面板当前模型；发起即登记 renaming（回执按会话摘除）。
fn spawn_title_task(
    app: &mut App,
    session_id: &str,
    messages: &[Value],
) -> Option<iced::Task<Message>> {
    use crate::settings_view::ai::NamingMode;
    let session = app.chat.sessions.iter().find(|s| s["id"] == json!(session_id))?;
    let agent_ref = session.get("agentId").and_then(Value::as_str).map(str::to_string);
    // 命名路由：设置指定模型优先（不依赖面板选择）；跟随所选 = 面板当前生效模型
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
                _ => key_scope(app)
                    .and_then(|scope| {
                        crate::credentials::load_provider_key(&scope, &provider.id).ok().flatten()
                    })
                    .filter(|k| !k.is_empty())?,
            };
            (provider.base_url.clone(), api_key, pick.model)
        }
        _ => {
            let default = resolve_run_config(app, agent_ref.as_deref(), None, false).ok()?;
            (default.base_url.clone(), default.api_key.clone(), default.model.clone())
        }
    };
    let excerpt = transcript_excerpt(messages);
    app.chat.renaming.insert(session_id.to_string());
    let sid = session_id.to_string();
    Some(iced::Task::future(async move {
        let Some(client) = agent::aid_client(&base_url) else {
            return Message::Chat(ChatMsg::AidDone(AidKind::AutoRename, sid, None));
        };
        let title = agent::auto_title(&client, &base_url, &api_key, &model, &excerpt).await;
        Message::Chat(ChatMsg::AidDone(AidKind::AutoRename, sid, title))
    }))
}

// ===== 插件 AI 契约编排（ai.chat.* 宿主侧） =====

/// 插件发起 / 续聊一轮对话：配置与就绪检查 → 会话就位（指定续聊须存在）→ user 消息落真源
/// → 起 run（回执经宿主请求通道异步送达插件；事件由 [`forward_plugin_event`] 流达）。
fn plugin_chat_send(
    app: &mut App,
    request_id: u64,
    plugin_id: &str,
    text: String,
    session_id: Option<String>,
) -> iced::Task<Message> {
    let deny = |app: &App, request_id: u64, reason: String| {
        app.plugins.resolve_chat_call(request_id, Err(reason));
    };
    let text = text.trim().to_string();
    if text.is_empty() {
        deny(app, request_id, "消息内容为空".to_string());
        return iced::Task::none();
    }
    if let Err(e) = check_ready(app) {
        deny(app, request_id, e);
        return iced::Task::none();
    }
    // 插件会话不指定 Agent 与 key：模型跟随面板当前选择（唯一模型来源），未选择即拒绝并回传原因
    let config = match resolve_run_config(app, None, None, false) {
        Ok(config) => config,
        Err(e) => {
            deny(app, request_id, e);
            return iced::Task::none();
        }
    };
    let session_id = match session_id {
        Some(id) => {
            if store().session_meta(&id).is_none() {
                deny(app, request_id, format!("会话 {id} 不存在"));
                return iced::Task::none();
            }
            id
        }
        None => {
            let id = uuid_v4();
            if store()
                .commit(CommitBatch {
                    created: vec![SessionDef {
                        id: id.clone(),
                        file: atelyx_core::ai::session::chat_message_file(&id),
                        title: None,
                        agent_id: None,
                        compaction: None,
                        created_at: now_ms(),
                        updated_at: now_ms(),
                        messages: Vec::new(),
                    }],
                    ..Default::default()
                })
                .is_err()
            {
                deny(app, request_id, "会话创建失败".to_string());
                return iced::Task::none();
            }
            id
        }
    };
    if app.chat.compacting.as_deref() == Some(session_id.as_str()) {
        deny(app, request_id, "该会话正在压缩上下文".to_string());
        return iced::Task::none();
    }
    if hub().is_running(&session_id) {
        deny(app, request_id, "该会话已有进行中的对话".to_string());
        return iced::Task::none();
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
        deny(app, request_id, format!("消息写入失败：{e}"));
        return iced::Task::none();
    }
    app.chat
        .plugin_sessions
        .insert(session_id.clone(), plugin_id.to_string());
    app.chat.refresh_session(&session_id);
    let compaction = app
        .chat
        .session_mut(&session_id)
        .and_then(|s| s.get("compaction").cloned())
        .filter(|c| !c.is_null());
    history.push(user_message);
    let ctx = tool_ctx(app);
    let store = Arc::clone(store());
    let hub = Arc::clone(hub());
    let events = run_event_channel().0.clone();
    let runtime = app.plugins.clone();
    iced::Task::future(async move {
        let result = match hub
            .start(store, events, session_id.clone(), compaction, config, history, ctx)
            .await
        {
            Ok(message_id) => Ok(json!({ "sessionId": session_id, "messageId": message_id })),
            Err(e) => Err(e),
        };
        runtime.resolve_chat_call(request_id, result);
        Message::Noop
    })
}

/// 插件读历史：无参 = 会话清单（按最近更新排序）；带参 = 单会话（limit 取末尾 N 条）。
fn plugin_history(session_id: &Option<String>, limit: Option<u32>) -> Result<Value, String> {
    let store = store();
    match session_id {
        Some(id) => {
            let meta = store
                .session_meta(id)
                .ok_or_else(|| format!("会话 {id} 不存在"))?;
            let mut messages = store.session_messages(id).unwrap_or_default();
            if let Some(limit) = limit {
                let limit = limit as usize;
                if messages.len() > limit {
                    messages = messages.split_off(messages.len() - limit);
                }
            }
            let wire = ChatSessionWire {
                id: id.clone(),
                title: meta.get("title").and_then(Value::as_str).map(str::to_string),
                created_at: meta.get("createdAt").and_then(Value::as_i64).unwrap_or(0),
                updated_at: meta.get("updatedAt").and_then(Value::as_i64).unwrap_or(0),
                messages,
            };
            serde_json::to_value(wire).map_err(|e| format!("会话出线序列化失败：{e}"))
        }
        None => {
            let mut sessions: Vec<ChatSessionSummary> = store
                .snapshot()
                .sessions
                .iter()
                .map(|s| ChatSessionSummary {
                    id: s.get("id").and_then(Value::as_str).unwrap_or_default().to_string(),
                    title: s.get("title").and_then(Value::as_str).map(str::to_string),
                    updated_at: s.get("updatedAt").and_then(Value::as_i64).unwrap_or(0),
                    message_count: s
                        .get("messages")
                        .and_then(Value::as_array)
                        .map(|ms| ms.len())
                        .unwrap_or(0) as u64,
                })
                .collect();
            sessions.sort_by_key(|s| std::cmp::Reverse(s.updated_at));
            serde_json::to_value(ChatHistoryResult { sessions, session: None })
                .map_err(|e| format!("会话清单序列化失败：{e}"))
        }
    }
}

/// 向发起会话的插件下发一条对话事件（无映射或运行时不可用静默跳过）。
fn notify_plugin_chat(app: &App, session_id: &str, message_id: String, event: ChatEvent) {
    let Some(plugin_id) = app.chat.plugin_sessions.get(session_id) else {
        return;
    };
    app.plugins.notify_chat_event(
        plugin_id,
        &ChatEventParams {
            session_id: session_id.to_string(),
            message_id,
            event,
        },
    );
}

/// run 事件 → 插件事件载荷映射：终态直映；流式拍由调用方取消息体与序号（无消息体则不发）。
fn plugin_chat_event_of(event: &RunEvent, message: Option<Value>, rev: u64) -> Option<ChatEvent> {
    match event {
        RunEvent::Finished { reason, .. } => Some(ChatEvent::Finished { reason: reason.clone() }),
        RunEvent::Failed { error, .. } => Some(ChatEvent::Failed { error: error.clone() }),
        _ => message.map(|message| ChatEvent::Message { rev, message }),
    }
}

/// run 事件转发给插件会话：终态（Finished / Failed）强制；流式拍按 `due` 节流发消息快照。
fn forward_plugin_event(app: &mut App, event: &RunEvent, due: bool) {
    let Some(plugin_id) = app.chat.plugin_sessions.get(event_session(event)).cloned() else {
        return;
    };
    let session_id = event_session(event).to_string();
    let message_id = match event {
        RunEvent::Started { message_id, .. }
        | RunEvent::TextDelta { message_id, .. }
        | RunEvent::ReasoningDelta { message_id, .. }
        | RunEvent::ToolStart { message_id, .. }
        | RunEvent::ToolEnd { message_id, .. }
        | RunEvent::Finished { message_id, .. }
        | RunEvent::Failed { message_id, .. } => message_id.clone(),
    };
    let chat_event = match event {
        RunEvent::Finished { .. } | RunEvent::Failed { .. } => {
            plugin_chat_event_of(event, None, 0).expect("终态事件恒有载荷")
        }
        _ => {
            if !due {
                return;
            }
            let Some(message) = store()
                .session_messages(&session_id)
                .and_then(|ms| {
                    ms.into_iter()
                        .find(|m| m.get("id").and_then(Value::as_str) == Some(message_id.as_str()))
                })
            else {
                return;
            };
            app.chat.plugin_event_seq += 1;
            plugin_chat_event_of(event, Some(message), app.chat.plugin_event_seq)
                .expect("流式事件带消息体时恒有载荷")
        }
    };
    app.plugins.notify_chat_event(
        &plugin_id,
        &ChatEventParams { session_id, message_id, event: chat_event },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event_of(event: RunEvent, message: Option<Value>) -> Option<ChatEvent> {
        plugin_chat_event_of(&event, message, 7)
    }

    #[test]
    fn terminal_run_events_map_straight_to_plugin_payloads() {
        assert_eq!(
            event_of(
                RunEvent::Finished {
                    session_id: "s".into(),
                    message_id: "m".into(),
                    reason: "interrupted".into(),
                },
                None,
            ),
            Some(ChatEvent::Finished { reason: "interrupted".into() })
        );
        assert_eq!(
            event_of(
                RunEvent::Failed {
                    session_id: "s".into(),
                    message_id: "m".into(),
                    error: "超时".into(),
                },
                None,
            ),
            Some(ChatEvent::Failed { error: "超时".into() })
        );
    }

    #[test]
    fn streaming_run_events_need_a_snapshot_body() {
        let delta = RunEvent::TextDelta {
            session_id: "s".into(),
            message_id: "m".into(),
            text: "你".into(),
        };
        // 无消息体（会话已删等）= 不发，插件靠后拍自愈
        assert_eq!(event_of(delta.clone(), None), None);
        let Some(ChatEvent::Message { rev, message }) = event_of(
            delta,
            Some(json!({ "id": "m", "role": "assistant", "content": "你" })),
        ) else {
            panic!("带消息体的流式事件应映射为快照");
        };
        assert_eq!(rev, 7);
        assert_eq!(message["content"], json!("你"));
    }
}
