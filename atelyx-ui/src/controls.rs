//! 基础控件：按钮 / 图标按钮 / 列表行 / 徽标 / 复选框 / 分段 tab / 空态 / 分隔线 / 输入框 / 进度条。
//!
//! 颜色与尺寸档取自 [`super::palette`]；
//! hover/禁用反馈经 `button::Status` 在样式闭包内表达。

use iced::widget::{button, column, container, mouse_area, row, space, text_input};
use iced::{Color, Element, Length, Padding, Theme, border, widget::container::Style};

use super::icon::{icon, icon_resolved};
use super::palette as pal;
use super::{bold, regular, text};
use super::{UiHost, UiMsg};

/// 按钮变体（底色 + 文字色 + hover 反馈）。
#[derive(Clone, Copy, PartialEq)]
pub enum Variant {
    Secondary,
    Ghost,
    /// 强调淡底（设置入口激活态等）。
    AccentSoft,
    /// 金底实心（卡片主动作：强调按钮）。
    Primary,
    /// 深底描边（卡片次级动作）。
    Outline,
    /// 透明底 + hover 出三级底（工具条「+」等触发钮）。
    SubtleBg,
    /// 危险动作（回滚确认 / 批量重建等）：红调文字，hover 出红色淡底。
    Danger,
}

/// 文字按钮尺寸档（高度 / 水平内边距 / 字号 / 圆角）。
pub struct TextSize {
    pub h: f32,
    pub pad_x: f32,
    pub font: f32,
    pub radius: f32,
}

/// `sm`（h-6）
pub fn btn_sm() -> TextSize {
    TextSize { h: 24.0, pad_x: 8.0, font: pal::fs_micro(), radius: pal::RADIUS_SM }
}

/// 胶囊档（全圆角；卡片行尾动作按钮）。
pub fn btn_pill() -> TextSize {
    TextSize { h: 26.0, pad_x: 14.0, font: pal::fs_micro(), radius: 999.0 }
}

/// 常规档（h 32；插件控件的 `Md` 尺寸）。
pub fn btn_md() -> TextSize {
    TextSize { h: 32.0, pad_x: 12.0, font: pal::fs_ui(), radius: pal::RADIUS_SM }
}

/// 宽松档（h 40；插件控件的 `Lg` 尺寸）。
pub fn btn_lg() -> TextSize {
    TextSize { h: 40.0, pad_x: 16.0, font: pal::fs_ui(), radius: pal::RADIUS_MD }
}

/// 图标按钮边长档（与文字按钮高度同档）。
pub const ICON_MD: f32 = 28.0;
pub const ICON_LG: f32 = 32.0;
pub const ICON_XL: f32 = 40.0;

struct VariantStyle {
    bg: Color,
    bg_hover: Color,
    text: Color,
    text_hover: Color,
    /// 描边色；TRANSPARENT = 无描边。
    border: Color,
}

