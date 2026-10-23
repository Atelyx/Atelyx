//! 只读笔记的拖选复制：与富文本渲染同源的段落（样式片段不拆分），叠加选区高亮与
//! 落点换算；选区真相（文档源字节区间）由应用层持有。

use iced::advanced::graphics::text::cosmic_text;
use iced::advanced::layout;
use iced::advanced::mouse;
use iced::advanced::renderer::Quad;
use iced::advanced::text as advanced_text;
use iced::advanced::text::{Paragraph as _, Span};
use iced::advanced::widget::{text, tree, Tree};
use iced::advanced::{Clipboard, Layout, Renderer as _, Shell, Widget};
use iced::alignment;
use iced::{Color, Element, Event, Font, Length, Point, Rectangle, Size, Vector};

use crate::markdown_edit::ParagraphT;
use crate::markdown_view::LinkTarget;
use crate::ui::palette as pal;
use crate::Message;

/// 只读档选区：文档源字节偏移（anchor = 起点，head = 拖拽端）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoteSelection {
    pub anchor: usize,
    pub head: usize,
    /// 左键拖拽在途（全局 Release 收口）。
    pub dragging: bool,
}

impl NoteSelection {
    pub fn range(&self) -> (usize, usize) {
        (self.anchor.min(self.head), self.anchor.max(self.head))
    }
}

/// 一个样式片段的源区间与对齐口径。`linear` = 可见文本与源区间逐字节对应；
/// 否则整段替换（图片占位、角标等），选择时按整体处理。
pub struct SelectRun {
    pub src: (usize, usize),
    pub linear: bool,
}

impl SelectRun {
    pub fn new(src: (usize, usize), text_len: usize) -> Self {
        Self { src, linear: src.1.saturating_sub(src.0) == text_len }
    }
}

/// 富文本段落（可拖选）。样式与 `markdown_view::rich` 同构，段落按未拆分的样式片段排版。
pub struct SelectableRich<'a> {
    spans: Vec<Span<'a, LinkTarget, Font>>,
    srcs: Vec<SelectRun>,
    size: f32,
    line_height: f32,
    align_x: alignment::Horizontal,
    color: Option<Color>,
    /// 文档源区间（已排序；None = 无选区）
    selection: Option<(usize, usize)>,
    dragging: bool,
}

impl<'a> SelectableRich<'a> {
    pub fn new(spans: Vec<Span<'a, LinkTarget, Font>>, srcs: Vec<SelectRun>) -> Self {
        Self {
            spans,
            srcs,
            size: 16.0,
            line_height: 1.0,
            align_x: alignment::Horizontal::Left,
            color: None,
            selection: None,
            dragging: false,
        }
    }

    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }

    pub fn line_height(mut self, line_height: f32) -> Self {
        self.line_height = line_height;
        self
    }

    pub fn align_x(mut self, align_x: alignment::Horizontal) -> Self {
        self.align_x = align_x;
        self
    }

    pub fn color_maybe(mut self, color: Option<Color>) -> Self {
        self.color = color;
        self
    }

    /// 选区区间（高亮用；空区间不画）。
    pub fn selection(mut self, range: (usize, usize)) -> Self {
        self.selection = Some(range);
        self
    }

    /// 拖拽在途标志：与选区区间分开传——锚点尚未移动时也要接住后续移动。
    pub fn dragging(mut self, dragging: bool) -> Self {
        self.dragging = dragging;
        self
    }
}

struct State {
    spans: Vec<Span<'static, LinkTarget, Font>>,
    paragraph: ParagraphT,
}

impl State {
    fn new() -> Self {
        Self { spans: Vec::new(), paragraph: ParagraphT::default() }
    }
}

/// 选区几何的最小单元：一段源区间与它的矩形（原子片段跨行时按行拆开，源区间相同）。
struct Cell {
    src: (usize, usize),
    rects: Vec<Rectangle>,
    rtl: bool,
}

