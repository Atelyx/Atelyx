//! OpenAI 兼容流式客户端：SSE 请求、帧解析与非流式兜底。
//! 解析器为纯函数（喂帧 → 事件序列）；工具调用分片按 `index` 槽位拼装。

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{json, Value};

/// 单帧读取空闲超时（覆盖首 token 前的等待；由调用方在每个读块上实施）。
pub const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// 线上消息（与 OpenAI chat.completions 消息同构；用户内容为文本段序列，
/// 引用文件内容以文本段内联）。
#[derive(Debug, Clone)]
pub enum LlmMessage {
    System { content: String },
    User { parts: Vec<String> },
    Assistant {
        content: Option<String>,
        tool_calls: Vec<ToolCall>,
    },
    Tool { tool_call_id: String, content: String },
}

/// 助手消息里的完整工具调用。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// 原始 JSON 参数文本（空 = "{}"）。
    pub arguments: String,
}

/// 工具声明（function calling 名册项）。
#[derive(Debug, Clone)]
pub struct ToolDecl {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

/// 推理努力等级（缺省 / off 不下发字段）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ReasoningEffort {
    Off,
    Low,
    Medium,
    High,
}

impl ReasoningEffort {
    fn wire(self) -> Option<&'static str> {
        match self {
            ReasoningEffort::Off => None,
            ReasoningEffort::Low => Some("low"),
            ReasoningEffort::Medium => Some("medium"),
            ReasoningEffort::High => Some("high"),
        }
    }

    /// 线格式（覆盖存储 / 请求字段取值）→ 枚举。
    pub fn parse_wire(s: &str) -> Option<Self> {
        match s {
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            _ => None,
        }
    }
}

/// 一次流式请求的全部输入。
#[derive(Debug, Clone)]
pub struct StreamRequest {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub messages: Vec<LlmMessage>,
    pub tools: Vec<ToolDecl>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub max_tokens: Option<u32>,
}

/// 停止原因（网关取值归一）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    Stop,
    ToolCalls,
    MaxTokens,
}

impl StopReason {
    fn parse(raw: &str) -> Self {
        match raw {
            "tool_calls" | "function_call" => StopReason::ToolCalls,
            "length" => StopReason::MaxTokens,
            _ => StopReason::Stop,
        }
    }
}

/// 解析器产出的增量事件（与网络解耦，供 UI 与循环消费）。
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    TextDelta(String),
    ReasoningDelta(String),
    /// 参数分片（含 id / name 首片）；参数累积由解析器持有，UI 可据此画「生成中」行。
    ToolCallDelta {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        args_delta: String,
    },
    /// 流末一次性发出的完整工具调用（携带流内槽位 index，按槽位序）。
    ToolCall { index: usize, call: ToolCall },
    Finish { reason: StopReason },
}

#[derive(Debug)]
pub enum StreamError {
    /// 请求未建立或未收到任何事件（传输级失败，可重试）。
    Transport(String),
    /// 流已开始后的失败（不可重试）。
    MidStream(String),
    /// HTTP 非 2xx（body 文本随行）。
    Http(u16, String),
    /// 调用方取消。
    Aborted,
}

// ===== 解析器 =====

#[derive(Default)]
struct ToolSlot {
    id: String,
    name: String,
    arguments: String,
}

/// 帧解析累积器：跨帧持有工具调用槽位；`saw_event` 供非流式兜底判定。
#[derive(Default)]
pub struct StreamAccum {
    text: String,
    reasoning: String,
    tool_slots: BTreeMap<usize, ToolSlot>,
    saw_event: bool,
    /// 终止事件只发一次：finish_reason 帧与 `[DONE]` 可能先后或同批到达。
    finish_emitted: bool,
}

impl StreamAccum {
    pub fn new() -> Self {
        Self::default()
    }

    /// 本轮已收到的正文增量（供「流开始后」判定与兜底重组）。
    pub fn has_seen_event(&self) -> bool {
        self.saw_event
    }

