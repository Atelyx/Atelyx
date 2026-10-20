//! 插件自绘画布部件：执行插件提交的绘图指令清单（`canvas.submit` 存档），指针事件按节点声明
//! 子集回发（命中由插件按指令几何自算）。缓存按「清单 Arc 指针 + 尺寸 + 界面字体族变更计数」
//! 失效——清单整发整换，指针不变即清单未变，直接复用上次几何。

use std::sync::Arc;
use std::time::Instant;

use iced::widget::canvas::{self, Canvas, Path as CanvasPath, Stroke, Text as CanvasText};
use iced::{Color, Element, Length, Point, Rectangle};

use atelyx_core::plugin_runtime::{
    CanvasColor, CanvasEventKind, CanvasPointer, ColorToken, DrawCommand, ImageSource, MouseButton,
    TextWeight,
};

use crate::plugin_actions;
use crate::ui::palette as pal;
use crate::{App, Message};

/// 双击判定：同键二次按下的最大间隔。
const DOUBLE_CLICK_MS: u64 = 500;
/// 双击判定：两次按下间的最大位移（逻辑像素；拖动不算双击）。
const DOUBLE_CLICK_SLOP: f32 = 4.0;

/// 部件跨帧状态（存 `tree::State`，随树投影存活）：缓存 + 拖拽 / 悬停 / 双击判定。
/// iced 每帧重建部件，本状态必须全部走内部可变性（`Program::draw` 只拿到共享引用）。
pub struct CanvasState {
    cache: canvas::Cache,
    /// 上次绘制用的清单标识（Arc 指针地址，0 = 无清单）；变化即清缓存。
    list_key: std::cell::Cell<usize>,
    /// 上次绘制用的界面字体族变更计数；换族后文字几何要重画（清单不变也得失效）。
    font_epoch: std::cell::Cell<u64>,
    /// 按下中：Move / Up 走绝对坐标换算（拖出画布不丢事件，抬起为止）。
    pressed: std::cell::Cell<bool>,
    /// 悬停中：Leave 按「从内到外」边沿触发，不逐帧重发。
    inside: std::cell::Cell<bool>,
    last_press: std::cell::RefCell<Option<(MouseButton, Instant, Point)>>,
    /// 上次上报给插件的画布身份与尺寸（节点 id 哈希 + 逻辑像素尺寸）；节点或尺寸变化即在绘制时
    /// 回发 `host:canvas.resized`——只比尺寸会让复用同一 `tree::State` 的另一个画布节点漏报初始尺寸。
    reported: std::cell::Cell<Option<(u64, f32, f32)>>,
}

impl Default for CanvasState {
    fn default() -> Self {
        Self {
            cache: canvas::Cache::new(),
            list_key: std::cell::Cell::new(0),
            font_epoch: std::cell::Cell::new(0),
            pressed: std::cell::Cell::new(false),
            inside: std::cell::Cell::new(false),
            last_press: std::cell::RefCell::new(None),
            reported: std::cell::Cell::new(None),
        }
    }
}

/// 插件自绘画布。
pub struct PluginCanvas<'a> {
    pub app: &'a App,
    pub window: iced::window::Id,
    /// 面身份随事件回发（树节点 id 借自投影树，此处收拢为自有值）。
    pub plugin_id: String,
    pub surface_id: String,
    pub node_id: &'a str,
    /// 最新指令清单（`None` = 未提交过；`Some(空)` = 清空画布）。
    pub list: Option<&'a Arc<Vec<DrawCommand>>>,
    pub mouse_events: &'a [CanvasEventKind],
}

impl PluginCanvas<'_> {
    fn declared(&self, kind: CanvasEventKind) -> bool {
        self.mouse_events.contains(&kind)
    }

    fn message(&self, pointer: CanvasPointer) -> Message {
        Message::PluginView {
            plugin_id: self.plugin_id.clone(),
            surface_id: self.surface_id.clone(),
            node_id: self.node_id.to_string(),
            event: plugin_actions::view_event_canvas(pointer),
        }
    }
}

