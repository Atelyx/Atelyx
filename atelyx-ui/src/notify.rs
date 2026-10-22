//! 通知宿主：右下角消息卡片（级别图标 + 文案 + 动作按钮 + 可选进度条）。
//!
//! 常规通知按级别 TTL 自动消失；常驻通知（persistent）不参与过期，
//! 用于更新下载等长流程（进度经 UiTick 从应用层暂存刷入）。

use std::time::{Duration, Instant};

use iced::alignment::{Horizontal, Vertical};
use iced::widget::{button, column, container, row, space};
use iced::{Color, Element, Length, Padding, border, widget::container::Style};

use super::icon::icon;
use super::palette as pal;
use super::text;
use super::{UiHost, UiMsg, controls};

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

/// 下载进度（received/total 字节；total = 0 视为未知不画比例）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub received: u64,
    pub total: u64,
}

/// 一条通知。
#[derive(Debug, Clone)]
pub struct Notification<M> {
    pub id: u64,
    pub level: Level,
    pub text: String,
    pub at: Instant,
    /// 常驻（不自动过期；同一时刻至多一条，由应用层保证）。
    pub persistent: bool,
    /// 下载进度（非空 = 渲染进度条）。
    pub progress: Option<Progress>,
    /// 动作按钮（文案 + 点击消息；渲染在文案下方）。
    pub actions: Vec<(String, M)>,
}

impl<M> Notification<M> {
    /// 常规通知（自动消失、无进度无动作）。
    pub fn new(id: u64, level: Level, text: impl Into<String>) -> Self {
        Self { id, level, text: text.into(), at: Instant::now(), persistent: false, progress: None, actions: Vec::new() }
    }

    pub fn expired(&self) -> bool {
        !self.persistent && self.at.elapsed() >= self.level.ttl()
    }
}

/// 通知宿主（右下角、底对齐向上堆叠、边距 16px、条间距 8px；最新一条贴角在最下）。
pub fn host<'a, M: Clone + 'a + 'static>(list: &'a [Notification<M>], on_dismiss: impl Fn(u64) -> M + 'a) -> Element<'a, M> {
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

fn card<'a, M: Clone + 'a + 'static>(n: &'a Notification<M>, on_dismiss: M) -> Element<'a, M> {
    let level = n.level;
    // 文本用 Shrink（自适应内容宽）：Fill 在 Shrink 容器内不放量会塌成 0 宽
    let body = text(n.text.clone())
        .size(pal::fs_caption())
        .color(pal::TEXT_SECONDARY);
    let mut col = column![
        row![
            icon(level.icon(), 16.0, level.color()),
            body,
            button(icon("x", 13.0, pal::TEXT_MUTED))
                .padding(2.0)
                .style(|_t, status| button::Style {
                    background: matches!(status, button::Status::Hovered | button::Status::Pressed)
                        .then_some(pal::HOVER.into()),
                    border: border::rounded(pal::RADIUS_XS),
                    ..Default::default()
                })
                .on_press(on_dismiss),
        ]
        .spacing(8),
    ]
    .spacing(6);
    if let Some(p) = n.progress {
        // 轨道定宽：卡片整体为 Shrink 宽，Fill 链在此不放量，比例用 Fixed 像素表达
        const TRACK_W: f32 = 216.0;
        let ratio = if p.total > 0 { p.received as f32 / p.total as f32 } else { 0.0 };
        col = col.push(controls::progress_bar(ratio, TRACK_W));
    }
    if !n.actions.is_empty() {
        let mut actions = row![].spacing(6);
        for (label, msg) in &n.actions {
            actions = actions.push(controls::btn(
                label.clone(),
                &controls::btn_sm(),
                controls::Variant::Secondary,
                Some(msg.clone()),
            ));
        }
        col = col.push(actions);
    }
    // 阴影与弹层同一近似：外扩半透明黑圆角底，卡片本体盖住中心
    let shadow_wrap = container(col)
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
pub fn layer<'a, M: UiHost>(list: &'a [Notification<M>]) -> Element<'a, M> {
    if list.is_empty() {
        return space().into();
    }
    host(list, |id| M::ui(UiMsg::NotifyDismiss(id)))
}