impl<'a> SelectableRich<'a> {
    /// 段落几何：逐字形取源区间与矩形（样式片段按行内字节位置切分）。
    fn geometry(&self, paragraph: &ParagraphT) -> Vec<Cell> {
        let mut out = Vec::new();
        // 各片段在跨行时被消费到的字节位置（行文本是片段文本按序拼接）
        let mut consumed = vec![0usize; self.spans.len()];
        for run in paragraph.buffer().layout_runs() {
            let mut groups: Vec<(usize, usize, usize)> = Vec::new();
            for glyph in run.glyphs {
                match groups.iter_mut().find(|(span, _, _)| *span == glyph.metadata) {
                    Some(group) => {
                        group.1 = group.1.min(glyph.start);
                        group.2 = group.2.max(glyph.end);
                    }
                    None => groups.push((glyph.metadata, glyph.start, glyph.end)),
                }
            }
            groups.sort_by_key(|group| group.1);
            for (span_index, lo, hi) in groups {
                let (Some(span), Some(src)) = (self.spans.get(span_index), self.srcs.get(span_index)) else {
                    continue;
                };
                let text = span.text.as_ref();
                let target = run.text.get(lo..hi).unwrap_or_default();
                let base = if target.is_empty() {
                    consumed[span_index]
                } else {
                    text.get(consumed[span_index]..)
                        .and_then(|tail| tail.find(target))
                        .map_or(consumed[span_index], |k| consumed[span_index] + k)
                };
                consumed[span_index] = (base + target.len()).min(text.len());
                if src.linear && src.src.1.saturating_sub(src.src.0) == text.len() {
                    for glyph in run.glyphs.iter().filter(|glyph| glyph.metadata == span_index) {
                        let from = base + glyph.start.saturating_sub(lo);
                        let to = base + glyph.end.saturating_sub(lo);
                        let src = (src.src.0 + from.min(text.len()), src.src.0 + to.min(text.len()));
                        out.push(Cell {
                            src: (src.0.min(src.1), src.0.max(src.1)),
                            rects: vec![glyph_rect(&run, glyph)],
                            rtl: run.rtl,
                        });
                    }
                } else {
                    let mut rects: Vec<Rectangle> = Vec::new();
                    for glyph in run.glyphs.iter().filter(|glyph| glyph.metadata == span_index) {
                        let rect = glyph_rect(&run, glyph);
                        match rects.last_mut() {
                            Some(last) if (last.y - rect.y).abs() < 0.01 => {
                                let right =
                                    (last.x + last.width).max(rect.x + rect.width);
                                last.x = last.x.min(rect.x);
                                last.width = right - last.x;
                            }
                            _ => rects.push(rect),
                        }
                    }
                    out.push(Cell { src: src.src, rects, rtl: run.rtl });
                }
            }
        }
        out
    }

    /// 落点 → 文档源偏移：取最近字形，按左右半选边界（行方向 RTL 时镜像）。
    fn offset_at(&self, cells: &[Cell], point: Point) -> Option<usize> {
        let mut best: Option<(f32, &Cell, &Rectangle)> = None;
        for cell in cells {
            for rect in &cell.rects {
                let (right, bottom) = (rect.x + rect.width, rect.y + rect.height);
                let dx = if point.x < rect.x {
                    rect.x - point.x
                } else if point.x > right {
                    point.x - right
                } else {
                    0.0
                };
                let dy = if point.y < rect.y {
                    rect.y - point.y
                } else if point.y > bottom {
                    point.y - bottom
                } else {
                    0.0
                };
                // 行距权重更高：优先同一行的字形，行尾空白处落到本行末
                let distance = dx + dy * 4.0;
                if best.is_none_or(|(current, _, _)| distance < current) {
                    best = Some((distance, cell, rect));
                }
            }
        }
        let (_, cell, rect) = best?;
        let start_half = if cell.rtl { point.x >= rect.center().x } else { point.x < rect.center().x };
        Some(if start_half { cell.src.0 } else { cell.src.1 })
    }
}

