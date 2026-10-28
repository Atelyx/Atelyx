//! 控件层：把按钮 / 输入框 / 下拉 / 勾选 / 滚动翻译成整面帧（绘制指令 + 命中区），并把宿主回发的
//! 原始输入解释成语义事件。用法是两阶段——先 [`FrameUi::event`] 拿事件、插件改自己的状态，
//! 再 [`FrameUi::render`] 声明并提交一帧；帧永远是「事件之后」的状态。
//!
//! 文字量算缺失时 `render` 内部批量补齐并重建（最多 [`MAX_MEASURE_PASSES`] 遍），插件不必感知。

mod build;
mod geom;
mod input;
mod metrics;
mod model;
mod text;
mod theme;
mod widgets;

use atelyx_plugin_protocol::envelope::RpcError;
use atelyx_plugin_protocol::frame::FrameInput;
use atelyx_plugin_protocol::method::MAX_MEASURE_ITEMS;

use crate::Context;

pub use build::Ui;
pub use widgets::TextRole;

/// 一次 `render` 内最多补几轮量算：缺项按估算先排，量算一到即重建。
const MAX_MEASURE_PASSES: usize = 3;

/// 原始输入解释出的语义事件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiEvent {
    /// 按钮 / 勾选框被点击（勾选框的勾选值由插件自己翻转）。
    Click { id: String },
    /// 输入框值变化：插件把新值写回自己的状态再重渲染。
    Text { id: String, value: String },
    /// 下拉选中一项（`index` 是声明时给出的选项下标）。
    Select { id: String, index: usize },
    /// 键盘焦点进入 / 离开某控件（输入框可据此提交草稿）。
    Focus { id: String, focused: bool },
}

/// 一个面的控件层状态与驱动：交互态、量算缓存与主题都随它跨帧保留。
pub struct FrameUi {
    state: model::UiState,
}

impl Default for FrameUi {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameUi {
    pub fn new() -> Self {
        Self { state: model::UiState::default() }
    }

    /// 面最近一次上报的逻辑尺寸；宿主首次绘制后才可信。
    pub fn size(&self) -> (f32, f32) {
        self.state.size
    }

    /// 解释一条宿主输入：翻成语义事件并把交互态（焦点 / 光标 / 选区 / 滚动）就地更新。
    /// `value_of` 必须在每次调用时读插件的当前值——编辑永远从事实出发，而不是从上一帧的绘制快照。
    /// 返回后插件应立即应用事件并 `render`，否则这次输入看不到结果。
    pub async fn event(
        &mut self,
        ctx: &Context,
        input: &FrameInput,
        value_of: impl Fn(&str) -> Option<String>,
    ) -> Vec<UiEvent> {
        input::handle(&mut self.state, ctx, input, value_of).await
    }

    /// 声明并提交本面的一帧：`build` 只声明控件，位置与尺寸由容器算。
    /// `build` 会被调用多次（量算补齐后重建），必须只读状态、不产生副作用。
    pub async fn render(
        &mut self,
        ctx: &Context,
        surface_id: &str,
        build: impl Fn(&mut Ui<'_, '_>),
    ) -> Result<(), RpcError> {
        self.ensure_theme(ctx).await;
        let theme = self.state.theme.clone().expect("主题已在 ensure_theme 中落地");
        let size = self.state.size;
        let area = geom::Rect::new(0.0, 0.0, size.0, size.1);
        let mut passes = 0usize;
        loop {
            let metrics = std::mem::take(&mut self.state.metrics);
            let out = {
                let mut ctxb = build::BuildCtx::new(&theme, metrics, &self.state);
                let items = {
                    let mut ui = build::Ui::new(&mut ctxb, area);
                    build(&mut ui);
                    ui.into_items()
                };
                build::layout_root(&mut ctxb, items, area);
                ctxb.finish()
            };
            self.state.metrics = out.metrics;
            self.state.layouts = out.layouts;
            self.state.focusables = out.focusables;
            self.state.scroll = out.scrolls;
            self.state.prune();
            if out.misses && passes < MAX_MEASURE_PASSES {
                let misses = self.state.metrics.take_misses();
                if let Err(err) = self.fill_metrics(ctx, misses).await {
                    eprintln!("量算失败，按估算字宽出这一帧：{err}");
                }
                passes += 1;
                continue;
            }
            out.frame
                .validate()
                .map_err(|err| RpcError::failed(format!("面 {surface_id} 的帧不合法：{err}")))?;
            ctx.submit_frame(surface_id, out.frame).await?;
            return Ok(());
        }
    }

