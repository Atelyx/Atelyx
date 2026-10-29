//! 日历面板的整面帧：月份导航、月网格（按日画标题色块）、当日清单与添加行。
//! 自绘网格的绘制闭包须独占数据，故出帧前把网格拷成一份只读快照。

use std::collections::HashMap;

use atelyx_plugin_protocol::frame::HitCursor;
use atelyx_plugin_protocol::paint::{ColorToken, DrawCommand, PaintColor};
use atelyx_plugin_sdk::frame_ui::{Canvas, Icon, Rect, TextRole, Ui};

use crate::date;
use crate::model::{EventView, Snapshot};

/// 星期表头高度。
const HEADER_H: f32 = 20.0;
/// 单个日格高度（含格间距）。
const CELL_H: f32 = 48.0;
/// 日格内缩（格底、描边与相邻格之间留白）。
const CELL_PAD: f32 = 2.0;
/// 日格内最多画几条日程标题。
const MAX_MARKS: usize = 2;
/// 日格文字与格左缘的距离。
const CELL_TEXT_X: f32 = 4.0;

/// 月网格的只读快照（自绘闭包要求 `'static`，不能借用整帧快照）。
#[derive(Clone)]
struct Grid {
    year: i64,
    month: u32,
    cells: Vec<Option<u32>>,
    selected: i64,
    today: i64,
    micro: f32,
    by_day: HashMap<i64, Vec<EventView>>,
}

impl Grid {
    fn of(snapshot: &Snapshot) -> Self {
        Self {
            year: snapshot.year,
            month: snapshot.month,
            cells: snapshot.cells.clone(),
            selected: snapshot.selected,
            today: snapshot.today,
            micro: snapshot.micro,
            by_day: snapshot.by_day.clone(),
        }
    }

    fn height(&self) -> f32 {
        HEADER_H + self.cells.len().div_ceil(7) as f32 * CELL_H
    }
}

pub fn frame(ui: &mut Ui<'_, '_>, snapshot: &Snapshot) {
    let grid = Grid::of(snapshot);
    let grid_height = grid.height();
    ui.scroll("page", 8.0, |page| {
        page.row(6.0, |row| {
            row.text(snapshot.month_title(), TextRole::Heading);
            row.grow();
            row.icon_button("prev-month", Icon::ChevronLeft);
            row.icon_button("next-month", Icon::ChevronRight);
            row.ghost_button("today", "今天");
        });
        let grid = grid.clone();
        page.canvas(grid_height, move |canvas| draw_grid(canvas, &grid));
        page.row(6.0, |row| {
            row.text(snapshot.heading.clone(), TextRole::Heading);
            row.grow();
            if !snapshot.events.is_empty() {
                row.chip(format!("{} 条", snapshot.events.len()), ColorToken::Accent);
            }
        });
        if let Some(error) = &snapshot.error {
            page.notice(error.clone(), ColorToken::Danger);
        }
        page.row(8.0, |row| {
            row.text_input("draft", snapshot.draft.clone(), "添加日程…");
            row.button_enabled("add", "添加", !snapshot.draft.trim().is_empty());
        });
        if !snapshot.ready {
            page.caption("载入中…");
        } else if snapshot.events.is_empty() {
            page.caption("这一天还没有日程。");
        }
        for item in &snapshot.events {
            let pending = snapshot.pending_delete.as_deref() == Some(item.id.as_str());
            page.card(format!("item:{}", item.id), false, |card| {
                card.row(6.0, |row| {
                    row.text(item.title.clone(), TextRole::Value);
                    row.grow();
                    if pending {
                        row.ghost_button(format!("delcancel:{}", item.id), "取消");
                        row.button(format!("delok:{}", item.id), "删除");
                    } else {
                        row.icon_button(format!("del:{}", item.id), Icon::X);
                    }
                });
                if let Some(note) = &item.note {
                    card.caption(note.clone());
                }
            });
        }
    });
}

