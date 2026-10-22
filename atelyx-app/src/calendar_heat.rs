//! 日历活动热力图：导航月居中的奇数月份窗口（至多 13 个月，按可用宽度收放），
//! 余量均分到月块之间；四档色阶；本地密度走活动缓存，空间按仓库历史聚合计数。
//! 横向拖拽超过阈值翻月（按压起点与松开收尾见 calendar_actions）。

use std::collections::HashMap;

use iced::widget::container::Style;
use iced::widget::{container, mouse_area, responsive, space, Column, Row};
use iced::{Element, Length, border};

use crate::calendar_actions::CalendarMsg;
use crate::calendar_grid;
use crate::calendar_view::display_month;
use crate::ui::palette as pal;
use crate::ui::text;
use crate::ui::UiMsg;
use crate::{App, Message, RepoKind};

/// 热力图格子边长与间距。
const CELL: f32 = 11.0;
const CELL_GAP: f32 = 2.0;
/// 月块最小间距（可用宽有余时余量均分进月块之间）。
const BLOCK_GAP: f32 = 10.0;
/// 热力图窗口月数上限（奇数，导航月居中）。
const MAX_MONTHS: u32 = 13;
/// 色阶透明度（1-3 档 accent 递增；0 档 = BG_TERTIARY）。
const LEVEL_ALPHAS: [f32; 3] = [0.25, 0.55, 0.9];

/// 活动热力图：导航月居中，按可用宽度容纳尽可能多的月份（余量均分，无滚动条）。
pub fn heatmap(app: &App, window: iced::window::Id) -> Element<'static, Message> {
    let counts = activity_counts(app);
    let (ey, em) = display_month(app);
    let mut col = Column::<Message>::new().push(
        responsive(move |size| month_strip(window, ey, em, size.width, &counts))
            .width(Length::Fill),
    );
    // 装载失败 / 在途以一行微字提示（热力图本体按空档渲染）
    match app.calendar.activity.as_ref() {
        Some(Err(e)) => {
            col = col.push(
                text(format!("活动统计失败：{e}"))
                    .size(pal::fs_micro())
                    .color(pal::TEXT_MUTED),
            );
        }
        None if matches!(app.repo_kind, RepoKind::Local) => {
            col = col.push(
                text("活动统计中…")
                    .size(pal::fs_micro())
                    .color(pal::TEXT_MUTED),
            );
        }
        _ => {}
    }
    col.into()
}

/// 单行月块条：块间以 Fill 均分余量；整条可横向拖拽翻月。
fn month_strip(
    window: iced::window::Id,
    base_y: i64,
    base_m: u32,
    available: f32,
    counts: &HashMap<i64, u32>,
) -> Element<'static, Message> {
    let months = fitted_months(base_y, base_m, available);
    let mut row = Row::<Message>::new().width(Length::Fill);
    for (i, (y, m)) in calendar_grid::month_window_around(base_y, base_m, months)
        .into_iter()
        .enumerate()
    {
        if i > 0 {
            row = row.push(space().width(Length::Fill));
        }
        row = row.push(month_block(window, y, m, counts));
    }
    mouse_area(row)
        .interaction(iced::mouse::Interaction::Grab)
        .on_press(Message::Calendar(CalendarMsg::HeatPress(window)))
        .into()
}

/// 可用宽度能容纳的最大奇数月数（至少 1 个月）。
fn fitted_months(base_y: i64, base_m: u32, available: f32) -> u32 {
    (1..=MAX_MONTHS)
        .rev()
        .find(|n| n % 2 == 1 && strip_width(base_y, base_m, *n) <= available)
        .unwrap_or(1)
}

/// 整条最小宽度：各月块宽 + 块间最小间距。
fn strip_width(base_y: i64, base_m: u32, months: u32) -> f32 {
    calendar_grid::month_window_around(base_y, base_m, months)
        .into_iter()
        .enumerate()
        .map(|(i, (y, m))| block_width(y, m) + if i > 0 { BLOCK_GAP } else { 0.0 })
        .sum()
}

