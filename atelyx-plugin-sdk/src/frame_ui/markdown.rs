//! 只读 Markdown 的排版与绘制：宿主给出结构化块，这里定字号 / 缩进 / 行距并落成行与形状。
//! 行内样式只有字重、等宽与链接色（指令集的文字没有斜体）；换行按可用宽度逐字测量。

use std::sync::{Arc, Mutex};

use atelyx_plugin_protocol::paint::{PaintColor, DrawCommand};
use atelyx_plugin_protocol::method::{MarkdownBlock, MarkdownSpan};
use atelyx_plugin_protocol::paint::{ColorToken, TextFamily, TextWeight};

use super::build::BuildCtx;
use super::geom::Rect;
use super::metrics::Metrics;
use super::model::FontSpec;
use super::text;
use super::theme::{self, Theme};

/// 每层嵌套的缩进。
const DEPTH_INDENT: f32 = 14.0;
/// 块之间的垂直间距。
const BLOCK_GAP: f32 = 6.0;
/// 行高倍数（按字号）。
const LINE_FACTOR: f32 = 1.5;
/// 引用竖条的宽度与文字左移量。
const QUOTE_BAR: f32 = 3.0;
const QUOTE_PAD: f32 = 8.0;
/// 代码块内边距。
const CODE_PAD: f32 = 8.0;

struct Run {
    text: String,
    spec: FontSpec,
    color: ColorToken,
}

struct Line {
    x: f32,
    y: f32,
    height: f32,
    runs: Vec<Run>,
}

struct Shape {
    rect: Rect,
    fill: Option<PaintColor>,
}

struct Layout {
    lines: Vec<Line>,
    shapes: Vec<Shape>,
    height: f32,
}

/// 排版缓存键：内容指纹 + 宽度位型 + 量算代次。
type CacheKey = (u64, u32, u64);

/// 跨帧排版缓存：同一段 Markdown 在滚动时按宽度反复重排是纯浪费——
/// 长 README 每帧重排整段会把滚动拖成个位数帧率。键含量算代次，真实字宽一落地即重排。
/// 用 `Mutex` 而非 `RefCell`：`FrameUi` 要能跨线程持有（插件在异步任务里驱动出帧）。
pub(crate) struct LayoutCache {
    entries: Mutex<Vec<(CacheKey, Arc<Layout>)>>,
}

/// 缓存条数上限：一段内容在不同宽度（含滚动条占位前后）各留几份即可。
const MAX_LAYOUTS: usize = 8;

impl Default for LayoutCache {
    fn default() -> Self {
        Self { entries: Mutex::new(Vec::new()) }
    }
}

impl LayoutCache {
    fn hit(&self, key: CacheKey) -> Option<Arc<Layout>> {
        let mut entries = self.entries.lock().unwrap();
        let index = entries.iter().position(|(entry, _)| *entry == key)?;
        let found = entries.remove(index);
        let layout = Arc::clone(&found.1);
        entries.insert(0, found);
        Some(layout)
    }

    fn store(&self, key: CacheKey, layout: Arc<Layout>) {
        let mut entries = self.entries.lock().unwrap();
        entries.insert(0, (key, layout));
        entries.truncate(MAX_LAYOUTS);
    }
}

/// 内容指纹：缓存键用（同一原文排版结果相同，宽度与量算代次另计入键）。
pub(crate) fn content_key(content: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in content.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash
}

/// 取（或重排并缓存）一段 Markdown 的版面。
fn laid_out(
    cache: &LayoutCache,
    metrics: &mut Metrics,
    theme: &Theme,
    content: u64,
    blocks: &[MarkdownBlock],
    width: f32,
) -> Arc<Layout> {
    // 空块 = 解析结果还没到：不入缓存，否则取到真块后会命中这份空版面
    if blocks.is_empty() {
        return Arc::new(layout(metrics, theme, blocks, width));
    }
    let key = (content, width.to_bits(), metrics.epoch());
    if let Some(hit) = cache.hit(key) {
        return hit;
    }
    let laid = Arc::new(layout(metrics, theme, blocks, width));
    cache.store(key, Arc::clone(&laid));
    laid
}

