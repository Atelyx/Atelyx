//! 输入框内的文字排布：用量算宽度换算光标 / 选区 / 省略，并在光标超出可见宽度时水平滚动窗口。

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

/// 多行输入的可视首行：光标行超出内框时整体上移，让光标行贴住底边。
/// 只取决于光标行，因此滚动位置无需跨帧保存。
pub(crate) fn top_line(caret_line: usize, line_height: f32, height: f32) -> usize {
    let visible = (height / line_height.max(1.0)).floor().max(1.0) as usize;
    (caret_line + 1).saturating_sub(visible)
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
    let ellipsis = "…";
    let ellipsis_width = metrics.width(ellipsis, spec);
    if ellipsis_width > available {
        return String::new();
    }
    let points = boundaries(text);
    let mut low = 0usize;
    let mut high = points.len() - 1;
    while low < high {
        let mid = (low + high).div_ceil(2);
        let width = metrics.width(&text[..points[mid]], spec) + ellipsis_width;
        if width <= available {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    format!("{}…", &text[..points[low]])
}

/// 单行文字绘制：`max_width` 给定时文字按该宽度换行。
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
    let height = ctx.metrics.measure(content, spec, max_width).height.max(spec.size * 1.2);
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
        max_width,
        align,
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
    fn top_line_keeps_the_caret_row_in_view() {
        assert_eq!(top_line(0, 10.0, 35.0), 0);
        assert_eq!(top_line(3, 10.0, 35.0), 1, "第 4 行超出 3 行高的内框时上移一行");
        assert_eq!(top_line(7, 10.0, 10.0), 7);
    }

    #[test]
    fn snap_rounds_to_char_boundaries() {
        assert_eq!(snap("中文", 4), 3);
        assert_eq!(snap("中文", 99), 6);
    }

    #[test]
    fn caret_at_x_hits_the_nearest_boundary() {
        let mut metrics = Metrics::default();
        assert_eq!(caret_at_x(&mut metrics, "abcd", font(), 6.0), 1);
        assert_eq!(caret_at_x(&mut metrics, "abcd", font(), 100.0), 4);
    }
}