fn variant_style(v: Variant) -> VariantStyle {
    match v {
        Variant::Secondary => VariantStyle {
            bg: pal::BG_TERTIARY,
            bg_hover: pal::HOVER,
            text: pal::TEXT_PRIMARY,
            text_hover: pal::TEXT_PRIMARY,
            border: Color::TRANSPARENT,
        },
        Variant::Ghost => VariantStyle {
            bg: Color::TRANSPARENT,
            bg_hover: pal::HOVER,
            text: pal::TEXT_SECONDARY,
            text_hover: pal::TEXT_PRIMARY,
            border: Color::TRANSPARENT,
        },
        Variant::AccentSoft => VariantStyle {
            bg: pal::ACCENT_SOFT,
            bg_hover: pal::ACCENT_SOFT,
            text: pal::ACCENT,
            text_hover: pal::ACCENT,
            border: Color::TRANSPARENT,
        },
        Variant::Primary => VariantStyle {
            bg: pal::ACCENT,
            bg_hover: pal::ACCENT_HOVER,
            text: pal::ACCENT_FG,
            text_hover: pal::ACCENT_FG,
            border: Color::TRANSPARENT,
        },
        Variant::Outline => VariantStyle {
            bg: pal::BG_SUNKEN,
            bg_hover: pal::BG_OVERLAY,
            text: pal::TEXT_PRIMARY,
            text_hover: pal::TEXT_PRIMARY,
            border: pal::BORDER_STRONG,
        },
        Variant::SubtleBg => VariantStyle {
            bg: Color::TRANSPARENT,
            bg_hover: pal::BG_TERTIARY,
            text: pal::TEXT_MUTED,
            text_hover: pal::TEXT_PRIMARY,
            border: Color::TRANSPARENT,
        },
        Variant::Danger => VariantStyle {
            bg: Color::TRANSPARENT,
            bg_hover: pal::DANGER.scale_alpha(0.14),
            text: pal::DANGER,
            text_hover: pal::DANGER,
            border: Color::TRANSPARENT,
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
            border: border::rounded(radius)
                .color(s.border)
                .width(if s.border == Color::TRANSPARENT { 0.0 } else { 1.0 }),
            ..button::Style::default()
        }
    }
}

/// 文字按钮（文字色走样式闭包：hover 换色经 text_hover 生效；内容垂直居中）。
pub fn btn<M: Clone + 'static>(
    label: impl Into<String>,
    size: &TextSize,
    variant: Variant,
    on_press: Option<M>,
) -> button::Button<'static, M> {
    let widget = button(
        container(
            text(label.into())
                .size(size.font)
                .font(bold()),
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

/// 档位的文字 / 图标取色（图标钮自备图形取色用）。
pub fn variant_text_color(variant: Variant) -> Color {
    variant_style(variant).text
}

/// 前置图标的文字按钮：图标与文字同排居中，尺寸档与文字按钮一致。
pub fn btn_with_icon<M: Clone + 'static>(
    label: impl Into<String>,
    icon: Element<'static, M>,
    size: &TextSize,
    variant: Variant,
    on_press: Option<M>,
) -> button::Button<'static, M> {
    let content = row![
        icon,
        text(label.into()).size(size.font).font(bold()),
    ]
    .spacing(6)
    .align_y(iced::alignment::Vertical::Center);
    let widget = button(
        container(content)
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
pub fn icon_button<M: UiHost>(
    name: &str,
    icon_px: f32,
    box_px: f32,
    variant: Variant,
    on_press: Option<M>,
    tip: Option<(iced::window::Id, String)>,
) -> Element<'static, M> {
    icon_button_el(icon(name, icon_px, variant_style(variant).text), box_px, variant, on_press, tip)
}

/// 图标按钮（图标内容由调用方给定：内置形状或插件自备图形都走这条）。
pub fn icon_button_el<M: UiHost>(
    icon_el: Element<'static, M>,
    box_px: f32,
    variant: Variant,
    on_press: Option<M>,
    tip: Option<(iced::window::Id, String)>,
) -> Element<'static, M> {
    let radius = if box_px <= 20.0 { pal::RADIUS_XS } else if box_px <= ICON_MD { pal::RADIUS_SM } else { pal::RADIUS_MD };
    let mut widget = button(
        container(icon_el)
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
    let element: Element<'static, M> = widget.into();
    match tip {
        Some((window, label)) => mouse_area(element)
            .on_enter(M::ui(UiMsg::HoverTip { window, label: label.clone(), box_px }))
            .on_exit(M::ui(UiMsg::HoverTipEnd { window, label }))
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
pub fn status_pill<M: 'static>(status: PillStatus, label: impl Into<String>) -> Element<'static, M> {
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
        text(label.into()).size(pal::fs_micro()).color(color),
    ]
    .spacing(6)
    .into()
}

/// 空态（图标 + 标题 + 描述 + 可选动作）。
pub fn empty_state<M: Clone + 'static>(
    icon_name: &str,
    title: impl Into<String>,
    desc: impl Into<String>,
    action: Option<(String, M)>,
) -> Element<'static, M> {
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
            .size(pal::fs_body())
            .color(pal::TEXT_PRIMARY)
            .font(bold()),
        text(desc.into())
            .size(pal::fs_ui())
            .color(pal::TEXT_MUTED),
    ]
    .spacing(8)
    .align_x(iced::alignment::Horizontal::Center);
    if let Some((label, msg)) = action {
        col = col.push(
            container(btn(label, &btn_sm(), Variant::Secondary, Some(msg)))
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
pub fn view_placeholder<M: 'static>(
    icon_name: &str,
    title: impl Into<String>,
    desc: impl Into<String>,
) -> Element<'static, M> {
    container(
        column![
            icon_resolved(icon_name, 64.0, pal::TEXT_MUTED.scale_alpha(0.6), 1.5),
            text(title.into())
                .size(pal::fs_h1())
                .color(pal::TEXT_PRIMARY)
                .font(bold()),
            text(desc.into())
                .size(pal::fs_body())
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
pub fn h_divider_subtle<M: 'static>() -> Element<'static, M> {
    h_divider_with(pal::BORDER_SUBTLE)
}

fn h_divider_with<M: 'static>(color: iced::Color) -> Element<'static, M> {
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
pub fn inline_input<M: Clone + 'static>(
    id: iced::widget::Id,
    placeholder: &str,
    value: &str,
    width: f32,
    on_input: impl Fn(String) -> M + 'static,
    on_submit: M,
    press_inside: M,
) -> Element<'static, M> {
    let input = text_input(placeholder, value)
        .id(id)
        .size(pal::fs_caption())
        .font(regular())
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
            selection: pal::SELECTION,
        });
    // 按下上报走 PressReporter：输入框自行捕获左键按下（摆光标 / 起拖选），mouse_area 会漏报
    PressReporter::new(input.into(), press_inside).into()
}

