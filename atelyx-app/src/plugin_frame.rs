//! 插件整面帧部件：执行帧的绘制指令清单，按命中区回发原始指针 / 键盘 / 输入法输入。
//! 坐标一律面本地逻辑像素；形状与图片按绘制段缓存几何，文字按段落缓存跨帧复用。

use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::advanced::clipboard::Clipboard;
use iced::advanced::input_method::{Event as ImeEvent, InputMethod, Purpose};
use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{self, Widget};
use iced::advanced::graphics::geometry::Renderer as _;
use iced::advanced::{Renderer as _, Shell};
use iced::event::Event;
use iced::keyboard;
use iced::keyboard::key::Named;
use iced::mouse;
use iced::widget::{canvas, container};
use iced::window;
use iced::{Element, Length, Padding, Point, Rectangle, Size, Vector};

use atelyx_core::plugin_runtime::{
    FrameInput, FrameRect, HitCursor, Key as FrameKey, KeyAction, Modifiers as FrameModifiers,
    MouseButton, NamedKey, PopupLayer, SurfaceFrame, SurfacePopup,
};

use crate::plugin_paint::{self, PaintCtx};
use crate::{App, Message};

/// 双击判定：同键二次按下的最大间隔。
pub(crate) const DOUBLE_CLICK_MS: u64 = 500;
/// 双击判定：两次按下间的最大位移（逻辑像素；拖动不算双击）。
pub(crate) const DOUBLE_CLICK_SLOP: f32 = 4.0;
/// 改宽那一拍等插件新帧的下限与上限：实际预算按插件的回帧延迟自适应（见 `FrameState::wait_budget`）。
/// 下限保证首次改宽也能等到一张按新宽度排好的帧；上限保证慢插件不会把 UI 线程压在这里。
const RESIZE_WAIT_MIN: Duration = Duration::from_millis(4);
const RESIZE_WAIT_MAX: Duration = Duration::from_millis(24);
/// 未测得过回帧延迟时的初始预算（改宽第一拍用）。
const RESIZE_WAIT_FIRST: Duration = Duration::from_millis(12);
/// 尺寸上报的抖动阈值（逻辑像素）：不足半像素的变化不值得让插件整面重排。
const RESIZE_STEP: f32 = 0.5;
/// 帧声明的排版尺寸与当前面尺寸的容许差：两端各自取整，1px 内的差不算换尺寸。
const SIZE_TOLERANCE: f32 = 0.5;

