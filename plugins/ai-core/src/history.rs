//! 会话消息（磁盘形状）→ 线上消息序列：压缩折叠、工具轮展开与附件位。
//! 只读 JSON，不触碰存储与网络；同一份历史在面板与插件侧得到同一请求形状。

use serde_json::{json, Value};

use crate::stream::{LlmMessage, ToolCall};

/// 参数生成中的合成工具步 id 前缀（流式占位行；收敛时 running 态归一为已中断）。
pub const PENDING_RUN_ID_PREFIX: &str = "pending:";

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

/// 压缩折叠的起点：锚点（含）之前不进上下文；0 = 未压缩或锚点不可用。
pub fn context_from(messages: &[Value], compaction: Option<&Value>) -> usize {
    let Some(c) = compaction else {
        return 0;
    };
    let anchor = c.get("upToMessageId").and_then(Value::as_str);
    let summary = c.get("summary").and_then(Value::as_str).unwrap_or("");
    let (Some(anchor), false) = (anchor, summary.is_empty()) else {
        return 0;
    };
    messages
        .iter()
        .position(|m| m.get("id").and_then(Value::as_str) == Some(anchor))
        .map(|idx| idx + 1)
        .unwrap_or(0)
}

/// 压缩折叠：锚点消息及其之前的内容替换为一条摘要 user 块。
pub fn apply_compaction(messages: &[Value], compaction: Option<&Value>) -> Vec<Value> {
    let Some(c) = compaction else {
        return messages.to_vec();
    };
    let summary = c.get("summary").and_then(Value::as_str).unwrap_or("");
    let start = context_from(messages, compaction);
    if start == 0 {
        return messages.to_vec();
    }
    let mut out = Vec::with_capacity(messages.len() - start + 1);
    out.push(json!({
        "id": "__compaction__",
        "role": "user",
        "content": format!("[以下是本会话早前内容的压缩摘要]\n\n{summary}"),
        "createdAt": 0,
    }));
    out.extend(messages[start..].iter().cloned());
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
