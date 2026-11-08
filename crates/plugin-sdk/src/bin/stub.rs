//! 端到端测试夹具：注册一个面板贡献、提交一帧文本，并把收到的领域事件写进自持数据。
//! 覆盖「安装即启用 → 握手 → 提交面 → 停用收尾」与容器切换 / 会话广播的投递，帧内交互通路
//! 由 `frame_stub` 覆盖。

use atelyx_plugin_sdk::frame_builders::{frame, text, token};
use atelyx_plugin_sdk::protocol::event::PublishParams;
use atelyx_plugin_sdk::protocol::method::ContributionSlot;
use atelyx_plugin_sdk::protocol::paint::ColorToken;
use atelyx_plugin_sdk::{run_stdio, Context, PluginHandler, PluginInfo};

/// 面板贡献 id，同时是该面挂载后的 surfaceId。
const PANEL: &str = "panel";

struct Stub;

impl Stub {
    async fn submit(&self, ctx: &Context) {
        let frame = frame(
            vec![text(0.0, 0.0, "就绪", 14.0, token(ColorToken::Text))],
            Vec::new(),
        );
        if let Err(e) = ctx.submit_frame(PANEL, frame).await {
            eprintln!("提交面帧失败：{e}");
        }
    }
}

#[async_trait::async_trait]
impl PluginHandler for Stub {
    async fn on_ready(&self, ctx: &Context) {
        if let Err(e) = ctx
            .register(ContributionSlot::Panel, PANEL, "桩", Some("puzzle"), None)
            .await
        {
            eprintln!("注册面板贡献失败：{e}");
        }
        for name in ["vault:switch", "ai:chat.changed"] {
            if let Err(e) = ctx.subscribe(name).await {
                eprintln!("订阅 {name} 失败：{e}");
            }
        }
        self.submit(ctx).await;
    }

    async fn on_surface_attached(
        &self,
        ctx: &Context,
        _params: atelyx_plugin_sdk::protocol::method::SurfaceAttachedParams,
    ) {
        self.submit(ctx).await;
    }

    async fn on_host_event(&self, ctx: &Context, params: PublishParams) {
        let payload = serde_json::json!({ "event": params.name, "payload": params.payload });
        if let Err(e) = ctx.state_write(payload).await {
            eprintln!("写入自持数据失败：{e}");
        }
    }
}

#[tokio::main]
async fn main() {
    let fixture =
        PluginInfo { id: "com.atelyx.stub".into(), name: "Stub".into(), version: "1.0.0".into() };
    if let Err(e) = run_stdio(fixture, Stub).await {
        eprintln!("夹具启动失败：{e}");
        std::process::exit(1);
    }
}
