//! 端到端测试夹具：握手后注册面板贡献并提交最小界面树（状态文本 + 输入 + 画布），
//! 指针事件驱动清单重发保持最小实现——仅供 `host_e2e` 拉起真实进程验证运行时链路，非示例插件。

use atelyx_plugin_sdk::canvas_builders::{circle, draw_text, line, path, rect, rgba, token};
use atelyx_plugin_sdk::protocol::canvas::DrawCommand;
use atelyx_plugin_sdk::protocol::event;
use atelyx_plugin_sdk::protocol::method::ContributionSlot;
use atelyx_plugin_sdk::protocol::view::{
    CanvasEventKind, CanvasPointer, ColorToken, EventKind, EventValue, MouseButton, ViewEvent,
    ViewNode,
};
use atelyx_plugin_sdk::view_builders::{canvas, column, text, text_input, NodeExt};
use atelyx_plugin_sdk::{run_stdio, Context, PluginHandler, PluginInfo};
use serde_json::json;
use tokio::sync::Mutex;

/// 面板贡献 id，同时是该面挂载后的 surfaceId。
const PANEL: &str = "panel";
/// 画布节点 id。
const BOARD: &str = "board";
/// 画布文档区（参考框）：夹具的采点与游标都约束在其内——宿主不裁剪内容，坐标语义归插件自管。
const BOARD_W: f32 = 320.0;
const BOARD_H: f32 = 200.0;

fn clamp_to_board(x: f32, y: f32) -> (f32, f32) {
    (x.clamp(0.0, BOARD_W), y.clamp(0.0, BOARD_H))
}

/// 夹具自持状态：任一事件都整树重发，界面即状态的投影。
#[derive(Default)]
struct Model {
    status: String,
    query: String,
    drawing: bool,
    trail: Vec<(f32, f32)>,
    cursor: Option<(f32, f32)>,
    radius: f32,
    note: String,
}

struct Stub {
    model: Mutex<Model>,
}

impl Stub {
    fn tree(model: &Model) -> ViewNode {
        column(
            "root",
            vec![
                text("status", model.status.clone()),
                text_input("query", model.query.clone()).with_events(vec![
                    EventKind::Change,
                    EventKind::Submit,
                ]),
                canvas(
                    BOARD,
                    vec![
                        CanvasEventKind::PointerDown,
                        CanvasEventKind::PointerMove,
                        CanvasEventKind::PointerUp,
                        CanvasEventKind::PointerLeave,
                        CanvasEventKind::Wheel,
                        CanvasEventKind::DoubleClick,
                    ],
                ),
            ],
        )
    }

    /// 画布指令清单：网格背景 + 采点折线 + 游标圆（半径随滚轮）。
    fn canvas_commands(model: &Model) -> Vec<DrawCommand> {
        let mut out = vec![
            rect(0.5, 0.5, BOARD_W - 1.0, BOARD_H - 1.0, None, Some(token(ColorToken::TextMuted))),
            draw_text(6.0, 4.0, model.note.clone(), 12.0, token(ColorToken::TextSecondary)),
        ];
        for x in (1..4).map(|i| i as f32 * BOARD_W / 4.0) {
            out.push(line(x, 0.0, x, BOARD_H, token(ColorToken::TextMuted)));
        }
        for y in (1..3).map(|i| i as f32 * BOARD_H / 3.0) {
            out.push(line(0.0, y, BOARD_W, y, token(ColorToken::TextMuted)));
        }
        if model.trail.len() >= 2 {
            out.push(path(model.trail.clone(), false, None, Some(rgba(64, 128, 255, 255))));
        }
        if let Some((x, y)) = model.cursor {
            out.push(circle(x, y, model.radius, Some(rgba(255, 176, 32, 160)), None));
        }
        out
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
        if let Err(e) = ctx.submit_canvas(PANEL, BOARD, Self::canvas_commands(&model)).await {
            eprintln!("提交画布失败：{e}");
        }
    }

