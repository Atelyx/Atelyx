//! 网格绘制：背景 / 选中淡底 / 单元格正文与图片 / 行边线 / 远端与本机选区 / 新行条 /
//! 行号列 / 表头 / 状态栏 / 滚动条。指令按声明顺序落进同一帧，宿主按序绘制。
//! 只画视口内的行列（虚拟窗口），命中区只用于光标形状——交互命中由插件按坐标自算。

use atelyx_format::table::ops::{self, TableSelection};
use atelyx_plugin_protocol::frame::{HitCursor, HitRegion};
use atelyx_format::table::TableField;
use atelyx_plugin_protocol::paint::{
    ColorToken, DrawCommand, PaintColor, PaintPoint, TextAlign, TextFamily, TextLayout, TextShaping,
    TextWrap,
};

use crate::geom::Geo;
use crate::images;
use crate::model::{cell_text, Rect, Session, Size, CELL_PAD_X};
use crate::text::{self, FontSpec};

/// 主题色快捷。
fn token(name: ColorToken) -> PaintColor {
    PaintColor::Token { name }
}

/// 单元格正文行高倍数（与量算、编辑浮层同源）。
pub const LINE_FACTOR: f32 = crate::text::LINE_FACTOR;

/// 列头菜单钮宽（px，未缩放）：与命中判定的右缘 18px 同口径。
const HEADER_MENU_W: f32 = 18.0;
/// 列头右缘拖宽把手宽（px，未缩放）：与命中判定的右缘 8px 同口径。
const COL_HANDLE_W: f32 = 8.0;

/// 正文排版口径：固定行高倍数 + 按宽折行 + 复杂整形（与量算请求同一项）。
fn text_layout(wrap_w: f32) -> TextLayout {
    TextLayout {
        line_height: Some(LINE_FACTOR),
        wrap: (wrap_w > 0.0).then_some(TextWrap::WordOrGlyph),
        shaping: Some(TextShaping::Advanced),
    }
}

/// 整面绘制：返回绘制指令与光标区（无会话时给空态）。
pub fn paint(
    session: Option<&Session>,
    size: Size,
    remote: &[(crate::model::Rect, [u8; 4])],
) -> (Vec<DrawCommand>, Vec<HitRegion>) {
    let mut out = Vec::new();
    let mut hits = Vec::new();
    // 背景铺满：无会话时也画底，避免面板透出下层
    out.push(DrawCommand::Rect {
        x: 0.0,
        y: 0.0,
        w: size.width.max(1.0),
        h: size.height.max(1.0),
        fill: Some(token(ColorToken::Background)),
        stroke: None,
        stroke_width: None,
        corner_radius: None,
        shadow: None,
    });
    let Some(session) = session else {
        draw_empty(&mut out, size);
        return (out, hits);
    };
    let Some(geo) = crate::geom::geo(&session.view.layout, session.view.scroll_x, session.view.scroll_y, session.view.zoom, size) else {
        draw_empty(&mut out, size);
        return (out, hits);
    };
    if size.width <= 0.0 || size.height <= 0.0 {
        return (out, hits);
    }
    let view = &session.view;

    // 整行 / 整列选中淡底（单格与区域只画框，不铺底）
    match view.selection.as_ref() {
        Some(TableSelection::Row(_)) => {
            let (_, rows) = crate::model::selection_sets(
                &session.data.fields,
                &session.data.rows,
                view.selection.as_ref(),
            );
            for row in rows {
                let y = geo.grid_top() + geo.row_y(row) - view.scroll_y;
                let height = geo.row_h(row);
                if y + height > geo.grid_top() && y < geo.height - geo.status_h() {
                    out.push(fill_rect(0.0, y, geo.width, height, token(ColorToken::AccentSoft)));
                }
            }
        }
        Some(TableSelection::Column(_)) => {
            let (fields, _) = crate::model::selection_sets(
                &session.data.fields,
                &session.data.rows,
                view.selection.as_ref(),
            );
            for field in fields {
                let x = geo.grid_left() + geo.col_x(field) - view.scroll_x;
                out.push(fill_rect(
                    x,
                    geo.grid_top(),
                    geo.col_w(field),
                    geo.viewport_h(),
                    token(ColorToken::AccentSoft),
                ));
            }
        }
        _ => {}
    }

    // 可见行区间 + 逐行绘制
    let rows = session.data.rows.len();
    let (first, last) = geo.visible_rows(rows);
    for row in first..=last.min(rows.saturating_sub(1)) {
        if rows == 0 {
            break;
        }
        let y = geo.grid_top() + geo.row_y(row) - view.scroll_y;
        let height = geo.row_h(row);
        // 行下边线
        out.push(DrawCommand::Line {
            x1: geo.grid_left(),
            y1: y + height,
            x2: geo.width,
            y2: y + height,
            stroke: token(ColorToken::BorderSubtle),
            width: 1.0,
            dash: None,
        });
        let Some(table_row) = session.data.rows.get(row) else { continue };
        for field in 0..session.data.fields.len() {
            if !geo.col_visible(field) {
                continue;
            }
            let rect = geo.cell_rect(row, field);
            let Some(field_def) = session.data.fields.get(field) else { continue };
            let style = ops::style_of(table_row, &field_def.id);
            // 单元格底色（样式底色优先）
            if let Some(bg) = style.bg.as_deref().and_then(text::parse_hex) {
                out.push(fill_rect(rect.x, rect.y, rect.w, rect.h, PaintColor::Rgba { rgba: bg }));
            }
            draw_cell(&mut out, session, field_def, row, field, rect, &style);
            // 右列线
            out.push(DrawCommand::Line {
                x1: rect.right(),
                y1: rect.y,
                x2: rect.right(),
                y2: rect.bottom(),
                stroke: token(ColorToken::BorderSubtle),
                width: 1.0,
                dash: None,
            });
        }
    }

    // 远端协作者选区描边（各自身份色）
    for (rect, color) in remote {
        out.push(stroke_rect(rect.x, rect.y, rect.w, rect.h, PaintColor::Rgba { rgba: *color }, 1.5));
    }
    // 本机选中框
    if let Some(rect) = selection_rect(&geo, session) {
        out.push(stroke_rect(rect.x, rect.y, rect.w, rect.h, token(ColorToken::Accent), 2.0));
    }
    // 行拖拽插入线
    if let Some(reorder) = &view.reorder {
        if let Some(target) = reorder_target_rect(&geo, session, reorder.target) {
            out.push(fill_rect(target.x, target.y, target.w, 2.0, token(ColorToken::Accent)));
        }
    }
    // 新行条
    draw_new_row(&mut out, &geo, session);
    // 单元格编辑器浮层（草稿文本 + 光标 + 选区底）
    draw_editor(&mut out, &mut hits, &geo, session);

    // Chrome：行号列 / 表头 / 状态栏 / 滚动条
    draw_chrome(&mut out, &mut hits, &geo, session);

    // 格式气泡（压在网格与 chrome 之上，灯箱浮层之下）
    crate::format::paint(&mut out, session, size);

    // 光标区：只表达指针形状（交互命中由插件按坐标自算）
    hits.push(HitRegion {
        id: "grid".to_string(),
        x: geo.grid_left(),
        y: geo.grid_top(),
        w: geo.viewport_w(),
        h: geo.viewport_h(),
        cursor: Some(HitCursor::Default),
    });
    for field in 0..session.data.fields.len() {
        let x = geo.grid_left() + geo.col_x(field) - view.scroll_x;
        let width = geo.col_w(field);
        if x + width < geo.grid_left() || x > geo.width {
            continue;
        }
        hits.push(HitRegion {
            id: format!("h:{field}"),
            x: x + width - 8.0 * geo.z,
            y: 0.0,
            w: 8.0 * geo.z,
            h: geo.header_h(),
            cursor: Some(HitCursor::ResizeHorizontal),
        });
    }
    (out, hits)
}

