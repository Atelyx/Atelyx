//! 控件层：把按钮 / 输入框 / 下拉 / 勾选 / 滚动翻译成整面帧（绘制指令 + 命中区），并把宿主回发的
//! 原始输入解释成语义事件。用法是两阶段——先 [`FrameUi::event`] 拿事件、插件改自己的状态，
//! 再 [`FrameUi::render`] 声明并提交一帧；帧永远是「事件之后」的状态。
//!
//! 文字量算与 Markdown 块缺失时 `render` 内部补齐并重建（最多 [`MAX_REBUILD_PASSES`] 遍），插件不必感知。

mod build;
mod geom;
mod icons;
mod input;
mod lucide;
mod markdown;
mod metrics;
mod model;
mod svgpath;
mod text;
mod theme;
mod widgets;

use atelyx_plugin_protocol::envelope::RpcError;
use atelyx_plugin_protocol::frame::{FrameInput, FrameSize};
use atelyx_plugin_protocol::method::MAX_MEASURE_ITEMS;

use crate::Context;

pub use build::Ui;
pub use build::Canvas;
pub use geom::Rect;
pub use icons::{commands as icon_commands, Icon};
pub use input::InputOutcome;
pub use widgets::{AreaHeight, BoxStyle, BoxWidth, Edges, TextRole, TextStyle};
/// 引用候选（`#笔记：方案.md` 这类标签）：交给 `Ui::markdown_with` 画成胶囊。
pub use atelyx_format::markdown::MentionCandidate;

/// 一次 `render` 内最多补几轮缺项（文字量算 / Markdown 块）：补齐后按最新数据重建。
const MAX_REBUILD_PASSES: usize = 3;

