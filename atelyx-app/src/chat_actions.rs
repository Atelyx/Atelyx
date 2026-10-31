//! AI 域宿主编排：会话真源装载、插件能力面请求处理、对话事件分发。
//!
//! 会话真源在 `core::ai::session::SessionStore`（本模块只经 commit/load 触达），
//! 展示由对话面板插件自理；run 循环在 AI 核心插件，事件经插件运行时回送。

use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

use serde_json::{json, Value};
use tokio::sync::mpsc;

use atelyx_core::ai::{
    agent::{self, RunConfig, RunEvent},
    refs as ai_refs,
    session::{CommitBatch, DeleteEntry, MetaPatch, SessionStore, WriteChainConfig},
    stream::LlmMessage,
    tools::{RepoAccess, SearchConfig, ToolCtx},
};
use atelyx_core::plugin_runtime::{
    ChatChangeKind, ChatEvent, ChatEventParams, ChatHistoryResult, ChatPersistError,
    ChatSendParams, ChatSessionSummary, ChatSessionWire,
};

use crate::Message;
use crate::App;
use crate::RepoKind;

/// 压缩 / 命名的等待上限：略高于 AI 核心自身的辅助调用超时，避免宿主先判失败而插件后写结果。
const AUX_CALL_WAIT: std::time::Duration = std::time::Duration::from_secs(130);
/// 终态与受理回执可能乱序多久：超过该窗口的终态记录不再用于拦截迟到的受理。
const ENDED_GRACE: std::time::Duration = std::time::Duration::from_secs(60);

// ===== 进程级共享运行时 =====

static STORE: OnceLock<Arc<SessionStore>> = OnceLock::new();
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

/// 宿主级默认覆盖（会话未显式指定时生效）：模型 / 推理等级 / Agent。
pub(crate) fn chat_overrides() -> (Option<Value>, Option<String>, Option<String>) {
    store().overrides()
}

pub(crate) fn run_event_channel() -> &'static EventSlot<RunEvent> {
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

// ===== 运行态 =====

/// 宿主侧对话运行态（跨窗口共享一份；会话真源在 store，面板展示由插件自持）。
#[derive(Default)]
pub struct ChatState {
    /// 会话真源已装载（发送 / 压缩 / 命名前置检查）。
    pub loaded: bool,
    /// 活跃 run 的会话集合（宿主侧互斥判断；显示由面板插件自理）。
    pub streaming: HashSet<String>,
    /// 已收到终态、但受理回执可能还没到的会话（终态时刻）：迟到的受理不再标成活跃。
    pub ended: std::collections::HashMap<String, std::time::Instant>,
    /// 压缩中的会话。
    pub compacting: Option<String>,
    /// 重新命名请求在途的会话集合（按会话防重）。
    pub renaming: HashSet<String>,
}

// ===== 消息 =====

#[derive(Debug, Clone)]
pub enum ChatMsg {
    /// 落盘失败状态（写链观察者；None = 恢复）。
    PersistStatus(Option<(String, i64)>),
    /// 外部文件拖入窗口（优先落到悬停的对话卡）。
    FileDropped(std::path::PathBuf),
    /// run 循环事件（画布直写 run 的受理失败）。
    Run(RunEvent),
    /// 会话真源装载完成（空间模式在自建线程上装载，回执经控制通道回 Elm）。
    /// 空间装载回执：`Ok(false)` = 已被更新的装载取代，结果不再应用。
    StoreLoaded(u64, Result<bool, String>),
    /// 退出前落盘回执。
    FlushDone(Result<(), String>),
    /// 画布卡压缩收尾（携带会话 id 与摘要结果）。
    AidDone(String, Option<String>),
    /// 插件请求发起 / 续聊一轮对话（ai.chat.send 的宿主编排）。
    PluginSend {
        request_id: u64,
        params: ChatSendParams,
    },
    /// 插件发送的空间前置回执（Agent 提示词取回 + 图片存在性确认）。
    PluginSendReady {
        gen: u64,
        plan: Box<PluginSendPlan>,
        result: Result<PluginSendPrepared, String>,
    },
    /// 插件请求中断一个会话的活跃 run（ai.chat.stop）。
    PluginStop { request_id: u64, session_id: String },
    /// 插件请求读会话清单或单会话消息（ai.chat.history）。
    PluginHistory {
        request_id: u64,
        session_id: Option<String>,
        limit: Option<u32>,
        offset: Option<u32>,
        query: Option<String>,
    },
    /// 插件请求重命名一个会话（ai.chat.rename）。
    PluginRename { request_id: u64, session_id: String, title: String },
    /// 插件请求删除一个会话（ai.chat.delete）。
    PluginDelete { request_id: u64, session_id: String },
    /// 插件请求压缩一个会话（ai.chat.compact；回执 = 已受理）。
    PluginCompact { request_id: u64, session_id: String },
    /// 插件请求让模型重新生成会话标题（ai.chat.title）。
    PluginRetitle { request_id: u64, session_id: String },
    /// 插件压缩收尾（清理压缩中标记并回填结果）。
    PluginCompactDone { request_id: u64, session_id: String, result: Result<Value, String> },
    /// 插件重新命名收尾。
    PluginRetitleDone { request_id: u64, session_id: String, result: Result<Value, String> },
    /// 插件请求读单个会话（chat.session.read）。
    PluginSessionRead { request_id: u64, session_id: String },
    /// 插件提交会话批次（chat.session.write）。
    PluginSessionWrite { request_id: u64, batch: Value },
    /// 插件请求全仓库快照（chat.session.snapshot）。
    PluginSessionSnapshot { request_id: u64 },
    /// 插件请求执行一次工具（ai.tool.execute）。
    PluginToolExecute { request_id: u64, params: atelyx_core::plugin_runtime::ToolExecParams },
    /// AI 核心插件已受理一轮 run（会话与占位消息已落真源）。
    PluginRunStarted { request_id: u64, result: Result<Value, String> },
}


// ===== 仓库装载 =====

/// 进仓 / 切仓调用（本地）：装载磁盘真源，完成后通知订阅方重取。
pub fn load_vault(app: &mut App, root: &std::path::Path) {
    prepare_load(app);
    let ticket = store().begin_load();
    match store().load(ticket, atelyx_core::ai::session_io::local_io(root)) {
        // 已被更新的装载取代：真源状态归那次装载，本次不再广播
        Ok(None) => {}
        Ok(Some(_)) => {
            app.chat.loaded = true;
            app.plugins.on_chat_store_reloaded();
        }
        Err(e) => {
            // 装载失败时摘掉旧后端：否则插件继续读写上一个容器
            detach_background();
            app.plugins.on_chat_store_reloaded();
            app.notify(crate::NoticeLevel::Error, format!("会话装载失败：{e}"));
        }
    }
}

