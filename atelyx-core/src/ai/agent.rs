//! Agent 循环运行时：run 是并发单位——同一会话至多一个活跃 run，多会话可并行。
//! 会话变更先经 commit 落真源（模型可见即已记录），再以 `RunEvent` 通知编排方。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tokio::sync::watch;

use crate::ai::{PENDING_RUN_ID_PREFIX};
use crate::ai::session::{AppendBatch, CommitBatch, PatchEntry, SessionStore};
use crate::ai::stream::{
    LlmMessage, ReasoningEffort, StreamEvent, StreamRequest, ToolCall, ToolDecl,
};
use crate::ai::tools::{builtin_tools, resolve_image_attachments, SearchConfig, ToolCtx};

/// 工具并行段上限（同一 parallel_safe 段内最多同时执行的工具数）。
const MAX_PARALLEL_TOOLS: usize = 10;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn uuid_v4() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// 一次 run 的完整配置（供应商路由、模型、Agent 名册、工具环境）。
/// 由调用方（壳 / 插件服务面）从仓库 AI 设置解析后传入，本模块不做配置读取。
#[derive(Clone)]
pub struct RunConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub reasoning_effort: Option<ReasoningEffort>,
    /// 系统提示词（Agent 提示词 + 提示词标记注入后的最终文本；None = 无 system）。
    pub system_prompt: Option<String>,
    /// Agent 勾选的内置工具 id 清单。
    pub tool_ids: Vec<String>,
    pub search: SearchConfig,
    pub exclude: Vec<String>,
}

impl std::fmt::Debug for RunConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // api_key 不进调试输出（该结构随消息/载荷派生 Debug，明文不得外泄）
        f.debug_struct("RunConfig")
            .field("base_url", &self.base_url)
            .field("api_key", &"<redacted>")
            .field("model", &self.model)
            .field("reasoning_effort", &self.reasoning_effort)
            .field("system_prompt", &self.system_prompt)
            .field("tool_ids", &self.tool_ids)
            .field("search", &self.search)
            .field("exclude", &self.exclude)
            .finish()
    }
}

