//! 插件绘制指令执行：图元与图片解释成 canvas 几何，图片按插件身份与来源取已载入的句柄。
//! 文字由 [`TextCache`] 以段落为单位缓存并绘制（内容不变就不重新成形），恒在形状与图片之上。

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use iced::advanced::text::paragraph;
use iced::advanced::text::{
    Alignment, LineHeight, Renderer as _, Shaping, Text as TextDef, Wrapping,
};
use iced::alignment::Vertical;
use iced::widget::canvas::{self, Path as CanvasPath, Stroke};
use iced::{Color, Pixels, Point, Rectangle, Size};

use atelyx_core::plugin_runtime::{
    PaintColor, PaintPoint, DrawCommand, ImageFit, ImageSource, TextAlign, TextFamily, TextWeight,
};

use crate::App;

/// 面板文字的段落类型（与宿主原生控件同一种：字形图集 + 缓存的排版结果）。
type TextParagraph = <iced::Renderer as iced::advanced::text::Renderer>::Paragraph;

/// 文字缓存条数上限：一屏通常远小于此，超出只留本帧用到的。
const MAX_TEXT_ENTRIES: usize = 1024;
/// 文字缓存内容字节上限：超出的帧退回逐帧排版（与不缓存时的成本相同），
/// 不拿插件正文去换宿主内存。
const MAX_TEXT_BYTES: usize = 512 << 10;

/// 绘制上下文：插件身份决定相对路径 / 私有目录图片的解析根。
pub struct PaintCtx<'a> {
    pub app: &'a App,
    pub plugin_id: &'a str,
}

/// 按表序执行一批非文字指令（文字恒在形状之上，走 [`TextCache`]）。
pub fn execute_shapes<'a>(
    frame: &mut canvas::Frame,
    commands: impl IntoIterator<Item = &'a DrawCommand>,
    ctx: &PaintCtx<'_>,
) {
    for command in commands {
        if !matches!(command, DrawCommand::Text { .. }) {
            execute_one(frame, command, ctx);
        }
    }
}

/// 一条已排版的文字：内容与排版参数不变时直接复用段落，不重新成形。
struct TextEntry {
    size_bits: u32,
    weight: Option<TextWeight>,
    family: Option<TextFamily>,
    max_width_bits: Option<u32>,
    paragraph: paragraph::Plain<TextParagraph>,
    /// 排版后的最小宽度（对齐换算用）。
    width: f32,
    last_used: u64,
}

/// 插件面的文字段落缓存：按内容 + 字号 + 字重 + 字族 + 最大宽度查段落。
/// 帧换了不清（改宽 / 滚动 / 悬停时文字内容通常不变），字体族变更时整体清空。
#[derive(Default)]
pub struct TextCache {
    buckets: HashMap<u64, Vec<TextEntry>>,
    entries: usize,
    bytes: usize,
    frame: u64,
}

impl TextCache {
    pub fn clear(&mut self) {
        self.buckets.clear();
        self.entries = 0;
        self.bytes = 0;
    }

    /// 按指令顺序绘制文字：位置用面本地坐标，接上外壳原点的平移。
    pub fn draw(
        &mut self,
        renderer: &mut iced::Renderer,
        commands: &[DrawCommand],
        bounds: Rectangle,
    ) {
        self.frame += 1;
        for command in commands {
            let DrawCommand::Text {
                x,
                y,
                content,
                size,
                color: text_color,
                weight,
                family,
                max_width,
                align,
            } = command
            else {
                continue;
            };
            let (paragraph, width) =
                self.entry(content, *size, *weight, *family, *max_width);
            let x = aligned_x(*x, *max_width, *align, width);
            renderer.fill_paragraph(
                paragraph,
                Point::new(bounds.x + x, bounds.y + *y),
                color(*text_color),
                bounds,
            );
        }
        self.trim(MAX_TEXT_ENTRIES, MAX_TEXT_BYTES);
    }

    /// 取一条段落；缺项即排版并落缓存。返回段落与它的最小宽度。
    fn entry(
        &mut self,
        content: &str,
        size: f32,
        weight: Option<TextWeight>,
        family: Option<TextFamily>,
        max_width: Option<f32>,
    ) -> (&TextParagraph, f32) {
        let size_bits = size.to_bits();
        let max_width_bits = max_width.map(f32::to_bits);
        let hash = text_hash(content, size_bits, weight, family, max_width_bits);
        let frame = self.frame;
        let bucket = self.buckets.entry(hash).or_default();
        let index = bucket.iter().position(|entry| {
            entry.size_bits == size_bits
                && entry.weight == weight
                && entry.family == family
                && entry.max_width_bits == max_width_bits
                && entry.paragraph.content() == content
        });
        let index = match index {
            Some(index) => index,
            None => {
                let paragraph = build_paragraph(content, size, weight, family, max_width);
                let width = paragraph.min_bounds().width;
                self.entries += 1;
                self.bytes += content.len();
                bucket.push(TextEntry {
                    size_bits,
                    weight,
                    family,
                    max_width_bits,
                    paragraph,
                    width,
                    last_used: frame,
                });
                bucket.len() - 1
            }
        };
        let entry = &mut bucket[index];
        entry.last_used = frame;
        (entry.paragraph.raw(), entry.width)
    }

