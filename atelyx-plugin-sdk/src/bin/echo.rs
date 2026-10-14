//! echo 示例插件：SDK 的最小完整样例兼开发夹具——注册一个面板贡献后提交界面树，
//! 输入 / 下拉 / 勾选 / 按钮事件回写状态与日志（整树重发，节点 id 不变由宿主复用控件态）；
//! 宿主广播走 state 写读往返（`echo.ping` 回显到界面，`vault:switch` 把容器身份落盘）。

use atelyx_plugin_sdk::protocol::event;
use atelyx_plugin_sdk::protocol::method::ContributionSlot;
use atelyx_plugin_sdk::protocol::view::{ButtonVariant, EventKind, EventValue, ViewEvent, ViewNode};
use atelyx_plugin_sdk::view_builders::{
    button, checkbox, column, dropdown, icon_button, list, row, text, text_input, NodeExt,
};
use atelyx_plugin_sdk::{run_stdio, Context, PluginHandler, PluginInfo};
use serde_json::json;
use tokio::sync::Mutex;

/// 面板贡献 id，同时是该面挂载后的 surfaceId。
const PANEL: &str = "panel";

/// 插件自持状态：任一事件都整树重发，界面即状态的投影。
#[derive(Default)]
struct Model {
    status: String,
    query: String,
    engine: Option<u32>,
    flag: bool,
    clicks: u64,
    log: Vec<String>,
}

struct Echo {
    model: Mutex<Model>,
}

impl Echo {
    fn tree(model: &Model) -> ViewNode {
        let mut entries: Vec<ViewNode> = model
            .log
            .iter()
            .enumerate()
            .map(|(index, line)| text(&format!("log{index}"), line.clone()))
            .collect();
        if entries.is_empty() {
            entries.push(text("log0", "（暂无操作）"));
        }
        column(
            "root",
            vec![
                text("status", model.status.clone()),
                text_input("query", model.query.clone()).with_events(vec![
                    EventKind::Change,
                    EventKind::Submit,
                ]),
                dropdown("engine", vec!["CPU".into(), "CUDA".into()], model.engine)
                    .with_events(vec![EventKind::Change]),
                checkbox("flag", model.flag).with_events(vec![EventKind::Change]),
                row(
                    "tools",
                    vec![
                        button("go", "开始", ButtonVariant::Primary)
                            .with_events(vec![EventKind::Click]),
                        icon_button("shot", "camera").with_events(vec![EventKind::Click]),
                    ],
                ),
                list("log", entries),
            ],
        )
    }

    /// 改状态后提交整树：持锁提交，保证「最后落地的树 = 最终状态」。
    async fn apply(&self, ctx: &Context, update: impl FnOnce(&mut Model)) {
        let mut model = self.model.lock().await;
        update(&mut model);
        if let Err(e) = ctx.submit_view(PANEL, Self::tree(&model)).await {
            eprintln!("提交界面失败：{e}");
        }
    }

    async fn submit(&self, ctx: &Context) {
        let model = self.model.lock().await;
        if let Err(e) = ctx.submit_view(PANEL, Self::tree(&model)).await {
            eprintln!("提交界面失败：{e}");
        }
    }
}

/// 事件值 → 文本（数值与布尔按最小形态呈现，示例面板直接展示宿主回发的值）。
fn value_text(value: EventValue) -> String {
    match value {
        EventValue::Text(text) => text,
        EventValue::Number(number) => number.to_string(),
        EventValue::Bool(flag) => flag.to_string(),
    }
}

#[async_trait::async_trait]
impl PluginHandler for Echo {
    async fn on_ready(&self, ctx: &Context) {
        // `echo.ping` 为示例事件名（宿主只广播 `vault:switch`，该名由宿主桩直接发送）
        for name in ["echo.ping", event::name::VAULT_SWITCH] {
            if let Err(e) = ctx.subscribe(name).await {
                eprintln!("订阅 {name} 失败：{e}");
            }
        }
        if let Err(e) = ctx.register(ContributionSlot::Panel, PANEL, "示例面板", Some("puzzle")).await
        {
            eprintln!("注册面板贡献失败：{e}");
        }
        self.apply(ctx, |m| m.status = "就绪".into()).await;
    }

    async fn on_view_event(
        &self,
        ctx: &Context,
        params: atelyx_plugin_sdk::protocol::method::ViewEventParams,
    ) {
        match (params.node_id.as_str(), params.event) {
            ("go", ViewEvent::Click) => {
                self.apply(ctx, |m| {
                    m.clicks += 1;
                    m.status = format!("点击 {} 次", m.clicks);
                    m.log.push(format!("点击第 {} 次", m.clicks));
                })
                .await;
            }
            ("shot", ViewEvent::Click) => {
                self.apply(ctx, |m| {
                    m.status = "已截图".into();
                    m.log.push("截图".into());
                })
                .await;
            }
            ("query", ViewEvent::Change { value: EventValue::Text(text) })
            | ("query", ViewEvent::Submit { value: EventValue::Text(text) }) => {
                self.apply(ctx, |m| {
                    m.status = format!("输入：{text}");
                    m.query = text;
                })
                .await;
            }
            ("engine", ViewEvent::Change { value }) => {
                let text = value_text(value);
                self.apply(ctx, |m| {
                    m.status = format!("引擎：{text}");
                    m.engine = text.parse::<u32>().ok();
                })
                .await;
            }
            ("flag", ViewEvent::Change { value: EventValue::Bool(on) }) => {
                self.apply(ctx, |m| {
                    m.status = if on { "自动启动：开" } else { "自动启动：关" }.into();
                    m.flag = on;
                })
                .await;
            }
            _ => {}
        }
    }

    /// 面重新就绪（面板刚挂上 / 插件重启后宿主重发 attached）：重发当前界面。
    async fn on_surface_attached(
        &self,
        ctx: &Context,
        _params: atelyx_plugin_sdk::protocol::method::SurfaceAttachedParams,
    ) {
        self.submit(ctx).await;
    }

    async fn on_host_event(
        &self,
        ctx: &Context,
        params: atelyx_plugin_sdk::protocol::event::PublishParams,
    ) {
        match params.name.as_str() {
            // 能力面往返：写 state → 读 state → 把读到的值回显到界面
            "echo.ping" => {
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
                self.apply(ctx, |m| m.status = status).await;
            }
            // 容器切换：新容器身份落进自持数据（插件重启后据此恢复上下文）
            event::name::VAULT_SWITCH => {
                if let Err(e) = ctx.state_write(json!({ "vault_switch": params.payload })).await {
                    eprintln!("写 vault:switch 失败：{e}");
                }
            }
            _ => {}
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
    if let Err(e) = run_stdio(plugin, Echo { model: Mutex::default() }).await {
        eprintln!("插件启动失败：{e}");
        std::process::exit(1);
    }
}
