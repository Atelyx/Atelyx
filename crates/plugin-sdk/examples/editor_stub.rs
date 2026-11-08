//! 端到端测试夹具：一个登记自有服务（`demoeditor`）的外部提供者。
//!
//! 登记后立即成为该服务的提供者，随后发起探针——探针同时走「直接调用 `demoeditor.selection`」
//! 与「`service.call` 信封」，把两侧结果写进自持数据，供测试断言两条路都落到本插件。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use atelyx_plugin_sdk::protocol::envelope::RpcError;
use atelyx_plugin_sdk::protocol::event::PublishParams;
use atelyx_plugin_sdk::protocol::service::HostCallParams;
use atelyx_plugin_sdk::{Context, PluginHandler};
use serde_json::{json, Value};

/// 夹具自有的服务名（不与内置服务重名：内置名归宿主，登记会被拒）。
const SERVICE: &str = "demoeditor";

struct Inner {
    /// 探针序号：每次落盘都带它，测试据此等新一次探针。
    probe: AtomicU64,
    /// 探针串行化：同一时刻只有一次探针在跑，落盘顺序与触发顺序一致。
    probe_lock: tokio::sync::Mutex<()>,
}

fn outcome<T: serde::Serialize>(result: Result<T, RpcError>) -> Value {
    match result {
        Ok(value) => json!({ "ok": value }),
        Err(error) => json!({ "error": error.message }),
    }
}

impl Inner {
    async fn probe(&self, ctx: &Context) {
        let _guard = self.probe_lock.lock().await;
        let seq = self.probe.fetch_add(1, Ordering::SeqCst) + 1;
        let selection = ctx.service_call(SERVICE, "selection", json!({})).await;
        let replace = ctx
            .service_call(SERVICE, "replaceSelection", json!({ "text": "*" }))
            .await;
        let direct = ctx.call_raw(&format!("{SERVICE}.selection"), json!({})).await;
        let state = json!({
            "probe": seq,
            "selection": outcome(selection),
            "replace": outcome(replace),
            "direct": outcome(direct),
        });
        if let Err(e) = ctx.state_write(state).await {
            eprintln!("探针落盘失败：{e}");
        }
    }
}

struct EditorStub {
    inner: Arc<Inner>,
}

#[async_trait::async_trait]
impl PluginHandler for EditorStub {
    async fn on_ready(&self, ctx: &Context) {
        if let Err(e) = ctx
            .register_service(SERVICE, &["selection", "replaceSelection"])
            .await
        {
            eprintln!("登记 {SERVICE} 服务失败：{e}");
        }
        if let Err(e) = ctx.subscribe("vault:file.changed").await {
            eprintln!("订阅文件变更失败：{e}");
        }
        self.inner.probe(ctx).await;
    }

    async fn on_host_event(&self, ctx: &Context, _params: PublishParams) {
        // 通知 worker 必须空出来处理 `host:service.call`：探针会调到本插件自己
        let inner = self.inner.clone();
        let ctx = ctx.clone();
        tokio::spawn(async move { inner.probe(&ctx).await });
    }

    async fn on_service_call(&self, ctx: &Context, params: HostCallParams) {
        let result = json!({
            "marker": "stub-editor",
            "service": params.service,
            "method": params.method,
            "params": params.params,
        });
        if let Err(e) = ctx.service_result(&params.call_id, result).await {
            eprintln!("回报服务调用失败：{e}");
        }
    }
}

atelyx_plugin_sdk::export_in_process_plugin!(EditorStub {
    inner: Arc::new(Inner {
        probe: AtomicU64::new(0),
        probe_lock: tokio::sync::Mutex::new(()),
    }),
});