/// 单元格正文与图片。
fn draw_cell(
    out: &mut Vec<DrawCommand>,
    session: &Session,
    field: &TableField,
    row: usize,
    field_index: usize,
    rect: Rect,
    style: &ops::CellStyle,
) {
    let Some(table_row) = session.data.rows.get(row) else { return };
    let value = table_row.values.get(&field.id);
    let zoom = session.view.zoom;
    let pad = crate::model::CELL_PAD_X * zoom;
    if field.field_type == "image" {
        images::paint_cell(out, session, row, field_index, rect, value);
        return;
    }
    // 编辑中的格不画原值（编辑器浮层接管）
    if session
        .view
        .editing
        .as_ref()
        .is_some_and(|edit| edit.row == row && edit.field == field_index)
    {
        return;
    }
    let font = FontSpec::of(style, field, zoom);
    let wrap_w = (rect.w - 2.0 * pad).max(8.0);
    let display = cell_text(value);
    let display = display.as_ref();
    match field.field_type.as_str() {
        "singleSelect" => {
            let filled = !display.is_empty();
            let color = if filled { font.color } else { token(ColorToken::TextMuted) };
            let chevron = 12.0 * zoom;
            let chevron_x = rect.right() - pad - chevron;
            let text_w = (chevron_x - rect.x - pad).max(4.0);
            let label = ellipsize(if filled { display } else { "未选择" }, font.size, text_w);
            out.push(text_command(
                &label,
                rect.x + pad,
                rect.y + (rect.h - font.size * LINE_FACTOR) / 2.0,
                FontSpec { color, ..font.clone() },
                Some(TextAlign::Start),
                0.0,
            ));
            icon_chevron_down(
                out,
                chevron_x,
                rect.y + (rect.h - chevron) / 2.0,
                chevron,
                color,
            );
        }
        "number" | "duration" => {
            let mut text = display.to_string();
            if field.field_type == "duration" && !text.is_empty() {
                // 时长按整秒显示，避免 1.0 秒这种宿主不会写出的形态
                if let Ok(seconds) = text.parse::<f64>() {
                    if seconds.fract() == 0.0 && seconds.abs() < 9.0e15 {
                        text = format!("{}", seconds as i64);
                    }
                }
                text.push_str(" 秒");
            }
            let text = ellipsize(&text, font.size, wrap_w);
            out.push(text_command(
                &text,
                rect.x + pad,
                rect.y + (rect.h - font.size * LINE_FACTOR) / 2.0,
                font,
                Some(TextAlign::Start),
                0.0,
            ));
        }
        _ => {
            let measured = session
                .view
                .layout
                .as_ref()
                .and_then(|cache| cache.wrap.peek(display, &font, wrap_w))
                .map(|lines| lines.to_vec());
            let lines = measured.unwrap_or_else(|| crate::text::estimate_lines_of(display, font.size, wrap_w));
            let mut y = rect.y + crate::model::CELL_PAD_Y * zoom;
            for line in &lines {
                if y > rect.bottom() {
                    break;
                }
                draw_line(out, line.text.as_str(), line.width, rect.x + pad, y, font.clone(), wrap_w, style);
                y += font.size * LINE_FACTOR;
            }
        }
    }
}

