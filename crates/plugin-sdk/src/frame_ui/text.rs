//! 输入框内的文字排布：用量算宽度换算光标 / 选区 / 省略，并在光标超出可见宽度时水平滚动窗口。

use std::borrow::Cow;

use atelyx_plugin_protocol::paint::{PaintColor, DrawCommand, TextAlign};

use super::build::BuildCtx;
use super::metrics::Metrics;
use super::model::FontSpec;

/// 逐前缀量算的字符数上限：更长的值按比例估算光标位置，免得一条输入把量算预算吃穿。
const MAX_PREFIX_MEASURES: usize = 512;

/// 字符边界（含 0 与末尾）。
pub(crate) fn boundaries(text: &str) -> Vec<usize> {
    let mut out: Vec<usize> = text.char_indices().map(|(index, _)| index).collect();
    out.push(text.len());
    out
}

/// 每个字符边界处的前缀宽度（与 [`boundaries`] 一一对应）。
pub(crate) fn prefix_widths(metrics: &mut Metrics, text: &str, spec: FontSpec) -> Vec<f32> {
    let points = boundaries(text);
    if points.len() > MAX_PREFIX_MEASURES {
        let total = metrics.width(text, spec);
        let last = points.len().saturating_sub(1) as f32;
        return points.iter().enumerate().map(|(index, _)| total * index as f32 / last).collect();
    }
    points.iter().map(|end| metrics.width(&text[..*end], spec)).collect()
}

/// 输入框可见窗口：`[start, end)` 为要绘制的字节区间，`caret` / `selection` 为相对文本区左缘的横向位置。
pub(crate) struct Window {
    pub start: usize,
    pub end: usize,
    pub caret: f32,
    pub selection: Option<(f32, f32)>,
    pub truncated: bool,
}

/// 在给定可用宽度下摆放光标与选区：光标超出右缘时把窗口整体左移，让光标留在可见区。
pub(crate) fn window(
    metrics: &mut Metrics,
    value: &str,
    spec: FontSpec,
    available: f32,
    caret: usize,
    anchor: usize,
) -> Window {
    let points = boundaries(value);
    let widths = prefix_widths(metrics, value, spec);
    let caret_index = index_of(&points, caret);
    let anchor_index = index_of(&points, anchor);
    let mut start = 0usize;
    if widths[caret_index] > available - 2.0 {
        let mut index = caret_index;
        while index > 0 && widths[caret_index] - widths[index - 1] <= available - 2.0 {
            index -= 1;
        }
        start = index;
    }
    let ellipsis = metrics.width("…", spec);
    let mut end = points.len() - 1;
    while end > start {
        let extra = if end == points.len() - 1 { 0.0 } else { ellipsis };
        if widths[end] - widths[start] + extra <= available {
            break;
        }
        end -= 1;
    }
    let shift = widths[start];
    let caret_x = widths[caret_index] - shift;
    let (from, to) = if anchor_index <= caret_index { (anchor_index, caret_index) } else { (caret_index, anchor_index) };
    let selection = (from != to).then(|| {
        let low = (widths[from] - shift).clamp(0.0, available);
        let high = (widths[to] - shift).clamp(0.0, available);
        (low, high)
    });
    Window { start: points[start], end: points[end], caret: caret_x.clamp(0.0, available), selection, truncated: end < points.len() - 1 }
}

/// 光标字节位 → 边界下标（越界或非边界时收敛到最近的边界）。
pub(crate) fn index_of(points: &[usize], caret: usize) -> usize {
    match points.binary_search(&caret) {
        Ok(index) => index,
        Err(index) => index.saturating_sub(1).min(points.len() - 1),
    }
}

/// 光标落到字符边界上（插件换值时旧光标可能停在多字节字符内部）。
pub(crate) fn snap(value: &str, index: usize) -> usize {
    let points = boundaries(value);
    points[index_of(&points, index.min(value.len()))]
}