/// 月块宽：周列数 × 格宽 + 列间距。
fn block_width(y: i64, m: u32) -> f32 {
    let weeks = (calendar_grid::month_cells(y, m).len() / 7) as f32;
    weeks * CELL + (weeks - 1.0) * CELL_GAP
}

/// 单月块：月份标签 + 周列网格（周一~周日 7 行）。
fn month_block(
    window: iced::window::Id,
    y: i64,
    m: u32,
    counts: &HashMap<i64, u32>,
) -> Element<'static, Message> {
    let cells = calendar_grid::month_cells(y, m);
    let first = calendar_grid::days_from_civil(y, m, 1);
    let mut weeks_row = Row::<Message>::new().spacing(CELL_GAP);
    for w in 0..cells.len() / 7 {
        let mut week_col = Column::<Message>::new().spacing(CELL_GAP);
        for wd in 0..7 {
            week_col = week_col.push(match cells[w * 7 + wd] {
                Some(day) => {
                    let days = first + day as i64 - 1;
                    let count = counts.get(&days).copied().unwrap_or(0);
                    heat_cell(window, days, count)
                }
                None => blank_cell(),
            });
        }
        weeks_row = weeks_row.push(week_col);
    }
    Column::<Message>::new()
        .spacing(3)
        .align_x(iced::Alignment::Center)
        .push(
            text(calendar_grid::month_label(y, m))
                .size(pal::fs_micro())
                .color(pal::TEXT_MUTED),
        )
        .push(weeks_row)
        .into()
}

/// 活动密度：本地 = 活动缓存；空间 = 仓库历史聚合按日计数。
fn activity_counts(app: &App) -> HashMap<i64, u32> {
    match &app.repo_kind {
        RepoKind::Local => app
            .calendar
            .activity
            .as_ref()
            .and_then(|r| r.as_ref().ok())
            .cloned()
            .unwrap_or_default(),
        RepoKind::Space(_) => app
            .repo_history
            .as_ref()
            .and_then(|r| r.as_ref().ok())
            .map(|groups| calendar_grid::activity_from_groups(groups))
            .unwrap_or_default(),
    }
}

/// 单格：色阶底 + 悬停提示「M月d日 · N 条活动」。
fn heat_cell(window: iced::window::Id, days: i64, count: u32) -> Element<'static, Message> {
    let (_y, m, d) = calendar_grid::civil_from_days(days);
    let label = format!("{m}月{d}日 · {count} 条活动");
    let bg = match calendar_grid::activity_level(count) {
        0 => pal::BG_TERTIARY,
        i => pal::ACCENT.scale_alpha(LEVEL_ALPHAS[(i - 1) as usize]),
    };
    mouse_area(
        container(space())
            .width(Length::Fixed(CELL))
            .height(Length::Fixed(CELL))
            .style(move |_| Style {
                background: Some(bg.into()),
                border: border::rounded(3.0),
                ..Style::default()
            }),
    )
    .on_enter(Message::Ui(UiMsg::HoverTip {
        window,
        label: label.clone(),
        box_px: CELL,
    }))
    .on_exit(Message::Ui(UiMsg::HoverTipEnd { window, label }))
    .into()
}

fn blank_cell() -> Element<'static, Message> {
    container(space())
        .width(Length::Fixed(CELL))
        .height(Length::Fixed(CELL))
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fitted_months_is_odd_and_width_bounded() {
        // 单月块宽：2026-10 为 5 周 = 5*11 + 4*2
        assert_eq!(block_width(2026, 10), 63.0);
        assert_eq!(fitted_months(2026, 10, 1.0), 1);
        let three = strip_width(2026, 10, 3);
        assert_eq!(fitted_months(2026, 10, three), 3);
        assert_eq!(fitted_months(2026, 10, three - 1.0), 1);
        // 超宽也不超过上限，且上限为奇数（导航月居中）
        assert_eq!(fitted_months(2026, 10, 10_000.0), MAX_MONTHS);
        assert_eq!(MAX_MONTHS % 2, 1);
    }
}