fn draw_grid(canvas: &mut Canvas<'_, '_>, grid: &Grid) {
    const WEEKDAYS: [&str; 7] = ["一", "二", "三", "四", "五", "六", "日"];
    let area = canvas.area();
    let col_w = area.w / 7.0;
    for (index, label) in WEEKDAYS.iter().enumerate() {
        let (width, height) = canvas.measure(label, grid.micro);
        let x = area.x + index as f32 * col_w + (col_w - width) / 2.0;
        canvas.text(*label, x, area.y + (HEADER_H - height) / 2.0, grid.micro, token(ColorToken::TextMuted));
    }
    for (index, cell) in grid.cells.iter().enumerate() {
        let Some(day) = cell else { continue };
        let days = date::days_from_civil(grid.year, grid.month, *day);
        let id = format!("day:{}", date::date_string(days));
        let col = (index % 7) as f32;
        let row = (index / 7) as f32;
        let slot = Rect::new(area.x + col * col_w, area.y + HEADER_H + row * CELL_H, col_w, CELL_H);
        let cell = Rect::new(
            slot.x + CELL_PAD,
            slot.y + CELL_PAD,
            (slot.w - CELL_PAD * 2.0).max(1.0),
            (slot.h - CELL_PAD * 2.0).max(1.0),
        );
        let selected = days == grid.selected;
        let today = days == grid.today;
        let fill = if selected {
            Some(token(ColorToken::AccentSoft))
        } else if canvas.hovered(&id) {
            Some(token(ColorToken::Hover))
        } else {
            None
        };
        let stroke = today.then(|| token(ColorToken::Accent));
        canvas.draw(DrawCommand::Rect {
            x: cell.x,
            y: cell.y,
            w: cell.w,
            h: cell.h,
            fill,
            stroke,
            stroke_width: stroke.map(|_| 1.0),
            corner_radius: Some(4.0),
        });
        let number = day.to_string();
        let (_, number_h) = canvas.measure(&number, grid.micro);
        let number_color = if selected || today { token(ColorToken::Accent) } else { token(ColorToken::Text) };
        canvas.text(number, cell.x + CELL_TEXT_X, cell.y + 2.0, grid.micro, number_color);
        if let Some(marks) = grid.by_day.get(&days) {
            let line_h = grid.micro + 2.0;
            let mut y = cell.y + 2.0 + number_h + 1.0;
            for mark in marks.iter().take(MAX_MARKS) {
                if y + line_h > cell.bottom() {
                    break;
                }
                let color = mark.color.as_deref().and_then(parse_color).unwrap_or_else(|| token(ColorToken::TextSecondary));
                let shown = elide(canvas, &mark.title, grid.micro, (cell.w - CELL_TEXT_X * 2.0).max(1.0));
                canvas.text(shown, cell.x + CELL_TEXT_X, y, grid.micro, color);
                y += line_h;
            }
        }
        canvas.hit(&id, cell, HitCursor::Pointer);
    }
}

/// 单行文字按宽度截断（尾部省略号），绘制的格子容不下换行。
fn elide(canvas: &mut Canvas<'_, '_>, text: &str, size: f32, max_width: f32) -> String {
    if canvas.measure(text, size).0 <= max_width {
        return text.to_string();
    }
    let mut shown = String::new();
    for ch in text.chars() {
        let mut candidate = shown.clone();
        candidate.push(ch);
        candidate.push('…');
        if canvas.measure(&candidate, size).0 > max_width {
            break;
        }
        shown.push(ch);
    }
    shown.push('…');
    shown
}

/// `#rrggbb` / `#rrggbbaa` → RGBA；非法值退回调用方给的缺省色。
fn parse_color(value: &str) -> Option<PaintColor> {
    let hex = value.strip_prefix('#')?;
    // 非 ASCII 的颜色值按字节切分会切进字符中间，先整体排除
    if !hex.is_ascii() {
        return None;
    }
    let byte = |text: &str| u8::from_str_radix(text, 16).ok();
    match hex.len() {
        6 => Some(PaintColor::Rgba { rgba: [byte(&hex[0..2])?, byte(&hex[2..4])?, byte(&hex[4..6])?, 255] }),
        8 => Some(PaintColor::Rgba {
            rgba: [byte(&hex[0..2])?, byte(&hex[2..4])?, byte(&hex[4..6])?, byte(&hex[6..8])?],
        }),
        _ => None,
    }
}

fn token(name: ColorToken) -> PaintColor {
    PaintColor::Token { name }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_parse_only_hex_literals() {
        assert_eq!(parse_color("#ff8800"), Some(PaintColor::Rgba { rgba: [255, 136, 0, 255] }));
        assert_eq!(parse_color("#ff880080"), Some(PaintColor::Rgba { rgba: [255, 136, 0, 128] }));
        assert_eq!(parse_color("ff8800"), None);
        assert_eq!(parse_color("#ff88"), None);
        assert_eq!(parse_color("#ff88é0"), None, "多字节颜色值不得切片越界");
    }
}
