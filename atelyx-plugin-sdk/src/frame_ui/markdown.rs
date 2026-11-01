//! 只读 Markdown 的排版与绘制：解析走领域库 `atelyx-markdown`（与宿主阅读面同一份语义、同一套源偏移），
//! 版面口径与宿主阅读面对齐（行高 1.625、标题按级放大、块间空行折算、引用竖条、代码块头、表格网格）。
//!
//! 排版结果按「内容 + 宽度 + 量算代次」缓存，滚动与悬停都不重排。

use std::sync::{Arc, Mutex};

use atelyx_markdown::{
    parse_inline, parse_markdown, InlineSpan, MarkdownBlock, MarkdownListItem, MentionCandidate,
    ParseOptions,
};
use atelyx_plugin_protocol::paint::{ColorToken, DrawCommand, PaintColor, PaintPoint, TextFamily, TextWeight};

use super::build::BuildCtx;
use super::geom::Rect;
use super::metrics::Metrics;
use super::model::FontSpec;
use super::text;
use super::theme::{self, Theme};

/// 散文行高倍数（与宿主阅读面同一口径）。
const PROSE_LINE: f32 = 1.625;
/// 标题行高倍数（标题字号已放大，行距按 1.3 收）。
const HEADING_LINE: f32 = 1.3;
/// 代码块行高倍数。
const CODE_LINE: f32 = 1.4;
/// 代码块最多展示行数（超出块内纵向滚动由插件自己决定，这里只按此定高）。
const CODE_MAX_LINES: usize = 18;
/// 引用竖条宽度与文字左移量。
const QUOTE_BAR: f32 = 3.0;
const QUOTE_PAD: f32 = 12.0;
/// 列表标记列最小宽度（普通项按源标记实测，任务项的勾选框占此列）。
const LIST_MARKER_W: f32 = 20.0;
/// 列表标记与内容的间距。
const LIST_GAP: f32 = 6.0;
/// 代码块头 / 正文内边距、外框圆角与边框宽度。
const CODE_HEAD_PAD_Y: f32 = 3.0;
const CODE_HEAD_PAD_X: f32 = 6.0;
const CODE_BODY_PAD: f32 = 8.0;
const CODE_RADIUS: f32 = 6.0;
const CODE_BORDER: f32 = 1.0;
/// 代码块头字号（与宿主同一档）与「复制」按钮的内边距 / 圆角。
const CODE_HEAD_SIZE: f32 = 11.0;
const CODE_BTN_PAD_X: f32 = 6.0;
const CODE_BTN_PAD_Y: f32 = 2.0;
/// 表格单元格内边距、分隔线与外边线宽。
const CELL_PAD_X: f32 = 8.0;
const CELL_PAD_Y: f32 = 4.0;
const TABLE_LINE: f32 = 1.0;
/// 任务框边长、圆角与勾线粗细。
const TASK_BOX: f32 = 12.0;
const TASK_RADIUS: f32 = 3.0;
const TASK_STROKE: f32 = 1.6;
/// 脚注角标与行内图片替代文字的字号倍数。
const REF_SCALE: f32 = 0.85;

/// 一个行内片段的绘制样式，逐片段随文本走。
#[derive(Clone, Copy, Debug, PartialEq)]
struct RunStyle {
    color: ColorToken,
    italic: bool,
    strike: bool,
    /// 引用标签这类整段实体：浅强调底 + 强调色文字。
    capsule: bool,
}

struct Run {
    text: String,
    spec: FontSpec,
    style: RunStyle,
}

/// 一行文字：`y` 相对块原点，`x` 已含缩进。
struct Line {
    x: f32,
    y: f32,
    height: f32,
    runs: Vec<Run>,
}

/// 矩形图元（底色 / 边框 / 圆角）。绘制顺序在文字之下。
struct Shape {
    rect: Rect,
    fill: Option<PaintColor>,
    stroke: Option<(PaintColor, f32)>,
    radius: Option<f32>,
}

/// 描边图元（任务框的勾线）：绘制顺序在矩形之上、文字之下。
struct Stroke {
    points: Vec<PaintPoint>,
    color: PaintColor,
    width: f32,
}

pub(crate) struct Layout {
    lines: Vec<Line>,
    shapes: Vec<Shape>,
    strokes: Vec<Stroke>,
    /// 代码块头「复制」按钮的落位与内容：命中按 `id::copy:<序>` 登记，按下即写剪贴板。
    copies: Vec<CopyHit>,
    height: f32,
}

struct CopyHit {
    rect: Rect,
    text: String,
}

/// 排版缓存键：内容指纹 + 宽度位型 + 量算代次。
type CacheKey = (u64, u32, u64);

/// 跨帧排版缓存：同一段 Markdown 在滚动时按宽度反复重排是纯浪费。
pub(crate) struct LayoutCache {
    entries: Mutex<Vec<(CacheKey, Arc<Layout>)>>,
}

/// 缓存条数上限：一段内容在几个宽度（滚动条占位前后 / 面板改宽）下各留一份即可。
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