/// 输入法组合串在值里的位置：`insert` 为插入点字节位，`len` 为串的字节长度。
/// 排版把组合串当成插入点处的临时字符，其后的文字顺次后移——输入拼音时后面的字就是被这样推开的。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Composed {
    insert: usize,
    len: usize,
}

impl Composed {
    /// `caret` 为插入点（值里的字节位）；空串等同未组合。
    pub(crate) fn new(caret: usize, preedit: &str) -> Self {
        Self { insert: caret, len: preedit.len() }
    }

    pub(crate) fn is_active(&self) -> bool {
        self.len > 0
    }

    /// 组合串在排版文本里的字节区间。
    pub(crate) fn span(&self) -> std::ops::Range<usize> {
        self.insert..self.insert + self.len
    }

    /// 值里的字节位 → 排版文本里的字节位（插入点之后整体后移组合串长度）。
    pub(crate) fn index(&self, byte: usize) -> usize {
        if byte <= self.insert {
            byte
        } else {
            byte + self.len
        }
    }

    /// 光标的排版字节位：落在插入点上时停在串尾，组合串不会被自己盖住。
    pub(crate) fn caret(&self, byte: usize) -> usize {
        if byte < self.insert {
            byte
        } else {
            byte + self.len
        }
    }
}

/// 排版文本：组合串插在插入点处；未组合时原样借用。
/// 插入点落在换行上时退到 `\r` 之前——CRLF 不可拆开。
pub(crate) fn compose<'a>(value: &'a str, at: Composed, preedit: &str) -> Cow<'a, str> {
    if preedit.is_empty() {
        return Cow::Borrowed(value);
    }
    let mut split = snap(value, at.insert);
    if value[split..].starts_with('\n') && split > 0 && value.as_bytes()[split - 1] == b'\r' {
        split -= 1;
    }
    let mut out = String::with_capacity(value.len() + preedit.len());
    out.push_str(&value[..split]);
    out.push_str(preedit);
    out.push_str(&value[split..]);
    Cow::Owned(out)
}

/// 软换行后的一行：`[start, end)` 是原文里的字节区间（不含换行符本身）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Line {
    pub start: usize,
    pub end: usize,
}

/// 按可用宽度与显式换行切行：贪心装字、每行至少一个字，空段落也占一行。
pub(crate) fn lines(metrics: &mut Metrics, text: &str, spec: FontSpec, width: f32) -> Vec<Line> {
    let width = width.max(1.0);
    let mut out = Vec::new();
    let mut offset = 0usize;
    for paragraph in text.split('\n') {
        let points = boundaries(paragraph);
        let widths = prefix_widths(metrics, paragraph, spec);
        let mut first = 0usize;
        for index in 1..points.len() {
            if widths[index] - widths[first] > width && index > first + 1 {
                out.push(Line { start: offset + points[first], end: offset + points[index - 1] });
                first = index - 1;
            }
        }
        out.push(Line { start: offset + points[first], end: offset + paragraph.len() });
        offset += paragraph.len() + 1;
    }
    out
}

/// 光标所在行：落在行尾时算该行（换行符归前一行）。
pub(crate) fn line_of(lines: &[Line], caret: usize) -> usize {
    lines.iter().position(|line| caret <= line.end).unwrap_or(lines.len().saturating_sub(1))
}

/// 多行输入的纵向偏移：用户滚动的偏移 + 光标跟随，按整行吸附。
/// 吸附是必需的：指令集没有裁剪，半截露在框边的行会整行消失，看着像掉字。
pub(crate) fn area_scroll(
    stored: f32,
    caret_line: usize,
    line_height: f32,
    view_height: f32,
    line_count: usize,
    focused: bool,
) -> f32 {
    let line_height = line_height.max(1.0);
    let content = line_count as f32 * line_height;
    let max = (content - view_height).max(0.0);
    let mut scroll = stored.clamp(0.0, max);
    if focused {
        let top = caret_line as f32 * line_height;
        if top < scroll {
            scroll = top;
        } else if top + line_height > scroll + view_height {
            scroll = top + line_height - view_height;
        }
    }
    (((scroll / line_height).ceil() * line_height).clamp(0.0, max)).max(0.0)
}

