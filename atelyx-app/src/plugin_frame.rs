//! 插件整面帧部件：执行帧的绘制指令清单，按命中区回发原始指针 / 键盘 / 输入法输入。
//! 坐标一律面本地逻辑像素；形状与图片按帧缓存几何，文字按段落缓存跨帧复用。

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
    FrameInput, HitCursor, Key as FrameKey, KeyAction, Modifiers as FrameModifiers, MouseButton,
    NamedKey, SurfaceFrame,
};

use crate::plugin_paint::{self, PaintCtx};
use crate::{App, Message};

/// 双击判定：同键二次按下的最大间隔。
const DOUBLE_CLICK_MS: u64 = 500;
/// 双击判定：两次按下间的最大位移（逻辑像素；拖动不算双击）。
const DOUBLE_CLICK_SLOP: f32 = 4.0;
/// 改宽那一拍等插件新帧的上限：插件回帧实测往返 0.5 ms 以内，留出余量给重排重的面。
/// 超时即按手上的帧画（下一拍再换新帧），不把 UI 线程压在慢插件上。
const RESIZE_FRAME_WAIT: Duration = Duration::from_millis(4);
/// 等帧超时后的冷却：这段时间内的尺寸变化不再同步等帧。拖动中慢插件每拍都超时，
/// 每拍白等一次比「内容晚一个往返」更伤手感。
const RESIZE_WAIT_COOLDOWN: Duration = Duration::from_millis(500);

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
fn distance_sq(a: Point, b: Point) -> f32 {
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
fn mouse_button(button: mouse::Button) -> Option<MouseButton> {
    match button {
        mouse::Button::Left => Some(MouseButton::Left),
        mouse::Button::Right => Some(MouseButton::Right),
        mouse::Button::Middle => Some(MouseButton::Middle),
        _ => None,
    }
}

/// 部件跨帧状态（存 `tree::State`）：绘制缓存 + 拖拽 / 悬停 / 双击判定 + 输入法开关。
struct FrameState {
    cache: canvas::Cache,
    /// 已排版的文字段落：内容不变跨帧复用，改宽 / 滚动 / 悬停不重新成形。
    texts: RefCell<plugin_paint::TextCache>,
    /// 上次绘制的帧：按 Arc 身份判换帧。持有引用，帧对象地址不会被分配器复用。
    painted: RefCell<Option<Arc<SurfaceFrame>>>,
    /// 上次绘制用的界面字体族变更计数。
    font_epoch: Cell<u64>,
    /// 上次绘制用的界面图片句柄代数。
    image_epoch: Cell<u64>,
    /// 上次上报给插件的面身份与尺寸（逻辑像素）；变化即回发 `Resize`。
    reported: Cell<Option<(u64, f32, f32)>>,
    /// 改宽等帧超时后的冷却截止：冷却内不再同步等帧。
    /// 不能在下一帧到达时提前解除——慢插件每拍都会超时，解除等于每拍都白等 4ms。
    resize_wait_until: Cell<Option<Instant>>,
    /// 本部件当前承载的面身份：变化即说明换了面，指针瞬态一律作废。
    identity: Cell<u64>,
    /// 按下中的键（`None` = 未按下）：抬笔只认这一个键，中途混按其它键不会错发 `PointerUp`。
    pressed: Cell<Option<MouseButton>>,
    /// 悬停中：Leave 按「从内到外」边沿触发，不逐帧重发。
    inside: Cell<bool>,
    /// 最近一次指针位置（面本地）：窗口失焦时按它补发 `PointerUp`，插件不会卡在拖拽态。
    last_point: Cell<Option<Point>>,
    last_press: RefCell<Option<(MouseButton, Instant, Point)>>,
    /// 已请求启用输入法（失去焦点或帧不再声明 `ime` 时请求停用一次）。
    ime_on: Cell<bool>,
}

impl Default for FrameState {
    fn default() -> Self {
        Self {
            cache: canvas::Cache::new(),
            texts: RefCell::new(plugin_paint::TextCache::default()),
            painted: RefCell::new(None),
            font_epoch: Cell::new(0),
            image_epoch: Cell::new(0),
            reported: Cell::new(None),
            resize_wait_until: Cell::new(None),
            identity: Cell::new(0),
            pressed: Cell::new(None),
            inside: Cell::new(false),
            last_point: Cell::new(None),
            last_press: RefCell::new(None),
            ime_on: Cell::new(false),
        }
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
        frame.hit_at(point.x, point.y).map(str::to_string)
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
            state.pressed.set(None);
            state.inside.set(false);
            state.last_point.set(None);
            *state.last_press.borrow_mut() = None;
            state.painted.borrow_mut().take();
            state.cache.clear();
            state.texts.borrow_mut().clear();
            state.resize_wait_until.set(None);
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
                        (-x * crate::canvas_view::WHEEL_LINE_PX, -y * crate::canvas_view::WHEEL_LINE_PX)
                    }
                    mouse::ScrollDelta::Pixels { x, y } => (-x, -y),
                };
                self.send(FrameInput::Wheel {
                    x: point.x,
                    y: point.y,
                    delta_x: dx,
                    delta_y: dy,
                    hit: self.hit(state, point),
                });
                // 面消费滚轮：外层滚动容器不再同帧滚动
                shell.capture_event();
            }
            Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, text, .. }) => {
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
                if !self.focused || self.frame.ime.is_none() {
                    return;
                }
                let input = match ime {
                    ImeEvent::Preedit(content, range) => FrameInput::Text {
                        text: content.clone(),
                        composing: true,
                        caret: range.as_ref().map(|range| range.end as u32),
                        candidates: self.frame.ime,
                    },
                    ImeEvent::Commit(content) => FrameInput::Text {
                        text: content.clone(),
                        composing: false,
                        caret: None,
                        candidates: self.frame.ime,
                    },
                    // 组合结束且无提交：空串表示放弃组合，不插入文本
                    ImeEvent::Closed => FrameInput::Text {
                        text: String::new(),
                        composing: true,
                        caret: None,
                        candidates: self.frame.ime,
                    },
                    ImeEvent::Opened => return,
                };
                self.send(input);
                shell.capture_event();
            }
            Event::Window(window::Event::RedrawRequested(_)) => {
                let enabled = self.focused && self.frame.ime.is_some();
                if let Some(ime) = self.frame.ime.filter(|_| enabled) {
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
        // 插件在另一进程，新尺寸的帧要一个往返才回来；这一拍就等它回来画进去——否则画出的内容
        // 永远是上一拍尺寸的，拖动时字样与右对齐项明显拖在分割线后面。
        let mut resized = None;
        let reported = (self.identity(), bounds.width, bounds.height);
        if state.reported.get() != Some(reported) {
            state.reported.set(Some(reported));
            let now = Instant::now();
            let cooling =
                state.resize_wait_until.get().is_some_and(|until| now < until);
            if cooling {
                self.send(FrameInput::Resize { width: bounds.width, height: bounds.height });
            } else {
                // 等的是「比已画的那一帧更新」：投影可能落后，拿它当基准会立刻返回旧帧
                let seen = state.painted.borrow().clone().unwrap_or_else(|| current.clone());
                // 先登记再发尺寸：登记期间回来的帧由这一拍取走画上，不再另发一条通知
                self.app.plugins.begin_surface_wait(
                    &self.plugin_id,
                    &self.surface_id,
                    seen,
                );
                self.send(FrameInput::Resize { width: bounds.width, height: bounds.height });
                match self.app.plugins.finish_surface_wait(
                    &self.plugin_id,
                    &self.surface_id,
                    RESIZE_FRAME_WAIT,
                ) {
                    Some(frame) => {
                        resized = Some(frame);
                        state.resize_wait_until.set(None);
                    }
                    None => state
                        .resize_wait_until
                        .set(Some(now + RESIZE_WAIT_COOLDOWN)),
                }
            }
        }
        let frame = resized.as_ref().unwrap_or(&current);
        // 按已画的那一帧判失效：同步等到的新帧下一拍追进投影时，同一帧不再重画一遍
        let frame_changed = !state
            .painted
            .borrow()
            .as_ref()
            .is_some_and(|painted| Arc::ptr_eq(painted, frame));
        if frame_changed
            || state.font_epoch.get() != epoch
            || state.image_epoch.get() != image_epoch
        {
            state.cache.clear();
            if state.font_epoch.get() != epoch {
                // 字体族变了：段落按旧字族排的版，整体作废
                state.texts.borrow_mut().clear();
            }
            state.font_epoch.set(epoch);
            state.image_epoch.set(image_epoch);
            if frame_changed {
                *state.painted.borrow_mut() = Some(frame.clone());
            }
        }
        let geometry = state.cache.draw(renderer, bounds.size(), |canvas| {
            plugin_paint::execute_shapes(
                canvas,
                &frame.commands,
                &PaintCtx { app: self.app, plugin_id: &self.plugin_id },
            );
        });
        renderer.with_translation(Vector::new(bounds.x, bounds.y), |renderer| {
            renderer.draw_geometry(geometry);
        });
        state
            .texts
            .borrow_mut()
            .draw(renderer, &frame.commands, bounds);
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
        match frame.hit_at(point.x, point.y) {
            Some(id) => interaction_of(frame.hit_cursor(id)),
            None => mouse::Interaction::None,
        }
    }
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

fn interaction_of(cursor: HitCursor) -> mouse::Interaction {
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

fn frame_modifiers(modifiers: keyboard::Modifiers) -> FrameModifiers {
    FrameModifiers {
        shift: modifiers.shift(),
        control: modifiers.control(),
        alt: modifiers.alt(),
        meta: modifiers.logo(),
    }
}