// ===== 开关 / 下拉触发 / 数字输入（设置表单基元） =====

/// 开关几何：轨道 44×24 + 滑块 20，内边距 2。
const TOGGLE_TRACK_W: f32 = 44.0;
const TOGGLE_TRACK_H: f32 = 24.0;
const TOGGLE_KNOB: f32 = 20.0;
const TOGGLE_PAD: f32 = 2.0;
/// 滑块位移（关态贴左、开态贴右）。
const TOGGLE_SHIFT: f32 = TOGGLE_TRACK_W - TOGGLE_KNOB - TOGGLE_PAD;

/// 开关（44×24 轨道 + 20px 滑块；开 = 强调底，关 = 内陷底）。
/// `on_toggle` None = 禁用展示：轨道与滑块取弱化色，与可用态区分。
pub fn toggle<M: Clone + 'static>(on: bool, on_toggle: Option<M>) -> Element<'static, M> {
    let enabled = on_toggle.is_some();
    let knob = container(space())
        .width(Length::Fixed(TOGGLE_KNOB))
        .height(Length::Fixed(TOGGLE_KNOB))
        .style(move |_t| Style {
            background: Some(if on {
                Color::WHITE
            } else if enabled {
                pal::TEXT_MUTED
            } else {
                pal::TEXT_MUTED.scale_alpha(0.5)
            }
            .into()),
            border: border::rounded(TOGGLE_KNOB / 2.0),
            ..Style::default()
        });
    let track = container(knob)
        .width(Length::Fixed(TOGGLE_TRACK_W))
        .height(Length::Fixed(TOGGLE_TRACK_H))
        .padding(Padding::ZERO.left(if on { TOGGLE_SHIFT } else { TOGGLE_PAD }).right(if on { TOGGLE_PAD } else { TOGGLE_SHIFT }))
        .align_y(iced::alignment::Vertical::Center)
        .style(move |_t| Style {
            background: Some(if on {
                if enabled { pal::ACCENT } else { pal::ACCENT.scale_alpha(0.4) }
            } else {
                pal::BG_SUNKEN
            }
            .into()),
            border: border::rounded(TOGGLE_TRACK_H / 2.0)
                .color(if enabled { pal::BORDER } else { pal::BORDER.scale_alpha(0.5) })
                .width(1.0),
            ..Style::default()
        });
    let widget = button(track).padding(0.0).style(|_t, _s| button::Style {
        ..Default::default()
    });
    match on_toggle {
        Some(m) => widget.on_press(m),
        None => widget,
    }
    .into()
}

