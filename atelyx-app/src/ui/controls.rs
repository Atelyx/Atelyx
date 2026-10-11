//! 基础控件：按钮 / 图标按钮 / 状态徽标 / 空态 / 分隔线 / 输入框。
//!
//! 颜色与尺寸档取自 [`super::palette`]；
//! hover/禁用反馈经 `button::Status` 在样式闭包内表达。

use iced::widget::{button, column, container, mouse_area, row, space, text, text_input};
use iced::{Color, Element, Length, Padding, Theme, border, widget::container::Style};

use super::icon::{icon, icon_stroke};
use super::palette as pal;
use super::UiMsg;
use crate::Message;

/// 按钮变体（底色 + 文字色 + hover 反馈）。
#[derive(Clone, Copy, PartialEq)]
pub enum Variant {
    Secondary,
    Ghost,
    /// 强调淡底（设置入口激活态等）。
    AccentSoft,
    /// 透明底 + hover 出三级底（工具条「+」等触发钮）。
    SubtleBg,
}

/// 文字按钮尺寸档（高度 / 水平内边距 / 字号 / 圆角）。
pub struct TextSize {
    pub h: f32,
    pub pad_x: f32,
    pub font: f32,
    pub radius: f32,
}

/// `sm`（h-6）
pub const BTN_SM: TextSize = TextSize { h: 24.0, pad_x: 8.0, font: pal::FS_MICRO, radius: pal::RADIUS_SM };

/// 图标按钮边长档（与文字按钮高度同档）。
pub const ICON_MD: f32 = 28.0;
pub const ICON_LG: f32 = 32.0;

struct VariantStyle {
    bg: Color,
    bg_hover: Color,
    text: Color,
    text_hover: Color,
}

fn variant_style(v: Variant) -> VariantStyle {
    match v {
        Variant::Secondary => VariantStyle {
            bg: pal::BG_TERTIARY,
            bg_hover: pal::HOVER,
            text: pal::TEXT_PRIMARY,
            text_hover: pal::TEXT_PRIMARY,
        },
        Variant::Ghost => VariantStyle {
            bg: Color::TRANSPARENT,
            bg_hover: pal::HOVER,
            text: pal::TEXT_SECONDARY,
            text_hover: pal::TEXT_PRIMARY,
        },
        Variant::AccentSoft => VariantStyle {
            bg: pal::ACCENT_SOFT,
            bg_hover: pal::ACCENT_SOFT,
            text: pal::ACCENT,
            text_hover: pal::ACCENT,
        },
        Variant::SubtleBg => VariantStyle {
            bg: Color::TRANSPARENT,
            bg_hover: pal::BG_TERTIARY,
            text: pal::TEXT_MUTED,
            text_hover: pal::TEXT_PRIMARY,
        },
    }
}

fn style_for(v: Variant, radius: f32) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let s = variant_style(v);
        let (bg, text_color) = match status {
            button::Status::Hovered | button::Status::Pressed => (s.bg_hover, s.text_hover),
            button::Status::Disabled => (s.bg.scale_alpha(0.4), s.text.scale_alpha(0.4)),
            button::Status::Active => (s.bg, s.text),
        };
        button::Style {
            background: Some(bg.into()),
            text_color,
            border: border::rounded(radius),
            ..button::Style::default()
        }
    }
}

/// 文字按钮（文字色走样式闭包：hover 换色经 text_hover 生效；内容垂直居中）。
pub fn btn(
    label: impl Into<String>,
    size: &TextSize,
    variant: Variant,
    on_press: Option<Message>,
) -> button::Button<'static, Message> {
    let widget = button(
        container(
            text(label.into())
                .size(size.font)
                .font(iced::Font { weight: iced::font::Weight::Medium, ..iced::Font::default() }),
        )
        .height(Length::Fill)
        .align_y(iced::alignment::Vertical::Center),
    )
    .padding(Padding::ZERO.horizontal(size.pad_x))
    .height(Length::Fixed(size.h))
    .style(style_for(variant, size.radius));
    match on_press {
        Some(m) => widget.on_press(m),
        None => widget,
    }
}

