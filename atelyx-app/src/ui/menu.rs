//! 菜单内容模型与渲染：条目（动作/分组头/分隔线/子面板钻入/自定义块）+ 多面板导航。
//!
//! 面板（pane）机制承接「右键 → 切换视图」两级菜单：`Submenu { pane }` 钻入，
//! 一次只渲染一个面板；子面板（pane > 0）头部固定「返回 + 分隔线」。

use iced::alignment::Vertical;
use iced::widget::{column, container, row, space};
use iced::{Element, Length, Padding, widget::container::Style};

use super::icon::{icon, icon_resolved};
use super::palette as pal;
use super::scroll;
use super::text;
use super::UiMsg;
use crate::Message;

/// 一条菜单内容。渲染顺序即数组顺序。
pub enum MenuEntry<'a> {
    Action {
        label: String,
        /// 内置图标名或插件自备图形的限定名（`<插件 id>::<名>`）。
        icon: Option<String>,
        /// 危险操作：红字 + hover 红调底。
        danger: bool,
        disabled: bool,
        on_press: Option<Message>,
        /// 行尾角标（「当前/本组/已占用」）：非禁用条目文字转强调色。
        note: Option<&'static str>,
        /// 悬停提示文案（None = 无）。
        hint: Option<String>,
    },
    /// 分组头（灰底小字）。
    Group(String),
    /// 当前选中项（下拉选项行形态：强调色文字 + 右侧对勾）。
    Checked { label: String, on_press: Message },
    Divider,
    /// 钻入子面板。
    Submenu { label: String, icon: Option<String>, disabled: bool, pane: usize },
    /// 自定义内容块（高度调用方给出，用于估高与钳制）。
    Custom(Element<'a, Message>, f32),
    /// 长清单滚动区（族名表等）：内部条目按菜单行渲染，`height` = 可视区高度（同时计入估高）。
    Scroll { entries: Vec<MenuEntry<'a>>, height: f32 },
}

impl<'a> MenuEntry<'a> {
    /// 快捷构造：普通动作项。
    pub fn action(label: impl Into<String>, on_press: Message) -> Self {
        MenuEntry::Action {
            label: label.into(),
            icon: None,
            danger: false,
            disabled: false,
            on_press: Some(on_press),
            note: None,
            hint: None,
        }
    }

    pub fn action_icon(
        label: impl Into<String>,
        icon: impl Into<String>,
        on_press: Message,
    ) -> Self {
        MenuEntry::Action {
            label: label.into(),
            icon: Some(icon.into()),
            danger: false,
            disabled: false,
            on_press: Some(on_press),
            note: None,
            hint: None,
        }
    }

    pub fn danger(label: impl Into<String>, icon: impl Into<String>, on_press: Message) -> Self {
        MenuEntry::Action {
            label: label.into(),
            icon: Some(icon.into()),
            danger: true,
            disabled: false,
            on_press: Some(on_press),
            note: None,
            hint: None,
        }
    }

    /// 带行尾角标的条目（非禁用 = 强调色文字，点击动作；禁用 = 灰字无动作）。
    pub fn noted(
        label: impl Into<String>,
        icon: impl Into<String>,
        note: &'static str,
        disabled: bool,
        on_press: Option<Message>,
    ) -> Self {
        MenuEntry::Action {
            label: label.into(),
            icon: Some(icon.into()),
            danger: false,
            disabled,
            on_press,
            note: Some(note),
            hint: None,
        }
    }

    /// 带悬停提示的动作项。
    pub fn hinted(mut self, hint: impl Into<String>) -> Self {
        if let MenuEntry::Action { hint: h, .. } = &mut self {
            *h = Some(hint.into());
        }
        self
    }
}

/// 渲染一个面板（`pane` 0 = 根；`entries` 含全部面板的条目，按 pane 过滤）。
/// 子面板头部固定「返回 + 分隔线」，其余条目按数组顺序排列。
pub fn pane_view<'a>(
    entries: Vec<(usize, MenuEntry<'a>)>,
    pane: usize,
    window: iced::window::Id,
) -> (Element<'a, Message>, f32) {
    let mut col = column![].padding(Padding::ZERO.vertical(pal::POPUP_PAD_Y));
    let mut height = pal::POPUP_PAD_Y * 2.0;
    let remaining = entries.into_iter().filter(|(p, _)| *p == pane);
    if pane != 0 {
        col = col.push(back_item(window));
        col = col.push(divider());
        height += pal::MENU_ITEM_H + pal::MENU_DIVIDER_H;
    }
    for (_, entry) in remaining {
        let (element, h) = entry_element(entry, window);
        col = col.push(element);
        height += h;
    }
    (col.into(), height)
}

/// 渲染单条目，并返回它占用的竖向高度（供弹层估高与钳制）。
fn entry_element<'a>(entry: MenuEntry<'a>, window: iced::window::Id) -> (Element<'a, Message>, f32) {
    match entry {
        MenuEntry::Action { label, icon: ic, danger, disabled, on_press, note, hint } => {
            (item(&label, ic, danger, disabled, on_press, note, hint, window), pal::MENU_ITEM_H)
        }
        MenuEntry::Group(name) => (
            container(text(name).size(pal::fs_micro()).color(pal::TEXT_MUTED))
                .padding(Padding::ZERO.left(12.0).top(6.0).bottom(2.0))
                .width(Length::Fill)
                .into(),
            pal::MENU_GROUP_H,
        ),
        MenuEntry::Divider => (divider(), pal::MENU_DIVIDER_H),
        MenuEntry::Checked { label, on_press } => {
            (checked_item(&label, on_press, window), pal::MENU_ITEM_H)
        }
        MenuEntry::Submenu { label, icon: ic, disabled, pane: target } => {
            (submenu_item(&label, ic, disabled, target, window), pal::MENU_ITEM_H)
        }
        MenuEntry::Custom(element, h) => (element, h),
        MenuEntry::Scroll { entries, height } => {
            let mut col = column![];
            for entry in entries {
                col = col.push(entry_element(entry, window).0);
            }
            (
                scroll::v(col)
                    .width(Length::Fill)
                    .height(Length::Fixed(height))
                    .into(),
                height,
            )
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn item(
    label: &str,
    icon_name: Option<String>,
    danger: bool,
    disabled: bool,
    on_press: Option<Message>,
    note: Option<&'static str>,
    hint: Option<String>,
    window: iced::window::Id,
) -> Element<'static, Message> {
    // 行尾角标条目（「当前」）：无论是否禁用都着强调色；其余按 danger/常规取色
    let text_color = if note.is_some() {
        pal::ACCENT
    } else if disabled {
        pal::TEXT_MUTED
    } else if danger {
        pal::DANGER
    } else {
        pal::TEXT_PRIMARY
    };
    let hover_bg = if danger {
        pal::DANGER.scale_alpha(0.14)
    } else {
        pal::HOVER
    };
    let mut row = row![].spacing(6).width(Length::Fill);
    if let Some(name) = icon_name {
        row = row.push(icon_resolved(&name, 13.0, text_color, 1.5));
    }
    row = row.push(text(label.to_string()).size(pal::fs_caption()).color(text_color));
    if let Some(note) = note {
        row = row.push(space().width(Length::Fill));
        row = row.push(text(note.to_string()).size(pal::fs_micro()).color(text_color));
    }
    item_button(container(row), hover_bg, disabled, on_press, hint, window)
}

/// 当前选中项（下拉选项行）：强调色文字 + 行尾对勾，与普通条目同款按钮底。
fn checked_item(
    label: &str,
    on_press: Message,
    window: iced::window::Id,
) -> Element<'static, Message> {
    let mut row = row![].spacing(6).width(Length::Fill);
    row = row.push(text(label.to_string()).size(pal::fs_caption()).color(pal::ACCENT));
    row = row.push(space().width(Length::Fill));
    row = row.push(icon("check", 13.0, pal::ACCENT));
    item_button(container(row), pal::HOVER, false, Some(on_press), None, window)
}

fn submenu_item(
    label: &str,
    icon_name: Option<String>,
    disabled: bool,
    pane: usize,
    window: iced::window::Id,
) -> Element<'static, Message> {    let text_color = if disabled { pal::TEXT_MUTED } else { pal::TEXT_PRIMARY };
    let mut row = row![].spacing(6).width(Length::Fill);
    row = row.push(icon("chevron-right", 13.0, text_color));
    if let Some(name) = icon_name {
        row = row.push(icon_resolved(&name, 13.0, text_color, 1.5));
    }
    row = row.push(text(label.to_string()).size(pal::fs_caption()).color(text_color));
    if disabled {
        item_button(container(row), pal::HOVER, true, None, None, window)
    } else {
        item_button(
            container(row),
            pal::HOVER,
            false,
            Some(Message::Ui(UiMsg::PopupPane(pane))),
            None,
            window,
        )
    }
}