/// 标识哈希（FNV-1a；面 / 面 id 的身份判定用，不参与协议）。
fn hash_id(value: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// 面身份哈希（FNV 混合；仅用于判定「同一个面的尺寸是否变过」，不参与协议）。
fn surface_identity(plugin_id: &str, surface_id: &str) -> u64 {
    hash_id(plugin_id) ^ hash_id(surface_id).rotate_left(17)
}

/// 两点距离平方（双击位移阈值比较用，免开方）。
pub(crate) fn distance_sq(a: Point, b: Point) -> f32 {
    let dx = a.x - b.x;
    let dy = a.y - b.y;
    dx * dx + dy * dy
}

/// 拖拽中的面本地坐标：光标在窗口内即可换算（允许拖出面采点，抬起为止）。
fn drag_point(cursor: mouse::Cursor, bounds: Rectangle) -> Option<Point> {
    let absolute = cursor.position()?;
    Some(Point::new(absolute.x - bounds.x, absolute.y - bounds.y))
}

/// 指针按钮 → 协议按钮（协议没有的侧键不转发）。
pub(crate) fn mouse_button(button: mouse::Button) -> Option<MouseButton> {
    match button {
        mouse::Button::Left => Some(MouseButton::Left),
        mouse::Button::Right => Some(MouseButton::Right),
        mouse::Button::Middle => Some(MouseButton::Middle),
        _ => None,
    }
}

/// 部件跨帧状态（存 `tree::State`）：绘制缓存 + 拖拽 / 悬停 / 双击判定 + 输入法开关。
struct FrameState {
    /// 形状段缓存：按绘制步骤下标各持一块，换帧时只作废内容变了的段。
    caches: RefCell<Vec<canvas::Cache>>,
    /// 已排版的文字段落：内容不变跨帧复用，改宽 / 滚动 / 悬停不重新成形。
    texts: RefCell<plugin_paint::TextCache>,
    /// 面板内浮层的绘制缓存（菜单 / 灯箱按需重画）。
    popup: PopupPaint,
    /// 上次绘制的步骤表（与 `painted` 同帧）：换帧时按段比对，只作废内容变了的几何缓存。
    steps: RefCell<Vec<plugin_paint::PaintStep>>,
    /// 下一帧的步骤表缓冲：与 `steps` 交换使用，换帧不分配新 Vec。
    next_steps: RefCell<Vec<plugin_paint::PaintStep>>,
    /// 上次绘制的帧：按 Arc 身份判换帧。持有引用，帧对象地址不会被分配器复用。
    painted: RefCell<Option<Arc<SurfaceFrame>>>,
    /// 上次绘制用的界面字体族变更计数。
    font_epoch: Cell<u64>,
    /// 上次绘制用的界面图片句柄代数。
    image_epoch: Cell<u64>,
    /// 上次上报给插件的面身份与尺寸（逻辑像素）；变化即回发 `Resize`。
    reported: Cell<Option<(u64, f32, f32)>>,
    /// 插件回帧延迟的指数平均（微秒）：等帧预算按它自适应，慢插件不会每拍都白等满上限。
    frame_latency: Cell<Option<u32>>,
    /// 连续等帧超时次数：到阈值即认定这个面慢，不再逐拍追新帧。
    slow: Cell<u8>,
    /// 本部件当前承载的面身份：变化即说明换了面，指针瞬态一律作废。
    identity: Cell<u64>,
    /// 按下中的键（`None` = 未按下）：抬笔只认这一个键，中途混按其它键不会错发 `PointerUp`。
    pressed: Cell<Option<MouseButton>>,
    /// 悬停中：Leave 按「从内到外」边沿触发，不逐帧重发。
    inside: Cell<bool>,
    /// 最近一次指针位置（面本地）：窗口失焦时按它补发 `PointerUp`，插件不会卡在拖拽态。
    last_point: Cell<Option<Point>>,
    last_press: RefCell<Option<(MouseButton, Instant, Point)>>,
    /// 最近一次键盘事件的修饰键快照（滚轮事件本身不带修饰键）。
    mods: Cell<FrameModifiers>,
    /// 已请求启用输入法（失去焦点或帧不再声明 `ime` 时请求停用一次）。
    ime_on: Cell<bool>,
    /// 上次上报的面左上角窗口坐标；窗口级浮层按它平移面本地坐标。
    origin: Cell<Option<Point>>,
}

/// 面级浮层的绘制缓存：几何分段、文字段落与上一帧内容。
#[derive(Default)]
pub(crate) struct PopupPaint {
    caches: RefCell<Vec<canvas::Cache>>,
    texts: RefCell<plugin_paint::TextCache>,
    /// 上次绘制过的浮层：内容一变即作废几何缓存（尺寸相同也不会画旧形状）。
    painted: RefCell<Option<SurfacePopup>>,
    /// 浮层步骤表缓冲（换帧不分配新 Vec）。
    steps: RefCell<Vec<plugin_paint::PaintStep>>,
}

impl PopupPaint {
    fn clear(&self) {
        self.caches.borrow_mut().iter().for_each(canvas::Cache::clear);
        self.texts.borrow_mut().clear();
        *self.painted.borrow_mut() = None;
        self.steps.borrow_mut().clear();
    }

    /// 绘制浮层：`origin` 是面左上角的窗口坐标，`canvas_size` 是几何缓存的口径，
    /// `clip` 是窗口裁剪矩形（浮层可越出面矩形，含盖住面板头部）。
    pub(crate) fn draw(
        &self,
        popup: &SurfacePopup,
        origin: Point,
        canvas_size: Size,
        clip: Rectangle,
        renderer: &mut iced::Renderer,
        ctx: &PaintCtx<'_>,
    ) {
        let area = Rectangle::new(
            Point::new(origin.x + popup.rect.x, origin.y + popup.rect.y),
            Size::new(popup.rect.w.max(0.0), popup.rect.h.max(0.0)),
        );
        // 可见性看浮层矩形，裁剪仍按窗口：阴影与越出浮层矩形的描边不能被矩形自身裁掉
        if !area.intersects(&clip) {
            return;
        }
        let changed = self
            .painted
            .borrow()
            .as_ref()
            .is_none_or(|painted| painted != popup);
        if changed {
            self.caches.borrow_mut().iter().for_each(canvas::Cache::clear);
            *self.painted.borrow_mut() = Some(popup.clone());
        }
        let mut steps = self.steps.borrow_mut();
        plugin_paint::plan_into(&popup.commands, &mut steps);
        let mut texts = self.texts.borrow_mut();
        texts.begin_frame();
        renderer.with_layer(clip, |renderer| {
            let mut caches = self.caches.borrow_mut();
            if caches.len() < steps.len() {
                caches.resize_with(steps.len(), canvas::Cache::new);
            }
            let mut layer_open = false;
            let mut max_rank = 0u8;
            for (slot, step) in steps.iter().enumerate() {
                let rank = plugin_paint::step_rank(step);
                if !layer_open || rank < max_rank {
                    if layer_open {
                        renderer.end_layer();
                    }
                    renderer.start_layer(clip);
                    layer_open = true;
                    max_rank = rank;
                } else {
                    max_rank = max_rank.max(rank);
                }
                match step {
                    plugin_paint::PaintStep::Shapes(range) => {
                        let geometry = caches[slot].draw(renderer, canvas_size, |canvas| {
                            plugin_paint::execute_shapes(canvas, &popup.commands, range.clone(), ctx);
                        });
                        renderer.with_translation(Vector::new(origin.x, origin.y), |renderer| {
                            renderer.draw_geometry(geometry);
                        });
                    }
                    plugin_paint::PaintStep::Image(index) => {
                        let geometry = caches[slot].draw(renderer, canvas_size, |canvas| {
                            plugin_paint::execute_shapes(
                                canvas,
                                &popup.commands,
                                *index..*index + 1,
                                ctx,
                            );
                        });
                        renderer.with_translation(Vector::new(origin.x, origin.y), |renderer| {
                            renderer.draw_geometry(geometry);
                        });
                    }
                    plugin_paint::PaintStep::Text(index) => {
                        texts.draw_one(renderer, &popup.commands[*index], origin, clip);
                    }
                    plugin_paint::PaintStep::Shadow(index) => {
                        plugin_paint::draw_shadow(
                            renderer,
                            Rectangle::new(origin, canvas_size),
                            &popup.commands[*index],
                        );
                    }
                }
            }
            if layer_open {
                renderer.end_layer();
            }
        });
        texts.end_frame();
    }
}

impl Default for FrameState {
    fn default() -> Self {
        Self {
            caches: RefCell::new(Vec::new()),
            texts: RefCell::new(plugin_paint::TextCache::default()),
            popup: PopupPaint::default(),
            steps: RefCell::new(Vec::new()),
            next_steps: RefCell::new(Vec::new()),
            painted: RefCell::new(None),
            font_epoch: Cell::new(0),
            image_epoch: Cell::new(0),
            reported: Cell::new(None),
            frame_latency: Cell::new(None),
            slow: Cell::new(0),
            identity: Cell::new(0),
            pressed: Cell::new(None),
            inside: Cell::new(false),
            last_point: Cell::new(None),
            last_press: RefCell::new(None),
            mods: Cell::new(FrameModifiers::default()),
            ime_on: Cell::new(false),
            origin: Cell::new(None),
        }
    }
}

impl FrameState {
    /// 这一拍等插件新帧的预算：按插件的实际回帧延迟自适应。
    /// 连续超时说明这个面本来就慢，退回下限——每拍白等满上限比「画面晚一拍」更伤手感。
    fn wait_budget(&self) -> Duration {
        if self.slow.get() >= 2 {
            return RESIZE_WAIT_MIN;
        }
        match self.frame_latency.get() {
            Some(latency) => {
                let budget = latency.saturating_mul(2) + 1_000;
                Duration::from_micros(u64::from(budget))
                    .clamp(RESIZE_WAIT_MIN, RESIZE_WAIT_MAX)
            }
            None => RESIZE_WAIT_FIRST,
        }
    }

    /// 记一次成功等到新帧：更新延迟指数平均并解除「慢面」判定。
    fn observe_latency(&self, elapsed: Duration) {
        let sample = u32::try_from(elapsed.as_micros()).unwrap_or(u32::MAX);
        let next = match self.frame_latency.get() {
            Some(latency) => (latency / 4) * 3 + sample / 4,
            None => sample,
        };
        self.frame_latency.set(Some(next));
        self.slow.set(0);
    }

    /// 记一次等帧超时：连续超时到阈值后不再追这个面的新帧。
    fn observe_timeout(&self) {
        self.slow.set(self.slow.get().saturating_add(1));
    }
}

/// 插件整面帧。
pub struct PluginFrame<'a> {
    pub app: &'a App,
    pub window: window::Id,
    pub plugin_id: String,
    pub surface_id: String,
    pub frame: &'a Arc<SurfaceFrame>,
    /// 本面当前是否持有键盘焦点（渲染期由壳侧焦点槽投影）。
    pub focused: bool,
}