/// 图标按钮（正方形；`label` 同时是无障碍语义名与悬停提示来源）。
pub fn icon_button(
    name: &str,
    icon_px: f32,
    box_px: f32,
    variant: Variant,
    on_press: Option<Message>,
    tip: Option<(iced::window::Id, String)>,
) -> Element<'static, Message> {
    let radius = if box_px <= 20.0 { pal::RADIUS_XS } else if box_px <= ICON_MD { pal::RADIUS_SM } else { pal::RADIUS_MD };
    let mut widget = button(
        container(icon(name, icon_px, variant_style(variant).text))
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(iced::alignment::Horizontal::Center)
            .align_y(iced::alignment::Vertical::Center),
    )
    .padding(0.0)
    .width(Length::Fixed(box_px))
    .height(Length::Fixed(box_px))
    .style(style_for(variant, radius));
    widget = match on_press {
        Some(m) => widget.on_press(m),
        None => widget,
    };
    let element: Element<'static, Message> = widget.into();
    match tip {
        Some((window, label)) => mouse_area(element)
            .on_enter(Message::Ui(UiMsg::HoverTip { window, label: label.clone(), box_px }))
            .on_exit(Message::Ui(UiMsg::HoverTipEnd { window, label }))
            .into(),
        None => element,
    }
}

/// 状态档（色 + 文字编码）。
#[derive(Clone, Copy)]
pub enum PillStatus {
    /// 进行中
    Pending,
}

impl PillStatus {
    fn color(self) -> Color {
        match self {
            PillStatus::Pending => pal::WARNING,
        }
    }
}

/// 状态徽标（`StatusPill`）。
pub fn status_pill(status: PillStatus, label: impl Into<String>) -> Element<'static, Message> {
    let color = status.color();
    row![
        container(space())
            .width(Length::Fixed(6.0))
            .height(Length::Fixed(6.0))
            .style(move |_t| Style {
                background: Some(color.into()),
                border: border::rounded(3.0),
                ..Style::default()
            }),
        text(label.into()).size(pal::FS_MICRO).color(color),
    ]
    .spacing(6)
    .into()
}

/// 空态（图标 + 标题 + 描述 + 可选动作）。
pub fn empty_state(
    icon_name: &str,
    title: impl Into<String>,
    desc: impl Into<String>,
    action: Option<(String, Message)>,
) -> Element<'static, Message> {
    let mut col = column![
        container(icon(icon_name, 18.0, pal::TEXT_MUTED))
            .width(Length::Fixed(40.0))
            .height(Length::Fixed(40.0))
            .align_x(iced::alignment::Horizontal::Center)
            .align_y(iced::alignment::Vertical::Center)
            .style(|_t| Style {
                background: Some(pal::BG_TERTIARY.into()),
                border: border::rounded(pal::RADIUS_MD),
                ..Style::default()
            }),
        text(title.into())
            .size(pal::FS_BODY)
            .color(pal::TEXT_PRIMARY)
            .font(iced::Font { weight: iced::font::Weight::Medium, ..iced::Font::default() }),
        text(desc.into())
            .size(pal::FS_UI)
            .color(pal::TEXT_MUTED),
    ]
    .spacing(8)
    .align_x(iced::alignment::Horizontal::Center);
    if let Some((label, msg)) = action {
        col = col.push(
            container(btn(label, &BTN_SM, Variant::Secondary, Some(msg)))
            .padding(Padding::ZERO.top(4.0)),
        );
    }
    container(col)
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into()
}

/// 视图占位引导（无文件/空视图）：64px 细描边图标 + 标题 + 描述，居中。
pub fn view_placeholder(
    icon_name: &str,
    title: impl Into<String>,
    desc: impl Into<String>,
) -> Element<'static, Message> {
    container(
        column![
            icon_stroke(icon_name, 64.0, pal::TEXT_MUTED.scale_alpha(0.6), 1.5),
            text(title.into())
                .size(pal::FS_H1)
                .color(pal::TEXT_PRIMARY)
                .font(crate::ui::semibold()),
            text(desc.into())
                .size(pal::FS_BODY)
                .color(pal::TEXT_MUTED),
        ]
        .spacing(16)
        .align_x(iced::alignment::Horizontal::Center),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .center_x(Length::Fill)
    .center_y(Length::Fill)
    .into()
}

/// 水平分隔线（`subtle` = 次级线色，用于工具条/分组分隔）。
pub fn h_divider_subtle() -> Element<'static, Message> {
    h_divider_with(pal::BORDER_SUBTLE)
}

fn h_divider_with(color: iced::Color) -> Element<'static, Message> {
    container(space())
        .width(Length::Fill)
        .height(Length::Fixed(1.0))
        .style(move |_t| Style {
            background: Some(color.into()),
            ..Style::default()
        })
        .into()
}

/// 行内重命名输入的固定部件 id（同窗口同时至多一个重命名会话；发起侧据此挂焦点任务）。
pub const INLINE_RENAME_INPUT: &str = "inline-rename-input";

