//! 通知宿主：右下角消息卡片（级别图标 + 文案 + 关闭），自动消失。

use std::time::{Duration, Instant};

use iced::alignment::{Horizontal, Vertical};
use iced::widget::{button, column, container, row, space, text};
use iced::{Color, Element, Length, Padding, border, widget::container::Style};

use super::icon::icon;
use super::palette as pal;
use super::UiMsg;
use crate::Message;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Success,
    Warning,
    Error,
}

impl Level {
    fn icon(self) -> &'static str {
        match self {
            Level::Info => "info",
            Level::Success => "circle-check-big",
            Level::Warning => "triangle-alert",
            Level::Error => "circle-x",
        }
    }

    fn color(self) -> iced::Color {
        match self {
            Level::Info => pal::TEXT_SECONDARY,
            Level::Success => pal::SUCCESS,
            Level::Warning => pal::WARNING,
            Level::Error => pal::DANGER,
        }
    }

    /// 自动消失时长。
    pub fn ttl(self) -> Duration {
        match self {
            Level::Info | Level::Success => Duration::from_secs(4),
            Level::Warning => Duration::from_secs(6),
            Level::Error => Duration::from_secs(8),
        }
    }
}

/// 一条通知。
#[derive(Debug, Clone)]
pub struct Notification {
    pub id: u64,
    pub level: Level,
    pub text: String,
    pub at: Instant,
}

impl Notification {
    pub fn expired(&self) -> bool {
        self.at.elapsed() >= self.level.ttl()
    }
}

/// 通知宿主（右下角、底对齐向上堆叠、边距 16px、条间距 8px；最新一条贴角在最下）。
pub fn host<'a>(list: &'a [Notification], on_dismiss: impl Fn(u64) -> Message + 'a) -> Element<'a, Message> {
    let mut col = column![].spacing(8).width(Length::Shrink);
    for n in list {
        col = col.push(card(n, on_dismiss(n.id)));
    }
    container(col)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Horizontal::Right)
        .align_y(Vertical::Bottom)
        .padding(Padding::ZERO.right(16.0).bottom(16.0))
        .into()
}

fn card<'a>(n: &'a Notification, on_dismiss: Message) -> Element<'a, Message> {
    let level = n.level;
    // 文本用 Shrink（自适应内容宽）：Fill 在 Shrink 容器内不放量会塌成 0 宽
    let body = text(n.text.clone())
        .size(pal::FS_CAPTION)
        .color(pal::TEXT_SECONDARY);
    let row = row![
        icon(level.icon(), 16.0, level.color()),
        body,
        button(icon("x", 13.0, pal::TEXT_MUTED))
            .padding(2.0)
            .style(|_t, status| button::Style {
                background: matches!(status, button::Status::Hovered | button::Status::Pressed)
                    .then_some(pal::HOVER.into()),
                ..Default::default()
            })
            .on_press(on_dismiss),
    ]
    .spacing(8);
    // 阴影与弹层同一近似：外扩半透明黑圆角底，卡片本体盖住中心
    let shadow_wrap = container(row)
        .width(Length::Shrink)
        .max_width(384)
        .padding(Padding::ZERO.horizontal(12.0).vertical(8.0))
        .style(move |_t| Style {
            background: Some(pal::BG_SECONDARY.into()),
            border: border::rounded(pal::RADIUS_SM).color(pal::BORDER).width(1.0),
            ..Style::default()
        });
    container(shadow_wrap)
        .padding(Padding { left: 3.0, top: 1.0, right: 3.0, bottom: 8.0 })
        .style(|_t| Style {
            background: Some(Color::BLACK.scale_alpha(0.35).into()),
            border: border::rounded(pal::RADIUS_SM + 3.0),
            ..Style::default()
        })
        .into()
}

/// 通知区盖在内容之上（占满窗口、透明）：仅卡片本体承接点击。
pub fn layer<'a>(list: &'a [Notification]) -> Element<'a, Message> {
    if list.is_empty() {
        return space().into();
    }
    host(list, |id| Message::Ui(UiMsg::NotifyDismiss(id)))
}
