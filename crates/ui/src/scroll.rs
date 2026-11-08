//! 可滚动容器：内嵌滚动条基元。
//!
//! 滚动条占布局空间（内容边界从滚动条内缘算起，不浮在内容之上），
//! 仅内容溢出该方向时才出现并占位（动态占位，不溢出时零成本）。
//! 外观与表格自绘滚动条同语言：BG_SUNKEN 轨道 + 滑块三态
//! （BORDER_STRONG / 悬停 TEXT_SECONDARY / 拖动 ACCENT）。
//! `Direction::Both` 的上游实现不为滚动条预留空间（0.14.2 布局只认单向），
//! 需要双向滚动的场景（笔记代码块）保持浮层，不用本基元。

use iced::widget::scrollable;
use iced::widget::scrollable::{Direction, Scrollbar};
use iced::widget::Scrollable;
use iced::{Element, Theme, border};

use super::palette as pal;

/// 滚动条厚度（px；与表格自绘滚动条同宽）。
pub const SCROLLBAR_W: f32 = 8.0;

/// 竖向内嵌滚动条（贴右缘；轨道内缘即内容边界）。
pub fn vertical() -> Scrollbar {
    Scrollbar::new().width(SCROLLBAR_W).scroller_width(SCROLLBAR_W).spacing(0.0)
}

/// 横向内嵌滚动条（贴底缘）。
pub fn horizontal() -> Scrollbar {
    Scrollbar::new().width(SCROLLBAR_W).scroller_width(SCROLLBAR_W).spacing(0.0)
}

/// 隐藏滚动条（宽 0：只滚不显、不占位；无溢出需求的滚动面用）。
pub fn hidden() -> Scrollbar {
    Scrollbar::new().width(0.0).scroller_width(0.0)
}

/// 竖向滚动容器（内嵌滚动条 + 统一样式）。
pub fn v<'a, Message>(
    content: impl Into<Element<'a, Message>>,
) -> Scrollable<'a, Message, Theme>
where
    Message: 'a,
{
    scrollable(content).direction(Direction::Vertical(vertical())).style(style)
}

/// 横向滚动容器（内嵌滚动条 + 统一样式）。
pub fn h<'a, Message>(
    content: impl Into<Element<'a, Message>>,
) -> Scrollable<'a, Message, Theme>
where
    Message: 'a,
{
    scrollable(content).direction(Direction::Horizontal(horizontal())).style(style)
}

/// 内嵌竖向滚动容器，厚度随宿主缩放（画布卡正文等整体按 zoom 等比缩放的场景）：
/// 厚度 = 8 × zoom、下限 2px（过薄不可点不可见），与卡内文字同比例，观感同 1× 面板。
pub fn v_scaled<'a, Message>(
    content: impl Into<Element<'a, Message>>,
    zoom: f32,
) -> Scrollable<'a, Message, Theme>
where
    Message: 'a,
{
    let w = (SCROLLBAR_W * zoom).max(2.0);
    scrollable(content)
        .direction(Direction::Vertical(vertical().width(w).scroller_width(w)))
        .style(style)
}

/// 滚动条样式：轨道 BG_SUNKEN；滑块三态按各自滚动条的悬停 / 拖动分别取色。
pub fn style(_theme: &Theme, status: scrollable::Status) -> scrollable::Style {
    let rail = |hovered: bool, dragged: bool| {
        let scroller = if dragged {
            pal::ACCENT
        } else if hovered {
            pal::TEXT_SECONDARY
        } else {
            pal::BORDER_STRONG
        };
        scrollable::Rail {
            background: Some(pal::BG_SUNKEN.into()),
            border: border::Border::default(),
            scroller: scrollable::Scroller {
                background: iced::Background::Color(scroller),
                border: border::Border::default(),
            },
        }
    };
    let (v_hover, h_hover, v_drag, h_drag) = match status {
        scrollable::Status::Active { .. } => (false, false, false, false),
        scrollable::Status::Hovered {
            is_horizontal_scrollbar_hovered,
            is_vertical_scrollbar_hovered,
            ..
        } => (
            is_vertical_scrollbar_hovered,
            is_horizontal_scrollbar_hovered,
            false,
            false,
        ),
        scrollable::Status::Dragged {
            is_horizontal_scrollbar_dragged,
            is_vertical_scrollbar_dragged,
            ..
        } => (
            false,
            false,
            is_vertical_scrollbar_dragged,
            is_horizontal_scrollbar_dragged,
        ),
    };
    scrollable::Style {
        container: iced::widget::container::Style::default(),
        vertical_rail: rail(v_hover, v_drag),
        horizontal_rail: rail(h_hover, h_drag),
        gap: None,
        auto_scroll: scrollable::AutoScroll {
            background: iced::Background::Color(iced::Color::TRANSPARENT),
            border: border::Border::default(),
            shadow: iced::Shadow::default(),
            icon: iced::Color::TRANSPARENT,
        },
    }
}