/// 行内输入（重命名等场景：透明底、提交走 on_submit；Escape 取消由应用层键盘路由承担）。
/// `press_inside` = 输入行被按下的上报消息：应用层全局按压据此区分「点在输入行内」
/// 与「点在外面」（失焦提交的判据；widget 消息先于全局按压消息入队，同帧标志可靠）。
pub fn inline_input(
    id: iced::widget::Id,
    placeholder: &str,
    value: &str,
    width: f32,
    on_input: impl Fn(String) -> Message + 'static,
    on_submit: Message,
    press_inside: Message,
) -> Element<'static, Message> {
    let input = text_input(placeholder, value)
        .id(id)
        .size(pal::FS_CAPTION)
        .padding(Padding::ZERO.horizontal(6.0).vertical(2.0))
        .width(Length::Fixed(width))
        .on_input(on_input)
        .on_submit(on_submit)
        .style(|_theme, status| text_input::Style {
            background: Color::TRANSPARENT.into(),
            border: border::rounded(0.0)
                .color(pal::ACCENT)
                .width(if matches!(status, text_input::Status::Focused { .. }) { 1.0 } else { 0.0 }),
            icon: pal::TEXT_MUTED,
            placeholder: pal::INPUT_PLACEHOLDER,
            value: pal::TEXT_PRIMARY,
            selection: pal::ACCENT.scale_alpha(0.35),
        });
    mouse_area(input).on_press(press_inside).into()
}

// ===== 窗口控制按钮（canvas 自绘：背景悬停态与图标悬停变色需同源判定） =====

/// 按下上报部件：委托内部部件绘制与交互，另在左键按下命中时无条件上报消息。
///
/// 不用 mouse_area 包按钮的原因：iced 事件分发中子级（按钮）先处理且左键按下
/// 会被捕获，mouse_area 对已捕获事件直接跳过，按下回调永远不会触发。
pub struct PressReporter<'a, Message> {
    inner: Element<'a, Message>,
    on_press: Message,
}

impl<'a, Message> PressReporter<'a, Message> {
    pub fn new(inner: Element<'a, Message>, on_press: Message) -> Self {
        Self { inner, on_press }
    }
}

impl<'a, Message: Clone + 'static> iced::advanced::Widget<Message, Theme, iced::Renderer>
    for PressReporter<'a, Message>
{
    fn size(&self) -> iced::Size<Length> {
        self.inner.as_widget().size()
    }

    fn size_hint(&self) -> iced::Size<Length> {
        self.inner.as_widget().size_hint()
    }

    fn state(&self) -> iced::advanced::widget::tree::State {
        iced::advanced::widget::tree::State::new(())
    }

    fn children(&self) -> Vec<iced::advanced::widget::Tree> {
        vec![iced::advanced::widget::Tree::new(self.inner.as_widget())]
    }

    fn diff(&self, tree: &mut iced::advanced::widget::Tree) {
        tree.diff_children(std::slice::from_ref(&self.inner));
    }

    fn layout(
        &mut self,
        tree: &mut iced::advanced::widget::Tree,
        renderer: &iced::Renderer,
        limits: &iced::advanced::layout::Limits,
    ) -> iced::advanced::layout::Node {
        self.inner
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut iced::advanced::widget::Tree,
        layout: iced::advanced::Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn iced::advanced::widget::Operation,
    ) {
        self.inner
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut iced::advanced::widget::Tree,
        event: &iced::Event,
        layout: iced::advanced::Layout<'_>,
        cursor: iced::mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn iced::advanced::Clipboard,
        shell: &mut iced::advanced::Shell<'_, Message>,
        viewport: &iced::Rectangle,
    ) {
        self.inner.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
        // 捕获状态不检查：按钮捕获左键按下后这里仍要拿到命中事实
        if let iced::Event::Mouse(iced::mouse::Event::ButtonPressed(iced::mouse::Button::Left)) =
            event
        {
            if cursor.is_over(layout.bounds()) {
                shell.publish(self.on_press.clone());
            }
        }
    }

    fn mouse_interaction(
        &self,
        tree: &iced::advanced::widget::Tree,
        layout: iced::advanced::Layout<'_>,
        cursor: iced::mouse::Cursor,
        viewport: &iced::Rectangle,
        renderer: &iced::Renderer,
    ) -> iced::mouse::Interaction {
        self.inner.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }

    fn draw(
        &self,
        tree: &iced::advanced::widget::Tree,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        style: &iced::advanced::renderer::Style,
        layout: iced::advanced::Layout<'_>,
        cursor: iced::mouse::Cursor,
        viewport: &iced::Rectangle,
    ) {
        self.inner.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut iced::advanced::widget::Tree,
        layout: iced::advanced::Layout<'b>,
        renderer: &iced::Renderer,
        viewport: &iced::Rectangle,
        translation: iced::Vector,
    ) -> Option<iced::advanced::overlay::Element<'b, Message, Theme, iced::Renderer>> {
        self.inner
            .as_widget_mut()
            .overlay(&mut tree.children[0], layout, renderer, viewport, translation)
    }
}