/// 进协作空间调用：装载空间真源（用户元数据：`chat/messages` 等键）。
/// 空间后端是阻塞 HTTP，装载落在自建线程上（tokio 线程上阻塞会 panic），回执经控制通道回 Elm。
pub fn load_space_sessions(app: &mut App) -> iced::Task<Message> {
    prepare_load(app);
    let Some(session) = crate::space_actions::session_of(app) else {
        app.notify(crate::NoticeLevel::Error, "会话装载失败：协作空间会话缺失");
        return iced::Task::none();
    };
    let Some(token) = app.space_token.clone() else {
        app.notify(crate::NoticeLevel::Error, "会话装载失败：协作空间未登录");
        return iced::Task::none();
    };
    let io = match atelyx_core::ai::session_io::space_io(
        &session.server_url,
        token,
        session.space_id.clone(),
    ) {
        Ok(io) => io,
        Err(e) => {
            app.notify(crate::NoticeLevel::Error, format!("会话装载失败：{e}"));
            return iced::Task::none();
        }
    };
    let gen = app.epoch();
    let ticket = store().begin_load();
    let store = Arc::clone(store());
    let tx = control_channel().0.clone();
    std::thread::spawn(move || {
        let result = store.load(ticket, io).map(|applied| applied.is_some());
        if result.is_err() {
            // 失败后不留旧后端：插件不得继续读写上一个容器
            store.detach();
        }
        let _ = tx.send(ChatMsg::StoreLoaded(gen, result));
    });
    iced::Task::none()
}

/// 装载前置：停全部在途 run（在途提交随换源失效），清宿主侧在途标记。
fn prepare_load(app: &mut App) {
    let _ = app.plugins.call_plugin(
        atelyx_core::plugin_runtime::AI_CORE_PLUGIN,
        atelyx_core::plugin_runtime::ai_run::SERVICE,
        atelyx_core::plugin_runtime::ai_run::STOP_ALL,
        json!({}),
    );
    app.chat.loaded = false;
    app.chat.streaming.clear();
    app.chat.ended.clear();
    app.chat.compacting = None;
    app.chat.renaming.clear();
}

/// 无真源（离开空间 / 未进仓）：清宿主侧在途标记、摘掉旧后端，通知订阅方清空展示。
pub fn detach_vault(app: &mut App) {
    prepare_load(app);
    detach_background();
    app.plugins.on_chat_store_reloaded();
}