    /// 超上限时只留本帧用到的；仍超（单帧条目过多）即整体清空，下一帧重建。
    fn trim(&mut self, max_entries: usize, max_bytes: usize) {
        if self.entries <= max_entries && self.bytes <= max_bytes {
            return;
        }
        let frame = self.frame;
        for bucket in self.buckets.values_mut() {
            bucket.retain(|entry| entry.last_used == frame);
        }
        self.recount();
        if self.entries > max_entries || self.bytes > max_bytes {
            self.clear();
        }
    }

    fn recount(&mut self) {
        self.entries = 0;
        self.bytes = 0;
        self.buckets.retain(|_, bucket| {
            for entry in bucket.iter() {
                self.entries += 1;
                self.bytes += entry.paragraph.content().len();
            }
            !bucket.is_empty()
        });
    }
}

/// 按内容与排版参数排版一段文字。参数与 canvas 文本保持一致（行高 1.2、词级换行、
/// 左上对齐），否则同一段文字在两种绘制路径下的位置与换行会不一致。
fn build_paragraph(
    content: &str,
    size: f32,
    weight: Option<TextWeight>,
    family: Option<TextFamily>,
    max_width: Option<f32>,
) -> paragraph::Plain<TextParagraph> {
    let mut paragraph = paragraph::Plain::default();
    paragraph.update(TextDef {
        content,
        bounds: Size::new(max_width.unwrap_or(f32::INFINITY), f32::INFINITY),
        size: Pixels(size),
        line_height: LineHeight::Relative(1.2),
        font: crate::plugin_theme::text_font(weight, family),
        align_x: Alignment::Default,
        align_y: Vertical::Top,
        shaping: Shaping::default(),
        wrapping: Wrapping::default(),
    });
    paragraph
}

/// 对齐区域的横坐标换算：区域左缘 = `x`，宽度 = `max_width`。
fn aligned_x(x: f32, max_width: Option<f32>, align: Option<TextAlign>, width: f32) -> f32 {
    match (max_width, align) {
        (Some(max_width), Some(TextAlign::Center)) => x + (max_width - width) / 2.0,
        (Some(max_width), Some(TextAlign::End)) => x + max_width - width,
        _ => x,
    }
}

fn text_hash(
    content: &str,
    size_bits: u32,
    weight: Option<TextWeight>,
    family: Option<TextFamily>,
    max_width_bits: Option<u32>,
) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut hasher);
    size_bits.hash(&mut hasher);
    weight.map(|weight| weight as u8).hash(&mut hasher);
    family.map(|family| family as u8).hash(&mut hasher);
    max_width_bits.hash(&mut hasher);
    hasher.finish()
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
        // 文字不在几何缓存里：由 [`TextCache`] 按段落跨帧复用、绘制在形状与图片之上
        DrawCommand::Text { .. } => {}
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligned_text_uses_the_measured_width() {
        assert_eq!(aligned_x(10.0, Some(100.0), Some(TextAlign::Center), 20.0), 50.0);
        assert_eq!(aligned_x(10.0, Some(100.0), Some(TextAlign::End), 20.0), 90.0);
        assert_eq!(aligned_x(10.0, Some(100.0), Some(TextAlign::Start), 20.0), 10.0);
        assert_eq!(aligned_x(10.0, None, None, 20.0), 10.0);
    }

    #[test]
    fn text_cache_reuses_the_paragraph_of_unchanged_text() {
        let mut cache = TextCache { frame: 1, ..TextCache::default() };
        let (first, width) = cache.entry("同一段文字", 13.0, None, None, None);
        let first: *const TextParagraph = first;
        cache.frame = 2;
        let (second, width_again) = cache.entry("同一段文字", 13.0, None, None, None);
        assert_eq!(first, second, "内容与排版参数不变即复用同一段落");
        assert_eq!(width, width_again);
        assert_eq!(cache.entries, 1);

        let _ = cache.entry("另一段文字", 13.0, None, None, None);
        assert_eq!(cache.entries, 2, "内容变了才新增条目");
    }

    #[test]
    fn text_cache_trims_entries_unused_by_the_current_frame() {
        let mut cache = TextCache { frame: 1, ..TextCache::default() };
        for index in 0..4 {
            let _ = cache.entry(&format!("line {index}"), 13.0, None, None, None);
        }
        cache.frame = 2;
        let _ = cache.entry("keep", 13.0, None, None, None);
        cache.trim(3, usize::MAX);
        assert_eq!(cache.entries, 1, "超出上限只留本帧用到的");

        // 单帧条目本身就超上限：整体清空，下一帧重建
        cache.trim(0, usize::MAX);
        assert_eq!(cache.entries, 0);
    }
}
