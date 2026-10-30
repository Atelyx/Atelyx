//! 天详情：未选中给引导；选中后是当日历史版本行（点开该文件历史）与当日日程的增删改行。

use atelyx_plugin_protocol::frame::HitCursor;
use atelyx_plugin_protocol::method::{CalendarItem, HistoryDayEntry};
use atelyx_plugin_protocol::paint::{ColorToken, DrawCommand};
use atelyx_plugin_sdk::frame_ui::{Canvas, Icon, Rect, TextStyle, TextRole, Ui};

use super::{action_label, elide, entry_time, item_color, kind_label, token};
use crate::model::Snapshot;

/// 色点直径（日程行与添加行共用）。
const DOT: f32 = 10.0;
/// 日内日程行高度。
const ROW_H: f32 = 24.0;
/// 历史行高度。
const HISTORY_ROW_H: f32 = 22.0;
/// 当日历史行数上限（超出显示「还有 N 条」）。
const HISTORY_CAP: usize = 20;
/// 行内标题输入与添加行输入宽度。
const INPUT_W: f32 = 180.0;
/// 添加行色板块的命中边长。
const SWATCH: f32 = 16.0;

pub fn detail(ui: &mut Ui<'_, '_>, snapshot: &Snapshot) {
    if snapshot.selected.is_none() {
        ui.caption("点选日期查看日程与历史");
        return;
    }
    section(ui, &snapshot.day_title().unwrap_or_default());
    history(ui, snapshot);
    section(ui, "日程");
    schedule(ui, snapshot);
}

/// 分区标题：小号加重的次要色文字。
fn section(ui: &mut Ui<'_, '_>, label: &str) {
    ui.text_styled(label, TextStyle::new(TextRole::FieldLabel));
}

fn history(ui: &mut Ui<'_, '_>, snapshot: &Snapshot) {
    let entries = match &snapshot.history {
        None => {
            ui.caption("历史加载中…");
            return;
        }
        Some(Err(error)) => {
            ui.caption(format!("历史加载失败：{error}"));
            return;
        }
        Some(Ok(entries)) => entries,
    };
    if entries.is_empty() {
        ui.caption("当日无历史版本");
        return;
    }
    let shown: Vec<HistoryDayEntry> = entries.iter().take(HISTORY_CAP).cloned().collect();
    let micro = snapshot.palette.micro;
    let radius = snapshot.palette.radius_sm;
    ui.canvas(shown.len() as f32 * HISTORY_ROW_H, move |canvas| draw_history(canvas, &shown, micro, radius));
    if entries.len() > HISTORY_CAP {
        ui.caption(format!("还有 {} 条", entries.len() - HISTORY_CAP));
    }
}

fn draw_history(canvas: &mut Canvas<'_, '_>, entries: &[HistoryDayEntry], micro: f32, radius: f32) {
    let area = canvas.area();
    let line_h = canvas.measure("日", micro).1;
    for (index, entry) in entries.iter().enumerate() {
        let row = Rect::new(area.x, area.y + index as f32 * HISTORY_ROW_H, area.w, HISTORY_ROW_H);
        let id = format!("hist:{index}");
        if canvas.hovered(&id) {
            canvas.draw(DrawCommand::Rect {
                x: row.x,
                y: row.y,
                w: row.w,
                h: row.h,
                fill: Some(token(ColorToken::Hover)),
                stroke: None,
                stroke_width: None,
                corner_radius: Some(radius),
            });
        }
        let y = row.y + (row.h - line_h) / 2.0;
        let mut x = row.x + 8.0;
        for (text, color) in [
            (entry_time(entry.ts), ColorToken::TextMuted),
            (action_label(&entry.action).to_string(), ColorToken::TextSecondary),
            (kind_label(&entry.kind).to_string(), ColorToken::TextMuted),
        ] {
            x += draw_cell(canvas, &text, x, y, micro, color);
            x += 8.0;
        }
        let author = elide(canvas, &entry.author_name, micro, (row.w * 0.3).max(1.0));
        x += draw_cell(canvas, &author, x, y, micro, ColorToken::TextMuted);
        x += 8.0;
        let Some(summary) = entry.summary.as_deref().filter(|text| !text.is_empty()) else {
            canvas.hit(&id, row, HitCursor::Pointer);
            continue;
        };
        let available = row.x + row.w - 8.0 - x;
        if available > 8.0 {
            let shown = elide(canvas, summary, micro, available);
            let width = canvas.measure(&shown, micro).0;
            canvas.text(shown, row.x + row.w - 8.0 - width, y, micro, token(ColorToken::TextMuted));
        }
        canvas.hit(&id, row, HitCursor::Pointer);
    }
}