fn back_item(window: iced::window::Id) -> Element<'static, Message> {
    let row = row![
        icon("chevron-left", 13.0, pal::TEXT_PRIMARY),
        text("返回").size(pal::fs_caption()).color(pal::TEXT_PRIMARY),
    ]
    .spacing(6)
    .width(Length::Fill);
    item_button(
        container(row),
        pal::HOVER,
        false,
        Some(Message::Ui(UiMsg::PopupPane(0))),
        None,
        window,
    )
}

/// 菜单项按钮外壳：清零 iced 默认 padding、定高 MENU_ITEM_H、统一 hover 底；
/// `hint` 非空时经 tooltip 通道（悬停 400ms 显现、移开 120ms 收起）。
fn item_button(
    content: container::Container<'static, Message>,
    hover_bg: iced::Color,
    disabled: bool,
    on_press: Option<Message>,
    hint: Option<String>,
    window: iced::window::Id,
) -> Element<'static, Message> {
    let content = content
        .width(Length::Fill)
        .height(Length::Fixed(pal::MENU_ITEM_H))
        .padding(Padding::ZERO.horizontal(12.0))
        .align_y(Vertical::Center);
    let element: Element<'static, Message> = if disabled {
        content.into()
    } else {
        iced::widget::button(content)
            .padding(0.0)
            .width(Length::Fill)
            .style(move |_t, status| iced::widget::button::Style {
                background: matches!(status, iced::widget::button::Status::Hovered | iced::widget::button::Status::Pressed)
                    .then_some(hover_bg.into()),
                text_color: pal::TEXT_PRIMARY,
                ..Default::default()
            })
            .on_press_maybe(on_press)
            .into()
    };
    match hint {
        Some(label) => iced::widget::mouse_area(element)
            .on_enter(Message::Ui(UiMsg::HoverTip {
                window,
                label: label.clone(),
                box_px: pal::MENU_ITEM_H,
            }))
            .on_exit(Message::Ui(UiMsg::HoverTipEnd { window, label }))
            .into(),
        None => element,
    }
}

/// 分隔线：上下 4px 留白 + 1px 通栏线。
fn divider() -> Element<'static, Message> {
    container(
        container(space())
            .height(Length::Fixed(1.0))
            .width(Length::Fill)
            .style(|_t| Style {
                background: Some(pal::BORDER.into()),
                ..Style::default()
            }),
    )
    .padding(Padding::ZERO.vertical(4.0))
    .width(Length::Fill)
    .into()
}
