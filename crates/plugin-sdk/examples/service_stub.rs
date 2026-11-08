//! 端到端测试夹具（同进程 cdylib）：寄存器一个自己定义的服务 `demo.echo`（应答带固定标记），
//! 并同时用「直接调用 `demo.echo`」与「`service.call` 信封」两条路探针，把两者结果与三处错误
//! 一并写进自持数据，供测试断言外部提供者路由、内置名占位拒绝与错误码。
//!
//! 探针由 `vault:file.changed` 触发并在独立任务里执行：`host:service.call` 与探针共用同一个
//! 通知 worker，探针若在回调里等自己提供的方法就会自锁。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use atelyx_plugin_sdk::protocol::envelope::RpcError;
use atelyx_plugin_sdk::protocol::event::PublishParams;
use atelyx_plugin_sdk::protocol::service::HostCallParams;
use atelyx_plugin_sdk::{Context, PluginHandler};
use serde_json::{json, Value};

/// 夹具登记的服务名：插件自有的服务，不与内置服务重名。
const SERVICE: &str = "demo";
/// 夹具实现的方法名。
const METHOD: &str = "echo";

struct Inner {
    /// 探针序号：每次落盘都带它，测试据此等新一次探针。
    probe: AtomicU64,
    /// 探针串行化：同一时刻只有一次探针在跑，落盘顺序与触发顺序一致。
    probe_lock: tokio::sync::Mutex<()>,
    /// 内置服务名的登记结果（应被拒），随探针一并落盘。
    builtin_registration: Mutex<Option<String>>,
}

impl Inner {
    async fn probe(&self, ctx: &Context) {
        let _guard = self.probe_lock.lock().await;
        let seq = self.probe.fetch_add(1, Ordering::SeqCst) + 1;
        // 用原始调用形态取结果：应答是提供者的任意形态，不能按内置结果反序列化
        let direct = ctx.call_raw(&format!("{SERVICE}.{METHOD}"), json!({ "text": "hi" })).await;
        let via = ctx.service_call(SERVICE, METHOD, json!({ "text": "hi" })).await;
        let unknown_service = ctx.service_call("nope", "any", Value::Null).await;
        let unknown_method = ctx.service_call(SERVICE, "nope", Value::Null).await;
        let builtin_registration = self.builtin_registration.lock().unwrap().clone();
        let state = json!({
            "probe": seq,
            "direct": outcome(direct),
            "via": outcome(via),
            "errors": {
                "unknownService": outcome(unknown_service),
                "unknownMethod": outcome(unknown_method),
                "builtinRegistration": builtin_registration,
            },
        });
        if let Err(e) = ctx.state_write(state).await {
            eprintln!("探针落盘失败：{e}");
        }
    }
}

fn outcome<T: serde::Serialize>(result: Result<T, RpcError>) -> Value {
    match result {
        Ok(value) => json!({ "ok": value }),
        Err(error) => json!({ "error": error.message }),
    }
}

struct ServiceStub {
    inner: Arc<Inner>,
}

#[async_trait::async_trait]
impl PluginHandler for ServiceStub {
    async fn on_ready(&self, ctx: &Context) {
        if let Err(e) = ctx.register_service(SERVICE, &[METHOD]).await {
            eprintln!("登记服务失败：{e}");
        }
        // 内置服务名归宿主：占用同名服务必须被可读拒绝
        let rejection = match ctx.register_service("markdown", &["parse"]).await {
            Ok(()) => None,
            Err(e) => Some(e.message),
        };
        *self.inner.builtin_registration.lock().unwrap() = rejection;
        if let Err(e) = ctx.subscribe("vault:file.changed").await {
            eprintln!("订阅文件变更失败：{e}");
        }
        self.inner.probe(ctx).await;
    }

    async fn on_host_event(&self, ctx: &Context, _params: PublishParams) {
        // 通知 worker 必须空出来处理 `host:service.call`：探针会调到本插件自己提供的方法
        let inner = self.inner.clone();
        let ctx = ctx.clone();
        tokio::spawn(async move { inner.probe(&ctx).await });
    }

    async fn on_service_call(&self, ctx: &Context, params: HostCallParams) {
        let result = json!({
            "marker": "stub-echo",
            "service": params.service,
            "method": params.method,
            "params": params.params,
        });
        if let Err(e) = ctx.service_result(&params.call_id, result).await {
            eprintln!("回报服务调用失败：{e}");
        }
    }
}

atelyx_plugin_sdk::export_in_process_plugin!(ServiceStub {
    inner: Arc::new(Inner {
        probe: AtomicU64::new(0),
        probe_lock: tokio::sync::Mutex::new(()),
        builtin_registration: Mutex::new(None),
    }),
});