/// 内容指纹：缓存键用（引用候选随会话变化，一并计入）。
pub(crate) fn content_key(content: &str, mentions: &[MentionCandidate]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in content.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    for mention in mentions {
        for byte in mention.key.as_bytes().iter().chain(mention.label.as_bytes()) {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    }
    hash
}

/// 取（或重排并缓存）一段 Markdown 的版面。
fn laid_out(
    cache: &LayoutCache,
    metrics: &mut Metrics,
    theme: &Theme,
    key: u64,
    content: &str,
    mentions: &[MentionCandidate],
    width: f32,
) -> Arc<Layout> {
    let full = (key, width.to_bits(), metrics.epoch());
    if let Some(hit) = cache.hit(full) {
        return hit;
    }
    let laid = Arc::new(layout(metrics, theme, content, mentions, width));
    cache.store(full, Arc::clone(&laid));
    laid
}

/// 一段 Markdown 排版后的高度（父容器定高用）。
pub(crate) fn height(
    cache: &LayoutCache,
    metrics: &mut Metrics,
    theme: &Theme,
    key: u64,
    content: &str,
    mentions: &[MentionCandidate],
    width: f32,
) -> f32 {
    laid_out(cache, metrics, theme, key, content, mentions, width).height
}

/// 绘制一块 Markdown 区域：图元在文字之下，超出裁剪区的不画。
pub(crate) fn paint(
    ctx: &mut BuildCtx<'_>,
    id: &str,
    key: u64,
    content: &str,
    mentions: &[MentionCandidate],
    rect: Rect,
    clip: Rect,
) {
    let laid = laid_out(&ctx.state.markdown_layouts, &mut ctx.metrics, ctx.theme, key, content, mentions, rect.w);
    ctx.markdown_seen.push((id.to_string(), Arc::clone(&laid)));
    for shape in &laid.shapes {
        let placed = Rect::new(rect.x + shape.rect.x, rect.y + shape.rect.y, shape.rect.w, shape.rect.h);
        let visible = placed.intersect(clip);
        if visible.is_empty() {
            continue;
        }
        ctx.draw(DrawCommand::Rect {
            x: visible.x,
            y: visible.y,
            w: visible.w,
            h: visible.h,
            fill: shape.fill,
            stroke: shape.stroke.map(|(color, _)| color),
            stroke_width: shape.stroke.map(|(_, width)| width),
            corner_radius: shape.radius,
            shadow: None,
        });
    }
    for stroke in &laid.strokes {
        let points: Vec<PaintPoint> =
            stroke.points.iter().map(|point| PaintPoint { x: rect.x + point.x, y: rect.y + point.y }).collect();
        ctx.draw(DrawCommand::Path {
            points,
            closed: false,
            fill: None,
            stroke: Some(stroke.color),
            stroke_width: Some(stroke.width),
        });
    }
    // 选区高亮先画：只画本块自己的选择
    let selection = ctx
        .state
        .selection
        .as_ref()
        .filter(|selection| selection.id == id)
        .map(|selection| selection.range());
    if let Some((from, to)) = selection {
        for (index, line) in laid.lines.iter().enumerate() {
            if index < from.0 || index > to.0 {
                continue;
            }
            let text = line_text(line);
            let spec = line.runs.first().map(|run| run.spec).unwrap_or(FontSpec { size: 13.0, weight: None, family: None });
            let start = if index == from.0 { from.1.min(text.len()) } else { 0 };
            let end = if index == to.0 { to.1.min(text.len()) } else { text.len() };
            let start = text.floor_char_boundary(start);
            let end = text.floor_char_boundary(end);
            let low = text::offset_in_line(&mut ctx.metrics, &text, spec, start);
            let high = if end > start {
                text::offset_in_line(&mut ctx.metrics, &text, spec, end)
            } else {
                low
            };
            let band = Rect::new(
                rect.x + line.x + low,
                rect.y + line.y,
                (high - low).max(1.0),
                line.height,
            );
            let visible = band.intersect(clip);
            if visible.is_empty() {
                continue;
            }
            ctx.draw(DrawCommand::Rect {
                x: visible.x,
                y: visible.y,
                w: visible.w,
                h: visible.h,
                fill: Some(ctx.theme.selection()),
                stroke: None,
                stroke_width: None,
                corner_radius: None,
                shadow: None,
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
            let width = text::advance(&mut ctx.metrics, &run.text, run.spec);
            if run.style.capsule {
                let band = Rect::new(x - 1.0, y + line.height * 0.08, width + 2.0, line.height * 0.84);
                let visible = band.intersect(clip);
                if !visible.is_empty() {
                    ctx.draw(DrawCommand::Rect {
                        x: visible.x,
                        y: visible.y,
                        w: visible.w,
                        h: visible.h,
                        fill: Some(theme::token(ColorToken::AccentSoft)),
                        stroke: None,
                        stroke_width: None,
                        corner_radius: Some(3.0),
                        shadow: None,
                    });
                }
            }
            if run.style.strike {
                let band = Rect::new(x, y + line.height * 0.5, width, 1.0);
                let visible = band.intersect(clip);
                if !visible.is_empty() {
                    ctx.draw(DrawCommand::Rect {
                        x: visible.x,
                        y: visible.y,
                        w: visible.w,
                        h: visible.h,
                        fill: Some(theme::token(ColorToken::TextSecondary)),
                        stroke: None,
                        stroke_width: None,
                        corner_radius: None,
                        shadow: None,
                    });
                }
            }
            ctx.draw(DrawCommand::Text {
                x,
                y,
                content: run.text.clone(),
                size: run.spec.size,
                color: theme::token(run.style.color),
                weight: run.spec.weight,
                family: run.spec.family,
                italic: run.style.italic,
                max_width: None,
                align: None,
            });
            x += width;
        }
    }
    for (index, copy) in laid.copies.iter().enumerate() {
        let placed = Rect::new(rect.x + copy.rect.x, rect.y + copy.rect.y, copy.rect.w, copy.rect.h);
        let visible = placed.intersect(clip);
        if visible.is_empty() {
            continue;
        }
        let hit = copy_hit_id(id, index);
        ctx.hit(&hit, visible, atelyx_plugin_protocol::frame::HitCursor::Pointer);
        ctx.record(&hit, super::model::Layout::Copy { text: copy.text.clone() });
    }
}

/// 落点 → （行序，行内字节位）：先按纵向定行，再按横向归到最近的一列（表格同行多列）。
pub(crate) fn hit(layout: &Layout, metrics: &mut Metrics, x: f32, y: f32) -> Option<(usize, usize)> {
    if layout.lines.is_empty() {
        return None;
    }
    let mut best: Option<(usize, f32)> = None;
    for (index, line) in layout.lines.iter().enumerate() {
        if y < line.y || y >= line.y + line.height {
            continue;
        }
        let text = line_text(line);
        let spec = line.runs.first().map(|run| run.spec).unwrap_or(FontSpec { size: 13.0, weight: None, family: None });
        let width = text::advance(metrics, &text, spec);
        let gap = if x < line.x {
            line.x - x
        } else if x > line.x + width {
            x - (line.x + width)
        } else {
            0.0
        };
        if best.is_none_or(|(_, nearest)| gap < nearest) {
            best = Some((index, gap));
        }
    }
    let index = best.map(|(index, _)| index).unwrap_or(layout.lines.len() - 1);
    let line = &layout.lines[index];
    let text = line_text(line);
    let spec = line.runs.first().map(|run| run.spec).unwrap_or(FontSpec { size: 13.0, weight: None, family: None });
    let offset = text::caret_at_x(metrics, &text, spec, (x - line.x).max(0.0));
    Some((index, offset))
}

/// 一行文字的纯文本（各片段拼接，不加标记）。
fn line_text(line: &Line) -> String {
    line.runs.iter().map(|run| run.text.as_str()).collect()
}

/// 选择区间的文字：跨行时按换行拼接，行内按字节切。
pub(crate) fn selected_text(layout: &Layout, range: ((usize, usize), (usize, usize))) -> String {
    let ((from_line, from_byte), (to_line, to_byte)) = range;
    let mut out = String::new();
    for (index, line) in layout.lines.iter().enumerate() {
        if index < from_line || index > to_line {
            continue;
        }
        let text = line_text(line);
        let start = if index == from_line { from_byte.min(text.len()) } else { 0 };
        let end = if index == to_line { to_byte.min(text.len()) } else { text.len() };
        if end <= start {
            if index != to_line {
                out.push('\n');
            }
            continue;
        }
        let start = text.floor_char_boundary(start);
        let end = text.floor_char_boundary(end);
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&text[start..end]);
    }
    out
}

/// 代码块「复制」按钮的命中 id：`<块 id>::copy:<序>`。
fn copy_hit_id(id: &str, index: usize) -> String {
    format!("{id}::copy:{index}")
}

/// 排版器：随块递归累积行与图元，坐标都相对块原点。
struct Builder<'a> {
    metrics: &'a mut Metrics,
    theme: &'a Theme,
    source: &'a str,
    options: ParseOptions,
    lines: Vec<Line>,
    shapes: Vec<Shape>,
    strokes: Vec<Stroke>,
    copies: Vec<CopyHit>,
    height: f32,
}

fn layout(
    metrics: &mut Metrics,
    theme: &Theme,
    source: &str,
    mentions: &[MentionCandidate],
    width: f32,
) -> Layout {
    let options = ParseOptions { mentions: mentions.to_vec(), ..ParseOptions::default() };
    let document = parse_markdown(source, &options);
    let mut builder = Builder {
        metrics,
        theme,
        source,
        options,
        lines: Vec::new(),
        shapes: Vec::new(),
        strokes: Vec::new(),
        copies: Vec::new(),
        height: 0.0,
    };
    let mut previous = 0usize;
    builder.blocks(&document.blocks, width.max(1.0), 0, &mut previous);
    Layout {
        lines: builder.lines,
        shapes: builder.shapes,
        strokes: builder.strokes,
        copies: builder.copies,
        height: builder.height,
    }
}

impl Builder<'_> {
    fn base_size(&self) -> f32 {
        self.theme.sizes.body
    }

    fn prose_height(&self, size: f32) -> f32 {
        (size * PROSE_LINE).max(size)
    }

    fn line_height(&mut self, spec: FontSpec, factor: f32) -> f32 {
        self.metrics.measure("", spec, None).height.max(spec.size * factor)
    }

    /// 块间空行：按源码里两块之间的空行数折算（与宿主同一口径）。
    fn gap_before(&mut self, previous: usize, from: usize, line: f32) {
        let gap = blank_lines(self.source, previous, from);
        if gap > 0 {
            self.height += gap as f32 * line;
        }
    }

    fn blocks(&mut self, blocks: &[MarkdownBlock], width: f32, depth: u32, previous: &mut usize) {
        for block in blocks {
            let (from, to) = block_range(block);
            self.gap_before(*previous, from, self.prose_height(self.base_size()));
            self.block(block, width, depth, previous, to);
            *previous = to;
        }
    }

    fn block(&mut self, block: &MarkdownBlock, width: f32, depth: u32, previous: &mut usize, to: usize) {
        let indent = depth as f32 * LIST_MARKER_W;
        let inner = (width - indent).max(1.0);
        match block {
            MarkdownBlock::Heading { level, inline, .. } => {
                let size = self.heading_size(*level);
                let spec = FontSpec { size, weight: Some(TextWeight::SemiBold), family: None };
                let runs = self.runs(inline, spec, ColorToken::Text);
                let line = (size * HEADING_LINE).max(size);
                self.flow(runs, indent, inner, line);
            }
            MarkdownBlock::Paragraph { inline, .. } => {
                if let Some(image) = image_only(inline) {
                    self.image_block(image, indent, inner, depth);
                } else {
                    let spec = FontSpec { size: self.base_size(), weight: None, family: None };
                    let runs = self.runs(inline, spec, ColorToken::Text);
                    let line = self.line_height(spec, PROSE_LINE);
                    self.flow(runs, indent, inner, line);
                }
            }
            MarkdownBlock::Blockquote { callout, children, .. } => {
                let start = self.height;
                let mut child_previous = *previous;
                let pad = QUOTE_PAD;
                if let Some(name) = callout {
                    let spec = FontSpec { size: self.theme.sizes.micro, weight: Some(TextWeight::SemiBold), family: None };
                    let runs = vec![Run {
                        text: name.to_string(),
                        spec,
                        style: RunStyle { color: ColorToken::Accent, italic: false, strike: false, capsule: true },
                    }];
                    let line = self.line_height(spec, PROSE_LINE);
                    self.flow(runs, indent + pad, inner - pad, line);
                }
                self.blocks(children, width - pad, depth + 1, &mut child_previous);
                let height = (self.height - start).max(1.0);
                self.shapes.push(Shape {
                    rect: Rect::new(indent, start, QUOTE_BAR, height),
                    fill: Some(theme::token(ColorToken::Border)),
                    stroke: None,
                    radius: None,
                });
            }
            MarkdownBlock::List { ordered, items, .. } => {
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        self.gap_before(items[index - 1].to, item.from, self.prose_height(self.base_size()));
                    }
                    self.list_item(item, *ordered, index, width, depth, previous);
                }
            }
            MarkdownBlock::FencedCode { lang, code, .. } => self.code_block(lang, code, indent, inner, depth),
            MarkdownBlock::IndentedCode { code, .. } => self.code_block("", code, indent, inner, depth),
            MarkdownBlock::Hr { .. } => {
                self.height += GAP_AROUND_HR;
                self.shapes.push(Shape {
                    rect: Rect::new(indent, self.height, inner, 1.0),
                    fill: Some(theme::token(ColorToken::Border)),
                    stroke: None,
                    radius: None,
                });
                self.height += 1.0 + GAP_AROUND_HR;
            }
            MarkdownBlock::Table { header, aligns, rows, .. } => {
                self.table(header, aligns, rows, indent, inner)
            }
            MarkdownBlock::MathBlock { tex, .. } => {
                let fenced = format!("$$\n{tex}\n$$");
                self.code_block("math", &fenced, indent, inner, depth)
            }
            MarkdownBlock::HtmlBlock { html, .. } => self.source_block(html, indent, inner),
            MarkdownBlock::FootnoteDef { label, inline, .. } => {
                let spec = FontSpec { size: self.base_size(), weight: None, family: None };
                let mut runs = vec![Run {
                    text: format!("[{label}] "),
                    spec,
                    style: RunStyle { color: ColorToken::Accent, italic: false, strike: false, capsule: false },
                }];
                runs.extend(self.runs(inline, spec, ColorToken::TextSecondary));
                let line = self.line_height(spec, PROSE_LINE);
                self.flow(runs, indent, inner, line);
            }
        }
        let _ = to;
    }

    fn heading_size(&self, level: u8) -> f32 {
        self.base_size()
            * match level {
                1 => 1.5,
                2 => 1.35,
                3 => 1.2,
                4 => 1.1,
                5 => 1.0,
                _ => 0.9,
            }
    }

    fn list_item(
        &mut self,
        item: &MarkdownListItem,
        ordered: bool,
        index: usize,
        width: f32,
        depth: u32,
        previous: &mut usize,
    ) {
        let indent = depth as f32 * LIST_MARKER_W;
        let spec = FontSpec { size: self.base_size(), weight: None, family: None };
        let marker_w = self.marker_width(item, ordered, index, spec);
        let (content_from, content_to) = item_content_range(self.source, item);
        let raw = &self.source[content_from.min(content_to)..content_to];
        // 引用块里的项带着 `>` 前缀：先去前缀再解析行内（这类项的源偏移只用于绘制）
        let inline = if raw.lines().any(|line| line.trim_start().starts_with('>')) {
            let cleaned = strip_quote_marks(raw);
            parse_inline(&cleaned, 0, cleaned.len(), &self.options)
        } else {
            parse_inline(self.source, content_from, content_to, &self.options)
        };
        let runs = self.runs(&inline, spec, ColorToken::Text);
        let start = self.height;
        let line = self.line_height(spec, PROSE_LINE);
        if item.task {
            self.task_box(indent, start, line, item.checked);
        } else {
            let marker = if ordered { format!("{}.", index + 1) } else { "•".to_string() };
            self.lines.push(Line {
                x: indent,
                y: start,
                height: line,
                runs: vec![Run {
                    text: marker,
                    spec,
                    style: RunStyle { color: ColorToken::TextSecondary, italic: false, strike: false, capsule: false },
                }],
            });
        }
        self.flow(runs, indent + marker_w, (width - indent - marker_w).max(1.0), line);
        if self.height <= start {
            self.height = start + line;
        }
        let mut child_previous = content_from;
        self.blocks(&item.children, width, depth + 1, &mut child_previous);
        *previous = item.to;
    }

    /// 标记列宽：普通项按源标记实测（不小于最小列宽），任务项固定占一列。
    fn marker_width(&mut self, item: &MarkdownListItem, ordered: bool, index: usize, spec: FontSpec) -> f32 {
        if item.task {
            return LIST_MARKER_W + LIST_GAP;
        }
        let marker = if ordered { format!("{}.", index + 1) } else { "•".to_string() };
        self.metrics.width(&marker, spec).max(LIST_MARKER_W - LIST_GAP) + LIST_GAP
    }

    /// 任务框：勾选态填强调色并画勾线，未勾选只描边。
    fn task_box(&mut self, x: f32, y: f32, line: f32, checked: bool) {
        let top = y + (line - TASK_BOX) / 2.0;
        let fill = checked.then(|| theme::token(ColorToken::Accent));
        self.shapes.push(Shape {
            rect: Rect::new(x, top, TASK_BOX, TASK_BOX),
            fill,
            stroke: (!checked).then(|| (theme::token(ColorToken::Border), 1.0)),
            radius: Some(TASK_RADIUS),
        });
        if checked {
            let color = theme::token(ColorToken::Surface);
            self.strokes.push(Stroke {
                points: vec![
                    PaintPoint { x: x + 2.6, y: top + 6.2 },
                    PaintPoint { x: x + 5.0, y: top + 8.6 },
                    PaintPoint { x: x + 9.4, y: top + 3.4 },
                ],
                color,
                width: TASK_STROKE,
            });
        }
    }

    fn code_block(&mut self, lang: &str, code: &str, indent: f32, width: f32, depth: u32) {
        let mono = FontSpec {
            size: self.theme.sizes.caption,
            weight: None,
            family: Some(TextFamily::Mono),
        };
        let head = self.code_head(lang, code, indent, width);
        let lines = code.trim_end_matches('\n').lines().count().max(1).min(CODE_MAX_LINES);
        let body_h = lines as f32 * (mono.size * CODE_LINE).max(mono.size) + CODE_BODY_PAD * 2.0;
        let total = head + body_h;
        let start = self.height;
        self.shapes.push(Shape {
            rect: Rect::new(indent, start, width, head + body_h),
            fill: Some(theme::token(ColorToken::Surface)),
            stroke: Some((theme::token(ColorToken::Border), CODE_BORDER)),
            radius: Some(CODE_RADIUS),
        });
        // 头与正文之间的分隔线
        self.shapes.push(Shape {
            rect: Rect::new(indent, start + head, width, 1.0),
            fill: Some(theme::token(ColorToken::Border)),
            stroke: None,
            radius: None,
        });
        let line = (mono.size * CODE_LINE).max(mono.size);
        let mut y = start + head + CODE_BODY_PAD;
        for source in code.trim_end_matches('\n').lines().take(CODE_MAX_LINES) {
            let shown = text::elide(self.metrics, source, mono, (width - CODE_BODY_PAD * 2.0).max(1.0));
            self.lines.push(Line {
                x: indent + CODE_BODY_PAD,
                y,
                height: line,
                runs: vec![Run {
                    text: shown,
                    spec: mono,
                    style: RunStyle { color: ColorToken::Text, italic: false, strike: false, capsule: false },
                }],
            });
            y += line;
        }
        self.height = start + total;
        let _ = depth;
    }

    /// 代码块头：语言标签 + 右对齐「复制」。返回头高。
    fn code_head(&mut self, lang: &str, code: &str, indent: f32, width: f32) -> f32 {
        let spec = FontSpec { size: CODE_HEAD_SIZE, weight: None, family: Some(TextFamily::Mono) };
        let line = self.line_height(spec, CODE_LINE);
        let head_h = line + CODE_HEAD_PAD_Y * 2.0;
        let label = if lang.trim().is_empty() { "代码".to_string() } else { lang.to_string() };
        let start = self.height;
        self.lines.push(Line {
            x: indent + CODE_HEAD_PAD_X,
            y: start + CODE_HEAD_PAD_Y,
            height: line,
            runs: vec![Run {
                text: label,
                spec,
                style: RunStyle { color: ColorToken::TextMuted, italic: false, strike: false, capsule: false },
            }],
        });
        let button = FontSpec { size: CODE_HEAD_SIZE, weight: None, family: None };
        let text_w = self.metrics.width("复制", button);
        let btn_w = text_w + CODE_BTN_PAD_X * 2.0;
        let btn_h = line + CODE_BTN_PAD_Y * 2.0;
        let btn_x = indent + width - CODE_HEAD_PAD_X - btn_w;
        self.lines.push(Line {
            x: btn_x + CODE_BTN_PAD_X,
            y: start + CODE_HEAD_PAD_Y,
            height: line,
            runs: vec![Run {
                text: "复制".to_string(),
                spec: button,
                style: RunStyle { color: ColorToken::TextSecondary, italic: false, strike: false, capsule: false },
            }],
        });
        self.copies.push(CopyHit {
            rect: Rect::new(btn_x, start + CODE_HEAD_PAD_Y - CODE_BTN_PAD_Y, btn_w, btn_h),
            text: code.to_string(),
        });
        head_h
    }

    /// 公式 / 原始 HTML 的降级形态：等宽小字放进浅底框（与宿主同一形态）。
    fn source_block(&mut self, content: &str, indent: f32, width: f32) {
        let mono = FontSpec { size: self.theme.sizes.caption, weight: None, family: Some(TextFamily::Mono) };
        let line = (mono.size * CODE_LINE).max(mono.size);
        let start = self.height;
        let mut y = start + CODE_BODY_PAD;
        for source in content.lines() {
            let shown = text::elide(self.metrics, source, mono, (width - CODE_BODY_PAD * 2.0).max(1.0));
            self.lines.push(Line {
                x: indent + CODE_BODY_PAD,
                y,
                height: line,
                runs: vec![Run {
                    text: shown,
                    spec: mono,
                    style: RunStyle { color: ColorToken::TextMuted, italic: false, strike: false, capsule: false },
                }],
            });
            y += line;
        }
        let body_h = (y - start).max(line) + CODE_BODY_PAD;
        self.shapes.push(Shape {
            rect: Rect::new(indent, start, width, body_h),
            fill: Some(theme::token(ColorToken::Surface)),
            stroke: Some((theme::token(ColorToken::Border), CODE_BORDER)),
            radius: Some(4.0),
        });
        self.height = start + body_h;
    }

    /// 表格：等宽列 + 1px 网格 + 表头浅底（与宿主 table 部件同一形态）。
    fn table(&mut self, header: &[atelyx_markdown::MarkdownTableCell], aligns: &[String], rows: &[Vec<atelyx_markdown::MarkdownTableCell>], indent: f32, width: f32) {
        let cols = header.len().max(rows.iter().map(Vec::len).max().unwrap_or(0)).max(1);
        let spec = FontSpec { size: self.base_size(), weight: None, family: None };
        let line = self.line_height(spec, PROSE_LINE);
        let cell_w = width / cols as f32;
        let start = self.height;
        let mut y = start;
        let draw_row = |builder: &mut Self, y: f32, cells: &[atelyx_markdown::MarkdownTableCell], head: bool| {
            let row_h = line + CELL_PAD_Y * 2.0;
            for column in 0..cols {
                let x = indent + column as f32 * cell_w;
                builder.shapes.push(Shape {
                    rect: Rect::new(x, y, cell_w, row_h),
                    fill: head.then(|| theme::token(ColorToken::Surface)),
                    stroke: Some((theme::token(ColorToken::Border), TABLE_LINE)),
                    radius: None,
                });
                let Some(cell) = cells.get(column) else { continue };
                if cell.to <= cell.from {
                    continue;
                }
                let inline = parse_inline(builder.source, cell.from, cell.to, &builder.options);
                let mut runs = builder.runs(&inline, spec, ColorToken::Text);
                if head {
                    for run in &mut runs {
                        run.spec.weight = Some(TextWeight::SemiBold);
                    }
                }
                let runs = align_runs(runs, aligns.get(column).map(String::as_str), cell_w - CELL_PAD_X * 2.0, builder.metrics, spec);
                builder.lines.push(Line { x: x + CELL_PAD_X, y: y + CELL_PAD_Y, height: line, runs });
            }
            y + row_h
        };
        y = draw_row(self, y, header, true);
        for row in rows {
            y = draw_row(self, y, row, false);
        }
        self.height = y;
    }

    /// 行内片段 → 带样式的绘制片段（强调 / 斜体 / 删除线 / 等宽 / 链接 / 标签胶囊 / 图片替代文字）。
    fn runs(&self, inline: &[InlineSpan], base: FontSpec, color: ColorToken) -> Vec<Run> {
        let mut out = Vec::new();
        self.push_spans(inline, base, RunStyle { color, italic: false, strike: false, capsule: false }, &mut out);
        out
    }

    fn push_spans(&self, inline: &[InlineSpan], base: FontSpec, style: RunStyle, out: &mut Vec<Run>) {
        for span in inline {
            match span {
                InlineSpan::Text { text, .. } => push_run(out, text, base, style),
                InlineSpan::Strong { children, .. } | InlineSpan::Highlight { children, .. } => {
                    let spec = FontSpec { weight: Some(TextWeight::SemiBold), ..base };
                    self.push_spans(children, spec, style, out);
                }
                InlineSpan::Em { children, .. } => {
                    self.push_spans(children, base, RunStyle { italic: true, ..style }, out);
                }
                InlineSpan::Strike { children, .. } => {
                    self.push_spans(children, base, RunStyle { strike: true, ..style }, out);
                }
                InlineSpan::Code { text, .. } | InlineSpan::MathInline { tex: text, .. } => {
                    let spec = FontSpec { family: Some(TextFamily::Mono), ..base };
                    push_run(out, text, spec, style);
                }
                InlineSpan::Link { label, .. } | InlineSpan::Autolink { label, .. } | InlineSpan::Wiki { label, .. } => {
                    let link = RunStyle { color: ColorToken::Accent, ..style };
                    push_run(out, label, base, link);
                }
                InlineSpan::Image { src, alt, .. } => {
                    let name = if alt.is_empty() { src.as_str() } else { alt.as_str() };
                    let spec = FontSpec { size: base.size * REF_SCALE, ..base };
                    let muted = RunStyle { color: ColorToken::TextMuted, italic: true, ..style };
                    push_run(out, &format!("[图片：{name}]"), spec, muted);
                }
                InlineSpan::Tag { tag, .. } => {
                    let capsule = RunStyle { color: ColorToken::Accent, capsule: true, ..style };
                    push_run(out, &format!("#{tag}"), base, capsule);
                }
                InlineSpan::Mention { label, trigger, .. } => {
                    let capsule = RunStyle { color: ColorToken::Accent, capsule: true, ..style };
                    push_run(out, &format!("{trigger}{label}"), base, capsule);
                }
                InlineSpan::FootnoteRef { label, .. } => {
                    let spec = FontSpec { size: base.size * REF_SCALE, ..base };
                    let accent = RunStyle { color: ColorToken::Accent, ..style };
                    push_run(out, &format!("[{label}]"), spec, accent);
                }
                InlineSpan::Html { html, .. } => {
                    let spec = FontSpec { family: Some(TextFamily::Mono), ..base };
                    let muted = RunStyle { color: ColorToken::TextMuted, ..style };
                    push_run(out, html, spec, muted);
                }
                InlineSpan::HardBreak { .. } => push_run(out, "\n", base, style),
                InlineSpan::Comment { .. } => {}
            }
        }
    }

    /// 把一个行内序列按宽度折行并落到行表：`x` 为块内左缘，`available` 为可用宽。
    fn flow(&mut self, runs: Vec<Run>, x: f32, available: f32, line_height: f32) {
        let available = available.max(1.0);
        let mut current: Vec<Run> = Vec::new();
        let mut remaining = available;
        for run in runs {
            let mut rest: &str = &run.text;
            while !rest.is_empty() {
                if let Some(newline) = rest.find('\n') {
                    self.push_piece(&mut current, &mut remaining, &rest[..newline], &run, available, x, line_height);
                    self.flush(&mut current, x, line_height);
                    remaining = available;
                    rest = &rest[newline + 1..];
                    continue;
                }
                self.push_piece(&mut current, &mut remaining, rest, &run, available, x, line_height);
                rest = "";
            }
        }
        if !current.is_empty() {
            self.flush(&mut current, x, line_height);
        }
    }

    fn push_piece(
        &mut self,
        current: &mut Vec<Run>,
        remaining: &mut f32,
        text: &str,
        run: &Run,
        available: f32,
        x: f32,
        line_height: f32,
    ) {
        let mut rest = text;
        while !rest.is_empty() {
            let (take, width) = fit_prefix(self.metrics, rest, run.spec, remaining.max(0.0));
            if take == rest.len() {
                if !rest.is_empty() {
                    current.push(Run { text: rest.to_string(), spec: run.spec, style: run.style });
                    *remaining = (*remaining - width).max(0.0);
                }
                return;
            }
            if take == 0 {
                // 极窄一行连一个字都放不下：当前行已有内容就换行，空行则丢弃这段避免死循环
                if current.is_empty() {
                    return;
                }
                self.flush(current, x, line_height);
                *remaining = available;
                continue;
            }
            current.push(Run { text: rest[..take].to_string(), spec: run.spec, style: run.style });
            self.flush(current, x, line_height);
            *remaining = available;
            rest = &rest[take..];
        }
    }

    fn flush(&mut self, current: &mut Vec<Run>, x: f32, line_height: f32) {
        if current.is_empty() {
            return;
        }
        let runs = std::mem::take(current);
        self.lines.push(Line { x, y: self.height, height: line_height, runs });
        self.height += line_height;
    }

    /// 段落里独占一行的图片：按可用宽等比缩放，最大不超过原图与可用宽。
    fn image_block(&mut self, image: &InlineSpan, indent: f32, width: f32, depth: u32) {
        let InlineSpan::Image { src, alt, width: spec_w, height: spec_h, .. } = image else {
            return;
        };
        let max_h = width;
        let height = parse_len(spec_h.as_deref()).unwrap_or(0.0);
        let height = if height > 0.0 { height.min(max_h) } else { 0.0 };
        if height <= 0.0 {
            // 未声明高度：按替代文字占一行（与行内图片同一形态）
            let spec = FontSpec { size: self.base_size() * REF_SCALE, weight: None, family: None };
            let name = if alt.is_empty() { src.as_str() } else { alt.as_str() };
            let style = RunStyle { color: ColorToken::TextMuted, italic: true, strike: false, capsule: false };
            let line = self.line_height(spec, PROSE_LINE);
            self.flow(vec![Run { text: format!("[图片：{name}]"), spec, style }], indent, width, line);
            return;
        }
        let start = self.height;
        self.shapes.push(Shape {
            rect: Rect::new(indent, start, width, height),
            fill: None,
            stroke: None,
            radius: None,
        });
        self.height = start + height;
        let _ = (parse_len(spec_w.as_deref()), depth);
    }
}

