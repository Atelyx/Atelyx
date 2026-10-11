//! 菜单内容模型与渲染：条目（动作/分组头/分隔线/子面板钻入/自定义块）+ 多面板导航。
//!
//! 面板（pane）机制承接「右键 → 切换视图」两级菜单：`Submenu { pane }` 钻入，
//! `Back` 返回根面板；一次只渲染一个面板。

use iced::alignment::Vertical;
use iced::widget::{column, container, row, space, text};
use iced::{Element, Length, Padding, widget::container::Style};

use super::icon::icon;
use super::palette as pal;
use crate::Message;

/// 一条菜单内容。渲染顺序即数组顺序。
pub enum MenuEntry<'a> {
    Action {
        label: String,
        icon: Option<&'static str>,
        /// 危险操作：红字 + hover 红调底。
        danger: bool,
        disabled: bool,
        on_press: Option<Message>,
    },
    /// 分组头（灰底小字）。
    Group(String),
    Divider,
    /// 钻入子面板。
    Submenu { label: String, icon: Option<&'static str>, disabled: bool, pane: usize },
    /// 返回根面板（仅子面板内渲染）。
    Back,
    /// 自定义内容块（高度调用方给出，用于估高与钳制）。
    Custom(Element<'a, Message>, f32),
}

impl<'a> MenuEntry<'a> {
    /// 快捷构造：普通动作项。
    pub fn action(label: impl Into<String>, on_press: Message) -> Self {
        MenuEntry::Action { label: label.into(), icon: None, danger: false, disabled: false, on_press: Some(on_press) }
    }

    pub fn action_icon(
        label: impl Into<String>,
        icon: &'static str,
        on_press: Message,
    ) -> Self {
        MenuEntry::Action { label: label.into(), icon: Some(icon), danger: false, disabled: false, on_press: Some(on_press) }
    }

    pub fn danger(label: impl Into<String>, icon: &'static str, on_press: Message) -> Self {
        MenuEntry::Action { label: label.into(), icon: Some(icon), danger: true, disabled: false, on_press: Some(on_press) }
    }

    pub fn disabled(label: impl Into<String>, icon: Option<&'static str>) -> Self {
        MenuEntry::Action { label: label.into(), icon, danger: false, disabled: true, on_press: None }
    }
}

/// 渲染一个面板（`pane` 0 = 根；`entries` 含全部面板的条目，按 pane 过滤）。
pub fn pane_view<'a>(
    entries: Vec<(usize, MenuEntry<'a>)>,
    pane: usize,
) -> (Element<'a, Message>, f32) {
    let mut col = column![].padding(Padding::ZERO.vertical(pal::POPUP_PAD_Y));
    let mut height = pal::POPUP_PAD_Y * 2.0;
    for (_, entry) in entries.into_iter().filter(|(p, _)| *p == pane) {
        let (element, h): (Element<'a, Message>, f32) = match entry {
            MenuEntry::Action { label, icon: ic, danger, disabled, on_press } => {
                (item(&label, ic, danger, disabled, on_press), pal::MENU_ITEM_H)
            }
            MenuEntry::Group(name) => (
                container(text(name).size(pal::FS_MICRO).color(pal::TEXT_MUTED))
                    .padding(Padding::ZERO.left(12.0).top(6.0).bottom(2.0))
                    .width(Length::Fill)
                    .into(),
                pal::MENU_GROUP_H,
            ),
            MenuEntry::Divider => (divider(), pal::MENU_DIVIDER_H),
            MenuEntry::Submenu { label, icon: ic, disabled, pane: target } => (
                submenu_item(&label, ic, disabled, target),
                pal::MENU_ITEM_H,
            ),
            MenuEntry::Back => (back_item(), pal::MENU_ITEM_H),
            MenuEntry::Custom(element, h) => (element, h),
        };
        col = col.push(element);
        height += h;
    }
    (col.into(), height)
}

fn item(
    label: &str,
    icon_name: Option<&'static str>,
    danger: bool,
    disabled: bool,
    on_press: Option<Message>,
) -> Element<'static, Message> {
    let text_color = if disabled {
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
        row = row.push(icon(name, 13.0, text_color));
    }
    row = row.push(text(label.to_string()).size(pal::FS_CAPTION).color(text_color));
    item_button(container(row), hover_bg, disabled, on_press)
}

fn submenu_item(
    label: &str,
    icon_name: Option<&'static str>,
    disabled: bool,
    pane: usize,
) -> Element<'static, Message> {
    let text_color = if disabled { pal::TEXT_MUTED } else { pal::TEXT_PRIMARY };
    let mut row = row![].spacing(6).width(Length::Fill);
    if let Some(name) = icon_name {
        row = row.push(icon(name, 13.0, text_color));
    }
    row = row.push(text(label.to_string()).size(pal::FS_CAPTION).color(text_color));
    row = row.push(space().width(Length::Fill));
    row = row.push(icon("chevron-right", 13.0, pal::TEXT_MUTED));
    if disabled {
        item_button(container(row), pal::HOVER, true, None)
    } else {
        item_button(
            container(row),
            pal::HOVER,
            false,
            Some(Message::Ui(super::UiMsg::PopupPane(pane))),
        )
    }
}

fn back_item() -> Element<'static, Message> {
    let row = row![
        icon("chevron-left", 13.0, pal::TEXT_PRIMARY),
        text("返回").size(pal::FS_CAPTION).color(pal::TEXT_PRIMARY),
    ]
    .spacing(6)
    .width(Length::Fill);
    item_button(
        container(row),
        pal::HOVER,
        false,
        Some(Message::Ui(super::UiMsg::PopupPane(0))),
    )
}

/// 菜单项按钮外壳：清零 iced 默认 padding、定高 MENU_ITEM_H、统一 hover 底。
fn item_button(
    content: container::Container<'static, Message>,
    hover_bg: iced::Color,
    disabled: bool,
    on_press: Option<Message>,
) -> Element<'static, Message> {
    let content = content
        .width(Length::Fill)
        .height(Length::Fixed(pal::MENU_ITEM_H))
        .padding(Padding::ZERO.horizontal(12.0))
        .align_y(Vertical::Center);
    if disabled {
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
    }
}

/// 分隔线：上下 4px 留白 + 1px 线。
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
    .padding(Padding::ZERO.vertical(4.0).horizontal(6.0))
    .width(Length::Fill)
    .into()
}
