//! 端到端测试夹具：注册面板贡献，在宿主重绘拍里同步画出这一面（计数按钮 + 文本输入区 + 滚轮视口）。
//! 全部原始输入经 `host:frame.input` 到达，改完状态请求重绘——仅覆盖面帧通路，非示例插件。

use std::sync::{Arc, Mutex};

use atelyx_plugin_sdk::frame_builders::{rect, text as draw_text, token};
use atelyx_plugin_sdk::protocol::frame::{
    FrameInput, FrameInputParams, FrameRect, FrameTickParams, HitRegion,
};
use atelyx_plugin_sdk::protocol::method::ContributionSlot;
use atelyx_plugin_sdk::protocol::frame::Key;
use atelyx_plugin_sdk::protocol::paint::{ColorToken, DrawCommand};
use atelyx_plugin_sdk::{Context, DrawFrame, FrameUi, PluginHandler};

/// 面板贡献 id，同时是该面挂载后的 surfaceId。
const PANEL: &str = "frame";

/// 夹具自持状态：绘制永远是它在这一拍上的投影。
struct Model {
    count: i64,
    typed: String,
    composing: String,
    last_key: String,
    offset: f32,
    tick: u64,
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
    fn commands(&self, w: f32, h: f32) -> Vec<DrawCommand> {
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
            draw_text(12.0, 128.0, format!("滚动：{:.0}", self.offset), 13.0, muted.clone()),
            draw_text(12.0, 148.0, format!("帧：{}", self.tick), 13.0, muted.clone()),
            draw_text(12.0, 168.0, format!("焦点：{}", self.focused), 13.0, muted),
            rect(12.0, 192.0, 90.0, 28.0, Some(token(ColorToken::AccentSoft)), Some(border.clone())),
            draw_text(34.0, 200.0, "加一", 13.0, token(ColorToken::Accent)),
            rect(112.0, 192.0, 70.0, 28.0, Some(token(ColorToken::SurfaceMuted)), Some(border.clone())),
            draw_text(128.0, 200.0, "清空", 13.0, text.clone()),
            rect(12.0, 230.0, w - 24.0, 30.0, Some(field), Some(border)),
            draw_text(18.0, 238.0, format!("{}{}", self.typed, self.composing), 13.0, text),
            // 视口：一条随滚轮下移的标记线
            rect(12.0, 268.0, w - 24.0, 4.0, Some(token(ColorToken::TextMuted)), None),
        ];
        out.push(rect(
            12.0 + self.offset,
            264.0,
            6.0,
            12.0,
            Some(token(ColorToken::Accent)),
            None,
        ));
        out
    }

    fn hits(&self, w: f32, h: f32) -> Vec<HitRegion> {
        vec![
            HitRegion { id: "surface".into(), x: 0.0, y: 0.0, w, h, cursor: None },
            HitRegion { id: "inc".into(), x: 12.0, y: 192.0, w: 90.0, h: 28.0, cursor: None },
            HitRegion { id: "clear".into(), x: 112.0, y: 192.0, w: 70.0, h: 28.0, cursor: None },
            HitRegion { id: "field".into(), x: 12.0, y: 230.0, w: w - 24.0, h: 30.0, cursor: None },
        ]
    }
}

struct FrameStub {
    model: Arc<Mutex<Model>>,
}

impl FrameStub {
    /// 改状态后请求重绘：下一次绘制就是新状态的投影。
    async fn apply(&self, ctx: &Context, update: impl FnOnce(&mut Model)) {
        update(&mut self.model.lock().unwrap());
        if let Err(e) = ctx.request_redraw().await {
            eprintln!("请求重绘失败：{e}");
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
    }

    fn on_draw(&self, frame: &mut DrawFrame<'_, '_>) {
        let (w, h) = frame.size();
        let model = self.model.lock().unwrap();
        for command in model.commands(w, h) {
            frame.surface().command(&command);
        }
        for hit in model.hits(w, h) {
            frame.surface().hit(&hit);
        }
        // 输入法光标随文本区；有可聚焦输入区即收键盘；帧拍 4 Hz
        frame.set_fps(Some(4));
        frame.surface().policy(
            Some(FrameRect { x: 18.0, y: 238.0, w: 2.0, h: 18.0 }),
            true,
            Some(4),
        );
    }

    async fn on_frame_input(&self, ctx: &Context, params: FrameInputParams, _ui: &mut FrameUi) {
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
                "field" => self.apply(ctx, |m| m.focused = true).await,
                _ => {}
            },
            FrameInput::Wheel { delta_y, .. } => {
                self.apply(ctx, |m| m.offset += delta_y).await
            }
            FrameInput::Key { key, action: _, modifiers: _, text } => {
                let label = key_label(&key);
                let typed = text.unwrap_or_default();
                self.apply(ctx, |m| {
                    m.last_key = label;
                    if !typed.is_empty() {
                        m.typed.push_str(&typed);
                    }
                })
                .await
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
            FrameInput::Focus { focused } => self.apply(ctx, |m| m.focused = focused).await,
            _ => {}
        }
    }

    async fn on_frame_tick(&self, ctx: &Context, _params: FrameTickParams) {
        self.apply(ctx, |m| m.tick += 1).await;
    }
}

// 夹具以 cdylib 交付：宿主按 C ABI 符号建会话，与真实插件同一条通路
atelyx_plugin_sdk::export_in_process_plugin!(FrameStub {
    model: Arc::new(Mutex::new(Model::default()))
});
