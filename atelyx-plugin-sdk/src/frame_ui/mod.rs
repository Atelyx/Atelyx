//! 控件层：把按钮 / 输入框 / 下拉 / 勾选 / 滚动翻译成整面帧（绘制指令 + 命中区），并把宿主回发的
//! 原始输入解释成语义事件。用法是两阶段——先 [`FrameUi::event`] 拿事件、插件改自己的状态，
//! 再 [`FrameUi::render`] 声明并提交一帧；帧永远是「事件之后」的状态。
//!
//! 文字量算与 Markdown 块缺失时 `render` 内部补齐并重建（最多 [`MAX_REBUILD_PASSES`] 遍），插件不必感知。

mod build;
mod geom;
mod icons;
mod input;
mod markdown;
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
pub use build::Canvas;
pub use geom::Rect;
pub use icons::Icon;
pub use widgets::{TextRole, TextStyle};

/// 一次 `render` 内最多补几轮缺项（文字量算 / Markdown 块）：补齐后按最新数据重建。
const MAX_REBUILD_PASSES: usize = 3;

/// Markdown 解析缓存条数上限：超出即按「本帧用到」收敛，避免长期运行的插件按历史内容累积。
const MAX_MARKDOWN_CACHE: usize = 8;

fn check_frame(
    surface_id: &str,
    frame: &atelyx_plugin_protocol::frame::SurfaceFrame,
) -> Result<(), RpcError> {
    frame
        .validate()
        .map_err(|err| RpcError::failed(format!("面 {surface_id} 的帧不合法：{err}")))
}

/// 原始输入解释出的语义事件。
#[derive(Clone, Debug, PartialEq)]
pub enum UiEvent {
    /// 按钮 / 勾选框 / 卡片被点击（勾选框的勾选值由插件自己翻转）。
    Click { id: String },
    /// 双击落在某控件上（在此之前已按一次按下处理：聚焦输入框并全选）。随后的抬起仍会另出 `Click`。
    DoubleClick { id: String },
    /// 右键落在某控件上（不改焦点；插件可据此展开行内菜单或走快捷动作）。
    RightClick { id: String },
    /// 输入框值变化：插件把新值写回自己的状态再重渲染。
    Text { id: String, value: String },
    /// 输入框回车（提交语义由插件定，例如检索或写回设置）。
    Submit { id: String },
    /// 下拉选中一项（`index` 是声明时给出的选项下标）。
    Select { id: String, index: usize },
    /// 键盘焦点进入 / 离开某控件（输入框可据此提交草稿）。
    Focus { id: String, focused: bool },
    /// 滚动容器偏移变化：`offset` 为当前偏移，`max_offset` 为可滚动余量（滚到底翻页这类判断用）。
    Scroll { id: String, offset: f32, max_offset: f32 },
}

/// 一个面的控件层状态与驱动：交互态、量算缓存与主题都随它跨帧保留。
pub struct FrameUi {
    state: model::UiState,
    fps: Option<u16>,
}

impl Default for FrameUi {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameUi {
    pub fn new() -> Self {
        Self { state: model::UiState::default(), fps: None }
    }

    /// 面最近一次上报的逻辑尺寸；宿主首次绘制后才可信。
    pub fn size(&self) -> (f32, f32) {
        self.state.size
    }

