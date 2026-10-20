//! 日历视图入口与月份导航：视图分派、月份偏移与本地「今天」的公共口径，
//! 以及月网格 / 天详情共用的色点与条目取色；分区渲染在 calendar_heat / calendar_month / calendar_day。

use iced::widget::{column, container, row, space};
use iced::{Element, Length, Padding};

use atelyx_core::calendar::CalendarItem;
use atelyx_core::history;

use crate::calendar_actions::CalendarMsg;
use crate::calendar_day;
use crate::calendar_heat;
use crate::calendar_month;
use crate::ui::controls;
use crate::ui::palette as pal;
use crate::ui::scroll;
use crate::ui::text;
use crate::ui::bold;
use crate::{App, Message, layout_view};

/// 日历视图（kind "calendar"）：垂直滚动承载各分区。
pub fn calendar_view(app: &App, window: iced::window::Id) -> Element<'_, Message> {
    if !crate::repohistory_view::container_ready(app) {
        return controls::empty_state("calendar-days", "未打开仓库", "打开仓库后显示", None);
    }
    let content = column![
        nav_bar(app, window),
        calendar_heat::heatmap(app, window),
        controls::h_divider_subtle(),
        calendar_month::month_grid(app, window),
        calendar_day::day_detail(app, window),
    ]
    .spacing(12)
    .padding(iced::Padding::ZERO.horizontal(12.0).vertical(10.0))
    .width(Length::Fill);
    scroll::v(container(content).width(Length::Fill))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// 当前导航到的月份（本月 + month_offset）。
pub(crate) fn display_month(app: &App) -> (i64, u32) {
    let (cy, cm, _) = crate::calendar_grid::civil_from_days(today());
    crate::calendar_grid::month_add(cy, cm, app.calendar.month_offset)
}

/// 本地当下（自纪元天数，本地日界）。
pub(crate) fn today() -> i64 {
    history::day_key(crate::now_secs() as i64 * 1000)
}

/// 条目色（缺省 = 强调色）。
pub(crate) fn item_color(item: &CalendarItem) -> iced::Color {
    item.color
        .as_deref()
        .and_then(layout_view::parse_hex_color)
        .unwrap_or(pal::ACCENT)
}

/// 圆点（直径 size）。
pub(crate) fn dot(color: iced::Color, size: f32) -> Element<'static, Message> {
    container(iced::widget::space())
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .style(move |_| iced::widget::container::Style {
            background: Some(color.into()),
            border: iced::border::rounded(size / 2.0),
            ..iced::widget::container::Style::default()
        })
        .into()
}

/// 顶部月份导航：‹ YYYY年M月 › 组偏左居中，「今天」贴右侧。
fn nav_bar(app: &App, window: iced::window::Id) -> Element<'static, Message> {
    let (y, m) = display_month(app);
    row![
        space().width(Length::Fill),
        controls::icon_button(
            "chevron-left",
            13.0,
            24.0,
            controls::Variant::Secondary,
            Some(Message::Calendar(CalendarMsg::PrevMonth)),
            Some((window, "上一月".into())),
        ),
        container(
            text(format!("{y}年{m}月"))
                .size(pal::fs_body())
                .font(bold())
        )
        .padding(Padding::ZERO.horizontal(4.0)),
        controls::icon_button(
            "chevron-right",
            13.0,
            24.0,
            controls::Variant::Secondary,
            Some(Message::Calendar(CalendarMsg::NextMonth)),
            Some((window, "下一月".into())),
        ),
        space().width(Length::Fill),
        controls::btn(
            "今天",
            &controls::btn_sm(),
            controls::Variant::Secondary,
            Some(Message::Calendar(CalendarMsg::Today)),
        ),
    ]
    .spacing(6)
    .width(Length::Fill)
    .into()
}
