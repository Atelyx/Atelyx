//! 日历面板整面帧：导航、活动热力图、月网格与天详情；自绘部分把绘制与命中收在各自模块。

mod day;
mod heat;
mod month;

use atelyx_plugin_protocol::paint::{ColorToken, PaintColor};
use atelyx_plugin_sdk::frame_ui::{Canvas, Icon, TextRole, Ui};

use crate::date;
use crate::model::Snapshot;

/// 月份标题的固定宽度（导航行里居中，翻月不抖动）。
const MONTH_LABEL_W: f32 = 110.0;
/// 导航行高度。
const NAV_H: f32 = 30.0;

pub fn frame(ui: &mut Ui<'_, '_>, snapshot: &Snapshot) {
    if !snapshot.opened {
        ui.column(6.0, |page| {
            page.text("未打开仓库", TextRole::Heading);
            page.caption("打开仓库后显示");
        });
        return;
    }
    ui.scroll("page", 12.0, |page| {
        nav(page, snapshot);
        if let Some(error) = &snapshot.error {
            page.notice(error.clone(), ColorToken::Danger);
        }
        if let Some(blocked) = snapshot.blocked {
            page.notice(blocked, ColorToken::Warning);
        }
        heat::strip(page, snapshot);
        if let Some(line) = &snapshot.activity_line {
            page.caption(line.clone());
        }
        page.rule();
        month::grid(page, snapshot);
        day::detail(page, snapshot);
    });
}

fn nav(ui: &mut Ui<'_, '_>, snapshot: &Snapshot) {
    let label = snapshot.month_title();
    let size = snapshot.palette.body;
    ui.row(6.0, |row| {
        row.grow();
        row.icon_button("prev-month", Icon::ChevronLeft);
        row.canvas_sized(MONTH_LABEL_W, NAV_H, move |canvas| {
            let (width, height) = canvas.measure(&label, size);
            let area = canvas.area();
            canvas.text(&label, area.x + (area.w - width) / 2.0, area.y + (area.h - height) / 2.0, size, token(ColorToken::Text));
        });
        row.icon_button("next-month", Icon::ChevronRight);
        row.grow();
        row.ghost_button("today", "今天");
    });
}

pub(super) fn token(name: ColorToken) -> PaintColor {
    PaintColor::Token { name }
}

/// `#rrggbb` / `#rrggbbaa` → RGBA；其余形态返回 `None`。
pub(super) fn parse_hex(value: &str) -> Option<[u8; 4]> {
    let hex = value.strip_prefix('#')?;
    if !hex.is_ascii() {
        return None;
    }
    let byte = |text: &str| u8::from_str_radix(text, 16).ok();
    match hex.len() {
        6 => Some([byte(&hex[0..2])?, byte(&hex[2..4])?, byte(&hex[4..6])?, 255]),
        8 => Some([byte(&hex[0..2])?, byte(&hex[2..4])?, byte(&hex[4..6])?, byte(&hex[6..8])?]),
        _ => None,
    }
}

/// 强调色按透明度绘制（热力色阶）；强调色不可解析时退回满色 token。
pub(super) fn accent_alpha(accent: &str, alpha: f32) -> PaintColor {
    match parse_hex(accent) {
        Some([r, g, b, _]) => PaintColor::Rgba { rgba: [r, g, b, (alpha * 255.0).round() as u8] },
        None => token(ColorToken::Accent),
    }
}

pub(super) fn item_color(color: Option<&str>, accent: &str) -> PaintColor {
    color
        .and_then(parse_hex)
        .map(|[r, g, b, a]| PaintColor::Rgba { rgba: [r, g, b, a] })
        .or_else(|| parse_hex(accent).map(|[r, g, b, a]| PaintColor::Rgba { rgba: [r, g, b, a] }))
        .unwrap_or_else(|| token(ColorToken::Accent))
}

/// 单行文字按宽度截断（尾部省略号）；绘制的格子容不下换行。
pub(super) fn elide(canvas: &mut Canvas<'_, '_>, text: &str, size: f32, max_width: f32) -> String {
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

pub(super) fn kind_label(kind: &str) -> &'static str {
    match kind {
        "note" => "笔记",
        "canvas" => "画布",
        "table" => "表格",
        _ => "文件",
    }
}

pub(super) fn action_label(action: &str) -> &'static str {
    if action == "restore" {
        "回滚"
    } else {
        "编辑"
    }
}

pub(super) fn entry_time(ts: i64) -> String {
    let (hour, minute) = date::time_hm(ts);
    format!("{hour:02}:{minute:02}")
}