/// 下拉触发钮（当前值 + 行尾 chevron；选项弹层由调用方经 popup 系统打开）。
/// 高度与表单输入一致（内边距 6 + caption 行高）；`width` 取 `Length`（表单固定宽、面板内容填满）。
/// 底与边取表单输入同档（`form_input_style`：BG_TERTIARY + BORDER_STRONG）——两者是同一视觉角色，
/// 同层同色，不得用下沉底把它们拉到更低层级。
pub fn dropdown_trigger<M: Clone + 'static>(label: &str, width: impl Into<Length>, on_open: Option<M>) -> Element<'static, M> {
    let content = row![
        text(label.to_string()).size(pal::fs_caption()).color(pal::TEXT_PRIMARY),
        space().width(Length::Fill),
        icon("chevron-down", 12.0, pal::TEXT_MUTED),
    ]
    .spacing(6);
    let widget = button(
        container(content)
            .width(Length::Fill)
            .padding(Padding::ZERO.horizontal(8.0))
            .height(Length::Fixed(30.0))
            .align_y(iced::alignment::Vertical::Center),
    )
    .width(width.into())
    .padding(0.0)
    .style(move |_t, _status| button::Style {
        background: Some(pal::BG_TERTIARY.into()),
        border: border::rounded(pal::RADIUS_SM).color(pal::BORDER_STRONG).width(1.0),
        ..Default::default()
    });
    match on_open {
        Some(m) => widget.on_press(m),
        None => widget,
    }
    .into()
}

/// 表单输入（设置页表单项；Enter 提交，输入过滤语义由调用方承担）。
pub fn form_input<M: UiHost>(
    placeholder: &str,
    value: &str,
    width: f32,
    on_input: impl Fn(String) -> M + 'static,
    on_submit: M,
) -> Element<'static, M> {
    let (on_input, on_submit) = arm_input(on_input, on_submit);
    styled_input(placeholder, value, Length::Fixed(width), Some(on_input), Some(on_submit), false)
}

/// 搜索输入（填满可用宽度；逐键回发即时过滤，无提交语义）。
pub fn search_input<M: Clone + 'static>(
    placeholder: &str,
    value: &str,
    on_input: impl Fn(String) -> M + 'static,
) -> Element<'static, M> {
    styled_input(placeholder, value, Length::Fill, Some(on_input), None, false)
}

/// 设置输入双提交装配：逐键武装失焦提交消息（同一提交消息也挂在 Enter 上），
/// 点击输入区外即收口（见 UiMsg::CommitPendingInput）。
fn arm_input<M: UiHost>(
    on_input: impl Fn(String) -> M + 'static,
    on_submit: M,
) -> (impl Fn(String) -> M, M) {
    let submit = on_submit.clone();
    let wrapped = move |v: String| {
        M::ui(UiMsg::InputEdit {
            edit: Box::new(on_input(v)),
            arm: Box::new(on_submit.clone()),
        })
    };
    (
        wrapped,
        M::ui(UiMsg::InputSubmit { commit: Box::new(submit) }),
    )
}

/// 只读展示输入（无输入/提交处理：值由服务端管理等场景）。
pub fn form_input_static<M: Clone + 'static>(placeholder: &str, value: &str, width: f32) -> Element<'static, M> {
    styled_input(placeholder, value, Length::Fixed(width), None::<fn(String) -> M>, None, false)
}

/// 密文输入（API key 等敏感值：掩码显示，其余同 form_input）。
pub fn secret_input<M: UiHost>(
    placeholder: &str,
    value: &str,
    width: f32,
    on_input: impl Fn(String) -> M + 'static,
    on_submit: M,
) -> Element<'static, M> {
    let (on_input, on_submit) = arm_input(on_input, on_submit);
    styled_input(placeholder, value, Length::Fixed(width), Some(on_input), Some(on_submit), true)
}