impl PluginFrame<'_> {
    fn identity(&self) -> u64 {
        surface_identity(&self.plugin_id, &self.surface_id)
    }

    /// 命中的基准是实际画在屏幕上的那一帧：改宽期间投影缓存可能还没追上，
    /// 但用户点的是眼前看到的画面。
    fn hit(&self, state: &FrameState, point: Point) -> Option<String> {
        let painted = state.painted.borrow();
        let frame = painted.as_deref().unwrap_or(self.frame.as_ref());
        // 面板内浮层画在主帧之上，命中优先取它自己的命中区（同用面本地坐标）
        // 窗口级浮层由根层接收输入，不在这里重复命中
        if let Some(popup) = frame.popup.as_ref().filter(|p| p.layer == PopupLayer::Panel) {
            if let Some(hit) = popup.hit_region_at(point.x, point.y) {
                return Some(hit.id.clone());
            }
        }
        frame.hit_region_at(point.x, point.y).map(|hit| hit.id.clone())
    }

    /// 输入法候选窗的锚点：浮层自带的 `ime` 优先（浮层内就地输入），否则用主帧的。
    fn ime_rect(&self) -> Option<FrameRect> {
        self.frame
            .popup
            .as_ref()
            .and_then(|popup| popup.ime)
            .or(self.frame.ime)
    }

    /// 回发一条原始输入（坐标已换算到面本地）。
    fn send(&self, input: FrameInput) {
        self.app
            .plugins
            .notify_frame_input(&self.plugin_id, &self.surface_id, &input);
    }

    fn focus_message(&self, focused: bool) -> Message {
        Message::PluginFrameFocus {
            window: self.window,
            plugin_id: self.plugin_id.to_string(),
            surface_id: self.surface_id.to_string(),
            focused,
        }
    }

    /// 指针移动是否要上报（按下中恒上报；悬停中仅在面内上报）。
    fn pointer_move(&self, state: &FrameState, point: Point) {
        self.send(FrameInput::PointerMove {
            x: point.x,
            y: point.y,
            hit: self.hit(state, point),
        });
    }
}