/// 行内字节位 → 相对行首的横向位置。
pub(crate) fn offset_in_line(metrics: &mut Metrics, line: &str, spec: FontSpec, byte: usize) -> f32 {
    metrics.width(&line[..byte.min(line.len())], spec)
}

/// 行内横向落点 → 最近的字节位（相对行首）。
pub(crate) fn caret_at_x(metrics: &mut Metrics, line: &str, spec: FontSpec, target: f32) -> usize {
    let points = boundaries(line);
    let widths = prefix_widths(metrics, line, spec);
    let mut best = 0usize;
    for (index, width) in widths.iter().enumerate() {
        if (*width - target).abs() < (widths[best] - target).abs() {
            best = index;
        }
    }
    points[best]
}

/// 按宽度省略：放不下时取最长可容纳前缀 + `…`。
pub(crate) fn elide(metrics: &mut Metrics, text: &str, spec: FontSpec, available: f32) -> String {
    if metrics.width(text, spec) <= available {
        return text.to_string();
    }
    let ellipsis_width = metrics.width("…", spec);
    if ellipsis_width > available {
        return String::new();
    }
    // 逐字累加定前缀：量算键只有「字 + 字型」，改宽不会按排版位置重新量算。
    // 超出可用宽度的字也照样取一次字宽，后面的宽度变宽时不再补量。
    let mut width = 0.0;
    let mut end = 0usize;
    let mut fits = true;
    for (index, ch) in text.char_indices() {
        let advance = metrics.char_width(ch, spec);
        if fits {
            if width + advance + ellipsis_width > available {
                fits = false;
            } else {
                width += advance;
                end = index + ch.len_utf8();
            }
        }
    }
    if end == text.len() {
        return text.to_string();
    }
    format!("{}…", &text[..end])
}

/// 软换行切行：逐字累加宽度（量算键只有「字 + 字型」，与可用宽度无关），每行至少一个字，空段落占一行。
/// 由插件自己切行绘制，宿主就不必按宽度量算整段——改宽不会为一屏文字重新登记量算。
pub(crate) fn wrap_lines<'a>(metrics: &mut Metrics, text: &'a str, spec: FontSpec, width: f32) -> Vec<&'a str> {
    let mut out = Vec::new();
    wrap_each(metrics, text, spec, width, |start, end| out.push(&text[start..end]));
    out
}

/// 贪心切行，把每行的字节区间交给 `emit`：要行表就收集切片，只定高就数次数（不分配行表）。
fn wrap_each(
    metrics: &mut Metrics,
    text: &str,
    spec: FontSpec,
    width: f32,
    mut emit: impl FnMut(usize, usize),
) {
    let width = width.max(1.0);
    let mut offset = 0usize;
    for paragraph in text.split('\n') {
        let mut start = 0usize;
        let mut used = 0.0;
        for (index, ch) in paragraph.char_indices() {
            let advance = metrics.char_width(ch, spec);
            if used + advance > width && index > start {
                emit(offset + start, offset + index);
                start = index;
                used = 0.0;
            }
            used += advance;
        }
        emit(offset + start, offset + paragraph.len());
        offset += paragraph.len() + 1;
    }
}

/// 单行高度：软换行的整块高度按行数累加。
pub(crate) fn line_height(metrics: &mut Metrics, spec: FontSpec) -> f32 {
    metrics.measure("", spec, None).height.max(spec.size * 1.2)
}

/// 单行宽度（逐字累加）：用于居中摆放已省略的标签，不按整串登记量算。
pub(crate) fn advance(metrics: &mut Metrics, text: &str, spec: FontSpec) -> f32 {
    text.chars().map(|ch| metrics.char_width(ch, spec)).sum()
}

