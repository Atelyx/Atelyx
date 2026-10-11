//! 基础控件：按钮 / 图标按钮 / 状态徽标 / 空态 / 分隔线 / 输入框。
//!
//! 颜色与尺寸档取自 [`super::palette`]（与旧壳 CSS 变量一一对应）；
//! hover/禁用反馈经 `button::Status` 在样式闭包内表达。

use iced::widget::{button, column, container, mouse_area, row, space, text, text_input};
use iced::{Color, Element, Length, Padding, Theme, border, widget::container::Style};

use super::icon::icon;
use super::palette as pal;
use super::UiMsg;
use crate::Message;

/// 按钮变体（底色 + 文字色 + hover 反馈）。
#[derive(Clone, Copy, PartialEq)]
pub enum Variant {
    Secondary,
    Ghost,
    Subtle,
    /// 强调淡底（设置入口激活态等）。
    AccentSoft,
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
        Variant::Subtle => VariantStyle {
            bg: Color::TRANSPARENT,
            bg_hover: Color::TRANSPARENT,
            text: pal::TEXT_MUTED,
            text_hover: pal::TEXT_PRIMARY,
        },
        Variant::AccentSoft => VariantStyle {
            bg: pal::ACCENT_SOFT,
            bg_hover: pal::ACCENT_SOFT,
            text: pal::ACCENT,
            text_hover: pal::ACCENT,
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

/// 文字按钮（文字色走样式闭包：hover 换色经 text_hover 生效）。
pub fn btn(
    label: impl Into<String>,
    size: &TextSize,
    variant: Variant,
    on_press: Option<Message>,
) -> button::Button<'static, Message> {
    let widget = button(
        text(label.into())
            .size(size.font)
            .font(iced::Font { weight: iced::font::Weight::Medium, ..iced::Font::default() }),
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
    let mut widget = button(icon(name, icon_px, variant_style(variant).text))
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
            .on_enter(Message::Ui(UiMsg::HoverTip { window, label: label.clone() }))
            .on_exit(Message::Ui(UiMsg::HoverTipEnd { window, label }))
            .into(),
        None => element,
    }
}

/// 状态档（色 + 点形 + 文字三重编码）。
#[derive(Clone, Copy)]
pub enum PillStatus {
    /// 进行中
    Pending,
    /// 失败（方点）
    Error,
}

impl PillStatus {
    fn color(self) -> Color {
        match self {
            PillStatus::Pending => pal::WARNING,
            PillStatus::Error => pal::DANGER,
        }
    }

    fn round(self) -> bool {
        matches!(self, PillStatus::Pending)
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
                border: if status.round() {
                    border::rounded(3.0)
                } else {
                    border::rounded(1.0)
                },
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

/// 水平分隔线。
pub fn h_divider() -> Element<'static, Message> {
    container(space())
        .width(Length::Fill)
        .height(Length::Fixed(1.0))
        .style(|_t| Style {
            background: Some(pal::BORDER.into()),
            ..Style::default()
        })
        .into()
}

/// 行内输入（重命名等场景：透明底、提交走 on_submit；Escape 取消由应用层键盘路由承担）。
pub fn inline_input(
    placeholder: &str,
    value: &str,
    width: f32,
    on_input: impl Fn(String) -> Message + 'static,
    on_submit: Message,
) -> text_input::TextInput<'static, Message> {
    text_input(placeholder, value)
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
        })
}

// ===== 窗口控制按钮（canvas 自绘：背景悬停态与图标悬停变色需同源判定） =====

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