impl Widget<Message, iced::Theme, iced::Renderer> for PluginFrame<'_> {
    fn tag(&self) -> widget::tree::Tag {
        widget::tree::Tag::of::<FrameState>()
    }

    fn state(&self) -> widget::tree::State {
        widget::tree::State::new(FrameState::default())
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn layout(
        &mut self,
        _tree: &mut widget::Tree,
        _renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::sized(limits, Length::Fill, Length::Fill, |limits| limits.max())
    }

    fn update(
        &mut self,
        tree: &mut widget::Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &iced::Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let state = tree.state.downcast_mut::<FrameState>();
        // 换了面（同槽位换了插件面）即作废指针瞬态与绘制基准：否则上一面的按下 / 悬停 /
        // 命中区会带到新面
        if state.identity.get() != self.identity() {
            state.identity.set(self.identity());
            state.origin.set(None);
            state.popup.clear();
            state.pressed.set(None);
            state.inside.set(false);
            state.last_point.set(None);
            *state.last_press.borrow_mut() = None;
            state.painted.borrow_mut().take();
            state.caches.borrow().iter().for_each(canvas::Cache::clear);
            state.texts.borrow_mut().clear();
            state.steps.borrow_mut().clear();
            state.next_steps.borrow_mut().clear();
            state.frame_latency.set(None);
            state.slow.set(0);
        }
        // 窗口级浮层按面原点平移；位置变化才上报（布局稳定时不产生额外消息）
        if state.origin.get() != Some(bounds.position()) {
            state.origin.set(Some(bounds.position()));
            shell.publish(Message::PluginSurfaceOrigin {
                window: self.window,
                plugin_id: self.plugin_id.clone(),
                surface_id: self.surface_id.clone(),
                origin: bounds.position(),
            });
        }
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(button)) => {
                let Some(point) = cursor.position_in(bounds) else {
                    // 面外按下即失焦；拖拽中又在面外按下另一键 = 本次拖拽被打断，补发抬起。
                    // 释放不受 `is_event_captured` 影响：点到宿主输入框时那次按下已先被对方截住，
                    // 若不在这时放掉，键盘会留在帧上、宿主快捷键继续被吞
                    if let Some(button) = state.pressed.get() {
                        state.pressed.set(None);
                        if let Some(point) = state.last_point.get() {
                            self.send(FrameInput::PointerUp {
                                x: point.x,
                                y: point.y,
                                button,
                                hit: self.hit(state, point),
                            });
                        }
                    }
                    state.inside.set(false);
                    if self.focused {
                        shell.publish(self.focus_message(false));
                        shell.request_redraw();
                    }
                    return;
                };
                // 面内按下已被别的部件（如浮层）处理时不再重复上报
                if shell.is_event_captured() {
                    return;
                }
                if state.pressed.get().is_some() {
                    return;
                }
                let Some(mouse_button) = mouse_button(*button) else {
                    return;
                };
                let now = Instant::now();
                let mut last = state.last_press.borrow_mut();
                let double_click = last.is_some_and(|(last_button, at, last_point)| {
                    last_button == mouse_button
                        && now.duration_since(at).as_millis() < u128::from(DOUBLE_CLICK_MS)
                        && distance_sq(point, last_point) < DOUBLE_CLICK_SLOP * DOUBLE_CLICK_SLOP
                });
                *last = if double_click { None } else { Some((mouse_button, now, point)) };
                drop(last);
                state.pressed.set(Some(mouse_button));
                state.inside.set(true);
                state.last_point.set(Some(point));
                // 按下即取焦：插件往往要等这次点击处理完才在下一帧声明 `keys` / `ime`，
                // 若等声明才取焦，用户得点第二次才拿得到键盘。按键与输入法仍按帧声明转发。
                if !self.focused {
                    self.send(FrameInput::Focus { focused: true });
                    shell.publish(self.focus_message(true));
                    // 聚焦后须再绘一帧：输入法在绘制期启用
                    shell.request_redraw();
                }
                let hit = self.hit(state, point);
                let input = if double_click {
                    FrameInput::DoubleClick { x: point.x, y: point.y, button: mouse_button, hit }
                } else {
                    FrameInput::PointerDown { x: point.x, y: point.y, button: mouse_button, hit }
                };
                self.send(input);
                shell.capture_event();
            }
            Event::Mouse(mouse::Event::ButtonReleased(button)) => {
                let Some(mouse_button) = mouse_button(*button) else {
                    return;
                };
                // 只认按下时那个键的抬起：混按期间别的键抬起不代表拖拽结束
                if state.pressed.get() != Some(mouse_button) {
                    return;
                }
                state.pressed.set(None);
                // 拖拽中抬笔可在面外：绝对坐标换算本地点；指针已离开窗口时退回最后已知位置
                let Some(point) = drag_point(cursor, bounds).or(state.last_point.get()) else {
                    return;
                };
                state.last_point.set(Some(point));
                self.send(FrameInput::PointerUp {
                    x: point.x,
                    y: point.y,
                    button: mouse_button,
                    hit: self.hit(state, point),
                });
                shell.capture_event();
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                if state.pressed.get().is_some() {
                    let Some(point) = drag_point(cursor, bounds) else {
                        return;
                    };
                    state.last_point.set(Some(point));
                    self.pointer_move(state, point);
                    shell.capture_event();
                    return;
                }
                // 悬停只在面内发 Move；从内到外边沿发 Leave
                let inside = cursor.position_in(bounds);
                let left = state.inside.get() && inside.is_none();
                state.inside.set(inside.is_some());
                if left {
                    state.last_point.set(None);
                    self.send(FrameInput::PointerLeave);
                    shell.capture_event();
                } else if let Some(point) = inside {
                    state.last_point.set(Some(point));
                    self.pointer_move(state, point);
                }
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                // iced 把全窗口鼠标事件广播给所有 widget：只认光标落在面内的滚轮（防跨面板串扰）
                let Some(point) = cursor.position_in(bounds) else {
                    return;
                };
                let (dx, dy) = match delta {
                    // iced 沿 winit（正 = 向上 / 向左）；协议按 Web 惯例（正 = 向下 / 向右），取反换算。
                    // 行增量按宿主同一口径折算成像素：协议只认像素，插件不必区分设备
                    mouse::ScrollDelta::Lines { x, y } => {
                        (-x * crate::WHEEL_LINE_PX, -y * crate::WHEEL_LINE_PX)
                    }
                    mouse::ScrollDelta::Pixels { x, y } => (-x, -y),
                };
                self.send(FrameInput::Wheel {
                    x: point.x,
                    y: point.y,
                    delta_x: dx,
                    delta_y: dy,
                    modifiers: state.mods.get(),
                    hit: self.hit(state, point),
                });
                // 面消费滚轮：外层滚动容器不再同帧滚动
                shell.capture_event();
            }
            Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, text, .. }) => {
                state.mods.set(frame_modifiers(*modifiers));
                if !self.focused || !self.frame.keys {
                    return;
                }
                let Some(key) = frame_key(key) else {
                    return;
                };
                self.send(FrameInput::Key {
                    key,
                    action: KeyAction::Press,
                    modifiers: frame_modifiers(*modifiers),
                    // 控制字符（Ctrl+字母等组合产出）不是可插入文本，不随按键下发
                    text: text
                        .as_ref()
                        .filter(|text| !text.chars().any(char::is_control))
                        .map(|text| text.to_string()),
                });
                shell.capture_event();
            }
            Event::Keyboard(keyboard::Event::KeyReleased { key, modifiers, .. }) => {
                state.mods.set(frame_modifiers(*modifiers));
                if !self.focused || !self.frame.keys {
                    return;
                }
                let Some(key) = frame_key(key) else {
                    return;
                };
                self.send(FrameInput::Key {
                    key,
                    action: KeyAction::Release,
                    modifiers: frame_modifiers(*modifiers),
                    text: None,
                });
                shell.capture_event();
            }
            Event::InputMethod(ime) => {
                let ime_rect = self.ime_rect();
                if !self.focused || ime_rect.is_none() {
                    return;
                }
                let input = match ime {
                    ImeEvent::Preedit(content, _) => FrameInput::Text {
                        text: content.clone(),
                        composing: true,
                        // 组合串插在文档当前光标处；range 是组合串内部的游标区间，不是文档位置
                        caret: None,
                        candidates: ime_rect,
                    },
                    ImeEvent::Commit(content) => FrameInput::Text {
                        text: content.clone(),
                        composing: false,
                        caret: None,
                        candidates: ime_rect,
                    },
                    // 组合结束且无提交：空串表示放弃组合，不插入文本
                    ImeEvent::Closed => FrameInput::Text {
                        text: String::new(),
                        composing: true,
                        caret: None,
                        candidates: ime_rect,
                    },
                    ImeEvent::Opened => return,
                };
                self.send(input);
                shell.capture_event();
            }
            Event::Window(window::Event::RedrawRequested(_)) => {
                let ime_rect = self.ime_rect();
                let enabled = self.focused && ime_rect.is_some();
                if let Some(ime) = ime_rect.filter(|_| enabled) {
                    let request: InputMethod<&str> = InputMethod::Enabled {
                        cursor: Rectangle::new(
                            bounds.position() + Vector::new(ime.x, ime.y),
                            Size::new(ime.w.max(1.0), ime.h.max(1.0)),
                        ),
                        purpose: Purpose::Normal,
                        preedit: None,
                    };
                    shell.request_input_method(&request);
                    state.ime_on.set(true);
                } else if state.ime_on.get() {
                    let request: InputMethod<&str> = InputMethod::Disabled;
                    shell.request_input_method(&request);
                    state.ime_on.set(false);
                }
            }
            Event::Window(window::Event::Unfocused) => {
                // 窗口失焦后收不到抬起：补发 `PointerUp`，插件不会卡在拖拽态
                if let Some(button) = state.pressed.get() {
                    state.pressed.set(None);
                    if let Some(point) = state.last_point.get() {
                        self.send(FrameInput::PointerUp {
                            x: point.x,
                            y: point.y,
                            button,
                            hit: self.hit(state, point),
                        });
                    }
                }
                if state.inside.replace(false) {
                    self.send(FrameInput::PointerLeave);
                }
            }
            _ => {}
        }
    }

    fn draw(
        &self,
        tree: &widget::Tree,
        renderer: &mut iced::Renderer,
        _theme: &iced::Theme,
        _style: &iced::advanced::renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        if !bounds.intersects(viewport) {
            return;
        }
        let state = tree.state.downcast_ref::<FrameState>();
        let epoch = crate::ui::epoch();
        let image_epoch = self.app.note_image_epoch;
        // 取运行时最新帧：改宽期间投影可能还没追上（那一帧已在绘制期取走、不再发通知），
        // 但这里必须画最新的一张，否则下一拍会退回投影里的旧帧。
        let current = self
            .app
            .plugins
            .surface_frame(&self.plugin_id, &self.surface_id)
            .unwrap_or_else(|| self.frame.clone());
        // 面身份或尺寸变化即上报插件：绘制拿到的是真实分配尺寸，这是插件重排的唯一来源。
        // 插件按新尺寸重排要一个往返才回来；这一拍就等它回来画进去——否则画出的是按上一拍
        // 宽度排好的帧，右对齐项与滚动条会停在旧边缘（拖动时看起来就是「飘」）。
        let mut resized = None;
        // 这一拍等过新帧但没等到：要退回上一张画面（不然整面空着）
        let mut timed_out = false;
        let size = (bounds.width, bounds.height);
        let reported = (self.identity(), bounds.width, bounds.height);
        // 半像素以内的变化不惊动插件：拖动时分割线的小数位抖动会让整面白重排一次
        let changed = state.reported.get().is_none_or(|(identity, width, height)| {
            identity != reported.0
                || (width - reported.1).abs() >= RESIZE_STEP
                || (height - reported.2).abs() >= RESIZE_STEP
        });
        if changed {
            state.reported.set(Some(reported));
            // 等的是「比已画的那一帧更新、且按当前尺寸排好的帧」：投影可能落后，拿它当基准
            // 会立刻返回旧帧；尺寸又过滤掉拖动中残留在路上的上一拍画面
            let seen = state.painted.borrow().clone().unwrap_or_else(|| current.clone());
            // 先登记再发尺寸：登记期间回来的帧由这一拍取走画上，不再另发一条通知
            self.app
                .plugins
                .begin_surface_wait(&self.plugin_id, &self.surface_id, seen);
            self.send(FrameInput::Resize { width: bounds.width, height: bounds.height });
            let started = Instant::now();
            match self.app.plugins.finish_surface_wait(
                &self.plugin_id,
                &self.surface_id,
                size,
                state.wait_budget(),
            ) {
                Some(frame) => {
                    state.observe_latency(started.elapsed());
                    resized = Some(frame);
                }
                None => {
                    state.observe_timeout();
                    timed_out = true;
                }
            }
        }
        let frame = resized.as_ref().unwrap_or(&current);
        // 只画按当前面尺寸排好的帧：改宽时手里那帧常是上一拍宽度排的（右对齐项与滚动条停在
        // 旧边缘），首帧还可能按插件自己的占位尺寸排。已经等过一拍还没等到、且屏幕上本来就有
        // 内容时退回旧帧（否则整面空着）；从没画过正确帧（刚挂载）就空着等，别先闪一张错的。
        if !laid_out_at(frame, bounds.width, bounds.height) {
            let has_content = state.painted.borrow().is_some();
            if !(timed_out && has_content) {
                // 没等过新帧就尺寸不符 = 插件还停在旧尺寸（尺寸那笔丢了或它先交了旧画面）：
                // 再报一次尺寸让它重排，这一拍先不画，新帧到了宿主会重绘
                self.send(FrameInput::Resize { width: bounds.width, height: bounds.height });
                return;
            }
        }
        // 按已画的那一帧判失效：同步等到的新帧下一拍追进投影时，同一帧不再重画一遍
        let frame_changed = !state
            .painted
            .borrow()
            .as_ref()
            .is_some_and(|painted| Arc::ptr_eq(painted, frame));
        let font_changed = state.font_epoch.get() != epoch;
        let image_changed = state.image_epoch.get() != image_epoch;
        if font_changed || image_changed {
            if font_changed {
                // 字体族变了：段落按旧字族排的版，整体作废
                state.texts.borrow_mut().clear();
            }
            state.caches.borrow().iter().for_each(canvas::Cache::clear);
            state.popup.clear();
            state.font_epoch.set(epoch);
            state.image_epoch.set(image_epoch);
        }
        if frame_changed || state.steps.borrow().is_empty() {
            let previous = state.painted.borrow().clone();
            {
                let old_steps = state.steps.borrow();
                let mut next = state.next_steps.borrow_mut();
                plugin_paint::plan_into(&frame.commands, &mut next);
                let mut caches = state.caches.borrow_mut();
                if caches.len() < next.len() {
                    caches.resize_with(next.len(), canvas::Cache::new);
                }
                // 只有内容真的变了的段才丢弃几何；悬停改一个按钮颜色不该让整面重新三角化
                for (slot, step) in next.iter().enumerate() {
                    let reusable = previous.as_ref().zip(old_steps.get(slot)).is_some_and(
                        |(old_frame, old_step)| same_step(old_frame, old_step, frame, step),
                    );
                    if !reusable {
                        caches[slot].clear();
                    }
                }
            }
            std::mem::swap(
                &mut *state.steps.borrow_mut(),
                &mut *state.next_steps.borrow_mut(),
            );
            if frame_changed {
                *state.painted.borrow_mut() = Some(frame.clone());
            }
        }
        let steps = state.steps.borrow();
        let ctx = PaintCtx { app: self.app, plugin_id: &self.plugin_id };
        let mut texts = state.texts.borrow_mut();
        texts.begin_frame();
        renderer.with_layer(bounds, |renderer| {
            let mut caches = state.caches.borrow_mut();
            if caches.len() < steps.len() {
                caches.resize_with(steps.len(), canvas::Cache::new);
            }
            // iced 在同一图层内按图元类型成批绘制（quad → 三角 → 图片 → 文字）。
            // 声明顺序与批次序相反（后声明的形状要压住先前的文字 / 图片）时另起图层，
            // 图层按顺序合成，声明顺序才真正成立；顺批次序的相邻步会被渲染器合并回一层。
            let mut layer_open = false;
            let mut max_rank = 0u8;
            for (slot, step) in steps.iter().enumerate() {
                let rank = plugin_paint::step_rank(step);
                if !layer_open || rank < max_rank {
                    if layer_open {
                        renderer.end_layer();
                    }
                    renderer.start_layer(bounds);
                    layer_open = true;
                    max_rank = rank;
                } else {
                    max_rank = max_rank.max(rank);
                }
                match step {
                    plugin_paint::PaintStep::Shapes(range) => {
                        let geometry = caches[slot].draw(renderer, bounds.size(), |canvas| {
                            plugin_paint::execute_shapes(canvas, &frame.commands, range.clone(), &ctx);
                        });
                        renderer.with_translation(Vector::new(bounds.x, bounds.y), |renderer| {
                            renderer.draw_geometry(geometry);
                        });
                    }
                    plugin_paint::PaintStep::Image(index) => {
                        let geometry = caches[slot].draw(renderer, bounds.size(), |canvas| {
                            plugin_paint::execute_shapes(canvas, &frame.commands, *index..*index + 1, &ctx);
                        });
                        renderer.with_translation(Vector::new(bounds.x, bounds.y), |renderer| {
                            renderer.draw_geometry(geometry);
                        });
                    }
                    plugin_paint::PaintStep::Text(index) => {
                        texts.draw_one(renderer, &frame.commands[*index], bounds.position(), bounds);
                    }
                    plugin_paint::PaintStep::Shadow(index) => {
                        plugin_paint::draw_shadow(renderer, bounds, &frame.commands[*index]);
                    }
                }
            }
            if layer_open {
                renderer.end_layer();
            }
        });
        match frame.popup.as_ref() {
            // 窗口级浮层由根层 overlay 绘制；本面不画
            Some(popup) if popup.layer == PopupLayer::Panel => {
                state.popup.draw(popup, bounds.position(), bounds.size(), *viewport, renderer, &ctx);
            }
            _ => state.popup.clear(),
        }
        texts.end_frame();
    }

    fn mouse_interaction(
        &self,
        tree: &widget::Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        let Some(point) = cursor.position_in(layout.bounds()) else {
            return mouse::Interaction::None;
        };
        let state = tree.state.downcast_ref::<FrameState>();
        let painted = state.painted.borrow();
        let frame = painted.as_deref().unwrap_or(self.frame.as_ref());
        if let Some(popup) = frame.popup.as_ref().filter(|p| p.layer == PopupLayer::Panel) {
            if let Some(hit) = popup.hit_region_at(point.x, point.y) {
                return interaction_of(hit.cursor.unwrap_or_default());
            }
        }
        match frame.hit_region_at(point.x, point.y) {
            Some(hit) => interaction_of(hit.cursor.unwrap_or_default()),
            None => mouse::Interaction::None,
        }
    }
}