/// 表单输入统一样式（设置表单与弹层表单同口径）。
pub fn form_input_style(status: text_input::Status) -> text_input::Style {
    text_input::Style {
        background: pal::BG_TERTIARY.into(),
        border: border::rounded(pal::RADIUS_SM)
            .color(if matches!(status, text_input::Status::Focused { .. }) { pal::ACCENT } else { pal::BORDER_STRONG })
            .width(1.0),
        icon: pal::TEXT_MUTED,
        placeholder: pal::INPUT_PLACEHOLDER,
        value: pal::TEXT_PRIMARY,
        selection: pal::SELECTION,
    }
}

/// form_input / secret_input / search_input 共用的样式构建（处理器缺省 = 不可编辑）。
fn styled_input<M: Clone + 'static>(
    placeholder: &str,
    value: &str,
    width: Length,
    on_input: Option<impl Fn(String) -> M + 'static>,
    on_submit: Option<M>,
    secure: bool,
) -> Element<'static, M> {
    let mut input = text_input(placeholder, value)
        .size(pal::fs_caption())
        .font(regular())
        .padding(Padding::ZERO.horizontal(8.0).vertical(6.0))
        .width(width)
        .secure(secure)
        .style(|_theme, status| form_input_style(status));
    if let Some(f) = on_input {
        input = input.on_input(f);
    }
    if let Some(m) = on_submit {
        input = input.on_submit(m);
    }
    input.into()
}

/// 数字输入（数字过滤与草稿提交语义由调用方承担；Enter 提交）。
pub fn number_input<M: UiHost>(
    value: &str,
    width: f32,
    on_input: impl Fn(String) -> M + 'static,
    on_submit: M,
) -> Element<'static, M> {
    form_input("", value, width, on_input, on_submit)
}

// ===== 列表行样式 / 胶囊徽标 / 键串 / 复选框 / 分段 tab / 进度条 =====

/// 列表行按钮样式：透明底、悬停/按下 HOVER 底、`selected` = ACCENT_SOFT 底。
/// 内容文字色由文字部件自带，样式层只兜底主文字色。
pub fn row_style(selected: bool, radius: f32) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_t, status| button::Style {
        background: if selected {
            Some(pal::ACCENT_SOFT.into())
        } else if matches!(status, button::Status::Hovered | button::Status::Pressed) {
            Some(pal::HOVER.into())
        } else {
            None
        },
        text_color: pal::TEXT_PRIMARY,
        border: border::rounded(radius),
        ..Default::default()
    }
}

/// 胶囊徽标（底色/字色由调用方按语义给，几何与字号统一）。
pub fn badge<M: 'static>(label: impl Into<String>, bg: Color, fg: Color) -> Element<'static, M> {
    container(text(label.into()).size(pal::fs_micro()).color(fg))
        .padding(Padding::ZERO.horizontal(8.0).vertical(2.0))
        .style(move |_t| Style {
            background: Some(bg.into()),
            border: border::rounded(999.0),
            ..Style::default()
        })
        .into()
}

/// 复选框三态（全选 / 部分 / 空）。
#[derive(Clone, Copy, PartialEq)]
pub enum CheckState {
    On,
    Partial,
    Off,
}

/// 复选框（lucide 形状：✓ 全选 / — 部分 / □ 空；`on_press` None = 禁用展示）。
pub fn checkbox<M: Clone + 'static>(state: CheckState, on_press: Option<M>) -> Element<'static, M> {
    let (glyph, color) = match state {
        CheckState::On => ("check", pal::ACCENT),
        CheckState::Partial => ("minus", pal::ACCENT),
        CheckState::Off => ("square", pal::TEXT_MUTED),
    };
    let widget = button(icon(glyph, 13.0, color))
        .padding(2.0)
        .style(|_t, status| button::Style {
            background: matches!(status, button::Status::Hovered | button::Status::Pressed)
                .then_some(pal::HOVER.into()),
            border: border::rounded(3.0),
            ..Default::default()
        });
    match on_press {
        Some(m) => widget.on_press(m),
        None => widget,
    }
    .into()
}