    /// 画布指针事件：只重发指令清单，不动界面树。
    async fn on_pointer(&self, ctx: &Context, pointer: CanvasPointer) {
        self.apply_canvas(ctx, |m| match pointer {
            CanvasPointer::Down { x, y, button: MouseButton::Left } if x <= BOARD_W && y <= BOARD_H => {
                m.drawing = true;
                m.trail.push((x, y));
                m.cursor = Some((x, y));
                m.note = format!("拖拽画线（{x:.0}, {y:.0}）");
            }
            CanvasPointer::Down { x, y, .. } => {
                m.cursor = Some(clamp_to_board(x, y));
                m.note = if x > BOARD_W || y > BOARD_H {
                    "框外按下无效".into()
                } else {
                    format!("非左键按下（{x:.0}, {y:.0}）")
                };
            }
            CanvasPointer::Move { x, y } => {
                let point = clamp_to_board(x, y);
                m.cursor = Some(point);
                if m.drawing {
                    m.trail.push(point);
                    m.note = format!("采点（{:.0}, {:.0}）", point.0, point.1);
                } else {
                    m.note = format!("（{:.0}, {:.0}）", point.0, point.1);
                }
            }
            CanvasPointer::Up { .. } => {
                m.drawing = false;
                m.note = "已抬笔".into();
            }
            CanvasPointer::Leave => {
                m.drawing = false;
                m.cursor = None;
                m.note = "移出画布".into();
            }
            CanvasPointer::Wheel { delta_y, .. } => {
                m.radius = (m.radius + delta_y * 4.0).clamp(2.0, 40.0);
                m.note = format!("游标半径 {:.0}", m.radius);
            }
            CanvasPointer::DoubleClick { .. } => {
                m.trail.clear();
                m.note = "双击清空".into();
            }
        })
        .await;
    }

    /// 改状态后重发指令清单。
    async fn apply_canvas(&self, ctx: &Context, update: impl FnOnce(&mut Model)) {
        let mut model = self.model.lock().await;
        update(&mut model);
        if let Err(e) = ctx.submit_canvas(PANEL, BOARD, Self::canvas_commands(&model)).await {
            eprintln!("提交画布失败：{e}");
        }
    }
}

#[async_trait::async_trait]
impl PluginHandler for Stub {
    async fn on_ready(&self, ctx: &Context) {
        // `stub.ping` 为夹具事件名（宿主只广播 `vault:switch`，该名由宿主桩直接发送）
        for name in ["stub.ping", event::name::VAULT_SWITCH] {
            if let Err(e) = ctx.subscribe(name).await {
                eprintln!("订阅 {name} 失败：{e}");
            }
        }
        if let Err(e) = ctx.register(ContributionSlot::Panel, PANEL, "面板", Some("puzzle")).await {
            eprintln!("注册面板贡献失败：{e}");
        }
        self.apply(ctx, |m| {
            m.status = "就绪".into();
            if m.radius == 0.0 {
                m.radius = 8.0;
            }
        })
        .await;
    }

    async fn on_view_event(
        &self,
        ctx: &Context,
        params: atelyx_plugin_sdk::protocol::method::ViewEventParams,
    ) {
        match (params.node_id.as_str(), params.event) {
            ("query", ViewEvent::Change { value: EventValue::Text(text) })
            | ("query", ViewEvent::Submit { value: EventValue::Text(text) }) => {
                self.apply(ctx, |m| {
                    m.status = format!("输入：{text}");
                    m.query = text;
                })
                .await;
            }
            ("board", ViewEvent::Canvas { pointer }) => self.on_pointer(ctx, pointer).await,
            _ => {}
        }
    }

    /// 面重新就绪（面板刚挂上 / 夹具重启后宿主重发 attached）：重发当前界面。
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
            "stub.ping" => {
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
            // 容器切换：新容器身份落进自持数据（夹具重启后据此恢复上下文）
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
    let fixture = PluginInfo {
        id: "com.atelyx.stub".into(),
        name: "Stub".into(),
        version: "0.1.0".into(),
    };
    if let Err(e) = run_stdio(fixture, Stub { model: Mutex::default() }).await {
        eprintln!("夹具启动失败：{e}");
        std::process::exit(1);
    }
}