/// 画一行正文（含下划线与删除线）。
#[allow(clippy::too_many_arguments)]
fn draw_line(
    out: &mut Vec<DrawCommand>,
    content: &str,
    width: f32,
    x: f32,
    y: f32,
    font: FontSpec,
    wrap_w: f32,
    style: &ops::CellStyle,
) {
    out.push(text_command(
        content,
        x,
        y,
        font.clone(),
        Some(TextAlign::Start),
        wrap_w,
    ));
    if style.u {
        out.push(DrawCommand::Line {
            x1: x,
            y1: y + font.size * LINE_FACTOR - 2.0,
            x2: x + width.min(wrap_w),
            y2: y + font.size * LINE_FACTOR - 2.0,
            stroke: font.color,
            width: 1.0,
            dash: None,
        });
    }
    if style.s {
        let mid = y + font.size * LINE_FACTOR / 2.0 + 1.0;
        out.push(DrawCommand::Line { x1: x, y1: mid, x2: x + width.min(wrap_w), y2: mid, stroke: font.color, width: 1.0, dash: None });
    }
}

/// 文字指令（统一口径：行高倍数 + 按宽折行 + 复杂整形）。
fn text_command(
    content: &str,
    x: f32,
    y: f32,
    font: FontSpec,
    align: Option<TextAlign>,
    max_width: f32,
) -> DrawCommand {
    DrawCommand::Text {
        x,
        y,
        content: content.to_string(),
        size: font.size,
        color: font.color,
        weight: font.weight,
        family: font.family,
        family_name: font.family_name.clone(),
        italic: font.italic,
        // 宽度 ≤ 0 = 不换行也不对齐（协议要求给宽度时必须为正、换行口径须与宽度同时给）
        max_width: (max_width > 0.0).then_some(max_width),
        align: (max_width > 0.0).then_some(align).flatten(),
        layout: Some(text_layout(max_width)),
    }
}

/// 行号列 / 表头 / 状态栏 / 滚动条。
fn draw_chrome(out: &mut Vec<DrawCommand>, hits: &mut Vec<HitRegion>, geo: &Geo<'_>, session: &Session) {
    // 行号列先画，表头与状态栏带体后画：滚入带内的行内容被不透明底色压回
    draw_gutter(out, hits, geo, session);
    out.push(fill_rect(0.0, 0.0, geo.width, geo.header_h(), token(ColorToken::SurfaceMuted)));
    draw_header(out, geo, session);
    let status_y = geo.height - geo.status_h();
    out.push(fill_rect(0.0, status_y, geo.width, geo.status_h(), token(ColorToken::SurfaceMuted)));
    draw_status(out, geo, session);
    draw_scrollbars(out, geo, session);
}

