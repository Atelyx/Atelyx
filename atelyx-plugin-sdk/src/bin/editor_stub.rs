//! 端到端测试夹具：`editor` 服务的外部提供者。
//!
//! 登记时只是候选（内置实现仍活动）；成为活动提供者后才会发起探针——内置路径由壳执行，
//! 没有壳的进程内测试里那些调用不会返回。探针同时走「直接调用 `editor.selection`」与
//! 「`service.call` 信封」，把两侧结果写进自持数据，供测试断言顶替后两条路都落到本插件。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use atelyx_plugin_sdk::protocol::envelope::RpcError;
use atelyx_plugin_sdk::protocol::event::PublishParams;
use atelyx_plugin_sdk::protocol::method;
use atelyx_plugin_sdk::protocol::service::{ChangedParams, HostCallParams};
use atelyx_plugin_sdk::{run_stdio, Context, PluginHandler, PluginInfo};
use serde_json::{json, Value};

const STUB_ID: &str = "com.atelyx.editor-stub";

struct Inner {
    /// 探针序号：每次落盘都带它，测试据此等新一次探针。
    probe: AtomicU64,
    /// 探针串行化：同一时刻只有一次探针在跑，落盘顺序与触发顺序一致。
    probe_lock: tokio::sync::Mutex<()>,
    /// 最近一次 `host:service.changed` 是否把自己报为活动提供者。
    active: AtomicBool,
}

fn outcome<T: serde::Serialize>(result: Result<T, RpcError>) -> Value {
    match result {
        Ok(value) => json!({ "ok": value }),
        Err(error) => json!({ "error": error.message }),
    }
}

impl Inner {
    async fn probe(&self, ctx: &Context) {
        if !self.active.load(Ordering::SeqCst) {
            return;
        }
        let _guard = self.probe_lock.lock().await;
        let seq = self.probe.fetch_add(1, Ordering::SeqCst) + 1;
        let selection = ctx.service_call("editor", "selection", json!({})).await;
        let replace = ctx
            .service_call("editor", "replaceSelection", json!({ "text": "*" }))
            .await;
        let direct = ctx.call_raw(method::editor::SELECTION, json!({})).await;
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
            .register_service("editor", &["selection", "replaceSelection"])
            .await
        {
            eprintln!("登记 editor 服务失败：{e}");
        }
        if let Err(e) = ctx.subscribe("vault:file.changed").await {
            eprintln!("订阅文件变更失败：{e}");
        }
    }

    async fn on_service_changed(&self, ctx: &Context, params: ChangedParams) {
        if params.service != "editor" {
            return;
        }
        self.inner
            .active
            .store(params.provider.as_deref() == Some(STUB_ID) && params.available, Ordering::SeqCst);
        // 通知 worker 必须空出来处理 `host:service.call`：探针会调到本插件自己
        let inner = self.inner.clone();
        let ctx = ctx.clone();
        tokio::spawn(async move { inner.probe(&ctx).await });
    }

    async fn on_host_event(&self, ctx: &Context, _params: PublishParams) {
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

#[tokio::main]
async fn main() {
    let fixture = PluginInfo {
        id: STUB_ID.into(),
        name: "EditorStub".into(),
        version: "1.0.0".into(),
    };
    let stub = EditorStub {
        inner: Arc::new(Inner {
            probe: AtomicU64::new(0),
            probe_lock: tokio::sync::Mutex::new(()),
            active: AtomicBool::new(false),
        }),
    };
    if let Err(e) = run_stdio(fixture, stub).await {
        eprintln!("夹具启动失败：{e}");
        std::process::exit(1);
    }
}
