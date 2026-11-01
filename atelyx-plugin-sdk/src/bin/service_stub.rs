//! 端到端测试夹具：一个外部服务提供者兼调用方。
//!
//! 登记的服务由命令行第一个参数决定：`demo` = 登记 `demo.echo`；缺省 = 登记 `stub.ping`
//! （应答带固定标记）——宿主拉起插件时不传参数，端到端用例走缺省形态。
//! 收到 `host:service.call` 时回显服务 / 方法与入参；每次探针都同时走「直接调用内置方法」
//! 与「`service.call` 信封」两条路，再调一次自己提供的服务，把三者结果与两处错误一并写进自持数据，
//! 供测试断言内置路径对拍、外部提供者的路由与回落。
//!
//! 探针由 `vault:file.changed` 触发并在独立任务里执行：`host:service.call` 与探针共用同一个
//! 通知 worker，探针若在回调里等自己提供的方法就会自锁。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use atelyx_plugin_sdk::protocol::envelope::RpcError;
use atelyx_plugin_sdk::protocol::method;
use atelyx_plugin_sdk::protocol::event::PublishParams;
use atelyx_plugin_sdk::protocol::service::HostCallParams;
use atelyx_plugin_sdk::{run_stdio, Context, PluginHandler, PluginInfo};
use serde_json::{json, Value};



#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Ping,
    Demo,
}

impl Mode {
    fn from_arg(arg: Option<&String>) -> Self {
        match arg.map(String::as_str) {
            Some("demo") => Self::Demo,
            _ => Self::Ping,
        }
    }

    fn service(self) -> &'static str {
        match self {
            Self::Ping => "stub",
            Self::Demo => "demo",
        }
    }

    fn method(self) -> &'static str {
        match self {
            Self::Ping => "ping",
            Self::Demo => "echo",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Ping => "stub",
            Self::Demo => "demo",
        }
    }
}

struct Inner {
    mode: Mode,
    /// 探针序号：每次落盘都带它，测试据此等新一次探针。
    probe: AtomicU64,
    /// 探针串行化：同一时刻只有一次探针在跑，落盘顺序与触发顺序一致。
    probe_lock: tokio::sync::Mutex<()>,
    /// `query` 的登记结果（不可替换服务应被拒），随探针一并落盘。
    query_registration: Mutex<Option<String>>,
}

impl Inner {
    async fn probe(&self, ctx: &Context) {
        let _guard = self.probe_lock.lock().await;
        let seq = self.probe.fetch_add(1, Ordering::SeqCst) + 1;
        // 用原始调用形态取结果：顶替生效后应答是提供者的任意形态，不能按内置结果反序列化
        // 内置方法：直接调用与总线调用必须落到同一实现
        let direct = ctx.call_raw(method::state::READ, Value::Null).await;
        let via = ctx.service_call("state", "read", Value::Null).await;
        // 自己提供的服务：走总线时落到本插件（顶替 / 回落由宿主决定）
        let provided = ctx.service_call("stub", "ping", json!({ "echo": 1 })).await;
        let unknown_service = ctx.service_call("nope", "any", Value::Null).await;
        let unknown_method = ctx.service_call("state", "nope", Value::Null).await;
        let query_registration = self.query_registration.lock().unwrap().clone();
        let state = json!({
            "probe": seq,
            "mode": self.mode.label(),
            "direct": outcome(direct),
            "via": outcome(via),
            "provided": outcome(provided),
            "errors": {
                "unknownService": outcome(unknown_service),
                "unknownMethod": outcome(unknown_method),
                "queryRegistration": query_registration,
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
        let mode = self.inner.mode;
        if let Err(e) = ctx.register_service(mode.service(), &[mode.method()]).await {
            eprintln!("登记服务失败：{e}");
        }
        // 不可替换的服务（字体 / 主题取自宿主 OS 与设置）必须被可读拒绝
        let rejection = match ctx.register_service("query", &["theme"]).await {
            Ok(()) => None,
            Err(e) => Some(e.message),
        };
        *self.inner.query_registration.lock().unwrap() = rejection;
        if let Err(e) = ctx.subscribe("vault:file.changed").await {
            eprintln!("订阅文件变更失败：{e}");
        }
        self.inner.probe(ctx).await;
    }

    async fn on_host_event(&self, ctx: &Context, _params: PublishParams) {
        // 通知 worker 必须空出来处理 `host:service.call`：顶替生效后探针会调到本插件自己
        let inner = self.inner.clone();
        let ctx = ctx.clone();
        tokio::spawn(async move { inner.probe(&ctx).await });
    }

    async fn on_service_call(&self, ctx: &Context, params: HostCallParams) {
        let result = json!({
            "marker": format!("stub-{}", self.inner.mode.label()),
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
    let mode = Mode::from_arg(std::env::args().nth(1).as_ref());
    let fixture = PluginInfo {
        id: "com.atelyx.service-stub".into(),
        name: "ServiceStub".into(),
        version: "1.0.0".into(),
    };
    let stub = ServiceStub {
        inner: Arc::new(Inner {
            mode,
            probe: AtomicU64::new(0),
            probe_lock: tokio::sync::Mutex::new(()),
            query_registration: Mutex::new(None),
        }),
    };
    if let Err(e) = run_stdio(fixture, stub).await {
        eprintln!("夹具启动失败：{e}");
        std::process::exit(1);
    }
}