/// 行号列：行号 / 拖拽钮 / 行高把手命中区；选中行铺三级底 + 左缘强调线。
fn draw_gutter(out: &mut Vec<DrawCommand>, hits: &mut Vec<HitRegion>, geo: &Geo<'_>, session: &Session) {
    let view = &session.view;
    let gutter = geo.gutter_w();
    let bottom = geo.height - geo.status_h();
    let selected = match view.selection.as_ref() {
        Some(TableSelection::Row(id)) => session.data.rows.iter().position(|row| &row.id == id),
        _ => None,
    };
    out.push(fill_rect(0.0, geo.grid_top(), gutter, geo.viewport_h(), token(ColorToken::SurfaceMuted)));
    let (first, last) = geo.visible_rows(session.data.rows.len());
    for row in first..=last {
        if row >= session.data.rows.len() {
            break;
        }
        let y = geo.grid_top() + geo.row_y(row) - view.scroll_y;
        let height = geo.row_h(row);
        if y + height <= geo.grid_top() || y >= bottom {
            continue;
        }
        let active = selected == Some(row);
        if active {
            out.push(fill_rect(0.0, y, gutter, height, token(ColorToken::Surface)));
            out.push(fill_rect(0.0, y, 2.0 * geo.z, height, token(ColorToken::Accent)));
        }
        let size = crate::theme::font_caption() * geo.z;
        out.push(text_command(
            &(row + 1).to_string(),
            6.0 * geo.z,
            y + (height - size * LINE_FACTOR) / 2.0,
            FontSpec {
                size,
                weight: None,
                family: Some(TextFamily::Mono),
                family_name: None,
                italic: false,
                color: token(ColorToken::TextMuted),
            },
            Some(TextAlign::Start),
            0.0,
        ));
        let grip = 10.0 * geo.z;
        icon_grip_vertical(
            out,
            gutter - 15.0 * geo.z,
            y + height / 2.0 - grip / 2.0,
            grip,
            token(if active { ColorToken::TextSecondary } else { ColorToken::BorderStrong }),
        );
        out.push(fill_rect(0.0, y + height - 1.0, gutter, 1.0, token(ColorToken::BorderSubtle)));
        // 拖拽钮（行中段）压在行高把手（底缘 6px）之下，后声明的把手优先
        hits.push(HitRegion {
            id: format!("gg:{row}"),
            x: 0.0,
            y: y + height / 2.0 - 9.0 * geo.z,
            w: gutter,
            h: 18.0 * geo.z,
            cursor: Some(HitCursor::Grab),
        });
        hits.push(HitRegion {
            id: format!("g:{row}"),
            x: 0.0,
            y: y + height - 6.0 * geo.z,
            w: gutter,
            h: 6.0 * geo.z,
            cursor: Some(HitCursor::ResizeVertical),
        });
    }
}

/// 列头：角钮 + 列名 + ⋮ + 拖宽把手 + 加列占位；选中列铺淡底。
fn draw_header(out: &mut Vec<DrawCommand>, geo: &Geo<'_>, session: &Session) {
    let view = &session.view;
    let height = geo.header_h();
    let size = crate::theme::font_caption() * geo.z;
    let corner = 10.0 * geo.z;
    icon_maximize(
        out,
        geo.gutter_w() / 2.0 - corner / 2.0,
        (height - corner) / 2.0,
        corner,
        token(ColorToken::TextMuted),
    );
    for (index, field) in session.data.fields.iter().enumerate() {
        if !geo.col_visible(index) {
            continue;
        }
        let x = geo.grid_left() + geo.col_x(index) - view.scroll_x;
        let width = geo.col_w(index);
        if matches!(view.selection.as_ref(), Some(TableSelection::Column(id)) if id == &field.id) {
            out.push(fill_rect(x, 0.0, width, height, token(ColorToken::AccentSoft)));
        }
        let text_w = (width - 2.0 * CELL_PAD_X * geo.z - HEADER_MENU_W * geo.z).max(4.0);
        let name = ellipsize(&field.name, size, text_w);
        out.push(text_command(
            &name,
            x + CELL_PAD_X * geo.z,
            (height - size * LINE_FACTOR) / 2.0,
            FontSpec {
                size,
                weight: None,
                family: None,
                family_name: None,
                italic: false,
                color: token(ColorToken::TextSecondary),
            },
            Some(TextAlign::Start),
            0.0,
        ));
        // ⋮ 居中于菜单命中区（右缘 18px 内、避开 8px 拖宽把手）
        icon_ellipsis(
            out,
            x + width - 13.0 * geo.z,
            height / 2.0,
            11.0 * geo.z,
            token(ColorToken::TextMuted),
        );
        out.push(DrawCommand::Line {
            x1: x + width - 1.0,
            y1: 0.0,
            x2: x + width - 1.0,
            y2: height,
            stroke: token(ColorToken::Border),
            width: 1.0,
            dash: None,
        });
        out.push(fill_rect(
            x + width - COL_HANDLE_W * geo.z,
            height - 3.0 * geo.z,
            COL_HANDLE_W * geo.z,
            3.0 * geo.z,
            token(ColorToken::Accent),
        ));
    }
    // 加列占位（列尾：底色 + 加号 + 两侧分隔线）
    let add_x = geo.grid_left() + geo.cache.col_x.last().copied().unwrap_or(0.0) - view.scroll_x;
    if add_x < geo.width {
        out.push(fill_rect(add_x, 0.0, geo.add_col_w(), height, token(ColorToken::SurfaceMuted)));
        icon_plus(
            out,
            add_x + geo.add_col_w() / 2.0,
            height / 2.0,
            12.0 * geo.z,
            token(ColorToken::TextMuted),
        );
        for x in [add_x - 1.0, add_x + geo.add_col_w() - 1.0] {
            out.push(DrawCommand::Line {
                x1: x,
                y1: 0.0,
                x2: x,
                y2: height,
                stroke: token(ColorToken::Border),
                width: 1.0,
                dash: None,
            });
        }
    }
    out.push(DrawCommand::Line {
        x1: 0.0,
        y1: height - 1.0,
        x2: geo.width,
        y2: height - 1.0,
        stroke: token(ColorToken::Border),
        width: 1.0,
        dash: None,
    });
}