/// 同一槽位的绘制段在两张帧里是否内容一致：一致即可复用几何缓存。
fn same_step(
    old_frame: &SurfaceFrame,
    old_step: &plugin_paint::PaintStep,
    new_frame: &SurfaceFrame,
    new_step: &plugin_paint::PaintStep,
) -> bool {
    match (old_step, new_step) {
        (plugin_paint::PaintStep::Shapes(old), plugin_paint::PaintStep::Shapes(new)) => {
            old.len() == new.len()
                && old_frame.commands.get(old.clone()) == new_frame.commands.get(new.clone())
        }
        (plugin_paint::PaintStep::Image(old), plugin_paint::PaintStep::Image(new)) => {
            old_frame.commands.get(*old) == new_frame.commands.get(*new)
        }
        _ => false,
    }
}

/// 帧是否按给定尺寸排版：未声明尺寸的帧不参与判定（老插件按「拿到什么画什么」处理）。
fn laid_out_at(frame: &Arc<SurfaceFrame>, width: f32, height: f32) -> bool {
    frame.size.is_none_or(|size| {
        (size.w - width).abs() <= SIZE_TOLERANCE && (size.h - height).abs() <= SIZE_TOLERANCE
    })
}

/// 整面帧渲染入口：铺满可用区域，外层按贡献声明的内边距包裹。
pub fn frame_view<'a>(
    app: &'a App,
    window: window::Id,
    plugin_id: &str,
    surface_id: &str,
    frame: &'a Arc<SurfaceFrame>,
    padding: Padding,
) -> Element<'a, Message> {
    let focused = app.plugin_frame_focus.as_ref().is_some_and(|focus| {
        focus.plugin_id == plugin_id && focus.surface_id == surface_id
    });
    let widget = PluginFrame {
        app,
        window,
        plugin_id: plugin_id.to_string(),
        surface_id: surface_id.to_string(),
        frame,
        focused,
    };
    container(Element::new(widget))
        .padding(padding)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