    /// 主题只取一次：宿主主题变化没有通知通道，插件可在面重新挂载时重建 `FrameUi` 重取。
    async fn ensure_theme(&mut self, ctx: &Context) {
        if self.state.theme.is_some() {
            return;
        }
        self.state.theme = Some(match ctx.query_theme().await {
            Ok(result) => theme::Theme::from(result),
            Err(err) => {
                eprintln!("取宿主主题失败，按缺省样式绘制：{err}");
                theme::Theme::default()
            }
        });
    }

    async fn fill_metrics(
        &mut self,
        ctx: &Context,
        misses: Vec<atelyx_plugin_protocol::method::MeasureItem>,
    ) -> Result<(), RpcError> {
        for chunk in misses.chunks(MAX_MEASURE_ITEMS) {
            let result = ctx.measure_batch(chunk.to_vec()).await?;
            self.state.metrics.apply(chunk, &result.sizes);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use atelyx_plugin_protocol::canvas::DrawCommand;
    use atelyx_plugin_protocol::envelope::Envelope;
    use atelyx_plugin_protocol::frame::{FrameInput, FrameSubmitParams, SurfaceFrame};
    use atelyx_plugin_protocol::method::{
        self, FontSizes, MeasureBatchParams, MeasureTextResult, Radii, TextPayload, ThemeColors, ThemeResult,
    };
    use atelyx_plugin_protocol::view::{Key, KeyAction, Modifiers, MouseButton, NamedKey};
    use tokio::sync::mpsc;

    use super::*;

    /// 假宿主：应答主题 / 量算 / 提交与剪贴板，并记下请求顺序与已提交的帧。
    struct Bench {
        ctx: Context,
        rx: mpsc::Receiver<Envelope>,
        ui: Arc<tokio::sync::Mutex<FrameUi>>,
        values: Arc<Mutex<HashMap<String, String>>>,
        clipboard: Arc<Mutex<String>>,
        frames: Vec<SurfaceFrame>,
        requests: Vec<String>,
    }

    impl Bench {
        async fn new(width: f32, height: f32) -> Self {
            let (tx, rx) = mpsc::channel(64);
            let mut bench = Self {
                ctx: Context::new(tx),
                rx,
                ui: Arc::new(tokio::sync::Mutex::new(FrameUi::new())),
                values: Arc::default(),
                clipboard: Arc::default(),
                frames: Vec::new(),
                requests: Vec::new(),
            };
            // 面尺寸只在宿主首次绘制时上报，测试里直接喂一次 Resize
            bench.event(FrameInput::Resize { width, height }).await;
            bench
        }

        async fn event(&mut self, input: FrameInput) -> Vec<UiEvent> {
            let ui = self.ui.clone();
            let ctx = self.ctx.clone();
            let values = self.values.clone();
            let task = tokio::spawn(async move {
                let mut guard = ui.lock().await;
                guard
                    .event(&ctx, &input, move |id| values.lock().unwrap().get(id).cloned())
                    .await
            });
            self.drive(task).await
        }

        async fn render(
            &mut self,
            build: impl Fn(&mut build::Ui<'_, '_>) + Send + 'static,
        ) -> Result<SurfaceFrame, RpcError> {
            let ui = self.ui.clone();
            let ctx = self.ctx.clone();
            let task = tokio::spawn(async move {
                let mut guard = ui.lock().await;
                guard.render(&ctx, "panel", build).await
            });
            self.drive(task).await?;
            Ok(self.frames.last().expect("渲染成功即提交了帧").clone())
        }

        async fn click_and_collect(&mut self, id: &str) -> Vec<UiEvent> {
            let mut events = Vec::new();
            for input in click(id) {
                events.extend(self.event(input).await);
            }
            events
        }

        fn set_value(&self, id: &str, value: &str) {
            self.values.lock().unwrap().insert(id.to_string(), value.to_string());
        }

        async fn drive<T>(&mut self, task: tokio::task::JoinHandle<T>) -> T {
            loop {
                if task.is_finished() {
                    return task.await.expect("任务不应 panic");
                }
                tokio::select! {
                    envelope = self.rx.recv() => {
                        if let Some(envelope) = envelope {
                            self.reply(envelope);
                        }
                    }
                    _ = tokio::time::sleep(Duration::from_millis(2)) => {}
                }
            }
        }

        fn reply(&mut self, envelope: Envelope) {
            let Envelope::Request(request) = envelope else {
                panic!("插件只发请求");
            };
            self.requests.push(request.method.clone());
            let result = match request.method.as_str() {
                m if m == method::query::THEME => serde_json::to_value(theme_result()).unwrap(),
                m if m == method::query::MEASURE => {
                    let params: MeasureBatchParams = serde_json::from_value(request.params).unwrap();
                    let sizes: Vec<MeasureTextResult> = params
                        .items
                        .iter()
                        .map(|item| MeasureTextResult {
                            width: fake_width(&item.text, item.size),
                            height: item.size * 1.2,
                            lines: 1,
                        })
                        .collect();
                    serde_json::json!({ "sizes": sizes })
                }
                m if m == method::frame::SUBMIT => {
                    let params: FrameSubmitParams = serde_json::from_value(request.params).unwrap();
                    params.frame.validate().expect("SDK 出帧必须合法");
                    self.frames.push(params.frame);
                    serde_json::json!({ "applied": true })
                }
                m if m == method::clipboard::READ_TEXT => {
                    serde_json::to_value(TextPayload { text: self.clipboard.lock().unwrap().clone() }).unwrap()
                }
                m if m == method::clipboard::WRITE_TEXT => {
                    let params: TextPayload = serde_json::from_value(request.params).unwrap();
                    *self.clipboard.lock().unwrap() = params.text;
                    serde_json::json!({})
                }
                other => panic!("未预期的请求：{other}"),
            };
            let sender = self.ctx.remove_pending(request.id).expect("应有在途调用");
            let _ = sender.send(Ok(result));
        }
    }

    /// 量算替身：ASCII 半宽、其他宽度按 1 字宽（结果只用于断言相对位置）。
    fn fake_width(text: &str, size: f32) -> f32 {
        text.chars().map(|c| if c.is_ascii() { 0.5 } else { 1.0 }).sum::<f32>() * size
    }

    fn theme_result() -> ThemeResult {
        ThemeResult {
            colors: ThemeColors { accent: "#e0a94e".into(), ..ThemeColors::default() },
            font_sizes: FontSizes { micro: 11.0, caption: 12.0, ui: 13.0, body: 14.0, h2: 16.0, h1: 20.0 },
            radii: Radii { xs: 4.0, sm: 6.0, md: 10.0 },
        }
    }

    fn texts(frame: &SurfaceFrame) -> Vec<String> {
        frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { content, .. } => Some(content.clone()),
                _ => None,
            })
            .collect()
    }

