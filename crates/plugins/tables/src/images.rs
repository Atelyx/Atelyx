//! 图片单元格：值形态、展示布局（轮播 / 九宫格）、按钮与缩略图几何、灯箱几何。
//! 轮播 = 主图区 + 队列条（窗口随当前页滚动）；九宫格按列数均分；按钮 22×z。

use atelyx_format::table::ops;
use atelyx_plugin_protocol::paint::{ColorToken, DrawCommand, ImageFit, ImageSource, PaintColor};

use crate::model::{Hit, Rect, Session, CELL_PAD_X, CELL_PAD_Y};

/// 轮播主图区高（px，未缩放）。
pub const IMAGE_CAROUSEL_H: f32 = 96.0;
/// 队列条高（px，未缩放）。
pub const IMAGE_QUEUE_H: f32 = 36.0;
/// 缩略图边长（px，未缩放）。
pub const IMAGE_THUMB: f32 = 28.0;
/// 图片间隙（px，未缩放）。
pub const IMAGE_GAP: f32 = 4.0;
/// 九宫格单块最大高（px，未缩放）。
pub const IMAGE_GRID_SINGLE_MAX: f32 = 160.0;
/// 图片格按钮边长（px，未缩放）。
pub const IMAGE_BTN: f32 = 22.0;
/// 缩略图长按进拖拽（毫秒）、移动取消阈值（px）、轮播翻页阈值与端点阻尼（px，未缩放）。
pub const IMAGE_LONG_PRESS_MS: u128 = 400;
pub const IMAGE_PRESS_CANCEL: f32 = 6.0;
pub const IMAGE_SWIPE_THRESHOLD: f32 = 48.0;
pub const IMAGE_EDGE_DAMPING: f32 = 0.35;