/// 表格单元格的行内对齐：按可用宽估算前置空白（指令集没有对齐参数）。
fn align_runs(
    runs: Vec<Run>,
    align: Option<&str>,
    available: f32,
    metrics: &mut Metrics,
    spec: FontSpec,
) -> Vec<Run> {
    let total: f32 = runs.iter().map(|run| metrics.width(&run.text, run.spec)).sum();
    let spare = (available - total).max(0.0);
    let pad = match align {
        Some("center") => spare / 2.0,
        Some("right") => spare,
        _ => 0.0,
    };
    if pad <= 0.5 {
        return runs;
    }
    let mut out = vec![Run {
        text: " ".repeat(((pad / (spec.size * 0.5)).max(0.0)) as usize),
        spec,
        style: RunStyle { color: ColorToken::Text, italic: false, strike: false, capsule: false },
    }];
    out.extend(runs);
    out
}

fn push_run(out: &mut Vec<Run>, text: &str, spec: FontSpec, style: RunStyle) {
    if text.is_empty() {
        return;
    }
    out.push(Run { text: text.to_string(), spec, style });
}

/// 在 `available` 宽度内能放下的最长前缀（字节）与该前缀字宽。
/// 逐字累加而非量算整段：量算键只有「字 + 字型」，改宽不随排版位置重新量算。
fn fit_prefix(metrics: &mut Metrics, text: &str, spec: FontSpec, available: f32) -> (usize, f32) {
    let mut width = 0.0;
    for (index, ch) in text.char_indices() {
        let advance = metrics.char_width(ch, spec);
        if width + advance > available {
            return (index, width);
        }
        width += advance;
    }
    (text.len(), width)
}

