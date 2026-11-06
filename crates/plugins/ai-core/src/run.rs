//! AI 运行：一轮对话的请求 / 流式 / 工具 / 落真源 / 事件发布。
//!
//! 会话真源经宿主 `chat.session.*` 读写，内置与插件工具经 `ai.tool.execute` 执行，
//! 出网经宿主 `http.stream.*` 保持地址守卫；本模块不直接碰磁盘与网络。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use atelyx_plugin_protocol::chat::{
    AppendBatch, CommitBatch, MessageRemoval, MetaPatch, PatchEntry, SessionDef,
};
use atelyx_plugin_protocol::method::{
    self, ChatEvent, ChatEventParams, HttpStreamOpenParams, RunConfigWire, RunDirectParams,
    RunStartParams, RunStartResult,
};
use atelyx_plugin_sdk::Context;
use base64::Engine;
use serde_json::{json, Value};
use tokio::sync::{mpsc, watch};
use uuid::Uuid;

use crate::history::{history_to_llm_messages, PENDING_RUN_ID_PREFIX};
use crate::stream::{
    request_body, LlmMessage, ReasoningEffort, SseDecoder, StreamEvent, StreamRequest, ToolCall,
    ToolDecl as LlmToolDecl,
};
use crate::title::clean_title;

/// 流式 patch / 快照的最短间隔（模型可见即已记录的下限延迟）。
const COMMIT_INTERVAL: Duration = Duration::from_millis(50);
/// 同一 parallel_safe 段内最多同时执行的工具数。
const MAX_PARALLEL_TOOLS: usize = 10;
/// 辅助调用（命名 / 压缩）的等待上限。
const AUX_TIMEOUT: Duration = Duration::from_secs(120);
/// 工具结果摘要截断。
const RESULT_SUMMARY_CHARS: usize = 120;

/// 运行登记与流式接收端（插件内共享）。
pub struct RunHub {
    runs: Mutex<HashMap<String, RunHandle>>,
    /// 已完成回复、仍在生成标题的会话（不占 run 名额，但退出收尾要等）。
    naming: Mutex<HashSet<String>>,
    /// 在途的压缩 / 手动命名调用（阻塞工作线程之外的辅助任务）。
    aux: AtomicUsize,
    streams: Mutex<HashMap<String, mpsc::UnboundedSender<StreamMsg>>>,
    seq: AtomicU64,
}

impl Default for RunHub {
    fn default() -> Self {
        Self::new()
    }
}

impl RunHub {
    pub fn new() -> Self {
        Self {
            runs: Mutex::new(HashMap::new()),
            naming: Mutex::new(HashSet::new()),
            aux: AtomicUsize::new(0),
            streams: Mutex::new(HashMap::new()),
            seq: AtomicU64::new(1),
        }
    }

    /// 注册一条流式接收端（在开流前登记，首个分片不会丢）。
    pub fn attach_stream(&self, stream_id: String, tx: mpsc::UnboundedSender<StreamMsg>) {
        self.streams.lock().unwrap().insert(stream_id, tx);
    }

    pub fn detach_stream(&self, stream_id: &str) {
        self.streams.lock().unwrap().remove(stream_id);
    }

    /// 下发一段流式数据；接收端已收场即丢弃（不阻塞 SDK worker）。
    pub fn push_chunk(&self, stream_id: &str, bytes: Vec<u8>) {
        if let Some(tx) = self.streams.lock().unwrap().get(stream_id) {
            let _ = tx.send(StreamMsg::Chunk(bytes));
        }
    }

    /// 流结束 / 失败 / 被取消。
    pub fn finish_stream(
        &self,
        stream_id: &str,
        error: Option<String>,
        cancelled: bool,
        status: Option<u16>,
    ) {
        if let Some(tx) = self.streams.lock().unwrap().remove(stream_id) {
            let _ = tx.send(StreamMsg::End { error, cancelled, status });
        }
    }

    pub fn next_id(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }

    /// 中断一个会话的 run（幂等）。
    pub fn stop(&self, session_id: &str) {
        if let Some(handle) = self.runs.lock().unwrap().get(session_id) {
            let _ = handle.cancel.send(true);
        }
    }

    pub fn stop_all(&self) {
        for (_, handle) in self.runs.lock().unwrap().iter() {
            let _ = handle.cancel.send(true);
        }
    }

    pub fn is_running(&self, session_id: &str) -> bool {
        self.runs.lock().unwrap().contains_key(session_id)
    }

    /// 是否还有活跃 run、在途命名或辅助调用（退出收尾等待用）。
    pub fn busy(&self) -> bool {
        !self.runs.lock().unwrap().is_empty()
            || !self.naming.lock().unwrap().is_empty()
            || self.aux.load(Ordering::Relaxed) > 0
    }

    pub fn begin_aux(&self) {
        self.aux.fetch_add(1, Ordering::Relaxed);
    }

    pub fn end_aux(&self) {
        self.aux.fetch_sub(1, Ordering::Relaxed);
    }
}

struct RunHandle {
    run_id: String,
    cancel: watch::Sender<bool>,
}

pub enum StreamMsg {
    Chunk(Vec<u8>),
    End {
        error: Option<String>,
        cancelled: bool,
        status: Option<u16>,
    },
}

/// run 的落点：存储型写会话真源；直写型只发快照（画布卡等由调用方持真源）。
#[derive(Clone, Copy)]
enum Persist {
    Store,
    Direct,
}

struct RunInput {
    persist: Persist,
    session_id: String,
    message_id: String,
    created_at: i64,
    config: RunConfigWire,
    history: Vec<Value>,
    compaction: Option<Value>,
    naming: Option<RunConfigWire>,
}

// ===== 步骤与展示行 =====

#[derive(Debug, Clone)]
enum Step {
    Reasoning(String),
    Narration(String),
    Tool(ToolRow),
}

