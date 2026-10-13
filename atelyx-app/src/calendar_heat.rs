//! 日历活动热力图：以导航月为末月的 12 个月，每月一块（周为列，周一~周日 7 行），
//! 四档色阶；本地密度走活动缓存，空间按仓库历史聚合计数。

use std::collections::HashMap;

use iced::widget::container::Style;
use iced::widget::scrollable::{Direction, Scrollbar};
use iced::widget::{container, mouse_area, scrollable, space, text};
use iced::{Element, Length, border};

use crate::calendar_grid;
use crate::ui::palette as pal;
use crate::ui::UiMsg;
use crate::{App, Message, RepoKind};
use crate::calendar_view::display_month;

/// 热力图格子边长与间距。
const CELL: f32 = 11.0;
const CELL_GAP: f32 = 2.0;
/// 热力图窗口月数（以导航月为末月向前取）。
const HEAT_MONTHS: u32 = 12;
/// 色阶透明度（1-3 档 accent 递增；0 档 = BG_TERTIARY）。
const LEVEL_ALPHAS: [f32; 3] = [0.25, 0.55, 0.9];

/// 活动热力图：单行 12 个月，超宽部分横向滚动（滚动条可见）。
pub fn heatmap(app: &App, window: iced::window::Id) -> Element<'static, Message> {
    let counts = activity_counts(app);
    let (ey, em) = display_month(app);
    let mut blocks = iced::widget::Row::<Message>::new().spacing(10);
    for (y, m) in calendar_grid::month_window(ey, em, HEAT_MONTHS) {
        let cells = calendar_grid::month_cells(y, m);
        let first = calendar_grid::days_from_civil(y, m, 1);
        let mut weeks_row = iced::widget::Row::<Message>::new().spacing(CELL_GAP);
        for w in 0..cells.len() / 7 {
            let mut week_col = iced::widget::Column::<Message>::new().spacing(CELL_GAP);
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
        blocks = blocks.push(
            iced::widget::Column::<Message>::new()
                .spacing(3)
                .push(
                    text(calendar_grid::month_label(y, m))
                        .size(pal::fs_micro())
                        .color(pal::TEXT_MUTED),
                )
                .push(weeks_row),
        );
    }
    let mut col = iced::widget::Column::<Message>::new().push(
        scrollable(container(blocks))
            .direction(Direction::Horizontal(
                Scrollbar::new().width(6.0).scroller_width(6.0),
            ))
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