    fn has_hit(frame: &SurfaceFrame, id: &str) -> bool {
        frame.hits.iter().any(|hit| hit.id == id)
    }

    fn key(name: NamedKey) -> FrameInput {
        FrameInput::Key { key: Key::Named { name }, action: KeyAction::Press, modifiers: Modifiers::default(), text: None }
    }

    fn typed(text: &str) -> FrameInput {
        FrameInput::Key {
            key: Key::Character { text: text.into() },
            action: KeyAction::Press,
            modifiers: Modifiers::default(),
            text: Some(text.into()),
        }
    }

    fn click(id: &str) -> Vec<FrameInput> {
        vec![
            FrameInput::PointerDown { x: 1.0, y: 1.0, button: MouseButton::Left, hit: Some(id.into()) },
            FrameInput::PointerUp { x: 1.0, y: 1.0, button: MouseButton::Left, hit: Some(id.into()) },
        ]
    }

    #[tokio::test]
    async fn render_declares_hits_text_and_keys() {
        let mut bench = Bench::new(320.0, 240.0).await;
        let frame = bench
            .render(|ui| {
                ui.column(8.0, |c| {
                    c.text("服务", TextRole::Heading);
                    c.text_input("host", "127.0.0.1", "127.0.0.1");
                    c.button("go", "启动");
                });
            })
            .await
            .unwrap();
        assert!(frame.keys, "有可聚焦控件即声明收键盘");
        assert!(has_hit(&frame, "host") && has_hit(&frame, "go"));
        assert!(texts(&frame).iter().any(|text| text == "服务"));
        assert!(texts(&frame).iter().any(|text| text == "启动"));
        assert!(frame.ime.is_none(), "没有聚焦输入框时不声明输入法光标");
    }

    #[tokio::test]
    async fn click_completes_only_when_released_over_the_same_control() {
        let mut bench = Bench::new(200.0, 120.0).await;
        bench.render(|ui| ui.button("go", "启动")).await.unwrap();

        let events = bench.click_and_collect("go").await;
        assert_eq!(events, vec![UiEvent::Click { id: "go".into() }]);

        bench.event(click("go")[0].clone()).await;
        assert!(bench.event(click("other")[1].clone()).await.is_empty(), "抬起不在按下控件上不算点击");
    }

