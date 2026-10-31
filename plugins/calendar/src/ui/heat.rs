//! 活动热力图：导航月居中的奇数月份窗口（按可用宽度收放），四档色阶；悬停格就地出提示。

use std::collections::HashMap;

use atelyx_plugin_protocol::frame::HitCursor;
use atelyx_plugin_protocol::paint::{ColorToken, DrawCommand};
use atelyx_plugin_sdk::frame_ui::{Canvas, Rect, Ui};

use super::{accent_alpha, token};
use crate::date;
use crate::model::Snapshot;

/// 热力图格子边长与间距。
const CELL: f32 = 11.0;
const CELL_GAP: f32 = 2.0;
/// 格子圆角。
const CELL_RADIUS: f32 = 3.0;
/// 月块最小间距（可用宽有余时余量均分进月块之间）。
const BLOCK_GAP: f32 = 10.0;
/// 热力图窗口月数上限（奇数，导航月居中）。
const MAX_MONTHS: u32 = 13;
/// 月份标签与提示行的固定高度。
const LABEL_H: f32 = 16.0;
const HINT_H: f32 = 16.0;
const LABEL_GAP: f32 = 3.0;
const HINT_GAP: f32 = 4.0;
/// 色阶透明度（1-3 档 accent 递增；0 档 = 三级表面）。
const LEVEL_ALPHAS: [f32; 3] = [0.25, 0.55, 0.9];

/// 整块高度：标签 + 7 行日格 + 提示行。
const HEIGHT: f32 = LABEL_H + LABEL_GAP + 7.0 * CELL + 6.0 * CELL_GAP + HINT_GAP + HINT_H;

pub fn strip(ui: &mut Ui<'_, '_>, snapshot: &Snapshot) {
    let year = snapshot.year;
    let month = snapshot.month;
    let counts = snapshot.counts.clone();
    let accent = snapshot.palette.accent.clone();
    let micro = snapshot.palette.micro;
    ui.canvas(HEIGHT, move |canvas| draw(canvas, year, month, &counts, &accent, micro));
}

fn draw(
    canvas: &mut Canvas<'_, '_>,
    year: i64,
    month: u32,
    counts: &HashMap<i64, u32>,
    accent: &str,
    micro: f32,
) {
    let area = canvas.area();
    let months = fitted_months(year, month, area.w);
    let window = date::month_window_around(year, month, months);
    let widths: Vec<f32> = window.iter().map(|(y, m)| block_width(*y, *m)).collect();
    let minimum: f32 = widths.iter().sum::<f32>() + BLOCK_GAP * (window.len() - 1) as f32;
    let extra = if window.len() > 1 {
        ((area.w - minimum) / (window.len() - 1) as f32).max(0.0)
    } else {
        0.0
    };
    let grid_y = area.y + LABEL_H + LABEL_GAP;
    let mut x = area.x;
    let mut hint = None;
    for ((y, m), width) in window.iter().zip(&widths) {
        let label = date::month_label(*y, *m);
        let (label_w, label_h) = canvas.measure(&label, micro);
        canvas.text(
            &label,
            x + (width - label_w) / 2.0,
            area.y + (LABEL_H - label_h) / 2.0,
            micro,
            token(ColorToken::TextMuted),
        );
        let cells = date::month_cells(*y, *m);
        let first = date::days_from_civil(*y, *m, 1);
        for (index, cell) in cells.iter().enumerate() {
            let Some(day) = cell else { continue };
            let days = first + *day as i64 - 1;
            let col = (index % 7) as f32;
            let week = (index / 7) as f32;
            let rect = Rect::new(x + week * (CELL + CELL_GAP), grid_y + col * (CELL + CELL_GAP), CELL, CELL);
            let count = counts.get(&days).copied().unwrap_or(0);
            let fill = match activity_level(count) {
                0 => token(ColorToken::Surface),
                level => accent_alpha(accent, LEVEL_ALPHAS[(level - 1) as usize]),
            };
            canvas.draw(DrawCommand::Rect {
                x: rect.x,
                y: rect.y,
                w: rect.w,
                h: rect.h,
                fill: Some(fill),
                stroke: None,
                stroke_width: None,
                corner_radius: Some(CELL_RADIUS),
                shadow: None,
            });
            let id = format!("heat:{}", date::date_string(days));
            if canvas.hovered(&id) {
                let (_, m, d) = date::civil_from_days(days);
                hint = Some(format!("{m}月{d}日 · {count} 条活动"));
            }
            canvas.hit(&id, rect, HitCursor::Grab);
        }
        x += width + BLOCK_GAP + extra;
    }
    if let Some(label) = hint {
        canvas.text(label, area.x, area.y + HEIGHT - HINT_H, micro, token(ColorToken::TextMuted));
    }
}

/// 可用宽度能容纳的最大奇数月数（至少 1 个月）。
fn fitted_months(year: i64, month: u32, available: f32) -> u32 {
    (1..=MAX_MONTHS)
        .rev()
        .find(|months| months % 2 == 1 && strip_width(year, month, *months) <= available)
        .unwrap_or(1)
}

fn strip_width(year: i64, month: u32, months: u32) -> f32 {
    date::month_window_around(year, month, months)
        .into_iter()
        .enumerate()
        .map(|(index, (y, m))| block_width(y, m) + if index > 0 { BLOCK_GAP } else { 0.0 })
        .sum()
}

/// 月块宽：周列数 × 格宽 + 列间距。
fn block_width(y: i64, m: u32) -> f32 {
    let weeks = (date::month_cells(y, m).len() / 7) as f32;
    weeks * CELL + (weeks - 1.0) * CELL_GAP
}

/// 色阶档位：0 = 无活动，1-3 / 4-7 / 8+ 三档。
fn activity_level(count: u32) -> u8 {
    match count {
        0 => 0,
        1..=3 => 1,
        4..=7 => 2,
        _ => 3,
    }
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
        assert_eq!(fitted_months(2026, 10, 10_000.0), MAX_MONTHS);
        assert_eq!(MAX_MONTHS % 2, 1);
    }

    #[test]
    fn activity_levels_split_at_three_seven_and_above() {
        assert_eq!(activity_level(0), 0);
        assert_eq!(activity_level(3), 1);
        assert_eq!(activity_level(4), 2);
        assert_eq!(activity_level(7), 2);
        assert_eq!(activity_level(8), 3);
    }
}
