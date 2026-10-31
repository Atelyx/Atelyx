//! AI 核心插件：对话循环、流式解析、工具编排、压缩与命名。
//! 无视图，不持有会话真源；只经宿主能力面读写会话、执行工具与出网。

mod history;
mod run;
mod stream;
mod title;

use std::sync::Arc;

use atelyx_plugin_protocol::method::{
    self, ChatEvent, ChatEventParams, HttpStreamChunkParams, HttpStreamEndParams, RunCompactParams,
    RunDirectParams, RunStartParams, RunStopParams, RunTitleParams,
};
use atelyx_plugin_protocol::service::HostCallParams;
use atelyx_plugin_sdk::{Context, PluginHandler, PluginInfo};
use serde_json::Value;

use crate::run::RunHub;

/// 插件 id（与 `plugin.json` 一致）。
const PLUGIN_ID: &str = "atelyx.ai-core";
/// 退出时等待运行收尾的上限。
const SHUTDOWN_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

struct AiCorePlugin {
    hub: Arc<RunHub>,
}

impl AiCorePlugin {
    fn new() -> Self {
        Self { hub: Arc::new(RunHub::new()) }
    }

    async fn call(&self, ctx: &Context, params: &HostCallParams) -> Result<Value, String> {
        if params.service != method::run::SERVICE {
            return Err(format!("未提供该服务：{}", params.service));
        }
        match params.method.as_str() {
            method::run::START => {
                let params: RunStartParams = serde_json::from_value(params.params.clone())
                    .map_err(|error| format!("run.start 参数不可解析：{error}"))?;
                let result = run::start(ctx.clone(), Arc::clone(&self.hub), params).await?;
                serde_json::to_value(result).map_err(|error| error.to_string())
            }
            method::run::START_DIRECT => {
                let params: RunDirectParams = serde_json::from_value(params.params.clone())
                    .map_err(|error| format!("run.startDirect 参数不可解析：{error}"))?;
                let result = run::start_direct(ctx.clone(), Arc::clone(&self.hub), params).await?;
                serde_json::to_value(result).map_err(|error| error.to_string())
            }
            method::run::STOP => {
                let params: RunStopParams = serde_json::from_value(params.params.clone())
                    .map_err(|error| format!("run.stop 参数不可解析：{error}"))?;
                self.hub.stop(&params.session_id);
                Ok(serde_json::json!({}))
            }
            method::run::STOP_ALL => {
                self.hub.stop_all();
                Ok(serde_json::json!({}))
            }
            other => Err(format!("未知运行方法：{other}")),
        }
    }

    /// 压缩 / 命名是长调用：放到独立线程回执，插件工作线程继续处理流式分片与输入。
    async fn spawn_aux(&self, ctx: Context, params: HostCallParams) {
        let method_name = params.method.clone();
        let call_id = params.call_id.clone();
        let raw = params.params.clone();
        let hub = Arc::clone(&self.hub);
        hub.begin_aux();
        let fallback = ctx.clone();
        let thread_hub = Arc::clone(&hub);
        let spawn = std::thread::Builder::new()
            .name("atelyx-ai-aux".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = ctx.service_error(&call_id, &format!("辅助任务运行时初始化失败：{error}"));
                        thread_hub.end_aux();
                        return;
                    }
                };
                let _ = runtime.block_on(async {
                    let outcome = match method_name.as_str() {
                        method::run::COMPACT => {
                            let params: RunCompactParams = serde_json::from_value(raw)
                                .map_err(|error| format!("run.compact 参数不可解析：{error}"))?;
                            run::compact(&ctx, &thread_hub, &params.session_id, &params.config).await
                        }
                        method::run::TITLE => {
                            let params: RunTitleParams = serde_json::from_value(raw)
                                .map_err(|error| format!("run.title 参数不可解析：{error}"))?;
                            let result = run::retitle(&ctx, &thread_hub, &params.session_id, &params.config).await?;
                            if let Some(title) = result.get("title").and_then(Value::as_str) {
                                emit_titled(&ctx, &params.session_id, title).await;
                            }
                            Ok(result)
                        }
                        _ => Err("未知辅助方法".to_string()),
                    };
                    let response = match outcome {
                        Ok(value) => ctx.service_result(&call_id, value).await,
                        Err(error) => ctx.service_error(&call_id, &error).await,
                    };
                    if let Err(error) = response {
                        ctx.log_error(&format!("回报辅助调用结果失败：{error}"));
                    }
                    Ok::<(), String>(())
                });
                thread_hub.end_aux();
            });
        if let Err(error) = spawn {
            hub.end_aux();
            let _ = fallback
                .service_error(&params.call_id, &format!("启动辅助任务失败：{error}"))
                .await;
        }
    }
}

async fn emit_titled(ctx: &Context, session_id: &str, title: &str) {
    let params = ChatEventParams {
        session_id: session_id.to_string(),
        message_id: String::new(),
        event: ChatEvent::Titled { title: Some(title.to_string()) },
    };
    if let Err(error) = ctx.chat_emit(params).await {
        ctx.log_error(&format!("标题事件发布失败：{error}"));
    }
}

#[async_trait::async_trait]
impl PluginHandler for AiCorePlugin {
    async fn on_ready(&self, _ctx: &Context) {}

    async fn on_shutdown(&self, _ctx: &Context) {
        run::shutdown(&self.hub, SHUTDOWN_WAIT).await;
    }

    async fn on_http_stream_chunk(&self, ctx: &Context, params: HttpStreamChunkParams) {
        use base64::Engine;
        match base64::engine::general_purpose::STANDARD.decode(params.base64) {
            Ok(bytes) => self.hub.push_chunk(&params.stream_id, bytes),
            Err(error) => {
                ctx.log_error(&format!("流式分片 base64 解码失败：{error}"));
            }
        }
    }

    async fn on_http_stream_end(&self, _ctx: &Context, params: HttpStreamEndParams) {
        self.hub.finish_stream(
            &params.stream_id,
            params.error,
            params.cancelled,
            params.status,
        );
    }

    async fn on_service_call(&self, ctx: &Context, params: HostCallParams) {
        if params.service == method::run::SERVICE
            && matches!(params.method.as_str(), method::run::COMPACT | method::run::TITLE)
        {
            self.spawn_aux(ctx.clone(), params).await;
            return;
        }
        let response = match self.call(ctx, &params).await {
            Ok(value) => ctx.service_result(&params.call_id, value).await,
            Err(error) => ctx.service_error(&params.call_id, &error).await,
        };
        if let Err(error) = response {
            ctx.log_error(&format!("回报运行调用结果失败：{error}"));
        }
    }
}

#[no_mangle]
pub extern "C" fn atelyx_plugin_api() -> *const atelyx_plugin_sdk::abi::PluginApi {
    atelyx_plugin_sdk::in_process::plugin_api(
        || PluginInfo {
            id: PLUGIN_ID.into(),
            name: "AI 核心".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
        || Box::new(AiCorePlugin::new()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_matches_the_crate_identity() {
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../plugin.json")).unwrap();
        assert_eq!(manifest["id"], PLUGIN_ID);
        assert_eq!(manifest["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(manifest["abiVersion"], atelyx_plugin_sdk::abi::ABI_VERSION);
    }
}
