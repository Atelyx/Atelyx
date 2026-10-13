//! 日历月视图网格：7 列日期格（周一起始、首尾补齐整周），今天 accent 描边 / 选中淡底 /
//! 日程色点（最多 3 点 + 「+N」）。

use std::collections::HashMap;

use iced::widget::{button, column, container, space, text};
use iced::{Element, Length, Padding, border};

use atelyx_core::calendar::CalendarItem;

use crate::calendar_actions::CalendarMsg;
use crate::calendar_grid;
use crate::calendar_view::{display_month, dot, item_color, today};
use crate::ui::palette as pal;
use crate::{App, Message};

/// 月视图格子高度。
const DAY_CELL_H: f32 = 40.0;
/// 月格底部日程色点数上限（超出显示「+N」）。
const DAY_DOTS: usize = 3;
/// 色点直径。
const DOT_PX: f32 = 6.0;

/// 月视图网格。
pub fn month_grid(app: &App) -> Element<'static, Message> {
    let (y, m) = display_month(app);
    let cells = calendar_grid::month_cells(y, m);
    let first = calendar_grid::days_from_civil(y, m, 1);
    let today = today();
    let by_day = items_by_day(&app.calendar.items);
    let mut grid = iced::widget::Column::<Message>::new().spacing(2);
    let mut head = iced::widget::Row::<Message>::new().spacing(2).width(Length::Fill);
    for label in ["一", "二", "三", "四", "五", "六", "日"] {
        head = head.push(
            container(text(label).size(pal::fs_micro()).color(pal::TEXT_MUTED))
                .width(Length::Fill)
                .center_x(Length::Fill),
        );
    }
    grid = grid.push(head);
    for week in cells.chunks(7) {
        let mut week_row = iced::widget::Row::<Message>::new().spacing(2).width(Length::Fill);
        for cell in week {
            week_row = week_row.push(match cell {
                Some(day) => {
                    let days = first + *day as i64 - 1;
                    let items = by_day.get(&days).map(Vec::as_slice).unwrap_or(&[]);
                    let colors: Vec<iced::Color> =
                        items.iter().take(DAY_DOTS).map(item_color).collect();
                    day_cell(
                        days,
                        *day,
                        days == today,
                        app.calendar.selected == Some(days),
                        colors,
                        items.len().saturating_sub(DAY_DOTS),
                    )
                }
                None => container(space())
                    .width(Length::Fill)
                    .height(Length::Fixed(DAY_CELL_H))
                    .into(),
            });
        }
        grid = grid.push(week_row);
    }
    grid.width(Length::Fill).into()
}

/// 日程按本地日归组（非法 date 跳过）。
fn items_by_day(items: &[CalendarItem]) -> HashMap<i64, Vec<CalendarItem>> {
    let mut map: HashMap<i64, Vec<CalendarItem>> = HashMap::new();
    for it in items {
        if let Some(day) = calendar_grid::parse_date(&it.date) {
            map.entry(day).or_default().push(it.clone());
        }
    }
    map
}

/// 单格：日号 + 日程色点。
fn day_cell(
    days: i64,
    day: u32,
    is_today: bool,
    is_selected: bool,
    colors: Vec<iced::Color>,
    extra: usize,
) -> Element<'static, Message> {
    let mut body = column![].spacing(3);
    body = body.push(
        text(day.to_string())
            .size(pal::fs_caption())
            .color(if is_today { pal::ACCENT } else { pal::TEXT_PRIMARY }),
    );
    if !colors.is_empty() {
        let mut dots = iced::widget::Row::<Message>::new().spacing(2);
        for c in colors {
            dots = dots.push(dot(c, DOT_PX));
        }
        if extra > 0 {
            dots = dots.push(
                text(format!("+{extra}"))
                    .size(pal::fs_micro())
                    .color(pal::TEXT_MUTED),
            );
        }
        body = body.push(dots);
    }
    button(
        container(body)
            .width(Length::Fill)
            .height(Length::Fixed(DAY_CELL_H))
            .padding(Padding::ZERO.horizontal(6.0).top(4.0)),
    )
    .width(Length::Fill)
    .style(move |_t, status| button::Style {
        background: if is_selected {
            Some(pal::ACCENT_SOFT.into())
        } else {
            matches!(status, button::Status::Hovered | button::Status::Pressed).then_some(pal::HOVER.into())
        },
        border: if is_today {
            border::rounded(pal::RADIUS_SM).width(1.0).color(pal::ACCENT)
        } else {
            border::rounded(pal::RADIUS_SM)
        },
        ..button::Style::default()
    })
    .on_press(Message::Calendar(CalendarMsg::SelectDay(days)))
    .into()
}