impl<'a, Message: Clone + 'static> From<PressReporter<'a, Message>> for iced::Element<'a, Message> {
    fn from(reporter: PressReporter<'a, Message>) -> Self {
        iced::Element::new(reporter)
    }
}

/// 窗口控制按钮种类。
#[derive(Clone, Copy, PartialEq)]
pub enum WindowButtonKind {
    Minimize,
    Maximize,
    Close,
}

/// 标题栏窗口控制按钮（46×36；关闭钮悬停 = 警告红底 + 白色图标）。
pub fn window_button(kind: WindowButtonKind, msg: Message) -> Element<'static, Message> {
    iced::widget::canvas(WindowButtonWidget { kind, msg })
        .width(Length::Fixed(46.0))
        .height(Length::Fixed(36.0))
        .into()
}

struct WindowButtonWidget {
    kind: WindowButtonKind,
    msg: Message,
}

impl iced::widget::canvas::Program<Message> for WindowButtonWidget {
    type State = ();

    fn mouse_interaction(
        &self,
        _state: &Self::State,
        bounds: iced::Rectangle,
        cursor: iced::mouse::Cursor,
    ) -> iced::mouse::Interaction {
        if cursor.is_over(bounds) {
            iced::mouse::Interaction::Pointer
        } else {
            iced::mouse::Interaction::default()
        }
    }

    fn update(
        &self,
        _state: &mut Self::State,
        event: &iced::Event,
        bounds: iced::Rectangle,
        cursor: iced::mouse::Cursor,
    ) -> Option<iced::widget::canvas::Action<Message>> {
        use iced::widget::canvas::Action;
        if let iced::Event::Mouse(iced::mouse::Event::ButtonPressed(iced::mouse::Button::Left)) = event
        {
            if cursor.is_over(bounds) {
                return Some(Action::publish(self.msg.clone()).and_capture());
            }
        }
        None
    }

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &iced::Renderer,
        _theme: &iced::Theme,
        bounds: iced::Rectangle,
        cursor: iced::mouse::Cursor,
    ) -> Vec<iced::widget::canvas::Geometry> {
        use iced::widget::canvas::{Frame, LineCap, LineDash, LineJoin, Path, Stroke, Style};

        let hovered = cursor.is_over(bounds);
        let mut frame = Frame::new(renderer, bounds.size());
        if hovered {
            let bg = match self.kind {
                WindowButtonKind::Close => pal::TITLEBAR_CLOSE_HOVER,
                _ => pal::TITLEBAR_HOVER,
            };
            frame.fill_rectangle(bounds.position(), bounds.size(), bg);
        }
        let icon_color = if hovered && self.kind == WindowButtonKind::Close {
            Color::WHITE
        } else {
            pal::TITLEBAR_ICON
        };
        let stroke = Stroke {
            style: Style::Solid(icon_color),
            width: 1.0,
            line_cap: LineCap::Square,
            line_join: LineJoin::Miter,
            line_dash: LineDash { segments: &[], offset: 0 },
        };
        // 10×10 图标居中
        let (cx, cy) = (bounds.width / 2.0, bounds.height / 2.0);
        let path = match self.kind {
            WindowButtonKind::Minimize => Path::new(|p| {
                p.move_to(iced::Point::new(cx - 5.0, cy));
                p.line_to(iced::Point::new(cx + 5.0, cy));
            }),
            WindowButtonKind::Maximize => Path::new(|p| {
                p.move_to(iced::Point::new(cx - 4.0, cy - 4.0));
                p.line_to(iced::Point::new(cx + 4.0, cy - 4.0));
                p.line_to(iced::Point::new(cx + 4.0, cy + 4.0));
                p.line_to(iced::Point::new(cx - 4.0, cy + 4.0));
                p.line_to(iced::Point::new(cx - 4.0, cy - 4.0));
            }),
            WindowButtonKind::Close => Path::new(|p| {
                p.move_to(iced::Point::new(cx - 4.0, cy - 4.0));
                p.line_to(iced::Point::new(cx + 4.0, cy + 4.0));
                p.move_to(iced::Point::new(cx + 4.0, cy - 4.0));
                p.line_to(iced::Point::new(cx - 4.0, cy + 4.0));
            }),
        };
        frame.stroke(&path, stroke);
        vec![frame.into_geometry()]
    }
}
