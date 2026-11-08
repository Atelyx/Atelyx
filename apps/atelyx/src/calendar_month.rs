//! 日历月视图网格：7 列日期格（周一起始、首尾补齐整周），今天 accent 淡底 / 选中 accent 描边 /
//! 日程条目块（主题色软底，最多 2 条 + 「+N」）。

use std::collections::HashMap;

use iced::widget::{button, column, container, mouse_area, row, space};
use iced::{Element, Length, Padding, border};

use atelyx_core::calendar::CalendarItem;

use crate::calendar_actions::CalendarMsg;
use crate::calendar_grid;
use crate::calendar_view::{display_month, dot, item_color, today};
use crate::ui::UiMsg;
use crate::ui::palette as pal;
use crate::ui::text;
use crate::{App, Message};

/// 月视图格子高度（日号 + 两条日程条目块的最小内容高，留 2px 余量）。
const DAY_CELL_H: f32 = 64.0;
/// 格内日程条目显示上限；有溢出时降为 1 条，给「+N」留行（完整列表在天详情区）。
const DAY_ITEMS: usize = 2;
/// 日程条目标题的最大估宽（ellipsize 按估宽截断，估宽刻意高于实测，截断结果不折行）。
const ITEM_TEXT_MAX_W: f32 = 96.0;
/// 条目块内色点直径。
const ITEM_DOT: f32 = 5.0;
/// 「+N」悬停提示的锚定盒边长（≈ micro 字行高）。
const ITEM_TIP_BOX: f32 = 16.0;

/// 月视图网格。
pub fn month_grid(app: &App, window: iced::window::Id) -> Element<'static, Message> {
    let (y, m) = display_month(app);
    let cells = calendar_grid::month_cells(y, m);
    let first = calendar_grid::days_from_civil(y, m, 1);
    let today = today();
    let by_day = items_by_day(&app.calendar.items);
    let mut grid = iced::widget::Column::<Message>::new().spacing(4);
    let mut head = iced::widget::Row::<Message>::new().spacing(4).width(Length::Fill);
    for label in ["一", "二", "三", "四", "五", "六", "日"] {
        head = head.push(
            container(text(label).size(pal::fs_micro()).color(pal::TEXT_MUTED))
                .width(Length::Fill)
                .center_x(Length::Fill),
        );
    }
    grid = grid.push(head);
    for week in cells.chunks(7) {
        let mut week_row = iced::widget::Row::<Message>::new().spacing(4).width(Length::Fill);
        for cell in week {
            week_row = week_row.push(match cell {
                Some(day) => {
                    let days = first + *day as i64 - 1;
                    let items = by_day.get(&days).map(Vec::as_slice).unwrap_or(&[]);
                    day_cell(
                        window,
                        days,
                        *day,
                        days == today,
                        app.calendar.selected == Some(days),
                        items,
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

/// 日程按本地日归组（非法 date 跳过；组内按创建序，与天详情列表同口径）。
fn items_by_day(items: &[CalendarItem]) -> HashMap<i64, Vec<CalendarItem>> {
    let mut map: HashMap<i64, Vec<CalendarItem>> = HashMap::new();
    for it in items {
        if let Some(day) = calendar_grid::parse_date(&it.date) {
            map.entry(day).or_default().push(it.clone());
        }
    }
    for list in map.values_mut() {
        list.sort_by_key(|it| it.created_at);
    }
    map
}

/// 单格：日号 + 日程条目块（主题色软底 + 色点 + 标题截断）。
fn day_cell(
    window: iced::window::Id,
    days: i64,
    day: u32,
    is_today: bool,
    is_selected: bool,
    items: &[CalendarItem],
) -> Element<'static, Message> {
    let extra = items.len().saturating_sub(DAY_ITEMS);
    // 有溢出时条目降为 1 条给「+N」留行：格高按两条目封顶，内容不溢出瓦片
    let shown = if extra > 0 { DAY_ITEMS - 1 } else { DAY_ITEMS };
    let mut body = column![].spacing(3);
    body = body.push(
        text(day.to_string())
            .size(pal::fs_caption())
            .color(if is_today { pal::ACCENT } else { pal::TEXT_PRIMARY }),
    );
    for it in items.iter().take(shown) {
        let c = item_color(it);
        body = body.push(
            container(
                row![
                    dot(c, ITEM_DOT),
                    text(crate::layout_view::ellipsize(&it.title, ITEM_TEXT_MAX_W, pal::fs_micro()))
                        .size(pal::fs_micro())
                        .color(pal::TEXT_PRIMARY),
                ]
                .spacing(4)
                .align_y(iced::alignment::Vertical::Center),
            )
            .width(Length::Fill)
            .padding(Padding::ZERO.horizontal(4.0).vertical(2.0))
            .style(move |_t| iced::widget::container::Style {
                background: Some(c.scale_alpha(0.15).into()),
                border: border::rounded(pal::RADIUS_XS),
                ..iced::widget::container::Style::default()
            }),
        );
    }
    if extra > 0 {
        // 溢出条目不可见：悬停「+N」提示被隐藏的日程数
        let label = format!("还有 {extra} 条日程");
        body = body.push(
            mouse_area(
                text(format!("+{extra}"))
                    .size(pal::fs_micro())
                    .color(pal::TEXT_MUTED),
            )
            .on_enter(Message::Ui(UiMsg::HoverTip {
                window,
                label: label.clone(),
                box_px: ITEM_TIP_BOX,
            }))
            .on_exit(Message::Ui(UiMsg::HoverTipEnd { window, label })),
        );
    }
    button(
        container(body)
            .width(Length::Fill)
            .height(Length::Fixed(DAY_CELL_H))
            .padding(Padding::ZERO.horizontal(6.0).top(4.0)),
    )
    .width(Length::Fill)
    .style(move |_t, status| button::Style {
        // 日期瓦片：默认三级底成卡片阵列，今天强调淡底，选中 accent 描边
        background: if is_today {
            Some(pal::ACCENT_SOFT.into())
        } else if matches!(status, button::Status::Hovered | button::Status::Pressed) {
            Some(pal::HOVER.into())
        } else {
            Some(pal::BG_TERTIARY.into())
        },
        border: if is_selected {
            border::rounded(pal::RADIUS_SM).width(1.0).color(pal::ACCENT)
        } else {
            border::rounded(pal::RADIUS_SM)
        },
        ..button::Style::default()
    })
    .on_press(Message::Calendar(CalendarMsg::SelectDay(days)))
    .into()
}