/// 图片条目（仓库相对路径或内嵌 dataURL）。
pub fn entries(value: Option<&serde_json::Value>) -> Vec<String> {
    value
        .map(|value| {
            ops::image_entries(value)
                .into_iter()
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// 图片展示方式（`grid` = 九宫格，缺省 = 轮播）。
pub fn display_mode(value: Option<&serde_json::Value>) -> &str {
    value.and_then(ops::image_display).unwrap_or("carousel")
}

/// 图片单元格占位高（行高下限之外的正文占位）。
pub fn image_area_height(value: Option<&serde_json::Value>, col_w: f32, zoom: f32) -> f32 {
    let list = entries(value);
    if list.is_empty() {
        return IMAGE_BTN * zoom;
    }
    if display_mode(value) == "grid" {
        let cols = grid_cols(col_w, zoom, list.len());
        let tile = grid_tile_w(col_w, zoom, cols).min(IMAGE_GRID_SINGLE_MAX * zoom);
        let rows = list.len().div_ceil(cols);
        return rows as f32 * tile
            + (rows.saturating_sub(1)) as f32 * IMAGE_GAP * zoom
            + 8.0 * zoom;
    }
    IMAGE_CAROUSEL_H * zoom + if list.len() > 1 { IMAGE_QUEUE_H * zoom } else { 0.0 }
}

/// 九宫格列数：≤2 张按张数；宽 ≥200×z 取 3 列，否则 2 列。
pub fn grid_cols(col_w: f32, zoom: f32, count: usize) -> usize {
    if count <= 2 {
        return count.max(1);
    }
    if col_w >= 200.0 * zoom {
        3
    } else {
        2
    }
}

/// 九宫格单块宽（按列数与间隙均分，右侧留 2×z 内衬）。
pub fn grid_tile_w(col_w: f32, zoom: f32, cols: usize) -> f32 {
    let inner = (col_w - 2.0 * CELL_PAD_X * zoom - 2.0 * zoom).max(0.0);
    ((inner - (cols.saturating_sub(1)) as f32 * IMAGE_GAP * zoom) / cols as f32)
        .max(24.0 * zoom)
}

/// 图片格布局（位置均为面本地坐标）。
#[derive(Default)]
pub struct ImageLayout {
    pub main: Option<Rect>,
    /// 队列缩略图：真实图片下标 + 绘制区（窗口以当前页收尾，下标 ≠ 切片位置）。
    pub thumbs: Vec<(usize, Rect)>,
    /// 九宫格图块：真实图片下标 + 绘制区。
    pub tiles: Vec<(usize, Rect)>,
    pub mode_btn: Option<Rect>,
    pub add_btn: Option<Rect>,
    pub remove_btn: Option<Rect>,
    /// 空值的整格添加热区。
    pub empty_add: Option<Rect>,
}

/// 图片格布局：`current` = 轮播当前页。
pub fn layout_of(cell: Rect, value: Option<&serde_json::Value>, zoom: f32, current: usize) -> ImageLayout {
    let list = entries(value);
    let pad_x = CELL_PAD_X * zoom;
    let pad_y = CELL_PAD_Y * zoom;
    let inner = Rect::new(
        cell.x + pad_x,
        cell.y + pad_y,
        (cell.w - 2.0 * pad_x).max(0.0),
        (cell.h - 2.0 * pad_y).max(0.0),
    );
    if list.is_empty() {
        return ImageLayout { empty_add: Some(inner), ..ImageLayout::default() };
    }
    let btn = IMAGE_BTN * zoom;
    let multi = list.len() > 1;
    let current = current.min(list.len() - 1);
    if display_mode(value) == "grid" {
        let cols = grid_cols(cell.w, zoom, list.len());
        let tile_w = grid_tile_w(cell.w, zoom, cols);
        let tile_h = tile_w.min(IMAGE_GRID_SINGLE_MAX * zoom);
        let mut tiles = Vec::new();
        for index in 0..list.len() {
            let row = index / cols;
            let col = index % cols;
            tiles.push((
                index,
                Rect::new(
                    inner.x + col as f32 * (tile_w + IMAGE_GAP * zoom),
                    inner.y + row as f32 * (tile_h + IMAGE_GAP * zoom),
                    tile_w,
                    tile_h,
                ),
            ));
        }
        return ImageLayout {
            main: None,
            thumbs: Vec::new(),
            tiles,
            mode_btn: multi.then(|| Rect::new(inner.x, inner.y, btn, btn)),
            add_btn: Some(Rect::new(inner.x + btn + IMAGE_GAP * zoom, inner.y, btn, btn)),
            remove_btn: None,
            empty_add: None,
        };
    }
    let queue_h = if multi { IMAGE_QUEUE_H * zoom } else { 0.0 };
    let main = Rect::new(inner.x, inner.y, inner.w, (inner.h - queue_h).max(0.0));
    let mut thumbs = Vec::new();
    if multi {
        let step = (IMAGE_THUMB + IMAGE_GAP) * zoom;
        let capacity = (((inner.w - 4.0 * zoom) / step).floor() as usize).max(1);
        let start = current.saturating_sub(capacity - 1);
        for index in list.iter().enumerate().skip(start).take(capacity).map(|(index, _)| index) {
            let slot = index - start;
            thumbs.push((
                index,
                Rect::new(
                    inner.x + 2.0 * zoom + slot as f32 * step,
                    inner.y + main.h + 4.0 * zoom,
                    IMAGE_THUMB * zoom,
                    IMAGE_THUMB * zoom,
                ),
            ));
        }
    }
    ImageLayout {
        main: Some(main),
        thumbs,
        tiles: Vec::new(),
        mode_btn: multi.then(|| Rect::new(inner.x, inner.y, btn, btn)),
        add_btn: Some(Rect::new(inner.x + btn + IMAGE_GAP * zoom, inner.y, btn, btn)),
        remove_btn: Some(Rect::new(inner.x + inner.w - btn, inner.y, btn, btn)),
        empty_add: None,
    }
}

/// 图片来源：仓库路径按内容面读取（缩略图带上限）。
pub fn source_of(rel: &str, max_edge: Option<u32>) -> ImageSource {
    ImageSource::Vault { path: rel.to_string(), max_edge }
}

/// 绘制图片格：主图 / 九宫格 / 队列 / 按钮（按钮只在指针位于本格时出现）。
pub fn paint_cell(
    out: &mut Vec<DrawCommand>,
    session: &Session,
    row: usize,
    field: usize,
    rect: Rect,
    value: Option<&serde_json::Value>,
) {
    let zoom = session.view.zoom;
    let row_id = session.data.rows.get(row).map(|row| row.id.as_str()).unwrap_or_default();
    let field_id = session.data.fields.get(field).map(|field| field.id.as_str()).unwrap_or_default();
    let current = session
        .view
        .carousel
        .get(&(row_id.to_string(), field_id.to_string()))
        .copied()
        .unwrap_or(0);
    let layout = layout_of(rect, value, zoom, current);
    let list = entries(value);
    if let Some(area) = layout.empty_add {
        if hovered(session, row, field) {
            out.push(fill(area, token(ColorToken::Hover)));
        }
        push_plus(out, area.x + area.w / 2.0, area.y + area.h / 2.0, 7.0 * zoom, token(ColorToken::TextMuted));
        return;
    }
    let current = current.min(list.len().saturating_sub(1));
    let edge = Some(crate::model::THUMB_MAX_EDGE);
    for (index, tile) in &layout.tiles {
        let Some(rel) = list.get(*index) else { continue };
        out.push(fill(*tile, token(ColorToken::SurfaceSunken)));
        out.push(image(*tile, source_of(rel, edge), Some(ImageFit::Contain)));
    }
    // 长按拖拽：被拖图块描边。
    if let Some(drag) = session
        .view
        .img_drag
        .as_ref()
        .filter(|drag| drag.row_id == row_id && drag.field_id == field_id)
    {
        if let Some((_, from)) = layout.tiles.get(drag.from) {
            out.push(outline(*from, 1.0, token(ColorToken::Accent)));
        }
    }
    if let Some(main) = layout.main {
        let dx = swipe_dx(session, row_id, field_id, current, list.len());
        for offset in [-1i32, 0, 1] {
            if offset != 0 && dx == 0.0 {
                continue;
            }
            let page = current as i32 + offset;
            if page < 0 || page as usize >= list.len() {
                continue;
            }
            let x = main.x + dx + offset as f32 * main.w;
            if x + main.w < rect.x || x > rect.right() {
                continue;
            }
            out.push(image(
                Rect::new(x, main.y, main.w, main.h),
                source_of(&list[page as usize], edge),
                Some(ImageFit::Contain),
            ));
        }
    }
    for (index, thumb) in &layout.thumbs {
        let Some(rel) = list.get(*index) else { continue };
        out.push(fill(*thumb, token(ColorToken::SurfaceSunken)));
        let inset = 2.0 * zoom;
        out.push(image(
            Rect::new(
                thumb.x + inset,
                thumb.y + inset,
                (thumb.w - 2.0 * inset).max(0.0),
                (thumb.h - 2.0 * inset).max(0.0),
            ),
            source_of(rel, edge),
            Some(ImageFit::Contain),
        ));
        let border = if *index == current { ColorToken::Accent } else { ColorToken::Border };
        out.push(outline(*thumb, 2.0 * zoom, token(border)));
    }
    if hovered(session, row, field) {
        if let Some(button) = layout.mode_btn {
            push_button(out, button, zoom, ButtonIcon::Mode);
        }
        if let Some(button) = layout.add_btn {
            push_button(out, button, zoom, ButtonIcon::Add);
        }
        if let Some(button) = layout.remove_btn {
            push_button(out, button, zoom, ButtonIcon::Remove);
        }
    }
}

/// 指针是否落在该图片格上（圆钮与空态悬停底只在悬停格出现）。
fn hovered(session: &Session, row: usize, field: usize) -> bool {
    matches!(session.view.hover, Some(Hit::Cell { row: r, field: f }) if r == row && f == field)
}

/// 轮播跟手位移：只取本格按压会话的位移，到两端时按阻尼衰减。
fn swipe_dx(session: &Session, row_id: &str, field_id: &str, index: usize, count: usize) -> f32 {
    let Some((row, field, _)) = session.view.pending_image_open.as_ref() else { return 0.0 };
    if row != row_id || field != field_id {
        return 0.0;
    }
    let Some(swipe) = session.view.swipe.as_ref() else { return 0.0 };
    let edge = (index == 0 && swipe.dx > 0.0) || (index + 1 >= count && swipe.dx < 0.0);
    if edge {
        swipe.dx * IMAGE_EDGE_DAMPING
    } else {
        swipe.dx
    }
}

/// 图片格按钮图标。
#[derive(Clone, Copy)]
enum ButtonIcon {
    Mode,
    Add,
    Remove,
}

/// 圆形按钮底 + 图标。
fn push_button(out: &mut Vec<DrawCommand>, button: Rect, zoom: f32, icon: ButtonIcon) {
    let cx = button.x + button.w / 2.0;
    let cy = button.y + button.h / 2.0;
    out.push(DrawCommand::Circle {
        cx,
        cy,
        r: (button.w / 2.0).max(1.0),
        fill: Some(token(ColorToken::Scrim)),
        stroke: None,
        stroke_width: None,
    });
    let color = token(ColorToken::Text);
    match icon {
        ButtonIcon::Mode => {
            let half = 5.0 * zoom;
            out.push(outline(
                Rect::new(cx - half, cy - half, half * 2.0, half * 2.0),
                1.3,
                color,
            ));
            out.push(DrawCommand::Line {
                x1: cx,
                y1: cy - half,
                x2: cx,
                y2: cy + half,
                stroke: color,
                width: 1.3,
                dash: None,
            });
        }
        ButtonIcon::Add => push_plus(out, cx, cy, 5.0 * zoom, color),
        ButtonIcon::Remove => {
            let d = 4.0 * zoom;
            out.push(DrawCommand::Line { x1: cx - d, y1: cy - d, x2: cx + d, y2: cy + d, stroke: color, width: 1.5, dash: None });
            out.push(DrawCommand::Line { x1: cx - d, y1: cy + d, x2: cx + d, y2: cy - d, stroke: color, width: 1.5, dash: None });
        }
    }
}

/// 加号（两条线）。
fn push_plus(out: &mut Vec<DrawCommand>, cx: f32, cy: f32, half: f32, color: PaintColor) {
    out.push(DrawCommand::Line { x1: cx - half, y1: cy, x2: cx + half, y2: cy, stroke: color, width: 1.5, dash: None });
    out.push(DrawCommand::Line { x1: cx, y1: cy - half, x2: cx, y2: cy + half, stroke: color, width: 1.5, dash: None });
}

/// 图片贴片。
fn image(rect: Rect, source: ImageSource, fit: Option<ImageFit>) -> DrawCommand {
    DrawCommand::Image { source, x: rect.x, y: rect.y, w: rect.w, h: rect.h, fit }
}

/// 实心矩形（图块底 / 悬停底）。
fn fill(rect: Rect, color: PaintColor) -> DrawCommand {
    DrawCommand::Rect {
        x: rect.x,
        y: rect.y,
        w: rect.w,
        h: rect.h,
        fill: Some(color),
        stroke: None,
        stroke_width: None,
        corner_radius: None,
        shadow: None,
    }
}

/// 描边矩形（九宫格拖拽占位 / 队列缩略图边框）。
fn outline(rect: Rect, width: f32, color: PaintColor) -> DrawCommand {
    DrawCommand::Rect {
        x: rect.x,
        y: rect.y,
        w: rect.w,
        h: rect.h,
        fill: None,
        stroke: Some(color),
        stroke_width: Some(width),
        corner_radius: None,
        shadow: None,
    }
}

fn token(name: ColorToken) -> PaintColor {
    PaintColor::Token { name }
}