/// 画一列文字并返回占用宽度（调用方决定列间距）。
fn draw_cell(canvas: &mut Canvas<'_, '_>, text: &str, x: f32, y: f32, size: f32, color: ColorToken) -> f32 {
    let (width, _) = canvas.measure(text, size);
    canvas.text(text, x, y, size, token(color));
    width
}

fn schedule(ui: &mut Ui<'_, '_>, snapshot: &Snapshot) {
    if !snapshot.ready {
        ui.caption("载入中…");
    } else if snapshot.events.is_empty() {
        ui.caption("当日无日程");
    }
    for item in &snapshot.events {
        schedule_row(ui, snapshot, item);
    }
    add_row(ui, snapshot);
}

fn schedule_row(ui: &mut Ui<'_, '_>, snapshot: &Snapshot, item: &CalendarItem) {
    let id = item.id.clone();
    let color = item_color(item.color.as_deref(), &snapshot.palette.accent);
    let caption = snapshot.palette.caption;
    let radius = snapshot.palette.radius_xs;
    let writable = snapshot.writable;
    let editing = snapshot.edit.as_ref().is_some_and(|(editing, _)| editing == &item.id);
    let editing_draft = snapshot.edit.as_ref().map(|(_, draft)| draft.clone()).unwrap_or_default();
    let confirming = snapshot.delete_confirm.as_deref() == Some(item.id.as_str());
    ui.row(6.0, |row| {
        let cycle = id.clone();
        row.canvas_sized(DOT + 4.0, ROW_H, move |canvas| {
            let area = canvas.area();
            canvas.draw(DrawCommand::Circle {
                cx: area.x + area.w / 2.0,
                cy: area.y + area.h / 2.0,
                r: DOT / 2.0,
                fill: Some(color),
                stroke: None,
                stroke_width: None,
            });
            if writable {
                canvas.hit(&format!("cycle:{cycle}"), area, HitCursor::Pointer);
            }
        });
        if editing {
            row.text_input_sized(format!("edit:{id}"), editing_draft, "", INPUT_W);
        } else {
            let title = item.title.clone();
            let target = format!("edit:{id}");
            row.canvas(ROW_H, move |canvas| {
                let area = canvas.area();
                if canvas.hovered(&target) {
                    canvas.draw(DrawCommand::Rect {
                        x: area.x,
                        y: area.y,
                        w: area.w,
                        h: area.h,
                        fill: Some(token(ColorToken::Hover)),
                        stroke: None,
                        stroke_width: None,
                        corner_radius: Some(radius),
                    });
                }
                let (_, line_h) = canvas.measure("日", caption);
                let shown = elide(canvas, &title, caption, (area.w - 12.0).max(1.0));
                canvas.text(shown, area.x + 6.0, area.y + (area.h - line_h) / 2.0, caption, token(ColorToken::Text));
                canvas.hit(&target, area, HitCursor::Pointer);
            });
        }
        if confirming {
            row.ghost_button(format!("delcancel:{id}"), "取消");
            row.button_enabled(format!("delok:{id}"), "删除", writable);
        } else {
            row.icon_button_enabled(format!("del:{id}"), Icon::Trash, writable);
        }
    });
}

fn add_row(ui: &mut Ui<'_, '_>, snapshot: &Snapshot) {
    let colors = snapshot.palette.event_colors.clone();
    let accent = snapshot.palette.accent.clone();
    let selected = snapshot.draft_color;
    let writable = snapshot.writable;
    let enabled = writable && !snapshot.draft.trim().is_empty();
    let draft = snapshot.draft.clone();
    ui.row(6.0, |row| {
        row.text_input_sized("draft", draft, "添加日程…", INPUT_W);
        for (index, hex) in colors.iter().enumerate() {
            let color = item_color(Some(hex), &accent);
            let chosen = index == selected % colors.len();
            row.canvas_sized(SWATCH + 4.0, ROW_H, move |canvas| {
                let area = canvas.area();
                if chosen {
                    canvas.draw(DrawCommand::Rect {
                        x: area.x,
                        y: area.y,
                        w: area.w,
                        h: area.h,
                        fill: None,
                        stroke: Some(token(ColorToken::Text)),
                        stroke_width: Some(1.0),
                        corner_radius: Some(SWATCH / 2.0),
                    });
                }
                canvas.draw(DrawCommand::Circle {
                    cx: area.x + area.w / 2.0,
                    cy: area.y + area.h / 2.0,
                    r: DOT / 2.0,
                    fill: Some(color),
                    stroke: None,
                    stroke_width: None,
                });
                if writable {
                    canvas.hit(&format!("color:{index}"), area, HitCursor::Pointer);
                }
            });
        }
        row.grow();
        row.button_enabled("add", "添加", enabled);
    });
}
