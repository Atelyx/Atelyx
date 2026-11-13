//! 表格 AI 字段的单次生成：不带工具、不带会话上下文、无状态，一次调用产出一个单元格值。

use crate::ai::stream::{stream, LlmMessage, StreamError, StreamEvent, StreamRequest};

/// 单次生成的输出上限（token）。
pub const MAX_TOKENS: u32 = 1800;

/// 生成一个单元格内容：`system` 为空 = 不下发 system；失败返回可直接展示的中文原因。
pub async fn generate(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    model: &str,
    system: Option<String>,
    user: String,
    max_tokens: u32,
) -> Result<String, String> {
    let mut messages = Vec::new();
    if let Some(system) = system.filter(|text| !text.trim().is_empty()) {
        messages.push(LlmMessage::System { content: system });
    }
    messages.push(LlmMessage::User { parts: vec![user], images: Vec::new() });
    let request = StreamRequest {
        base_url: base_url.to_string(),
        api_key: api_key.to_string(),
        model: model.to_string(),
        messages,
        tools: Vec::new(),
        reasoning_effort: None,
        max_tokens: Some(max_tokens),
    };
    let mut text = String::new();
    let mut attempt = 0;
    let result = loop {
        text.clear();
        attempt += 1;
        let outcome = stream(client, &request, |event| {
            if let StreamEvent::TextDelta(delta) = event {
                text.push_str(&delta);
            }
        })
        .await;
        match outcome {
            Ok(_) => break Ok(()),
            Err(error) if attempt < 2 && retryable(&error) => {
                tokio::time::sleep(std::time::Duration::from_millis(800)).await;
            }
            Err(error) => break Err(describe(error)),
        }
    };
    result?;
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err("模型返回空内容".to_string());
    }
    Ok(text)
}

/// 仅传输级失败与限流重试一次；HTTP 4xx / 流开始后的失败不重试。
fn retryable(error: &StreamError) -> bool {
    match error {
        StreamError::Transport(_) => true,
        StreamError::Http(code, _) => *code == 429,
        StreamError::MidStream(_) | StreamError::Aborted => false,
    }
}

fn describe(error: StreamError) -> String {
    match error {
        StreamError::Transport(message) => format!("网络连接失败：{message}"),
        StreamError::MidStream(message) => format!("生成中断：{message}"),
        StreamError::Http(401 | 403, _) => "API 密钥无效或无权限，请到设置里检查".to_string(),
        StreamError::Http(429, _) => "模型请求过于频繁（429），稍后重试".to_string(),
        StreamError::Http(code, body) => {
            let body = body.trim();
            let detail: String = body.chars().take(200).collect();
            if detail.is_empty() {
                format!("模型请求失败（HTTP {code}）")
            } else {
                format!("模型请求失败（HTTP {code}）：{detail}")
            }
        }
        StreamError::Aborted => "生成已取消".to_string(),
    }
}