/// 分段 tab 条（子分区切换）：选中 ACCENT_SOFT 底 + ACCENT 字，未选悬停 HOVER 底。
/// `on_select` 收到 tab id，返回切换消息。
pub fn segmented_tabs<M: Clone + 'static>(
    tabs: &[(&'static str, &'static str)],
    active_id: &str,
    on_select: impl Fn(&'static str) -> M + 'static,
) -> Element<'static, M> {
    let mut bar = row![].spacing(4);
    for (id, label) in tabs {
        let active = *id == active_id;
        bar = bar.push(
            button(
                text((*label).to_string())
                    .size(pal::fs_caption())
                    .color(if active { pal::ACCENT } else { pal::TEXT_SECONDARY }),
            )
            .padding(Padding::ZERO.horizontal(10.0).vertical(5.0))
            .style(row_style(active, pal::RADIUS_SM))
            .on_press(on_select(id)),
        );
    }
    bar.into()
}

/// 进度条（轨道定宽；`ratio` 0–1 外值钳制；total 未知时由调用方传 0 值比例）。
pub fn progress_bar<M: 'static>(ratio: f32, track_w: f32) -> Element<'static, M> {
    container(
        container(space())
            .width(Length::Fixed(track_w * ratio.clamp(0.0, 1.0)))
            .height(Length::Fill)
            .style(|_t| Style {
                background: Some(pal::ACCENT.into()),
                border: border::rounded(2.0),
                ..Style::default()
            }),
    )
    .width(Length::Fixed(track_w))
    .height(Length::Fixed(3.0))
    .style(|_t| Style {
        background: Some(pal::BG_SUNKEN.into()),
        border: border::rounded(2.0),
        ..Style::default()
    })
    .into()
}

// ===== 窗口控制按钮（canvas 自绘：背景悬停态与图标悬停变色需同源判定） =====

/// 按下上报部件：委托内部部件绘制与交互，另在左键按下命中时无条件上报消息。
///
/// 不用 mouse_area 包按钮的原因：iced 事件分发中子级（按钮）先处理且左键按下
/// 会被捕获，mouse_area 对已捕获事件直接跳过，按下回调永远不会触发。
pub struct PressReporter<'a, M> {
    inner: Element<'a, M>,
    on_press: M,
}

impl<'a, M> PressReporter<'a, M> {
    pub fn new(inner: Element<'a, M>, on_press: M) -> Self {
        Self { inner, on_press }
    }
}

impl<'a, M: Clone + 'static> iced::advanced::Widget<M, Theme, iced::Renderer>
    for PressReporter<'a, M>
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
        shell: &mut iced::advanced::Shell<'_, M>,
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
    ) -> Option<iced::advanced::overlay::Element<'b, M, Theme, iced::Renderer>> {
        self.inner
            .as_widget_mut()
            .overlay(&mut tree.children[0], layout, renderer, viewport, translation)
    }
}

impl<'a, M: Clone + 'static> From<PressReporter<'a, M>> for iced::Element<'a, M> {
    fn from(reporter: PressReporter<'a, M>) -> Self {
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
pub fn window_button<M: Clone + 'static>(kind: WindowButtonKind, msg: M) -> Element<'static, M> {
    iced::widget::canvas(WindowButtonWidget { kind, msg })
        .width(Length::Fixed(46.0))
        .height(Length::Fixed(36.0))
        .into()
}

struct WindowButtonWidget<M> {
    kind: WindowButtonKind,
    msg: M,
}

impl<M: Clone + 'static> iced::widget::canvas::Program<M> for WindowButtonWidget<M> {
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
    ) -> Option<iced::widget::canvas::Action<M>> {
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