fn glyph_rect(run: &cosmic_text::LayoutRun<'_>, glyph: &cosmic_text::LayoutGlyph) -> Rectangle {
    Rectangle::new(
        Point::new(glyph.x, run.line_top + glyph.y),
        Size::new(glyph.w, glyph.line_height_opt.unwrap_or(run.line_height)),
    )
}
/// 选区高亮：命中单元矩形按行合并（相邻字形同底色无接缝）。
fn selection_rects(cells: &[Cell], range: (usize, usize)) -> Vec<Rectangle> {
    let (from, to) = range;
    let mut rects: Vec<Rectangle> = cells
        .iter()
        .filter(|cell| cell.src.1 > cell.src.0 && cell.src.0 < to && from < cell.src.1)
        .flat_map(|cell| cell.rects.iter().copied())
        .collect();
    rects.sort_by(|a, b| a.y.total_cmp(&b.y).then(a.x.total_cmp(&b.x)));
    let mut out: Vec<Rectangle> = Vec::new();
    for rect in rects {
        match out.last_mut() {
            Some(last)
                if (last.y - rect.y).abs() < 0.01
                    && (last.height - rect.height).abs() < 0.01
                    && rect.x <= last.x + last.width + 0.5 =>
            {
                let right = (last.x + last.width).max(rect.x + rect.width);
                last.width = right - last.x;
            }
            _ => out.push(rect),
        }
    }
    out
}

impl Widget<Message, iced::Theme, iced::Renderer> for SelectableRich<'_> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::new())
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Shrink)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        _renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::sized(limits, Length::Fill, Length::Shrink, |limits| {
            let bounds = limits.max();
            let state = tree.state.downcast_mut::<State>();
            let text = || advanced_text::Text {
                content: self.spans.as_slice(),
                bounds,
                size: self.size.into(),
                line_height: advanced_text::LineHeight::Relative(self.line_height),
                font: crate::ui::regular(),
                align_x: self.align_x.into(),
                align_y: alignment::Vertical::Top,
                shaping: advanced_text::Shaping::Advanced,
                wrapping: advanced_text::Wrapping::default(),
            };
            if state.spans != self.spans {
                state.paragraph = ParagraphT::with_spans(text());
                state.spans = self.spans.iter().cloned().map(|span| span.to_static()).collect();
            } else {
                match state.paragraph.compare(advanced_text::Text {
                    content: (),
                    bounds,
                    size: self.size.into(),
                    line_height: advanced_text::LineHeight::Relative(self.line_height),
                    font: crate::ui::regular(),
                    align_x: self.align_x.into(),
                    align_y: alignment::Vertical::Top,
                    shaping: advanced_text::Shaping::Advanced,
                    wrapping: advanced_text::Wrapping::default(),
                }) {
                    advanced_text::Difference::None => {}
                    advanced_text::Difference::Bounds => state.paragraph.resize(bounds),
                    advanced_text::Difference::Shape => {
                        state.paragraph = ParagraphT::with_spans(text());
                    }
                }
            }
            state.paragraph.min_bounds()
        })
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        _theme: &iced::Theme,
        style: &iced::advanced::renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        if !bounds.intersects(viewport) {
            return;
        }
        let state = tree.state.downcast_ref::<State>();
        let paragraph = &state.paragraph;
        let translation = layout.position() - Point::ORIGIN;

        // 样式底色（代码 / 标签 / 标记）先铺，选区叠在其上
        for (index, span) in self.spans.iter().enumerate() {
            let Some(highlight) = span.highlight else { continue };
            for region in paragraph.span_bounds(index) {
                let region = Rectangle::new(
                    region.position() - Vector::new(span.padding.left, span.padding.top),
                    region.size() + Size::new(span.padding.x(), span.padding.y()),
                );
                renderer.fill_quad(
                    Quad {
                        bounds: region + translation,
                        border: highlight.border,
                        ..Quad::default()
                    },
                    highlight.background,
                );
            }
        }

        if let Some(range) = self.selection {
            let cells = self.geometry(paragraph);
            for rect in selection_rects(&cells, range) {
                renderer.fill_quad(
                    Quad { bounds: rect + translation, ..Quad::default() },
                    pal::SELECTION,
                );
            }
        }

        // 下划线 / 删除线（与富文本渲染同口径）
        for (index, span) in self.spans.iter().enumerate() {
            if !(span.underline || span.strikethrough) {
                continue;
            }
            let size = span.size.unwrap_or(self.size.into());
            let line_height = span
                .line_height
                .unwrap_or(advanced_text::LineHeight::Relative(self.line_height))
                .to_absolute(size);
            let color = span.color.or(self.color).unwrap_or(style.text_color);
            let baseline = translation + Vector::new(0.0, size.0 + (line_height.0 - size.0) / 2.0);
            for region in paragraph.span_bounds(index) {
                if span.underline {
                    renderer.fill_quad(
                        Quad {
                            bounds: Rectangle::new(
                                region.position() + baseline - Vector::new(0.0, size.0 * 0.08),
                                Size::new(region.width, 1.0),
                            ),
                            ..Quad::default()
                        },
                        color,
                    );
                }
                if span.strikethrough {
                    renderer.fill_quad(
                        Quad {
                            bounds: Rectangle::new(
                                region.position() + baseline - Vector::new(0.0, size.0 / 2.0),
                                Size::new(region.width, 1.0),
                            ),
                            ..Quad::default()
                        },
                        color,
                    );
                }
            }
        }

        text::draw(
            renderer,
            style,
            bounds,
            paragraph,
            text::Style { color: self.color },
            viewport,
        );
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &iced::Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let Some(position) = cursor.position_in(layout.bounds()) else { return };

        match event {
            // 不捕获按压：外层双击进编辑仍要收到同一事件
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let state = tree.state.downcast_ref::<State>();
                if let Some(offset) = self.offset_at(&self.geometry(&state.paragraph), position) {
                    shell.publish(Message::NoteSelectStart(offset));
                }
            }
            // 拖选在途：只有光标落在本块内才按本块几何推进（块间空隙保留上一个落点）
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                if !self.dragging {
                    return;
                }
                let state = tree.state.downcast_ref::<State>();
                if let Some(offset) = self.offset_at(&self.geometry(&state.paragraph), position) {
                    shell.publish(Message::NoteSelectTo(offset));
                }
            }
            _ => {}
        }
    }

    fn mouse_interaction(
        &self,
        _tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        if cursor.is_over(layout.bounds()) {
            mouse::Interaction::Text
        } else {
            mouse::Interaction::None
        }
    }
}