    /// 声明宿主帧拍频率（1–120）：声明后宿主按该节拍回发 `host:frame.tick`，
    /// 插件的播放 / 动画在帧拍里推进并重渲染；`None` = 不推帧拍。
    pub fn set_fps(&mut self, fps: Option<u16>) {
        self.fps = fps;
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
            let mut out = {
                let mut ctxb = build::BuildCtx::new(&theme, metrics, &self.state);
                let items = {
                    let mut ui = build::Ui::new(&mut ctxb, area);
                    build(&mut ui);
                    ui.into_items()
                };
                build::layout_root(&mut ctxb, items, area);
                ctxb.finish()
            };
            out.frame.fps = self.fps;
            self.state.metrics = out.metrics;
            self.state.layouts = out.layouts;
            self.state.focusables = out.focusables;
            self.state.scroll = out.scrolls;
            self.state.scroll_max = out.scroll_max;
            self.state.follow_prev = out.follow_prev;
            // Markdown 缓存按条数上限收敛：滚出视口的内容不再算「本帧用到」，不能被立即剪掉
            if self.state.markdown.len() > MAX_MARKDOWN_CACHE {
                self.state.markdown.retain(|content, _| out.markdown_used.contains(content));
            }
            self.state.prune();
            // 尺寸刚变且本帧还有缺项：先按估算字宽交一帧（拖动跟手），量算补齐后再交精确帧
            if std::mem::take(&mut self.state.resize_pending)
                && (out.misses || !out.markdown_misses.is_empty())
            {
                check_frame(surface_id, &out.frame)?;
                self.submit_if_changed(ctx, surface_id, out.frame.clone())
                    .await?;
            }
            if !out.markdown_misses.is_empty() {
                for content in out.markdown_misses {
                    match ctx.markdown(&content).await {
                        Ok(result) => {
                            self.state.markdown.insert(content, result.blocks);
                        }
                        Err(err) => {
                            // 失败也落缓存（空块）：否则每帧都把同一内容当缺项重试，白耗往返
                            eprintln!("Markdown 解析失败，按空块出这一帧：{err}");
                            self.state.markdown.insert(content, Vec::new());
                        }
                    }
                }
                if passes < MAX_REBUILD_PASSES {
                    passes += 1;
                    continue;
                }
            }
            if out.misses && passes < MAX_REBUILD_PASSES {
                let misses = self.state.metrics.take_misses();
                if let Err(err) = self.fill_metrics(ctx, misses).await {
                    eprintln!("量算失败，按估算字宽出这一帧：{err}");
                }
                passes += 1;
                continue;
            }
            check_frame(surface_id, &out.frame)?;
            self.submit_if_changed(ctx, surface_id, out.frame).await?;
            return Ok(());
        }
    }

    /// 同内容帧不再提交：宿主每收一帧都要重投影并整窗重绘，指针移动这类「没改画面」的输入
    /// 会按设备上报频率把宿主拖到重绘风暴里。
    ///
    /// 只有宿主确认 `applied` 才记为已提交：错位被丢的帧不能挡住后续同内容提交。
    async fn submit_if_changed(
        &mut self,
        ctx: &Context,
        surface_id: &str,
        frame: atelyx_plugin_protocol::frame::SurfaceFrame,
    ) -> Result<(), RpcError> {
        let same = self
            .state
            .submitted
            .as_ref()
            .is_some_and(|(id, submitted)| id == surface_id && submitted == &frame);
        if same {
            return Ok(());
        }
        let result = ctx.submit_frame(surface_id, frame.clone()).await?;
        if result.applied {
            self.state.submitted = Some((surface_id.to_string(), frame));
        }
        Ok(())
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

    use atelyx_plugin_protocol::paint::{ColorToken, DrawCommand, ImageFit, ImageSource, PaintColor};
    use atelyx_plugin_protocol::envelope::Envelope;
    use atelyx_plugin_protocol::frame::{
        FrameRegionRequest, FrameSubmitSharedParams, FrameInput, HitCursor, SurfaceFrame,
    };
    use atelyx_plugin_protocol::frame_codec;
    use atelyx_plugin_protocol::frame_region::{self, FrameRegionInfo, SLOT_CAPACITY};
    use memmap2::{Mmap, MmapOptions};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use atelyx_plugin_protocol::method::{
        self, FontSizes, MeasureBatchParams, MeasureTextResult, Radii, TextPayload, ThemeColors, ThemeResult,
    };
    use atelyx_plugin_protocol::frame::{Key, KeyAction, Modifiers, MouseButton, NamedKey};
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
        region: FrameRegionInfo,
        region_map: Option<Mmap>,
        region_paths: Vec<PathBuf>,
    }

    /// 测试用的共享内存区域：真文件 + 真映射，读写路径与宿主一致。
    fn test_region(capacity: u32, generation: u64) -> (FrameRegionInfo, Mmap, PathBuf) {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "atelyx-sdk-test-{}-{}.bin",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let info = FrameRegionInfo {
            path: path.to_string_lossy().into_owned(),
            generation,
            ..FrameRegionInfo::new_layout(capacity)
        };
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .expect("创建测试区域文件失败");
        file.set_len(info.byte_len() as u64).expect("设置测试区域长度失败");
        let mut map = unsafe { MmapOptions::new().len(info.byte_len()).map_mut(&file).expect("映射测试区域失败") };
        frame_region::init_region(&mut map, &info).expect("初始化测试区域头失败");
        map.flush().expect("刷写测试区域头失败");
        drop(map);
        let map = unsafe { MmapOptions::new().len(info.byte_len()).map(&file).expect("映射测试区域失败") };
        (info, map, path)
    }

