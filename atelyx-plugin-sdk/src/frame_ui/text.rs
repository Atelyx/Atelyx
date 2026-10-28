//! 输入框内的文字排布：用量算宽度换算光标 / 选区 / 省略，并在光标超出可见宽度时水平滚动窗口。

use atelyx_plugin_protocol::canvas::{CanvasColor, DrawCommand, TextAlign};

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
    color: CanvasColor,
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