/// 控件层自驱动画（光标闪烁、转圈指示）需要的帧拍频率：10 拍/秒。
/// 声明它只是为了让画面动起来——宿主对同内容帧会去重，闪烁一拍里通常只有相位翻转那几帧真正上屏。
pub const TICK_HZ: u16 = 10;

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
    /// 输入框光标 / 选区变化（文本未必变）：插件据此重算与光标位置相关的浮层（如 `#` 候选）。
    Caret { id: String, caret: usize, anchor: usize },
    /// 输入框回车（提交语义由插件定，例如检索或写回设置）。
    Submit { id: String },
    /// 下拉选中一项（`index` 是声明时给出的选项下标）。
    Select { id: String, index: usize },
    /// 键盘焦点进入 / 离开某控件（输入框可据此提交草稿）。
    Focus { id: String, focused: bool },
    /// 滚动容器偏移变化：`offset` 为当前偏移，`max_offset` 为可滚动余量（滚到底翻页这类判断用）。
    Scroll { id: String, offset: f32, max_offset: f32 },
    /// 自绘子区上的拖拽越过起拖阈值（此后的抬起不再出 `Click`）。
    DragStart { id: String },
    /// 拖拽中：`dx` / `dy` 是从按下点的累计位移。
    Drag { id: String, dx: f32, dy: f32 },
    /// 拖拽结束（抬起时的累计位移）：插件据此落结果。
    DragEnd { id: String, dx: f32, dy: f32 },
    /// 拖拽被取消（Esc 或面失去焦点）：插件回退拖拽期间的预览。
    DragCancel { id: String },
    /// Esc 落在聚焦的输入框：先于 `Focus { focused: false }` 上报，插件据此放弃编辑草稿而不是提交。
    Cancel { id: String },
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

    /// 面重挂：丢弃主题与依赖主题的版面缓存，尺寸 / 滚动 / 焦点保留；
    /// 下次 `render` 会重新查询宿主主题。
    pub fn refresh_theme(&mut self) {
        self.state.refresh_theme();
    }

    /// 声明宿主帧拍频率（1–120）：声明后宿主按该节拍回发 `host:frame.tick`，
    /// 插件的播放 / 动画在帧拍里推进并重渲染；`None` = 只按控件层的需要推帧。
    /// 控件层自己会为光标闪烁与转圈指示要求 [`TICK_HZ`]，这里只用于插件自有的动画。
    pub fn set_fps(&mut self, fps: Option<u16>) {
        self.fps = fps;
    }

    /// 推进一拍：自转指示的相位由拍数算，插件在 `on_frame_tick` 里调用。
    pub fn tick(&mut self) {
        self.state.clock = self.state.clock.wrapping_add(1);
    }

    /// 程序化聚焦输入框（如点开行内编辑）：光标与选区落到 `value` 末尾。已聚焦时不重复置位，
    /// 也不产生 `Focus` 事件（那是宿主输入的解释结果，插件自己知道刚聚焦了谁）。
    /// 须在 `render` 声明该输入框之前调用——构建布局后 `prune` 会清掉未出现在帧里的焦点。
    pub fn focus_input(&mut self, id: &str, value: &str) {
        if self.state.focus.as_deref() == Some(id) {
            return;
        }
        if let Some(old) = self.state.focus.take() {
            if let Some(item) = self.state.inputs.get_mut(&old) {
                item.preedit.clear();
            }
        }
        let len = value.len();
        self.state
            .inputs
            .insert(
                id.to_string(),
                model::InputState { caret: len, anchor: len, ..model::InputState::default() },
            );
        self.state.focus = Some(id.to_string());
    }

    /// 把滚动容器直接拉到底（回到底部浮钮这类动作）；下一帧贴底跟随继续生效。
    pub fn scroll_to_end(&mut self, id: &str) {
        self.state.scroll.insert(id.to_string(), f32::MAX);
    }

    /// 把滚动容器定位到指定偏移（候选键盘巡航这类动作）；落帧时按内容高度钳制。
    pub fn scroll_to(&mut self, id: &str, offset: f32) {
        self.state.scroll.insert(id.to_string(), offset.max(0.0));
    }

    /// 某输入框当前光标与选区的字节偏移（`None` = 该输入框不在本帧或未交互过）。
    pub fn caret(&self, id: &str) -> Option<(usize, usize)> {
        self.state.inputs.get(id).map(|item| (item.caret, item.anchor))
    }

    /// 当前持有键盘焦点的控件 id（无焦点 = `None`）。插件自己转发语义按键前先确认焦点归属。
    pub fn focus_id(&self) -> Option<&str> {
        self.state.focus.as_deref()
    }

    /// 设置输入框的光标与选区（`value` 只用于把光标夹在文本长度内）；文本本身由插件在
    /// `render` 时传入。须在 `render` 前调用。
    pub fn set_input(&mut self, id: &str, value: &str, caret: usize) {
        let caret = caret.min(value.len());
        self.state
            .inputs
            .insert(
                id.to_string(),
                model::InputState { caret, anchor: caret, ..model::InputState::default() },
            );
    }

    /// 全选输入框内容并清空其撤销栈（进入槽位编辑时键入即替换原值，上一槽的撤销历史不得带进来）；
    /// 须在 `render` 前调用。
    pub fn select_all_input(&mut self, id: &str, value: &str) {
        let caret = value.len();
        self.state
            .inputs
            .insert(
                id.to_string(),
                model::InputState { caret, anchor: 0, ..model::InputState::default() },
            );
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
        self.event_outcome(ctx, input, value_of).await.events
    }

    /// 与 [`FrameUi::event`] 相同，另报这次输入是否改变了画面：`repaint` 为假（纯悬停移动）时
    /// 插件可以跳过 `render`——交互态、光标相位与事件都没变，重排出来的帧与上一帧一致。
    pub async fn event_outcome(
        &mut self,
        ctx: &Context,
        input: &FrameInput,
        value_of: impl Fn(&str) -> Option<String>,
    ) -> InputOutcome {
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
        // 收到首个尺寸前也要出帧：宿主只在有帧时才创建绘制部件，而部件正是上报尺寸的一端，
        // 在这里拦住就等于两头互等。首帧按占位尺寸排，宿主不会把它画出来（帧带排版尺寸）。
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
                build::layout_overlays(&mut ctxb);
                // 提示压在最后：悬停够久才画，浮在所有内容之上
                ctxb.paint_hint();
                ctxb.finish()
            };
            // 光标闪烁、自转指示与提示等待到点都要按拍推进
            let animated = out.wants_ticks || self.state.focus.is_some() || self.state.hint_pending();
            out.frame.fps = self.fps.or(animated.then_some(TICK_HZ));
            // 帧带上本帧的排版尺寸：改宽期间宿主手里的帧常常是上一拍宽度排的，
            // 只有声明了尺寸，宿主才能把这种帧认出来不画
            out.frame.size = Some(FrameSize { w: size.0, h: size.1 });
            self.state.metrics = out.metrics;
            self.state.layouts = out.layouts;
            self.state.focusables = out.focusables;
            self.state.draggables = out.draggables;
            self.state.scroll = out.scrolls;
            self.state.scroll_max = out.scroll_max;
            self.state.follow_prev = out.follow_prev;
            self.state.overlays = out.overlays;
            self.state.hints = out.hints;
            self.state.markdown_order = out.markdown_seen.iter().map(|(id, _)| id.clone()).collect();
            self.state.markdown_last = out.markdown_seen.into_iter().collect();
            self.state.overlay_scrolls = out.overlay_scrolls;
            self.state.prune();
            let mut frame = Some(out.frame);
            // 尺寸刚变且本帧还有缺项：先按估算字宽交一帧（拖动跟手），量算补齐后再交精确帧
            if std::mem::take(&mut self.state.resize_pending) && out.misses {
                self.submit_if_changed(ctx, surface_id, frame.take().expect("本帧尚未提交"))
                    .await?;
            }
            if out.misses && passes < MAX_REBUILD_PASSES {
                let misses = self.state.metrics.take_misses();
                match self.fill_metrics(ctx, misses).await {
                    Ok(()) => {
                        passes += 1;
                        continue;
                    }
                    // 会话收尾期间量算必然失败：按估算字宽把这一帧交出去即可，不进日志
                    Err(err) => {
                        if !err.message.contains("会话已结束") {
                            eprintln!("量算失败，按估算字宽出这一帧：{err}");
                        }
                    }
                }
            }
            if let Some(frame) = frame {
                self.submit_if_changed(ctx, surface_id, frame).await?;
            }
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
            .is_some_and(|(id, submitted)| id == surface_id && submitted.as_ref() == &frame);
        if same {
            return Ok(());
        }
        let frame = std::sync::Arc::new(frame);
        let result = ctx.submit_frame_shared(surface_id, frame.clone()).await?;
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

    use atelyx_plugin_protocol::paint::{
        ColorToken, DrawCommand, ImageFit, ImageSource, PaintColor, TextWeight,
    };
    use atelyx_plugin_protocol::envelope::Envelope;
    use atelyx_plugin_protocol::frame::{FrameInput, HitCursor, SurfaceFrame};
    use atelyx_plugin_protocol::method::SubmitResult;
    use atelyx_plugin_protocol::method::{
        self, FontSizes, MeasureBatchParams, MeasureTextResult, Radii, TextPayload, ThemeColors, ThemeResult,
    };
    use atelyx_plugin_protocol::frame::{Key, KeyAction, Modifiers, MouseButton, NamedKey};
    use tokio::sync::mpsc;

    use super::*;
    use super::model::Layout;

    /// 假宿主：应答主题 / 量算 / 提交与剪贴板，并记下请求顺序与已提交的帧。
    struct Bench {
        ctx: Context,
        rx: mpsc::Receiver<Envelope>,
        ui: Arc<tokio::sync::Mutex<FrameUi>>,
        values: Arc<Mutex<HashMap<String, String>>>,
        clipboard: Arc<Mutex<String>>,
        frames: Arc<Mutex<Vec<SurfaceFrame>>>,
        requests: Arc<Mutex<Vec<String>>>,
    }

    impl Bench {
        async fn new(width: f32, height: f32) -> Self {
            let mut bench = Self::without_size().await;
            // 面尺寸只在宿主首次绘制时上报，测试里直接喂一次 Resize
            bench.event(FrameInput::Resize { width, height }).await;
            bench
        }

        /// 还没收到宿主尺寸的基准：验证「首个 resize 之前也能出帧」这条约束。
        async fn without_size() -> Self {
            let (tx, rx) = mpsc::channel(64);
            let frames = Arc::new(Mutex::new(Vec::new()));
            let requests = Arc::new(Mutex::new(Vec::new()));
            let submitted = frames.clone();
            let logged = requests.clone();
            let ctx = Context::build_fixture(tx, Arc::new(move |_, frame| {
                frame
                    .validate()
                    .map_err(|error| RpcError::failed(format!("帧不合法：{error}")))?;
                logged.lock().unwrap().push("submit_frame".to_string());
                submitted.lock().unwrap().push((*frame).clone());
                Ok(SubmitResult { applied: true })
            }));
            Self {
                ctx,
                rx,
                ui: Arc::new(tokio::sync::Mutex::new(FrameUi::new())),
                values: Arc::default(),
                clipboard: Arc::default(),
                frames,
                requests,
            }
        }

        /// 已提交的帧快照（提交在后台闭包里，按需取一份）。
        fn frames(&self) -> Vec<SurfaceFrame> {
            self.frames.lock().unwrap().clone()
        }

        /// 已发出的请求序列快照。
        fn requests(&self) -> Vec<String> {
            self.requests.lock().unwrap().clone()
        }

        async fn event(&mut self, input: FrameInput) -> Vec<UiEvent> {
            self.outcome(input).await.events
        }

        /// 带「是否要重绘」的输入结果（悬停合并与不让插件空转都据此判断）。
        async fn outcome(&mut self, input: FrameInput) -> InputOutcome {
            let ui = self.ui.clone();
            let ctx = self.ctx.clone();
            let values = self.values.clone();
            let task = tokio::spawn(async move {
                let mut guard = ui.lock().await;
                guard
                    .event_outcome(&ctx, &input, move |id| values.lock().unwrap().get(id).cloned())
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
            Ok(self
                .frames
                .lock()
                .unwrap()
                .last()
                .expect("渲染成功即提交了帧")
                .clone())
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

        async fn drive<T>(&mut self, mut task: tokio::task::JoinHandle<T>) -> T {
            loop {
                tokio::select! {
                    envelope = self.rx.recv() => {
                        if let Some(envelope) = envelope {
                            self.reply(envelope);
                        }
                    }
                    result = &mut task => return result.expect("任务不应 panic"),
                }
            }
        }

        fn reply(&mut self, envelope: Envelope) {
            let Envelope::Request(request) = envelope else {
                panic!("插件只发请求");
            };
            self.requests.lock().unwrap().push(request.method.clone());
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
    segments: None,
                        })
                        .collect();
                    serde_json::json!({ "sizes": sizes })
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
            event_colors: vec!["#e06c75".into()],
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

    /// 光标竖条的横坐标（一条高约一行的 1px 细条）。
    fn caret_bar(frame: &SurfaceFrame) -> Option<f32> {
        frame.commands.iter().find_map(|command| match command {
            DrawCommand::Rect { x, w, h, .. } if (*w - 1.0).abs() < f32::EPSILON && *h >= 10.0 => Some(*x),
            _ => None,
        })
    }

    fn has_hit(frame: &SurfaceFrame, id: &str) -> bool {
        frame.hits.iter().any(|hit| hit.id == id)
    }

    /// 某个坐标上最后声明的命中（宿主取最上面的一条）。
    fn top_hit(frame: &SurfaceFrame, x: f32, y: f32) -> Option<&str> {
        frame
            .hits
            .iter()
            .rev()
            .find(|hit| x >= hit.x && x <= hit.x + hit.w && y >= hit.y && y <= hit.y + hit.h)
            .map(|hit| hit.id.as_str())
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
    async fn overlay_draws_after_main_and_owns_the_wheel() {
        let mut bench = Bench::new(360.0, 300.0).await;
        let frame = bench
            .render(|ui| {
                ui.scroll_sized("main-list", 240.0, 0.0, false, |ui| {
                    for index in 0..30 {
                        ui.text(format!("行{index}"), TextRole::Label);
                    }
                });
                ui.overlay(Rect::new(40.0, 40.0, 240.0, 140.0), |ui| {
                    ui.card("overlay-card", false, |ui| {
                        ui.scroll_sized("overlay-list", 80.0, 0.0, false, |ui| {
                            for index in 0..30 {
                                ui.text(format!("项{index}"), TextRole::Label);
                            }
                        });
                    });
                });
            })
            .await
            .unwrap();

        let all = texts(&frame);
        let first_overlay = all.iter().position(|text| text.starts_with('项')).expect("浮层文字");
        let last_main = all.iter().rposition(|text| text.starts_with('行')).expect("主内容文字");
        assert!(first_overlay > last_main, "浮层内容整体在主内容之后绘制：{all:?}");
        assert!(has_hit(&frame, "overlay-card"), "浮层命中区随帧提交");

        let scrolls = bench
            .event(FrameInput::Wheel { x: 80.0, y: 80.0, delta_x: 0.0, delta_y: 24.0, modifiers: Default::default(), hit: None })
            .await;
        assert!(
            matches!(scrolls.as_slice(), [UiEvent::Scroll { id, .. }] if id == "overlay-list"),
            "浮层打开时滚轮只作用于浮层内：{scrolls:?}"
        );

        let outside = bench
            .event(FrameInput::Wheel { x: 330.0, y: 100.0, delta_x: 0.0, delta_y: 24.0, modifiers: Default::default(), hit: None })
            .await;
        assert!(outside.is_empty(), "浮层外的滚轮不再落到下层列表：{outside:?}");

        let blank = bench
            .event(FrameInput::Wheel { x: 270.0, y: 170.0, delta_x: 0.0, delta_y: 24.0, modifiers: Default::default(), hit: None })
            .await;
        assert!(blank.is_empty(), "浮层内、滚动区外的滚轮也不落到下层：{blank:?}");
    }

    #[tokio::test]
    async fn overlay_blocks_clicks_outside_its_children() {
        let mut bench = Bench::new(300.0, 200.0).await;
        let frame = bench
            .render(|ui| {
                ui.button("under", "底层");
                ui.overlay(Rect::new(20.0, 20.0, 260.0, 160.0), |ui| {
                    ui.button("floating", "浮层按钮");
                });
            })
            .await
            .unwrap();
        assert_eq!(top_hit(&frame, 275.0, 175.0), Some("@overlay:0"), "浮层空白承接点击，不穿透下层");
        assert_ne!(top_hit(&frame, 5.0, 5.0), Some("@overlay:0"), "浮层外不受它拦截");
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
        assert_eq!(
            events,
            vec![UiEvent::Caret { id: "body".into(), caret: 6, anchor: 6 }],
            "上下键只挪光标"
        );
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

    /// 组合串进入排版：光标之后的文字被推开，光标落在串尾、候选窗锚在串首。
    #[tokio::test]
    async fn composition_pushes_the_following_text_and_anchors_the_candidates_at_its_head() {
        let mut bench = Bench::new(320.0, 120.0).await;
        let build = |ui: &mut build::Ui<'_, '_>| ui.text_area_submit("draft", "abcdef", "", 48.0);
        bench.render(build).await.unwrap();
        {
            let mut ui = bench.ui.lock().await;
            ui.focus_input("draft", "abcdef");
            ui.set_input("draft", "abcdef", 3);
        }
        bench.set_value("draft", "abcdef");
        let frame = bench.render(build).await.unwrap();
        let lit = caret_bar(&frame).expect("聚焦后光标可见");

        bench
            .event(FrameInput::Text { text: "zhong".into(), composing: true, caret: Some(5), candidates: None })
            .await;
        let frame = bench.render(build).await.unwrap();
        let all = texts(&frame);
        assert!(all.iter().any(|text| text == "abczhongdef"), "组合串进入正文，后面的字被推开：{all:?}");
        assert!(all.iter().any(|text| text == "zhong"), "组合串叠强调色：{all:?}");
        let ime = frame.ime.expect("组合期仍要报候选窗锚点");
        let after = caret_bar(&frame).expect("组合期光标可见");
        assert!(ime.x < after, "候选窗贴组合串起点（{}），光标在串尾（{}）", ime.x, after);
        assert!(after > lit, "光标随组合串推到串尾");
    }

    /// 组合串也参与换行：窄框里它把后面的内容顶到下一行。
    #[tokio::test]
    async fn composition_wraps_like_regular_text() {
        let mut bench = Bench::new(48.0, 140.0).await;
        let build = |ui: &mut build::Ui<'_, '_>| ui.text_area_submit("draft", "abcd", "", 90.0);
        bench.render(build).await.unwrap();
        {
            let mut ui = bench.ui.lock().await;
            ui.focus_input("draft", "abcd");
            ui.set_input("draft", "abcd", 4);
        }
        bench.set_value("draft", "abcd");
        bench.render(build).await.unwrap();

        bench
            .event(FrameInput::Text { text: "zhong".into(), composing: true, caret: Some(5), candidates: None })
            .await;
        let frame = bench.render(build).await.unwrap();
        assert!(
            texts(&frame).iter().any(|text| text.starts_with("abcd")),
            "组合串接在正文之后而不是替换它：{:?}",
            texts(&frame)
        );
        let accent_rows: Vec<f32> = frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { color: PaintColor::Token { name: ColorToken::Accent }, y, .. } => {
                    Some(*y)
                }
                _ => None,
            })
            .collect();
        assert!(
            accent_rows.windows(2).any(|pair| (pair[0] - pair[1]).abs() > 1.0),
            "组合串跟着排版折到下一行：{accent_rows:?}"
        );
    }

    /// 组合期点选与按键都归输入法：光标、选区与文本一律不动。
    #[tokio::test]
    async fn composition_keeps_the_caret_and_ignores_editing_keys() {
        let mut bench = Bench::new(320.0, 120.0).await;
        let build = |ui: &mut build::Ui<'_, '_>| ui.text_area_submit("draft", "abcdef", "", 48.0);
        bench.render(build).await.unwrap();
        {
            let mut ui = bench.ui.lock().await;
            ui.focus_input("draft", "abcdef");
            ui.set_input("draft", "abcdef", 3);
        }
        bench.set_value("draft", "abcdef");
        bench.render(build).await.unwrap();
        bench
            .event(FrameInput::Text { text: "zhong".into(), composing: true, caret: Some(5), candidates: None })
            .await;

        bench
            .event(FrameInput::PointerDown { x: 40.0, y: 10.0, button: MouseButton::Left, hit: Some("draft".into()) })
            .await;
        assert_eq!(
            bench.ui.lock().await.caret("draft").map(|(caret, _)| caret),
            Some(3),
            "组合期点选不接管光标"
        );

        let events = bench.event(key(NamedKey::Backspace)).await;
        assert!(events.is_empty(), "组合期按键交给输入法：{events:?}");
        let events = bench.event(typed("x")).await;
        assert!(events.is_empty(), "组合期的字符由输入法提交，不直接进值：{events:?}");
    }

    /// 只读正文里拖选后 Ctrl+C：按版面取可见文字写剪贴板（不含 Markdown 标记）。
    #[tokio::test]
    async fn dragging_read_only_markdown_selects_text_for_ctrl_c() {
        let mut bench = Bench::new(320.0, 200.0).await;
        let build = |ui: &mut build::Ui<'_, '_>| ui.markdown("第一行文字");
        bench.render(build).await.unwrap();
        let id = Some(format!("@md:1:{:x}", super::markdown::content_key("第一行文字", &[])));
        bench
            .event(FrameInput::PointerDown { x: 4.0, y: 4.0, button: MouseButton::Left, hit: id.clone() })
            .await;
        bench
            .event(FrameInput::PointerMove { x: 200.0, y: 6.0, hit: id.clone() })
            .await;
        bench
            .event(FrameInput::PointerUp { x: 200.0, y: 6.0, button: MouseButton::Left, hit: id })
            .await;
        let ctrl = Modifiers { control: true, ..Modifiers::default() };
        let events = bench
            .event(FrameInput::Key {
                key: Key::Character { text: "c".into() },
                action: KeyAction::Press,
                modifiers: ctrl,
                text: None,
            })
            .await;
        assert!(events.is_empty(), "复制不该再给插件报事件：{events:?}");
        assert_eq!(
            bench.clipboard.lock().unwrap().as_str(),
            "第一行文字",
            "拖选整行后复制应拿到该行文字"
        );
    }

    /// 代码块复制钮与任务框的命中区要压在整块正文命中区之上：子区点击不被正文拖选吞掉。
    #[tokio::test]
    async fn markdown_sub_hits_sit_above_the_block_hit() {
        let mut bench = Bench::new(360.0, 240.0).await;
        let build = |ui: &mut build::Ui<'_, '_>| {
            ui.markdown_named("md", "```\nlet a = 1;\n```\n\n- [ ] 待办", Vec::new());
        };
        bench.render(build).await.unwrap();
        let frame = bench.frames().last().expect("应提交一帧").clone();
        for suffix in ["::copy:0", "::task:0"] {
            let hit = frame
                .hits
                .iter()
                .find(|hit| hit.id.ends_with(suffix))
                .unwrap_or_else(|| panic!("缺少子区命中 {suffix}"));
            assert_eq!(
                frame.hit_at(hit.x + hit.w / 2.0, hit.y + hit.h / 2.0),
                Some(hit.id.as_str()),
                "{suffix} 应是最上层命中"
            );
        }
    }

    /// 表格 / 代码块混排里拖选整块：命中与取字都不得 panic（同行多列、多个片段行）。
    #[tokio::test]
    async fn selecting_across_a_table_and_a_code_block_is_safe() {
        let source = "| 甲 | 乙 |
| --- | --- |
| 1 | 2 |

```rust
let a = 1;
```

正文收尾";
        let mut bench = Bench::new(320.0, 320.0).await;
        let build = move |ui: &mut build::Ui<'_, '_>| ui.markdown(source);
        bench.render(build).await.unwrap();
        let id = Some(format!("@md:1:{:x}", super::markdown::content_key(source, &[])));
        bench
            .event(FrameInput::PointerDown { x: 2.0, y: 2.0, button: MouseButton::Left, hit: id.clone() })
            .await;
        for y in [10.0_f32, 40.0, 80.0, 140.0, 200.0, 300.0] {
            bench
                .event(FrameInput::PointerMove { x: 300.0, y, hit: id.clone() })
                .await;
        }
        bench
            .event(FrameInput::PointerUp { x: 300.0, y: 300.0, button: MouseButton::Left, hit: id })
            .await;
        let ctrl = Modifiers { control: true, ..Modifiers::default() };
        bench
            .event(FrameInput::Key {
                key: Key::Character { text: "c".into() },
                action: KeyAction::Press,
                modifiers: ctrl,
                text: None,
            })
            .await;
        let copied = bench.clipboard.lock().unwrap().clone();
        assert!(!copied.is_empty(), "跨表格与代码块拖选后应能复制到文字");
    }

    /// 只读正文单击只留下空选择：不画光标细条，Ctrl+C 也拿不到内容。
    #[tokio::test]
    async fn clicking_read_only_markdown_draws_no_caret() {
        let mut bench = Bench::new(320.0, 200.0).await;
        let build = |ui: &mut build::Ui<'_, '_>| ui.markdown("第一行文字");
        bench.render(build).await.unwrap();
        let id = Some(format!("@md:1:{:x}", super::markdown::content_key("第一行文字", &[])));
        bench
            .event(FrameInput::PointerDown { x: 20.0, y: 8.0, button: MouseButton::Left, hit: id.clone() })
            .await;
        bench
            .event(FrameInput::PointerUp { x: 20.0, y: 8.0, button: MouseButton::Left, hit: id })
            .await;
        let frame = bench.render(build).await.unwrap();
        assert!(caret_bar(&frame).is_none(), "单击不得画出光标细条");
        let ctrl = Modifiers { control: true, ..Modifiers::default() };
        bench
            .event(FrameInput::Key {
                key: Key::Character { text: "c".into() },
                action: KeyAction::Press,
                modifiers: ctrl,
                text: None,
            })
            .await;
        assert!(bench.clipboard.lock().unwrap().is_empty(), "空选区不产生复制内容");
    }

    /// 两块正文之间拖选：选择按绘制序跨块延伸，Ctrl+C 取到两块的文字。
    #[tokio::test]
    async fn dragging_between_two_markdown_blocks_copies_across_them() {
        let mut bench = Bench::new(320.0, 200.0).await;
        let build = |ui: &mut build::Ui<'_, '_>| {
            ui.markdown_named("md-a", "甲行", Vec::new());
            ui.markdown_named("md-b", "乙行", Vec::new());
        };
        let frame = bench.render(build).await.unwrap();
        let a = frame.hits.iter().find(|hit| hit.id == "md-a").cloned().expect("第一块命中区");
        let b = frame.hits.iter().find(|hit| hit.id == "md-b").cloned().expect("第二块命中区");
        bench
            .event(FrameInput::PointerDown {
                x: a.x + 2.0,
                y: a.y + a.h * 0.5,
                button: MouseButton::Left,
                hit: Some("md-a".into()),
            })
            .await;
        bench
            .event(FrameInput::PointerMove { x: b.x + b.w - 2.0, y: b.y + b.h * 0.5, hit: Some("md-b".into()) })
            .await;
        bench
            .event(FrameInput::PointerUp {
                x: b.x + b.w - 2.0,
                y: b.y + b.h * 0.5,
                button: MouseButton::Left,
                hit: Some("md-b".into()),
            })
            .await;
        let ctrl = Modifiers { control: true, ..Modifiers::default() };
        bench
            .event(FrameInput::Key {
                key: Key::Character { text: "c".into() },
                action: KeyAction::Press,
                modifiers: ctrl,
                text: None,
            })
            .await;
        assert_eq!(bench.clipboard.lock().unwrap().as_str(), "甲行\n乙行");
    }

    /// 端点块不再出帧（被虚拟滚动裁掉或删除）时选区作废，滚回来不会复原旧选区。
    #[tokio::test]
    async fn selection_drops_when_an_end_block_leaves_the_frame() {
        let mut bench = Bench::new(320.0, 200.0).await;
        let both = |ui: &mut build::Ui<'_, '_>| {
            ui.markdown_named("md-a", "甲行", Vec::new());
            ui.markdown_named("md-b", "乙行", Vec::new());
        };
        let frame = bench.render(both).await.unwrap();
        let a = frame.hits.iter().find(|hit| hit.id == "md-a").cloned().expect("第一块命中区");
        let b = frame.hits.iter().find(|hit| hit.id == "md-b").cloned().expect("第二块命中区");
        bench
            .event(FrameInput::PointerDown {
                x: a.x + 2.0,
                y: a.y + a.h * 0.5,
                button: MouseButton::Left,
                hit: Some("md-a".into()),
            })
            .await;
        bench
            .event(FrameInput::PointerMove { x: b.x + b.w - 2.0, y: b.y + b.h * 0.5, hit: Some("md-b".into()) })
            .await;
        bench
            .event(FrameInput::PointerUp {
                x: b.x + b.w - 2.0,
                y: b.y + b.h * 0.5,
                button: MouseButton::Left,
                hit: Some("md-b".into()),
            })
            .await;
        assert!(bench.ui.lock().await.state.selection.is_some(), "拖选后应有选区");
        bench.render(|ui| ui.markdown_named("md-a", "甲行", Vec::new())).await.unwrap();
        assert!(bench.ui.lock().await.state.selection.is_none(), "端点块离帧后选区应作废");
    }

    /// 超宽表格：按最宽格定列宽并把内容并进块内横向滚动，绘制裁在块矩形里。
    #[tokio::test]
    async fn wide_table_scrolls_inside_its_block() {
        let long = "一二三四五六七八九十一二三四五六七八九十";
        let source = format!("| {long} | 乙 |\n| --- | --- |\n| 丙 | 丁 |");
        let mut bench = Bench::new(320.0, 200.0).await;
        let build = move |ui: &mut build::Ui<'_, '_>| ui.markdown_named("wide", source.clone(), Vec::new());
        let frame = bench.render(build.clone()).await.unwrap();
        let rect = frame.hits.iter().find(|hit| hit.id == "wide").cloned().expect("表格块命中区");
        let content_w = {
            let ui = bench.ui.lock().await;
            match ui.state.layouts.get("wide") {
                Some(Layout::Markdown { content_w, .. }) => *content_w,
                _ => panic!("应记录为 Markdown 版面"),
            }
        };
        assert!(content_w > rect.w + 0.5, "超宽表格的自然宽应大于外框：{content_w}/{}", rect.w);
        assert!(has_hit(&frame, "wide::hbar"), "超宽内容应给横向滚动条");

        let events = bench
            .event(FrameInput::Wheel {
                x: rect.x + rect.w * 0.5,
                y: rect.y + rect.h * 0.5,
                delta_x: 40.0,
                delta_y: 0.0,
                modifiers: Default::default(),
                hit: None,
            })
            .await;
        let UiEvent::Scroll { id, offset, max_offset } = &events[0] else {
            panic!("横向滚动应回滚动位置：{events:?}");
        };
        assert_eq!(id, "wide");
        assert!((*offset - 40.0).abs() < 0.01 && *max_offset > 40.0, "{offset}/{max_offset}");

        let frame = bench.render(build).await.unwrap();
        assert!(!texts(&frame).iter().any(|text| text == long), "滚出左缘的内容应被裁掉");
        for command in &frame.commands {
            match command {
                DrawCommand::Rect { x, y, w, .. } if (rect.y..=rect.y + rect.h).contains(y) => {
                    assert!(*x >= rect.x - 0.5 && *x + *w <= rect.x + rect.w + 0.5, "矩形越界：{x}+{w}");
                }
                DrawCommand::Text { x, y, .. } if (rect.y..=rect.y + rect.h).contains(y) => {
                    assert!(*x >= rect.x - 0.5 && *x <= rect.x + rect.w + 0.5, "文字越界：{x}");
                }
                _ => {}
            }
        }
    }

    /// 超宽代码块：正文在块内横向滚动，头部（含复制钮）保持原位。
    #[tokio::test]
    async fn wide_code_block_scrolls_without_moving_the_header() {
        let code = format!("```rust\n{}\n```", "let a = 1; ".repeat(30));
        let mut bench = Bench::new(320.0, 200.0).await;
        let build = move |ui: &mut build::Ui<'_, '_>| ui.markdown_named("code", code.clone(), Vec::new());
        let frame = bench.render(build).await.unwrap();
        assert!(has_hit(&frame, "code::hbar"), "超宽代码正文应给横向滚动条");
        assert!(has_hit(&frame, "code::copy:0"), "代码头复制钮不随正文滚动");
    }

    /// 输入框行高参数：文本区的行距与外层渲染行一致（揭示块按它逐行对齐）。
    #[tokio::test]
    async fn input_line_height_sets_the_text_area_rows() {
        let mut bench = Bench::new(320.0, 200.0).await;
        let build = |ui: &mut build::Ui<'_, '_>| {
            ui.input_line_height(Some(24.0));
            ui.text_area_plain_auto("draft", "甲\n乙", 10.0, 200.0, 14.0, Edges::symmetric(0.0, 0.0));
        };
        let frame = bench.render(build).await.unwrap();
        let hit = frame.hits.iter().find(|hit| hit.id == "draft").expect("输入面命中区");
        assert!((hit.h - 48.0).abs() < 0.5, "两行按 24px 行高：{}", hit.h);
        let line = {
            let ui = bench.ui.lock().await;
            match ui.state.layouts.get("draft") {
                Some(Layout::TextArea { line, .. }) => *line,
                _ => panic!("应记录为多行输入面"),
            }
        };
        assert!((line - 24.0).abs() < 0.5, "版面记录的行高应取自参数：{line}");
    }

    /// 悬停提示：停够时间才画，等待期间要帧拍；指针移开即消失。
    #[tokio::test]
    async fn hovering_a_hinted_control_draws_the_tip_after_the_delay() {
        let mut bench = Bench::new(240.0, 120.0).await;
        let build = |ui: &mut build::Ui<'_, '_>| {
            ui.hint("go", "运行一次");
            ui.button("go", "运行");
        };
        bench.render(build).await.unwrap();
        bench
            .event(FrameInput::PointerMove { x: 10.0, y: 10.0, hit: Some("go".into()) })
            .await;
        let frame = bench.render(build).await.unwrap();
        assert!(!texts(&frame).iter().any(|text| text == "运行一次"), "未到点不出提示");
        assert_eq!(frame.fps, Some(TICK_HZ), "等待到点期间要帧拍");

        {
            let mut ui = bench.ui.lock().await;
            if let Some(hint) = ui.state.hint.as_mut() {
                hint.since = std::time::Instant::now() - std::time::Duration::from_millis(600);
            }
        }
        let frame = bench.render(build).await.unwrap();
        assert!(texts(&frame).iter().any(|text| text == "运行一次"), "到点后画出提示");

        bench.event(FrameInput::PointerMove { x: 10.0, y: 10.0, hit: None }).await;
        let frame = bench.render(build).await.unwrap();
        assert!(!texts(&frame).iter().any(|text| text == "运行一次"), "指针离开即收起");
    }

    /// 悬停路径：同一悬停区内反复移动不改画面，进入控件或换提示目标才要重绘。
    #[tokio::test]
    async fn hover_move_inside_the_same_control_skips_the_repaint() {
        let mut bench = Bench::new(240.0, 120.0).await;
        let build = |ui: &mut build::Ui<'_, '_>| {
            ui.hint("go", "运行一次");
            ui.button("go", "运行");
            ui.button("other", "其它");
        };
        bench.render(build).await.unwrap();
        let enter = bench
            .outcome(FrameInput::PointerMove { x: 10.0, y: 10.0, hit: Some("go".into()) })
            .await;
        assert!(enter.repaint, "进入控件要换悬停样式并重新开始提示计时");
        let same = bench
            .outcome(FrameInput::PointerMove { x: 14.0, y: 12.0, hit: Some("go".into()) })
            .await;
        assert!(same.events.is_empty(), "悬停不产生事件：{:?}", same.events);
        assert!(!same.repaint, "悬停区与提示目标都没变，重排出来的帧与上一帧相同");
    }

    /// 点击路径：按下改按下态、抬起出点击事件，两步都要重绘。
    #[tokio::test]
    async fn click_repaints_on_press_and_release() {
        let mut bench = Bench::new(200.0, 120.0).await;
        bench.render(|ui| ui.button("go", "启动")).await.unwrap();
        let down = bench
            .outcome(FrameInput::PointerDown { x: 1.0, y: 1.0, button: MouseButton::Left, hit: Some("go".into()) })
            .await;
        assert!(down.repaint, "按下态要立刻显示");
        assert!(down.events.is_empty(), "按钮按下不产生事件");
        let up = bench
            .outcome(FrameInput::PointerUp { x: 1.0, y: 1.0, button: MouseButton::Left, hit: Some("go".into()) })
            .await;
        assert!(up.repaint);
        assert_eq!(up.events, vec![UiEvent::Click { id: "go".into() }]);
    }

    /// 拖选路径：只延伸选区、不出事件，也必须重绘——选择高亮由控件层画。
    #[tokio::test]
    async fn dragging_a_read_only_selection_repaints_without_events() {
        let mut bench = Bench::new(320.0, 120.0).await;
        let build = |ui: &mut build::Ui<'_, '_>| ui.markdown("第一行文字");
        bench.render(build).await.unwrap();
        let id = Some(format!("@md:1:{:x}", super::markdown::content_key("第一行文字", &[])));
        let down = bench
            .outcome(FrameInput::PointerDown { x: 20.0, y: 8.0, button: MouseButton::Left, hit: id.clone() })
            .await;
        assert!(down.repaint, "按下即起选");
        let moved = bench
            .outcome(FrameInput::PointerMove { x: 200.0, y: 8.0, hit: id })
            .await;
        assert!(moved.events.is_empty(), "拖选不产生事件：{:?}", moved.events);
        assert!(moved.repaint, "选区延伸要重画高亮");
    }

    /// 输入法路径：组合串只改预输入（没有事件）也必须重绘，否则候选串不上屏。
    #[tokio::test]
    async fn ime_composition_repaints_without_events() {
        let mut bench = Bench::new(240.0, 120.0).await;
        bench.render(|ui| ui.text_input("host", "", "")).await.unwrap();
        bench
            .event(FrameInput::PointerDown { x: 20.0, y: 10.0, button: MouseButton::Left, hit: Some("host".into()) })
            .await;
        let composing = bench
            .outcome(FrameInput::Text { text: "zhong".into(), composing: true, caret: Some(5), candidates: None })
            .await;
        assert!(composing.events.is_empty(), "组合串不进入值");
        assert!(composing.repaint, "组合串要画在输入框里");
    }

    /// 滚动路径：偏移变了要重绘；滚到底再滚、偏移被钳住时不必重绘。
    #[tokio::test]
    async fn wheel_scroll_repaints_only_when_the_offset_moves() {
        let mut bench = Bench::new(320.0, 200.0).await;
        let build = |ui: &mut build::Ui<'_, '_>| {
            ui.scroll_sized("list", 120.0, 0.0, false, |ui| {
                for index in 0..20 {
                    ui.text(format!("行{index}"), TextRole::Label);
                }
            });
        };
        bench.render(build).await.unwrap();
        let scrolled = bench
            .outcome(FrameInput::Wheel {
                x: 80.0,
                y: 40.0,
                delta_x: 0.0,
                delta_y: 1000.0,
                modifiers: Default::default(),
                hit: None,
            })
            .await;
        assert!(scrolled.repaint);
        assert!(matches!(scrolled.events.as_slice(), [UiEvent::Scroll { .. }]));
        let clamped = bench
            .outcome(FrameInput::Wheel {
                x: 80.0,
                y: 40.0,
                delta_x: 0.0,
                delta_y: 24.0,
                modifiers: Default::default(),
                hit: None,
            })
            .await;
        assert!(clamped.events.is_empty(), "已在终点，本次滚轮没有事件");
        assert!(!clamped.repaint, "偏移没动，画面不变");
    }

    /// 聚焦即按拍推进（光标闪烁），闪烁的暗相只收竖条、不撤输入法锚点。
    #[tokio::test]
    async fn focused_input_blinks_the_caret_and_keeps_the_ime_anchor() {
        let mut bench = Bench::new(240.0, 120.0).await;
        let build = |ui: &mut build::Ui<'_, '_>| ui.text_area_submit("draft", "abcd", "", 48.0);
        bench.render(build).await.unwrap();
        bench.ui.lock().await.focus_input("draft", "abcd");
        let frame = bench.render(build).await.unwrap();
        assert_eq!(frame.fps, Some(TICK_HZ), "光标闪烁要帧拍");
        assert!(caret_bar(&frame).is_some(), "刚聚焦 / 刚编辑时光标可见");
        assert!(frame.ime.is_some());

        bench.ui.lock().await.state.blink_at =
            std::time::Instant::now() - std::time::Duration::from_millis(700);
        let frame = bench.render(build).await.unwrap();
        assert!(caret_bar(&frame).is_none(), "暗相不画竖条");
        assert!(frame.ime.is_some(), "暗相仍要报锚点，否则宿主会关掉输入法");

        bench.ui.lock().await.tick();
        let frame = bench.render(build).await.unwrap();
        assert!(caret_bar(&frame).is_none(), "帧拍推进不会凭空点亮光标");
    }

    #[tokio::test]
    async fn enter_during_ime_composition_does_not_submit() {
        let mut bench = Bench::new(240.0, 120.0).await;
        bench.render(|ui| ui.text_area_submit("draft", "hi", "", 48.0)).await.unwrap();
        bench
            .event(FrameInput::PointerDown { x: 20.0, y: 10.0, button: MouseButton::Left, hit: Some("draft".into()) })
            .await;
        bench
            .event(FrameInput::Text { text: "zhong".into(), composing: true, caret: Some(5), candidates: None })
            .await;

        let events = bench.event(key(NamedKey::Enter)).await;
        assert!(events.is_empty(), "组合期回车留给输入法取词，不提交也不换行：{events:?}");

        bench
            .event(FrameInput::Text { text: "中".into(), composing: false, caret: None, candidates: None })
            .await;
        let events = bench.event(key(NamedKey::Enter)).await;
        assert_eq!(events, vec![UiEvent::Submit { id: "draft".into() }]);
    }

    #[tokio::test]
    async fn select_expands_inline_and_reports_the_chosen_option() {
        let mut bench = Bench::new(240.0, 200.0).await;
        let options = vec!["hf".to_string(), "hf-mirror".to_string()];
        let first = options.clone();
        bench.render(move |ui| ui.select("mirror", first.clone(), 0)).await.unwrap();
        assert!(has_hit(&bench.frames().last().unwrap().clone(), "mirror"));

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
            .event(FrameInput::Wheel { x: 100.0, y: 60.0, delta_x: 0.0, delta_y: 100.0, modifiers: Default::default(), hit: None })
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
        assert_eq!(bench.frames().len(), 2, "先按估算交一版，量算补齐后再交精确帧");
        assert_ne!(bench.frames()[0], bench.frames()[1], "精确帧与估算帧不同");
        assert_eq!(
            bench.requests(),
            vec![
                method::query::THEME.to_string(),
                "submit_frame".to_string(),
                method::query::MEASURE.to_string(),
                "submit_frame".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn measurement_misses_are_batched_once_and_then_cached() {
        let mut bench = Bench::new(200.0, 160.0).await;
        bench.render(|ui| ui.text("中文说明", TextRole::Label)).await.unwrap();
        assert_eq!(
            bench.requests(),
            vec![
                method::query::THEME.to_string(),
                "submit_frame".to_string(),
                method::query::MEASURE.to_string(),
            ],
            "估算帧先交，量算补齐后画面不变就不再交"
        );
        let measures = bench.requests().iter().filter(|m| **m == method::query::MEASURE).count();
        bench.render(|ui| ui.text("中文说明", TextRole::Label)).await.unwrap();
        assert_eq!(
            bench.requests().iter().filter(|m| **m == method::query::MEASURE).count(),
            measures,
            "同文字不再量算"
        );
    }

    /// 换行文字的高度曾在改宽时按新宽度重新量算：一屏日志每拖一像素就是几百条量算，
    /// 宿主逐条同步排版会把 UI 线程堵住。改宽后不得再登记量算。
    #[tokio::test]
    async fn resizing_wrapped_text_queues_no_measurements() {
        let mut bench = Bench::new(260.0, 200.0).await;
        let lines: Vec<String> = (0..120)
            .map(|index| format!("[{index:04}] 普通输出行，包含中英文混排 abc 123 与足够触发换行的说明文字"))
            .collect();
        let build = move |ui: &mut build::Ui<'_, '_>| {
            ui.scroll_sized("logs", 180.0, 2.0, false, |list| {
                for line in &lines {
                    list.text_styled(line, TextStyle::new(TextRole::Caption).wrap());
                }
            });
        };
        bench.render(build.clone()).await.unwrap();
        let first = bench.requests().iter().filter(|m| **m == method::query::MEASURE).count();
        assert!(first > 0, "首帧应登记量算");

        for width in [260.0_f32, 259.0, 258.0, 200.0, 320.0] {
            bench.event(FrameInput::Resize { width, height: 200.0 }).await;
            bench.render(build.clone()).await.unwrap();
            assert_eq!(
                bench.requests().iter().filter(|m| **m == method::query::MEASURE).count(),
                first,
                "改宽到 {width} 后不得再登记量算"
            );
        }
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
            .event(FrameInput::Wheel { x: 100.0, y: 60.0, delta_x: 0.0, delta_y: 2.0, modifiers: Default::default(), hit: None })
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
            .event(FrameInput::Wheel { x: 100.0, y: 300.0, delta_x: 0.0, delta_y: 40.0, modifiers: Default::default(), hit: None })
            .await;
        assert!(
            matches!(events.first(), Some(UiEvent::Scroll { id, .. }) if id == "left"),
            "左栏滚动应命中：{events:?}"
        );

        // 右侧参数栏
        let events = bench
            .event(FrameInput::Wheel { x: 300.0, y: 300.0, delta_x: 0.0, delta_y: 40.0, modifiers: Default::default(), hit: None })
            .await;
        assert!(
            matches!(events.first(), Some(UiEvent::Scroll { id, .. }) if id == "right"),
            "右栏滚动应命中：{events:?}"
        );

        // 底部日志区（固定高度，贴底）
        let events = bench
            .event(FrameInput::Wheel { x: 200.0, y: 580.0, delta_x: 0.0, delta_y: -40.0, modifiers: Default::default(), hit: None })
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
        let submitted = bench.frames().len();
        bench.render(build).await.unwrap();
        assert_eq!(bench.frames().len(), submitted, "同内容帧不得重复提交");

        bench.render(|ui| ui.button("go", "停止")).await.unwrap();
        assert_eq!(bench.frames().len(), submitted + 1, "内容变了仍要提交");

        // 重挂后宿主手里未必还有上一帧：尺寸输入到达即让去重失效
        bench.event(FrameInput::Resize { width: 200.0, height: 120.0 }).await;
        bench.render(|ui| ui.button("go", "停止")).await.unwrap();
        assert_eq!(bench.frames().len(), submitted + 2, "重挂后同内容帧不得被吞");
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
        assert!(bench.frames().is_empty(), "非法帧不提交");
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
            .event(FrameInput::Wheel { x: 100.0, y: 60.0, delta_x: 0.0, delta_y: 80.0, modifiers: Default::default(), hit: None })
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
            .event(FrameInput::Wheel { x: 120.0, y: 100.0, delta_x: 0.0, delta_y: 40.0, modifiers: Default::default(), hit: None })
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

    /// 解析在控件层本地完成：首帧就有文字，且不向宿主发任何解析请求。
    #[tokio::test]
    async fn markdown_is_laid_out_locally_and_painted_in_the_same_frame() {
        let mut bench = Bench::new(240.0, 160.0).await;
        let frame = bench.render(|ui| ui.markdown("# 标题")).await.unwrap();
        assert!(
            frame.commands.iter().any(|command| matches!(
                command,
                DrawCommand::Text { content, .. } if content == "标题"
            )),
            "首帧即出正文：{:?}",
            texts(&frame)
        );
        bench.render(|ui| ui.markdown("# 标题")).await.unwrap();
        assert!(
            bench.requests().iter().all(|method| !method.starts_with("markdown.")),
            "本地解析不该再有宿主往返：{:?}",
            bench.requests
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

    /// 首个尺寸到达之前也必须出帧：宿主只在有帧时才创建绘制部件，而部件正是上报尺寸的一端，
    /// 在这里不出帧就是两头互等（面板恒不加载）。
    #[tokio::test]
    async fn render_before_the_first_resize_still_submits_a_frame() {
        let mut bench = Bench::without_size().await;
        bench.render(|ui| ui.text("hi", TextRole::Body)).await.unwrap();
        let frame = bench.frames().last().expect("应提交一帧").clone();
        assert_eq!(
            frame.size,
            Some(crate::protocol::frame::FrameSize { w: 320.0, h: 400.0 }),
            "按缺省占位尺寸排并如实声明"
        );
    }

    #[tokio::test]
    async fn undo_history_restores_the_previous_value() {
        let mut bench = Bench::new(240.0, 120.0).await;
        bench.render(|ui| ui.text_input("host", "", "")).await.unwrap();
        bench.set_value("host", "");
        bench
            .event(FrameInput::PointerDown {
                x: 20.0,
                y: 10.0,
                button: MouseButton::Left,
                hit: Some("host".into()),
            })
            .await;

        let events = bench.event(typed("a")).await;
        assert_eq!(events, vec![UiEvent::Text { id: "host".into(), value: "a".into() }]);
        bench.set_value("host", "a");
        let events = bench.event(typed("b")).await;
        assert_eq!(events, vec![UiEvent::Text { id: "host".into(), value: "ab".into() }]);
        bench.set_value("host", "ab");

        let control = Modifiers { control: true, ..Modifiers::default() };
        let undo = FrameInput::Key {
            key: Key::Character { text: "z".into() },
            action: KeyAction::Press,
            modifiers: control,
            text: None,
        };
        assert_eq!(
            bench.event(undo.clone()).await,
            vec![UiEvent::Text { id: "host".into(), value: "a".into() }],
            "撤销回上一笔"
        );
        bench.set_value("host", "a");
        assert_eq!(
            bench.event(undo.clone()).await,
            vec![UiEvent::Text { id: "host".into(), value: String::new() }],
            "再撤销回空"
        );
        bench.set_value("host", "");
        let redo = FrameInput::Key {
            key: Key::Character { text: "y".into() },
            action: KeyAction::Press,
            modifiers: control,
            text: None,
        };
        assert_eq!(
            bench.event(redo).await,
            vec![UiEvent::Text { id: "host".into(), value: "a".into() }],
            "重做前进一笔"
        );
    }

    #[tokio::test]
    async fn double_click_selects_the_word_under_the_pointer() {
        let mut bench = Bench::new(240.0, 120.0).await;
        bench.render(|ui| ui.text_input("host", "alpha beta", "")).await.unwrap();
        bench.set_value("host", "alpha beta");
        bench
            .event(FrameInput::PointerDown {
                x: 20.0,
                y: 10.0,
                button: MouseButton::Left,
                hit: Some("host".into()),
            })
            .await;
        // 落在第一个词上：双击只选中该词
        let events = bench
            .event(FrameInput::DoubleClick {
                x: 12.0,
                y: 10.0,
                button: MouseButton::Left,
                hit: Some("host".into()),
            })
            .await;
        assert!(events.iter().any(|event| matches!(event, UiEvent::DoubleClick { .. })));
        bench.set_value("host", "alpha beta");
        assert_eq!(
            bench.event(key(NamedKey::Backspace)).await,
            vec![UiEvent::Text { id: "host".into(), value: " beta".into() }],
            "双击选中的是词，不是一个字符"
        );
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
                    shadow: None,
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
    async fn custom_sub_area_drag_reports_deltas_and_suppresses_click() {
        let mut bench = Bench::new(200.0, 120.0).await;
        bench
            .render(|ui| {
                ui.canvas(40.0, |c| {
                    let area = c.area();
                    c.hit("strip", Rect::new(area.x, area.y, area.w, area.h), HitCursor::Grab);
                });
            })
            .await
            .unwrap();
        let down = FrameInput::PointerDown { x: 20.0, y: 10.0, button: MouseButton::Left, hit: Some("strip".into()) };
        assert!(bench.event(down.clone()).await.is_empty(), "按下不起拖");

        let events = bench
            .event(FrameInput::PointerMove { x: 22.0, y: 10.0, hit: Some("strip".into()) })
            .await;
        assert!(events.is_empty(), "位移未过阈值仍按点击解释：{events:?}");

        let events = bench
            .event(FrameInput::PointerMove { x: 60.0, y: 10.0, hit: None })
            .await;
        assert_eq!(
            events,
            vec![
                UiEvent::DragStart { id: "strip".into() },
                UiEvent::Drag { id: "strip".into(), dx: 40.0, dy: 0.0 },
            ],
            "拖出子区也继续上报累计位移"
        );

        let events = bench
            .event(FrameInput::PointerUp { x: 60.0, y: 14.0, button: MouseButton::Left, hit: None })
            .await;
        assert_eq!(
            events,
            vec![UiEvent::DragEnd { id: "strip".into(), dx: 40.0, dy: 4.0 }],
            "起拖后的抬起只报结束，不再算点击"
        );

        assert!(bench.event(down).await.is_empty());
        assert!(bench
            .event(FrameInput::PointerMove { x: 23.0, y: 10.0, hit: Some("strip".into()) })
            .await
            .is_empty());
        let events = bench
            .event(FrameInput::PointerUp { x: 23.0, y: 10.0, button: MouseButton::Left, hit: Some("strip".into()) })
            .await;
        assert_eq!(events, vec![UiEvent::Click { id: "strip".into() }], "未过阈值仍是点击");
    }

    #[tokio::test]
    async fn pointer_sub_area_jitter_stays_a_click() {
        let mut bench = Bench::new(200.0, 120.0).await;
        bench
            .render(|ui| {
                ui.canvas(40.0, |c| {
                    let area = c.area();
                    c.hit("cell", Rect::new(area.x, area.y, area.w, area.h), HitCursor::Pointer);
                });
            })
            .await
            .unwrap();
        bench
            .event(FrameInput::PointerDown { x: 20.0, y: 10.0, button: MouseButton::Left, hit: Some("cell".into()) })
            .await;
        let events = bench
            .event(FrameInput::PointerMove { x: 26.0, y: 10.0, hit: Some("cell".into()) })
            .await;
        assert!(events.is_empty(), "非拖动光标不上报拖拽：{events:?}");
        let events = bench
            .event(FrameInput::PointerUp { x: 26.0, y: 10.0, button: MouseButton::Left, hit: Some("cell".into()) })
            .await;
        assert_eq!(events, vec![UiEvent::Click { id: "cell".into() }], "普通点选区的小抖动仍算点击");
    }

    #[tokio::test]
    async fn escape_cancels_a_started_drag_then_an_input_edit() {
        let mut bench = Bench::new(200.0, 120.0).await;
        bench
            .render(|ui| {
                ui.canvas(40.0, |c| {
                    let area = c.area();
                    c.hit("strip", Rect::new(area.x, area.y, area.w, area.h), HitCursor::Grab);
                });
            })
            .await
            .unwrap();
        bench
            .event(FrameInput::PointerDown { x: 20.0, y: 10.0, button: MouseButton::Left, hit: Some("strip".into()) })
            .await;
        bench
            .event(FrameInput::PointerMove { x: 80.0, y: 10.0, hit: None })
            .await;

        let events = bench.event(key(NamedKey::Escape)).await;
        assert_eq!(events, vec![UiEvent::DragCancel { id: "strip".into() }]);
        let events = bench
            .event(FrameInput::PointerUp { x: 80.0, y: 10.0, button: MouseButton::Left, hit: None })
            .await;
        assert!(events.is_empty(), "取消后的抬起不再报拖拽：{events:?}");

        bench.render(|ui| ui.text_input("q", "abc", "")).await.unwrap();
        bench
            .event(FrameInput::PointerDown { x: 20.0, y: 10.0, button: MouseButton::Left, hit: Some("q".into()) })
            .await;
        let events = bench.event(key(NamedKey::Escape)).await;
        assert_eq!(
            events,
            vec![UiEvent::Cancel { id: "q".into() }, UiEvent::Focus { id: "q".into(), focused: false }],
            "Esc 先报取消再离焦，插件据此放弃草稿"
        );
    }

    #[tokio::test]
    async fn image_declares_a_tile_with_the_requested_fit() {
        let mut bench = Bench::new(200.0, 120.0).await;
        let frame = bench
            .render(|ui| ui.image_fit(ImageSource::Vault { path: "a.png".into(), max_edge: None }, 80.0, ImageFit::Cover))
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

    #[tokio::test]
    async fn scroll_max_hugs_short_content_and_caps_tall_content() {
        let mut bench = Bench::new(300.0, 300.0).await;
        let frame = bench
            .render(|ui| {
                ui.column(0.0, |c| {
                    c.scroll_max("short", 120.0, 0.0, |s| s.text("单行", TextRole::Label));
                    c.button("after-short", "后");
                });
            })
            .await
            .unwrap();
        assert!(!has_hit(&frame, "short::bar"), "短内容不出滚动条");
        let short_y = frame.hits.iter().find(|hit| hit.id == "after-short").expect("后续控件").y;
        assert!(short_y < 40.0, "短内容按自身高度收住：{short_y}");

        let mut bench = Bench::new(300.0, 300.0).await;
        let frame = bench
            .render(|ui| {
                ui.column(0.0, |c| {
                    c.scroll_max("tall", 120.0, 0.0, |s| {
                        for index in 0..30 {
                            s.text(format!("行{index}"), TextRole::Label);
                        }
                    });
                    c.button("after-tall", "后");
                });
            })
            .await
            .unwrap();
        assert!(has_hit(&frame, "tall::bar"), "超过限高出现滚动条");
        let tall_y = frame.hits.iter().find(|hit| hit.id == "after-tall").expect("后续控件").y;
        assert!((tall_y - 120.0).abs() < 2.0, "超过限高按限高收住：{tall_y}");
    }

    #[tokio::test]
    async fn fill_spacer_centers_the_column_content() {
        let mut bench = Bench::new(200.0, 200.0).await;
        let frame = bench
            .render(|ui| {
                ui.fill_column(0.0, |c| {
                    c.fill_spacer();
                    c.text("居中", TextRole::Label);
                    c.fill_spacer();
                });
            })
            .await
            .unwrap();
        let y = frame
            .commands
            .iter()
            .find_map(|command| match command {
                DrawCommand::Text { y, .. } => Some(*y),
                _ => None,
            })
            .expect("居中文字");
        let line_height = 13.0 * 1.2;
        assert!((y - (200.0 - line_height) / 2.0).abs() < 1.5, "上下弹簧均分剩余高度：{y}");
    }

    #[tokio::test]
    async fn text_style_weight_overrides_the_role_default() {
        let mut bench = Bench::new(200.0, 80.0).await;
        let frame = bench
            .render(|ui| {
                ui.text_styled(
                    "加粗",
                    TextStyle::new(TextRole::Body).weight(TextWeight::SemiBold),
                );
            })
            .await
            .unwrap();
        assert!(matches!(
            frame.commands.first(),
            Some(DrawCommand::Text { weight: Some(TextWeight::SemiBold), .. })
        ));
    }

    #[tokio::test]
    async fn passive_overlay_does_not_block_the_wheel() {
        fn build(ui: &mut build::Ui<'_, '_>, passive: bool) {
            ui.scroll_sized("list", 60.0, 0.0, false, |s| {
                for index in 0..10 {
                    s.button(format!("row{index}"), format!("行{index}"));
                }
            });
            if passive {
                ui.overlay_passive(Rect::new(0.0, 0.0, 200.0, 160.0), |layer| {
                    layer.hit_area("badge");
                });
            } else {
                ui.overlay(Rect::new(0.0, 0.0, 200.0, 160.0), |layer| {
                    layer.hit_area("popup");
                });
            }
        }
        let mut bench = Bench::new(200.0, 160.0).await;
        bench.render(|ui| build(ui, true)).await.unwrap();
        let events = bench
            .event(FrameInput::Wheel { x: 100.0, y: 30.0, delta_x: 0.0, delta_y: 40.0, modifiers: Default::default(), hit: None })
            .await;
        assert!(
            matches!(events.as_slice(), [UiEvent::Scroll { id, .. }] if id == "list"),
            "被动浮层不拦滚轮：{events:?}"
        );

        let mut bench = Bench::new(200.0, 160.0).await;
        bench.render(|ui| build(ui, false)).await.unwrap();
        let events = bench
            .event(FrameInput::Wheel { x: 100.0, y: 30.0, delta_x: 0.0, delta_y: 40.0, modifiers: Default::default(), hit: None })
            .await;
        assert!(events.is_empty(), "阻挡浮层期间滚轮不落到下层：{events:?}");
    }

    #[tokio::test]
    async fn refresh_theme_keeps_the_reported_size() {
        let bench = Bench::new(320.0, 240.0).await;
        assert_eq!(bench.ui.lock().await.size(), (320.0, 240.0));
        bench.ui.lock().await.refresh_theme();
        assert_eq!(bench.ui.lock().await.size(), (320.0, 240.0), "重挂不清尺寸");
    }

    #[tokio::test]
    async fn boxed_can_paint_a_shadow() {
        let mut bench = Bench::new(200.0, 100.0).await;
        let frame = bench
            .render(|ui| {
                ui.boxed(
                    BoxWidth::Fixed(120.0),
                    BoxStyle::new(Some(ColorToken::Surface), None, 10.0).shadow(
                        atelyx_plugin_protocol::paint::PaintShadow {
                            offset_x: 0.0,
                            offset_y: 6.0,
                            blur: 18.0,
                            color: PaintColor::Rgba { rgba: [0, 0, 0, 90] },
                        },
                    ),
                    Edges::all(8.0),
                    0.0,
                    |body| body.text("浮层", TextRole::Label),
                );
            })
            .await
            .unwrap();
        assert!(matches!(
            frame.commands.first(),
            Some(DrawCommand::Rect { shadow: Some(_), .. })
        ));
    }

    #[tokio::test]
    async fn spinner_rotates_on_the_frame_clock_and_asks_for_ticks() {
        let mut bench = Bench::new(100.0, 60.0).await;
        let frame = bench.render(|ui| ui.spinner(20.0, ColorToken::TextMuted)).await.unwrap();
        assert!(matches!(
            frame.commands.first(),
            Some(DrawCommand::Path { points, stroke: Some(_), .. }) if points.len() == 25
        ));
        assert_eq!(frame.fps, Some(TICK_HZ), "自转的转圈要自己声明帧拍");
        let first = arc_start(&frame);
        bench.ui.lock().await.tick();
        let frame = bench.render(|ui| ui.spinner(20.0, ColorToken::TextMuted)).await.unwrap();
        assert_ne!(first, arc_start(&frame), "每拍转一格");
    }

    /// 转圈指令的起点（相位变化的直接体现）。
    fn arc_start(frame: &SurfaceFrame) -> (f32, f32) {
        match frame.commands.first() {
            Some(DrawCommand::Path { points, .. }) => (points[0].x, points[0].y),
            other => panic!("首个指令应为圆弧路径：{other:?}"),
        }
    }
}