/// 两块之间的源码空行数。
fn blank_lines(source: &str, from: usize, to: usize) -> usize {
    if to <= from || to > source.len() {
        return 0;
    }
    source[from..to].matches('\n').count().saturating_sub(1)
}

/// 块的源区间（列表 / 引用按整体取，避免嵌套块重复计空行）。
fn block_range(block: &MarkdownBlock) -> (usize, usize) {
    match block {
        MarkdownBlock::Heading { from, to, .. }
        | MarkdownBlock::Paragraph { from, to, .. }
        | MarkdownBlock::Blockquote { from, to, .. }
        | MarkdownBlock::List { from, to, .. }
        | MarkdownBlock::FencedCode { from, to, .. }
        | MarkdownBlock::IndentedCode { from, to, .. }
        | MarkdownBlock::Hr { from, to }
        | MarkdownBlock::Table { from, to, .. }
        | MarkdownBlock::MathBlock { from, to, .. }
        | MarkdownBlock::HtmlBlock { from, to, .. }
        | MarkdownBlock::FootnoteDef { from, to, .. } => (*from, *to),
    }
}

/// 列表项正文的源区间：剥掉标记（`-` / `1.` / `[x]`），止于首个嵌套块。
fn item_content_range(source: &str, item: &MarkdownListItem) -> (usize, usize) {
    let from = item.from.min(source.len());
    let to = item.to.min(source.len());
    if to <= from {
        return (from, to);
    }
    let raw = &source[from..to];
    let bytes = raw.as_bytes();
    let skip_ws = |mut index: usize| {
        while matches!(bytes.get(index), Some(b' ') | Some(b'\t')) {
            index += 1;
        }
        index
    };
    let mut index = skip_ws(0);
    match bytes.get(index) {
        Some(b'-') | Some(b'+') | Some(b'*') => index += 1,
        Some(c) if c.is_ascii_digit() => {
            while matches!(bytes.get(index), Some(c) if c.is_ascii_digit()) {
                index += 1;
            }
            if matches!(bytes.get(index), Some(b'.') | Some(b')')) {
                index += 1;
            }
        }
        _ => {}
    }
    index = skip_ws(index);
    if matches!(bytes.get(index), Some(b'['))
        && matches!(bytes.get(index + 1), Some(b' ') | Some(b'x') | Some(b'X'))
        && matches!(bytes.get(index + 2), Some(b']'))
    {
        index = skip_ws(index + 3);
    }
    let content_from = from + index;
    let content_to = item
        .children
        .first()
        .map(block_range)
        .map(|(child_from, _)| child_from)
        .unwrap_or(to)
        .clamp(content_from, to);
    let trimmed = source[content_from..content_to].trim_end().len();
    (content_from, content_from + trimmed)
}