/// 编排方可见的 run 事件（会话 + 消息双重身份携带，防迟到事件串会话）。
#[derive(Debug, Clone)]
pub enum RunEvent {
    Started {
        session_id: String,
        message_id: String,
    },
    TextDelta {
        session_id: String,
        message_id: String,
        text: String,
    },
    ReasoningDelta {
        session_id: String,
        message_id: String,
        text: String,
    },
    ToolStart {
        session_id: String,
        message_id: String,
        run_id: String,
        name: String,
        args_summary: String,
    },
    ToolEnd {
        session_id: String,
        message_id: String,
        run_id: String,
        is_error: bool,
        result_summary: String,
    },
    Finished {
        session_id: String,
        message_id: String,
        /// 收敛原因：`stop` = 正常收敛；`interrupted` = 用户中断（行已归一）。
        reason: String,
    },
    Failed {
        session_id: String,
        message_id: String,
        error: String,
    },
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

/// 工具步行（disk 形状与壳渲染共用）。
#[derive(Debug, Clone)]
pub struct ToolRow {
    pub run_id: String,
    pub name: String,
    pub status: &'static str, // running | done | error
    pub args: String,
    pub args_summary: String,
    pub result_summary: String,
    pub result: String,
    pub is_error: bool,
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

/// 参数摘要（对象键排序、长值截断；数组/标量原样截断）。
pub fn args_summary(args: &Value) -> String {
    match args {
        Value::Object(map) => {
            let mut parts: Vec<String> = map
                .iter()
                .map(|(k, v)| {
                    let s = match v {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    let mut cut: String = s.chars().take(60).collect();
                    if s.chars().count() > 60 {
                        cut.push('…');
                    }
                    format!("{k}: {cut}")
                })
                .collect();
            parts.sort();
            parts.join("，")
        }
        Value::Null => String::new(),
        other => {
            let mut s = other.to_string();
            if s.chars().count() > 60 {
                s = s.chars().take(60).collect::<String>() + "…";
            }
            s
        }
    }
}

// ===== 历史转换（纯函数）=====

/// 会话消息（disk 形状）→ 线上消息序列。
/// 语义：user 原样（refs 携带的文本段并入）；assistant 按 steps 展开——每工具轮一条
/// assistant(tool_calls) + 逐个 role:tool；思考步不进上下文；纯叙述轮降级为普通文本；
/// 最终正文收尾一条 assistant；`[错误]` 占位与 system 角色过滤；压缩注解非破坏折叠
/// （锚点及其前替换为摘要块，锚点缺失退回全历史）。
pub fn history_to_llm_messages(
    messages: &[Value],
    compaction: Option<&Value>,
) -> Vec<LlmMessage> {
    let slice = apply_compaction(messages, compaction);
    let mut out = Vec::new();
    for m in &slice {
        let role = m.get("role").and_then(Value::as_str).unwrap_or("");
        let content = m.get("content").and_then(Value::as_str).unwrap_or("");
        match role {
            "system" => {}
            "user" => {
                let mut parts = vec![content.to_string()];
                if let Some(refs) = m.get("refs").and_then(Value::as_array) {
                    for r in refs {
                        if let Some(text) = r.get("text").and_then(Value::as_str) {
                            parts.push(text.to_string());
                        }
                    }
                }
                // 图片附件：payload 为调用方读回的 dataURL（空/缺 = 读回失败，该附件跳过）
                let images: Vec<String> = m
                    .get("attachments")
                    .and_then(Value::as_array)
                    .map(|atts| {
                        atts.iter()
                            .filter(|a| a.get("kind").and_then(Value::as_str) == Some("image"))
                            .filter_map(|a| a.get("payload").and_then(Value::as_str))
                            .filter(|p| !p.is_empty())
                            .map(|p| p.to_string())
                            .collect()
                    })
                    .unwrap_or_default();
                out.push(LlmMessage::User { parts, images });
            }
            "assistant" => expand_assistant(content, m.get("steps"), &mut out),
            _ => {}
        }
    }
    out
}

/// 压缩折叠：锚点消息及其之前的内容替换为一条摘要 user 块。
fn apply_compaction(messages: &[Value], compaction: Option<&Value>) -> Vec<Value> {
    let Some(c) = compaction else {
        return messages.to_vec();
    };
    let anchor = c.get("upToMessageId").and_then(Value::as_str);
    let summary = c.get("summary").and_then(Value::as_str).unwrap_or("");
    let (Some(anchor), false) = (anchor, summary.is_empty()) else {
        return messages.to_vec();
    };
    let Some(idx) = messages
        .iter()
        .position(|m| m.get("id").and_then(Value::as_str) == Some(anchor))
    else {
        return messages.to_vec();
    };
    let mut out = Vec::with_capacity(messages.len() - idx);
    out.push(json!({
        "id": "__compaction__",
        "role": "user",
        "content": format!("[以下是本会话早前内容的压缩摘要]\n\n{summary}"),
        "createdAt": 0,
    }));
    out.extend(messages[idx + 1..].iter().cloned());
    out
}

fn expand_assistant(content: &str, steps: Option<&Value>, out: &mut Vec<LlmMessage>) {
    let Some(steps) = steps.and_then(Value::as_array) else {
        if visible_content(content) {
            out.push(LlmMessage::Assistant {
                content: Some(content.to_string()),
                tool_calls: Vec::new(),
            });
        }
        return;
    };
    // 分组：每轮 = 思考/叙述段 + 其后连续工具行；末轮（无工具行）为最终正文候选
    let mut rounds: Vec<(String, Vec<&Value>)> = Vec::new();
    let mut narration = String::new();
    // 当前轮是否已收工具：同一轮的连续工具调用必须归进同一条 assistant(tool_calls)，
    // 拆成多条会与实时请求形状不一致（下一轮请求的历史保真度受损）
    let mut round_open = false;
    for s in steps {
        match s.get("kind").and_then(Value::as_str) {
            Some("text") => {
                if let Some(t) = s.get("text").and_then(Value::as_str) {
                    narration.push_str(t);
                }
                round_open = false; // 叙述之后再有工具 = 新一轮
            }
            Some("tool") => {
                if round_open {
                    if let Some(last) = rounds.last_mut() {
                        last.1.push(s);
                    }
                } else {
                    rounds.push((std::mem::take(&mut narration), vec![s]));
                    round_open = true;
                }
            }
            _ => {} // reasoning 不进上下文
        }
    }
    rounds.push((narration, Vec::new()));
    let last = rounds.len() - 1;
    for (i, (texts, tools)) in rounds.into_iter().enumerate() {
        let calls: Vec<ToolCall> = tools
            .iter()
            .filter_map(|t| {
                let run = t.get("run")?;
                let id = run.get("id").and_then(Value::as_str)?;
                if id.starts_with(PENDING_RUN_ID_PREFIX) {
                    return None; // 合成行不可发送
                }
                Some(ToolCall {
                    id: id.to_string(),
                    name: run.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                    arguments: run
                        .get("args")
                        .map(|a| a.to_string())
                        .unwrap_or_else(|| "{}".to_string()),
                })
            })
            .collect();
        if !calls.is_empty() {
            out.push(LlmMessage::Assistant {
                // 纯工具轮正文置 null（空串部分网关 500）
                content: if texts.is_empty() { None } else { Some(texts) },
                tool_calls: calls,
            });
            for t in tools {
                if let Some(run) = t.get("run") {
                    let id = run.get("id").and_then(Value::as_str).unwrap_or("");
                    if id.is_empty() || id.starts_with(PENDING_RUN_ID_PREFIX) {
                        continue;
                    }
                    let text = run
                        .get("result")
                        .and_then(Value::as_str)
                        .or_else(|| run.get("resultSummary").and_then(Value::as_str))
                        .unwrap_or("（无结果）");
                    out.push(LlmMessage::Tool {
                        tool_call_id: id.to_string(),
                        content: text.to_string(),
                    });
                }
            }
        } else if i == last && visible_content(content) {
            out.push(LlmMessage::Assistant {
                content: Some(content.to_string()),
                tool_calls: Vec::new(),
            });
        } else if !texts.is_empty() {
            out.push(LlmMessage::Assistant {
                content: Some(texts),
                tool_calls: Vec::new(),
            });
        }
    }
}

fn visible_content(content: &str) -> bool {
    !content.is_empty() && !content.starts_with("[错误]")
}

// ===== 运行枢纽 =====

/// run 注册表：会话 → (取消信号, run 身份)。收尾摘除按身份比对，
/// 防止旧 run 的收尾摘掉其后继 run 的登记（击穿单 run 不变量）。
pub struct AgentHub {
    runs: std::sync::Mutex<HashMap<String, (watch::Sender<bool>, String)>>,
}

impl Default for AgentHub {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentHub {
    pub fn new() -> Self {
        Self {
            runs: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// 会话是否有活跃 run（同步读；不得在 async 上下文调用）。
    pub fn is_running(&self, session_id: &str) -> bool {
        self.runs.lock().unwrap_or_else(|e| e.into_inner()).contains_key(session_id)
    }

    /// 停止会话的活跃 run（在途流被截断、未启动的工具不再执行）。返回是否有 run 被停。
    pub fn stop(&self, session_id: &str) -> bool {
        let tx = self
            .runs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(session_id);
        tx.map(|(tx, _)| tx.send(true).is_ok()).unwrap_or(false)
    }

    /// 停止全部活跃 run（换仓 / 脱离仓库时调用）。
    pub fn stop_all(&self) {
        for (_, (tx, _)) in self.runs.lock().unwrap_or_else(|e| e.into_inner()).drain() {
            let _ = tx.send(true);
        }
    }

    /// 启动 run：注册取消信号 → 占位 assistant 消息落真源 → 起循环任务。
    /// 返回占位 assistant 消息 id（编排方回执发起方用）。会话已有活跃 run 时拒绝（run 不变量）。
    #[allow(clippy::too_many_arguments)]
    pub async fn start(
        self: &Arc<Self>,
        store: Arc<SessionStore>,
        events: tokio::sync::mpsc::UnboundedSender<RunEvent>,
        session_id: String,
        compaction: Option<Value>,
        config: RunConfig,
        history: Vec<Value>,
        ctx: ToolCtx,
    ) -> Result<String, String> {
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let message_id = uuid_v4();
        let run_id = uuid_v4();
        {
            let mut runs = self.runs.lock().unwrap_or_else(|e| e.into_inner());
            if runs.contains_key(&session_id) {
                return Err("该会话已有进行中的对话".to_string());
            }
            runs.insert(session_id.clone(), (cancel_tx, run_id.clone()));
        }
        let created_at = now_ms();
        // 占位 assistant 消息即落真源（模型可见即已记录的起点）
        let commit = store.commit(CommitBatch {
            appends: vec![AppendBatch {
                session_id: session_id.clone(),
                messages: vec![json!({
                    "id": message_id,
                    "role": "assistant",
                    "content": "",
                    "createdAt": created_at,
                })],
            }],
            ..Default::default()
        });
        if let Err(e) = commit {
            self.runs
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&session_id);
            return Err(format!("会话占位消息写入失败：{e}"));
        }
        let _ = events.send(RunEvent::Started {
            session_id: session_id.clone(),
            message_id: message_id.clone(),
        });
        let hub = Arc::clone(self);
        let sid = session_id.clone();
        let reply_message_id = message_id.clone();
        tokio::spawn(async move {
            run_loop(RunParams {
                store,
                events,
                session_id: sid.clone(),
                message_id,
                created_at,
                config,
                history,
                compaction,
                ctx,
                cancel: cancel_rx,
            })
            .await;
            // 收尾按 run 身份摘除：仅当仍是本 run 时移除（防摘掉后继 run 的登记）
            hub.runs
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .retain(|_, (_, rid)| rid != &run_id);
        });
        Ok(reply_message_id)
    }
}

struct RunParams {
    store: Arc<SessionStore>,
    events: tokio::sync::mpsc::UnboundedSender<RunEvent>,
    session_id: String,
    message_id: String,
    created_at: i64,
    config: RunConfig,
    history: Vec<Value>,
    compaction: Option<Value>,
    ctx: ToolCtx,
    cancel: watch::Receiver<bool>,
}

/// 一轮 run 的步骤累积态（内容 / 步骤行 / 补丁 rev）。
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

    fn commit_patch(&self, store: &SessionStore, session_id: &str, message_id: &str) {
        let _ = store.commit(CommitBatch {
            patches: vec![PatchEntry {
                session_id: session_id.to_string(),
                message_id: message_id.to_string(),
                rev: self.rev,
                content: Some(self.content.clone()),
                steps: Some(self.steps_value()),
            }],
            ..Default::default()
        });
    }
}

const ABORTED: &str = "__aborted__";

/// 主循环：请求 → 增量 → 工具轮 → 再请求，直至不再调工具 / 中断 / 失败。
async fn run_loop(p: RunParams) {
    let RunParams {
        store,
        events,
        session_id,
        message_id,
        created_at,
        config,
        history,
        compaction,
        ctx,
        mut cancel,
    } = p;
    let tool_defs: Vec<ToolDecl> = builtin_tools()
        .into_iter()
        .filter(|t| config.tool_ids.iter().any(|id| id == t.name))
        .map(|t| ToolDecl {
            name: t.name.to_string(),
            description: t.description.to_string(),
            parameters: t.parameters,
        })
        .collect();
    let mut llm_history = {
        // 先折叠压缩再读回附件：被折叠的消息不再产生读盘 / 网络读取
        let mut folded = apply_compaction(&history, compaction.as_ref());
        resolve_image_attachments(&mut folded, &ctx.repo).await;
        history_to_llm_messages(&folded, None)
    };
    let mut state = RunState {
        steps: Vec::new(),
        content: String::new(),
        rev: 1,
    };
    let client = match http_client(&config.base_url) {
        Ok(client) => client,
        Err(e) => {
            // 出网守卫客户端构建失败：按 run 失败收场（不回落无守卫客户端）
            finalize_interrupted(&store, &session_id, &message_id, created_at, &mut state, &e);
            let _ = events.send(RunEvent::Failed {
                session_id,
                message_id,
                error: e,
            });
            return;
        }
    };
    let outcome: Result<(), String> = loop {
        if *cancel.borrow() {
            break Err(ABORTED.to_string());
        }
        let steps_base = state.steps.len(); // 本轮工具行的起点（回填线上历史用）
        let mut last_commit = std::time::Instant::now() - std::time::Duration::from_millis(50);
        state.rev += 1;
        state.commit_patch(&store, &session_id, &message_id);
        // 本轮流式累积：正文增量进 narrations（工具轮叙述），完整调用收集在 calls
        let round_text = Arc::new(Mutex::new(String::new()));
        let calls: Arc<Mutex<Vec<(usize, ToolCall)>>> = Arc::new(Mutex::new(Vec::new()));
        let req = StreamRequest {
            base_url: config.base_url.clone(),
            api_key: config.api_key.clone(),
            model: config.model.clone(),
            messages: system_then(&config.system_prompt, &llm_history),
            tools: tool_defs.clone(),
            reasoning_effort: config.reasoning_effort,
            max_tokens: None,
        };
        {
            let round_text = Arc::clone(&round_text);
            let calls = Arc::clone(&calls);
            let mut on_event = |event: StreamEvent| match event {
                StreamEvent::TextDelta(t) => {
                    round_text.lock().unwrap_or_else(|e| e.into_inner()).push_str(&t);
                    if last_commit.elapsed() >= std::time::Duration::from_millis(50) {
                        last_commit = std::time::Instant::now();
                        state.content = round_text.lock().unwrap_or_else(|e| e.into_inner()).clone();
                        state.commit_patch(&store, &session_id, &message_id);
                    }
                    let _ = events.send(RunEvent::TextDelta {
                        session_id: session_id.clone(),
                        message_id: message_id.clone(),
                        text: t,
                    });
                }
                StreamEvent::ReasoningDelta(t) => {
                    state.push_reasoning(&t);
                    if last_commit.elapsed() >= std::time::Duration::from_millis(50) {
                        last_commit = std::time::Instant::now();
                        state.commit_patch(&store, &session_id, &message_id);
                    }
                    let _ = events.send(RunEvent::ReasoningDelta {
                        session_id: session_id.clone(),
                        message_id: message_id.clone(),
                        text: t,
                    });
                }
                StreamEvent::ToolCallDelta { index, name, args_delta, .. } => {
                    ensure_pending_row(&mut state, index, name, &args_delta);
                    if last_commit.elapsed() >= std::time::Duration::from_millis(50) {
                        last_commit = std::time::Instant::now();
                        state.commit_patch(&store, &session_id, &message_id);
                    }
                }
                StreamEvent::ToolCall { index, call } => {
                    calls.lock().unwrap_or_else(|e| e.into_inner()).push((index, call));
                }
                StreamEvent::Finish { .. } => {}
            };
            let stream_fut = crate::ai::stream::stream(&client, &req, &mut on_event);
            let cancel_fut = cancel.wait_for(|v| *v);
            let result = tokio::select! {
                r = stream_fut => r,
                _ = cancel_fut => Err(crate::ai::stream::StreamError::Aborted),
            };
            match result {
                Ok(_) => {}
                Err(crate::ai::stream::StreamError::Transport(_e)) => {
                    // 请求未建立 / 未收到任何事件：传输级失败重试一次（流开始后不重试）
                    if *cancel.borrow() {
                        break Err(ABORTED.to_string());
                    }
                    let retry = tokio::select! {
                        r = crate::ai::stream::stream(&client, &req, &mut on_event) => r,
                        _ = cancel.wait_for(|v| *v) => Err(crate::ai::stream::StreamError::Aborted),
                    };
                    if let Err(e) = retry {
                        break Err(stream_error_text(e));
                    }
                }
                Err(crate::ai::stream::StreamError::Aborted) => break Err(ABORTED.to_string()),
                Err(e) => break Err(stream_error_text(e)),
            }
        }
        let done_calls: Vec<(usize, ToolCall)> =
            std::mem::take(&mut *calls.lock().unwrap_or_else(|e| e.into_inner()));
        if done_calls.is_empty() {
            // 最终轮：正文 = 本轮叙述增量
            state.content = std::mem::take(&mut *round_text.lock().unwrap_or_else(|e| e.into_inner()));
            state.rev += 1;
            state.commit_patch(&store, &session_id, &message_id);
            break Ok(());
        }
        // 叙述增量固化为叙述步：插到本轮首个工具行之前（保持发生顺序）
        let narration = std::mem::take(&mut *round_text.lock().unwrap_or_else(|e| e.into_inner()));
        // 同一段文本已进步骤：清掉流式期间写进 content 的那份，否则面板与续聊历史各显示一次
        state.content.clear();
        if !narration.is_empty() {
            let insert_at = state.steps[steps_base..]
                .iter()
                .position(|s| matches!(s, Step::Tool(_)))
                .map(|i| steps_base + i)
                .unwrap_or(state.steps.len());
            state.steps.insert(insert_at, Step::Narration(narration.clone()));
        }
        // 占位行固化：pending 行（按流内槽位 index）替换为真实 id 行
        fix_pending_rows(&mut state, &done_calls);
        // 逐个执行：parallel_safe 段并发（上限 MAX_PARALLEL_TOOLS），读写段串行屏障
        execute_calls(&ctx, &done_calls, &mut state, &events, &session_id, &message_id).await;
        state.rev += 1;
        state.commit_patch(&store, &session_id, &message_id);
        // 工具结果回填线上历史（下一轮请求可见；只取本轮起点之后的工具行）
        llm_history.push(LlmMessage::Assistant {
            content: if narration.is_empty() { None } else { Some(narration) },
            tool_calls: done_calls.iter().map(|(_, c)| ToolCall {
                id: c.id.clone(),
                name: c.name.clone(),
                arguments: c.arguments.clone(),
            }).collect(),
        });
        for step in state.steps.iter().skip(steps_base) {
            if let Step::Tool(r) = step {
                llm_history.push(LlmMessage::Tool {
                    tool_call_id: r.run_id.clone(),
                    content: if r.result.is_empty() {
                        r.result_summary.clone()
                    } else {
                        r.result.clone()
                    },
                });
            }
        }
    };
    match outcome {
        Ok(()) => {
            let _ = events.send(RunEvent::Finished {
                session_id,
                message_id,
                reason: "stop".to_string(),
            });
        }
        Err(e) => {
            finalize_interrupted(&store, &session_id, &message_id, created_at, &mut state, &e);
            if e == ABORTED {
                let _ = events.send(RunEvent::Finished {
                    session_id,
                    message_id,
                    reason: "interrupted".to_string(),
                });
            } else {
                let _ = events.send(RunEvent::Failed {
                    session_id,
                    message_id,
                    error: e,
                });
            }
        }
    }
}

/// 供应商流式客户端：入口先过地址策略校验，客户端构建失败即返回错误——
/// 不得回落默认客户端（那会摘掉重定向逐跳复检与 DNS 逐 IP 过滤，出网守卫失效）。
fn http_client(base_url: &str) -> Result<reqwest::Client, String> {
    let policy = crate::net_guard::policy_for_configured_url(base_url);
    crate::net_guard::ensure_http_url(base_url, policy)?;
    crate::net_guard::client_for(
        policy,
        crate::net_guard::ClientTimeout::IdleRead(60),
    )
}

fn system_then(system: &Option<String>, history: &[LlmMessage]) -> Vec<LlmMessage> {
    let mut out = Vec::with_capacity(history.len() + 1);
    if let Some(s) = system {
        if !s.trim().is_empty() {
            out.push(LlmMessage::System { content: s.clone() });
        }
    }
    out.extend(history.iter().cloned());
    out
}

fn ensure_pending_row(state: &mut RunState, index: usize, name: Option<String>, args_delta: &str) {
    let run_id = format!("{PENDING_RUN_ID_PREFIX}{index}");
    if let Some(Step::Tool(row)) = state
        .steps
        .iter_mut()
        .rev()
        .find(|s| matches!(s, Step::Tool(t) if t.run_id == run_id))
    {
        row.args.push_str(args_delta);
        if let Some(n) = name {
            row.name = n;
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

/// 流末完整调用到达：按流内槽位 index 对位替换占位行（真实 id / 完整参数）。
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
        if let Some(pos) = state
            .steps
            .iter()
            .position(|s| matches!(s, Step::Tool(t) if t.run_id == pending_id))
        {
            state.steps[pos] = Step::Tool(row);
        } else {
            state.steps.push(Step::Tool(row));
        }
    }
}

/// 按调用序执行：连续 parallel_safe 成段并发（有界），读写/未知工具单独串行屏障；
/// 结果按调用序回填对应行。
async fn execute_calls(
    ctx: &ToolCtx,
    calls: &[(usize, ToolCall)],
    state: &mut RunState,
    events: &tokio::sync::mpsc::UnboundedSender<RunEvent>,
    session_id: &str,
    message_id: &str,
) {
    let defs = builtin_tools();
    let is_parallel_safe = |name: &str| {
        defs.iter()
            .find(|d| d.name == name)
            .map(|d| d.parallel_safe)
            .unwrap_or(false)
    };
    let mut i = 0;
    while i < calls.len() {
        let (_, call) = &calls[i];
        if !is_parallel_safe(&call.name) {
            emit_tool_start(events, session_id, message_id, call);
            let args = parse_args(&call.arguments);
            let result = crate::ai::tools::execute(ctx, &call.name, args).await;
            settle_tool_row(state, events, session_id, message_id, call, &result);
            i += 1;
            continue;
        }
        // 并行段：连续 parallel_safe 调用分块并发（结果按调用序回填）
        let mut j = i;
        while j < calls.len() && is_parallel_safe(&calls[j].1.name) {
            j += 1;
        }
        for chunk in calls[i..j].chunks(MAX_PARALLEL_TOOLS) {
            let mut futs = Vec::with_capacity(chunk.len());
            for (_, call) in chunk {
                emit_tool_start(events, session_id, message_id, call);
                let ctx2 = ctx.clone();
                let call2 = call.clone();
                futs.push(tokio::spawn(async move {
                    let args = parse_args(&call2.arguments);
                    let result = crate::ai::tools::execute(&ctx2, &call2.name, args).await;
                    (call2.id, result)
                }));
            }
            let mut outcomes: Vec<(String, crate::ai::tools::ToolResult)> = Vec::new();
            for fut in futs {
                if let Ok(pair) = fut.await {
                    outcomes.push(pair);
                }
            }
            for (_, call) in chunk {
                let result = outcomes
                    .iter()
                    .find(|(id, _)| *id == call.id)
                    .map(|(_, r)| r.clone())
                    .unwrap_or(crate::ai::tools::ToolResult {
                        text: "（结果未回填）".to_string(),
                        is_error: true,
                    });
                settle_tool_row(state, events, session_id, message_id, call, &result);
            }
        }
        i = j;
    }
}

fn parse_args(raw: &str) -> Value {
    serde_json::from_str::<Value>(raw).unwrap_or_else(|_| {
        json!({ "_raw": raw })
    })
}

fn emit_tool_start(
    events: &tokio::sync::mpsc::UnboundedSender<RunEvent>,
    session_id: &str,
    message_id: &str,
    call: &ToolCall,
) {
    let _ = events.send(RunEvent::ToolStart {
        session_id: session_id.to_string(),
        message_id: message_id.to_string(),
        run_id: call.id.clone(),
        name: call.name.clone(),
        args_summary: args_summary(&parse_args(&call.arguments)),
    });
}

fn settle_tool_row(
    state: &mut RunState,
    events: &tokio::sync::mpsc::UnboundedSender<RunEvent>,
    session_id: &str,
    message_id: &str,
    call: &ToolCall,
    result: &crate::ai::tools::ToolResult,
) {
    let summary: String = result.text.chars().take(120).collect();
    let Some(Step::Tool(row)) = state
        .steps
        .iter_mut()
        .rev()
        .find(|s| matches!(s, Step::Tool(t) if t.run_id == call.id))
    else {
        return;
    };
    row.status = if result.is_error { "error" } else { "done" };
    row.is_error = result.is_error;
    row.result = result.text.clone();
    row.result_summary = summary.clone();
    let _ = events.send(RunEvent::ToolEnd {
        session_id: session_id.to_string(),
        message_id: message_id.to_string(),
        run_id: call.id.clone(),
        is_error: result.is_error,
        result_summary: summary,
    });
}

/// 中断 / 失败收敛：running 行归一为已中断；空占位移除；失败以 `[错误]` 落正文。
fn finalize_interrupted(
    store: &SessionStore,
    session_id: &str,
    message_id: &str,
    _created_at: i64,
    state: &mut RunState,
    error: &str,
) {
    let aborted = error == ABORTED;
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
        // 空占位移除（决定清理：不可见内容不留垃圾行）
        let _ = store.commit(CommitBatch {
            message_removals: vec![crate::ai::session::MessageRemoval {
                session_id: session_id.to_string(),
                message_id: message_id.to_string(),
            }],
            ..Default::default()
        });
        return;
    }
    if !aborted && state.content.is_empty() {
        state.content = format!("[错误]{error}");
    }
    state.rev += 1;
    state.commit_patch(store, session_id, message_id);
}

fn stream_error_text(e: crate::ai::stream::StreamError) -> String {
    match e {
        crate::ai::stream::StreamError::Http(code, body) => format!(
            "请求失败：HTTP {code}（{}）",
            body.chars().take(300).collect::<String>()
        ),
        crate::ai::stream::StreamError::Transport(m) => format!("请求失败：{m}"),
        crate::ai::stream::StreamError::MidStream(m) => format!("流式响应中断：{m}"),
        crate::ai::stream::StreamError::Aborted => ABORTED.to_string(),
    }
}

// ===== 辅助调用（命名 / 压缩）：不算 run，不追加 assistant 消息 =====

/// 辅助调用客户端（压缩 / 命名共用；策略按地址形态选择）。
/// 不可用时返回 None 并留日志：调用方以「辅助调用失败」收尾（可见失败 + 可定位原因）。
pub fn aid_client(base_url: &str) -> Option<reqwest::Client> {
    match http_client(base_url) {
        Ok(client) => Some(client),
        Err(e) => {
            eprintln!("辅助调用客户端不可用：{e}");
            None
        }
    }
}

/// 上下文压缩摘要：结构化检查点指令（非流式；失败 None，不写半成品）。
pub async fn compact_summary(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    model: &str,
    transcript: &str,
) -> Option<String> {
    let req = StreamRequest {
        base_url: base_url.to_string(),
        api_key: api_key.to_string(),
        model: model.to_string(),
        messages: vec![
            LlmMessage::System {
                content: "你负责压缩对话历史。输出结构化检查点：会话主题 / 已完成 / 关键决策 / 产出文件 / 待办 / 当前状态。只输出检查点本身。".to_string(),
            },
            LlmMessage::User {
                parts: vec![format!("压缩以下对话历史：\n\n{transcript}")],
                images: Vec::new(),
            },
        ],
        tools: Vec::new(),
        reasoning_effort: None,
        max_tokens: Some(2048),
    };
    let mut text = String::new();
    crate::ai::stream::stream(client, &req, |e| {
        if let StreamEvent::TextDelta(t) = e {
            text.push_str(&t);
        }
    })
    .await
    .ok()
    .map(|_| text.trim().to_string())
    .filter(|t| !t.is_empty())
}

/// 话题自动命名：非流式单次调用，返回清洗后的标题（失败 None，不阻塞对话）。
pub async fn auto_title(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    model: &str,
    excerpt: &str,
) -> Option<String> {
    let req = StreamRequest {
        base_url: base_url.to_string(),
        api_key: api_key.to_string(),
        model: model.to_string(),
        messages: vec![
            LlmMessage::System {
                content: "你负责为对话生成标题。只输出标题本身（不超过 16 字），不要引号、解释、标点或编号。".to_string(),
            },
            LlmMessage::User {
                parts: vec![format!("为下面的对话起一个简洁的标题：\n\n{excerpt}")],
                images: Vec::new(),
            },
        ],
        tools: Vec::new(),
        reasoning_effort: None,
        max_tokens: Some(512),
    };
    let mut text = String::new();
    crate::ai::stream::stream(client, &req, |e| {
        if let StreamEvent::TextDelta(t) = e {
            text.push_str(&t);
        }
    })
    .await
    .ok()
    .map(|_| clean_title(&text))
    .filter(|s| !s.is_empty())
}

/// 标题清洗：去引号 / 「标题：」前缀 / 编号 / 取首行 / 截 16 字。
pub fn clean_title(raw: &str) -> String {
    let first_line = raw.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let mut t = first_line.trim();
    for prefix in ["标题：", "标题:"] {
        if let Some(rest) = t.strip_prefix(prefix) {
            t = rest.trim();
        }
    }
    let t = t.trim_start_matches(|c: char| c.is_ascii_digit() || c == '.' || c == '、' || c == ' ');
    let t = t.trim_matches(|c: char| {
        matches!(c, '"' | '“' | '”' | '「' | '」' | '『' | '』' | '\'' | '‘' | '’' | '　')
    });
    t.chars().take(16).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::stream::request_body;

    #[test]
    fn same_round_tool_calls_share_one_assistant_message() {
        // 同一轮的两个工具调用（并发段）必须归进同一条 assistant(tool_calls)：
        // 拆成多条会让下一轮请求的历史形状与实时请求不一致
        let history = vec![
            json!({ "id": "u1", "role": "user", "content": "读两个文件", "createdAt": 1 }),
            json!({
                "id": "a1", "role": "assistant", "content": "最终回复", "createdAt": 2,
                "steps": [
                    { "kind": "text", "text": "并行读取" },
                    { "kind": "tool", "run": { "id": "c1", "name": "read_file", "status": "done",
                        "args": { "path": "a.md" }, "result": "A", "resultSummary": "a.md" } },
                    { "kind": "tool", "run": { "id": "c2", "name": "read_file", "status": "done",
                        "args": { "path": "b.md" }, "result": "B", "resultSummary": "b.md" } },
                    { "kind": "text", "text": "再查一次" },
                    { "kind": "tool", "run": { "id": "c3", "name": "grep", "status": "done",
                        "args": { "pattern": "x" }, "result": "命中", "resultSummary": "1 命中" } },
                ]
            }),
        ];
        let msgs = history_to_llm_messages(&history, None);
        // user + 第一轮 assistant(2 调用) + 2 条 tool + 第二轮 assistant(1 调用) + 1 条 tool + 最终正文
        assert_eq!(msgs.len(), 7);
        match &msgs[1] {
            LlmMessage::Assistant { content, tool_calls } => {
                assert_eq!(content.as_deref(), Some("并行读取"));
                assert_eq!(tool_calls.len(), 2);
                assert_eq!(tool_calls[0].id, "c1");
                assert_eq!(tool_calls[1].id, "c2");
            }
            other => panic!("{other:?}"),
        }
        match &msgs[4] {
            LlmMessage::Assistant { content, tool_calls } => {
                assert_eq!(content.as_deref(), Some("再查一次"));
                assert_eq!(tool_calls.len(), 1);
                assert_eq!(tool_calls[0].id, "c3");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn history_conversion_expands_tools_and_filters() {
        let history = vec![
            json!({ "id": "u1", "role": "user", "content": "帮我查", "createdAt": 1 }),
            json!({
                "id": "a1", "role": "assistant", "content": "最终回复", "createdAt": 2,
                "steps": [
                    { "kind": "reasoning", "text": "思考" },
                    { "kind": "text", "text": "先查文件" },
                    { "kind": "tool", "run": { "id": "c1", "name": "read_file", "status": "done",
                        "args": { "path": "a.md" }, "result": "内容", "resultSummary": "a.md 10 行" } },
                    { "kind": "tool", "run": { "id": "pending:1:0", "name": "web_search", "status": "error" } }
                ]
            }),
            json!({ "id": "u2", "role": "system", "content": "过滤掉", "createdAt": 3 }),
            json!({ "id": "a2", "role": "assistant", "content": "[错误]请求失败", "createdAt": 4 }),
        ];
        let msgs = history_to_llm_messages(&history, None);
        // user1 + assistant(tool_calls) + tool + assistant(最终回复)；[错误] 与 system 过滤
        assert_eq!(msgs.len(), 4);
        match &msgs[1] {
            LlmMessage::Assistant { content, tool_calls } => {
                assert_eq!(content.as_deref(), Some("先查文件"));
                assert_eq!(tool_calls.len(), 1);
                assert_eq!(tool_calls[0].id, "c1");
                assert_eq!(tool_calls[0].arguments, "{\"path\":\"a.md\"}");
            }
            other => panic!("{other:?}"),
        }
        match &msgs[2] {
            LlmMessage::Tool { tool_call_id, content } => {
                assert_eq!(tool_call_id, "c1");
                assert_eq!(content, "内容");
            }
            other => panic!("{other:?}"),
        }
        match &msgs[3] {
            LlmMessage::Assistant { content, .. } => {
                assert_eq!(content.as_deref(), Some("最终回复"))
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn history_conversion_collects_image_payloads_and_skips_failures() {
        let history = vec![json!({
            "id": "u1", "role": "user", "content": "看图", "createdAt": 1,
            "attachments": [
                { "kind": "image", "mime": "image/png", "file": "a.png",
                  "payload": "data:image/png;base64,QUJD" },
                { "kind": "image", "mime": "image/png", "file": "b.png" }
            ]
        })];
        let msgs = history_to_llm_messages(&history, None);
        match &msgs[0] {
            LlmMessage::User { parts, images } => {
                assert_eq!(parts, &["看图".to_string()]);
                // 读回失败的附件（无 payload）不进请求
                assert_eq!(images, &["data:image/png;base64,QUJD".to_string()]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn pure_narration_round_degrades_to_text() {
        let history = vec![json!({
            "id": "a", "role": "assistant", "content": "", "createdAt": 1,
            "steps": [
                { "kind": "reasoning", "text": "想想" },
                { "kind": "text", "text": "叙述行" }
            ]
        })];
        let msgs = history_to_llm_messages(&history, None);
        assert_eq!(msgs.len(), 1);
        match &msgs[0] {
            LlmMessage::Assistant { content, tool_calls } => {
                assert_eq!(content.as_deref(), Some("叙述行"));
                assert!(tool_calls.is_empty());
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn empty_assistant_content_becomes_null_in_wire() {
        let m = LlmMessage::Assistant {
            content: Some(String::new()),
            tool_calls: vec![ToolCall {
                id: "c".into(),
                name: "glob".into(),
                arguments: "{}".into(),
            }],
        };
        let body = request_body(&StreamRequest {
            base_url: "https://x/v1".into(),
            api_key: "k".into(),
            model: "m".into(),
            messages: vec![m],
            tools: Vec::new(),
            reasoning_effort: None,
            max_tokens: None,
        });
        assert_eq!(body["messages"][0]["content"], Value::Null);
    }

    #[test]
    fn compaction_folds_at_anchor_and_falls_back() {
        let history = vec![
            json!({ "id": "u1", "role": "user", "content": "一", "createdAt": 1 }),
            json!({ "id": "a1", "role": "assistant", "content": "二", "createdAt": 2 }),
            json!({ "id": "u2", "role": "user", "content": "三", "createdAt": 3 }),
        ];
        let compaction = json!({ "summary": "之前聊了二三事", "upToMessageId": "a1", "messageCount": 2 });
        let msgs = history_to_llm_messages(&history, Some(&compaction));
        assert_eq!(msgs.len(), 2);
        match &msgs[0] {
            LlmMessage::User { parts, .. } => assert!(parts[0].contains("压缩摘要")),
            other => panic!("{other:?}"),
        }
        let bad = json!({ "summary": "x", "upToMessageId": "不存在" });
        assert_eq!(history_to_llm_messages(&history, Some(&bad)).len(), 3);
    }

    #[test]
    fn clean_title_rules() {
        assert_eq!(clean_title("标题：「关于 Rust 的讨论」\n第二行"), "关于 Rust 的讨论");
        assert_eq!(clean_title("1. 部署方案"), "部署方案");
        assert_eq!(
            clean_title("这是一个非常非常非常非常非常长的标题超出十六字"),
            "这是一个非常非常非常非常非常长的"
        );
        assert_eq!(clean_title(""), "");
    }

    #[test]
    fn args_summary_shapes_objects() {
        assert_eq!(
            args_summary(&json!({ "path": "a.md", "limit": 10 })),
            "limit: 10，path: a.md"
        );
        let long = "很".repeat(61);
        let out = args_summary(&json!({ "query": long }));
        assert!(out.ends_with("…") && out.chars().count() < 70);
    }

    #[test]
    fn tool_row_shape_and_pending_prefix() {
        let row = ToolRow {
            run_id: "c1".into(),
            name: "read_file".into(),
            status: "done",
            args: "{\"path\":\"a.md\"}".into(),
            args_summary: "path: a.md".into(),
            result_summary: "10 行".into(),
            result: "内容".into(),
            is_error: false,
        };
        let v = row.to_value();
        assert_eq!(v["kind"], json!("tool"));
        assert_eq!(v["run"]["status"], json!("done"));
        assert_eq!(v["run"]["args"]["path"], json!("a.md"));
        assert_eq!(v["run"]["result"], json!("内容"));
        assert!(PENDING_RUN_ID_PREFIX.starts_with("pending:"));
    }
}