/// 软换行后的整块高度：只数行数，不建行表。
pub(crate) fn wrapped_height(metrics: &mut Metrics, text: &str, spec: FontSpec, width: f32) -> f32 {
    let line = line_height(metrics, spec);
    wrapped_height_with_line(metrics, text, spec, width, line)
}

/// 软换行后的整块高度，行高由调用方给定（输入框可与正文行距对齐）。
pub(crate) fn wrapped_height_with_line(metrics: &mut Metrics, text: &str, spec: FontSpec, width: f32, line: f32) -> f32 {
    let mut lines = 0usize;
    wrap_each(metrics, text, spec, width, |_, _| lines += 1);
    lines as f32 * line
}

/// 单行文字绘制：`max_width` 给定时文字按该宽度换行（只用于输入框这类定宽单行控件）。
/// 指令集没有裁剪：文字按整块判断，垂直方向不完整落在可见区内即不画（半截文字会串到相邻区域）。
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint(
    ctx: &mut BuildCtx<'_>,
    content: &str,
    spec: FontSpec,
    color: PaintColor,
    x: f32,
    y: f32,
    max_width: Option<f32>,
    align: Option<TextAlign>,
    clip: super::geom::Rect,
) {
    // 高度按逐字宽累计的行数算：改宽不会为整段文字登记新的量算条目，
    // 绘制期也就不会向宿主发请求（宿主正在等这一帧时会互相卡住）
    let height = match max_width {
        Some(width) => wrapped_height(&mut ctx.metrics, content, spec, width),
        None => line_height(&mut ctx.metrics, spec),
    }
    .max(spec.size * 1.2);
    if y < clip.y || y + height > clip.bottom() + 0.5 {
        return;
    }
    ctx.draw(DrawCommand::Text {
        x,
        y,
        content: content.to_string(),
        size: spec.size,
        color,
        weight: spec.weight,
        family: spec.family,
        family_name: None,
        italic: false,
        max_width,
        align,
        layout: None,
    });
}

