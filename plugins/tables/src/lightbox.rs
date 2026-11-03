//! 灯箱（图片全屏预览）：浮层帧绘制与输入（滚轮缩放、拖动平移、双击复位、左右翻页、点外关闭）。

use atelyx_plugin_protocol::frame::{FrameRect, PopupCapture, SurfacePopup};
use atelyx_plugin_protocol::paint::{ColorToken, DrawCommand, ImageFit, PaintColor};

use crate::images;
use crate::model::{Lightbox, Point, Rect, Session, Size};

/// 灯箱要盖住面板顶部头部行（宿主头部行高 30）。
pub const HEADER_INSET: f32 = 30.0;
/// 显示区内边距（px）。
const INSET: f32 = 48.0;
/// 未声明排版口径时帧文本的相对行高（与宿主缺省一致）。
const TEXT_LINE: f32 = 1.2;

/// 灯箱当前图（仓库相对路径；下标越界时取末张）。
pub fn current_image(session: &Session, lightbox: &Lightbox) -> Option<String> {
    let row = session.data.rows.iter().find(|row| row.id == lightbox.row_id)?;
    let value = row.values.get(&lightbox.field_id)?;
    let list = images::entries(Some(value));
    list.get(lightbox.index.min(list.len().checked_sub(1)?)).cloned()
}

/// 当前图片格的图片张数。
pub fn image_count(session: &Session, lightbox: &Lightbox) -> usize {
    session
        .data
        .rows
        .iter()
        .find(|row| row.id == lightbox.row_id)
        .and_then(|row| row.values.get(&lightbox.field_id))
        .map(|value| images::entries(Some(value)).len())
        .unwrap_or(0)
}

/// 灯箱浮层帧：遮罩 + 图片（含平移）+ 翻页按钮 + 计数。
pub fn popup_frame(session: &Session, size: Size) -> Option<SurfacePopup> {
    let lightbox = session.view.lightbox.as_ref()?;
    let mut commands = Vec::new();
    let rect = FrameRect {
        x: 0.0,
        y: -HEADER_INSET,
        w: size.width.max(1.0),
        h: (size.height + HEADER_INSET).max(1.0),
    };
    // 遮罩
    commands.push(DrawCommand::Rect {
        x: rect.x,
        y: rect.y,
        w: rect.w,
        h: rect.h,
        fill: Some(PaintColor::Token { name: ColorToken::Scrim }),
        stroke: None,
        stroke_width: None,
        corner_radius: None,
        shadow: None,
    });
    let inner = Rect::new(
        INSET,
        INSET - HEADER_INSET,
        (size.width - 2.0 * INSET).max(1.0),
        (size.height - 2.0 * INSET).max(1.0),
    );
    let display = display_rect(inner, lightbox.zoom, lightbox.pan);
    let count = image_count(session, lightbox);
    match current_image(session, lightbox) {
        Some(rel) => {
            commands.push(DrawCommand::Image {
                source: atelyx_plugin_protocol::paint::ImageSource::Vault { path: rel, max_edge: None },
                x: display.x,
                y: display.y,
                w: display.w,
                h: display.h,
                fit: Some(ImageFit::Contain),
            });
        }
        None => {
            commands.push(DrawCommand::Text {
                x: rect.x,
                y: rect.y + rect.h / 2.0 - crate::theme::font_caption() * TEXT_LINE / 2.0,
                content: "图片已清空".to_string(),
                size: crate::theme::font_caption(),
                color: PaintColor::Token { name: ColorToken::TextMuted },
                weight: None,
                family: None,
                family_name: None,
                italic: false,
                max_width: Some(rect.w),
                align: Some(atelyx_plugin_protocol::paint::TextAlign::Center),
                layout: None,
            });
        }
    }
    // 翻页按钮（多图）：左右两侧垂直居中
    if count > 1 {
        let mid_y = size.height / 2.0;
        for x in [24.0, rect.w - 24.0] {
            let left = x < rect.w / 2.0;
            commands.push(DrawCommand::Circle {
                cx: x,
                cy: mid_y,
                r: 16.0,
                fill: Some(PaintColor::Token { name: ColorToken::Scrim }),
                stroke: None,
                stroke_width: None,
            });
            let (tip, back) = if left { (x + 3.0, x - 3.0) } else { (x - 3.0, x + 3.0) };
            commands.push(DrawCommand::Path {
                points: vec![
                    atelyx_plugin_protocol::paint::PaintPoint { x: tip, y: mid_y - 5.0 },
                    atelyx_plugin_protocol::paint::PaintPoint { x: back, y: mid_y },
                    atelyx_plugin_protocol::paint::PaintPoint { x: tip, y: mid_y + 5.0 },
                ],
                closed: false,
                fill: None,
                stroke: Some(PaintColor::Token { name: ColorToken::Text }),
                stroke_width: Some(1.5),
            });
        }
    }
    // 计数（底部居中）
    if count > 1 {
        let index = lightbox.index.min(count - 1);
        commands.push(DrawCommand::Text {
            x: rect.x,
            y: rect.y + rect.h - 8.0 - crate::theme::font_caption() * TEXT_LINE,
            content: format!("{}/{}", index + 1, count),
            size: crate::theme::font_caption(),
            color: PaintColor::Token { name: ColorToken::Text },
            weight: None,
            family: None,
            family_name: None,
            italic: false,
            max_width: Some(rect.w),
            align: Some(atelyx_plugin_protocol::paint::TextAlign::Center),
            layout: None,
        });
    }
    Some(SurfacePopup {
        rect,
        capture: PopupCapture::All,
        commands,
        hits: Vec::new(),
        ime: None,
    })
}

