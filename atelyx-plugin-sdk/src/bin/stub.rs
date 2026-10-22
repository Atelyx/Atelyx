//! 端到端测试夹具：握手后注册面板贡献并提交最小界面树（状态文本 + 输入 + 画布），
//! 指针事件驱动清单重发保持最小实现——仅供 `host_e2e` 拉起真实进程验证运行时链路，非示例插件。

use std::sync::{Arc, Mutex};

use atelyx_plugin_sdk::canvas_builders::{circle, draw_text, line, path, rect, rgba, token};
use atelyx_plugin_sdk::protocol::canvas::DrawCommand;
use atelyx_plugin_sdk::protocol::event;
use atelyx_plugin_sdk::protocol::method::ContributionSlot;
use atelyx_plugin_sdk::protocol::view::{
    CanvasEventKind, CanvasPointer, ColorToken, EventKind, EventValue, MouseButton, ReportMode,
    ViewEvent, ViewNode,
};
use atelyx_plugin_sdk::view_builders::{canvas, column, text, text_input, NodeExt, NodeId};
use atelyx_plugin_sdk::{run_stdio, Context, PluginHandler, PluginInfo};
use serde_json::json;

/// 面板贡献 id，同时是该面挂载后的 surfaceId。
const PANEL: &str = "panel";
const STATUS: NodeId<'static> = NodeId::new("status");
const QUERY: NodeId<'static> = NodeId::new("query");
/// 画布节点 id。
const BOARD: NodeId<'static> = NodeId::new("board");
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
    model: Arc<Mutex<Model>>,
    /// 画布清单整发整换：提交串行在门内，避免旧清单后到覆盖新清单。
    canvas_gate: tokio::sync::Mutex<()>,
}

impl Stub {
    fn tree(model: &Model) -> ViewNode {
        column(
            "root",
            vec![
                text(STATUS, model.status.clone()),
                text_input(QUERY, model.query.clone(), ReportMode::Immediate).with_events(vec![
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

    /// 改状态后通知 SDK 重发整树：构建与提交串行在面级门内，最后落地的树 = 最终状态。
    async fn apply(&self, ctx: &Context, update: impl FnOnce(&mut Model)) {
        update(&mut self.model.lock().unwrap());
        if let Err(e) = ctx.ui().notify(PANEL).await {
            eprintln!("提交界面失败：{e}");
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

    /// 改状态后重发指令清单：清单在状态锁内算好，提交串行在画布门内。
    async fn apply_canvas(&self, ctx: &Context, update: impl FnOnce(&mut Model)) {
        let _gate = self.canvas_gate.lock().await;
        let commands = {
            let mut model = self.model.lock().unwrap();
            update(&mut model);
            Self::canvas_commands(&model)
        };
        if let Err(e) = ctx.submit_canvas(PANEL, BOARD.as_str(), commands).await {
            eprintln!("提交画布失败：{e}");
        }
    }
}

#[async_trait::async_trait]
impl PluginHandler for Stub {
    async fn on_ready(&self, ctx: &Context) {
        let model = self.model.clone();
        ctx.ui().mount(PANEL, move || Self::tree(&model.lock().unwrap())).await;
        // `stub.ping` 为夹具事件名（宿主只广播领域事件，该名由宿主桩直接发送）
        for name in ["stub.ping", event::name::VAULT_SWITCH, event::name::AI_CHAT_CHANGED] {
            if let Err(e) = ctx.subscribe(name).await {
                eprintln!("订阅 {name} 失败：{e}");
            }
        }
        if let Err(e) = ctx.register(ContributionSlot::Panel, PANEL, "面板", Some("puzzle"), None).await {
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
        match params.event {
            ViewEvent::Change { value: EventValue::Text(text) }
            | ViewEvent::Submit { value: EventValue::Text(text) }
                if params.node_id == QUERY =>
            {
                self.apply(ctx, |m| {
                    m.status = format!("输入：{text}");
                    m.query = text;
                })
                .await;
            }
            ViewEvent::Canvas { pointer } if params.node_id == BOARD => {
                self.on_pointer(ctx, pointer).await;
            }
            _ => {}
        }
    }

    /// 面重新就绪（面板刚挂上 / 夹具重启后宿主重发 attached）：SDK 已重发界面树，这里补交画布清单。
    async fn on_surface_attached(
        &self,
        ctx: &Context,
        _params: atelyx_plugin_sdk::protocol::method::SurfaceAttachedParams,
    ) {
        self.apply_canvas(ctx, |_| {}).await;
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
            // 会话变更：载荷落进自持数据
            event::name::AI_CHAT_CHANGED => {
                if let Err(e) = ctx.state_write(json!({ "chat_changed": params.payload })).await {
                    eprintln!("写 ai:chat.changed 失败：{e}");
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
    let stub = Stub {
        model: Arc::new(Mutex::new(Model::default())),
        canvas_gate: tokio::sync::Mutex::new(()),
    };
    if let Err(e) = run_stdio(fixture, stub).await {
        eprintln!("夹具启动失败：{e}");
        std::process::exit(1);
    }
}