/// 已定高的一行文字：高度由调用方给出，不按行文本量算。
/// 软换行 / 省略后的行文本随可用宽度变化，按文本量算会在改宽时为每一行登记新条目。
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_line(
    ctx: &mut BuildCtx<'_>,
    content: &str,
    spec: FontSpec,
    color: PaintColor,
    x: f32,
    y: f32,
    height: f32,
    clip: super::geom::Rect,
) {
    if y < clip.y || y + height > clip.bottom() + 0.5 {
        return;
    }
    ctx.draw(DrawCommand::Text {
        x,
        y,
        content: content.to_string(),
        size: spec.size,
        color,
        weight: spec.weight,
        family: spec.family,
        family_name: None,
        italic: false,
        max_width: None,
        align: None,
        layout: None,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn font() -> FontSpec {
        FontSpec { size: 10.0, weight: None, family: None }
    }

    #[test]
    fn lines_split_on_width_and_explicit_breaks() {
        let mut metrics = Metrics::default();
        // 量算未落地时按估算字宽：ASCII 每字 5.5px，12px 一行装两个字
        assert_eq!(
            lines(&mut metrics, "abcd", font(), 12.0),
            vec![Line { start: 0, end: 2 }, Line { start: 2, end: 4 }]
        );
        // 末尾换行也要留出一行空行，光标才能停在它上面
        assert_eq!(
            lines(&mut metrics, "a\nb\n", font(), 100.0),
            vec![Line { start: 0, end: 1 }, Line { start: 2, end: 3 }, Line { start: 4, end: 4 }]
        );
    }

    #[test]
    fn wrap_lines_break_at_the_estimated_width() {
        let mut metrics = Metrics::default();
        // 量算未落地时按估算字宽：ASCII 每字 5.5px，22px 一行装四个字
        assert_eq!(wrap_lines(&mut metrics, "abcdefgh", font(), 22.0), vec!["abcd", "efgh"]);
        assert_eq!(wrap_lines(&mut metrics, "ab\ncd", font(), 22.0), vec!["ab", "cd"], "显式换行照切");
        assert_eq!(wrap_lines(&mut metrics, "", font(), 22.0), vec![""], "空段落也占一行");
    }

    #[test]
    fn wrapped_height_counts_the_same_lines_as_the_line_table() {
        let mut metrics = Metrics::default();
        for width in [1.0_f32, 12.0, 40.0, 100.0, 1000.0] {
            for text in ["", "abcd", "a\nb\n", "中英混排 abc 123 中英混排", "换行\n\n空白"] {
                let lines = wrap_lines(&mut metrics, text, font(), width).len() as f32;
                let expected = lines * line_height(&mut metrics, font());
                let counted = wrapped_height(&mut metrics, text, font(), width);
                assert_eq!(counted, expected, "width={width} text={text:?}");
            }
        }
    }

    #[test]
    fn area_scroll_keeps_the_caret_row_in_view_and_snaps_to_lines() {
        // 3 行高的内框：第 4 行（下标 3）露出时整体上移一行
        assert_eq!(area_scroll(0.0, 0, 10.0, 35.0, 10, true), 0.0);
        assert_eq!(area_scroll(0.0, 3, 10.0, 35.0, 10, true), 10.0, "露出第 4 行下缘 5px，吸附到整行");
        assert_eq!(area_scroll(0.0, 9, 10.0, 35.0, 10, true), 65.0, "末行贴底，不越过可滚动余量");
        // 未聚焦时保留用户滚动值，不跟光标走
        assert_eq!(area_scroll(30.0, 0, 10.0, 35.0, 10, false), 30.0);
        // 内容不足一屏时不滚
        assert_eq!(area_scroll(30.0, 0, 10.0, 200.0, 10, true), 0.0);
    }

    #[test]
    fn snap_rounds_to_char_boundaries() {
        assert_eq!(snap("中文", 4), 3);
        assert_eq!(snap("中文", 99), 6);
    }

    /// 组合串插在插入点、后续文字顺次后移；未组合时原样借用。
    #[test]
    fn compose_pushes_the_tail_after_the_composition() {
        let at = Composed::new(3, "zhong");
        assert_eq!(compose("abcdef", at, "zhong").as_ref(), "abczhongdef");
        assert_eq!(compose("abc", Composed::default(), "").as_ref(), "abc");
    }

    /// 插入点落在 CRLF 的换行上：组合串退到 `\r` 之前，不把行尾拆开。
    /// `甲` 占 0..3，`\r\n` 占 3..5，`乙` 从 5 开始。
    #[test]
    fn compose_keeps_crlf_together() {
        let value = "甲\r\n乙";
        assert_eq!(compose(value, Composed::new(3, "ni"), "ni").as_ref(), "甲ni\r\n乙");
        assert_eq!(compose(value, Composed::new(5, "ni"), "ni").as_ref(), "甲\r\nni乙");
    }

    /// 下标换算：插入点及其后的位置整体后移；光标落在插入点上时停在串尾。
    #[test]
    fn composed_offsets_shift_only_the_tail() {
        let at = Composed::new(3, "zhong");
        assert_eq!(at.span(), 3..8);
        assert_eq!(at.index(0), 0);
        assert_eq!(at.index(3), 3, "插入点自身仍在串首之前");
        assert_eq!(at.index(4), 9);
        assert_eq!(at.caret(3), 8, "光标停在串尾");
        assert_eq!(at.caret(2), 2);
    }

    /// 多字节值里插入点不在字符边界时先收敛到边界，组合串不会切坏字符。
    #[test]
    fn compose_snaps_to_a_char_boundary() {
        assert_eq!(compose("中文", Composed::new(1, "ni"), "ni").as_ref(), "ni中文");
    }

    #[test]
    fn caret_at_x_hits_the_nearest_boundary() {
        let mut metrics = Metrics::default();
        assert_eq!(caret_at_x(&mut metrics, "abcd", font(), 6.0), 1);
        assert_eq!(caret_at_x(&mut metrics, "abcd", font(), 100.0), 4);
    }
}