    #[tokio::test]
    async fn typing_edits_the_value_and_the_next_frame_shows_it_with_a_caret() {
        let mut bench = Bench::new(240.0, 120.0).await;
        bench.render(|ui| ui.text_input("host", "", "127.0.0.1")).await.unwrap();

        let events = bench
            .event(FrameInput::PointerDown { x: 20.0, y: 10.0, button: MouseButton::Left, hit: Some("host".into()) })
            .await;
        assert!(events.iter().any(|event| matches!(event, UiEvent::Focus { id, focused: true } if id == "host")));

        let events = bench.event(typed("a")).await;
        assert_eq!(events, vec![UiEvent::Text { id: "host".into(), value: "a".into() }]);
        bench.set_value("host", "a");

        let frame = bench.render(|ui| ui.text_input("host", "a", "127.0.0.1")).await.unwrap();
        assert!(texts(&frame).iter().any(|text| text == "a"), "重渲染显示新值");
        assert!(frame.ime.is_some(), "聚焦输入框后声明输入法光标");

        let events = bench.event(key(NamedKey::Backspace)).await;
        assert_eq!(events, vec![UiEvent::Text { id: "host".into(), value: String::new() }]);
    }

    #[tokio::test]
    async fn tab_moves_focus_between_inputs() {
        let mut bench = Bench::new(240.0, 160.0).await;
        bench
            .render(|ui| {
                ui.column(8.0, |c| {
                    c.text_input("first", "a", "");
                    c.text_input("second", "b", "");
                });
            })
            .await
            .unwrap();
        bench
            .event(FrameInput::PointerDown { x: 20.0, y: 10.0, button: MouseButton::Left, hit: Some("first".into()) })
            .await;

        let events = bench.event(key(NamedKey::Tab)).await;
        assert_eq!(
            events,
            vec![
                UiEvent::Focus { id: "first".into(), focused: false },
                UiEvent::Focus { id: "second".into(), focused: true },
            ]
        );

        let shift = Modifiers { shift: true, ..Modifiers::default() };
        let events = bench
            .event(FrameInput::Key {
                key: Key::Named { name: NamedKey::Tab },
                action: KeyAction::Press,
                modifiers: shift,
                text: None,
            })
            .await;
        assert_eq!(
            events,
            vec![
                UiEvent::Focus { id: "second".into(), focused: false },
                UiEvent::Focus { id: "first".into(), focused: true },
            ]
        );
    }

    #[tokio::test]
    async fn ime_composition_is_drawn_as_preedit_and_only_commits_on_commit() {
        let mut bench = Bench::new(240.0, 120.0).await;
        bench.render(|ui| ui.text_input("host", "", "")).await.unwrap();
        bench
            .event(FrameInput::PointerDown { x: 20.0, y: 10.0, button: MouseButton::Left, hit: Some("host".into()) })
            .await;

        let events = bench
            .event(FrameInput::Text { text: "zhong".into(), composing: true, caret: Some(5), candidates: None })
            .await;
        assert!(events.is_empty(), "组合串不进入值");
        let frame = bench.render(|ui| ui.text_input("host", "", "")).await.unwrap();
        assert!(texts(&frame).iter().any(|text| text == "zhong"), "组合串画在输入框里");

        let events = bench
            .event(FrameInput::Text { text: "中".into(), composing: false, caret: None, candidates: None })
            .await;
        assert_eq!(events, vec![UiEvent::Text { id: "host".into(), value: "中".into() }]);
    }

    #[tokio::test]
    async fn select_expands_inline_and_reports_the_chosen_option() {
        let mut bench = Bench::new(240.0, 200.0).await;
        let options = vec!["hf".to_string(), "hf-mirror".to_string()];
        let first = options.clone();
        bench.render(move |ui| ui.select("mirror", first.clone(), 0)).await.unwrap();
        assert!(has_hit(&bench.frames.last().unwrap().clone(), "mirror"));

        let events = bench.click_and_collect("mirror").await;
        assert!(
            events.iter().all(|event| matches!(event, UiEvent::Focus { .. })),
            "展开由控件层自己处理，只上报聚焦：{events:?}"
        );

        let second = options.clone();
        let frame = bench.render(move |ui| ui.select("mirror", second.clone(), 0)).await.unwrap();
        assert!(has_hit(&frame, "mirror::option:1"), "展开后就地列出选项");

        let events = bench.click_and_collect("mirror::option:1").await;
        assert_eq!(events, vec![UiEvent::Select { id: "mirror".into(), index: 1 }]);
    }