/// 排版结果的高度：给父容器定高用（内容随宽度变化，故按宽度重排）。
pub(crate) fn height(
    cache: &LayoutCache,
    metrics: &mut Metrics,
    theme: &Theme,
    content: u64,
    blocks: &[MarkdownBlock],
    width: f32,
) -> f32 {
    laid_out(cache, metrics, theme, content, blocks, width).height
}

/// 绘制一块 Markdown 区域；超出裁剪区的行不画。
pub(crate) fn paint(ctx: &mut BuildCtx<'_>, content: u64, blocks: &[MarkdownBlock], rect: Rect, clip: Rect) {
    let laid = laid_out(&ctx.state.markdown_layouts, &mut ctx.metrics, ctx.theme, content, blocks, rect.w);
    for shape in &laid.shapes {
        let placed = Rect::new(rect.x + shape.rect.x, rect.y + shape.rect.y, shape.rect.w, shape.rect.h);
        let visible = placed.intersect(clip);
        if visible.is_empty() {
            continue;
        }
        if let Some(fill) = shape.fill {
            ctx.draw(DrawCommand::Rect {
                x: visible.x,
                y: visible.y,
                w: visible.w,
                h: visible.h,
                fill: Some(fill),
                stroke: None,
                stroke_width: None,
                corner_radius: None,
            });
        }
    }
    for line in &laid.lines {
        let y = rect.y + line.y;
        if y + line.height < clip.y || y > clip.bottom() {
            continue;
        }
        let mut x = rect.x + line.x;
        for run in &line.runs {
            let width = ctx.metrics.width(&run.text, run.spec);
            text::paint(ctx, &run.text, run.spec, theme::token(run.color), x, y, None, None, clip);
            x += width;
        }
    }
}

fn layout(metrics: &mut Metrics, theme: &Theme, blocks: &[MarkdownBlock], width: f32) -> Layout {
    let mut out = Layout { lines: Vec::new(), shapes: Vec::new(), height: 0.0 };
    for (index, block) in blocks.iter().enumerate() {
        if index > 0 {
            out.height += BLOCK_GAP;
        }
        layout_block(metrics, theme, block, width, &mut out);
    }
    out
}