pub(crate) fn interaction_of(cursor: HitCursor) -> mouse::Interaction {
    match cursor {
        HitCursor::Default => mouse::Interaction::Idle,
        HitCursor::Pointer => mouse::Interaction::Pointer,
        HitCursor::Text => mouse::Interaction::Text,
        HitCursor::Grab => mouse::Interaction::Grab,
        HitCursor::Grabbing => mouse::Interaction::Grabbing,
        HitCursor::Crosshair => mouse::Interaction::Crosshair,
        HitCursor::NotAllowed => mouse::Interaction::NotAllowed,
        HitCursor::ResizeHorizontal => mouse::Interaction::ResizingHorizontally,
        HitCursor::ResizeVertical => mouse::Interaction::ResizingVertically,
    }
}

/// iced 按键 → 协议按键：只转发功能键 / 编辑键 / 字符，其余（修饰键自身、媒体键等）不转发。
fn frame_key(key: &keyboard::Key) -> Option<FrameKey> {
    match key {
        keyboard::Key::Named(named) => frame_named(*named).map(|name| FrameKey::Named { name }),
        keyboard::Key::Character(text) if !text.is_empty() => {
            Some(FrameKey::Character { text: text.to_string() })
        }
        _ => None,
    }
}

fn frame_named(named: Named) -> Option<NamedKey> {
    Some(match named {
        Named::Escape => NamedKey::Escape,
        Named::Enter => NamedKey::Enter,
        Named::Tab => NamedKey::Tab,
        Named::Space => NamedKey::Space,
        Named::Backspace => NamedKey::Backspace,
        Named::Delete => NamedKey::Delete,
        Named::Insert => NamedKey::Insert,
        Named::Home => NamedKey::Home,
        Named::End => NamedKey::End,
        Named::PageUp => NamedKey::PageUp,
        Named::PageDown => NamedKey::PageDown,
        Named::ArrowUp => NamedKey::ArrowUp,
        Named::ArrowDown => NamedKey::ArrowDown,
        Named::ArrowLeft => NamedKey::ArrowLeft,
        Named::ArrowRight => NamedKey::ArrowRight,
        Named::F1 => NamedKey::F1,
        Named::F2 => NamedKey::F2,
        Named::F3 => NamedKey::F3,
        Named::F4 => NamedKey::F4,
        Named::F5 => NamedKey::F5,
        Named::F6 => NamedKey::F6,
        Named::F7 => NamedKey::F7,
        Named::F8 => NamedKey::F8,
        Named::F9 => NamedKey::F9,
        Named::F10 => NamedKey::F10,
        Named::F11 => NamedKey::F11,
        Named::F12 => NamedKey::F12,
        _ => return None,
    })
}