    /// 喂入一个 SSE 帧（单个 `data:` 行的内容，已去前缀）；返回产出事件。
    /// `frame` 为 `data: ` 之后的载荷。
    pub fn feed(&mut self, frame: &str) -> Vec<StreamEvent> {
        let frame = frame.trim();
        if frame == "[DONE]" {
            // 收束信号在部分网关替代 finish_reason：补齐终止事件，网络层据此立即返回，
            // 否则要等连接关闭（或读超时）才判定收束，被误报为「空闲超时」中断
            let mut events = self.finish();
            if !self.finish_emitted {
                self.finish_emitted = true;
                let reason = if events.is_empty() {
                    StopReason::Stop
                } else {
                    StopReason::ToolCalls
                };
                events.push(StreamEvent::Finish { reason });
            }
            return events;
        }
        let Ok(v) = serde_json::from_str::<Value>(frame) else {
            return Vec::new(); // 单帧解析失败跳过（网关噪声）
        };
        // HTTP 200 中的错误事件如实上抛（以 [错误] 文本进正文，供面板呈现）
        if let Some(err) = v.get("error") {
            let msg = err
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| err.to_string());
            return vec![
                StreamEvent::TextDelta(format!("[错误]{msg}")),
                StreamEvent::Finish {
                    reason: StopReason::Stop,
                },
            ];
        }
        let Some(choice) = v.get("choices").and_then(Value::as_array).and_then(|c| c.first())
        else {
            return Vec::new();
        };
        let delta = choice.get("delta");
        let mut events = Vec::new();
        if delta.is_none() {
            // 非流式形状（网关忽略 stream:true 返回整 JSON）：按完整消息一次性落地
            if let Some(message) = choice.get("message") {
                if let Some(t) = message.get("content").and_then(Value::as_str) {
                    if !t.is_empty() {
                        self.text.push_str(t);
                        events.push(StreamEvent::TextDelta(t.to_string()));
                    }
                }
                if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
                    for call in calls {
                        let index = call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                        let slot = self.tool_slots.entry(index).or_default();
                        slot.id = call
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        slot.name = call
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        slot.arguments = call
                            .get("function")
                            .and_then(|f| f.get("arguments"))
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                    }
                }
            }
        }
        if let Some(d) = delta {
            if let Some(t) = d.get("content").and_then(Value::as_str) {
                if !t.is_empty() {
                    self.text.push_str(t);
                    events.push(StreamEvent::TextDelta(t.to_string()));
                }
            }
            if let Some(r) = reasoning_of(d) {
                self.reasoning.push_str(&r);
                events.push(StreamEvent::ReasoningDelta(r));
            }
            if let Some(calls) = d.get("tool_calls").and_then(Value::as_array) {
                for call in calls {
                    let index = call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                    let slot = self.tool_slots.entry(index).or_default();
                    let mut id = None;
                    let mut name = None;
                    if let Some(i) = call.get("id").and_then(Value::as_str) {
                        if !i.is_empty() {
                            slot.id = i.to_string();
                            id = Some(i.to_string());
                        }
                    }
                    if let Some(n) = call
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(Value::as_str)
                    {
                        if !n.is_empty() {
                            slot.name = n.to_string();
                            name = Some(n.to_string());
                        }
                    }
                    let args_delta = call
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    if !args_delta.is_empty() {
                        slot.arguments.push_str(&args_delta);
                    }
                    if id.is_some() || name.is_some() || !args_delta.is_empty() {
                        events.push(StreamEvent::ToolCallDelta {
                            index,
                            id,
                            name,
                            args_delta,
                        });
                    }
                }
            }
        }
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            if !reason.is_empty() {
                let mut out = events;
                out.extend(self.finish());
                if !self.finish_emitted {
                    self.finish_emitted = true;
                    out.push(StreamEvent::Finish {
                        reason: StopReason::parse(reason),
                    });
                }
                return out;
            }
        }
        self.saw_event = !events.is_empty() || self.saw_event;
        events
    }

    /// 流末收敛：发出完整工具调用（携带槽位 index，按槽位序）。
    fn finish(&mut self) -> Vec<StreamEvent> {
        self.saw_event = true;
        let slots = std::mem::take(&mut self.tool_slots);
        slots
            .into_iter()
            .map(|(index, s)| StreamEvent::ToolCall {
                index,
                call: ToolCall {
                    id: s.id,
                    name: s.name,
                    arguments: s.arguments,
                },
            })
            .collect()
    }
}