/// 状态栏：Σ 角格 + 各列「统计方式 + 值」；列宽不够按单行省略。
fn draw_status(out: &mut Vec<DrawCommand>, geo: &Geo<'_>, session: &Session) {
    let view = &session.view;
    let height = geo.status_h();
    let top = geo.height - height;
    let sigma = 13.0 * geo.z;
    icon_sigma(
        out,
        (geo.gutter_w() - sigma) / 2.0,
        top + (height - sigma) / 2.0,
        sigma,
        token(ColorToken::TextMuted),
    );
    for (index, field) in session.data.fields.iter().enumerate() {
        if !geo.col_visible(index) {
            continue;
        }
        let Some(calc) = ops::compute_column_calc(field, &session.data.rows) else { continue };
        let label = format!(
            "{} {}",
            ops::calc_type_label(field.calc_type.as_deref().unwrap_or("")),
            calc
        );
        let size = crate::theme::font_caption() * geo.z;
        let text = ellipsize(&label, size, (geo.col_w(index) - 2.0 * CELL_PAD_X * geo.z).max(4.0));
        let x = geo.grid_left() + geo.col_x(index) - view.scroll_x;
        out.push(text_command(
            &text,
            x + CELL_PAD_X * geo.z,
            top + (height - size * LINE_FACTOR) / 2.0,
            FontSpec {
                size,
                weight: None,
                family: Some(TextFamily::Mono),
                family_name: None,
                italic: false,
                color: token(ColorToken::Text),
            },
            Some(TextAlign::Start),
            0.0,
        ));
    }
    out.push(DrawCommand::Line {
        x1: 0.0,
        y1: top,
        x2: geo.width,
        y2: top,
        stroke: token(ColorToken::Border),
        width: 1.0,
        dash: None,
    });
}

/// 新行条：内容末尾的「+ 新行」，指针落在条上才有悬停底。
fn draw_new_row(out: &mut Vec<DrawCommand>, geo: &Geo<'_>, session: &Session) {
    let view = &session.view;
    let y = geo.grid_top() + geo.row_y(session.data.rows.len()) - view.scroll_y;
    if y + geo.new_row_h() <= geo.grid_top() || y >= geo.height - geo.status_h() {
        return;
    }
    if matches!(view.hover, Some(crate::model::Hit::NewRow)) {
        let strip_w = (geo.total_w() - view.scroll_x - geo.gutter_w()).clamp(0.0, geo.viewport_w());
        out.push(fill_rect(geo.grid_left(), y, strip_w, geo.new_row_h(), token(ColorToken::Hover)));
    }
    let cy = y + geo.new_row_h() / 2.0;
    icon_plus(out, geo.grid_left() + 16.0 * geo.z, cy, 12.0 * geo.z, token(ColorToken::TextMuted));
    let size = crate::theme::font_caption() * geo.z;
    out.push(text_command(
        "新行",
        geo.grid_left() + 26.0 * geo.z,
        cy - size * LINE_FACTOR / 2.0,
        FontSpec {
            size,
            weight: None,
            family: None,
            family_name: None,
            italic: false,
            color: token(ColorToken::TextMuted),
        },
        Some(TextAlign::Start),
        0.0,
    ));
}

/// 横向 / 竖向滚动条：仅该方向溢出时显示，轨道起止为表头下缘到状态栏上缘。
/// 滑块三态：拖拽强调色 / 悬停轨道次级文字色 / 常态强边界色。
fn draw_scrollbars(out: &mut Vec<DrawCommand>, geo: &Geo<'_>, session: &Session) {
    let view = &session.view;
    let bar = geo.scrollbar_w();
    if geo.max_scroll_y() > 0.0 {
        let top = geo.grid_top();
        let track_h = (geo.height - geo.status_h() - top).max(0.0);
        out.push(fill_rect(geo.width - bar, top, bar, track_h, token(ColorToken::SurfaceSunken)));
        let color = if view.vscroll_drag.is_some() {
            token(ColorToken::Accent)
        } else if matches!(view.hover, Some(crate::model::Hit::VScrollbar)) {
            token(ColorToken::TextSecondary)
        } else {
            token(ColorToken::BorderStrong)
        };
        let (start, len, _) = geo.v_thumb();
        out.push(fill_rect(geo.width - bar, top + start, bar, len, color));
    }
    if geo.max_scroll_x() > 0.0 {
        let left = geo.gutter_w();
        let y = geo.height - geo.status_h() - bar;
        let track_w = (geo.width - left - bar).max(0.0);
        out.push(fill_rect(left, y, track_w, bar, token(ColorToken::SurfaceSunken)));
        out.push(fill_rect(left, y, track_w, 1.0, token(ColorToken::BorderSubtle)));
        let color = if view.hscroll_drag.is_some() {
            token(ColorToken::Accent)
        } else if matches!(view.hover, Some(crate::model::Hit::HScrollbar)) {
            token(ColorToken::TextSecondary)
        } else {
            token(ColorToken::BorderStrong)
        };
        let (start, len, _) = geo.h_thumb();
        out.push(fill_rect(left + start, y, len, bar, color));
    }
}