/// 落旧真源在途写可能要访问网络（空间后端）；放到自建线程，装载票保证它不会抹掉
/// 随后装载成功的新容器。调用方须先把宿主侧置为未装载，插件读写才被挡住。
fn detach_background() {
    let store = Arc::clone(store());
    std::thread::spawn(move || store.detach());
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

/// 解析 run 配置（供应商路由 / 模型 / Agent 名册）。配置缺失 = Err（可读原因）。
/// `space_prompt`：空间模式下已取回的 Agent 提示词正文（本地模式自取，传 None）。
/// `for_run`：辅助调用（压缩 / 命名）不消费提示词，传 false 跳过提示词解析，
/// 空间模式下带提示词文件的 Agent 不因此报错。
/// `model_pick`：调用方显式指定的（供应商 id, 模型 id）；None = 宿主默认覆盖。
pub(crate) fn resolve_run_config(
    app: &App,
    agent_id: Option<&str>,
    space_prompt: Option<String>,
    for_run: bool,
    model_pick: Option<(&str, &str)>,
) -> Result<RunConfig, String> {
    let (override_pick, override_effort, _) = chat_overrides();
    // 显式指定优先于默认覆盖；都缺 = 报错拒绝
    let pick = model_pick
        .map(|(provider_id, model)| (provider_id.to_string(), model.to_string()))
        .or_else(|| {
            let value = override_pick?;
            let provider_id = value.get("providerId").and_then(Value::as_str)?;
            let model = value.get("model").and_then(Value::as_str)?;
            Some((provider_id.to_string(), model.to_string()))
        });
    let Some((provider_id, model)) = pick else {
        return Err("未选择模型：请在对话面板的模型菜单中选择".to_string());
    };
    // 推理等级覆盖只对默认模型生效；调用方显式指定模型时不沿用
    let reasoning_effort = match model_pick {
        Some(_) => None,
        None => override_effort
            .as_deref()
            .and_then(atelyx_core::ai::stream::ReasoningEffort::parse_wire),
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
        reasoning_effort,
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
pub(crate) fn tool_ctx(app: &App) -> ToolCtx {
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
        plugin_tools: app.plugins.plugin_tools(),
        plugins: Some(app.plugins.clone()),
    }
}

// ===== 更新循环 =====

pub fn update(app: &mut App, msg: ChatMsg) -> iced::Task<Message> {
    update_inner(app, msg)
}

fn update_inner(app: &mut App, msg: ChatMsg) -> iced::Task<Message> {
    match msg {
        ChatMsg::PersistStatus(status) => {
            let error = status.map(|(message, at)| ChatPersistError { message, at });
            if let Some(error) = &error {
                app.notify(
                    crate::NoticeLevel::Error,
                    format!("会话落盘失败：{}", error.message),
                );
            }
            app.plugins.on_chat_persist(error);
            iced::Task::none()
        }
        ChatMsg::FileDropped(path) => {
            if let Some(node) = crate::canvas_chat::hovered_conversation(app) {
                crate::canvas_chat::add_attachments(app, &node, vec![path]);
            } else {
                // 面板由插件自理：宿主只转发绝对路径，插件按自身界面状态决定是否接收
                app.plugins.on_files_dropped(vec![path.to_string_lossy().into_owned()]);
            }
            iced::Task::none()
        }
        ChatMsg::Run(event) => on_run_event(app, event),
        ChatMsg::StoreLoaded(gen, result) => {
            if gen != app.epoch() {
                // 装载在途期间换了容器：旧真源快照整体丢弃
                return iced::Task::none();
            }
            match result {
                Ok(true) => {
                    app.chat.loaded = true;
                    app.plugins.on_chat_store_reloaded();
                }
                // 被更新的装载取代：真源归那次装载，不当成已装载
                Ok(false) => {}
                Err(e) => {
                    app.plugins.on_chat_store_reloaded();
                    app.notify(crate::NoticeLevel::Error, format!("会话装载失败：{e}"));
                }
            }
            iced::Task::none()
        }
        ChatMsg::FlushDone(result) => {
            if let Err(e) = result {
                // 退出前的最后一次落盘失败：提示可见（若窗口已关，至少留下可定位原因）
                eprintln!("会话落盘失败（退出前）：{e}");
                app.notify(crate::NoticeLevel::Error, format!("会话落盘失败：{e}"));
            }
            iced::Task::none()
        }
        ChatMsg::AidDone(session_id, result) => {
            if let Some(node) =
                crate::canvas_chat::node_of_session(&session_id).map(str::to_string)
            {
                crate::canvas_chat::compact_done(app, &node, result);
            }
            iced::Task::none()
        }
        ChatMsg::PluginSend { request_id, params } => plugin_chat_send(app, request_id, params),
        ChatMsg::PluginSendReady { gen, plan, result } => {
            if gen != app.epoch() {
                // 仓库已切换：前置结果属于旧仓库，不再落消息
                app.plugins.resolve_shell_call(plan.request_id, Err("仓库已切换，发送已取消".into()));
                return iced::Task::none();
            }
            match result {
                Ok(prepared) => {
                    plugin_chat_send_now(app, *plan, prepared.space_prompt, prepared.attachments)
                }
                Err(e) => {
                    app.plugins.resolve_shell_call(plan.request_id, Err(e));
                    iced::Task::none()
                }
            }
        }
        ChatMsg::PluginStop { request_id, session_id } => {
            stop_run(app, &session_id);
            app.chat.streaming.remove(&session_id);
            app.plugins.resolve_shell_call(request_id, Ok(json!({})));
            iced::Task::none()
        }
        ChatMsg::PluginHistory { request_id, session_id, limit, offset, query } => {
            let result = require_chat_loaded(app)
                .and_then(|()| plugin_history(&session_id, limit, offset, query.as_deref()));
            app.plugins.resolve_shell_call(request_id, result);
            iced::Task::none()
        }
        ChatMsg::PluginRename { request_id, session_id, title } => {
            let result = require_chat_loaded(app)
                .and_then(|()| plugin_rename(app, &session_id, &title));
            app.plugins.resolve_shell_call(request_id, result);
            iced::Task::none()
        }
        ChatMsg::PluginDelete { request_id, session_id } => {
            let result =
                require_chat_loaded(app).and_then(|()| plugin_delete(app, &session_id));
            app.plugins.resolve_shell_call(request_id, result);
            iced::Task::none()
        }
        ChatMsg::PluginCompact { request_id, session_id } => {
            if let Err(e) = require_chat_loaded(app) {
                app.plugins.resolve_shell_call(request_id, Err(e));
                return iced::Task::none();
            }
            if let Err(e) = plugin_compact_check(app, &session_id) {
                app.plugins.resolve_shell_call(request_id, Err(e));
                return iced::Task::none();
            }
            let config = match resolve_run_config(app, None, None, false, None) {
                Ok(config) => wire_config(&config),
                Err(e) => {
                    app.plugins.resolve_shell_call(request_id, Err(e));
                    return iced::Task::none();
                }
            };
            app.chat.compacting = Some(session_id.clone());
            let params = atelyx_core::plugin_runtime::RunCompactParams {
                session_id: session_id.clone(),
                config,
            };
            let params = match serde_json::to_value(params) {
                Ok(params) => params,
                Err(e) => {
                    app.chat.compacting = None;
                    app.plugins
                        .resolve_shell_call(request_id, Err(format!("压缩参数序列化失败：{e}")));
                    return iced::Task::none();
                }
            };
            let tx = control_channel().0.clone();
            match app.plugins.call_plugin_timeout(
                atelyx_core::plugin_runtime::AI_CORE_PLUGIN,
                atelyx_core::plugin_runtime::ai_run::SERVICE,
                atelyx_core::plugin_runtime::ai_run::COMPACT,
                params,
                AUX_CALL_WAIT,
            ) {
                Ok(rx) => {
                    std::thread::spawn(move || {
                        let result = rx
                            .recv()
                            .unwrap_or_else(|_| Err("AI 核心未返回压缩结果".to_string()));
                        let _ = tx.send(ChatMsg::PluginCompactDone {
                            request_id,
                            session_id,
                            result,
                        });
                    });
                    iced::Task::none()
                }
                Err(e) => {
                    app.chat.compacting = None;
                    app.plugins.resolve_shell_call(request_id, Err(e));
                    iced::Task::none()
                }
            }
        }
        ChatMsg::PluginRetitle { request_id, session_id } => {
            if let Err(e) = require_chat_loaded(app) {
                app.plugins.resolve_shell_call(request_id, Err(e));
                return iced::Task::none();
            }
            if store().session_meta(&session_id).is_none() {
                app.plugins
                    .resolve_shell_call(request_id, Err(format!("会话 {session_id} 不存在")));
                return iced::Task::none();
            }
            if app.chat.streaming.contains(&session_id)
                || app.chat.renaming.contains(&session_id)
                || app.chat.compacting.as_deref() == Some(&session_id)
            {
                app.plugins.resolve_shell_call(
                    request_id,
                    Err("该会话有进行中的操作，暂不能重新命名".to_string()),
                );
                return iced::Task::none();
            }
            let config = match resolve_run_config(app, None, None, false, None) {
                Ok(config) => wire_config(&config),
                Err(e) => {
                    app.plugins.resolve_shell_call(request_id, Err(e));
                    return iced::Task::none();
                }
            };
            let params = atelyx_core::plugin_runtime::RunTitleParams {
                session_id: session_id.clone(),
                config,
            };
            let params = match serde_json::to_value(params) {
                Ok(params) => params,
                Err(e) => {
                    app.plugins
                        .resolve_shell_call(request_id, Err(format!("命名参数序列化失败：{e}")));
                    return iced::Task::none();
                }
            };
            app.chat.renaming.insert(session_id.clone());
            let tx = control_channel().0.clone();
            match app.plugins.call_plugin_timeout(
                atelyx_core::plugin_runtime::AI_CORE_PLUGIN,
                atelyx_core::plugin_runtime::ai_run::SERVICE,
                atelyx_core::plugin_runtime::ai_run::TITLE,
                params,
                AUX_CALL_WAIT,
            ) {
                Ok(rx) => {
                    std::thread::spawn(move || {
                        let result = rx
                            .recv()
                            .unwrap_or_else(|_| Err("AI 核心未返回命名结果".to_string()));
                        let _ = tx.send(ChatMsg::PluginRetitleDone {
                            request_id,
                            session_id,
                            result,
                        });
                    });
                    iced::Task::none()
                }
                Err(e) => {
                    app.chat.renaming.remove(&session_id);
                    app.plugins.resolve_shell_call(request_id, Err(e));
                    iced::Task::none()
                }
            }
        }
        ChatMsg::PluginSessionRead { request_id, session_id } => {
            let result = require_chat_loaded(app)
                .and_then(|()| plugin_session_read(&session_id));
            app.plugins.resolve_shell_call(request_id, result);
            iced::Task::none()
        }
        ChatMsg::PluginSessionWrite { request_id, batch } => {
            let result = require_chat_loaded(app)
                .and_then(|()| plugin_session_write(app, batch));
            app.plugins.resolve_shell_call(request_id, result);
            iced::Task::none()
        }
        ChatMsg::PluginSessionSnapshot { request_id } => {
            let result = require_chat_loaded(app)
                .and_then(|()| {
                    serde_json::to_value(store().snapshot())
                        .map_err(|e| format!("会话快照序列化失败：{e}"))
                });
            app.plugins.resolve_shell_call(request_id, result);
            iced::Task::none()
        }
        ChatMsg::PluginToolExecute { request_id, params } => {
            let ctx = tool_ctx(app);
            let runtime = app.plugins.clone();
            iced::Task::future(async move {
                let result = atelyx_core::ai::tools::execute(&ctx, &params.name, params.arguments).await;
                runtime.resolve_shell_call(
                    request_id,
                    Ok(json!({ "content": result.text, "isError": result.is_error })),
                );
                Message::Noop
            })
        }
        ChatMsg::PluginRunStarted { request_id, result } => {
            if let Ok(value) = &result {
                if let Some(session_id) = value.get("sessionId").and_then(Value::as_str) {
                    // 终态可能先于受理回执到达：已经收尾的会话不再标成活跃
                    if app.chat.ended.remove(session_id).is_none() {
                        app.chat.streaming.insert(session_id.to_string());
                    }
                }
            }
            app.plugins.resolve_shell_call(request_id, result);
            iced::Task::none()
        }
        ChatMsg::PluginCompactDone { request_id, session_id, result } => {
            if app.chat.compacting.as_deref() == Some(session_id.as_str()) {
                app.chat.compacting = None;
            }
            app.plugins.resolve_shell_call(request_id, result);
            iced::Task::none()
        }
        ChatMsg::PluginRetitleDone { request_id, session_id, result } => {
            app.chat.renaming.remove(&session_id);
            app.plugins.resolve_shell_call(request_id, result);
            iced::Task::none()
        }
    }
}

/// 中断一个会话的活跃 run（AI 核心幂等）。
pub(crate) fn stop_run(app: &App, session_id: &str) {
    let _ = app.plugins.call_plugin(
        atelyx_core::plugin_runtime::AI_CORE_PLUGIN,
        atelyx_core::plugin_runtime::ai_run::SERVICE,
        atelyx_core::plugin_runtime::ai_run::STOP,
        json!({ "sessionId": session_id }),
    );
}

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

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub(crate) fn uuid_v4() -> String {
    uuid::Uuid::new_v4().to_string()
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
                    let node_id = r.get("nodeId").and_then(Value::as_str).map(str::to_string);
                    // 画布卡引用只有 nodeId；文件引用只有 path
                    let path = r
                        .get("path")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .or_else(|| node_id.clone())?;
                    let label = r.get("label").and_then(Value::as_str)?;
                    let kind = ai_refs::RefKind::parse_key(
                        r.get("refKind").and_then(Value::as_str).unwrap_or(""),
                    )?;
                    Some(ai_refs::RefEntry {
                        path,
                        label: label.to_string(),
                        kind,
                        node_id,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 附件落盘后端（本地写盘队列 / 空间端点）。
pub(crate) enum ImageBackend {
    Local {
        root: std::path::PathBuf,
        queue: atelyx_core::write_queue::WriteQueue,
    },
    Space {
        client: Arc<atelyx_core::space_client::SpaceClient>,
        space_id: String,
    },
}

pub(crate) fn image_backend(app: &App) -> Result<ImageBackend, String> {
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
pub(crate) async fn materialize_images(
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
                .map(|_| ())
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

// ===== 输入面与 # 引用 =====

/// # 引用候选：仓库文件 / 目录或画布内已有卡。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MentionTarget {
    Vault { rel: String, is_dir: bool },
    Card { node_id: String },
}

/// # 引用候选浮层状态。
pub struct MentionPicker {
    /// 触发 `#` 的字节位置。
    pub at: usize,
    /// 触发位到光标之间的查询串。
    pub query: String,
    /// 键盘选中行。
    pub selected: usize,
    /// 过滤后的候选（画布卡排在前，其后为仓库文件与目录）。
    pub candidates: Vec<MentionTarget>,
}

/// 候选条数上限（超出只显示前 N 条，继续键入收窄）。
const MENTION_CANDIDATE_CAP: usize = 50;

/// 引用候选：打开中的文件（笔记 / 表格 / 画布）置顶，其余按文件树缓存序（md / atb / atlx
/// 文件与目录；隐藏与排除目录不在缓存内自然缺席）。查询按子串过滤（不区分大小写），
/// 置顶项同样受过滤，已置顶的路径不再在文件树里重复出现。
pub(crate) fn mention_candidates(app: &App, query: &str) -> Vec<MentionTarget> {
    let needle = query.to_lowercase();
    let mut out = Vec::new();
    seed_open_candidates(&crate::space_actions::open_file_paths(app), &needle, &mut out);
    collect_candidates(app, "", &needle, &mut out);
    out
}

/// 打开中的文件前置：命中查询者按打开顺序入列，已在列里的路径不再重复。
fn seed_open_candidates(open: &[String], needle: &str, out: &mut Vec<MentionTarget>) {
    for rel in open {
        if (needle.is_empty() || rel.to_lowercase().contains(needle)) && !listed(out, rel) {
            out.push(MentionTarget::Vault { rel: rel.clone(), is_dir: false });
        }
    }
}

fn collect_candidates(app: &App, dir: &str, needle: &str, out: &mut Vec<MentionTarget>) {
    if out.len() >= MENTION_CANDIDATE_CAP {
        return;
    }
    let mut children = app.children.get(dir).cloned().unwrap_or_default();
    atelyx_core::vault::sort_children(&mut children, &app.file_sort);
    for child in children {
        let hit = needle.is_empty() || child.rel.to_lowercase().contains(needle);
        if hit && !listed(out, &child.rel) {
            out.push(MentionTarget::Vault { rel: child.rel.clone(), is_dir: child.is_dir });
            if out.len() >= MENTION_CANDIDATE_CAP {
                return;
            }
        }
        if child.is_dir {
            collect_candidates(app, &child.rel, needle, out);
        }
    }
}

/// 路径是否已在候选里（置顶项与文件树项去重）。
fn listed(out: &[MentionTarget], rel: &str) -> bool {
    out.iter().any(|t| matches!(t, MentionTarget::Vault { rel: r, .. } if r == rel))
}

/// 扩展名 → MIME（托盘四类图片）。
pub(crate) fn image_mime(ext: &str) -> String {
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
    if !app.chat.loaded {
        return Err("会话未装载：请先打开仓库或进入协作空间".to_string());
    }
    if matches!(app.repo_kind, RepoKind::Local) && app.vault_root.is_none() {
        return Err("仓库未打开".to_string());
    }
    Ok(())
}

/// 压缩任务：按既有检查点重建转写并请求摘要，回执 `ChatMsg::AidDone`。
/// 压缩中标志由调用方自持（面板 = `ChatState.compacting`，卡 = 卡运行态）。
pub(crate) fn compact_task(
    session_key: String,
    config: RunConfig,
    messages: &[Value],
    compaction: Option<Value>,
) -> iced::Task<Message> {
    let mut transcript = String::new();
    for m in agent::history_to_llm_messages(messages, compaction.as_ref()) {
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
    iced::Task::future(async move {
        let Some(client) = agent::aid_client(&config.base_url) else {
            return Message::Chat(ChatMsg::AidDone(session_key, None));
        };
        let summary = agent::compact_summary(
            &client,
            &config.base_url,
            &config.api_key,
            &config.model,
            &transcript,
        )
        .await;
        Message::Chat(ChatMsg::AidDone(session_key, summary))
    })
}


// ===== run 事件应用 =====

fn on_run_event(app: &mut App, event: RunEvent) -> iced::Task<Message> {
    // 该通道只承载画布直写 run 的受理失败；存储型会话的展示与落盘由核心插件事件驱动
    if crate::canvas_chat::node_of_session(event_session(&event)).is_none() {
        return iced::Task::none();
    }
    crate::canvas_chat::on_run_event(app, event)
}

/// 应用 AI 核心发布的一条对话事件：画布卡会话按直写快照落卡，存储型会话只维护在途集合。
pub fn on_ai_chat_event(app: &mut App, params: ChatEventParams) -> iced::Task<Message> {
    let session_id = params.session_id.clone();
    if crate::canvas_chat::node_of_session(&session_id).is_some() {
        let event = match params.event {
            ChatEvent::Message { rev, message } => RunEvent::Snapshot {
                session_id: session_id.clone(),
                message_id: params.message_id.clone(),
                message,
                rev,
            },
            ChatEvent::Finished { reason } => RunEvent::Finished {
                session_id: session_id.clone(),
                message_id: params.message_id.clone(),
                reason,
            },
            ChatEvent::Failed { error } => RunEvent::Failed {
                session_id: session_id.clone(),
                message_id: params.message_id.clone(),
                error,
            },
            ChatEvent::Removed => RunEvent::MessageRemoved {
                session_id: session_id.clone(),
                message_id: params.message_id.clone(),
            },
            ChatEvent::Titled { .. } => return iced::Task::none(),
        };
        return crate::canvas_chat::on_run_event(app, event);
    }
    // 存储型会话的展示由对话面板插件自理；壳只维护在途集合（压缩 / 停止的互斥判断用）
    match params.event {
        ChatEvent::Message { .. } => {
            app.chat.ended.remove(&session_id);
            app.chat.streaming.insert(session_id);
        }
        ChatEvent::Finished { .. } | ChatEvent::Failed { .. } | ChatEvent::Removed => {
            app.chat.streaming.remove(&session_id);
            // 只覆盖「终态先于受理回执」的短窗口，其余条目按时间清掉防无限增长
            let now = std::time::Instant::now();
            app.chat.ended.retain(|_, at| now.duration_since(*at) < ENDED_GRACE);
            app.chat.ended.insert(session_id, now);
        }
        ChatEvent::Titled { .. } => {}
    }
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
        | RunEvent::Failed { session_id, .. }
        | RunEvent::Snapshot { session_id, .. }
        | RunEvent::MessageRemoved { session_id, .. } => session_id,
    }
}

// ===== 插件 AI 契约编排（ai.chat.* 宿主侧） =====

/// 插件发送请求的编排包：空间模式下提示词与图片存在性需先向服务端确认，
/// 回执 [`ChatMsg::PluginSendReady`] 后带着就绪结果回到 [`plugin_chat_send_now`]。
#[derive(Debug, Clone)]
pub struct PluginSendPlan {
    request_id: u64,
    params: ChatSendParams,
    /// 插件指定的模型（供应商 id + 模型 id）；None = 安装级默认。
    pick: Option<(String, String)>,
}

/// 空间前置结果：Agent 提示词正文（无提示词 = None）与已确认存在的图片附件元数据。
#[derive(Debug, Clone)]
pub struct PluginSendPrepared {
    space_prompt: Option<String>,
    attachments: Vec<Value>,
}

/// 插件发起 / 续聊一轮对话：校验（Agent / 模型 / 图片）→ 空间前置 → 会话就位
/// → user 消息落真源 → 起 run（回执经宿主请求通道异步送达插件，运行事件由核心插件广播）。
fn plugin_chat_send(
    app: &mut App,
    request_id: u64,
    params: ChatSendParams,
) -> iced::Task<Message> {
    let deny = |app: &App, reason: String| {
        app.plugins.resolve_shell_call(request_id, Err(reason));
    };
    if let Err(e) = require_chat_loaded(app) {
        deny(app, e);
        return iced::Task::none();
    }
    if let Some(session_id) = params.session_id.as_deref() {
        app.chat.ended.remove(session_id);
    }
    let params = ChatSendParams { text: params.text.trim().to_string(), ..params };
    if params.text.is_empty() && params.images.is_empty() {
        deny(app, "消息内容为空".to_string());
        return iced::Task::none();
    }
    let explicit_agent = params
        .agent_id
        .as_deref()
        .filter(|id| !id.is_empty())
        .map(str::to_string);
    if let Some(agent_id) = &explicit_agent {
        if !app.agents.iter().any(|a| a.id == *agent_id) {
            // 名册随容器装载（空间模式经网络）：空清单 = 尚未就绪，不当作「不存在」
            let reason = if app.agents.is_empty() {
                "Agent 名册尚未读取完成，请稍后重试".to_string()
            } else {
                format!("Agent 不存在：{agent_id}")
            };
            deny(app, reason);
            return iced::Task::none();
        }
    }
    let pick = match plugin_model_pick(app, &params) {
        Ok(pick) => pick,
        Err(e) => {
            deny(app, e);
            return iced::Task::none();
        }
    };
    if let Err(e) = check_ready(app) {
        deny(app, e);
        return iced::Task::none();
    }
    // 续聊未指定 Agent：沿用会话已记录的选择（与面板续聊同一口径）
    let session_agent = match &params.session_id {
        Some(id) => match store().session_meta(id) {
            Some(meta) => meta.get("agentId").and_then(Value::as_str).map(str::to_string),
            None => {
                deny(app, format!("会话 {id} 不存在"));
                return iced::Task::none();
            }
        },
        None => None,
    };
    let params = ChatSendParams { agent_id: explicit_agent.or(session_agent), ..params };
    let entries = match plugin_image_entries(&params.images) {
        Ok(entries) => entries,
        Err(e) => {
            deny(app, e);
            return iced::Task::none();
        }
    };
    let prompt_file = params.agent_id.as_ref().and_then(|id| {
        app.agents
            .iter()
            .find(|a| &a.id == id)
            .and_then(|a| a.system_prompt_file.clone())
    });
    let plan = PluginSendPlan { request_id, params, pick };
    if !matches!(app.repo_kind, RepoKind::Space(_)) {
        return match local_image_attachments(app, entries) {
            Ok(attachments) => plugin_chat_send_now(app, plan, None, attachments),
            Err(e) => {
                deny(app, e);
                iced::Task::none()
            }
        };
    }
    let Some(session) = crate::space_actions::session_of(app) else {
        deny(app, "协作空间未连接".to_string());
        return iced::Task::none();
    };
    let client = session.client.clone();
    let space_id = session.space_id.clone();
    let gen = app.epoch();
    let message_plan = plan.clone();
    iced::Task::perform(
        async move {
            let space_prompt = match prompt_file {
                Some(file) => client
                    .get_file_text(&space_id, &file)
                    .await
                    .map(Some)
                    .map_err(|e| format!("Agent 提示词读取失败（{file}）：{e}")),
                None => Ok(None),
            }?;
            let attachments = space_image_attachments(&client, &space_id, entries).await?;
            Ok(PluginSendPrepared { space_prompt, attachments })
        },
        move |result| {
            Message::Chat(ChatMsg::PluginSendReady {
                gen,
                plan: Box::new(message_plan.clone()),
                result,
            })
        },
    )
}

/// 插件指定的模型解析为（供应商 id, 模型 id）：未配置 / 多供应商歧义 = 可读拒绝；
/// 未指定 = None（沿用面板当前选择）。
fn plugin_model_pick(
    app: &App,
    params: &ChatSendParams,
) -> Result<Option<(String, String)>, String> {
    let Some(model) = params.model.as_deref().filter(|m| !m.is_empty()) else {
        // providerId 只用于给 model 消歧，单独给没有意义
        return if params.provider_id.as_deref().is_some_and(|p| !p.is_empty()) {
            Err("指定 providerId 时必须同时指定 model".to_string())
        } else {
            Ok(None)
        };
    };
    let providers = app.repo_config.providers.as_deref().unwrap_or_default();
    if let Some(provider_id) = params.provider_id.as_deref().filter(|p| !p.is_empty()) {
        let Some(provider) = providers.iter().find(|p| p.id == provider_id) else {
            return Err(format!("供应商不存在：{provider_id}"));
        };
        if !provider.models.iter().any(|m| m.id == model) {
            return Err(format!("供应商 {provider_id} 未配置模型 {model}"));
        }
        return Ok(Some((provider_id.to_string(), model.to_string())));
    }
    let mut hits = providers.iter().filter(|p| p.models.iter().any(|m| m.id == model));
    let Some(provider) = hits.next() else {
        return Err(format!("模型 {model} 未在供应商配置中"));
    };
    if hits.next().is_some() {
        return Err(format!("模型 {model} 在多个供应商中配置，请指定 providerId"));
    }
    Ok(Some((provider.id.clone(), model.to_string())))
}

/// 插件图片路径的语法与扩展名校验（只拦越界、允许隐藏段；存在性由落仓形态各自确认）。
fn plugin_image_entries(paths: &[String]) -> Result<Vec<(String, String)>, String> {
    let mut entries = Vec::new();
    for (index, raw) in paths.iter().enumerate() {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(format!("第 {} 张图片路径为空", index + 1));
        }
        let rel = atelyx_core::vault::validate_plugin_path(raw)
            .map_err(|e| format!("第 {} 张图片路径不合法：{e}", index + 1))?;
        let name = rel.rsplit('/').next().unwrap_or(&rel);
        let ext = atelyx_core::vault::image_ext_of(name)
            .ok_or_else(|| format!("非图片文件：{raw}（仅 png/jpg/jpeg/webp/gif）"))?;
        entries.push((rel, image_mime(ext)));
    }
    Ok(entries)
}

/// 本地仓库图片存在性确认（仓库内相对路径，越界即拒绝）。
fn local_image_attachments(
    app: &App,
    entries: Vec<(String, String)>,
) -> Result<Vec<Value>, String> {
    let root = app.vault_root.as_ref().ok_or_else(|| "仓库未打开".to_string())?;
    let mut out = Vec::new();
    for (rel, mime) in entries {
        let exists = atelyx_core::vault::file_exists(root, &rel)
            .map_err(|e| format!("图片路径不合法（{rel}）：{e}"))?;
        if !exists {
            return Err(format!("图片不存在：{rel}"));
        }
        out.push(json!({ "kind": "image", "mime": mime, "file": rel }));
    }
    Ok(out)
}

/// 协作空间图片存在性确认（服务端文件树即真源）。
async fn space_image_attachments(
    client: &Arc<atelyx_core::space_client::SpaceClient>,
    space_id: &str,
    entries: Vec<(String, String)>,
) -> Result<Vec<Value>, String> {
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    let tree = client.tree(space_id).await.map_err(|e| format!("图片校验失败：{e}"))?;
    let mut out = Vec::new();
    for (rel, mime) in entries {
        if !crate::space_actions::tree_has_file(&tree, &rel) {
            return Err(format!("图片不存在：{rel}"));
        }
        out.push(json!({ "kind": "image", "mime": mime, "file": rel }));
    }
    Ok(out)
}

/// 插件发送第二段（配置与前置已就绪）：会话就位 → user 消息落真源 → 起 run。
fn plugin_chat_send_now(
    app: &mut App,
    plan: PluginSendPlan,
    space_prompt: Option<String>,
    attachments: Vec<Value>,
) -> iced::Task<Message> {
    let request_id = plan.request_id;
    let deny = |app: &App, reason: String| {
        app.plugins.resolve_shell_call(request_id, Err(reason));
    };
    let mut config = match resolve_run_config(
        app,
        plan.params.agent_id.as_deref(),
        space_prompt,
        true,
        plan.pick.as_ref().map(|(provider, model)| (provider.as_str(), model.as_str())),
    ) {
        Ok(config) => config,
        Err(e) => {
            deny(app, e);
            return iced::Task::none();
        }
    };
    if let Some(effort) = plan.params.effort.as_deref() {
        config.reasoning_effort = match effort {
            "off" => None,
            "low" => Some(atelyx_core::ai::stream::ReasoningEffort::Low),
            "medium" => Some(atelyx_core::ai::stream::ReasoningEffort::Medium),
            "high" => Some(atelyx_core::ai::stream::ReasoningEffort::High),
            other => {
                deny(app, format!("推理等级无效：{other}"));
                return iced::Task::none();
            }
        };
    }
    if let Some(id) = plan.params.session_id.as_deref() {
        if store().session_meta(id).is_none() {
            deny(app, format!("会话 {id} 不存在"));
            return iced::Task::none();
        }
        if app.chat.compacting.as_deref() == Some(id) {
            deny(app, "该会话正在压缩上下文".to_string());
            return iced::Task::none();
        }
    }
    let display = plan
        .params
        .display_text
        .clone()
        .unwrap_or_else(|| plan.params.text.clone())
        .trim()
        .to_string();
    let refs = chat_ref_entries(&plan.params);
    let hits = ai_refs::scan_hits(&display, &refs);
    let mut content = ai_refs::reference_path_block(&hits);
    content.push_str(&display);
    let refs_json: Vec<Value> = hits
        .iter()
        .filter_map(|hit| {
            let entry = plan.params.refs.iter().find(|entry| entry.path == hit.path)?;
            Some(json!({
                "kind": "file",
                "path": hit.path,
                "label": entry.label,
                "refKind": hit.kind.key(),
            }))
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
    let compaction = plan
        .params
        .session_id
        .as_deref()
        .and_then(|id| store().session_meta(id))
        .and_then(|meta| meta.get("compaction").cloned())
        .filter(|value| !value.is_null());
    let naming = naming_config_wire(app, plan.params.agent_id.as_deref());
    let params = atelyx_core::plugin_runtime::RunStartParams {
        session_id: plan.params.session_id.clone(),
        agent_id: plan.params.agent_id.clone(),
        user_message,
        compaction,
        config: wire_config(&config),
        naming,
    };
    let params = match serde_json::to_value(params) {
        Ok(params) => params,
        Err(e) => {
            deny(app, format!("运行参数序列化失败：{e}"));
            return iced::Task::none();
        }
    };
    let runtime = app.plugins.clone();
    let tx = control_channel().0.clone();
    match runtime.call_plugin_timeout(
        atelyx_core::plugin_runtime::AI_CORE_PLUGIN,
        atelyx_core::plugin_runtime::ai_run::SERVICE,
        atelyx_core::plugin_runtime::ai_run::START,
        params,
        AUX_CALL_WAIT,
    ) {
        Ok(rx) => {
            std::thread::spawn(move || {
                let result = rx
                    .recv()
                    .unwrap_or_else(|_| Err("AI 核心未返回受理结果".to_string()));
                let _ = tx.send(ChatMsg::PluginRunStarted { request_id, result });
            });
            iced::Task::none()
        }
        Err(e) => {
            deny(app, e);
            iced::Task::none()
        }
    }
}

/// 插件消息里的 # 引用 → 纯逻辑词表（未知领域类型按不可引用丢弃）。
fn chat_ref_entries(params: &ChatSendParams) -> Vec<ai_refs::RefEntry> {
    params
        .refs
        .iter()
        .filter_map(|entry| {
            let kind = ai_refs::RefKind::parse_key(&entry.ref_kind)?;
            Some(ai_refs::RefEntry {
                path: entry.path.clone(),
                label: entry.label.clone(),
                kind,
                node_id: None,
            })
        })
        .collect()
}

/// 宿主解析出的运行配置 → 跨库运行配置。
pub(crate) fn wire_config(config: &RunConfig) -> atelyx_core::plugin_runtime::RunConfigWire {
    use atelyx_core::ai::stream::ReasoningEffort;
    atelyx_core::plugin_runtime::RunConfigWire {
        base_url: config.base_url.clone(),
        api_key: config.api_key.clone(),
        model: config.model.clone(),
        reasoning_effort: match config.reasoning_effort {
            Some(ReasoningEffort::Low) => Some("low".to_string()),
            Some(ReasoningEffort::Medium) => Some("medium".to_string()),
            Some(ReasoningEffort::High) => Some("high".to_string()),
            Some(ReasoningEffort::Off) | None => None,
        },
        system_prompt: config.system_prompt.clone(),
        tool_ids: config.tool_ids.clone(),
        search: atelyx_core::plugin_runtime::SearchConfigWire {
            provider: config.search.provider.clone(),
            tavily_key: config.search.tavily_key.clone(),
            searxng_url: config.search.searxng_url.clone(),
        },
        exclude: config.exclude.clone(),
    }
}

/// 自动命名配置（Off = None；Custom / Follow 按设置页路由解析）。
fn naming_config_wire(
    app: &App,
    agent_ref: Option<&str>,
) -> Option<atelyx_core::plugin_runtime::RunConfigWire> {
    use crate::settings_view::ai::NamingMode;
    match crate::settings_view::ai::naming_mode(app) {
        NamingMode::Off => None,
        NamingMode::Custom => {
            let pick = app.repo_config.auto_naming_model.clone()?;
            let provider = app
                .repo_config
                .providers
                .as_ref()?
                .iter()
                .find(|provider| provider.id == pick.provider_id)?;
            let api_key = match &provider.api_key {
                Some(key) if !key.is_empty() => key.clone(),
                _ => key_scope(app)
                    .and_then(|scope| {
                        crate::credentials::load_provider_key(&scope, &provider.id).ok().flatten()
                    })
                    .filter(|key| !key.is_empty())?,
            };
            Some(atelyx_core::plugin_runtime::RunConfigWire {
                base_url: provider.base_url.clone(),
                api_key,
                model: pick.model,
                reasoning_effort: None,
                system_prompt: None,
                tool_ids: Vec::new(),
                search: Default::default(),
                exclude: Vec::new(),
            })
        }
        NamingMode::FollowDefault => resolve_run_config(app, agent_ref, None, false, None)
            .ok()
            .map(|config| wire_config(&config)),
    }
}

/// 会话真源可用性：装载未完成 / 失败期间拒绝插件读写，避免落到上一个容器。
fn require_chat_loaded(app: &App) -> Result<(), String> {
    if app.chat.loaded {
        Ok(())
    } else {
        Err("会话仓库未装载".to_string())
    }
}

/// 插件读单个会话（`chat.session.read`）：元数据 + 消息数组，形态与会话磁盘格式同源。
fn plugin_session_read(session_id: &str) -> Result<Value, String> {
    let meta = store()
        .session_meta(session_id)
        .ok_or_else(|| format!("会话 {session_id} 不存在"))?;
    let messages = store().session_messages(session_id).unwrap_or_default();
    let mut record = meta;
    if let Some(obj) = record.as_object_mut() {
        obj.insert("messages".into(), json!(messages));
    }
    Ok(record)
}

/// 插件提交会话批次（`chat.session.write`）：与本地编排同一入口（校验后一次应用、写链落盘）。
fn plugin_session_write(app: &mut App, batch: Value) -> Result<Value, String> {
    let batch: CommitBatch =
        serde_json::from_value(batch).map_err(|e| format!("会话批次不可解析：{e}"))?;
    // 流式补丁不算语义变更：不发 `ai:chat.changed`、不重建展示投影（订阅方按事件快照自愈）
    let semantic = !batch.created.is_empty()
        || !batch.metas.is_empty()
        || !batch.appends.is_empty()
        || !batch.truncations.is_empty()
        || !batch.deletes.is_empty()
        || !batch.message_removals.is_empty()
        || batch.overrides.model_override.is_some()
        || batch.overrides.effort_override.is_some()
        || batch.overrides.agent_override.is_some();
    let mut changes: Vec<(String, ChatChangeKind)> = Vec::new();
    for def in &batch.created {
        changes.push((def.id.clone(), ChatChangeKind::Created));
    }
    for meta in &batch.metas {
        changes.push((meta.id.clone(), ChatChangeKind::Meta));
    }
    for append in &batch.appends {
        changes.push((append.session_id.clone(), ChatChangeKind::Messages));
    }
    for patch in &batch.patches {
        changes.push((patch.session_id.clone(), ChatChangeKind::Messages));
    }
    for truncation in &batch.truncations {
        changes.push((truncation.session_id.clone(), ChatChangeKind::Messages));
    }
    for removal in &batch.message_removals {
        changes.push((removal.session_id.clone(), ChatChangeKind::Messages));
    }
    for delete in &batch.deletes {
        changes.push((delete.session_id.clone(), ChatChangeKind::Deleted));
    }
    let deleted: Vec<(String, bool)> =
        batch.deletes.iter().map(|d| (d.session_id.clone(), d.remove_files)).collect();
    store().commit(batch)?;
    for (id, remove_files) in deleted {
        stop_run(app, &id);
        app.chat.streaming.remove(&id);
        if remove_files {
            cleanup_chat_attachments(app, &id);
        }
    }
    if semantic {
        for (id, kind) in changes {
            app.plugins.on_chat_changed(&id, kind);
        }
    }
    Ok(json!({}))
}

/// 插件读会话历史：清单模式只填 `sessions`；单会话模式先按 `query` 过滤、再按末尾 `offset` 跳过后取
/// `limit` 条（缺省全量）。
fn plugin_history(
    session_id: &Option<String>,
    limit: Option<u32>,
    offset: Option<u32>,
    query: Option<&str>,
) -> Result<Value, String> {
    let store = store();
    match session_id {
        Some(id) => {
            let meta = store
                .session_meta(id)
                .ok_or_else(|| format!("会话 {id} 不存在"))?;
            let messages = store.session_messages(id).unwrap_or_default();
            let result = single_session_history(id, &meta, &messages, limit, offset, query);
            serde_json::to_value(result).map_err(|e| format!("会话出线序列化失败：{e}"))
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

/// 单会话模式的回执装配：先按 `query` 过滤，再从末尾跳过 `offset` 条、取 `limit` 条（缺省全量）。
/// 两种模式共用 [`ChatHistoryResult`] 外层，单会话只填 `session`。
fn single_session_history(
    id: &str,
    meta: &Value,
    messages: &[Value],
    limit: Option<u32>,
    offset: Option<u32>,
    query: Option<&str>,
) -> ChatHistoryResult {
    // 只收引用再切窗口：分页调用只付窗口大小的拷贝代价，不复制整段历史
    let needle = query.map(str::trim).filter(|q| !q.is_empty()).map(str::to_lowercase);
    let selected: Vec<&Value> = messages
        .iter()
        .filter(|m| needle.as_deref().is_none_or(|needle| message_matches_query(m, needle)))
        .collect();
    let end = selected.len().saturating_sub(offset.unwrap_or(0) as usize);
    let start = match limit {
        Some(limit) => end.saturating_sub(limit as usize),
        None => 0,
    };
    ChatHistoryResult {
        sessions: Vec::new(),
        session: Some(ChatSessionWire {
            id: id.to_string(),
            title: meta.get("title").and_then(Value::as_str).map(str::to_string),
            created_at: meta.get("createdAt").and_then(Value::as_i64).unwrap_or(0),
            updated_at: meta.get("updatedAt").and_then(Value::as_i64).unwrap_or(0),
            messages: selected[start..end].iter().map(|m| (*m).clone()).collect(),
        }),
    }
}

/// 一条消息是否命中关键词：在正文与展示文本里不区分大小写查找。
fn message_matches_query(message: &Value, needle: &str) -> bool {
    ["content", "displayContent"]
        .iter()
        .any(|key| message.get(key).and_then(Value::as_str).is_some_and(|text| text.to_lowercase().contains(needle)))
}

/// 立即删除一个会话：落盘删除、停掉在途 run、附件临时目录清理（本地仓库自管区）。
/// 落盘失败即中止，不产生半删状态。
fn delete_session_now(app: &mut App, id: &str) -> Result<(), String> {
    store().commit(CommitBatch {
        deletes: vec![DeleteEntry { session_id: id.to_string(), remove_files: true }],
        ..Default::default()
    })?;
    stop_run(app, id);
    app.chat.streaming.remove(id);
    app.plugins.on_chat_changed(id, ChatChangeKind::Deleted);
    cleanup_chat_attachments(app, id);
    Ok(())
}

/// 删除已落盘的会话后清理其附件临时目录（本地仓库自管区；空间附件随会话文件由服务端删）。
fn cleanup_chat_attachments(app: &mut App, id: &str) {
    if !matches!(app.repo_kind, RepoKind::Local) {
        return;
    }
    let Some(root) = app.vault_root.clone() else {
        return;
    };
    if let Ok(dir) = atelyx_core::vault::chat_attachment_dir(&root, id) {
        if let Err(e) = std::fs::remove_dir_all(&dir) {
            app.notify(crate::NoticeLevel::Warning, format!("会话附件临时目录清理失败：{e}"));
        }
    }
}

/// 插件重命名会话：写元数据标题（时间戳与全仓库同一口径：毫秒）。
fn plugin_rename(app: &mut App, session_id: &str, title: &str) -> Result<Value, String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("标题不能为空".to_string());
    }
    if store().session_meta(session_id).is_none() {
        return Err(format!("会话 {session_id} 不存在"));
    }
    let now = now_ms();
    store().commit(CommitBatch {
        metas: vec![MetaPatch {
            id: session_id.to_string(),
            title: Some(title.to_string()),
            agent_id: None,
            compaction: None,
            updated_at: Some(now),
        }],
        ..Default::default()
    })?;
    app.plugins.on_chat_changed(session_id, ChatChangeKind::Meta);
    Ok(json!({}))
}

/// 插件删除会话：走与手工删除同一条链；落盘失败以可读原因回给插件，不静默当成功。
fn plugin_delete(app: &mut App, session_id: &str) -> Result<Value, String> {
    if store().session_meta(session_id).is_none() {
        return Err(format!("会话 {session_id} 不存在"));
    }
    delete_session_now(app, session_id)?;
    Ok(json!({}))
}

/// 插件压缩会话的前置校验：只放行「确实能起压缩」的请求（会话存在、无进行中的 run / 压缩、
/// 且消息数够）。真正压缩由 AI 核心插件执行；回执 = 已受理，结果经 `ai.chat.history` 复核。
fn plugin_compact_check(app: &App, session_id: &str) -> Result<(), String> {
    if store().session_meta(session_id).is_none() {
        return Err(format!("会话 {session_id} 不存在"));
    }
    if app.chat.compacting.is_some() {
        return Err("已有压缩在进行".to_string());
    }
    if app.chat.streaming.contains(&session_id.to_string()) {
        return Err("该会话有进行中的回复，暂不能压缩".to_string());
    }
    if store().session_messages(session_id).unwrap_or_default().len() < 2 {
        return Err("对话内容太少，无需压缩".to_string());
    }
    Ok(())
}

/// 当前仍在途的会话（面板插件会话 + 画布卡会话），AI 核心异常收场时补发终态用。
pub fn in_flight_sessions(app: &App) -> Vec<String> {
    let mut sessions: Vec<String> = app.chat.streaming.iter().cloned().collect();
    for session in crate::canvas_chat::streaming_sessions(app) {
        if !sessions.contains(&session) {
            sessions.push(session);
        }
    }
    sessions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_image_entries_normalizes_paths_and_mimes() {
        let entries = plugin_image_entries(&[
            "shots/a.PNG".to_string(),
            "shots\\sub\\b.jpg".to_string(),
            "  c.webp  ".to_string(),
            "shots/./d.gif".to_string(),
        ])
        .unwrap();
        assert_eq!(
            entries,
            vec![
                ("shots/a.PNG".to_string(), "image/png".to_string()),
                ("shots/sub/b.jpg".to_string(), "image/jpeg".to_string()),
                ("c.webp".to_string(), "image/webp".to_string()),
                ("shots/d.gif".to_string(), "image/gif".to_string()),
            ]
        );
    }

    #[test]
    fn single_session_history_uses_the_shared_envelope() {
        let meta = json!({ "title": "对话", "createdAt": 1, "updatedAt": 2 });
        let messages = vec![
            json!({ "id": "m1", "content": "甲" }),
            json!({ "id": "m2", "content": "乙" }),
            json!({ "id": "m3", "content": "甲" }),
        ];
        // 过滤后剩 m1/m3；从末尾跳过 1 条再取 1 条 → m1
        let result = single_session_history("s", &meta, &messages, Some(1), Some(1), Some("甲"));
        assert!(result.sessions.is_empty(), "单会话模式不填清单");
        let wire = result.session.as_ref().expect("单会话模式填 session");
        assert_eq!(wire.id, "s");
        assert_eq!(wire.title.as_deref(), Some("对话"));
        assert_eq!((wire.created_at, wire.updated_at), (1, 2));
        assert_eq!(wire.messages, vec![json!({ "id": "m1", "content": "甲" })]);

        // 回执要按协议类型原样可解，插件才不会读到两个空槽
        let value = serde_json::to_value(result).expect("可序列化");
        assert!(value.get("sessions").is_none(), "空清单不落字段");
        let decoded: ChatHistoryResult = serde_json::from_value(value).expect("按协议类型可解");
        assert_eq!(decoded.session.map(|s| s.id), Some("s".to_string()));
    }

    #[test]
    fn plugin_image_entries_rejects_unsafe_or_non_image_paths() {
        for bad in [
            "",
            "  ",
            "/abs.png",
            "../escape.png",
            "shots/../../x.png",
            "notes/a.md",
        ] {
            assert!(plugin_image_entries(&[bad.to_string()]).is_err(), "应拒绝：{bad:?}");
        }
        // 插件自管附件可落在隐藏目录
        assert!(plugin_image_entries(&[".atelyx/temp/chats/a.png".to_string()]).is_ok());
    }

    /// 打开中的文件排在候选最前，其余按文件树序追加；同一路径只出现一次，查询同样过滤置顶项。
    #[test]
    fn open_files_lead_mention_candidates() {
        let open = vec!["笔记/甲.md".to_string(), "数据.atb".to_string()];
        let mut out = Vec::new();
        seed_open_candidates(&open, "", &mut out);
        assert_eq!(
            out,
            vec![
                MentionTarget::Vault { rel: "笔记/甲.md".into(), is_dir: false },
                MentionTarget::Vault { rel: "数据.atb".into(), is_dir: false },
            ]
        );

        // 已在列里的路径不重复（文件树递归会再次扫到同一文件）
        seed_open_candidates(&open, "", &mut out);
        assert_eq!(out.len(), 2, "已完成列出的路径不再入列");

        // 查询只放过命中的置顶项
        let mut filtered = Vec::new();
        seed_open_candidates(&open, "数据", &mut filtered);
        assert_eq!(
            filtered,
            vec![MentionTarget::Vault { rel: "数据.atb".into(), is_dir: false }]
        );
    }
}
