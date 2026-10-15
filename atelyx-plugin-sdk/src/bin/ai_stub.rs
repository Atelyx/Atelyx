//! 端到端测试夹具：握手后发起一轮 AI 对话（`ai` 权限），把收到的最新快照与终态
//! 写进自持数据——仅供 `ai_chat_e2e` 拉起真实进程验证对话契约链路，非示例插件。

use atelyx_plugin_sdk::protocol::method::ChatEventParams;
use atelyx_plugin_sdk::{run_stdio, Context, PluginHandler, PluginInfo};
use serde_json::{json, Map, Value};
use tokio::sync::Mutex as AsyncMutex;

struct AiStub {
    /// 自持数据草稿（整写自持文件；事件乱序到达也不丢字段）。
    model: AsyncMutex<Map<String, Value>>,
}

impl AiStub {
    /// 全程持锁写自持数据：快照与落盘之间不放锁，避免并发事件回调用旧快照整写覆盖新字段。
    async fn flush_locked(&self, ctx: &Context, model: &mut Map<String, Value>) {
        let _ = ctx.state_write(Value::Object(model.clone())).await;
    }
}

#[async_trait::async_trait]
impl PluginHandler for AiStub {
    async fn on_ready(&self, ctx: &Context) {
        let result = ctx.chat_send("你好", None).await;
        let mut model = self.model.lock().await;
        match result {
            Ok(r) => {
                model.insert("sessionId".into(), json!(r.session_id));
                model.insert("messageId".into(), json!(r.message_id));
            }
            Err(e) => {
                model.insert("error".into(), json!(e.message));
            }
        }
        self.flush_locked(ctx, &mut model).await;
    }

    async fn on_chat_event(&self, ctx: &Context, params: ChatEventParams) {
        {
            let mut model = self.model.lock().await;
            model
                .entry("sessionId".to_string())
                .or_insert_with(|| json!(params.session_id));
            match params.event {
                atelyx_plugin_sdk::protocol::method::ChatEvent::Message { message, .. } => {
                    let content = message
                        .get("content")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    model.insert("content".into(), json!(content));
                }
                atelyx_plugin_sdk::protocol::method::ChatEvent::Finished { reason } => {
                    model.insert("finished".into(), json!(reason));
                }
                atelyx_plugin_sdk::protocol::method::ChatEvent::Failed { error } => {
                    model.insert("failed".into(), json!(error));
                }
                atelyx_plugin_sdk::protocol::method::ChatEvent::Titled { title } => {
                    model.insert("titled".into(), json!(title));
                }
            }
            self.flush_locked(ctx, &mut model).await;
        }
    }
}

#[tokio::main]
async fn main() {
    let fixture = PluginInfo {
        id: "com.atelyx.ai-stub".into(),
        name: "AiStub".into(),
        version: "0.1.0".into(),
    };
    if let Err(e) = run_stdio(fixture, AiStub { model: AsyncMutex::default() }).await {
        eprintln!("夹具启动失败：{e}");
        std::process::exit(1);
    }
}
