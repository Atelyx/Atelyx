//! 端到端测试夹具：注册面板贡献并提交整面帧（计数按钮 + 文本输入区 + 滚轮视口），
//! 全部原始输入经 `host:frame.input` 驱动整帧重发——仅覆盖面帧通路，非示例插件。

use std::sync::{Arc, Mutex};

use atelyx_plugin_sdk::canvas_builders::{draw_text, rect, token};
use atelyx_plugin_sdk::frame_builders::{fps, frame, hit, ime, ime_rect, keys};
use atelyx_plugin_sdk::protocol::canvas::DrawCommand;
use atelyx_plugin_sdk::protocol::frame::{FrameInput, FrameInputParams, FrameTickParams};
use atelyx_plugin_sdk::protocol::method::ContributionSlot;
use atelyx_plugin_sdk::protocol::view::{ColorToken, Key};
use atelyx_plugin_sdk::{run_stdio, Context, PluginHandler, PluginInfo};

/// 面板贡献 id，同时是该面挂载后的 surfaceId。
const PANEL: &str = "frame";

/// 夹具自持状态：任一输入都整帧重发，帧即状态的投影。
struct Model {
    count: i64,
    typed: String,
    composing: String,
    last_key: String,
    offset: f32,
    tick: u64,
    size: (f32, f32),
    focused: bool,
}

impl Default for Model {
    fn default() -> Self {
        Self {
            count: 0,
            typed: String::new(),
            composing: String::new(),
            last_key: "无".into(),
            offset: 0.0,
            tick: 0,
            size: (320.0, 280.0),
            focused: false,
        }
    }
}

fn key_label(key: &Key) -> String {
    match key {
        Key::Named { name } => format!("named:{name:?}"),
        Key::Character { text } => format!("char:{text}"),
    }
}

impl Model {
    fn commands(&self) -> Vec<DrawCommand> {
        let (w, h) = self.size;
        let text = token(ColorToken::Text);
        let muted = token(ColorToken::TextMuted);
        let field = token(ColorToken::SurfaceSunken);
        let border = token(ColorToken::Border);
        let mut out = vec![
            rect(0.0, 0.0, w, h, Some(token(ColorToken::Surface)), None),
            draw_text(12.0, 8.0, format!("计数：{}", self.count), 16.0, text.clone()),
            draw_text(12.0, 32.0, format!("键入：{}", self.typed), 13.0, text.clone()),
            draw_text(12.0, 52.0, format!("组合：{}", self.composing), 13.0, muted.clone()),
            draw_text(12.0, 72.0, format!("键：{}", self.last_key), 13.0, muted.clone()),
            draw_text(12.0, 92.0, format!("尺寸：{w:.0}x{h:.0}"), 13.0, muted.clone()),
            draw_text(12.0, 112.0, format!("滚动：{:.0}", self.offset), 13.0, muted.clone()),
            draw_text(12.0, 132.0, format!("帧：{}", self.tick), 13.0, muted.clone()),
            draw_text(12.0, 152.0, format!("焦点：{}", self.focused), 13.0, muted),
            rect(12.0, 176.0, 90.0, 28.0, Some(token(ColorToken::AccentSoft)), Some(border.clone())),
            draw_text(34.0, 184.0, "加一", 13.0, token(ColorToken::Accent)),
            rect(112.0, 176.0, 70.0, 28.0, Some(token(ColorToken::SurfaceMuted)), Some(border.clone())),
            draw_text(128.0, 184.0, "清空", 13.0, text.clone()),
            rect(12.0, 214.0, w - 24.0, 30.0, Some(field), Some(border)),
            draw_text(
                18.0,
                222.0,
                format!("{}{}", self.typed, self.composing),
                13.0,
                text,
            ),
            // 视口：一条随滚轮下移的标记线
            rect(12.0, 252.0, w - 24.0, 4.0, Some(token(ColorToken::TextMuted)), None),
        ];
        out.push(rect(
            12.0 + self.offset,
            248.0,
            6.0,
            12.0,
            Some(token(ColorToken::Accent)),
            None,
        ));
        out
    }

    fn frame(&self) -> atelyx_plugin_sdk::protocol::frame::SurfaceFrame {
        let (w, _) = self.size;
        let hits = vec![
            hit("surface", 0.0, 0.0, w, self.size.1),
            hit("inc", 12.0, 176.0, 90.0, 28.0),
            hit("clear", 112.0, 176.0, 70.0, 28.0),
            hit("field", 12.0, 214.0, w - 24.0, 30.0),
        ];
        let base = frame(self.commands(), hits);
        fps(ime(keys(base), ime_rect(18.0, 222.0, 2.0, 18.0)), 4)
    }
}

struct FrameStub {
    model: Arc<Mutex<Model>>,
}

impl FrameStub {
    /// 改状态后整帧重发：输入回调按到达顺序执行，提交即在回调内串行完成。
    async fn apply(&self, ctx: &Context, update: impl FnOnce(&mut Model)) {
        let frame = {
            let mut model = self.model.lock().unwrap();
            update(&mut model);
            model.frame()
        };
        if let Err(e) = ctx.submit_frame(PANEL, frame).await {
            eprintln!("提交面帧失败：{e}");
        }
    }
}

#[async_trait::async_trait]
impl PluginHandler for FrameStub {
    async fn on_ready(&self, ctx: &Context) {
        if let Err(e) = ctx
            .register(ContributionSlot::Panel, PANEL, "面帧", Some("puzzle"), None)
            .await
        {
            eprintln!("注册面板贡献失败：{e}");
        }
        self.apply(ctx, |_| {}).await;
    }

    async fn on_frame_input(&self, ctx: &Context, params: FrameInputParams) {
        if params.surface_id != PANEL {
            return;
        }
        match params.input {
            FrameInput::PointerDown { hit: Some(id), .. } => match id.as_str() {
                "inc" => self.apply(ctx, |m| m.count += 1).await,
                "clear" => {
                    self.apply(ctx, |m| {
                        m.typed.clear();
                        m.composing.clear();
                    })
                    .await
                }
                _ => {}
            },
            FrameInput::Wheel { delta_y, .. } => {
                self.apply(ctx, |m| {
                    m.offset = (m.offset + delta_y * 8.0).clamp(0.0, 80.0);
                })
                .await
            }
            FrameInput::Key { key, .. } => {
                self.apply(ctx, |m| m.last_key = key_label(&key)).await
            }
            FrameInput::Text { text, composing, .. } => {
                self.apply(ctx, |m| {
                    if composing {
                        m.composing = text;
                    } else {
                        m.composing.clear();
                        m.typed.push_str(&text);
                    }
                })
                .await
            }
            FrameInput::Focus { focused } => {
                self.apply(ctx, |m| {
                    m.focused = focused;
                    m.last_key = format!("focus:{focused}");
                })
                .await
            }
            FrameInput::Resize { width, height } => {
                self.apply(ctx, |m| m.size = (width, height)).await
            }
            _ => {}
        }
    }

    async fn on_frame_tick(&self, ctx: &Context, params: FrameTickParams) {
        if params.surface_id != PANEL {
            return;
        }
        self.apply(ctx, |m| m.tick = params.frame).await;
    }
}

#[tokio::main]
async fn main() {
    let fixture = PluginInfo {
        id: "com.atelyx.frame-stub".into(),
        name: "FrameStub".into(),
        version: "0.1.0".into(),
    };
    let stub = FrameStub { model: Arc::new(Mutex::new(Model::default())) };
    if let Err(e) = run_stdio(fixture, stub).await {
        eprintln!("夹具启动失败：{e}");
        std::process::exit(1);
    }
}
