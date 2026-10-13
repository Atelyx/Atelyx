//! echo 示例插件：SDK 的最小完整样例兼开发夹具——握手后提交一棵界面树，
//! 按钮点击 / 输入提交 / 宿主广播事件都会回写状态文本（整树重发，键不变由宿主 diff）。

use std::sync::atomic::{AtomicU64, Ordering};

use atelyx_plugin_sdk::protocol::view::{ButtonVariant, EventKind, ViewEvent, ViewNode};
use atelyx_plugin_sdk::view_builders::{button, column, text, text_input, NodeExt};
use atelyx_plugin_sdk::{run_stdio, Context, PluginHandler, PluginInfo};
use serde_json::json;

const SURFACE: &str = "main";

struct Echo {
    clicks: AtomicU64,
}

impl Echo {
    fn tree(&self, status: &str) -> ViewNode {
        column(
            "root",
            vec![
                text("status", status),
                text_input("query", "").with_events(vec![EventKind::Submit]),
                button("go", "点击", ButtonVariant::Primary).with_events(vec![EventKind::Click]),
            ],
        )
    }
}

#[async_trait::async_trait]
impl PluginHandler for Echo {
    async fn on_ready(&self, ctx: &Context) {
        if let Err(e) = ctx.subscribe("echo.ping").await {
            eprintln!("订阅 echo.ping 失败：{e}");
        }
        if let Err(e) = ctx.submit_view(SURFACE, self.tree("就绪")).await {
            eprintln!("提交初始界面失败：{e}");
        }
    }

    async fn on_view_event(
        &self,
        ctx: &Context,
        params: atelyx_plugin_sdk::protocol::method::ViewEventParams,
    ) {
        let status = match (&params.event, params.node_id.as_str()) {
            (ViewEvent::Click, "go") => {
                let n = self.clicks.fetch_add(1, Ordering::Relaxed) + 1;
                format!("点击 {n} 次")
            }
            (ViewEvent::Submit { value }, "query") => {
                format!("收到：{}", serde_json::to_string(value).unwrap_or_default())
            }
            _ => return,
        };
        if let Err(e) = ctx.submit_view(SURFACE, self.tree(&status)).await {
            eprintln!("提交界面失败：{e}");
        }
    }

    async fn on_host_event(
        &self,
        ctx: &Context,
        params: atelyx_plugin_sdk::protocol::event::PublishParams,
    ) {
        if params.name != "echo.ping" {
            return;
        }
        // 能力面往返：写 state → 读 state → 把读到的值回显到界面
        let pong = json!({ "pong": params.payload });
        if let Err(e) = ctx.state_write(pong).await {
            eprintln!("写 state 失败：{e}");
            return;
        }
        let read = match ctx.state_read().await {
            Ok(v) => v,
            Err(e) => {
                eprintln!("读 state 失败：{e}");
                return;
            }
        };
        let status = format!("回声：{}", read["pong"]);
        if let Err(e) = ctx.submit_view(SURFACE, self.tree(&status)).await {
            eprintln!("提交界面失败：{e}");
        }
    }
}

#[tokio::main]
async fn main() {
    let plugin = PluginInfo {
        id: "com.atelyx.echo".into(),
        name: "Echo".into(),
        version: "0.1.0".into(),
    };
    if let Err(e) = run_stdio(plugin, Echo { clicks: AtomicU64::new(0) }).await {
        eprintln!("插件启动失败：{e}");
        std::process::exit(1);
    }
}