pub(crate) fn frame_modifiers(modifiers: keyboard::Modifiers) -> FrameModifiers {
    FrameModifiers {
        shift: modifiers.shift(),
        control: modifiers.control(),
        alt: modifiers.alt(),
        meta: modifiers.logo(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atelyx_core::plugin_runtime::{ColorToken, DrawCommand, PaintColor};

    fn rect(width: f32) -> DrawCommand {
        DrawCommand::Rect {
            x: 0.0,
            y: 0.0,
            w: width,
            h: 10.0,
            fill: Some(PaintColor::Token { name: ColorToken::Surface }),
            stroke: None,
            stroke_width: None,
            corner_radius: None,
            shadow: None,
        }
    }

    fn frame(commands: Vec<DrawCommand>) -> SurfaceFrame {
        SurfaceFrame { commands, ..SurfaceFrame::default() }
    }

    #[test]
    fn same_step_reuses_only_equal_segments() {
        let first = frame(vec![rect(10.0)]);
        let same = frame(vec![rect(10.0)]);
        let changed = frame(vec![rect(20.0)]);
        let shapes = plugin_paint::PaintStep::Shapes(0..1);
        assert!(same_step(&first, &shapes, &same, &shapes));
        assert!(!same_step(&first, &shapes, &changed, &shapes));
        assert!(!same_step(&first, &shapes, &same, &plugin_paint::PaintStep::Text(0)));
    }
}