/// 选中框矩形（单格 / 区域 / 整行 / 整列 / 全表）。
fn selection_rect(geo: &Geo<'_>, session: &Session) -> Option<Rect> {
    let selection = session.view.selection.as_ref()?;
    let region = ops::region_of(Some(selection), &session.data.fields, &session.data.rows)?;
    let first = geo.cell_rect(region.row_start, region.col_start);
    let last = geo.cell_rect(region.row_end, region.col_end);
    Some(Rect::new(
        first.x,
        first.y,
        (last.right() - first.x).max(0.0),
        (last.bottom() - first.y).max(0.0),
    ))
}

/// 行拖拽目标位置的插入线矩形。
fn reorder_target_rect(geo: &Geo<'_>, session: &Session, target: usize) -> Option<Rect> {
    let y = if target >= session.data.rows.len() {
        geo.grid_top() + geo.row_y(session.data.rows.len()) - session.view.scroll_y
    } else {
        geo.grid_top() + geo.row_y(target) - session.view.scroll_y
    };
    Some(Rect::new(geo.grid_left(), y - 1.0, geo.viewport_w(), 2.0))
}

/// 空态（未打开表格）。
fn draw_empty(out: &mut Vec<DrawCommand>, size: Size) {
    let cx = size.width / 2.0;
    let cy = size.height / 2.0;
    out.push(text_command(
        "未打开表格",
        cx - 60.0,
        cy - 10.0,
        FontSpec { size: crate::theme::font_body(), weight: None, family: None, family_name: None, italic: false, color: token(ColorToken::TextSecondary)  },
        Some(TextAlign::Start),
        200.0,
    ));
    out.push(text_command(
        "从文件面板打开一个 .atb 文件",
        cx - 90.0,
        cy + 14.0,
        FontSpec { size: crate::theme::font_caption(), weight: None, family: None, family_name: None, italic: false, color: token(ColorToken::TextMuted)  },
        Some(TextAlign::Start),
        240.0,
    ));
}

/// 填充矩形。
fn fill_rect(x: f32, y: f32, w: f32, h: f32, color: PaintColor) -> DrawCommand {
    DrawCommand::Rect { x, y, w, h, fill: Some(color), stroke: None, stroke_width: None, corner_radius: None, shadow: None }
}

/// 描边矩形。
fn stroke_rect(x: f32, y: f32, w: f32, h: f32, color: PaintColor, width: f32) -> DrawCommand {
    DrawCommand::Rect { x, y, w, h, fill: None, stroke: Some(color), stroke_width: Some(width), corner_radius: None, shadow: None }
}

/// 图标折线：24 单位视框按 `size` 缩放后描边（2 单位描边随缩放）。
fn icon_path(
    out: &mut Vec<DrawCommand>,
    points: &[(f32, f32)],
    x: f32,
    y: f32,
    size: f32,
    color: PaintColor,
) {
    let k = size / 24.0;
    let points = points
        .iter()
        .map(|(px, py)| PaintPoint { x: x + px * k, y: y + py * k })
        .collect();
    out.push(DrawCommand::Path {
        points,
        closed: false,
        fill: None,
        stroke: Some(color),
        stroke_width: Some(2.0 * k),
    });
}

/// 加号（lucide `plus`），`(cx, cy)` 为图标中心。
fn icon_plus(out: &mut Vec<DrawCommand>, cx: f32, cy: f32, size: f32, color: PaintColor) {
    let (x, y) = (cx - size / 2.0, cy - size / 2.0);
    icon_path(out, &[(12.0, 5.0), (12.0, 19.0)], x, y, size, color);
    icon_path(out, &[(5.0, 12.0), (19.0, 12.0)], x, y, size, color);
}

/// 省略号（lucide `ellipsis`），`(cx, cy)` 为图标中心。
fn icon_ellipsis(out: &mut Vec<DrawCommand>, cx: f32, cy: f32, size: f32, color: PaintColor) {
    let k = size / 24.0;
    for dx in [-7.0, 0.0, 7.0] {
        out.push(DrawCommand::Circle {
            cx: cx + dx * k,
            cy,
            r: 1.4 * k,
            fill: Some(color),
            stroke: None,
            stroke_width: None,
        });
    }
}

/// 竖向拖拽点（lucide `grip-vertical`），`(x, y)` 为图标左上角。
fn icon_grip_vertical(out: &mut Vec<DrawCommand>, x: f32, y: f32, size: f32, color: PaintColor) {
    let k = size / 24.0;
    for gx in [9.0, 15.0] {
        for gy in [5.0, 12.0, 19.0] {
            out.push(DrawCommand::Circle {
                cx: x + gx * k,
                cy: y + gy * k,
                r: 1.4 * k,
                fill: Some(color),
                stroke: None,
                stroke_width: None,
            });
        }
    }
}