impl Step {
    fn to_value(&self) -> Value {
        match self {
            Step::Reasoning(text) => json!({ "kind": "reasoning", "text": text }),
            Step::Narration(text) => json!({ "kind": "text", "text": text }),
            Step::Tool(row) => row.to_value(),
        }
    }
}

#[derive(Debug, Clone)]
struct ToolRow {
    run_id: String,
    name: String,
    status: &'static str,
    args: String,
    args_summary: String,
    result_summary: String,
    result: String,
    is_error: bool,
}

impl ToolRow {
    fn to_value(&self) -> Value {
        let mut run = json!({
            "id": self.run_id,
            "name": self.name,
            "status": self.status,
            "argsSummary": self.args_summary,
            "resultSummary": self.result_summary,
            "isError": self.is_error,
        });
        if !self.args.is_empty() {
            run["args"] = serde_json::from_str::<Value>(&self.args).unwrap_or(json!(self.args));
        }
        if !self.result.is_empty() {
            run["result"] = json!(self.result);
        }
        json!({ "kind": "tool", "run": run })
    }
}

struct RunState {
    steps: Vec<Step>,
    content: String,
    rev: u64,
}

impl RunState {
    fn push_reasoning(&mut self, text: &str) {
        if let Some(Step::Reasoning(prev)) = self.steps.last_mut() {
            prev.push_str(text);
            return;
        }
        self.steps.push(Step::Reasoning(text.to_string()));
    }

    fn steps_value(&self) -> Value {
        Value::Array(self.steps.iter().map(Step::to_value).collect())
    }

    fn message(&self, message_id: &str, created_at: i64) -> Value {
        let mut message = json!({
            "id": message_id,
            "role": "assistant",
            "content": self.content,
            "createdAt": created_at,
        });
        if !self.steps.is_empty() {
            message["steps"] = self.steps_value();
        }
        message
    }
}

/// 流式错误分级：传输级失败可重试一次；HTTP 错误与流开始后的失败不可重试。
enum StreamFailure {
    Transport(String),
    Http(String),
    MidStream(String),
    Aborted,
}

impl StreamFailure {
    fn text(&self) -> String {
        match self {
            StreamFailure::Transport(message) => format!("请求失败：{message}"),
            StreamFailure::Http(message) => format!("供应商返回错误：{message}"),
            StreamFailure::MidStream(message) => format!("流式响应中断：{message}"),
            StreamFailure::Aborted => "已中断".to_string(),
        }
    }
}

// ===== 对外入口 =====

/// 起一轮存储型 run：会话 / 用户消息 / 占位 assistant 先落真源，再起循环。
pub async fn start(
    ctx: Context,
    hub: Arc<RunHub>,
    params: RunStartParams,
) -> Result<RunStartResult, String> {
    validate_config(&params.config)?;
    if let Some(session_id) = params.session_id.as_deref() {
        if hub.is_running(session_id) {
            return Err("该会话已有进行中的回复".to_string());
        }
    }
    let new_session = params.session_id.is_none();
    let session_id = params.session_id.clone().unwrap_or_else(|| Uuid::new_v4().to_string());
    let message_id = Uuid::new_v4().to_string();
    let user_id = params
        .user_message
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let created_at = now_ms();
    let placeholder = json!({
        "id": message_id,
        "role": "assistant",
        "content": "",
        "createdAt": created_at,
    });
    let mut batch = CommitBatch::default();
    if new_session {
        batch.created.push(SessionDef {
            id: session_id.clone(),
            file: String::new(),
            title: None,
            agent_id: params.agent_id.clone(),
            compaction: None,
            created_at,
            updated_at: created_at,
            messages: Vec::new(),
        });
    }
    batch.appends.push(AppendBatch {
        session_id: session_id.clone(),
        messages: vec![params.user_message, placeholder],
    });
    if let Err(error) = ctx.chat_session_write(&batch).await {
        return Err(format!("会话消息写入失败：{error}"));
    }
    let record = match ctx.chat_session_read(&session_id).await {
        Ok(record) => record,
        Err(error) => {
            rollback_start(&ctx, &session_id, new_session, &user_id, &message_id).await;
            return Err(format!("会话读取失败：{error}"));
        }
    };
    let history = record.messages;
    let compaction = params.compaction.or(record.compaction);
    let input = RunInput {
        persist: Persist::Store,
        session_id: session_id.clone(),
        message_id: message_id.clone(),
        created_at,
        config: params.config,
        history,
        compaction,
        naming: params.naming,
    };
    if let Err((cleanup_ctx, error)) = spawn_run(ctx, hub, input) {
        rollback_start(&cleanup_ctx, &session_id, new_session, &user_id, &message_id).await;
        return Err(error);
    }
    Ok(RunStartResult { session_id, message_id })
}

/// 首轮写入后未能起跑：新会话整条删除，既有会话摘掉本轮 user 消息与占位 assistant。
async fn rollback_start(
    ctx: &Context,
    session_id: &str,
    new_session: bool,
    user_id: &str,
    message_id: &str,
) {
    if new_session {
        if let Err(error) = ctx.chat_delete(session_id).await {
            ctx.log_error(&format!("收回未起跑的会话失败（{session_id}）：{error}"));
        }
        return;
    }
    let mut removals = vec![MessageRemoval {
        session_id: session_id.to_string(),
        message_id: message_id.to_string(),
    }];
    if !user_id.is_empty() {
        removals.push(MessageRemoval {
            session_id: session_id.to_string(),
            message_id: user_id.to_string(),
        });
    }
    if let Err(error) = ctx
        .chat_session_write(&CommitBatch { message_removals: removals, ..Default::default() })
        .await
    {
        ctx.log_error(&format!("收回未起跑的本轮消息失败（{session_id}）：{error}"));
    }
}