/// 逐行剥掉引用前缀（缩进 ≤3 + `>` + 至多一个空格，可叠层）。
fn strip_quote_marks(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            out.push('\n');
        }
        let mut rest = line;
        while let Some(width) = quote_prefix_len(rest) {
            rest = &rest[width..];
        }
        out.push_str(rest);
    }
    out
}

fn quote_prefix_len(text: &str) -> Option<usize> {
    let indent = text.len() - text.trim_start_matches([' ', '\t']).len();
    if indent > 3 || !text[indent..].starts_with('>') {
        return None;
    }
    let after = &text[indent + 1..];
    let spaces = after.len() - after.trim_start_matches([' ', '\t']).len();
    Some(indent + 1 + spaces.min(1))
}

/// 宽度 / 高度声明（`![x](p.png =120x80)` 那类）：解析成像素。
fn parse_len(value: Option<&str>) -> Option<f32> {
    let raw = value?.trim().trim_end_matches(['%', 'x', 'X']);
    raw.parse::<f32>().ok().filter(|value| *value > 0.0)
}

fn image_only(inline: &[InlineSpan]) -> Option<&InlineSpan> {
    match inline {
        [span @ InlineSpan::Image { .. }] => Some(span),
        _ => None,
    }
}

/// 块与块之间的额外留白（分隔线上下）。
const GAP_AROUND_HR: f32 = 6.0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headings_grow_by_level_and_paragraphs_wrap() {
        let theme = Theme::default();
        let mut metrics = Metrics::default();
        let laid = layout(&mut metrics, &theme, "# 标题\n\n正文", &[], 240.0);
        let sizes: Vec<f32> = laid.lines.iter().map(|line| line.runs[0].spec.size).collect();
        assert!(sizes[0] > sizes[1], "标题字号要大于正文：{sizes:?}");
        assert_eq!(laid.lines[0].runs[0].text, "标题");
    }

    #[test]
    fn blank_lines_between_blocks_become_vertical_space() {
        let theme = Theme::default();
        let mut metrics = Metrics::default();
        let tight = layout(&mut metrics, &theme, "甲\n\n乙", &[], 240.0).height;
        let loose = layout(&mut metrics, &theme, "甲\n\n\n乙", &[], 240.0).height;
        assert!(loose > tight, "多一个空行就要多一段行高：{tight} vs {loose}");
    }

    #[test]
    fn inline_styles_keep_italic_and_strike_and_capsules() {
        let theme = Theme::default();
        let mut metrics = Metrics::default();
        let source = "**粗** *斜* ~~删~~ `码` #标签";
        let laid = layout(&mut metrics, &theme, source, &[], 400.0);
        let runs: Vec<&Run> = laid.lines.iter().flat_map(|line| line.runs.iter()).collect();
        assert!(runs.iter().any(|run| run.style.italic && run.text == "斜"));
        assert!(runs.iter().any(|run| run.style.strike && run.text == "删"));
        assert!(runs.iter().any(|run| run.style.capsule && run.text == "#标签"), "标签画成胶囊");
        assert!(runs.iter().any(|run| run.spec.family == Some(TextFamily::Mono) && run.text == "码"));
    }

    #[test]
    fn mentions_become_capsules_for_the_message_bubble() {
        let theme = Theme::default();
        let mut metrics = Metrics::default();
        let mentions = vec![MentionCandidate { key: "笔记/方案.md".into(), label: "笔记：方案.md".into() }];
        let laid = layout(&mut metrics, &theme, "看看 #笔记：方案.md 怎么写", &mentions, 400.0);
        let runs: Vec<&Run> = laid.lines.iter().flat_map(|line| line.runs.iter()).collect();
        assert!(
            runs.iter().any(|run| run.style.capsule && run.text.contains("方案.md")),
            "引用标签要画成胶囊：{:?}",
            runs.iter().map(|run| run.text.as_str()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn code_blocks_carry_a_copy_region_and_clip_long_lines() {
        let theme = Theme::default();
        let mut metrics = Metrics::default();
        let source = "```rust\nlet a = 1;\n```";
        let laid = layout(&mut metrics, &theme, source, &[], 300.0);
        assert_eq!(laid.copies.len(), 1, "每个代码块一个复制钮");
        assert!(laid.copies[0].text.contains("let a = 1;"));
        assert!(laid.lines.iter().any(|line| line.runs.iter().any(|run| run.text == "rust")));
    }

    #[test]
    fn task_items_paint_a_box_and_lists_stay_indented() {
        let theme = Theme::default();
        let mut metrics = Metrics::default();
        let laid = layout(&mut metrics, &theme, "- [x] 做完\n- [ ] 没做", &[], 300.0);
        assert!(laid.strokes.len() >= 1, "勾选态要有勾线");
        let bullets: Vec<&Line> = laid.lines.iter().filter(|line| line.runs.iter().any(|run| run.text == "做完")).collect();
        assert!(!bullets.is_empty());
        assert!(bullets[0].x > 0.0, "列表正文要缩进到标记列之后");
    }

    #[test]
    fn rendering_the_same_content_twice_reuses_the_layout() {
        let cache = LayoutCache::default();
        let theme = Theme::default();
        let mut metrics = Metrics::default();
        let key = content_key("一段正文", &[]);
        let first = laid_out(&cache, &mut metrics, &theme, key, "一段正文", &[], 240.0);
        let again = laid_out(&cache, &mut metrics, &theme, key, "一段正文", &[], 240.0);
        assert!(Arc::ptr_eq(&first, &again), "同内容同宽度应命中缓存");
        let narrower = laid_out(&cache, &mut metrics, &theme, key, "一段正文", &[], 120.0);
        assert!(!Arc::ptr_eq(&first, &narrower), "宽度变了要按新宽度重排");
    }

    #[test]
    fn resizing_does_not_queue_new_measurements() {
        use atelyx_plugin_protocol::method::MeasureTextResult;

        let long = "一段用于验证排版宽度的普通正文，包含中文与 ASCII 混排 abc 123。".repeat(20);
        let source = format!("# 标题\n\n{long}\n\n- 列表条目\n\n> {long}\n\n```\nlet value = 1;\n```\n");
        let theme = Theme::default();
        let mut metrics = Metrics::default();
        let cache = LayoutCache::default();
        let key = content_key(&source, &[]);

        let _ = laid_out(&cache, &mut metrics, &theme, key, &source, &[], 360.0);
        let first = metrics.take_misses();
        let sizes: Vec<MeasureTextResult> = first
            .iter()
            .map(|item| MeasureTextResult { width: item.size.max(1.0), height: item.size * 1.5, lines: 1 })
            .collect();
        metrics.apply(&first, &sizes);

        for width in [360.0_f32, 359.0, 358.0, 300.0, 420.0] {
            let _ = laid_out(&cache, &mut metrics, &theme, key, &source, &[], width);
            assert!(metrics.take_misses().is_empty(), "改宽到 {width} 后不得再登记量算");
        }
    }
}