/// 全选角钮（lucide `maximize-2`），`(x, y)` 为图标左上角。
fn icon_maximize(out: &mut Vec<DrawCommand>, x: f32, y: f32, size: f32, color: PaintColor) {
    icon_path(out, &[(15.0, 3.0), (21.0, 3.0), (21.0, 9.0)], x, y, size, color);
    icon_path(out, &[(9.0, 21.0), (3.0, 21.0), (3.0, 15.0)], x, y, size, color);
    icon_path(out, &[(21.0, 3.0), (14.0, 10.0)], x, y, size, color);
    icon_path(out, &[(3.0, 21.0), (10.0, 14.0)], x, y, size, color);
}

/// 统计角标（lucide `sigma`），`(x, y)` 为图标左上角。
fn icon_sigma(out: &mut Vec<DrawCommand>, x: f32, y: f32, size: f32, color: PaintColor) {
    icon_path(
        out,
        &[(18.0, 5.0), (7.0, 5.0), (13.0, 12.0), (7.0, 19.0), (18.0, 19.0)],
        x,
        y,
        size,
        color,
    );
}

/// 下拉箭头（lucide `chevron-down`），`(x, y)` 为图标左上角。
fn icon_chevron_down(out: &mut Vec<DrawCommand>, x: f32, y: f32, size: f32, color: PaintColor) {
    icon_path(out, &[(6.0, 9.0), (12.0, 15.0), (18.0, 9.0)], x, y, size, color);
}

/// 按像素宽截断并补省略号（表头 / 状态栏 / 数字类字段）。
pub fn ellipsize(content: &str, size: f32, max_w: f32) -> String {
    let width: f32 = content.chars().map(|ch| size * if ch.is_ascii() { 0.55 } else { 1.0 }).sum();
    if width <= max_w || max_w <= 0.0 {
        return content.to_string();
    }
    let mut out = String::new();
    let mut used = 0.0f32;
    for ch in content.chars() {
        let advance = size * if ch.is_ascii() { 0.55 } else { 1.0 };
        if used + advance + size * 0.55 > max_w {
            break;
        }
        out.push(ch);
        used += advance;
    }
    out.push('…');
    out
}


/// 单元格编辑器浮层：透明底（选中框即编辑边框）、草稿文本逐行、光标与选区。
fn draw_editor(
    out: &mut Vec<DrawCommand>,
    hits: &mut Vec<HitRegion>,
    geo: &Geo<'_>,
    session: &Session,
) {
    let Some(edit) = session.view.editing.as_ref() else { return };
    let (Some(field), Some(table_row)) = (
        session.data.fields.get(edit.field),
        session.data.rows.get(edit.row),
    ) else {
        return;
    };
    let zoom = session.view.zoom;
    let cell = geo.cell_rect(edit.row, edit.field);
    let width = cell.w.max(40.0);
    let height = cell.h.max(24.0);
    let style = ops::style_of(table_row, &field.id);
    let font = FontSpec::of(&style, field, zoom);
    let pad_x = crate::model::CELL_PAD_X * zoom;
    let pad_y = if field.field_type == "text" {
        crate::model::CELL_PAD_Y * zoom
    } else {
        ((height - font.size * LINE_FACTOR) / 2.0).max(0.0)
    };
    // 选区底（光标与锚点之间）
    let (caret, anchor) = crate::edit::display_caret(edit);
    let text = crate::edit::display_text(edit);
    let wrap_w = (width - 2.0 * pad_x).max(8.0);
    let lines = crate::text::estimate_lines_of(&text, font.size, wrap_w);
    let line_h = font.size * LINE_FACTOR;
    // 光标所在行与行内横坐标
    let mut line_start = 0usize;
    let mut caret_line = 0usize;
    let mut caret_x = 0.0f32;
    for (index, line) in lines.iter().enumerate() {
        let line_end = line_start + line.text.len();
        if caret >= line_start && caret <= line_end {
            caret_line = index;
            let prefix = &text[line_start..caret.min(text.len())];
            caret_x = prefix
                .chars()
                .map(|ch| font.size * if ch.is_ascii() { 0.55 } else { 1.0 })
                .sum();
        }
        // 折行段连续（不跳字符），硬换行才吃掉一个换行符
        line_start = if text.as_bytes().get(line_end) == Some(&b'\n') {
            line_end + 1
        } else {
            line_end
        };
    }
    let origin_x = cell.x + pad_x;
    let origin_y = cell.y + pad_y;
    // 指令集无裁剪：超出格高的行段直接不画，长草稿不压到相邻行
    let clip_bottom = cell.y + height;
    // 选区底
    if caret != anchor {
        let (from, to) = crate::editor::ordered(caret, anchor);
        let mut start = 0usize;
        for (index, line) in lines.iter().enumerate() {
            let line_end = start + line.text.len();
            let low = from.max(start);
            let high = to.min(line_end);
            if low < high && origin_y + (index as f32 + 1.0) * line_h <= clip_bottom {
                let x0: f32 = text[start..low]
                    .chars()
                    .map(|ch| font.size * if ch.is_ascii() { 0.55 } else { 1.0 })
                    .sum::<f32>()
                    .min(wrap_w);
                let x1: f32 = text[start..high]
                    .chars()
                    .map(|ch| font.size * if ch.is_ascii() { 0.55 } else { 1.0 })
                    .sum::<f32>()
                    .min(wrap_w);
                if x1 > x0 {
                    out.push(fill_rect(
                        origin_x + x0,
                        origin_y + index as f32 * line_h,
                        (x1 - x0).max(1.0),
                        line_h,
                        token(ColorToken::Selection),
                    ));
                }
            }
            start = if text.as_bytes().get(line_end) == Some(&b'\n') {
                line_end + 1
            } else {
                line_end
            };
        }
    }
    // 文本逐行（放不下的整行裁掉）
    for (index, line) in lines.iter().enumerate() {
        let top = origin_y + index as f32 * line_h;
        if top + line_h > clip_bottom {
            break;
        }
        if line.text.is_empty() {
            continue;
        }
        out.push(text_command(
            &line.text,
            origin_x,
            top,
            font.clone(),
            Some(TextAlign::Start),
            wrap_w,
        ));
    }
    // 光标（组合串也画在插入点前，随光标走）
    if origin_y + (caret_line as f32 + 1.0) * line_h <= clip_bottom {
        out.push(fill_rect(
            origin_x + caret_x.min(wrap_w),
            origin_y + caret_line as f32 * line_h,
            1.5,
            line_h,
            token(ColorToken::Accent),
        ));
    }
    hits.push(HitRegion {
        id: "editor".to_string(),
        x: cell.x,
        y: cell.y,
        w: width,
        h: height,
        cursor: Some(HitCursor::Text),
    });
}