/// 起一轮直写型 run：不写会话，只发布消息快照与终态。
pub async fn start_direct(
    ctx: Context,
    hub: Arc<RunHub>,
    params: RunDirectParams,
) -> Result<RunStartResult, String> {
    validate_config(&params.config)?;
    if hub.is_running(&params.session_id) {
        return Err("该会话已有进行中的回复".to_string());
    }
    let message_id = Uuid::new_v4().to_string();
    let created_at = now_ms();
    let mut history = params.history;
    history.push(params.user_message);
    let input = RunInput {
        persist: Persist::Direct,
        session_id: params.session_id.clone(),
        message_id: message_id.clone(),
        created_at,
        config: params.config,
        history,
        compaction: params.compaction,
        naming: None,
    };
    spawn_run(ctx, hub, input).map_err(|(_, error)| error)?;
    Ok(RunStartResult { session_id: params.session_id, message_id })
}

fn validate_config(config: &RunConfigWire) -> Result<(), String> {
    if config.base_url.trim().is_empty() {
        return Err("模型供应商地址为空".to_string());
    }
    if config.model.trim().is_empty() {
        return Err("未选择模型".to_string());
    }
    Ok(())
}

fn spawn_run(
    ctx: Context,
    hub: Arc<RunHub>,
    input: RunInput,
) -> Result<(), (Context, String)> {
    let session_id = input.session_id.clone();
    if hub.is_running(&session_id) {
        return Err((ctx, "该会话已有进行中的对话".to_string()));
    }
    let run_id = Uuid::new_v4().to_string();
    let (cancel_tx, cancel_rx) = watch::channel(false);
    hub.runs.lock().unwrap().insert(
        session_id.clone(),
        RunHandle { run_id: run_id.clone(), cancel: cancel_tx },
    );
    let thread_hub = Arc::clone(&hub);
    let thread_run_id = run_id.clone();
    let thread_session = session_id.clone();
    let cleanup_ctx = ctx.clone();
    std::thread::Builder::new()
        .name("atelyx-ai-run".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(runtime) => runtime,
                Err(error) => {
                    ctx.log_error(&format!("AI 运行线程运行时初始化失败：{error}"));
                    thread_hub.runs.lock().unwrap().remove(&thread_session);
                    return;
                }
            };
            let logger = ctx.clone();
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                runtime.block_on(run_loop(ctx, Arc::clone(&thread_hub), input, cancel_rx));
            }));
            if outcome.is_err() {
                logger.log_error("AI 运行线程异常退出");
            }
            let mut runs = thread_hub.runs.lock().unwrap();
            if runs.get(&thread_session).is_some_and(|handle| handle.run_id == thread_run_id) {
                runs.remove(&thread_session);
            }
            thread_hub.naming.lock().unwrap().remove(&thread_session);
        })
        .map_err(|error| {
            hub.runs.lock().unwrap().remove(&session_id);
            (cleanup_ctx, format!("启动 AI 运行线程失败：{error}"))
        })?;
    Ok(())
}

