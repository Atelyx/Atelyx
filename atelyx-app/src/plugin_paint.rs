//! 插件绘制指令执行：整面帧的图元 / 文字 / 图片共用一套解释，
//! 坐标一律相对当前绘制坐标系；图片按插件身份与来源取宿主已载入的句柄。

use iced::widget::canvas::{self, Path as CanvasPath, Stroke, Text as CanvasText};
use iced::{Color, Point, Rectangle, Size};

use atelyx_core::plugin_runtime::{
    PaintColor, PaintPoint, DrawCommand, ImageFit, ImageSource, TextAlign, TextFamily, TextWeight,
};

use crate::App;

/// 绘制上下文：插件身份决定相对路径 / 私有目录图片的解析根。
pub struct PaintCtx<'a> {
    pub app: &'a App,
    pub plugin_id: &'a str,
}

/// 按表序执行一批指令。
pub fn execute<'a>(
    frame: &mut canvas::Frame,
    commands: impl IntoIterator<Item = &'a DrawCommand>,
    ctx: &PaintCtx<'_>,
) {
    for command in commands {
        execute_one(frame, command, ctx);
    }
}

fn execute_one(frame: &mut canvas::Frame, command: &DrawCommand, ctx: &PaintCtx<'_>) {
    match command {
        DrawCommand::Line { x1, y1, x2, y2, stroke, width } => {
            let path = CanvasPath::line(Point::new(*x1, *y1), Point::new(*x2, *y2));
            frame.stroke(&path, stroke_of(color(*stroke), *width));
        }
        DrawCommand::Rect { x, y, w, h, fill, stroke, stroke_width, corner_radius } => {
            let top_left = Point::new(*x, *y);
            let size = Size::new(*w, *h);
            let radius = corner_radius.filter(|r| *r > 0.0);
            if let Some(fill) = fill {
                match radius {
                    Some(radius) => frame.fill(
                        &CanvasPath::rounded_rectangle(top_left, size, radius.into()),
                        color(*fill),
                    ),
                    None => frame.fill_rectangle(top_left, size, color(*fill)),
                }
            }
            if let Some(stroke) = stroke {
                let width = stroke_width.unwrap_or(1.0);
                match radius {
                    Some(radius) => frame.stroke(
                        &CanvasPath::rounded_rectangle(top_left, size, radius.into()),
                        stroke_of(color(*stroke), width),
                    ),
                    None => frame.stroke_rectangle(top_left, size, stroke_of(color(*stroke), width)),
                }
            }
        }
        DrawCommand::Path { points, closed, fill, stroke, stroke_width } => {
            let path = polyline(points, *closed);
            if let Some(fill) = fill {
                frame.fill(&path, color(*fill));
            }
            if let Some(stroke) = stroke {
                frame.stroke(&path, stroke_of(color(*stroke), stroke_width.unwrap_or(1.0)));
            }
        }
        DrawCommand::Circle { cx, cy, r, fill, stroke, stroke_width } => {
            let path = CanvasPath::circle(Point::new(*cx, *cy), *r);
            if let Some(fill) = fill {
                frame.fill(&path, color(*fill));
            }
            if let Some(stroke) = stroke {
                frame.stroke(&path, stroke_of(color(*stroke), stroke_width.unwrap_or(1.0)));
            }
        }
        DrawCommand::Text { x, y, content, size, color: text_color, weight, family, max_width, align } => {
            frame.fill_text(CanvasText {
                content: content.clone(),
                position: Point::new(text_x(*x, content, *size, *weight, *family, *max_width, *align), *y),
                color: color(*text_color),
                size: (*size).into(),
                font: crate::plugin_theme::text_font(*weight, *family),
                max_width: max_width.unwrap_or(f32::INFINITY),
                // 缺省 Shaping::Auto 已覆盖 CJK 文本
                ..CanvasText::default()
            });
        }
        DrawCommand::Image { source, x, y, w, h, fit } => {
            let Some((handle, source_size)) = image_handle(ctx, source) else {
                return;
            };
            let target = Rectangle::new(Point::new(*x, *y), Size::new(*w, *h));
            let fit = fit.unwrap_or(ImageFit::Fill);
            // 原图尺寸未就绪（或解码失败）：按拉伸绘制，等比尺寸无从计算
            let Some((source_w, source_h)) = source_size.filter(|(w, h)| *w > 0 && *h > 0) else {
                frame.draw_image(target, &handle);
                return;
            };
            let (source_w, source_h) = (source_w as f32, source_h as f32);
            let scale = match fit {
                ImageFit::Fill => None,
                ImageFit::Contain => Some((w / source_w).min(h / source_h)),
                ImageFit::Cover => Some((w / source_w).max(h / source_h)),
            };
            let Some(scale) = scale else {
                frame.draw_image(target, &handle);
                return;
            };
            let draw_w = source_w * scale;
            let draw_h = source_h * scale;
            let rect = Rectangle::new(
                Point::new(x + (w - draw_w) / 2.0, y + (h - draw_h) / 2.0),
                Size::new(draw_w, draw_h),
            );
            // cover 的溢出部分裁到目标框；contain 的绘制框本就不超出
            if fit == ImageFit::Cover {
                frame.with_clip(target, |frame| frame.draw_image(rect, &handle));
            } else {
                frame.draw_image(rect, &handle);
            }
        }
    }
}

/// 文字摆放的横坐标：对齐区域 = `maxWidth`，区域左缘 = `x`。
/// 单行时 iced 的段落对齐不生效，多行时按段落自身宽度对齐（不等于区域宽），
/// 故对齐一律由本端按量算结果换算，段落本身保持左对齐。
fn text_x(
    x: f32,
    content: &str,
    size: f32,
    weight: Option<TextWeight>,
    family: Option<TextFamily>,
    max_width: Option<f32>,
    align: Option<TextAlign>,
) -> f32 {
    let (Some(max_width), Some(align)) = (max_width, align) else {
        return x;
    };
    let width = crate::plugin_theme::measure_text(content, size, weight, family, Some(max_width)).width;
    match align {
        TextAlign::Start => x,
        TextAlign::Center => x + (max_width - width) / 2.0,
        TextAlign::End => x + max_width - width,
    }
}

fn polyline(points: &[PaintPoint], closed: bool) -> CanvasPath {
    CanvasPath::new(|path| {
        let mut points = points.iter();
        if let Some(first) = points.next() {
            path.move_to(Point::new(first.x, first.y));
        }
        for point in points {
            path.line_to(Point::new(point.x, point.y));
        }
        if closed {
            path.close();
        }
    })
}

/// 图片来源 → 已载入的句柄与固有尺寸（未载入 / 不可用 = `None`，该条指令跳过）。
fn image_handle(
    ctx: &PaintCtx<'_>,
    source: &ImageSource,
) -> Option<(iced::widget::image::Handle, Option<(u32, u32)>)> {
    let key = crate::plugin_actions::image_key(ctx.app, ctx.plugin_id, source)?;
    let handle = ctx.app.note_images.get(&key).cloned()?;
    Some((handle, ctx.app.note_image_sizes.get(&key).copied()))
}

fn color(value: PaintColor) -> Color {
    match value {
        PaintColor::Token { name } => crate::plugin_theme::token_color(name),
        PaintColor::Rgba { rgba: [r, g, b, a] } => Color::from_rgba8(r, g, b, f32::from(a) / 255.0),
    }
}

fn stroke_of(color: Color, width: f32) -> Stroke<'static> {
    Stroke { style: canvas::Style::Solid(color), width: width.max(0.1), ..Stroke::default() }
}