fn layout_block(metrics: &mut Metrics, theme: &Theme, block: &MarkdownBlock, width: f32, out: &mut Layout) {
    let sizes = &theme.sizes;
    match block {
        MarkdownBlock::Heading { level, depth, spans } => {
            let size = match level {
                1 => sizes.h1,
                2 => sizes.h2,
                _ => sizes.body * 1.1,
            };
            let spec = FontSpec { size, weight: Some(TextWeight::SemiBold), family: None };
            let indent = indent_of(*depth);
            let runs = runs_of(spans, spec, ColorToken::Text);
            wrap_from(metrics, runs, width - indent, indent, out);
        }
        MarkdownBlock::Paragraph { depth, spans } => {
            let spec = FontSpec { size: sizes.body, weight: None, family: None };
            let indent = indent_of(*depth);
            let runs = runs_of(spans, spec, ColorToken::Text);
            wrap_from(metrics, runs, width - indent, indent, out);
        }
        MarkdownBlock::ListItem { depth, ordered, index, spans } => {
            let spec = FontSpec { size: sizes.body, weight: None, family: None };
            let indent = indent_of(*depth);
            let marker = if *ordered { format!("{index}.") } else { "•".to_string() };
            let marker_width = metrics.width(&marker, spec).max(10.0) + 8.0;
            let start = out.height;
            let before = out.lines.len();
            let runs = runs_of(spans, spec, ColorToken::Text);
            wrap_from(metrics, runs, width - indent - marker_width, indent + marker_width, out);
            if out.lines.len() == before {
                // 空项也要占一行：标记仍要看得见
                out.height += line_height(metrics, spec);
            }
            out.lines.push(Line {
                x: indent,
                y: start,
                height: line_height(metrics, spec),
                runs: vec![Run { text: marker, spec, color: ColorToken::TextSecondary }],
            });
        }
        MarkdownBlock::Quote { depth, spans } => {
            let spec = FontSpec { size: sizes.body, weight: None, family: None };
            let indent = indent_of(*depth);
            let start = out.height;
            let runs = runs_of(spans, spec, ColorToken::TextSecondary);
            wrap_from(metrics, runs, width - indent - QUOTE_PAD, indent + QUOTE_PAD, out);
            out.shapes.push(Shape {
                rect: Rect::new(indent - QUOTE_PAD, start, QUOTE_BAR, (out.height - start).max(1.0)),
                fill: Some(theme::token(ColorToken::Border)),
            });
        }
        MarkdownBlock::Code { depth, content, .. } => {
            let spec = FontSpec { size: sizes.caption, weight: None, family: Some(TextFamily::Mono) };
            let indent = indent_of(*depth);
            let start = out.height;
            let inner = (width - indent - CODE_PAD * 2.0).max(1.0);
            let mut y = start + CODE_PAD;
            for source in content.lines() {
                let shown = text::elide(metrics, source, spec, inner);
                out.lines.push(Line {
                    x: indent + CODE_PAD,
                    y,
                    height: line_height(metrics, spec),
                    runs: vec![Run { text: shown, spec, color: ColorToken::Text }],
                });
                y += line_height(metrics, spec);
            }
            let block_height = (y + CODE_PAD - start).max(CODE_PAD * 2.0 + line_height(metrics, spec));
            out.shapes.push(Shape {
                rect: Rect::new(indent, start, width - indent, block_height),
                fill: Some(theme::token(ColorToken::Surface)),
            });
            out.height = start + block_height;
        }
        MarkdownBlock::Table { depth, rows } => {
            let spec = FontSpec { size: sizes.caption, weight: None, family: Some(TextFamily::Mono) };
            let indent = indent_of(*depth);
            let inner = (width - indent).max(1.0);
            for row in rows {
                let joined = row.join(" │ ");
                let shown = text::elide(metrics, &joined, spec, inner);
                out.lines.push(Line {
                    x: indent,
                    y: out.height,
                    height: line_height(metrics, spec),
                    runs: vec![Run { text: shown, spec, color: ColorToken::Text }],
                });
                out.height += line_height(metrics, spec);
            }
        }
        MarkdownBlock::Divider => {
            out.shapes.push(Shape {
                rect: Rect::new(0.0, out.height, width, 1.0),
                fill: Some(theme::token(ColorToken::Border)),
            });
            out.height += 1.0;
        }
    }
}

fn indent_of(depth: u32) -> f32 {
    depth.min(8) as f32 * DEPTH_INDENT
}

fn line_height(metrics: &mut Metrics, spec: FontSpec) -> f32 {
    metrics.measure("", spec, None).height.max(spec.size * LINE_FACTOR)
}

fn runs_of(spans: &[MarkdownSpan], base: FontSpec, color: ColorToken) -> Vec<Run> {
    spans
        .iter()
        .filter(|span| !span.text.is_empty())
        .map(|span| Run {
            text: span.text.clone(),
            spec: FontSpec {
                size: base.size,
                weight: if span.strong { Some(TextWeight::SemiBold) } else { base.weight },
                family: if span.code { Some(TextFamily::Mono) } else { base.family },
            },
            color: if span.link.is_some() { ColorToken::Accent } else { color },
        })
        .collect()
}