/// 灯箱显示区：内框按倍率放大并居中，叠加夹紧后的平移；图片由宿主在框内按比例 `Contain`。
pub fn display_rect(inner: Rect, zoom: f32, pan: Point) -> Rect {
    let w = inner.w * zoom;
    let h = inner.h * zoom;
    let base_x = inner.x - (w - inner.w) / 2.0;
    let base_y = inner.y - (h - inner.h) / 2.0;
    let max_pan_x = ((w - inner.w) / 2.0).max(0.0);
    let max_pan_y = ((h - inner.h) / 2.0).max(0.0);
    Rect::new(
        base_x + pan.x.clamp(-max_pan_x, max_pan_x),
        base_y + pan.y.clamp(-max_pan_y, max_pan_y),
        w,
        h,
    )
}

/// 把累积平移夹回可拖范围，避免松手后状态越界、下一次拖拽先跳一段。
pub fn clamp_pan(lightbox: &mut Lightbox, size: Size) {
    let inner = inner_rect(size);
    let max_x = ((inner.w * lightbox.zoom - inner.w) / 2.0).max(0.0);
    let max_y = ((inner.h * lightbox.zoom - inner.h) / 2.0).max(0.0);
    lightbox.pan = Point::new(
        lightbox.pan.x.clamp(-max_x, max_x),
        lightbox.pan.y.clamp(-max_y, max_y),
    );
}

/// 灯箱显示区（面本地坐标，供输入判定复用）。
pub fn inner_rect(size: Size) -> Rect {
    Rect::new(
        INSET,
        INSET - HEADER_INSET,
        (size.width - 2.0 * INSET).max(1.0),
        (size.height - 2.0 * INSET).max(1.0),
    )
}

/// 翻页：`dir` = -1 上一张 / 1 下一张（取模成环，切图复位缩放）。
pub fn navigate(lightbox: &mut Lightbox, count: usize, dir: i32) {
    if count == 0 {
        return;
    }
    lightbox.index = (lightbox.index as i32 + dir).rem_euclid(count as i32) as usize;
    lightbox.reset_zoom();
}

/// 滚轮缩放（以光标为锚）：协议里 `delta_y` 为正 = 向下滚 = 缩小。
pub fn zoom_at(lightbox: &mut Lightbox, size: Size, cursor: Point, delta_y: f32) {
    let before = display_rect(inner_rect(size), lightbox.zoom, lightbox.pan);
    let factor = (-delta_y * 0.0035).exp();
    let next = (lightbox.zoom * factor).clamp(1.0, 8.0);
    if (next - lightbox.zoom).abs() < f32::EPSILON {
        return;
    }
    // 以光标为锚：缩放后同一点仍落在光标处
    let anchor_x = (cursor.x - before.x) / before.w.max(1.0);
    let anchor_y = (cursor.y - before.y) / before.h.max(1.0);
    lightbox.zoom = next;
    let after = display_rect(inner_rect(size), lightbox.zoom, lightbox.pan);
    let target_x = cursor.x - anchor_x * after.w;
    let target_y = cursor.y - anchor_y * after.h;
    lightbox.pan = Point::new(target_x - after.x + lightbox.pan.x, target_y - after.y + lightbox.pan.y);
    clamp_pan(lightbox, size);
}
