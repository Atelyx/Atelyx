//! 文本行数上限：按真实排版结果截断并补省略号，超出部分不画到相邻控件上。

use iced::advanced::layout::{self, Layout, Limits, Node};
use iced::advanced::text::paragraph::{self, Paragraph as _};
use iced::advanced::text::{Alignment, LineHeight, Shaping, Text, Wrapping};
use iced::advanced::text::Renderer as _;
use iced::advanced::widget::{tree, Operation};
use iced::advanced::{mouse, renderer, Widget};
use iced::alignment::Vertical;
use iced::{Color, Font, Length, Pixels, Rectangle, Renderer, Size, Theme};

/// 行数判定容差：排版尺寸按行高累加，按浮点比较。
const LINE_EPSILON: f32 = 0.5;

type TextParagraph = <Renderer as iced::advanced::text::Renderer>::Paragraph;

/// 限行文本：超过 `max_lines` 时截到最长可容纳前缀并补省略号。
pub(crate) struct TruncatedText<'a> {
    content: &'a str,
    size: f32,
    font: Font,
    color: Color,
    max_lines: u32,
    wrapping: Wrapping,
}

impl<'a> TruncatedText<'a> {
    pub(crate) fn new(
        content: &'a str,
        size: f32,
        font: Font,
        color: Color,
        max_lines: u32,
        wrapping: Wrapping,
    ) -> Self {
        Self { content, size, font, color, max_lines, wrapping }
    }
}

#[derive(Debug, Default)]
struct State {
    paragraph: paragraph::Plain<TextParagraph>,
    rendered: String,
    measured: Option<Measured>,
}

/// 截断结果的量算输入：同一内容 / 边界 / 版式不重复二分。
#[derive(Debug, Clone, PartialEq)]
struct Measured {
    content: String,
    bounds: Size,
    size: f32,
    font: Font,
    max_lines: u32,
    wrapping: Wrapping,
}

impl<Message: Clone + 'static> Widget<Message, Theme, Renderer> for TruncatedText<'_> {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Shrink, Length::Shrink)
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn layout(&mut self, tree: &mut tree::Tree, _renderer: &Renderer, limits: &Limits) -> Node {
        let bounds = limits.max();
        let state = tree.state.downcast_mut::<State>();
        let measured = Measured {
            content: self.content.to_string(),
            bounds,
            size: self.size,
            font: self.font,
            max_lines: self.max_lines,
            wrapping: self.wrapping,
        };
        if state.measured.as_ref() != Some(&measured) {
            state.rendered = truncated(self.content, bounds, self.size, self.font, self.max_lines, self.wrapping);
            state.measured = Some(measured);
        }
        state.paragraph.update(definition(&state.rendered, bounds, self.size, self.font, self.wrapping));
        layout::sized(limits, Length::Shrink, Length::Shrink, |_| state.paragraph.min_bounds())
    }

    fn draw(
        &self,
        tree: &tree::Tree,
        renderer: &mut Renderer,
        _theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_ref::<State>();
        let paragraph = state.paragraph.raw();
        let anchor = layout.bounds().anchor(
            paragraph.min_bounds(),
            paragraph.align_x(),
            paragraph.align_y(),
        );
        renderer.fill_paragraph(paragraph, anchor, self.color, *viewport);
    }

    fn operate(
        &mut self,
        tree: &mut tree::Tree,
        layout: Layout<'_>,
        _renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        let state = tree.state.downcast_ref::<State>();
        operation.text(None, layout.bounds(), &state.rendered);
    }
}

fn definition(
    content: &str,
    bounds: Size,
    size: f32,
    font: Font,
    wrapping: Wrapping,
) -> Text<&str, Font> {
    Text {
        content,
        bounds,
        size: Pixels(size),
        line_height: LineHeight::default(),
        font,
        align_x: Alignment::Default,
        align_y: Vertical::Top,
        shaping: Shaping::default(),
        wrapping,
    }
}

fn truncated(
    content: &str,
    bounds: Size,
    size: f32,
    font: Font,
    max_lines: u32,
    wrapping: Wrapping,
) -> String {
    let line_limit = LineHeight::default().to_absolute(Pixels(size)).0 * max_lines as f32;
    // 量算高度不限：行数上限是节点自身语义，不能因父级高度较小而少算行、漏掉省略号。
    let probe_bounds = Size::new(bounds.width, f32::INFINITY);
    let mut probe = paragraph::Plain::<TextParagraph>::default();
    let mut fits = |candidate: &str| {
        probe.update(definition(candidate, probe_bounds, size, font, wrapping));
        let measured = probe.min_bounds();
        measured.width <= bounds.width + LINE_EPSILON
            && (wrapping == Wrapping::None || measured.height <= line_limit + LINE_EPSILON)
    };
    truncate_with_ellipsis(content, &mut fits)
}

/// 二分出最长可容纳的字符前缀并补省略号；候选越长越放不下，判定单调。
fn truncate_with_ellipsis(content: &str, fits: &mut impl FnMut(&str) -> bool) -> String {
    if fits(content) {
        return content.to_string();
    }
    let cuts: Vec<usize> = content
        .char_indices()
        .map(|(index, _)| index)
        .chain(std::iter::once(content.len()))
        .collect();
    let (mut low, mut high) = (0, cuts.len() - 1);
    while low < high {
        let mid = (low + high).div_ceil(2);
        if fits(&format!("{}…", &content[..cuts[mid]])) {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    format!("{}…", &content[..cuts[low]])
}

#[cfg(test)]
mod tests {
    use super::truncate_with_ellipsis;

    #[test]
    fn keeps_longest_fitting_prefix() {
        let mut fits = |candidate: &str| candidate.chars().count() <= 5;
        assert_eq!(truncate_with_ellipsis("abcdefgh", &mut fits), "abcd…");
    }

    #[test]
    fn falls_back_to_marker_when_no_prefix_fits() {
        let mut fits = |candidate: &str| candidate.chars().count() <= 1;
        assert_eq!(truncate_with_ellipsis("abcdefgh", &mut fits), "…");
    }

    #[test]
    fn cuts_on_char_boundaries() {
        let mut fits = |candidate: &str| candidate.chars().count() <= 3;
        assert_eq!(truncate_with_ellipsis("中文标签测试", &mut fits), "中文…");
    }

    #[test]
    fn fitting_text_is_unchanged() {
        let mut fits = |candidate: &str| candidate.chars().count() <= 8;
        assert_eq!(truncate_with_ellipsis("短文本", &mut fits), "短文本");
    }
}
