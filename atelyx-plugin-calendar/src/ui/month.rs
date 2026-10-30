//! 月视图网格：7 列日期格（周一起始、首尾补齐整周），今天强调淡底 / 选中描边 /
//! 日程条目块（色点 + 标题，最多 2 条，超出降为 1 条并显示「+N」）。

use std::collections::HashMap;

use atelyx_plugin_protocol::frame::HitCursor;
use atelyx_plugin_protocol::method::CalendarItem;
use atelyx_plugin_protocol::paint::{ColorToken, DrawCommand, PaintColor};
use atelyx_plugin_sdk::frame_ui::{Canvas, Rect, Ui};

use super::{elide, item_color, token};
use crate::date;
use crate::model::{Palette, Snapshot};

/// 星期表头高度。
const HEADER_H: f32 = 20.0;
/// 日格高度（日号 + 两条日程块）。
const DAY_CELL_H: f32 = 64.0;
/// 格间距（行与列同档）。
const GRID_GAP: f32 = 4.0;
/// 悬停提示行高度。
const HINT_H: f32 = 16.0;
/// 格内日程显示上限；有溢出时降为 1 条给「+N」留行。
const DAY_ITEMS: usize = 2;
/// 条目块内色点直径。
const ITEM_DOT: f32 = 5.0;

pub fn grid(ui: &mut Ui<'_, '_>, snapshot: &Snapshot) {
    let weeks = snapshot.cells.len() / 7;
    let height = HEADER_H + weeks as f32 * DAY_CELL_H + weeks.saturating_sub(1) as f32 * GRID_GAP + HINT_H;
    let cells = snapshot.cells.clone();
    let by_day = snapshot.by_day.clone();
    let today = snapshot.today;
    let selected = snapshot.selected;
    let year = snapshot.year;
    let month = snapshot.month;
    let palette = snapshot.palette.clone();
    ui.canvas(height, move |canvas| {
        draw(
            canvas,
            &cells,
            &by_day,
            year,
            month,
            today,
            selected,
            &palette,
        )
    });
}

#[allow(clippy::too_many_arguments)]
fn draw(
    canvas: &mut Canvas<'_, '_>,
    cells: &[Option<u32>],
    by_day: &HashMap<i64, Vec<CalendarItem>>,
    year: i64,
    month: u32,
    today: i64,
    selected: Option<i64>,
    palette: &Palette,
) {
    let (micro, caption, radius_xs, radius_sm) =
        (palette.micro, palette.caption, palette.radius_xs, palette.radius_sm);
    let accent = palette.accent.as_str();
    const WEEKDAYS: [&str; 7] = ["一", "二", "三", "四", "五", "六", "日"];
    let area = canvas.area();
    let col_w = area.w / 7.0;
    for (index, label) in WEEKDAYS.iter().enumerate() {
        let (width, height) = canvas.measure(label, micro);
        let x = area.x + index as f32 * col_w + (col_w - width) / 2.0;
        canvas.text(*label, x, area.y + (HEADER_H - height) / 2.0, micro, token(ColorToken::TextMuted));
    }
    let first = date::days_from_civil(year, month, 1);
    let grid_height = HEADER_H + (cells.len() / 7) as f32 * DAY_CELL_H + (cells.len() / 7).saturating_sub(1) as f32 * GRID_GAP;
    let mut any_hint = false;
    for (index, cell) in cells.iter().enumerate() {
        let Some(day) = cell else { continue };
        let days = first + *day as i64 - 1;
        let col = (index % 7) as f32;
        let week = (index / 7) as f32;
        let tile = Rect::new(
            area.x + col * col_w + GRID_GAP / 2.0,
            area.y + HEADER_H + week * (DAY_CELL_H + GRID_GAP),
            (col_w - GRID_GAP).max(1.0),
            DAY_CELL_H,
        );
        let id = format!("day:{}", date::date_string(days));
        let hovered = canvas.hovered(&id);
        let is_today = days == today;
        let fill = if is_today {
            token(ColorToken::AccentSoft)
        } else if hovered {
            token(ColorToken::Hover)
        } else {
            token(ColorToken::Surface)
        };
        let stroke = (selected == Some(days)).then(|| token(ColorToken::Accent));
        canvas.draw(DrawCommand::Rect {
            x: tile.x,
            y: tile.y,
            w: tile.w,
            h: tile.h,
            fill: Some(fill),
            stroke,
            stroke_width: stroke.map(|_| 1.0),
            corner_radius: Some(radius_sm),
        });
        let number = day.to_string();
        let (_, number_h) = canvas.measure(&number, caption);
        let number_color = if is_today { token(ColorToken::Accent) } else { token(ColorToken::Text) };
        canvas.text(number, tile.x + 6.0, tile.y + 4.0, caption, number_color);
        let events = by_day.get(&days).map(Vec::as_slice).unwrap_or(&[]);
        let extra = events.len().saturating_sub(DAY_ITEMS);
        let shown = if extra > 0 { DAY_ITEMS - 1 } else { DAY_ITEMS };
        let line_h = canvas.measure("日", micro).1;
        let block_h = line_h + 4.0;
        let mut y = tile.y + 4.0 + number_h + 3.0;
        for item in events.iter().take(shown) {
            let color = item_color(item.color.as_deref(), accent);
            canvas.draw(DrawCommand::Rect {
                x: tile.x + 4.0,
                y,
                w: (tile.w - 8.0).max(1.0),
                h: block_h,
                fill: Some(alpha(color, 0.15)),
                stroke: None,
                stroke_width: None,
                corner_radius: Some(radius_xs),
            });
            canvas.draw(DrawCommand::Circle {
                cx: tile.x + 4.0 + ITEM_DOT / 2.0,
                cy: y + block_h / 2.0,
                r: ITEM_DOT / 2.0,
                fill: Some(color),
                stroke: None,
                stroke_width: None,
            });
            let title = elide(canvas, &item.title, micro, (tile.w - 17.0).max(1.0));
            canvas.text(title, tile.x + 13.0, y + (block_h - line_h) / 2.0, micro, token(ColorToken::Text));
            y += block_h + 2.0;
        }
        if extra > 0 {
            canvas.text(format!("+{extra}"), tile.x + 6.0, y, micro, token(ColorToken::TextMuted));
            if hovered && !any_hint {
                any_hint = true;
                let hint = format!("还有 {extra} 条日程");
                canvas.text(hint, area.x + 6.0, area.y + grid_height + 2.0, micro, token(ColorToken::TextMuted));
            }
        }
        canvas.hit(&id, tile, HitCursor::Pointer);
    }
}

/// 条目底色：取色后统一按 15% 不透明度绘制。
fn alpha(color: PaintColor, opacity: f32) -> PaintColor {
    match color {
        PaintColor::Rgba { rgba: [r, g, b, _] } => PaintColor::Rgba { rgba: [r, g, b, (opacity * 255.0).round() as u8] },
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alpha_keeps_token_colors_and_applies_opacity_to_rgba() {
        assert!(matches!(alpha(token(ColorToken::Accent), 0.15), PaintColor::Token { .. }));
        assert_eq!(alpha(PaintColor::Rgba { rgba: [1, 2, 3, 255] }, 0.5), PaintColor::Rgba { rgba: [1, 2, 3, 128] });
    }
}