/// 输入法光标矩形（面本地坐标）：单元格编辑或浮层输入期间给出，宿主据此摆放候选窗。
pub fn ime_rect(session: &Session, size: Size) -> Option<atelyx_plugin_protocol::frame::FrameRect> {
    if let Some(prompt) = session.view.prompt.as_ref() {
        let area = crate::prompt::rect(prompt, size);
        let input_x = area.x + 8.0 + 6.0;
        let (caret, _) = (prompt.editor.caret, prompt.editor.anchor);
        let display = prompt.editor.display_text();
        let prefix = &display[..caret.min(display.len())];
        let offset: f32 = prefix
            .chars()
            .map(|ch| crate::theme::font_caption() * if ch.is_ascii() { 0.55 } else { 1.0 })
            .sum();
        return Some(atelyx_plugin_protocol::frame::FrameRect {
            x: input_x + offset,
            y: area.y + 8.0 + 4.0,
            w: 1.5,
            h: 18.0,
        });
    }
    let edit = session.view.editing.as_ref()?;
    let geo = crate::geom::geo(
        &session.view.layout,
        session.view.scroll_x,
        session.view.scroll_y,
        session.view.zoom,
        size,
    )?;
    let (field, row) = (
        session.data.fields.get(edit.field)?,
        session.data.rows.get(edit.row)?,
    );
    let cell = geo.cell_rect(edit.row, edit.field);
    let style = ops::style_of(row, &field.id);
    let font = FontSpec::of(&style, field, session.view.zoom);
    let pad_x = crate::model::CELL_PAD_X * session.view.zoom;
    let pad_y = if field.field_type == "text" {
        crate::model::CELL_PAD_Y * session.view.zoom
    } else {
        ((cell.h - font.size * LINE_FACTOR) / 2.0).max(0.0)
    };
    let wrap_w = (cell.w - 2.0 * pad_x).max(8.0);
    let (caret, _) = crate::edit::display_caret(edit);
    let text = crate::edit::display_text(edit);
    let lines = crate::text::estimate_lines_of(&text, font.size, wrap_w);
    let line_h = font.size * LINE_FACTOR;
    let mut line_start = 0usize;
    let mut caret_line = 0usize;
    let mut caret_x = 0.0f32;
    for (index, line) in lines.iter().enumerate() {
        let line_end = line_start + line.text.len();
        if caret >= line_start && caret <= line_end {
            caret_line = index;
            caret_x = text[line_start..caret.min(text.len())]
                .chars()
                .map(|ch| font.size * if ch.is_ascii() { 0.55 } else { 1.0 })
                .sum();
        }
        line_start = if text.as_bytes().get(line_end) == Some(&b'\n') {
            line_end + 1
        } else {
            line_end
        };
    }
    Some(atelyx_plugin_protocol::frame::FrameRect {
        x: cell.x + pad_x + caret_x,
        y: cell.y + pad_y + caret_line as f32 * line_h,
        w: 1.5,
        h: line_h,
    })
}