impl canvas::Program<Message> for PluginCanvas<'_> {
    type State = CanvasState;

    fn update(
        &self,
        state: &mut Self::State,
        event: &canvas::Event,
        bounds: Rectangle,
        cursor: iced::mouse::Cursor,
    ) -> Option<iced::widget::Action<Message>> {
        use canvas::Event as Ev;
        use iced::mouse::Event as MouseEv;
        let publish = |pointer: CanvasPointer, capture: bool| {
            let action = iced::widget::Action::publish(self.message(pointer));
            if capture { Some(action.and_capture()) } else { Some(action) }
        };
        match event {
            Ev::Mouse(MouseEv::ButtonPressed(button)) => {
                let point = cursor.position_in(bounds)?;
                if state.pressed.get() {
                    return None;
                }
                let mouse_button = mouse_button(*button)?;
                // 双击宿主判定（同键二次按下、间隔与位移都在阈值内），插件无需自己算
                let now = Instant::now();
                let mut last = state.last_press.borrow_mut();
                let double_click = last.is_some_and(|(last_button, at, last_point)| {
                    last_button == mouse_button
                        && now.duration_since(at).as_millis() < u128::from(DOUBLE_CLICK_MS)
                        && distance_sq(point, last_point) < DOUBLE_CLICK_SLOP * DOUBLE_CLICK_SLOP
                });
                *last = if double_click { None } else { Some((mouse_button, now, point)) };
                drop(last);
                state.pressed.set(true);
                state.inside.set(true);
                let (kind, pointer) = if double_click {
                    (
                        CanvasEventKind::DoubleClick,
                        CanvasPointer::DoubleClick { x: point.x, y: point.y, button: mouse_button },
                    )
                } else {
                    (
                        CanvasEventKind::PointerDown,
                        CanvasPointer::Down { x: point.x, y: point.y, button: mouse_button },
                    )
                };
                self.declared(kind).then(|| publish(pointer, true)).flatten()
            }
            Ev::Mouse(MouseEv::ButtonReleased(button)) => {
                if !state.pressed.get() {
                    return None;
                }
                let mouse_button = mouse_button(*button)?;
                state.pressed.set(false);
                // 拖拽中抬笔可在画布外：绝对坐标换算本地点
                let point = drag_point(cursor, bounds)?;
                self.declared(CanvasEventKind::PointerUp)
                    .then(|| publish(CanvasPointer::Up { x: point.x, y: point.y, button: mouse_button }, true))
                    .flatten()
            }
            Ev::Mouse(MouseEv::CursorMoved { .. }) => {
                if state.pressed.get() {
                    let point = drag_point(cursor, bounds)?;
                    self.declared(CanvasEventKind::PointerMove)
                        .then(|| publish(CanvasPointer::Move { x: point.x, y: point.y }, true))
                        .flatten()
                } else {
                    // 悬停只在画布内发 Move；从内到外边沿发 Leave
                    let inside = cursor.position_in(bounds).is_some();
                    let left = state.inside.get() && !inside;
                    state.inside.set(inside);
                    if left && self.declared(CanvasEventKind::PointerLeave) {
                        publish(CanvasPointer::Leave, false)
                    } else if inside && self.declared(CanvasEventKind::PointerMove) {
                        let point = cursor.position_in(bounds)?;
                        publish(CanvasPointer::Move { x: point.x, y: point.y }, false)
                    } else {
                        None
                    }
                }
            }
            Ev::Mouse(MouseEv::WheelScrolled { delta }) => {
                // iced 把全窗口鼠标事件广播给所有 widget：只认光标落在画布内的滚轮（防跨面板串扰）
                let point = cursor.position_in(bounds)?;
                let (dx, dy) = match delta {
                    // iced 沿 winit（正 = 向上 / 向左）；协议按 Web 惯例（正 = 向下 / 向右），取反换算
                    iced::mouse::ScrollDelta::Lines { x, y } | iced::mouse::ScrollDelta::Pixels { x, y } => (-x, -y),
                };
                self.declared(CanvasEventKind::Wheel)
                    // 声明了 wheel 即画布消费滚轮（如缩放），外层滚动容器不再同帧滚动
                    .then(|| publish(CanvasPointer::Wheel { x: point.x, y: point.y, delta_x: dx, delta_y: dy }, true))
                    .flatten()
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        state: &Self::State,
        renderer: &iced::Renderer,
        _theme: &iced::Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let list_key = self
            .list
            .map(|list| Arc::as_ptr(list) as usize)
            .unwrap_or_default();
        let epoch = crate::ui::epoch();
        if state.list_key.get() != list_key || state.font_epoch.get() != epoch {
            state.cache.clear();
            state.list_key.set(list_key);
            state.font_epoch.set(epoch);
        }
        // 节点或尺寸变化即上报插件：绘制拿到的是真实分配尺寸，这是插件对齐坐标与滚动范围的唯一来源
        let reported = (node_identity(self.node_id), bounds.width, bounds.height);
        if state.reported.get() != Some(reported) {
            state.reported.set(Some(reported));
            self.app.plugins.notify_canvas_resized(
                &self.plugin_id,
                &self.surface_id,
                self.node_id,
                reported.1,
                reported.2,
            );
        }
        let commands: &[DrawCommand] = self.list.map(|list| list.as_slice()).unwrap_or(&[]);
        let geometry = state.cache.draw(renderer, bounds.size(), |frame| {
            for command in commands {
                execute(frame, command, self);
            }
        });
        vec![geometry]
    }
}

impl<'a> From<PluginCanvas<'a>> for Element<'a, Message> {
    fn from(widget: PluginCanvas<'a>) -> Element<'a, Message> {
        Canvas::new(widget).width(Length::Fill).height(Length::Fill).into()
    }
}

/// 节点身份哈希（FNV-1a；仅用于"是不是同一个画布节点"的判重，不参与协议）。
fn node_identity(node_id: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in node_id.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// 两点距离平方（双击位移阈值比较用，免开方）。
fn distance_sq(a: Point, b: Point) -> f32 {
    let dx = a.x - b.x;
    let dy = a.y - b.y;
    dx * dx + dy * dy
}

/// 拖拽中的本地点：光标在窗口内即可换算（允许拖出画布采点，抬起为止）。
fn drag_point(cursor: iced::mouse::Cursor, bounds: Rectangle) -> Option<Point> {
    let absolute = cursor.position()?;
    Some(Point::new(absolute.x - bounds.x, absolute.y - bounds.y))
}

fn mouse_button(button: iced::mouse::Button) -> Option<MouseButton> {
    match button {
        iced::mouse::Button::Left => Some(MouseButton::Left),
        iced::mouse::Button::Right => Some(MouseButton::Right),
        iced::mouse::Button::Middle => Some(MouseButton::Middle),
        _ => None,
    }
}

/// 执行一条绘图指令。图片来源句柄未就绪（载入中或不可用）时跳过该条，不阻塞其余指令。
fn execute(frame: &mut canvas::Frame, command: &DrawCommand, widget: &PluginCanvas<'_>) {
    match command {
        DrawCommand::Line { x1, y1, x2, y2, stroke, width } => {
            let path = CanvasPath::line(Point::new(*x1, *y1), Point::new(*x2, *y2));
            frame.stroke(&path, stroke_of(color(*stroke), *width));
        }
        DrawCommand::Rect { x, y, w, h, fill, stroke, stroke_width, corner_radius } => {
            let top_left = Point::new(*x, *y);
            let size = iced::Size::new(*w, *h);
            let radius = corner_radius.filter(|r| *r > 0.0);
            if let Some(fill) = fill {
                match radius {
                    Some(radius) => frame.fill(&CanvasPath::rounded_rectangle(top_left, size, radius.into()), color(*fill)),
                    None => frame.fill_rectangle(top_left, size, color(*fill)),
                }
            }
            if let Some(stroke) = stroke {
                let width = stroke_width.unwrap_or(1.0);
                match radius {
                    Some(radius) => frame.stroke(&CanvasPath::rounded_rectangle(top_left, size, radius.into()), stroke_of(color(*stroke), width)),
                    None => frame.stroke_rectangle(top_left, size, stroke_of(color(*stroke), width)),
                }
            }
        }
        DrawCommand::Path { points, closed, fill, stroke, stroke_width } => {
            let path = polyline(points, *closed);
            if let Some(fill) = fill {
                frame.fill(&path, color(*fill));
            }
            if let Some(stroke) = stroke {
                frame.stroke(&path, stroke_of(color(*stroke), stroke_width.unwrap_or(1.0)));
            }
        }
        DrawCommand::Circle { cx, cy, r, fill, stroke, stroke_width } => {
            let path = CanvasPath::circle(Point::new(*cx, *cy), *r);
            if let Some(fill) = fill {
                frame.fill(&path, color(*fill));
            }
            if let Some(stroke) = stroke {
                frame.stroke(&path, stroke_of(color(*stroke), stroke_width.unwrap_or(1.0)));
            }
        }
        DrawCommand::Text { x, y, content, size, color: text_color, weight } => {
            frame.fill_text(CanvasText {
                content: content.clone(),
                position: Point::new(*x, *y),
                color: color(*text_color),
                size: (*size).into(),
                font: weight.map(font_of).unwrap_or_else(crate::ui::regular),
                // 缺省 Shaping::Auto 已覆盖 CJK 文本
                ..CanvasText::default()
            });
        }
        DrawCommand::Image { source, x, y, w, h } => {
            let Some(handle) = image_handle(widget, source) else {
                return;
            };
            frame.draw_image(Rectangle::new(Point::new(*x, *y), iced::Size::new(*w, *h)), &handle);
        }
    }
}

fn polyline(points: &[atelyx_core::plugin_runtime::CanvasPoint], closed: bool) -> CanvasPath {
    CanvasPath::new(|path| {
        let mut points = points.iter();
        if let Some(first) = points.next() {
            path.move_to(Point::new(first.x, first.y));
        }
        for point in points {
            path.line_to(Point::new(point.x, point.y));
        }
        if closed {
            path.close();
        }
    })
}

fn image_handle(widget: &PluginCanvas<'_>, source: &ImageSource) -> Option<iced::widget::image::Handle> {
    let key = plugin_actions::image_key(widget.app, &widget.plugin_id, source)?;
    widget.app.note_images.get(&key).cloned()
}

/// token → 主题色（插件控件与画布共用同一映射）。
pub(crate) fn token_color(token: ColorToken) -> Color {
    match token {
        ColorToken::Text => pal::TEXT_PRIMARY,
        ColorToken::TextSecondary => pal::TEXT_SECONDARY,
        ColorToken::TextMuted => pal::TEXT_MUTED,
        ColorToken::Accent => pal::ACCENT,
        ColorToken::Danger => pal::DANGER,
        ColorToken::Success => pal::SUCCESS,
        ColorToken::Warning => pal::WARNING,
    }
}

fn color(value: CanvasColor) -> Color {
    match value {
        CanvasColor::Token { name } => token_color(name),
        CanvasColor::Rgba { rgba: [r, g, b, a] } => Color::from_rgba8(r, g, b, f32::from(a) / 255.0),
    }
}

/// 字重 → 字体（同 [`crate::plugin_view`] 的控件口径）；插件只有常规与强调两档。
pub(crate) fn font_of(weight: TextWeight) -> iced::Font {
    match weight {
        TextWeight::Normal => crate::ui::regular(),
        TextWeight::Medium | TextWeight::SemiBold => crate::ui::bold(),
    }
}

fn stroke_of(color: Color, width: f32) -> Stroke<'static> {
    Stroke { style: canvas::Style::Solid(color), width: width.max(0.1), ..Stroke::default() }
}