    impl Drop for Bench {
        fn drop(&mut self) {
            // 先释放测试侧映射再删文件；删不掉由系统清理临时目录
            self.region_map.take();
            for path in &self.region_paths {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    impl Bench {
        async fn new(width: f32, height: f32) -> Self {
            let (tx, rx) = mpsc::channel(64);
            let (region, region_map, path) = test_region(SLOT_CAPACITY, 1);
            let ctx = Context::new(tx);
            ctx.attach_frame_region(region.clone()).expect("测试区域应可接上");
            let mut bench = Self {
                ctx,
                rx,
                ui: Arc::new(tokio::sync::Mutex::new(FrameUi::new())),
                values: Arc::default(),
                clipboard: Arc::default(),
                frames: Vec::new(),
                requests: Vec::new(),
                region,
                region_map: Some(region_map),
                region_paths: vec![path],
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
                m if m == method::markdown::PARSE => {
                    let params: atelyx_plugin_protocol::method::MarkdownParams =
                        serde_json::from_value(request.params).unwrap();
                    assert!(!params.content.is_empty());
                    serde_json::json!({
                        "blocks": [{
                            "kind": "paragraph",
                            "depth": 0,
                            "spans": [{ "text": "正文" }]
                        }]
                    })
                }
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
                m if m == method::frame::SUBMIT_SHARED => {
                    let params: FrameSubmitSharedParams = serde_json::from_value(request.params).unwrap();
                    assert_eq!(params.generation, self.region.generation, "提交须带当前代次");
                    let bytes = frame_region::read_slot(
                        self.region_map.as_ref().unwrap(),
                        &self.region,
                        params.slot,
                        params.seq,
                        params.len,
                    )
                    .expect("槽位与长度应合法")
                    .expect("提交已返回，槽内内容须完整");
                    let frame = frame_codec::decode(&bytes).expect("槽内应是合法帧编码");
                    frame.validate().expect("SDK 出帧必须合法");
                    self.frames.push(frame);
                    serde_json::json!({ "applied": true })
                }
                m if m == method::frame::REGION => {
                    let req: FrameRegionRequest = serde_json::from_value(request.params).unwrap();
                    let mut capacity = self.region.slot_capacity;
                    while capacity < req.min_bytes {
                        capacity = capacity.saturating_mul(2);
                    }
                    let (info, map, path) = test_region(capacity, self.region.generation + 1);
                    self.region_paths.push(path);
                    self.region = info.clone();
                    self.region_map = Some(map);
                    serde_json::to_value(info).unwrap()
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
    async fn text_area_breaks_on_enter_and_moves_by_rows() {
        let mut bench = Bench::new(120.0, 200.0).await;
        let frame = bench.render(|ui| ui.text_area("body", "line1\nline2", "写点什么", 60.0)).await.unwrap();
        assert_eq!(texts(&frame), vec!["line1".to_string(), "line2".to_string()], "显式换行各占一行");
        assert!(has_hit(&frame, "body"));

        let events = bench
            .event(FrameInput::PointerDown { x: 100.0, y: 30.0, button: MouseButton::Left, hit: Some("body".into()) })
            .await;
        assert!(events.iter().any(|event| matches!(event, UiEvent::Focus { id, focused: true } if id == "body")));

        // 多行的回车是插入换行，不是单行的「提交」
        let events = bench.event(key(NamedKey::Enter)).await;
        assert_eq!(events, vec![UiEvent::Text { id: "body".into(), value: "line1\nline2\n".into() }]);

        bench.set_value("body", "line1\nline2\n");
        let frame = bench.render(|ui| ui.text_area("body", "line1\nline2\n", "", 60.0)).await.unwrap();
        assert!(frame.ime.is_some(), "聚焦的多行框声明输入法光标");

        // 光标在末尾空行：上键回到上一行的同一横向位置
        let events = bench.event(key(NamedKey::ArrowUp)).await;
        assert!(events.is_empty(), "上下键只挪光标");
        let events = bench.event(typed("X")).await;
        assert_eq!(events, vec![UiEvent::Text { id: "body".into(), value: "line1\nXline2\n".into() }]);
    }

    #[tokio::test]
    async fn submit_text_area_sends_on_enter_and_breaks_on_shift_enter() {
        let mut bench = Bench::new(200.0, 160.0).await;
        bench.render(|ui| ui.text_area_submit("draft", "hi", "输入问题", 48.0)).await.unwrap();
        bench
            .event(FrameInput::PointerDown { x: 190.0, y: 20.0, button: MouseButton::Left, hit: Some("draft".into()) })
            .await;

        let shift = Modifiers { shift: true, ..Modifiers::default() };
        let events = bench
            .event(FrameInput::Key {
                key: Key::Named { name: NamedKey::Enter },
                action: KeyAction::Press,
                modifiers: shift,
                text: None,
            })
            .await;
        assert_eq!(events, vec![UiEvent::Text { id: "draft".into(), value: "hi\n".into() }], "Shift+回车换行");

        let events = bench.event(key(NamedKey::Enter)).await;
        assert_eq!(events, vec![UiEvent::Submit { id: "draft".into() }], "回车提交不插换行");
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
    async fn wheel_scrolls_content_out_of_the_viewport() {
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
            .event(FrameInput::Wheel { x: 100.0, y: 60.0, delta_x: 0.0, delta_y: 100.0, hit: None })
            .await;
        let frame = bench.render(build).await.unwrap();
        assert!(!has_hit(&frame, "row0"), "滚出视口的行不再可命中");
        assert!(has_hit(&frame, "row5"), "滚到底后最后一行可见");
    }

    #[tokio::test]
    async fn resize_submits_an_estimate_then_a_refined_frame() {
        let mut bench = Bench::new(200.0, 160.0).await;
        let build = |ui: &mut build::Ui<'_, '_>| {
            ui.row(0.0, |r| {
                r.chip("abcdef", ColorToken::Accent);
                r.chip("尾巴", ColorToken::Accent);
            });
        };
        bench.render(build).await.unwrap();
        assert_eq!(bench.frames.len(), 2, "先按估算交一版，量算补齐后再交精确帧");
        assert_ne!(bench.frames[0], bench.frames[1], "精确帧与估算帧不同");
        assert_eq!(
            bench.requests,
            vec![
                method::query::THEME.to_string(),
                method::frame::SUBMIT_SHARED.to_string(),
                method::query::MEASURE.to_string(),
                method::frame::SUBMIT_SHARED.to_string(),
            ]
        );
    }

    /// 帧超过槽容量时走 `frame.region` 换新区域：扩容 → 重新映射 → 按新代次写槽提交。
    #[tokio::test]
    async fn oversized_frame_grows_the_region_and_rewrites() {
        let mut bench = Bench::new(200.0, 160.0).await;
        let commands = (0..5000)
            .map(|index| DrawCommand::Text {
                x: 0.0,
                y: index as f32,
                content: format!("{index:0>64}"),
                size: 12.0,
                color: PaintColor::Token { name: ColorToken::Text },
                weight: None,
                family: None,
                max_width: None,
                align: None,
            })
            .collect();
        let frame = SurfaceFrame { commands, ..SurfaceFrame::default() };
        let ctx = bench.ctx.clone();
        let task = tokio::spawn(async move { ctx.submit_frame("panel", frame).await });
        let result = bench.drive(task).await.unwrap();
        assert!(result.applied, "扩容后应正常受理");
        assert!(bench.region.slot_capacity > SLOT_CAPACITY, "槽容量应已翻倍");
        assert_eq!(bench.region.generation, 2, "扩容换代");
        assert_eq!(bench.frames.len(), 1);
        assert_eq!(bench.region_paths.len(), 2, "旧区域文件记入待删清单");
    }

    #[tokio::test]
    async fn measurement_misses_are_batched_once_and_then_cached() {
        let mut bench = Bench::new(200.0, 160.0).await;
        bench.render(|ui| ui.text("中文说明", TextRole::Label)).await.unwrap();
        assert_eq!(
            bench.requests,
            vec![
                method::query::THEME.to_string(),
                method::frame::SUBMIT_SHARED.to_string(),
                method::query::MEASURE.to_string(),
            ],
            "估算帧先交，量算补齐后画面不变就不再交"
        );
        let measures = bench.requests.iter().filter(|m| **m == method::query::MEASURE).count();
        bench.render(|ui| ui.text("中文说明", TextRole::Label)).await.unwrap();
        assert_eq!(
            bench.requests.iter().filter(|m| **m == method::query::MEASURE).count(),
            measures,
            "同文字不再量算"
        );
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
    async fn wheel_at_the_followed_end_keeps_the_bottom() {
        let mut bench = Bench::new(200.0, 120.0).await;
        let build = |ui: &mut build::Ui<'_, '_>| {
            ui.scroll_sized("logs", 60.0, 4.0, true, |s| {
                for index in 0..4 {
                    s.button(format!("row{index}"), format!("行{index}"));
                }
            });
        };
        bench.render(build).await.unwrap();
        let events = bench
            .event(FrameInput::Wheel { x: 100.0, y: 60.0, delta_x: 0.0, delta_y: 2.0, hit: None })
            .await;
        assert!(events.is_empty(), "已在终点时继续向下滚不产生偏移变化：{events:?}");
        let frame = bench.render(build).await.unwrap();
        assert!(has_hit(&frame, "row3"), "贴底不被块吸附拉回");
    }

    #[tokio::test]
    async fn right_click_reports_the_hit_without_taking_focus() {
        let mut bench = Bench::new(240.0, 120.0).await;
        bench.render(|ui| ui.text_input("host", "abc", "")).await.unwrap();
        let events = bench
            .event(FrameInput::PointerDown {
                x: 20.0,
                y: 10.0,
                button: MouseButton::Right,
                hit: Some("host".into()),
            })
            .await;
        assert_eq!(events, vec![UiEvent::RightClick { id: "host".into() }]);
        let events = bench.event(key(NamedKey::Backspace)).await;
        assert!(events.is_empty(), "右键不取焦点");
    }

    #[tokio::test]
    async fn password_input_draws_masks_instead_of_the_value() {
        let mut bench = Bench::new(240.0, 120.0).await;
        let frame = bench.render(|ui| ui.text_input_password("secret", "hunter2", "")).await.unwrap();
        let painted = texts(&frame);
        assert!(!painted.iter().any(|text| text == "hunter2"), "值不得出现在绘制指令里");
        assert!(painted.iter().any(|text| text.contains('•')));
    }

    #[tokio::test]
    async fn card_lets_inner_controls_take_the_hit() {
        let mut bench = Bench::new(240.0, 160.0).await;
        let frame = bench
            .render(|ui| {
                ui.card("row", false, |body| body.icon_button("inner", Icon::Copy));
            })
            .await
            .unwrap();
        let top = frame.hits.last().expect("应有命中区");
        assert_eq!(top.id, "inner", "卡片命中先登记，子控件压在上面");
    }

    #[tokio::test]
    async fn scrollbar_drag_moves_the_offset() {
        let mut bench = Bench::new(200.0, 120.0).await;
        let build = |ui: &mut build::Ui<'_, '_>| {
            ui.scroll("list", 4.0, |s| {
                for index in 0..8 {
                    s.button(format!("row{index}"), format!("行{index}"));
                }
            });
        };
        let frame = bench.render(build).await.unwrap();
        assert!(frame.hits.iter().any(|hit| hit.id == "list::bar"), "内容超一屏时给滚动条命中区");

        let events = bench
            .event(FrameInput::PointerDown {
                x: 198.0,
                y: 60.0,
                button: MouseButton::Left,
                hit: Some("list::bar".into()),
            })
            .await;
        let UiEvent::Scroll { offset, max_offset, .. } = &events[0] else {
            panic!("按下滚动条应回滚动位置：{events:?}");
        };
        assert!(*offset > 0.0 && *offset < *max_offset, "落在轨道中部：{offset}/{max_offset}");

        let events = bench
            .event(FrameInput::PointerMove { x: 198.0, y: 400.0, hit: None })
            .await;
        let UiEvent::Scroll { offset, max_offset, .. } = &events[0] else {
            panic!("拖动滚动条应回滚动位置：{events:?}");
        };
        assert!((*offset - *max_offset).abs() < 0.5, "拖到底即最大偏移：{offset}/{max_offset}");

        bench
            .event(FrameInput::PointerUp { x: 198.0, y: 400.0, button: MouseButton::Left, hit: None })
            .await;
        let events = bench
            .event(FrameInput::PointerMove { x: 198.0, y: 60.0, hit: None })
            .await;
        assert!(events.is_empty(), "抬起后不再按拖动处理");
    }

    #[tokio::test]
    async fn wheel_reaches_panes_and_a_fixed_log_area() {
        let mut bench = Bench::new(400.0, 600.0).await;
        let build = |ui: &mut build::Ui<'_, '_>| {
            ui.column(8.0, |c| {
                c.pad(12.0, 10.0, |p| p.row(8.0, |r| r.button("start", "启动")));
                c.row_fill(10.0, |row| {
                    row.scroll("left", 6.0, |s| {
                        for index in 0..10 {
                            s.button(format!("l{index}"), "模型");
                        }
                    });
                    row.scroll("right", 8.0, |s| {
                        for index in 0..30 {
                            s.button(format!("r{index}"), "参数");
                        }
                    });
                });
                c.scroll_sized("logs", 220.0, 2.0, true, |s| {
                    for index in 0..50 {
                        s.text(format!("log{index}"), TextRole::Caption);
                    }
                });
            });
        };
        bench.render(build).await.unwrap();

        // 左栏（x 在面板左半）
        let events = bench
            .event(FrameInput::Wheel { x: 100.0, y: 300.0, delta_x: 0.0, delta_y: 40.0, hit: None })
            .await;
        assert!(
            matches!(events.first(), Some(UiEvent::Scroll { id, .. }) if id == "left"),
            "左栏滚动应命中：{events:?}"
        );

        // 右侧参数栏
        let events = bench
            .event(FrameInput::Wheel { x: 300.0, y: 300.0, delta_x: 0.0, delta_y: 40.0, hit: None })
            .await;
        assert!(
            matches!(events.first(), Some(UiEvent::Scroll { id, .. }) if id == "right"),
            "右栏滚动应命中：{events:?}"
        );

        // 底部日志区（固定高度，贴底）
        let events = bench
            .event(FrameInput::Wheel { x: 200.0, y: 580.0, delta_x: 0.0, delta_y: -40.0, hit: None })
            .await;
        assert!(
            matches!(events.first(), Some(UiEvent::Scroll { id, .. }) if id == "logs"),
            "日志区滚动应命中：{events:?}"
        );
    }

    /// 没改画面内容的重复出帧不该再提交：宿主每收一帧都要重投影并整窗重绘，
    /// 指针移动这类高频输入会把宿主拖进重绘风暴。
    #[tokio::test]
    async fn identical_frames_are_not_resubmitted() {
        let mut bench = Bench::new(200.0, 120.0).await;
        let build = |ui: &mut build::Ui<'_, '_>| ui.button("go", "启动");
        bench.render(build).await.unwrap();
        let submitted = bench.frames.len();
        bench.render(build).await.unwrap();
        assert_eq!(bench.frames.len(), submitted, "同内容帧不得重复提交");

        bench.render(|ui| ui.button("go", "停止")).await.unwrap();
        assert_eq!(bench.frames.len(), submitted + 1, "内容变了仍要提交");

        // 重挂后宿主手里未必还有上一帧：尺寸输入到达即让去重失效
        bench.event(FrameInput::Resize { width: 200.0, height: 120.0 }).await;
        bench.render(|ui| ui.button("go", "停止")).await.unwrap();
        assert_eq!(bench.frames.len(), submitted + 2, "重挂后同内容帧不得被吞");
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

    #[tokio::test]
    async fn enter_submits_the_focused_input() {
        let mut bench = Bench::new(240.0, 120.0).await;
        bench.render(|ui| ui.text_input("q", "模型", "")).await.unwrap();
        bench.set_value("q", "模型");
        bench
            .event(FrameInput::PointerDown { x: 20.0, y: 10.0, button: MouseButton::Left, hit: Some("q".into()) })
            .await;
        let events = bench.event(key(NamedKey::Enter)).await;
        assert_eq!(events, vec![UiEvent::Submit { id: "q".into() }], "回车只报提交，不改值");
    }

    #[tokio::test]
    async fn wheel_reports_the_scroll_offset_of_the_container() {
        let mut bench = Bench::new(200.0, 120.0).await;
        let build = |ui: &mut build::Ui<'_, '_>| {
            ui.scroll("list", 4.0, |s| {
                for index in 0..6 {
                    s.button(format!("row{index}"), format!("行{index}"));
                }
            });
        };
        bench.render(build).await.unwrap();
        let events = bench
            .event(FrameInput::Wheel { x: 100.0, y: 60.0, delta_x: 0.0, delta_y: 80.0, hit: None })
            .await;
        let UiEvent::Scroll { id, offset, max_offset } = &events[0] else {
            panic!("滚轮应上报滚动位置：{events:?}");
        };
        assert_eq!(id, "list");
        assert!(*offset > 0.0 && *max_offset > 0.0);
    }

    /// 大块列表（卡片高过一格滚轮）也必须跟着滚轮走：偏移按像素累加，不吸附到块边界。
    #[tokio::test]
    async fn wheel_moves_a_list_of_tall_cards() {
        let mut bench = Bench::new(240.0, 200.0).await;
        let build = |ui: &mut build::Ui<'_, '_>| {
            ui.scroll("cards", 10.0, |s| {
                for index in 0..8 {
                    s.card(format!("c{index}"), false, |card| {
                        card.text(format!("模型 {index}"), TextRole::Label);
                    });
                }
            });
        };
        bench.render(build).await.unwrap();
        let events = bench
            .event(FrameInput::Wheel { x: 120.0, y: 100.0, delta_x: 0.0, delta_y: 40.0, hit: None })
            .await;
        let Some(UiEvent::Scroll { offset, max_offset, .. }) = events.first() else {
            panic!("一格滚轮就要动：{events:?}");
        };
        assert!((*offset - 40.0).abs() < 0.5, "偏移按像素累加，实际 {offset}");
        assert!(*max_offset > *offset);
    }

    #[tokio::test]
    async fn sized_scroll_follows_the_end_when_new_rows_arrive() {
        let mut bench = Bench::new(200.0, 120.0).await;
        let build = |ui: &mut build::Ui<'_, '_>| {
            ui.scroll_sized("logs", 60.0, 4.0, true, |s| {
                for index in 0..4 {
                    s.button(format!("row{index}"), format!("行{index}"));
                }
            });
        };
        let frame = bench.render(build).await.unwrap();
        assert!(!has_hit(&frame, "row0"), "贴底跟随首帧即停在终点");
        assert!(has_hit(&frame, "row3"), "最新一行可见");
    }

    #[tokio::test]
    async fn markdown_is_fetched_once_and_painted_in_the_frame() {
        let mut bench = Bench::new(240.0, 160.0).await;
        let frame = bench.render(|ui| ui.markdown("# 标题")).await.unwrap();
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::Text { content, .. } if content == "正文"
        )));
        bench.render(|ui| ui.markdown("# 标题")).await.unwrap();
        assert_eq!(
            bench.requests.iter().filter(|method| method.as_str() == method::markdown::PARSE).count(),
            1,
            "同内容只取一次"
        );
    }

    #[tokio::test]
    async fn fill_row_gives_a_sized_scroll_the_whole_row_height() {
        let mut bench = Bench::new(400.0, 200.0).await;
        let frame = bench
            .render(|ui| {
                ui.row_fill(8.0, |row| {
                    row.scroll("left", 4.0, |s| {
                        for index in 0..8 {
                            s.button(format!("l{index}"), "左栏");
                        }
                    });
                    row.scroll("right", 4.0, |s| s.button("r0", "右栏"));
                });
            })
            .await
            .unwrap();
        let left = frame.hits.iter().find(|hit| hit.id == "l0").expect("左栏应有可见行");
        let right = frame.hits.iter().find(|hit| hit.id == "r0").expect("右栏应有可见行");
        assert!(left.x < 5.0 && right.x > 190.0, "两栏各占一半宽度：{left:?} / {right:?}");
        assert!(left.y < 100.0, "滚动区拿到整行高度，内容从顶部落位：{left:?}");
    }

    #[tokio::test]
    async fn badge_progress_and_icon_button_declare_their_hits() {
        let mut bench = Bench::new(240.0, 160.0).await;
        let frame = bench
            .render(|ui| {
                ui.column(6.0, |c| {
                    c.chip("3 个", atelyx_plugin_protocol::paint::ColorToken::Accent);
                    c.progress(0.5);
                    c.icon_button("copy", Icon::Copy);
                });
            })
            .await
            .unwrap();
        assert!(has_hit(&frame, "copy"));
        assert!(texts(&frame).iter().any(|text| text == "3 个"));
    }

    #[tokio::test]
    async fn double_click_selects_the_input_and_reports_its_own_event() {
        let mut bench = Bench::new(240.0, 120.0).await;
        bench.render(|ui| ui.text_input("host", "abc", "")).await.unwrap();
        bench.set_value("host", "abc");

        let events = bench
            .event(FrameInput::DoubleClick { x: 20.0, y: 10.0, button: MouseButton::Left, hit: Some("host".into()) })
            .await;
        assert!(events.iter().any(|event| matches!(event, UiEvent::Focus { id, focused: true } if id == "host")));
        assert_eq!(events.last(), Some(&UiEvent::DoubleClick { id: "host".into() }));

        let events = bench.event(key(NamedKey::Backspace)).await;
        assert_eq!(events, vec![UiEvent::Text { id: "host".into(), value: String::new() }], "双击选中整个值");

        let events = bench
            .event(FrameInput::DoubleClick { x: 20.0, y: 10.0, button: MouseButton::Right, hit: Some("host".into()) })
            .await;
        assert_eq!(events, vec![UiEvent::RightClick { id: "host".into() }], "右键双击不按左键双击处理");
    }

    #[tokio::test]
    async fn canvas_draws_text_and_routes_sub_area_clicks_and_hover() {
        let mut bench = Bench::new(200.0, 120.0).await;
        let build = |ui: &mut build::Ui<'_, '_>| {
            ui.canvas(60.0, |c| {
                let area = c.area();
                let fill = if c.hovered("cell") { ColorToken::Accent } else { ColorToken::Surface };
                c.draw(DrawCommand::Rect {
                    x: area.x,
                    y: area.y,
                    w: area.w,
                    h: area.h,
                    fill: Some(PaintColor::Token { name: fill }),
                    stroke: None,
                    stroke_width: None,
                    corner_radius: None,
                });
                c.text("日", area.x + 4.0, area.y + 4.0, 12.0, PaintColor::Token { name: ColorToken::Text });
                c.hit("cell", Rect::new(area.x, area.y, 40.0, 40.0), HitCursor::Pointer);
            });
        };
        let frame = bench.render(build).await.unwrap();
        assert!(has_hit(&frame, "cell"));
        assert!(texts(&frame).iter().any(|text| text == "日"));

        let events = bench.click_and_collect("cell").await;
        assert_eq!(events, vec![UiEvent::Click { id: "cell".into() }]);

        bench
            .event(FrameInput::PointerMove { x: 5.0, y: 5.0, hit: Some("cell".into()) })
            .await;
        let frame = bench.render(build).await.unwrap();
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::Rect { fill: Some(PaintColor::Token { name: ColorToken::Accent }), .. }
        )), "悬停态反映在自绘外观里");
    }

    #[tokio::test]
    async fn image_declares_a_tile_with_the_requested_fit() {
        let mut bench = Bench::new(200.0, 120.0).await;
        let frame = bench
            .render(|ui| ui.image_fit(ImageSource::Vault { path: "a.png".into() }, 80.0, ImageFit::Cover))
            .await
            .unwrap();
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::Image { fit: Some(ImageFit::Cover), w, h, .. } if *w == 200.0 && *h == 80.0
        )));
    }

    #[tokio::test]
    async fn declared_fps_rides_on_every_frame() {
        let mut bench = Bench::new(200.0, 120.0).await;
        let frame = bench.render(|ui| ui.text("静止", TextRole::Label)).await.unwrap();
        assert_eq!(frame.fps, None, "未声明即不推帧拍");
        bench.ui.lock().await.set_fps(Some(30));
        let frame = bench.render(|ui| ui.text("播放中", TextRole::Label)).await.unwrap();
        assert_eq!(frame.fps, Some(30));
    }
}