/// 停止全部 run 并等待它们收尾（插件退出；最多等 `wait`）。
pub async fn shutdown(hub: &Arc<RunHub>, wait: Duration) {
    hub.stop_all();
    let deadline = Instant::now() + wait;
    while hub.busy() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// ===== 主循环 =====

async fn run_loop(
    ctx: Context,
    hub: Arc<RunHub>,
    input: RunInput,
    mut cancel: watch::Receiver<bool>,
) {
    let RunInput {
        persist,
        session_id,
        message_id,
        created_at,
        config,
        mut history,
        compaction,
        naming,
    } = input;
    // 图片读回可能走网络：放到 run 线程里做，run.start 的回执不等它
    let from = crate::history::context_from(&history, compaction.as_ref());
    resolve_images(&ctx, &mut history[from..]).await;
    let mut llm_history = history_to_llm_messages(&history, compaction.as_ref());
    let tools = match ctx.tool_list(config.tool_ids.clone()).await {
        Ok(tools) => tools,
        Err(error) => {
            finish_failed(&ctx, persist, &session_id, &message_id, &error.to_string()).await;
            return;
        }
    };
    let defs: Vec<LlmToolDecl> = tools
        .iter()
        .map(|tool| LlmToolDecl {
            name: tool.name.clone(),
            description: tool.description.clone(),
            parameters: tool.parameters.clone(),
        })
        .collect();
    let parallel: HashMap<String, bool> =
        tools.iter().map(|tool| (tool.name.clone(), tool.parallel_safe)).collect();
    let mut state = RunState { steps: Vec::new(), content: String::new(), rev: 1 };
    let outcome = run_turns(
        &ctx,
        &hub,
        persist,
        &session_id,
        &message_id,
        created_at,
        &config,
        &defs,
        &parallel,
        &mut llm_history,
        &mut state,
        &mut cancel,
    )
    .await;
    match outcome {
        Ok(()) => {
            emit(&ctx, &session_id, &message_id, ChatEvent::Finished { reason: "stop".into() }).await;
            // 命名不占 run 名额：摘除 run 登记后转入命名集合，新消息可以立即开跑
            hub.runs.lock().unwrap().remove(&session_id);
            if let Some(naming) = naming {
                hub.naming.lock().unwrap().insert(session_id.clone());
                if let Some(title) = auto_title(&ctx, &hub, &session_id, &naming).await {
                    emit(&ctx, &session_id, &message_id, ChatEvent::Titled { title: Some(title) }).await;
                }
                hub.naming.lock().unwrap().remove(&session_id);
            }
        }
        Err(error) => {
            let aborted = matches!(error, StreamFailure::Aborted);
            finalize_after_failure(&ctx, persist, &session_id, &message_id, created_at, &mut state, &error).await;
            if aborted {
                emit(&ctx, &session_id, &message_id, ChatEvent::Finished { reason: "interrupted".into() }).await;
            } else {
                emit(&ctx, &session_id, &message_id, ChatEvent::Failed { error: error.text() }).await;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_turns(
    ctx: &Context,
    hub: &Arc<RunHub>,
    persist: Persist,
    session_id: &str,
    message_id: &str,
    created_at: i64,
    config: &RunConfigWire,
    defs: &[LlmToolDecl],
    parallel: &HashMap<String, bool>,
    llm_history: &mut Vec<LlmMessage>,
    state: &mut RunState,
    cancel: &mut watch::Receiver<bool>,
) -> Result<(), StreamFailure> {
    loop {
        if *cancel.borrow() {
            return Err(StreamFailure::Aborted);
        }
        let steps_base = state.steps.len();
        let mut last_commit = Instant::now() - COMMIT_INTERVAL;
        state.rev += 1;
        commit_patch(ctx, persist, session_id, message_id, created_at, state).await;
        let mut round_text = String::new();
        let mut calls: Vec<(usize, ToolCall)> = Vec::new();
        let messages = system_then(&config.system_prompt, llm_history);
        let body = request_body(&StreamRequest {
            model: config.model.clone(),
            messages,
            tools: defs.to_vec(),
            reasoning_effort: config.reasoning_effort.as_deref().and_then(ReasoningEffort::parse_wire),
            max_tokens: None,
        });
        let mut attempt = 0;
        loop {
            attempt += 1;
            let round = stream_round(
                ctx,
                hub,
                persist,
                session_id,
                message_id,
                created_at,
                config,
                &body,
                state,
                &mut round_text,
                &mut calls,
                cancel,
                &mut last_commit,
            )
            .await;
            match round {
                Ok(()) => break,
                Err(StreamFailure::Transport(_)) if attempt == 1 && !*cancel.borrow() => {
                    // 请求未建立 / 未收到任何事件：重试一次（流开始后不重试）
                    calls.clear();
                }
                Err(error) => return Err(error),
            }
        }
        if calls.is_empty() {
            state.content = round_text;
            state.rev += 1;
            commit_patch(ctx, persist, session_id, message_id, created_at, state).await;
            return Ok(());
        }
        // 叙述增量固化为叙述步：插到本轮首个工具行之前
        state.content.clear();
        if !round_text.is_empty() {
            let insert_at = state.steps[steps_base..]
                .iter()
                .position(|step| matches!(step, Step::Tool(_)))
                .map(|index| steps_base + index)
                .unwrap_or(state.steps.len());
            state.steps.insert(insert_at, Step::Narration(round_text.clone()));
        }
        fix_pending_rows(state, &calls);
        execute_calls(ctx, parallel, &calls, state, cancel).await;
        state.rev += 1;
        commit_patch(ctx, persist, session_id, message_id, created_at, state).await;
        llm_history.push(LlmMessage::Assistant {
            content: (!round_text.is_empty()).then_some(round_text),
            tool_calls: calls.iter().map(|(_, call)| call.clone()).collect(),
        });
        for step in state.steps.iter().skip(steps_base) {
            if let Step::Tool(row) = step {
                llm_history.push(LlmMessage::Tool {
                    tool_call_id: row.run_id.clone(),
                    content: if row.result.is_empty() {
                        row.result_summary.clone()
                    } else {
                        row.result.clone()
                    },
                });
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn stream_round(
    ctx: &Context,
    hub: &Arc<RunHub>,
    persist: Persist,
    session_id: &str,
    message_id: &str,
    created_at: i64,
    config: &RunConfigWire,
    body: &Value,
    state: &mut RunState,
    round_text: &mut String,
    calls: &mut Vec<(usize, ToolCall)>,
    cancel: &mut watch::Receiver<bool>,
    last_commit: &mut Instant,
) -> Result<(), StreamFailure> {
    let stream_id = format!("ai-{}-{}", session_id, hub.next_id());
    let (tx, mut rx) = mpsc::unbounded_channel();
    hub.attach_stream(stream_id.clone(), tx);
    let mut headers = BTreeMap::new();
    headers.insert("Authorization".to_string(), format!("Bearer {}", config.api_key));
    headers.insert("Content-Type".to_string(), "application/json".to_string());
    ctx.http_stream_open(HttpStreamOpenParams {
        stream_id: Some(stream_id.clone()),
        method: "POST".to_string(),
        url: format!("{}/chat/completions", config.base_url.trim_end_matches('/')),
        headers,
        body: Some(body.to_string()),
        // 推理模型两段输出之间可能长时间无数据；600s 只是防半开连接，不是请求总超时
        timeout_ms: Some(600_000),
    })
    .await
    .map_err(|error| {
        hub.detach_stream(&stream_id);
        StreamFailure::Transport(error.to_string())
    })?;
    let mut decoder = SseDecoder::new();
    let mut raw = String::new();
    let result = loop {
        tokio::select! {
            _ = cancel.changed() => {
                if *cancel.borrow() {
                    let _ = ctx.http_stream_cancel(&stream_id).await;
                    hub.detach_stream(&stream_id);
                    break Err(StreamFailure::Aborted);
                }
            }
            message = rx.recv() => match message {
                Some(StreamMsg::Chunk(bytes)) => {
                    raw.push_str(&String::from_utf8_lossy(&bytes));
                    for event in decoder.feed(&bytes) {
                        handle_event(
                            ctx, persist, session_id, message_id, created_at, state, round_text,
                            calls, event, last_commit,
                        )
                        .await;
                    }
                    if decoder.stop_reason().is_some() {
                        let _ = ctx.http_stream_cancel(&stream_id).await;
                        hub.detach_stream(&stream_id);
                        break Ok(());
                    }
                }
                Some(StreamMsg::End { error, cancelled, status }) => {
                    if cancelled || *cancel.borrow() {
                        hub.detach_stream(&stream_id);
                        break Err(StreamFailure::Aborted);
                    }
                    for event in decoder.end() {
                        handle_event(
                            ctx, persist, session_id, message_id, created_at, state, round_text,
                            calls, event, last_commit,
                        )
                        .await;
                    }
                    hub.detach_stream(&stream_id);
                    break if decoder.stop_reason().is_some() {
                        Ok(())
                    } else if let Some(error) = error {
                        let detail = if raw.trim().is_empty() {
                            error
                        } else {
                            format!("{error}（{}）", raw.chars().take(300).collect::<String>())
                        };
                        Err(if status.is_some_and(|status| !(200..300).contains(&status)) {
                            StreamFailure::Http(detail)
                        } else if decoder.has_seen_event() {
                            StreamFailure::MidStream(detail)
                        } else {
                            StreamFailure::Transport(detail)
                        })
                    } else if !decoder.has_seen_event() {
                        Err(StreamFailure::Transport("未收到任何流式事件".to_string()))
                    } else {
                        Ok(())
                    };
                }
                None => {
                    hub.detach_stream(&stream_id);
                    break Err(StreamFailure::Transport("流式连接已关闭".to_string()));
                }
            },
        }
    };
    if result.is_ok() {
        // 流内已给出终止事件：提交一次当前快照，保证面板拿到最新形态
        state.content = round_text.clone();
        commit_patch(ctx, persist, session_id, message_id, created_at, state).await;
    }
    result
}

#[allow(clippy::too_many_arguments)]
async fn handle_event(
    ctx: &Context,
    persist: Persist,
    session_id: &str,
    message_id: &str,
    created_at: i64,
    state: &mut RunState,
    round_text: &mut String,
    calls: &mut Vec<(usize, ToolCall)>,
    event: StreamEvent,
    last_commit: &mut Instant,
) {
    match event {
        StreamEvent::TextDelta(text) => {
            round_text.push_str(&text);
            if last_commit.elapsed() >= COMMIT_INTERVAL {
                state.content = round_text.clone();
                *last_commit = Instant::now();
                commit_patch(ctx, persist, session_id, message_id, created_at, state).await;
            }
        }
        StreamEvent::ReasoningDelta(text) => {
            state.push_reasoning(&text);
            if last_commit.elapsed() >= COMMIT_INTERVAL {
                *last_commit = Instant::now();
                commit_patch(ctx, persist, session_id, message_id, created_at, state).await;
            }
        }
        StreamEvent::ToolCallDelta { index, name, args_delta, .. } => {
            ensure_pending_row(state, index, name, &args_delta);
            if last_commit.elapsed() >= COMMIT_INTERVAL {
                *last_commit = Instant::now();
                commit_patch(ctx, persist, session_id, message_id, created_at, state).await;
            }
        }
        StreamEvent::ToolCall { index, call } => calls.push((index, call)),
        StreamEvent::Finish { .. } => {}
    }
}

async fn execute_calls(
    ctx: &Context,
    parallel: &HashMap<String, bool>,
    calls: &[(usize, ToolCall)],
    state: &mut RunState,
    cancel: &mut watch::Receiver<bool>,
) {
    let is_parallel_safe = |name: &str| parallel.get(name).copied().unwrap_or(false);
    let mut index = 0;
    while index < calls.len() {
        if *cancel.borrow() {
            return;
        }
        let (_, call) = &calls[index];
        if !parallel.contains_key(&call.name) {
            settle_tool_row(
                state,
                call,
                Err(format!("工具不可用：{}（不在本轮工具名册中）", call.name)),
            );
            index += 1;
            continue;
        }
        if !is_parallel_safe(&call.name) {
            let result = tokio::select! {
                result = ctx.tool_execute(&call.name, parse_args(&call.arguments)) => {
                    result.map_err(|error| error.to_string())
                }
                _ = cancel.changed() => Err("工具调用已中断".to_string()),
            };
            settle_tool_row(state, call, result);
            if *cancel.borrow() {
                return;
            }
            index += 1;
            continue;
        }
        let mut end = index;
        while end < calls.len() && is_parallel_safe(&calls[end].1.name) {
            end += 1;
        }
        for chunk in calls[index..end].chunks(MAX_PARALLEL_TOOLS) {
            let mut tasks = tokio::task::JoinSet::new();
            for (_, call) in chunk {
                let ctx = ctx.clone();
                let call = call.clone();
                tasks.spawn(async move {
                    let result = ctx
                        .tool_execute(&call.name, parse_args(&call.arguments))
                        .await
                        .map_err(|error| error.to_string());
                    (call.id.clone(), result)
                });
            }
            let mut outcomes: Vec<(String, Result<method::ToolExecResult, String>)> = Vec::new();
            while !tasks.is_empty() {
                tokio::select! {
                    outcome = tasks.join_next() => {
                        if let Some(Ok(pair)) = outcome {
                            outcomes.push(pair);
                        }
                    }
                    _ = cancel.changed() => tasks.abort_all(),
                }
            }
            for (_, call) in chunk {
                let result = outcomes
                    .iter()
                    .find(|(id, _)| id == &call.id)
                    .map(|(_, result)| result.clone())
                    .unwrap_or_else(|| Err("工具调用已中断".to_string()));
                settle_tool_row(state, call, result);
            }
            if *cancel.borrow() {
                return;
            }
        }
        index = end;
    }
}

fn settle_tool_row(
    state: &mut RunState,
    call: &ToolCall,
    result: Result<method::ToolExecResult, String>,
) {
    let (text, is_error) = match result {
        Ok(result) => (result.content, result.is_error),
        Err(error) => (error, true),
    };
    let summary: String = text.chars().take(RESULT_SUMMARY_CHARS).collect();
    if let Some(Step::Tool(row)) = state
        .steps
        .iter_mut()
        .rev()
        .find(|step| matches!(step, Step::Tool(row) if row.run_id == call.id))
    {
        row.status = if is_error { "error" } else { "done" };
        row.is_error = is_error;
        row.result = text;
        row.result_summary = summary;
    }
}

// ===== 提交与事件 =====

async fn commit_patch(
    ctx: &Context,
    persist: Persist,
    session_id: &str,
    message_id: &str,
    created_at: i64,
    state: &RunState,
) {
    let message = state.message(message_id, created_at);
    if let Persist::Store = persist {
        let batch = CommitBatch {
            patches: vec![PatchEntry {
                session_id: session_id.to_string(),
                message_id: message_id.to_string(),
                rev: state.rev,
                content: Some(state.content.clone()),
                steps: Some(state.steps_value()),
            }],
            ..Default::default()
        };
        if let Err(error) = ctx.chat_session_write(&batch).await {
            ctx.log_error(&format!("会话补丁写入失败：{error}"));
        }
    }
    emit(
        ctx,
        session_id,
        message_id,
        ChatEvent::Message { rev: state.rev, message },
    )
    .await;
}

async fn emit(ctx: &Context, session_id: &str, message_id: &str, event: ChatEvent) {
    let params = ChatEventParams {
        session_id: session_id.to_string(),
        message_id: message_id.to_string(),
        event,
    };
    if let Err(error) = ctx.chat_emit(params).await {
        ctx.log_error(&format!("对话事件发布失败：{error}"));
    }
}

async fn finish_failed(
    ctx: &Context,
    persist: Persist,
    session_id: &str,
    message_id: &str,
    error: &str,
) {
    if let Persist::Store = persist {
        if let Err(write_error) = ctx
            .chat_session_write(&CommitBatch {
                message_removals: vec![MessageRemoval {
                    session_id: session_id.to_string(),
                    message_id: message_id.to_string(),
                }],
                ..Default::default()
            })
            .await
        {
            ctx.log_error(&format!("失败后移除占位消息失败（{message_id}）：{write_error}"));
        }
    }
    emit(ctx, session_id, message_id, ChatEvent::Failed { error: error.to_string() }).await;
}

async fn finalize_after_failure(
    ctx: &Context,
    persist: Persist,
    session_id: &str,
    message_id: &str,
    created_at: i64,
    state: &mut RunState,
    failure: &StreamFailure,
) {
    let aborted = matches!(failure, StreamFailure::Aborted);
    for step in state.steps.iter_mut() {
        if let Step::Tool(row) = step {
            if row.status == "running" {
                row.status = "error";
                row.result_summary = "（已中断）".to_string();
                row.is_error = true;
            }
        }
    }
    if aborted && state.content.is_empty() && state.steps.is_empty() {
        if let Persist::Store = persist {
            if let Err(write_error) = ctx
                .chat_session_write(&CommitBatch {
                    message_removals: vec![MessageRemoval {
                        session_id: session_id.to_string(),
                        message_id: message_id.to_string(),
                    }],
                    ..Default::default()
                })
                .await
            {
                ctx.log_error(&format!("中断后移除空占位消息失败（{message_id}）：{write_error}"));
            }
        } else {
            emit(ctx, session_id, message_id, ChatEvent::Removed).await;
        }
        return;
    }
    if !aborted && state.content.is_empty() {
        state.content = format!("[错误]{}", failure.text());
    }
    state.rev += 1;
    commit_patch(ctx, persist, session_id, message_id, created_at, state).await;
}

// ===== 工具行 =====

fn ensure_pending_row(state: &mut RunState, index: usize, name: Option<String>, args_delta: &str) {
    let run_id = format!("{PENDING_RUN_ID_PREFIX}{index}");
    if let Some(Step::Tool(row)) = state
        .steps
        .iter_mut()
        .rev()
        .find(|step| matches!(step, Step::Tool(row) if row.run_id == run_id))
    {
        row.args.push_str(args_delta);
        if let Some(name) = name {
            row.name = name;
        }
        row.args_summary =
            args_summary(&serde_json::from_str::<Value>(&row.args).unwrap_or(json!({})));
    } else {
        state.steps.push(Step::Tool(ToolRow {
            run_id,
            name: name.unwrap_or_default(),
            status: "running",
            args: args_delta.to_string(),
            args_summary: String::new(),
            result_summary: String::new(),
            result: String::new(),
            is_error: false,
        }));
    }
}

fn fix_pending_rows(state: &mut RunState, calls: &[(usize, ToolCall)]) {
    for (index, call) in calls {
        let row = ToolRow {
            run_id: call.id.clone(),
            name: call.name.clone(),
            status: "running",
            args: call.arguments.clone(),
            args_summary: args_summary(
                &serde_json::from_str::<Value>(&call.arguments).unwrap_or(json!({})),
            ),
            result_summary: String::new(),
            result: String::new(),
            is_error: false,
        };
        let pending_id = format!("{PENDING_RUN_ID_PREFIX}{index}");
        if let Some(position) = state
            .steps
            .iter()
            .position(|step| matches!(step, Step::Tool(row) if row.run_id == pending_id))
        {
            state.steps[position] = Step::Tool(row);
        } else {
            state.steps.push(Step::Tool(row));
        }
    }
}

fn parse_args(raw: &str) -> Value {
    serde_json::from_str::<Value>(raw).unwrap_or_else(|_| json!({ "_raw": raw }))
}

/// 参数摘要（对象键排序、长值截断；数组/标量原样截断）。
fn args_summary(args: &Value) -> String {
    match args {
        Value::Object(map) => {
            let mut parts: Vec<String> = map
                .iter()
                .map(|(key, value)| {
                    let text = match value {
                        Value::String(text) => text.clone(),
                        other => other.to_string(),
                    };
                    let mut cut: String = text.chars().take(60).collect();
                    if text.chars().count() > 60 {
                        cut.push('…');
                    }
                    format!("{key}: {cut}")
                })
                .collect();
            parts.sort();
            parts.join("，")
        }
        Value::Null => String::new(),
        other => {
            let mut text = other.to_string();
            if text.chars().count() > 60 {
                text = text.chars().take(60).collect::<String>() + "…";
            }
            text
        }
    }
}

// ===== 附件 / 辅助调用 =====

async fn resolve_images(ctx: &Context, messages: &mut [Value]) {
    for message in messages.iter_mut() {
        if message.get("role").and_then(Value::as_str) != Some("user") {
            continue;
        }
        let Some(attachments) = message.get_mut("attachments").and_then(Value::as_array_mut)
        else {
            continue;
        };
        for attachment in attachments {
            if attachment.get("kind").and_then(Value::as_str) != Some("image") {
                continue;
            }
            if attachment
                .get("payload")
                .and_then(Value::as_str)
                .is_some_and(|payload| !payload.is_empty())
            {
                continue;
            }
            let Some(file) = attachment.get("file").and_then(Value::as_str) else {
                continue;
            };
            let mime = attachment
                .get("mime")
                .and_then(Value::as_str)
                .unwrap_or("application/octet-stream");
            match ctx.vault_read_binary(file).await {
                Ok(bytes) => {
                    let data = base64::engine::general_purpose::STANDARD.encode(bytes);
                    attachment["payload"] = json!(format!("data:{mime};base64,{data}"));
                }
                Err(error) => ctx.log_error(&format!("图片附件读取失败（{file}）：{error}")),
            }
        }
    }
}

fn system_then(system: &Option<String>, history: &[LlmMessage]) -> Vec<LlmMessage> {
    let mut out = Vec::with_capacity(history.len() + 1);
    if let Some(system) = system {
        if !system.trim().is_empty() {
            out.push(LlmMessage::System { content: system.clone() });
        }
    }
    out.extend(history.iter().cloned());
    out
}

/// 辅助调用（命名 / 压缩）：非工具、非流式消费，返回正文文本。
async fn aux_text(
    ctx: &Context,
    hub: &Arc<RunHub>,
    config: &RunConfigWire,
    system: &str,
    user: &str,
    max_tokens: u32,
) -> Result<String, String> {
    let body = request_body(&StreamRequest {
        model: config.model.clone(),
        messages: vec![
            LlmMessage::System { content: system.to_string() },
            LlmMessage::User { parts: vec![user.to_string()], images: Vec::new() },
        ],
        tools: Vec::new(),
        reasoning_effort: None,
        max_tokens: Some(max_tokens),
    });
    let stream_id = format!("ai-aux-{}", Uuid::new_v4());
    let (tx, mut rx) = mpsc::unbounded_channel();
    hub.attach_stream(stream_id.clone(), tx);
    let mut headers = BTreeMap::new();
    headers.insert("Authorization".to_string(), format!("Bearer {}", config.api_key));
    headers.insert("Content-Type".to_string(), "application/json".to_string());
    if let Err(error) = ctx
        .http_stream_open(HttpStreamOpenParams {
            stream_id: Some(stream_id.clone()),
            method: "POST".to_string(),
            url: format!("{}/chat/completions", config.base_url.trim_end_matches('/')),
            headers,
            body: Some(body.to_string()),
            timeout_ms: Some(600_000),
        })
        .await
    {
        hub.detach_stream(&stream_id);
        return Err(error.to_string());
    }
    let mut decoder = SseDecoder::new();
    let mut text = String::new();
    let collect = async {
        loop {
            match rx.recv().await {
                Some(StreamMsg::Chunk(bytes)) => {
                    for event in decoder.feed(&bytes) {
                        if let StreamEvent::TextDelta(delta) = event {
                            text.push_str(&delta);
                        }
                    }
                    if decoder.stop_reason().is_some() {
                        return Ok(());
                    }
                }
                Some(StreamMsg::End { error, cancelled, .. }) => {
                    for event in decoder.end() {
                        if let StreamEvent::TextDelta(delta) = event {
                            text.push_str(&delta);
                        }
                    }
                    if cancelled {
                        return Err("辅助调用已取消".to_string());
                    }
                    return match error {
                        Some(error) if decoder.stop_reason().is_none() => Err(error),
                        _ => Ok(()),
                    };
                }
                None => return Err("流式连接已关闭".to_string()),
            }
        }
    };
    let outcome = match tokio::time::timeout(AUX_TIMEOUT, collect).await {
        Ok(result) => result,
        Err(_) => Err(format!("辅助调用超时（{}s）", AUX_TIMEOUT.as_secs())),
    };
    let _ = ctx.http_stream_cancel(&stream_id).await;
    hub.detach_stream(&stream_id);
    if let Err(error) = outcome {
        // 部分内容不足以作为摘要 / 标题：把失败如实交给调用方，不静默降级成成功
        return Err(error);
    }
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err("模型未返回内容".to_string());
    }
    Ok(text)
}

async fn auto_title(
    ctx: &Context,
    hub: &Arc<RunHub>,
    session_id: &str,
    config: &RunConfigWire,
) -> Option<String> {
    let record = ctx.chat_session_read(session_id).await.ok()?;
    if record.title.as_deref().is_some_and(|title| !title.trim().is_empty()) {
        return None;
    }
    let excerpt: String = record
        .messages
        .iter()
        .filter_map(|message| {
            let role = message.get("role").and_then(Value::as_str)?;
            let content = message.get("content").and_then(Value::as_str)?;
            if content.is_empty() {
                return None;
            }
            Some(format!("{role}: {content}"))
        })
        .collect::<Vec<_>>()
        .join("\n\n")
        .chars()
        .take(2000)
        .collect();
    if excerpt.is_empty() {
        return None;
    }
    let raw = aux_text(
        ctx,
        hub,
        config,
        "你负责为对话生成标题。只输出标题本身（不超过 16 字），不要引号、解释、标点或编号。",
        &format!("为下面的对话起一个简洁的标题：\n\n{excerpt}"),
        512,
    )
    .await
    .ok()?;
    let title = clean_title(&raw);
    if title.is_empty() {
        return None;
    }
    // 命名期间用户可能已手动改名：写前复核，不覆盖非空标题
    if ctx
        .chat_session_read(session_id)
        .await
        .ok()
        .and_then(|record| record.title)
        .is_some_and(|title| !title.trim().is_empty())
    {
        return None;
    }
    let batch = CommitBatch {
        metas: vec![MetaPatch {
            id: session_id.to_string(),
            title: Some(title.clone()),
            ..Default::default()
        }],
        ..Default::default()
    };
    if let Err(error) = ctx.chat_session_write(&batch).await {
        ctx.log_error(&format!("会话标题写入失败：{error}"));
        return None;
    }
    Some(title)
}

/// 压缩一个会话：把较早消息总结为压缩注解（非破坏）。
pub async fn compact(
    ctx: &Context,
    hub: &Arc<RunHub>,
    session_id: &str,
    config: &RunConfigWire,
) -> Result<Value, String> {
    validate_config(config)?;
    if hub.is_running(session_id) {
        return Err("该会话有进行中的回复，暂不能压缩".to_string());
    }
    let record = ctx
        .chat_session_read(session_id)
        .await
        .map_err(|error| format!("会话读取失败：{error}"))?;
    if record.messages.len() < 2 {
        return Err("对话内容太少，无需压缩".to_string());
    }
    let transcript = transcript_of(&record.messages, record.compaction.as_ref());
    let summary = aux_text(
        ctx,
        hub,
        config,
        "你负责压缩对话历史。输出结构化检查点：会话主题 / 已完成 / 关键决策 / 产出文件 / 待办 / 当前状态。只输出检查点本身。",
        &format!("压缩以下对话历史：\n\n{transcript}"),
        2048,
    )
    .await?;
    let anchor = record
        .messages
        .last()
        .and_then(|message| message.get("id"))
        .and_then(Value::as_str)
        .ok_or_else(|| "会话没有可锚定的消息".to_string())?
        .to_string();
    let count = record.messages.len();
    let batch = CommitBatch {
        metas: vec![MetaPatch {
            id: session_id.to_string(),
            compaction: Some(json!({
                "summary": summary,
                "upToMessageId": anchor,
                "messageCount": count,
                "createdAt": now_ms(),
            })),
            ..Default::default()
        }],
        ..Default::default()
    };
    ctx.chat_session_write(&batch)
        .await
        .map_err(|error| format!("压缩摘要写入失败：{error}"))?;
    Ok(json!({}))
}

/// 压缩一段调用方给出的消息：只回摘要，不读写会话记录。
pub async fn compact_direct(
    ctx: &Context,
    hub: &Arc<RunHub>,
    messages: &[Value],
    compaction: Option<&Value>,
    config: &RunConfigWire,
) -> Result<Value, String> {
    validate_config(config)?;
    if messages.len() < 2 {
        return Err("对话内容太少，无需压缩".to_string());
    }
    let transcript = transcript_of(messages, compaction);
    let summary = aux_text(
        ctx,
        hub,
        config,
        "你负责压缩对话历史。输出结构化检查点：会话主题 / 已完成 / 关键决策 / 产出文件 / 待办 / 当前状态。只输出检查点本身。",
        &format!("压缩以下对话历史：\n\n{transcript}"),
        2048,
    )
    .await?;
    Ok(json!({ "summary": summary }))
}

/// 把消息转成压缩用的纯文本记录：工具调用与结果也进入记录，避免摘要丢掉执行事实。
fn transcript_of(messages: &[Value], compaction: Option<&Value>) -> String {
    let mut transcript = String::new();
    for message in history_to_llm_messages(messages, compaction) {
        match message {
            LlmMessage::User { parts, .. } => {
                transcript.push_str(&format!("用户：{}\n\n", parts.join("\n")));
            }
            LlmMessage::Assistant { content, tool_calls } => {
                let body = match content {
                    Some(content) if !content.is_empty() => content,
                    _ if !tool_calls.is_empty() => format!(
                        "（调用工具 {}）",
                        tool_calls.iter().map(|call| call.name.as_str()).collect::<Vec<_>>().join("、")
                    ),
                    _ => String::new(),
                };
                transcript.push_str(&format!("助手：{body}\n\n"));
            }
            LlmMessage::Tool { content, .. } => {
                transcript.push_str(&format!("工具结果：{content}\n\n"));
            }
            LlmMessage::System { .. } => {}
        }
    }
    transcript
}

pub async fn retitle(
    ctx: &Context,
    hub: &Arc<RunHub>,
    session_id: &str,
    config: &RunConfigWire,
) -> Result<Value, String> {
    validate_config(config)?;
    if hub.is_running(session_id) {
        return Err("该会话有进行中的回复，暂不能重新命名".to_string());
    }
    let record = ctx
        .chat_session_read(session_id)
        .await
        .map_err(|error| format!("会话读取失败：{error}"))?;
    let excerpt: String = record
        .messages
        .iter()
        .filter_map(|message| {
            let role = message.get("role").and_then(Value::as_str)?;
            let content = message.get("content").and_then(Value::as_str)?;
            if content.is_empty() {
                return None;
            }
            Some(format!("{role}: {content}"))
        })
        .collect::<Vec<_>>()
        .join("\n\n")
        .chars()
        .take(2000)
        .collect();
    if excerpt.is_empty() {
        return Err("会话没有可命名的内容".to_string());
    }
    let raw = aux_text(
        ctx,
        hub,
        config,
        "你负责为对话生成标题。只输出标题本身（不超过 16 字），不要引号、解释、标点或编号。",
        &format!("为下面的对话起一个简洁的标题：\n\n{excerpt}"),
        512,
    )
    .await?;
    let title = clean_title(&raw);
    if title.is_empty() {
        return Err("命名结果为空".to_string());
    }
    let batch = CommitBatch {
        metas: vec![MetaPatch {
            id: session_id.to_string(),
            title: Some(title.clone()),
            ..Default::default()
        }],
        ..Default::default()
    };
    ctx.chat_session_write(&batch)
        .await
        .map_err(|error| format!("会话标题写入失败：{error}"))?;
    Ok(json!({ "title": title }))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}