    #[tokio::test]
    async fn checkbox_click_and_space_both_toggle() {
        let mut bench = Bench::new(240.0, 120.0).await;
        bench.render(|ui| ui.checkbox("auto", false, "自动启动")).await.unwrap();
        let events = bench.click_and_collect("auto").await;
        assert_eq!(
            events,
            vec![UiEvent::Focus { id: "auto".into(), focused: true }, UiEvent::Click { id: "auto".into() }]
        );

        let events = bench.event(key(NamedKey::Space)).await;
        assert_eq!(events, vec![UiEvent::Click { id: "auto".into() }], "聚焦后空格也能切换");
    }

    #[tokio::test]
    async fn wheel_scrolls_by_content_block_boundaries() {
        let mut bench = Bench::new(200.0, 120.0).await;
        let build = |ui: &mut build::Ui<'_, '_>| {
            ui.scroll("list", 4.0, |s| {
                for index in 0..6 {
                    s.button(format!("row{index}"), format!("行{index}"));
                }
            });
        };
        let frame = bench.render(build).await.unwrap();
        assert!(has_hit(&frame, "row0"), "起始位置第一行完整可见");

        bench
            .event(FrameInput::Wheel { x: 100.0, y: 60.0, delta_x: 0.0, delta_y: 2.0, hit: None })
            .await;
        let frame = bench.render(build).await.unwrap();
        assert!(!has_hit(&frame, "row0"), "滚出视口的整块不再可命中");
        assert!(has_hit(&frame, "row1"));
    }

    #[tokio::test]
    async fn measurement_misses_are_batched_once_and_then_cached() {
        let mut bench = Bench::new(200.0, 160.0).await;
        bench.render(|ui| ui.text("中文说明", TextRole::Label)).await.unwrap();
        assert_eq!(
            bench.requests,
            vec![method::query::THEME.to_string(), method::query::MEASURE.to_string(), method::frame::SUBMIT.to_string()],
            "缺项先量算一轮再提交"
        );
        bench.render(|ui| ui.text("中文说明", TextRole::Label)).await.unwrap();
        assert_eq!(bench.requests.last().map(String::as_str), Some(method::frame::SUBMIT), "同文字不再量算");
    }

    #[tokio::test]
    async fn row_gives_remaining_width_to_the_filling_child() {
        let mut bench = Bench::new(200.0, 120.0).await;
        let frame = bench
            .render(|ui| {
                ui.row(8.0, |r| {
                    r.text("很长的模型路径", TextRole::Label);
                    r.ghost_button("pick", "选择…");
                });
            })
            .await
            .unwrap();
        let button = frame.hits.iter().find(|hit| hit.id == "pick").expect("按钮应有命中区");
        assert!(button.x > 100.0, "收缩钮被推到行尾：{button:?}");
        assert!((button.x + button.w - 200.0).abs() < 1.0, "按钮贴右缘：{button:?}");
    }

    #[tokio::test]
    async fn clipboard_paste_and_select_all_clear_edit_the_value() {
        let mut bench = Bench::new(240.0, 120.0).await;
        bench.render(|ui| ui.text_input("host", "abc", "")).await.unwrap();
        bench.set_value("host", "abc");
        bench
            .event(FrameInput::PointerDown { x: 20.0, y: 10.0, button: MouseButton::Left, hit: Some("host".into()) })
            .await;

        // Ctrl+A 全选后 Backspace 清空
        let control = Modifiers { control: true, ..Modifiers::default() };
        bench
            .event(FrameInput::Key {
                key: Key::Character { text: "a".into() },
                action: KeyAction::Press,
                modifiers: control,
                text: None,
            })
            .await;
        let events = bench.event(key(NamedKey::Backspace)).await;
        assert_eq!(events, vec![UiEvent::Text { id: "host".into(), value: String::new() }]);
        bench.set_value("host", "");

        // Ctrl+V 粘贴：值从剪贴板进入
        *bench.clipboard.lock().unwrap() = "粘贴".into();
        let events = bench
            .event(FrameInput::Key {
                key: Key::Character { text: "v".into() },
                action: KeyAction::Press,
                modifiers: control,
                text: None,
            })
            .await;
        assert_eq!(events, vec![UiEvent::Text { id: "host".into(), value: "粘贴".into() }]);
    }

    #[tokio::test]
    async fn duplicate_control_ids_are_rejected_before_submitting() {
        let mut bench = Bench::new(200.0, 120.0).await;
        let error = bench
            .render(|ui| {
                ui.button("same", "一");
                ui.button("same", "二");
            })
            .await
            .unwrap_err();
        assert!(error.message.contains("重复"), "{}", error.message);
        assert!(bench.frames.is_empty(), "非法帧不提交");
    }
}