/// 把片段按宽度折成行；行内遇到 `\n` 直接断行（软换行在宿主解析产物里就是换行符）。
fn wrap_from(metrics: &mut Metrics, runs: Vec<Run>, width: f32, indent: f32, out: &mut Layout) {
    let available = width.max(1.0);
    let mut lines: Vec<Vec<Run>> = Vec::new();
    let mut current: Vec<Run> = Vec::new();
    let mut remaining = available;
    for run in runs {
        let mut rest: &str = &run.text;
        while !rest.is_empty() {
            if let Some(newline) = rest.find('\n') {
                push_text(metrics, &mut current, &rest[..newline], &run, &mut remaining, available, &mut lines);
                lines.push(std::mem::take(&mut current));
                remaining = available;
                rest = &rest[newline + 1..];
                continue;
            }
            push_text(metrics, &mut current, rest, &run, &mut remaining, available, &mut lines);
            rest = "";
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    for line in lines {
        let Some(first) = line.first() else { continue };
        let height = line_height(metrics, first.spec);
        out.lines.push(Line { x: indent, y: out.height, height, runs: line });
        out.height += height;
    }
}

/// 把一段文字尽量塞进当前行：放不下就按字符切分并在行尾断行。
fn push_text(
    metrics: &mut Metrics,
    current: &mut Vec<Run>,
    text: &str,
    run: &Run,
    remaining: &mut f32,
    available: f32,
    lines: &mut Vec<Vec<Run>>,
) {
    let mut rest = text;
    while !rest.is_empty() {
        let width = metrics.width(rest, run.spec);
        if width <= *remaining {
            current.push(Run { text: rest.to_string(), spec: run.spec, color: run.color });
            *remaining -= width;
            return;
        }
        let take = fit_prefix(metrics, rest, run.spec, remaining.max(0.0));
        if take == 0 {
            // 极窄的一行连一个字符都放不下：当前行没有内容就丢弃这段，避免死循环
            if current.is_empty() {
                return;
            }
            lines.push(std::mem::take(current));
            *remaining = available;
            continue;
        }
        current.push(Run { text: rest[..take].to_string(), spec: run.spec, color: run.color });
        lines.push(std::mem::take(current));
        *remaining = available;
        rest = &rest[take..];
    }
}

/// 在 `available` 宽度内能放下的最长前缀字节数（至少一个字符；放不下任何字符时为 0）。
fn fit_prefix(metrics: &mut Metrics, text: &str, spec: FontSpec, available: f32) -> usize {
    if available <= 0.0 {
        return 0;
    }
    let points: Vec<usize> = text.char_indices().map(|(index, _)| index).chain(std::iter::once(text.len())).collect();
    let mut low = 0usize;
    let mut high = points.len() - 1;
    while low < high {
        let mid = (low + high).div_ceil(2);
        if metrics.width(&text[..points[mid]], spec) <= available {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    points[low]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paragraph() -> Vec<MarkdownBlock> {
        vec![MarkdownBlock::Paragraph {
            depth: 0,
            spans: vec![MarkdownSpan { text: "一段正文".into(), strong: false, code: false, link: None }],
        }]
    }

    /// 滚动时每帧都要按宽度问一次高度：同内容同宽度必须复用已排好的版面。
    #[test]
    fn the_same_content_at_the_same_width_is_laid_out_once() {
        let cache = LayoutCache::default();
        let theme = Theme::default();
        let mut metrics = Metrics::default();
        let key = content_key("一段正文");
        let first = laid_out(&cache, &mut metrics, &theme, key, &paragraph(), 240.0);
        let again = laid_out(&cache, &mut metrics, &theme, key, &paragraph(), 240.0);
        assert!(Arc::ptr_eq(&first, &again), "同内容同宽度应命中缓存");
        let narrower = laid_out(&cache, &mut metrics, &theme, key, &paragraph(), 120.0);
        assert!(!Arc::ptr_eq(&first, &narrower), "宽度变了要按新宽度重排");
    }

    #[test]
    fn the_cache_keeps_only_the_recent_layouts() {
        let cache = LayoutCache::default();
        let mut metrics = Metrics::default();
        for index in 0..(MAX_LAYOUTS as u64 * 3) {
            let laid = Arc::new(layout(&mut metrics, &Theme::default(), &[], 100.0));
            cache.store((index, 0, 0), laid);
        }
        assert_eq!(cache.entries.lock().unwrap().len(), MAX_LAYOUTS);
    }
}