impl<'a> From<SelectableRich<'a>> for Element<'a, Message> {
    fn from(widget: SelectableRich<'a>) -> Self {
        Element::new(widget)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown_edit::ParagraphT;

    fn paragraph(spans: &[Span<'_, LinkTarget, Font>]) -> ParagraphT {
        ParagraphT::with_spans(advanced_text::Text {
            content: spans,
            bounds: Size::new(f32::INFINITY, f32::INFINITY),
            size: 16.0.into(),
            line_height: advanced_text::LineHeight::Relative(1.6),
            font: crate::ui::regular(),
            align_x: advanced_text::Alignment::Left,
            align_y: alignment::Vertical::Top,
            shaping: advanced_text::Shaping::Advanced,
            wrapping: advanced_text::Wrapping::default(),
        })
    }

    /// 逐字形几何按「样式片段 + 行内字节位置」还原源偏移：跨片段边界不错位。
    #[test]
    fn geometry_maps_glyphs_to_source_bytes() {
        let spans = vec![
            Span::<'_, LinkTarget, Font>::new("ab"),
            Span::<'_, LinkTarget, Font>::new("cd"),
        ];
        let widget = SelectableRich::new(
            spans.clone(),
            vec![SelectRun::new((0, 2), 2), SelectRun::new((2, 4), 2)],
        );
        let cells = widget.geometry(&paragraph(&spans));
        let srcs: Vec<(usize, usize)> =
            cells.iter().flat_map(|cell| cell.rects.iter().map(move |_| cell.src)).collect();
        assert_eq!(srcs, vec![(0, 1), (1, 2), (2, 3), (3, 4)]);
    }

    /// 落点按字形左右半取边界；相邻选区矩形按行合并为整段。
    #[test]
    fn offset_and_selection_follow_glyph_rects() {
        let spans = vec![Span::<'_, LinkTarget, Font>::new("ab")];
        let widget = SelectableRich::new(spans.clone(), vec![SelectRun::new((0, 2), 2)]);
        let cells = widget.geometry(&paragraph(&spans));
        let first = cells[0].rects[0];
        let left = Point::new(first.x + first.width * 0.25, first.y + 1.0);
        let right = Point::new(first.x + first.width * 0.75, first.y + 1.0);
        assert_eq!(widget.offset_at(&cells, left), Some(0));
        assert_eq!(widget.offset_at(&cells, right), Some(1));
        let merged = selection_rects(&cells, (0, 2));
        assert_eq!(merged.len(), 1);
    }
}