fn reasoning_of(delta: &Value) -> Option<String> {
    for key in ["reasoning_content", "reasoning", "reasoning_text"] {
        if let Some(s) = delta.get(key).and_then(Value::as_str) {
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

/// 请求体组装（ messages / tools / reasoning_effort / max_tokens；流式恒开）。
pub fn request_body(req: &StreamRequest) -> Value {
    let mut body = json!({
        "model": req.model,
        "stream": true,
        "messages": req.messages.iter().map(message_to_wire).collect::<Vec<_>>(),
    });
    if !req.tools.is_empty() {
        body["tools"] = json!(req
            .tools
            .iter()
            .map(|t| json!({
                "type": "function",
                "function": { "name": t.name, "description": t.description, "parameters": t.parameters }
            }))
            .collect::<Vec<_>>());
    }
    if let Some(effort) = req.reasoning_effort.and_then(|e| e.wire()) {
        body["reasoning_effort"] = json!(effort);
    }
    if let Some(max) = req.max_tokens {
        body["max_tokens"] = json!(max);
    }
    body
}

fn message_to_wire(m: &LlmMessage) -> Value {
    match m {
        LlmMessage::System { content } => json!({ "role": "system", "content": content }),
        LlmMessage::User { parts } => {
            if parts.len() == 1 {
                json!({ "role": "user", "content": parts[0] })
            } else {
                json!({
                    "role": "user",
                    "content": parts
                        .iter()
                        .map(|p| json!({ "type": "text", "text": p }))
                        .collect::<Vec<_>>()
                })
            }
        }
        LlmMessage::Assistant { content, tool_calls } => {
            let mut obj = serde_json::Map::new();
            obj.insert("role".into(), json!("assistant"));
            // 空串按规范置 null（部分网关对空串 500）
            obj.insert(
                "content".into(),
                content.as_deref().filter(|c| !c.is_empty()).map(|c| json!(c)).unwrap_or(Value::Null),
            );
            if !tool_calls.is_empty() {
                obj.insert(
                    "tool_calls".into(),
                    json!(tool_calls
                        .iter()
                        .map(|c| json!({
                            "id": c.id,
                            "type": "function",
                            "function": { "name": c.name, "arguments": c.arguments }
                        }))
                        .collect::<Vec<_>>()),
                );
            }
            Value::Object(obj)
        }
        LlmMessage::Tool {
            tool_call_id,
            content,
        } => json!({ "role": "tool", "tool_call_id": tool_call_id, "content": content }),
    }
}

// ===== 网络层 =====

/// 执行流式请求：逐帧喂累积器、回调产出事件；返回停止原因。
/// `on_event` 在流期间逐事件回调（错误事件也会经回调送达，随后以 Finish 收敛）。
/// 空闲超时（每读块 60s）与取消（future 被 drop / select 竞争）由调用方控制。
pub async fn stream(
    client: &reqwest::Client,
    req: &StreamRequest,
    mut on_event: impl FnMut(StreamEvent),
) -> Result<StopReason, StreamError> {
    let url = format!("{}/chat/completions", req.base_url.trim_end_matches('/'));
    let resp = client
        .post(&url)
        .bearer_auth(&req.api_key)
        .json(&request_body(req))
        .send()
        .await
        .map_err(|e| StreamError::Transport(e.to_string()))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(StreamError::Http(status.as_u16(), body));
    }
    let mut accum = StreamAccum::new();
    let mut buffer = String::new();
    let mut bytes = resp.bytes_stream();
    let mut finish: Option<StopReason> = None;
    loop {
        let chunk = {
            // 逐块读取（空闲超时包裹；超时按「流是否已开始」定级 Transport/MidStream）
            let fut = futures_util::StreamExt::next(&mut bytes);
            match tokio::time::timeout(STREAM_IDLE_TIMEOUT, fut).await {
                Err(_) => {
                    if accum.has_seen_event() {
                        return Err(StreamError::MidStream("流式响应空闲超时".into()));
                    }
                    return Err(StreamError::Transport("流式响应空闲超时".into()));
                }
                Ok(None) => break,
                Ok(Some(Err(e))) => {
                    return if accum.has_seen_event() {
                        Err(StreamError::MidStream(e.to_string()))
                    } else {
                        Err(StreamError::Transport(e.to_string()))
                    }
                }
                Ok(Some(Ok(bytes))) => bytes,
            }
        };
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        // 按空行分帧（兼容 \r\n\r\n）
        while let Some(pos) = find_frame_end(&buffer) {
            let frame = buffer[..pos.0].to_string();
            buffer.drain(..pos.1);
            for event in feed_frame(&mut accum, &frame) {
                match event {
                    StreamEvent::Finish { reason } => finish = Some(reason),
                    other => on_event(other),
                }
            }
            if let Some(reason) = finish {
                return Ok(reason);
            }
        }
    }
    // 流结束但未收到 Finish：残留兜底（网关忽略 stream:true 返回整 JSON 等）
    if !buffer.trim().is_empty() {
        for event in feed_frame(&mut accum, &buffer) {
            match event {
                StreamEvent::Finish { reason } => finish = Some(reason),
                other => on_event(other),
            }
        }
    }
    if let Some(reason) = finish {
        return Ok(reason);
    }
    if !accum.has_seen_event() {
        return Err(StreamError::Transport("未收到任何流式事件".into()));
    }
    Ok(StopReason::Stop)
}

/// 喂入一个完整 SSE 帧（可能含多个 `data:` 行）。
fn feed_frame(accum: &mut StreamAccum, frame: &str) -> Vec<StreamEvent> {
    let mut events = Vec::new();
    for line in frame.lines() {
        let Some(data) = line.strip_prefix("data:") else {
            continue; // event:/id:/注释行忽略
        };
        let data = data.strip_prefix(' ').unwrap_or(data);
        if data.is_empty() {
            continue;
        }
        events.extend(accum.feed(data));
    }
    events
}

/// 找下一帧边界：返回（内容终点，消费长度），兼容 `\n\n` 与 `\r\n\r\n`。
fn find_frame_end(buffer: &str) -> Option<(usize, usize)> {
    let bytes = buffer.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'\n' {
            if bytes[i + 1] == b'\n' {
                return Some((i, i + 2));
            }
            if bytes[i + 1] == b'\r' && i + 2 < bytes.len() && bytes[i + 2] == b'\n' {
                return Some((i, i + 3));
            }
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(acc: &mut StreamAccum, frame: &str) -> Vec<StreamEvent> {
        feed_frame(acc, frame)
    }

    #[test]
    fn text_and_reasoning_deltas() {
        let mut acc = StreamAccum::new();
        let events = feed(
            &mut acc,
            "data: {\"choices\":[{\"delta\":{\"content\":\"你\",\"reasoning_content\":\"想\"}}]}\n\n",
        );
        assert_eq!(
            events,
            vec![
                StreamEvent::TextDelta("你".into()),
                StreamEvent::ReasoningDelta("想".into()),
            ]
        );
        // reasoning 多字段探测：reasoning / reasoning_text 同样接受；空串拒绝
        let events = feed(
            &mut acc,
            "data: {\"choices\":[{\"delta\":{\"reasoning\":\"继续\"}}]}\n\n",
        );
        assert_eq!(events, vec![StreamEvent::ReasoningDelta("继续".into())]);
        let events = feed(
            &mut acc,
            "data: {\"choices\":[{\"delta\":{\"reasoning\":\"\"}}]}\n\n",
        );
        assert!(events.is_empty());
    }

    #[test]
    fn tool_call_slots_assemble_and_complete_at_done() {
        let delta_frame = |calls: Value| {
            format!(
                "data: {}\n\n",
                json!({ "choices": [{ "delta": { "tool_calls": [calls] } }] })
            )
        };
        let mut acc = StreamAccum::new();
        let _ = feed(
            &mut acc,
            &delta_frame(json!({
                "index": 0, "id": "c1",
                "function": { "name": "read_file", "arguments": "{\"path\"" }
            })),
        );
        let _ = feed(
            &mut acc,
            &delta_frame(json!({
                "index": 0,
                "function": { "arguments": ":\"a.md\"}" }
            })),
        );
        let _ = feed(
            &mut acc,
            &delta_frame(json!({
                "index": 1, "id": "c2",
                "function": { "name": "glob", "arguments": "{}" }
            })),
        );
        let events = feed(&mut acc, "data: [DONE]\n\n");
        // [DONE] 除落完整工具调用外还要补终止事件：只发 [DONE] 的网关据此立即收束
        assert_eq!(
            events,
            vec![
                StreamEvent::ToolCall { index: 0, call: ToolCall {
                    id: "c1".into(),
                    name: "read_file".into(),
                    arguments: "{\"path\":\"a.md\"}".into(),
                } },
                StreamEvent::ToolCall { index: 1, call: ToolCall {
                    id: "c2".into(),
                    name: "glob".into(),
                    arguments: "{}".into(),
                } },
                StreamEvent::Finish { reason: StopReason::ToolCalls },
            ]
        );
    }

    #[test]
    fn finish_reason_normalizes_and_flushes_calls() {
        let mut acc = StreamAccum::new();
        let _ = feed(
            &mut acc,
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"glob\",\"arguments\":\"{}\"}}]}}]}\n\n",
        );
        let events = feed(
            &mut acc,
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        );
        assert_eq!(
            events,
            vec![
                StreamEvent::ToolCall { index: 0, call: ToolCall {
                    id: "c1".into(),
                    name: "glob".into(),
                    arguments: "{}".into(),
                } },
                StreamEvent::Finish {
                    reason: StopReason::ToolCalls
                },
            ]
        );
        let mut acc = StreamAccum::new();
        let events = feed(
            &mut acc,
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}]}\n\n",
        );
        assert_eq!(
            events.last(),
            Some(&StreamEvent::Finish {
                reason: StopReason::MaxTokens
            })
        );
    }

    #[test]
    fn crlf_frames_and_noise_lines_skipped() {
        let mut acc = StreamAccum::new();
        let events = feed(
            &mut acc,
            "data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\r\n\r\n:keepalive\n\n",
        );
        assert_eq!(events, vec![StreamEvent::TextDelta("a".into())]);
    }

    #[test]
    fn error_event_surfaces_as_text() {
        let mut acc = StreamAccum::new();
        let events = feed(
            &mut acc,
            "data: {\"error\":{\"message\":\"额度不足\"}}\n\n",
        );
        assert_eq!(
            events,
            vec![
                StreamEvent::TextDelta("[错误]额度不足".into()),
                StreamEvent::Finish {
                    reason: StopReason::Stop
                },
            ]
        );
    }

    #[test]
    fn non_stream_body_fallback_reassembles() {
        // 网关忽略 stream:true 返回整 JSON：message 形状按完整消息一次性落地
        let body = "{\"choices\":[{\"message\":{\"role\":\"assistant\",\"content\":\"完整回复\"},\"finish_reason\":\"stop\"}]}";
        let mut acc = StreamAccum::new();
        let events = feed(&mut acc, &format!("data: {body}\n\n"));
        assert_eq!(
            events,
            vec![
                StreamEvent::TextDelta("完整回复".into()),
                StreamEvent::Finish {
                    reason: StopReason::Stop
                },
            ]
        );
    }

    #[test]
    fn request_body_shape() {
        let req = StreamRequest {
            base_url: "https://api.test/v1".into(),
            api_key: "k".into(),
            model: "m".into(),
            messages: vec![
                LlmMessage::System {
                    content: "s".into(),
                },
                LlmMessage::User {
                    parts: vec!["u1".into(), "u2".into()],
                },
                LlmMessage::Assistant {
                    content: Some("".into()),
                    tool_calls: vec![ToolCall {
                        id: "c1".into(),
                        name: "glob".into(),
                        arguments: "{}".into(),
                    }],
                },
                LlmMessage::Tool {
                    tool_call_id: "c1".into(),
                    content: "结果".into(),
                },
            ],
            tools: vec![ToolDecl {
                name: "glob".into(),
                description: "d".into(),
                parameters: json!({"type": "object"}),
            }],
            reasoning_effort: Some(ReasoningEffort::Off),
            max_tokens: Some(512),
        };
        let body = request_body(&req);
        assert_eq!(body["stream"], json!(true));
        assert!(body.get("reasoning_effort").is_none(), "off 不下发字段");
        assert_eq!(body["max_tokens"], json!(512));
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], json!("system"));
        assert_eq!(
            msgs[1]["content"],
            json!([
                { "type": "text", "text": "u1" },
                { "type": "text", "text": "u2" }
            ])
        );
        assert_eq!(msgs[2]["content"], Value::Null, "空正文按规范置 null");
        assert_eq!(msgs[2]["tool_calls"][0]["id"], json!("c1"));
        assert_eq!(msgs[3]["role"], json!("tool"));
        assert_eq!(body["tools"][0]["function"]["name"], json!("glob"));
    }
}
