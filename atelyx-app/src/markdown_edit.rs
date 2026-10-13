//! Markdown 编辑面：源码态块部件（光标所在块自绘）+ 纯文本变换。
//! 非活动块复用只读面块渲染，故两档视觉一致；正文真相保存在应用层。

use atelyx_core::markdown::{parse_inline, InlineSpan, ParseOptions};
use iced::advanced::clipboard::Kind as ClipboardKind;
use iced::advanced::input_method::{Event as ImeEvent, InputMethod, Purpose};
use iced::advanced::layout::{self, Layout};
use iced::advanced::mouse;
use iced::advanced::renderer::Quad;
use iced::advanced::text as advanced_text;
use iced::advanced::text::{Paragraph as _, Span};
use iced::advanced::widget::{self, Widget};
use iced::advanced::{Clipboard, Shell};
use iced::advanced::{Renderer as _, text::Renderer as _};
use iced::alignment;
use iced::event::Event;
use iced::keyboard;
use iced::keyboard::key::Named;
use iced::keyboard::Key;
use iced::window;
use iced::{Color, Element, Font, Length, Padding, Point, Rectangle, Size, Theme, Vector};

use std::time::{Duration, Instant};

use crate::commands::{self, NoteBindings};
use crate::layout_view::parse_hex_color;
use crate::markdown_view::SourceKind;
use crate::ui::palette as pal;
use crate::ui::semibold;
use crate::Message;

/// 行级缩进单位：两个空格（列表嵌套的通行写法）。
const INDENT_UNIT: &str = "  ";

/// 文本变换结果：新正文 + 变换后的选区（源偏移）。
pub struct LineEdit {
    pub text: String,
    pub from: usize,
    pub to: usize,
}

/// `offset` 所在行的行首偏移（offset 非字符边界时向下取整）。
fn line_start_at(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    text[..offset].rfind('\n').map_or(0, |i| i + 1)
}

/// 自 `offset` 起本行行尾偏移（不含换行符）。
fn line_end_at(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    text[offset..].find('\n').map_or(text.len(), |i| offset + i)
}

/// `offset` 所在行文本（不含换行符）。
fn line_text_at(text: &str, offset: usize) -> &str {
    let start = line_start_at(text, offset);
    &text[start..line_end_at(text, start)]
}

/// 列表行标记解析结果。
struct ListMarker {
    /// 行首缩进字节长度。
    indent: usize,
    /// 标记整体末尾（缩进 + 标记 + 标记后空白 + 任务框）。
    end: usize,
    /// 有序项序号（无序为 None）。
    ordinal: Option<u64>,
    /// 有序定界符 `'.'` / `')'`；无序为 0。
    delim: u8,
    /// 无序标记字符；有序为 0。
    bullet: u8,
    /// 是否任务项。
    task: bool,
    /// 任务框内勾选位的行内偏移（非任务项为 0）。
    task_char: usize,
}

/// 解析列表行首标记：缩进 + 标记 + 标记后一以上空白或行尾 + 可选任务框。
/// 标记后既无空白又非行尾（如 `-foo`）按非列表处理。
fn parse_list_marker(line: &str) -> Option<ListMarker> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while matches!(bytes.get(i), Some(b' ') | Some(b'\t')) {
        i += 1;
    }
    let indent = i;
    let (bullet, ordinal, delim) = match bytes.get(i) {
        Some(c @ (b'-' | b'+' | b'*')) => {
            i += 1;
            (*c, None, 0)
        }
        Some(c) if c.is_ascii_digit() => {
            let start = i;
            while matches!(bytes.get(i), Some(c) if c.is_ascii_digit()) {
                i += 1;
            }
            let delim = match bytes.get(i) {
                Some(d @ (b'.' | b')')) => *d,
                _ => return None,
            };
            let ordinal = line[start..i].parse().ok()?;
            i += 1;
            (0, Some(ordinal), delim)
        }
        _ => return None,
    };
    let ws_start = i;
    while matches!(bytes.get(i), Some(b' ') | Some(b'\t')) {
        i += 1;
    }
    if i == ws_start && i < line.len() {
        return None;
    }
    let mut task = false;
    let mut task_char = 0;
    if bytes.get(i) == Some(&b'[')
        && matches!(bytes.get(i + 1), Some(b' ') | Some(b'x') | Some(b'X'))
        && bytes.get(i + 2) == Some(&b']')
    {
        task_char = i + 1;
        i += 3;
        while matches!(bytes.get(i), Some(b' ') | Some(b'\t')) {
            i += 1;
        }
        task = true;
    }
    Some(ListMarker { indent, end: i, ordinal, delim, bullet, task, task_char })
}

/// 列表项行判定：行首缩进 + 标记 + 标记后空白或行尾。
fn is_list_line(line: &str) -> bool {
    parse_list_marker(line).is_some()
}

/// 选区覆盖的行首偏移：折叠光标即当前行；有选区时取最后一个被选中字符所在行（行首终点不计入）。
fn covered_line_starts(text: &str, from: usize, to: usize) -> Vec<usize> {
    let last = line_start_at(text, if to > from { to - 1 } else { from });
    let mut starts = Vec::new();
    let mut start = line_start_at(text, from);
    loop {
        starts.push(start);
        if start >= last {
            return starts;
        }
        match text[start..].find('\n') {
            Some(i) => start += i + 1,
            None => return starts,
        }
    }
}

/// 列表行 Enter 的文本变换：光标所在行是列表标记行时延续该项——
/// 无序沿用同字符、有序序号 +1（保留定界符）、任务项带空任务框，缩进保持同级。
/// 光标停在标记之前时返回 None（那是「在标记前换行」，续标记会拆出嵌套列表）。
pub fn list_enter_edit(text: &str, from: usize) -> Option<(String, usize)> {
    let line_start = line_start_at(text, from);
    let line = &text[line_start..line_end_at(text, from)];
    let marker = parse_list_marker(line)?;
    if from < line_start + marker.end {
        return None;
    }
    let indent = &text[line_start..line_start + marker.indent];
    let next = match marker.ordinal {
        Some(n) => format!("{}{}", n + 1, marker.delim as char),
        None => (marker.bullet as char).to_string(),
    };
    let insert = format!("\n{indent}{next} {}", if marker.task { "[ ] " } else { "" });
    let mut out = String::with_capacity(text.len() + insert.len());
    out.push_str(&text[..from]);
    out.push_str(&insert);
    out.push_str(&text[from..]);
    Some((out, from + insert.len()))
}

/// 空列表项行上的退格变换：行内只有标记（无内容）且光标落在标记之内或之后时，
/// 一次删掉整段标记（缩进 + 标记 + 标记后空白 + 任务框），光标到行首。
pub fn list_backspace_edit(text: &str, from: usize) -> Option<(String, usize)> {
    let line_start = line_start_at(text, from);
    let line = &text[line_start..line_end_at(text, from)];
    let marker = parse_list_marker(line)?;
    if marker.end != line.len() {
        return None;
    }
    let marker_end = line_start + marker.end;
    if from <= line_start || from > marker_end {
        return None;
    }
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..line_start]);
    out.push_str(&text[marker_end..]);
    Some((out, line_start))
}

/// 任务框切换：`item_from` 所在列表项是任务项时翻转勾选态（`[ ]` ⇄ `[x]`）。
/// 返回 (新正文, 勾选位字节偏移)。
pub fn toggle_task(text: &str, item_from: usize) -> Option<(String, usize)> {
    let line_start = line_start_at(text, item_from);
    let line = &text[line_start..line_end_at(text, item_from)];
    let marker = parse_list_marker(line)?;
    if !marker.task {
        return None;
    }
    let at = line_start + marker.task_char;
    let ch = if text.as_bytes().get(at) == Some(&b' ') { "x" } else { " " };
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..at]);
    out.push_str(ch);
    out.push_str(&text[at + 1..]);
    Some((out, at))
}

/// Tab 缩进变换：有选区或光标停在列表项行时整行缩进（列表靠行首缩进判定嵌套）；
/// 其余情况只在光标处插入一级缩进。
pub fn indent_edit(text: &str, from: usize, to: usize) -> LineEdit {
    if from == to && !is_list_line(line_text_at(text, from)) {
        let mut out = String::with_capacity(text.len() + INDENT_UNIT.len());
        out.push_str(&text[..from]);
        out.push_str(INDENT_UNIT);
        out.push_str(&text[from..]);
        return LineEdit {
            text: out,
            from: from + INDENT_UNIT.len(),
            to: to + INDENT_UNIT.len(),
        };
    }
    let starts = covered_line_starts(text, from, to);
    let shift = |offset: usize| {
        offset + INDENT_UNIT.len() * starts.iter().filter(|s| **s <= offset).count()
    };
    let mut out = String::with_capacity(text.len() + INDENT_UNIT.len() * starts.len());
    let mut prev = 0;
    for &start in &starts {
        out.push_str(&text[prev..start]);
        out.push_str(INDENT_UNIT);
        prev = start;
    }
    out.push_str(&text[prev..]);
    LineEdit { text: out, from: shift(from), to: shift(to) }
}

/// Shift+Tab 反缩进变换：选区覆盖的每一行去掉一级前导缩进——行首制表符整体删一个，
/// 否则删至多一个缩进单位的空格；没有任何可删的行返回 None。
pub fn outdent_edit(text: &str, from: usize, to: usize) -> Option<LineEdit> {
    let mut removals: Vec<(usize, usize)> = Vec::new();
    for start in covered_line_starts(text, from, to) {
        let rest = &text[start..];
        let count = if rest.starts_with('\t') {
            1
        } else {
            let mut n = 0;
            while n < INDENT_UNIT.len() && rest.as_bytes().get(n) == Some(&b' ') {
                n += 1;
            }
            n
        };
        if count > 0 {
            removals.push((start, count));
        }
    }
    if removals.is_empty() {
        return None;
    }
    let map = |offset: usize| -> usize {
        let mut removed = 0;
        for &(start, count) in &removals {
            if start >= offset {
                break;
            }
            if offset < start + count {
                return start - removed;
            }
            removed += count;
        }
        offset - removed
    };
    let mut out = String::with_capacity(text.len());
    let mut prev = 0;
    for &(start, count) in &removals {
        out.push_str(&text[prev..start]);
        prev = start + count;
    }
    out.push_str(&text[prev..]);
    Some(LineEdit { text: out, from: map(from), to: map(to) })
}

/// 编辑面配色（与只读面同源派生）。
#[derive(Clone, Copy)]
pub struct EditPalette {
    pub text: Color,
    pub muted: Color,
    pub accent: Color,
    pub panel: Color,
    pub mark_bg: Color,
}

impl EditPalette {
    pub fn from_theme(theme: &Theme) -> Self {
        let p = theme.palette();
        Self {
            text: p.text,
            muted: p.text.scale_alpha(0.6),
            accent: p.primary,
            panel: p.text.scale_alpha(0.06),
            mark_bg: Color::from_rgba(1.0, 0.84, 0.28, 0.35),
        }
    }
}

/// 编辑面事件：部件只上报；正文/光标/选区真相与变更都在应用层。
#[derive(Debug, Clone)]
pub enum EditorEvent {
    /// 编辑面内按下或拖选：命中目标（落点由应用层换算）。
    Hit { target: HitTarget, extend: bool },
    /// 点击编辑面之外（应用层据此收起光标）。
    Blur,
    /// 光标落到源偏移（块内上下移动换行）。
    SetCaret { offset: usize, extend: bool },
    /// 插入文本（字符 / 上屏内容 / 粘贴）。
    Insert(String),
    Backspace,
    Delete,
    Newline,
    /// Tab 缩进 / Shift+Tab 反缩进。
    Indent,
    Outdent,
    /// 光标移动。
    Move { motion: Motion, extend: bool },
    Undo,
    Redo,
    SelectAll,
    /// 删除当前选区（剪切 / 键入前）。
    DeleteSelection,
    /// 输入法组合态（None = 结束）。
    Preedit(Option<String>),
}

/// 编辑面命中目标。
#[derive(Debug, Clone, Copy)]
pub enum HitTarget {
    /// 命中已揭示块（源码态）：按其源码排版精确换算落点。
    BlockText { index: usize, point: Point, width: f32 },
    /// 命中未揭示块（富渲染）：内部几何不可得，按行的上下半区吸到块首 / 块尾。
    BlockEdge { index: usize, upper: bool },
    /// 命中空行行：已按行首表换算为源偏移（第 k 根空行的行首）。
    GapLine { offset: usize },
    /// 命中内容区上方 / 下方空白：落到文首 / 文末。
    Start,
    End,
}

/// 光标移动种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
}

/// 源码态光标闪烁半周期。
const BLINK_MS: u128 = 500;

/// Unix 毫秒时间戳（空行光标的闪烁相位取墙钟：任何一帧重绘都能算出确定相位，
/// 不依赖部件状态与续帧链路）。
fn unix_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |t| t.as_millis())
}

type ParagraphT = <iced::Renderer as advanced_text::Renderer>::Paragraph;

/// 源码态单字符样式。
#[derive(Clone, Copy, Default)]
struct CharStyle {
    bold: bool,
    italic: bool,
    strike: bool,
    mono: bool,
    link: bool,
    code_bg: bool,
    mark: bool,
    muted: bool,
}

/// 行内片段的源区间。
fn inline_range(span: &InlineSpan) -> (usize, usize) {
    match span {
        InlineSpan::Text { from, to, .. }
        | InlineSpan::Strong { from, to, .. }
        | InlineSpan::Em { from, to, .. }
        | InlineSpan::Strike { from, to, .. }
        | InlineSpan::Highlight { from, to, .. }
        | InlineSpan::Code { from, to, .. }
        | InlineSpan::MathInline { from, to, .. }
        | InlineSpan::Comment { from, to }
        | InlineSpan::Link { from, to, .. }
        | InlineSpan::Autolink { from, to, .. }
        | InlineSpan::Wiki { from, to, .. }
        | InlineSpan::Image { from, to, .. }
        | InlineSpan::Tag { from, to, .. }
        | InlineSpan::FootnoteRef { from, to, .. }
        | InlineSpan::Mention { from, to, .. }
        | InlineSpan::Html { from, to, .. }
        | InlineSpan::HardBreak { from, to } => (*from, *to),
    }
}

/// 行内片段 → 样式段；容器片段的前后剩余区间按标记（弱化）处理。
fn collect_runs(list: &[InlineSpan], st: CharStyle, out: &mut Vec<(usize, usize, CharStyle)>) {
    for span in list {
        match span {
            InlineSpan::Strong { from, to, children } => {
                nest(*from, *to, children, CharStyle { bold: true, ..st }, out)
            }
            InlineSpan::Em { from, to, children } => {
                nest(*from, *to, children, CharStyle { italic: true, ..st }, out)
            }
            InlineSpan::Strike { from, to, children } => {
                nest(*from, *to, children, CharStyle { strike: true, ..st }, out)
            }
            InlineSpan::Highlight { from, to, children } => {
                nest(*from, *to, children, CharStyle { mark: true, ..st }, out)
            }
            InlineSpan::Code { from, to, .. } => {
                out.push((*from, *to, CharStyle { mono: true, code_bg: true, ..st }))
            }
            InlineSpan::Link { from, to, .. }
            | InlineSpan::Autolink { from, to, .. }
            | InlineSpan::Wiki { from, to, .. } => {
                out.push((*from, *to, CharStyle { link: true, ..st }))
            }
            InlineSpan::Tag { from, to, .. }
            | InlineSpan::FootnoteRef { from, to, .. }
            | InlineSpan::Mention { from, to, .. } => {
                out.push((*from, *to, CharStyle { link: true, ..st }))
            }
            InlineSpan::MathInline { from, to, .. } => {
                out.push((*from, *to, CharStyle { italic: true, muted: true, ..st }))
            }
            InlineSpan::Image { from, to, .. }
            | InlineSpan::Html { from, to, .. }
            | InlineSpan::Comment { from, to } => {
                out.push((*from, *to, CharStyle { muted: true, ..st }))
            }
            _ => {
                let (from, to) = inline_range(span);
                out.push((from, to, st));
            }
        }
    }
}

/// 容器片段：子片段内容按内层样式，前后未被内容覆盖处按标记（弱化）。
fn nest(
    from: usize,
    to: usize,
    children: &[InlineSpan],
    inner: CharStyle,
    out: &mut Vec<(usize, usize, CharStyle)>,
) {
    let marker = CharStyle { muted: true, ..CharStyle::default() };
    let (ca, cb) = match (children.first(), children.last()) {
        (Some(a), Some(b)) => (
            inline_range(a).0.clamp(from, to),
            inline_range(b).1.clamp(from, to),
        ),
        _ => (from, from),
    };
    if ca > from {
        out.push((from, ca, marker));
    }
    collect_runs(children, inner, out);
    if cb < to {
        out.push((cb, to, marker));
    }
}

/// 块内逐字符样式：按行内片段覆盖，未被覆盖处按标记（弱化）；下标与字符一一对应。
fn source_styles(source: &str, from: usize, to: usize, opts: &ParseOptions) -> Vec<CharStyle> {
    let mut runs: Vec<(usize, usize, CharStyle)> = Vec::new();
    let inline = parse_inline(source, from, to, opts);
    collect_runs(&inline, CharStyle::default(), &mut runs);
    runs.sort_by_key(|r| r.0);
    let mut out = Vec::new();
    for (idx, _) in source[from..to].char_indices() {
        let abs = from + idx;
        let st = runs
            .iter()
            .find(|(a, b, _)| *a <= abs && abs < *b)
            .map(|(_, _, s)| *s)
            .unwrap_or(CharStyle { muted: true, ..CharStyle::default() });
        out.push(st);
    }
    out
}

/// 构造逐字符样式的段落（单字符 span 便于按字符取几何）。
fn build_paragraph(
    content: &str,
    size: f32,
    line_height: f32,
    width: f32,
    styles: &[CharStyle],
    pal: EditPalette,
) -> ParagraphT {
    let mut spans: Vec<Span<'static, (), Font>> = Vec::with_capacity(content.chars().count());
    for (k, ch) in content.chars().enumerate() {
        let st = styles.get(k).copied().unwrap_or_default();
        let base = if st.mono { Font::MONOSPACE } else { Font::DEFAULT };
        let font = Font {
            weight: if st.bold { iced::font::Weight::Bold } else { base.weight },
            style: if st.italic { iced::font::Style::Italic } else { base.style },
            ..base
        };
        let mut span = Span::new(ch.to_string()).font(font);
        if st.strike {
            span = span.strikethrough(true);
        }
        if st.link {
            span = span.color(pal.accent).underline(true);
        } else if st.muted {
            span = span.color(pal.muted);
        }
        if st.code_bg {
            span = span.background(pal.panel);
        } else if st.mark {
            span = span.background(pal.mark_bg);
        }
        spans.push(span);
    }
    ParagraphT::with_spans(advanced_text::Text {
        content: spans.as_slice(),
        bounds: Size::new(width, f32::INFINITY),
        size: size.into(),
        line_height: advanced_text::LineHeight::Relative(line_height),
        font: Font::DEFAULT,
        align_x: advanced_text::Alignment::Left,
        align_y: alignment::Vertical::Top,
        shaping: advanced_text::Shaping::Advanced,
        wrapping: advanced_text::Wrapping::default(),
    })
}

/// 块内字节偏移 → 字符下标。
fn char_index(content: &str, byte: usize) -> usize {
    let mut b = byte.min(content.len());
    while !content.is_char_boundary(b) {
        b -= 1;
    }
    content[..b].chars().count()
}

/// 字符下标 → 光标坐标 (x, y, 行高像素)；空块退回行高像素。
fn caret_xy(p: &ParagraphT, count: usize, idx: usize, line_px: f32) -> (f32, f32, f32) {
    if idx < count {
        if let Some(r) = p.span_bounds(idx).into_iter().next() {
            return (r.x, r.y, r.height);
        }
    }
    if idx > 0 {
        if let Some(r) = p.span_bounds(idx - 1).into_iter().next() {
            return (r.x + r.width, r.y, r.height);
        }
    }
    (0.0, 0.0, line_px)
}

/// 选区矩形：同行的字符矩形合并。
fn selection_rects(p: &ParagraphT, from: usize, to: usize) -> Vec<Rectangle> {
    let mut out: Vec<Rectangle> = Vec::new();
    for i in from..to {
        let Some(r) = p.span_bounds(i).into_iter().next() else {
            continue;
        };
        match out.last_mut() {
            Some(last) if (last.y - r.y).abs() < 0.5 => {
                last.width = (r.x + r.width) - last.x;
                last.height = last.height.max(r.height);
            }
            _ => out.push(r),
        }
    }
    out
}

/// 最近字符下标：按逐字符矩形取与点位最近的字符。
/// 不用 `hit_test`——它的下标是所在行内的下标，多行块上无法直接换算成块内偏移。
fn char_at_point(p: &ParagraphT, count: usize, point: Point) -> usize {
    let mut best: Option<(usize, f32, f32, Rectangle)> = None;
    for i in 0..count {
        for r in p.span_bounds(i) {
            let dy = if point.y < r.y {
                r.y - point.y
            } else if point.y > r.y + r.height {
                point.y - (r.y + r.height)
            } else {
                0.0
            };
            let dx = if point.x < r.x {
                r.x - point.x
            } else if point.x > r.x + r.width {
                point.x - (r.x + r.width)
            } else {
                0.0
            };
            let better = match best {
                None => true,
                Some((_, bdy, bdx, _)) => dy < bdy - 0.5 || (dy < bdy + 0.5 && dx < bdx),
            };
            if better {
                best = Some((i, dy, dx, r));
            }
        }
    }
    best.map_or(0, |(i, _, _, _)| i)
}

/// 字符下标 → 内容内字节偏移。
fn byte_of_char(content: &str, index: usize) -> usize {
    content.char_indices().nth(index).map_or(content.len(), |(b, _)| b)
}

/// 命中换算：局部坐标 → 内容内字节偏移（按命中字符的左右半取边界）。
fn hit_offset(p: &ParagraphT, content: &str, point: Point) -> usize {
    let count = content.chars().count();
    let i = char_at_point(p, count, point);
    let split = p
        .span_bounds(i)
        .into_iter()
        .next()
        .is_some_and(|r| point.x > r.x + r.width / 2.0);
    byte_of_char(content, if split { i + 1 } else { i })
}

/// 远端协作者光标竖条宽（比本端光标 1.5 略宽，便于区分归属）。
const REMOTE_CARET_W: f32 = 2.0;
/// 远端名签与光标条的垂直间距（名签画在条上方）。
const REMOTE_LABEL_GAP: f32 = 2.0;

/// 远端光标（note_text 偏移）→ 本块绘制区间（块内字节偏移）：选区两端夹回块界，
/// 光标位仅在 head 落在块内时给出。head 在 frontmatter 区（< body_start）时整条不画；
/// 锚点落在 frontmatter 区时归正文零点，选区伸入正文的部分照画。
fn remote_span_in_block(
    head: usize,
    anchor: usize,
    body_start: usize,
    from: usize,
    to: usize,
) -> Option<(usize, usize, Option<usize>)> {
    let head = head.checked_sub(body_start)?;
    let anchor = anchor.saturating_sub(body_start);
    let lo = anchor.min(head).clamp(from, to);
    let hi = anchor.max(head).clamp(from, to);
    let caret = (from <= head && head <= to).then_some(head);
    (hi > lo || caret.is_some()).then_some((lo, hi, caret))
}

/// 源码态块部件：整块源码（标记显形）+ 光标/选区，托管输入法。
pub struct SourceBlock<'a> {
    pub from: usize,
    pub to: usize,
    pub text: &'a str,
    pub caret: usize,
    pub anchor: usize,
    pub preedit: Option<String>,
    pub source: SourceKind,
    pub pal: EditPalette,
    pub opts: &'a ParseOptions,
    /// 编辑面是否持有光标（应用层状态；为真才显示光标、闪动、接管按键与输入法）。
    pub focused: bool,
    /// 协作成员远端光标（note_text 字节偏移，绘制时换算到本块）。
    pub cursors: &'a [crate::RemoteCursorView],
    /// 正文在 note_text 中的起点（远端偏移 → 编辑面坐标换算基准）。
    pub body_start: usize,
    /// 撤销/重做键位（设置覆盖解析结果，每帧由壳传入）。
    pub bindings: NoteBindings,
}

/// 源码态排版口径：字号与行高倍数。
fn source_metrics(source: &SourceKind) -> (f32, f32) {
    match source {
        SourceKind::Prose { size, line_height } => (*size, *line_height),
        SourceKind::Mono { size } => (*size, 1.5),
    }
}

/// 帧内行高像素 = 字号 × 行高倍数。
fn source_line_px(source: &SourceKind) -> f32 {
    let (size, factor) = source_metrics(source);
    size * factor
}

#[derive(Default)]
struct SourceState {
    paragraph: Option<ParagraphT>,
    content: String,
    size: f32,
    line_height: f32,
    width: f32,
    /// 闪烁相位基准：光标或内容变化时重置（输入时保持可见）。
    blink_anchor: Option<Instant>,
    /// 已见光标位（变化即重置相位）。
    seen_caret: usize,
}

impl Widget<Message, Theme, iced::Renderer> for SourceBlock<'_> {
    fn tag(&self) -> widget::tree::Tag {
        widget::tree::Tag::of::<SourceState>()
    }

    fn state(&self) -> widget::tree::State {
        widget::tree::State::new(SourceState::default())
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Shrink)
    }

    fn layout(
        &mut self,
        tree: &mut widget::Tree,
        _renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::sized(limits, Length::Fill, Length::Shrink, |limits| {
            let width = limits.max().width;
            let (size, line_height) = source_metrics(&self.source);
            let to = self.to.min(self.text.len());
            let from = self.from.min(to);
            let content = &self.text[from..to];
            let state = tree.state.downcast_mut::<SourceState>();
            let stale = state.paragraph.is_none()
                || state.content.as_str() != content
                || state.size != size
                || state.line_height != line_height
                || (state.width - width).abs() > 0.5;
            if stale {
                let styles = source_styles(self.text, from, to, self.opts);
                state.paragraph = Some(build_paragraph(content, size, line_height, width, &styles, self.pal));
                state.content = content.to_string();
                state.size = size;
                state.line_height = line_height;
                state.width = width;
                // 内容变化即重置闪烁相位：输入时保持光标可见
                state.blink_anchor = Some(Instant::now());
            }
            let h = state
                .paragraph
                .as_ref()
                .map_or(size * line_height, |p| p.min_bounds().height);
            Size::new(width, h.max(size * line_height))
        })
    }

    fn draw(
        &self,
        tree: &widget::Tree,
        renderer: &mut iced::Renderer,
        _theme: &Theme,
        style: &iced::advanced::renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        if !bounds.intersects(viewport) {
            return;
        }
        let state = tree.state.downcast_ref::<SourceState>();
        let Some(p) = state.paragraph.as_ref() else {
            return;
        };
        let origin = bounds.position();
        let line_px = source_line_px(&self.source);
        let count = state.content.chars().count();

        // 选区（先画，压在正文下）
        let sel_from = self.anchor.min(self.caret).max(self.from).min(self.to) - self.from;
        let sel_to = self.anchor.max(self.caret).max(self.from).min(self.to) - self.from;
        if sel_to > sel_from {
            let tint = self.pal.accent.scale_alpha(0.25);
            for r in selection_rects(p, char_index(&state.content, sel_from), char_index(&state.content, sel_to)) {
                renderer.fill_quad(
                    Quad { bounds: Rectangle::new(origin + Vector::new(r.x, r.y), r.size()), ..Quad::default() },
                    tint,
                );
            }
        }

        // 正文
        iced::advanced::widget::text::draw(
            renderer,
            style,
            bounds,
            p,
            iced::advanced::widget::text::Style { color: None },
            viewport,
        );

        // 远端协作者光标（不随本端焦点隐藏；本端组合串与光标压其上）
        self.draw_remote_cursors(renderer, state, p, origin, bounds, count);

        if !self.focused {
            return;
        }
        let clamped = self.caret.clamp(self.from, self.to);
        let caret_char = char_index(&state.content, clamped - self.from);
        let (x, y, h) = caret_xy(p, count, caret_char, line_px);
        // 未上屏组合串画在光标处（组合期恒可见，不随光标闪烁）
        if let Some(pre) = self.preedit.as_deref().filter(|s| !s.is_empty()) {
            renderer.fill_text(
                advanced_text::Text {
                    content: pre.to_string(),
                    bounds: Size::new(f32::INFINITY, h),
                    size: state.size.into(),
                    line_height: advanced_text::LineHeight::Relative(state.line_height),
                    font: Font::DEFAULT,
                    align_x: advanced_text::Alignment::Left,
                    align_y: alignment::Vertical::Top,
                    shaping: advanced_text::Shaping::Auto,
                    wrapping: advanced_text::Wrapping::None,
                },
                origin + Vector::new(x, y),
                self.pal.muted,
                bounds,
            );
        }
        // 闪烁相位：半周期可见
        let on = state
            .blink_anchor
            .map_or(0u128, |anchor| anchor.elapsed().as_millis() / BLINK_MS)
            .is_multiple_of(2);
        if !on {
            return;
        }
        renderer.fill_quad(
            Quad {
                bounds: Rectangle::new(origin + Vector::new(x, y), Size::new(1.5, h)),
                ..Quad::default()
            },
            self.pal.text,
        );
    }

    fn mouse_interaction(
        &self,
        _tree: &widget::Tree,
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

    fn update(
        &mut self,
        tree: &mut widget::Tree,
        event: &Event,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let state = tree.state.downcast_mut::<SourceState>();
        if state.seen_caret != self.caret {
            state.seen_caret = self.caret;
            state.blink_anchor = Some(Instant::now());
        }
        if !self.focused {
            return;
        }

        match event {
            Event::InputMethod(ime) => handle_ime_event(ime, shell),
            Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, text, .. })
                if self.preedit.is_none() =>
            {
                // 剪贴板三键就地处理（部件内可直接读写信箱）
                if modifiers.control() {
                    if let Key::Character(c) = key {
                        let sel = selected_text(self.text, self.anchor, self.caret);
                        if handle_clipboard_key(c.to_lowercase().as_str(), sel, clipboard, shell) {
                            return;
                        }
                    }
                }
                if let Some(action) = key_action(key, *modifiers, text.as_deref(), &self.bindings) {
                    self.apply_key(action, state, shell);
                }
            }
            Event::Window(window::Event::RedrawRequested(_)) => {
                let count = state.content.chars().count();
                let clamped = self.caret.clamp(self.from, self.to);
                let caret_char = char_index(&state.content, clamped - self.from);
                let line_px = source_line_px(&self.source);
                if let Some(p) = state.paragraph.as_ref() {
                    let (x, y, h) = caret_xy(p, count, caret_char, line_px);
                    let ime: InputMethod<&str> = InputMethod::Enabled {
                        cursor: Rectangle::new(bounds.position() + Vector::new(x, y), Size::new(1.0, h)),
                        purpose: Purpose::Normal,
                        preedit: None,
                    };
                    shell.request_input_method(&ime);
                }
                // 闪烁节拍：下一个半周期续帧
                if let Some(anchor) = state.blink_anchor {
                    let phase = anchor.elapsed().as_millis() % BLINK_MS;
                    if phase < BLINK_MS {
                        shell.request_redraw_at(Instant::now() + Duration::from_millis((BLINK_MS - phase) as u64));
                    }
                }
            }
            _ => {}
        }
    }
}

impl SourceBlock<'_> {
    /// 远端协作者光标绘制：选区（成员色 25%，压正文上）+ 光标条 + 条上方名签。
    /// 名签上方无空间时落回块内首行；越出块右缘由块边界裁剪。`count` = 块内字符数（复用主绘制）。
    fn draw_remote_cursors(
        &self,
        renderer: &mut iced::Renderer,
        state: &SourceState,
        p: &ParagraphT,
        origin: Point,
        bounds: Rectangle,
        count: usize,
    ) {
        if self.cursors.is_empty() {
            return;
        }
        let line_px = source_line_px(&self.source);
        for cursor in self.cursors {
            let Some((lo, hi, caret)) =
                remote_span_in_block(cursor.head, cursor.anchor, self.body_start, self.from, self.to)
            else {
                continue;
            };
            let Some(color) = parse_hex_color(&cursor.color) else {
                continue;
            };
            if hi > lo {
                let tint = color.scale_alpha(0.25);
                for r in selection_rects(
                    p,
                    char_index(&state.content, lo - self.from),
                    char_index(&state.content, hi - self.from),
                ) {
                    renderer.fill_quad(
                        Quad {
                            bounds: Rectangle::new(origin + Vector::new(r.x, r.y), r.size()),
                            ..Quad::default()
                        },
                        tint,
                    );
                }
            }
            let Some(caret) = caret else { continue };
            let (x, y, h) = caret_xy(p, count, char_index(&state.content, caret - self.from), line_px);
            renderer.fill_quad(
                Quad {
                    bounds: Rectangle::new(origin + Vector::new(x, y), Size::new(REMOTE_CARET_W, h)),
                    ..Quad::default()
                },
                color,
            );
            renderer.fill_text(
                advanced_text::Text {
                    content: cursor.name.clone(),
                    bounds: Size::new(f32::INFINITY, f32::INFINITY),
                    size: pal::fs_micro().into(),
                    line_height: advanced_text::LineHeight::Relative(1.0),
                    font: semibold(),
                    align_x: advanced_text::Alignment::Left,
                    align_y: alignment::Vertical::Top,
                    shaping: advanced_text::Shaping::Auto,
                    wrapping: advanced_text::Wrapping::None,
                },
                origin + Vector::new(x, (y - pal::fs_micro() - REMOTE_LABEL_GAP).max(0.0)),
                color,
                bounds,
            );
        }
    }

    /// 已解析按键 → 事件上报。
    fn apply_key(
        &self,
        action: KeyAction,
        state: &mut SourceState,
        shell: &mut Shell<'_, Message>,
    ) {
        let send = |shell: &mut Shell<'_, Message>, e: EditorEvent| {
            shell.publish(Message::Editor(e));
            shell.capture_event();
            shell.request_redraw();
        };
        match action {
            KeyAction::Event(e) => send(shell, e),
            KeyAction::LineUp(extend) => {
                let line_px = source_line_px(&self.source);
                let count = state.content.chars().count();
                let idx = char_index(&state.content, self.caret.clamp(self.from, self.to) - self.from);
                let moved = match state.paragraph.as_ref() {
                    Some(p) => {
                        let (x, y, _) = caret_xy(p, count, idx, line_px);
                        let ty = y - line_px;
                        if ty < 0.0 {
                            None
                        } else {
                            let ci = char_at_point(p, count, Point::new(x, ty + line_px / 2.0));
                            Some(self.from + byte_of_char(&state.content, ci))
                        }
                    }
                    None => None,
                };
                match moved {
                    Some(off) => send(shell, EditorEvent::SetCaret { offset: off, extend }),
                    None => send(shell, EditorEvent::Move { motion: Motion::Up, extend }),
                }
            }
            KeyAction::LineDown(extend) => {
                let line_px = source_line_px(&self.source);
                let count = state.content.chars().count();
                let idx = char_index(&state.content, self.caret.clamp(self.from, self.to) - self.from);
                let moved = match state.paragraph.as_ref() {
                    Some(p) => {
                        let (x, y, _) = caret_xy(p, count, idx, line_px);
                        let ty = y + line_px;
                        if ty > p.min_bounds().height {
                            None
                        } else {
                            let ci = char_at_point(p, count, Point::new(x, ty + line_px / 2.0));
                            Some(self.from + byte_of_char(&state.content, ci))
                        }
                    }
                    None => None,
                };
                match moved {
                    Some(off) => send(shell, EditorEvent::SetCaret { offset: off, extend }),
                    None => send(shell, EditorEvent::Move { motion: Motion::Down, extend }),
                }
            }
        }
    }
}

/// 已解析按键动作。
enum KeyAction {
    Event(EditorEvent),
    LineUp(bool),
    LineDown(bool),
}

/// 选区文本（源区间，两端夹回文本长度）。
fn selected_text(text: &str, anchor: usize, caret: usize) -> String {
    let a = anchor.min(caret).min(text.len());
    let b = anchor.max(caret).min(text.len());
    if b > a {
        text[a..b].to_string()
    } else {
        String::new()
    }
}

/// Ctrl+C/X/V 就地处理（C 复制非空选区；X 复制并删选区；V 粘贴）。
/// 剪贴板经部件入参读写，避免应用层往返。返回是否消费该按键。
fn handle_clipboard_key(
    ch: &str,
    sel: String,
    clipboard: &mut dyn Clipboard,
    shell: &mut Shell<'_, Message>,
) -> bool {
    match ch {
        "c" => {
            if !sel.is_empty() {
                clipboard.write(ClipboardKind::Standard, sel);
            }
            shell.capture_event();
            true
        }
        "x" => {
            if !sel.is_empty() {
                clipboard.write(ClipboardKind::Standard, sel);
                shell.publish(Message::Editor(EditorEvent::DeleteSelection));
                shell.request_redraw();
            }
            shell.capture_event();
            true
        }
        "v" => {
            if let Some(t) = clipboard.read(ClipboardKind::Standard) {
                shell.publish(Message::Editor(EditorEvent::Insert(t)));
                shell.capture_event();
                shell.request_redraw();
            }
            true
        }
        _ => false,
    }
}

/// 输入法事件上报（组合态 / 上屏 / 关闭），块内与空行光标共用。
fn handle_ime_event(ime: &ImeEvent, shell: &mut Shell<'_, Message>) {
    match ime {
        ImeEvent::Closed => {
            shell.publish(Message::Editor(EditorEvent::Preedit(None)));
            shell.request_redraw();
        }
        ImeEvent::Preedit(content, _) => {
            shell.publish(Message::Editor(EditorEvent::Preedit(Some(content.clone()))));
            shell.capture_event();
            shell.request_redraw();
        }
        ImeEvent::Commit(content) => {
            shell.publish(Message::Editor(EditorEvent::Insert(content.clone())));
            shell.publish(Message::Editor(EditorEvent::Preedit(None)));
            shell.capture_event();
            shell.request_redraw();
        }
        ImeEvent::Opened => {}
    }
}

/// 按键映射（组合期由输入法消化，不进入本函数）。撤销/重做查注入绑定表
/// （键位可重映射，修饰集合精确匹配）；命中即消费，未命中维持原事件。
fn key_action(
    key: &Key,
    modifiers: keyboard::Modifiers,
    text: Option<&str>,
    bindings: &NoteBindings,
) -> Option<KeyAction> {
    let extend = modifiers.shift();
    if modifiers.control() || modifiers.logo() {
        if commands::matches(&bindings.undo, modifiers, key) {
            return Some(KeyAction::Event(EditorEvent::Undo));
        }
        if commands::matches(&bindings.redo, modifiers, key) {
            return Some(KeyAction::Event(EditorEvent::Redo));
        }
        // 重做别名：redo 未被重定义时 mod+shift+z 沿用（既有习惯键位）
        if bindings.redo_alias && modifiers.shift() && commands::key_text(key) == "z" {
            return Some(KeyAction::Event(EditorEvent::Redo));
        }
        let ch = match key {
            Key::Character(c) => c.to_lowercase(),
            _ => return None,
        };
        return match ch.as_str() {
            "a" => Some(KeyAction::Event(EditorEvent::SelectAll)),
            _ => None,
        };
    }
    match key {
        Key::Named(Named::ArrowLeft) => Some(KeyAction::Event(EditorEvent::Move {
            motion: Motion::Left,
            extend,
        })),
        Key::Named(Named::ArrowRight) => Some(KeyAction::Event(EditorEvent::Move {
            motion: Motion::Right,
            extend,
        })),
        Key::Named(Named::ArrowUp) => Some(KeyAction::LineUp(extend)),
        Key::Named(Named::ArrowDown) => Some(KeyAction::LineDown(extend)),
        Key::Named(Named::Home) => Some(KeyAction::Event(EditorEvent::Move { motion: Motion::Home, extend })),
        Key::Named(Named::End) => Some(KeyAction::Event(EditorEvent::Move { motion: Motion::End, extend })),
        Key::Named(Named::Backspace) => Some(KeyAction::Event(EditorEvent::Backspace)),
        Key::Named(Named::Delete) => Some(KeyAction::Event(EditorEvent::Delete)),
        Key::Named(Named::Enter) => Some(KeyAction::Event(EditorEvent::Newline)),
        Key::Named(Named::Tab) if modifiers.shift() => Some(KeyAction::Event(EditorEvent::Outdent)),
        Key::Named(Named::Tab) => Some(KeyAction::Event(EditorEvent::Indent)),
        Key::Character(_) => text
            .filter(|t| !t.is_empty())
            .map(|t| KeyAction::Event(EditorEvent::Insert(t.to_string()))),
        _ => None,
    }
}

/// 编辑面容器的一行：块行或空行行。
pub(crate) enum SurfaceRow<'a> {
    Block {
        index: usize,
        /// 该行渲染为源码态（已揭示）：落点恒可按源码排版精确换算。
        editable: bool,
        /// 富渲染与源码渲染几何一致（未揭示时也可精确换算）。
        exact: bool,
        covered: bool,
        element: Element<'a, Message>,
    },
    Gap {
        /// 空行在源文本中的区间（上一块末 → 下一块首）。
        from: usize,
        to: usize,
        /// 各空行的行首偏移（行数 = 长度；行高 = 行总高 / 行数）。
        line_starts: Vec<usize>,
        /// 被选区覆盖的空行序号半开区间（k0, k1）。
        covered: (usize, usize),
        element: Element<'a, Message>,
    },
}

impl<'a> SurfaceRow<'a> {
    fn element(&self) -> &Element<'a, Message> {
        match self {
            SurfaceRow::Block { element, .. } | SurfaceRow::Gap { element, .. } => element,
        }
    }

    fn element_mut(&mut self) -> &mut Element<'a, Message> {
        match self {
            SurfaceRow::Block { element, .. } | SurfaceRow::Gap { element, .. } => element,
        }
    }
}

/// 空行区间内各空行的行首偏移（跳过上一块的终止行；空行内可含空白等字符，
/// 不假设每行一字节）。
pub(crate) fn gap_line_starts(source: &str, from: usize, to: usize) -> Vec<usize> {
    let mut out = Vec::new();
    let mut pos = from;
    for chunk in source[from..to].split_inclusive('\n') {
        if pos > from {
            out.push(pos);
        }
        pos += chunk.len();
    }
    out
}

/// 文末空白区的行首表：空白行行首 + 文本以换行结尾时的文末虚拟行（len）。
pub(crate) fn tail_blank_starts(source: &str, from: usize) -> Vec<usize> {
    let mut starts = gap_line_starts(source, from, source.len());
    if source.ends_with('\n') {
        starts.push(source.len());
    }
    starts
}

/// 文首空白区（首块之前）的各空白行行首：第一行也是空白行，不跳过；
/// 末尾不带换行的片段是首块的行首缩进，不是空白行。
pub(crate) fn edge_blank_starts(source: &str, from: usize, to: usize) -> Vec<usize> {
    let mut out = Vec::new();
    let mut pos = from;
    for chunk in source[from..to].split_inclusive('\n') {
        if chunk.ends_with('\n') {
            out.push(pos);
        }
        pos += chunk.len();
    }
    out
}

/// 空行行内单行高度（行总高 / 行数，防零防负）。
fn gap_line_height(height: f32, lines: usize) -> f32 {
    if lines > 0 {
        (height / lines as f32).max(1.0)
    } else {
        height.max(1.0)
    }
}

/// 空行行命中换算：行内纵向位置 → 第 k 根空行 → 该行行首源偏移。超出按最后一根吸附。
fn gap_hit_offset(line_starts: &[usize], local_y: f32, height: f32) -> usize {
    let lines = line_starts.len().max(1);
    let k = ((local_y / gap_line_height(height, lines)) as usize).min(lines - 1);
    line_starts.get(k).copied().unwrap_or(0)
}

/// 光标所在的空行序号：行首 ≤ 光标的最后一根，且光标未越过该行行尾。
fn gap_caret_line(line_starts: &[usize], to: usize, offset: usize) -> Option<usize> {
    let k = line_starts.iter().rposition(|&s| s <= offset)?;
    let end = line_starts.get(k + 1).copied().unwrap_or(to);
    (offset < end).then_some(k)
}

/// 空行选区覆盖：第 k 根空行占字节 [行首, 下一行首或 to)，选区触及即覆盖该行。
/// 返回覆盖的空行序号半开区间；无覆盖返回 (0, 0)。
pub(crate) fn gap_covered_lines(
    line_starts: &[usize],
    to: usize,
    sel_from: usize,
    sel_to: usize,
) -> (usize, usize) {
    if sel_to <= sel_from {
        return (0, 0);
    }
    let (mut k0, mut k1) = (usize::MAX, 0);
    for (k, &s) in line_starts.iter().enumerate() {
        let end = line_starts.get(k + 1).copied().unwrap_or(to);
        if sel_from < end && sel_to > s {
            k0 = k0.min(k);
            k1 = k1.max(k + 1);
        }
    }
    if k0 == usize::MAX { (0, 0) } else { (k0, k1) }
}

/// 上下移动的目标偏移：块内跨到相邻块首 / 末，末块向下 / 首块向上进入毗邻的
/// 文末 / 文首空白区；空白区光标按 `blank_starts`（该区各空白行行首）逐行移动，
/// 越过端点落到相邻块。无路可走返回 None。
pub(crate) fn vertical_target(
    caret: usize,
    spans: &[(usize, usize)],
    blank_starts: &[usize],
    forward: bool,
) -> Option<usize> {
    if let Some(i) = spans.iter().position(|&(f, t)| caret >= f && caret <= t) {
        return if forward {
            match blank_starts.iter().find(|&&s| s > caret) {
                Some(&s) => Some(s),
                None => spans.get(i + 1).map(|&(f, _)| f),
            }
        } else {
            match blank_starts.iter().rposition(|&s| s < caret) {
                Some(k) => Some(blank_starts[k]),
                None => Some(spans[i.checked_sub(1)?].1),
            }
        };
    }
    match spans.iter().rposition(|&(f, _)| f <= caret) {
        // 块间 / 文末空白区：逐行走，越过端点落相邻块
        Some(i) => {
            let prev_to = spans[i].1;
            if forward {
                match blank_starts.iter().find(|&&s| s > caret) {
                    Some(&s) => Some(s),
                    None => spans.get(i + 1).map(|&(f, _)| f),
                }
            } else {
                match blank_starts
                    .iter()
                    .rposition(|&s| s <= caret)
                    .and_then(|k| k.checked_sub(1))
                    .and_then(|k| blank_starts.get(k))
                {
                    Some(&s) => Some(s),
                    None => Some(prev_to),
                }
            }
        }
        // 文首空白区：向下逐行 / 进首块，向上逐行回退
        None => {
            if forward {
                match blank_starts.iter().find(|&&s| s > caret) {
                    Some(&s) => Some(s),
                    None => spans.first().map(|&(f, _)| f),
                }
            } else {
                blank_starts
                    .iter()
                    .rposition(|&s| s < caret)
                    .map(|k| blank_starts[k])
            }
        }
    }
}

/// 编辑面容器：纵向排布块行与空行行，统一承接鼠标——按下/拖选按行换算落点、
/// 点内容区空白落到文末、点编辑面之外收起光标；块间空行的光标由容器自绘，
/// 键盘 / 输入法 / 剪贴板也由容器托管（空行光标不属任何块）。
pub struct EditSurface<'a> {
    rows: Vec<SurfaceRow<'a>>,
    pal: EditPalette,
    /// 正文真相（应用层持有）：空行光标的剪贴板读写用。
    text: &'a str,
    caret: usize,
    anchor: usize,
    preedit: Option<String>,
    focused: bool,
    /// 撤销/重做键位（空行光标路径的按键匹配源）。
    bindings: NoteBindings,
}

/// 编辑面容器的持久状态（跨帧存活：拖选在途）。
#[derive(Default)]
struct SurfaceState {
    dragging: bool,
}

/// 内容区四周留白（与只读档一致）。
const SURFACE_PAD: f32 = 16.0;
/// 空行行内自绘光标的竖条高度。
const GAP_CARET_H: f32 = 20.0;

/// 命中行结果。
enum RowHit {
    Row(usize, Point, f32, f32),
    Above,
    Below,
}

impl<'a> EditSurface<'a> {
    pub(crate) fn new(
        rows: Vec<SurfaceRow<'a>>,
        pal: EditPalette,
        text: &'a str,
        caret: usize,
        anchor: usize,
        preedit: Option<String>,
        focused: bool,
        bindings: NoteBindings,
    ) -> Self {
        Self { rows, pal, text, caret, anchor, preedit, focused, bindings }
    }

    /// 命中行：返回 (行号, 行内局部坐标, 行宽, 行高)；行间无空隙，仅区分上下越界。
    fn row_at(layout: Layout<'_>, point: Point) -> RowHit {
        let mut top = f32::MAX;
        let mut bottom = f32::MIN;
        for child in layout.children() {
            let b = child.bounds();
            top = top.min(b.y);
            bottom = bottom.max(b.y + b.height);
        }
        if top == f32::MAX || point.y < top {
            return RowHit::Above;
        }
        if point.y > bottom {
            return RowHit::Below;
        }
        let mut best: Option<(usize, Rectangle)> = None;
        for (i, child) in layout.children().enumerate() {
            let b = child.bounds();
            if b.contains(point) {
                return RowHit::Row(i, Point::new(point.x - b.x, point.y - b.y), b.width, b.height);
            }
            if point.y >= b.y && point.y <= b.y + b.height {
                best = Some((i, b));
            }
        }
        match best {
            Some((i, b)) => RowHit::Row(i, Point::new(point.x - b.x, point.y - b.y), b.width, b.height),
            None => RowHit::Below,
        }
    }

    /// 命中 → 目标。已揭示块与几何一致的富块可精确换算；其余富块内部几何不可得，
    /// 按行的上下半区吸到块首 / 块尾（该块的选区本就只画块级底色，吸附与所见一致）。
    fn target_of(&self, hit: RowHit) -> HitTarget {
        match hit {
            RowHit::Row(i, local, width, height) => match &self.rows[i] {
                SurfaceRow::Block { index, editable, exact, .. } => {
                    if *editable || *exact {
                        HitTarget::BlockText { index: *index, point: local, width }
                    } else {
                        HitTarget::BlockEdge { index: *index, upper: local.y < height / 2.0 }
                    }
                }
                SurfaceRow::Gap { line_starts, .. } => HitTarget::GapLine {
                    offset: gap_hit_offset(line_starts, local.y, height),
                },
            },
            RowHit::Above => HitTarget::Start,
            RowHit::Below => HitTarget::End,
        }
    }

    /// 光标所在的空行行序号。开区间端点归相邻块；文首行 from = 0 之上无相邻块，
    /// 端点 0 由该行自己托管。
    fn gap_row_at(&self, offset: usize) -> Option<usize> {
        self.rows.iter().position(|r| {
            matches!(r, SurfaceRow::Gap { from, to, .. }
                if offset < *to && (offset > *from || *from == 0))
        })
    }

    /// 空行光标的按键上报（无块内几何，上下移动直接跨块）。
    fn apply_key(&self, action: KeyAction, shell: &mut Shell<'_, Message>) {
        let send = |shell: &mut Shell<'_, Message>, e: EditorEvent| {
            shell.publish(Message::Editor(e));
            shell.capture_event();
            shell.request_redraw();
        };
        match action {
            KeyAction::Event(e) => send(shell, e),
            KeyAction::LineUp(extend) => send(shell, EditorEvent::Move { motion: Motion::Up, extend }),
            KeyAction::LineDown(extend) => {
                send(shell, EditorEvent::Move { motion: Motion::Down, extend })
            }
        }
    }

    /// 空行光标的输入法定位与闪烁续帧（块内光标由 SourceBlock 处理）。
    fn gap_ime(&self, layout: Layout<'_>, shell: &mut Shell<'_, Message>) {
        if !self.focused {
            return;
        }
        let Some(row_index) = self.gap_row_at(self.caret) else {
            return;
        };
        let Some(SurfaceRow::Gap { to, line_starts, .. }) = self.rows.get(row_index) else {
            return;
        };
        let Some(b) = layout.children().nth(row_index).map(|l| l.bounds()) else {
            return;
        };
        let line_h = gap_line_height(b.height, line_starts.len());
        let k = gap_caret_line(line_starts, *to, self.caret).unwrap_or(0);
        let y = b.y + k as f32 * line_h + (line_h - GAP_CARET_H).max(0.0) * 0.5;
        let ime: InputMethod<&str> = InputMethod::Enabled {
            cursor: Rectangle::new(Point::new(b.x, y), Size::new(1.0, GAP_CARET_H)),
            purpose: Purpose::Normal,
            preedit: None,
        };
        shell.request_input_method(&ime);
        // 闪烁节拍：按墙钟对齐下一个半周期边界
        let phase = unix_millis() % BLINK_MS;
        shell.request_redraw_at(Instant::now() + Duration::from_millis((BLINK_MS - phase) as u64));
    }
}

impl Widget<Message, Theme, iced::Renderer> for EditSurface<'_> {
    fn tag(&self) -> widget::tree::Tag {
        widget::tree::Tag::of::<SurfaceState>()
    }

    fn state(&self) -> widget::tree::State {
        widget::tree::State::new(SurfaceState::default())
    }

    fn children(&self) -> Vec<widget::Tree> {
        self.rows.iter().map(|r| widget::Tree::new(r.element().as_widget())).collect()
    }

    fn diff(&self, tree: &mut widget::Tree) {
        // 必须走 Tree::diff：标签不同时由它重建子树状态（行元素会在只读块与源码块之间切换）
        tree.diff_children_custom(
            &self.rows,
            |state, row| state.diff(row.element().as_widget()),
            |row| widget::Tree::new(row.element().as_widget()),
        );
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Shrink)
    }

    fn layout(
        &mut self,
        tree: &mut widget::Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let limits = limits.width(Length::Fill);
        let child_limits = limits.loose().shrink(Padding {
            top: SURFACE_PAD,
            bottom: SURFACE_PAD,
            left: SURFACE_PAD,
            right: SURFACE_PAD,
        });
        let mut nodes = Vec::with_capacity(self.rows.len());
        let mut y = 0.0f32;
        for (i, row) in self.rows.iter_mut().enumerate() {
            let node = row
                .element_mut()
                .as_widget_mut()
                .layout(&mut tree.children[i], renderer, &child_limits);
            let height = node.size().height;
            nodes.push(node.move_to(Point::new(SURFACE_PAD, SURFACE_PAD + y)));
            y += height;
        }
        let available = limits.max();
        layout::Node::with_children(Size::new(available.width, y + SURFACE_PAD * 2.0), nodes)
    }

    fn draw(
        &self,
        tree: &widget::Tree,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        style: &iced::advanced::renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let tint = self.pal.accent.scale_alpha(0.25);
        // 闪烁相位：半周期可见（墙钟取模，与帧时机无关）
        let blink_on = unix_millis() / BLINK_MS % 2 == 0;
        for ((row, state), child_layout) in self.rows.iter().zip(&tree.children).zip(layout.children()) {
            row.element()
                .as_widget()
                .draw(state, renderer, theme, style, child_layout, cursor, viewport);
            let bounds = child_layout.bounds();
            if !bounds.intersects(viewport) {
                continue;
            }
            match row {
                SurfaceRow::Block { covered: true, .. } => {
                    renderer.fill_quad(Quad { bounds, ..Quad::default() }, tint);
                }
                // 空行选区底色按空行粒度画（第 k 根空行对应其换行符字节）
                SurfaceRow::Gap { line_starts, covered: (k0, k1), .. } if k1 > k0 => {
                    let line_h = gap_line_height(bounds.height, line_starts.len());
                    for k in *k0..*k1 {
                        renderer.fill_quad(
                            Quad {
                                bounds: Rectangle::new(
                                    Point::new(bounds.x, bounds.y + k as f32 * line_h),
                                    Size::new(bounds.width, line_h),
                                ),
                                ..Quad::default()
                            },
                            tint,
                        );
                    }
                }
                _ => {}
            }
            // 空行行的光标与预编辑：空行不属任何块，由容器自绘。
            // 区间端点归相邻块的光标（文首行的 0 端点无相邻块，归自己），避免同帧画两根。
            if let SurfaceRow::Gap { from, to, line_starts, .. } = row {
                if self.focused
                    && self.caret < *to
                    && (self.caret > *from || *from == 0)
                {
                    let line_h = gap_line_height(bounds.height, line_starts.len());
                    let k = gap_caret_line(line_starts, *to, self.caret).unwrap_or(0);
                    let y = bounds.y + k as f32 * line_h + (line_h - GAP_CARET_H).max(0.0) * 0.5;
                    if blink_on {
                        renderer.fill_quad(
                            Quad {
                                bounds: Rectangle::new(Point::new(bounds.x, y), Size::new(1.5, GAP_CARET_H)),
                                ..Quad::default()
                            },
                            self.pal.text,
                        );
                    }
                    // 未上屏组合串画在光标处
                    if let Some(pre) = self.preedit.as_deref().filter(|s| !s.is_empty()) {
                        renderer.fill_text(
                            advanced_text::Text {
                                content: pre.to_string(),
                                bounds: Size::new(f32::INFINITY, line_h),
                                size: crate::markdown_view::base_size().into(),
                                line_height: advanced_text::LineHeight::Relative(
                                    crate::markdown_view::LINE_HEIGHT,
                                ),
                                font: Font::DEFAULT,
                                align_x: advanced_text::Alignment::Left,
                                align_y: alignment::Vertical::Top,
                                shaping: advanced_text::Shaping::Auto,
                                wrapping: advanced_text::Wrapping::None,
                            },
                            Point::new(bounds.x + 2.0, y),
                            self.pal.muted,
                            bounds,
                        );
                    }
                }
            }
        }
    }

    fn update(
        &mut self,
        tree: &mut widget::Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        for ((row, state), child_layout) in
            self.rows.iter_mut().zip(&mut tree.children).zip(layout.children())
        {
            row.element_mut().as_widget_mut().update(
                state,
                event,
                child_layout,
                cursor,
                renderer,
                clipboard,
                shell,
                viewport,
            );
        }
        // 子控件（链接 / 勾选框）已接管则不再换算落点
        if shell.is_event_captured() {
            return;
        }
        let bounds = layout.bounds();
        let state = tree.state.downcast_mut::<SurfaceState>();
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                // 行命中要拿绝对坐标（行 bounds 是绝对坐标）；position_in 是相对量，不能混用
                match cursor.position().filter(|p| bounds.contains(*p)) {
                    Some(p) => {
                        state.dragging = true;
                        let target = self.target_of(Self::row_at(layout, p));
                        shell.publish(Message::Editor(EditorEvent::Hit { target, extend: false }));
                        shell.capture_event();
                        shell.request_redraw();
                    }
                    None => shell.publish(Message::Editor(EditorEvent::Blur)),
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) if state.dragging => {
                // 事件里的 position 是窗口坐标，而滚动容器传下来的 cursor 已平移为
                // 与行 bounds 同基准的内容坐标，必须用后者，否则滚动后拖选整体错位；
                // 拖出边界时 cursor 变 Levitating（保持最后位置），land() 取回
                let Some(p) = cursor.land().position() else {
                    return;
                };
                // 越界拖选：点夹回内容区，仍按最近行换算
                let clamped = Point::new(
                    p.x.clamp(bounds.x, bounds.x + bounds.width),
                    p.y.clamp(bounds.y, bounds.y + bounds.height),
                );
                let target = self.target_of(Self::row_at(layout, clamped));
                shell.publish(Message::Editor(EditorEvent::Hit { target, extend: true }));
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                state.dragging = false;
            }
            Event::Window(window::Event::RedrawRequested(_)) => {
                self.gap_ime(layout, shell);
            }
            // 空行光标的输入法与键盘托管（块内光标由 SourceBlock 处理，两者互斥）
            Event::InputMethod(ime) if self.focused && self.gap_row_at(self.caret).is_some() => {
                handle_ime_event(ime, shell);
            }
            Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, text, .. })
                if self.focused
                    && self.preedit.is_none()
                    && self.gap_row_at(self.caret).is_some() =>
            {
                if modifiers.control() {
                    if let Key::Character(c) = key {
                        let sel = selected_text(self.text, self.anchor, self.caret);
                        if handle_clipboard_key(c.to_lowercase().as_str(), sel, clipboard, shell) {
                            return;
                        }
                    }
                }
                if let Some(action) = key_action(key, *modifiers, text.as_deref(), &self.bindings) {
                    self.apply_key(action, shell);
                }
            }
            _ => {}
        }
    }

    fn operate(
        &mut self,
        tree: &mut widget::Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn widget::Operation,
    ) {
        for ((row, state), child_layout) in
            self.rows.iter_mut().zip(&mut tree.children).zip(layout.children())
        {
            row.element_mut()
                .as_widget_mut()
                .operate(state, child_layout, renderer, operation);
        }
    }

    fn mouse_interaction(
        &self,
        tree: &widget::Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        let child = self
            .rows
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .map(|((row, state), child_layout)| {
                row.element()
                    .as_widget()
                    .mouse_interaction(state, child_layout, cursor, viewport, renderer)
            })
            .max()
            .unwrap_or_default();
        if child != mouse::Interaction::None {
            child
        } else if cursor.is_over(layout.bounds()) {
            mouse::Interaction::Text
        } else {
            mouse::Interaction::None
        }
    }
}

impl<'a> From<EditSurface<'a>> for Element<'a, Message> {
    fn from(surface: EditSurface<'a>) -> Self {
        Element::new(surface)
    }
}

impl<'a> From<SourceBlock<'a>> for Element<'a, Message> {
    fn from(block: SourceBlock<'a>) -> Self {
        Element::new(block)
    }
}

/// 命中换算：在已揭示块内按源码排版求落点（块外或富渲染块不走此入口）。
pub(crate) fn offset_in_block(
    source: &str,
    from: usize,
    to: usize,
    source_kind: &SourceKind,
    width: f32,
    point: Point,
    opts: &ParseOptions,
) -> usize {
    let (size, line_height) = source_metrics(source_kind);
    let to = to.min(source.len());
    let from = from.min(to);
    let content = &source[from..to];
    let styles = source_styles(source, from, to, opts);
    let pal = EditPalette {
        text: Color::BLACK,
        muted: Color::BLACK,
        accent: Color::BLACK,
        panel: Color::TRANSPARENT,
        mark_bg: Color::TRANSPARENT,
    };
    let paragraph = build_paragraph(content, size, line_height, width.max(1.0), &styles, pal);
    from + hit_offset(&paragraph, content, point)
}

/// 编辑面状态（应用层持有）：光标/选区/活动块/撤销栈/组合态。
#[derive(Default)]
pub struct EditorState {
    pub caret: usize,
    pub anchor: usize,
    /// 当前源码态块序号（None = 无活动块，全篇按只读渲染）。
    pub active: Option<usize>,
    /// 输入法未上屏文本。
    pub preedit: Option<String>,
    /// 编辑面是否持有光标（点击编辑面内为真，点编辑面外为假）。
    pub focused: bool,
    undo: Vec<(String, usize, usize)>,
    redo: Vec<(String, usize, usize)>,
}

impl EditorState {
    /// 编辑前快照（用于撤销）。
    pub fn snapshot(&mut self, text: &str) {
        self.undo.push((text.to_string(), self.caret, self.anchor));
        if self.undo.len() > 200 {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// 有序选区 (from, to)。
    pub fn selection(&self) -> (usize, usize) {
        (self.anchor.min(self.caret), self.anchor.max(self.caret))
    }

    /// 撤销：返回 (正文, 光标, 锚点)。
    pub fn undo(&mut self, text: &str) -> Option<(String, usize, usize)> {
        let (prev, caret, anchor) = self.undo.pop()?;
        self.redo.push((text.to_string(), self.caret, self.anchor));
        Some((prev, caret, anchor))
    }

    /// 重做：返回 (正文, 光标, 锚点)。
    pub fn redo(&mut self, text: &str) -> Option<(String, usize, usize)> {
        let (next, caret, anchor) = self.redo.pop()?;
        self.undo.push((text.to_string(), self.caret, self.anchor));
        Some((next, caret, anchor))
    }

    /// 载入新文件后重置。
    pub fn reset(&mut self, caret: usize) {
        self.caret = caret;
        self.anchor = caret;
        self.active = None;
        self.preedit = None;
        self.focused = false;
        self.undo.clear();
        self.redo.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_enter_continues_bullet() {
        let text = "- 甲";
        let (out, cursor) = list_enter_edit(text, text.len()).unwrap();
        assert_eq!(out, "- 甲\n- ");
        assert_eq!(cursor, text.len() + 3);
    }

    #[test]
    fn list_enter_increments_ordinal_and_keeps_delimiter() {
        let text = "2) 甲";
        let (out, cursor) = list_enter_edit(text, text.len()).unwrap();
        assert_eq!(out, "2) 甲\n3) ");
        assert_eq!(cursor, text.len() + 4);
    }

    #[test]
    fn list_enter_continues_task_item_as_unchecked() {
        let text = "- [x] 已完成";
        let (out, cursor) = list_enter_edit(text, text.len()).unwrap();
        assert_eq!(out, "- [x] 已完成\n- [ ] ");
        assert_eq!(cursor, text.len() + 7);
    }

    #[test]
    fn list_enter_keeps_indent() {
        let text = "  - 甲";
        let (out, cursor) = list_enter_edit(text, text.len()).unwrap();
        assert_eq!(out, "  - 甲\n  - ");
        assert_eq!(cursor, text.len() + 5);
    }

    #[test]
    fn list_enter_splits_item_at_caret() {
        let (out, cursor) = list_enter_edit("- 甲乙", 5).unwrap();
        assert_eq!(out, "- 甲\n- 乙");
        assert_eq!(cursor, 8);
    }

    #[test]
    fn list_enter_continues_empty_item() {
        let text = "- 甲\n- ";
        let (out, cursor) = list_enter_edit(text, text.len()).unwrap();
        assert_eq!(out, "- 甲\n- \n- ");
        assert_eq!(cursor, text.len() + 3);
    }

    #[test]
    fn list_enter_indented_empty_item() {
        let text = "  - ";
        let (out, cursor) = list_enter_edit(text, text.len()).unwrap();
        assert_eq!(out, "  - \n  - ");
        assert_eq!(cursor, text.len() + 5);
    }

    #[test]
    fn list_enter_rejects_non_list_and_marker_prefix() {
        assert!(list_enter_edit("普通段落", 4).is_none());
        assert!(list_enter_edit("- 甲\n续行", 7).is_none());
        assert!(list_enter_edit("- 甲", 0).is_none());
        assert!(list_enter_edit("- 甲", 1).is_none());
        assert!(list_enter_edit("  1. 甲", 3).is_none());
    }

    #[test]
    fn list_backspace_removes_empty_marker() {
        assert_eq!(list_backspace_edit("- 甲\n- ", 8).unwrap(), ("- 甲\n".to_string(), 6));
        assert_eq!(list_backspace_edit("  - ", 4).unwrap(), (String::new(), 0));
        assert_eq!(list_backspace_edit("- [ ] ", 6).unwrap(), (String::new(), 0));
        assert_eq!(list_backspace_edit("1. ", 3).unwrap(), (String::new(), 0));
        assert_eq!(list_backspace_edit("- ", 1).unwrap(), (String::new(), 0));
        assert_eq!(list_backspace_edit("-", 1).unwrap(), (String::new(), 0));
    }

    #[test]
    fn list_backspace_rejects_content_and_line_start() {
        assert!(list_backspace_edit("- 甲", 3).is_none());
        assert!(list_backspace_edit("普通段落", 4).is_none());
        assert!(list_backspace_edit("- ", 0).is_none());
    }

    #[test]
    fn indent_shifts_list_lines_wholly() {
        assert_eq!(indent_edit("- 甲", 3, 3).text, "  - 甲");
        assert_eq!(indent_edit("- 甲", 3, 3).from, 5);
        assert_eq!(indent_edit("- 甲", 0, 0).text, "  - 甲");
        assert_eq!(indent_edit("- 甲", 0, 0).from, 2);
        assert_eq!(indent_edit("1. 甲", 4, 4).text, "  1. 甲");
        assert_eq!(indent_edit("- [ ] 甲", 7, 7).text, "  - [ ] 甲");
    }

    #[test]
    fn indent_nested_scenario_only_second_item() {
        let text = "- 项目一\n- 项目二";
        let edit = indent_edit(text, text.len(), text.len());
        assert_eq!(edit.text, "- 项目一\n  - 项目二");
        assert_eq!(edit.from, text.len() + 2);
    }

    #[test]
    fn indent_inserts_at_caret_for_non_list_lines() {
        assert_eq!(indent_edit("普通段落", 6, 6).text, "普通  段落");
        assert_eq!(indent_edit("普通段落", 6, 6).from, 8);
        assert_eq!(indent_edit("const x = 1;", 12, 12).text, "const x = 1;  ");
        assert_eq!(indent_edit("普通段落", 0, 0).text, "  普通段落");
    }

    #[test]
    fn indent_selection_shifts_each_covered_line() {
        let text = "- 甲\n- 乙\n- 丙";
        let edit = indent_edit(text, 2, 12);
        assert_eq!(edit.text, "  - 甲\n  - 乙\n- 丙");
        assert_eq!(edit.from, 4);
        assert_eq!(edit.to, 16);
        assert_eq!(indent_edit("普通段落", 3, 9).text, "  普通段落");
    }

    #[test]
    fn toggle_task_flips_checkbox() {
        assert_eq!(toggle_task("- [ ] 甲", 0).unwrap(), ("- [x] 甲".to_string(), 3));
        assert_eq!(toggle_task("- [x] 甲", 0).unwrap(), ("- [ ] 甲".to_string(), 3));
        assert_eq!(toggle_task("  - [X] 甲", 2).unwrap(), ("  - [ ] 甲".to_string(), 5));
        assert!(toggle_task("- 甲", 0).is_none());
    }

    #[test]
    fn outdent_removes_one_unit() {
        assert_eq!(outdent_edit("  - 甲", 7, 7).unwrap().text, "- 甲");
        assert_eq!(outdent_edit("  - 甲", 7, 7).unwrap().from, 5);
        assert_eq!(outdent_edit(" - 甲", 6, 6).unwrap().text, "- 甲");
        assert_eq!(outdent_edit("\t- 甲", 6, 6).unwrap().text, "- 甲");
        assert_eq!(outdent_edit("\t- 甲", 6, 6).unwrap().from, 5);
        assert_eq!(outdent_edit("  - 甲", 1, 1).unwrap().text, "- 甲");
        assert_eq!(outdent_edit("  - 甲", 1, 1).unwrap().from, 0);
    }

    #[test]
    fn outdent_multi_line_and_none() {
        let text = "  - 甲\n- 乙\n  - 丙";
        let edit = outdent_edit(text, 2, text.len()).unwrap();
        assert_eq!(edit.text, "- 甲\n- 乙\n- 丙");
        assert_eq!(edit.from, 0);
        assert_eq!(edit.to, 17);
        assert!(outdent_edit("- 甲", 5, 5).is_none());
        assert!(outdent_edit("普通段落", 6, 6).is_none());
    }

    #[test]
    fn gap_line_starts_scans_inner_lines() {
        // "ab\n\ncd" 的空行区间 [2, 4)：终止行跳过，空行行首 = 3
        assert_eq!(gap_line_starts("ab\n\ncd", 2, 4), vec![3]);
        // "ab\n\n\ncd" 的空行区间 [2, 5)：两根空行行首 3、4
        assert_eq!(gap_line_starts("ab\n\n\ncd", 2, 5), vec![3, 4]);
        // 空行含空白字符：行首仍按实际字节推进
        assert_eq!(gap_line_starts("ab\n  \n\ncd", 2, 7), vec![3, 6]);
        // 引用空延续行算一根空行
        assert_eq!(gap_line_starts("ab\n>\n\ncd", 2, 6), vec![3, 5]);
    }

    #[test]
    fn edge_blank_starts_includes_first_line() {
        // 文首空白区每行都是空白行（含第一行）
        assert_eq!(edge_blank_starts("\n\n甲", 0, 2), vec![0, 1]);
        // 末尾不带换行的片段是首块的行首缩进，不算空白行
        assert_eq!(edge_blank_starts("  甲", 0, 2), Vec::<usize>::new());
        assert_eq!(edge_blank_starts("  \n  甲", 0, 5), vec![0]);
    }

    #[test]
    fn tail_blank_starts_appends_virtual_line() {
        // "ab\n"：尾部区间无真实空行，虚拟文末行首 = 3（len）
        assert_eq!(tail_blank_starts("ab\n", 2), vec![3]);
        // "ab\n\n"：一根真实空行 + 虚拟文末行
        assert_eq!(tail_blank_starts("ab\n\n", 2), vec![3, 4]);
        // 不以换行结尾：无虚拟行
        assert_eq!(tail_blank_starts("ab\n  ", 2), vec![3]);
    }

    #[test]
    fn gap_hit_offset_maps_line_to_source_offset() {
        // 两根空行（行首 10、11）：点上半区落行 0，下半区落行 1，越界吸附最后一根
        assert_eq!(gap_hit_offset(&[10, 11], 0.0, 40.0), 10);
        assert_eq!(gap_hit_offset(&[10, 11], 19.0, 40.0), 10);
        assert_eq!(gap_hit_offset(&[10, 11], 20.0, 40.0), 11);
        assert_eq!(gap_hit_offset(&[10, 11], 100.0, 40.0), 11);
    }

    #[test]
    fn gap_covered_lines_tracks_selection() {
        let starts = [10, 11];
        // 选区起于空行 0（offset 10）拖入下一块：两根空行全覆盖
        assert_eq!(gap_covered_lines(&starts, 12, 10, 20), (0, 2));
        // 选区只触及空行 0 的换行符
        assert_eq!(gap_covered_lines(&starts, 12, 5, 11), (0, 1));
        // 选区止于上一块末（未触及空行）
        assert_eq!(gap_covered_lines(&starts, 12, 5, 10), (0, 0));
        // 选区从下一块起（未触及空行）
        assert_eq!(gap_covered_lines(&starts, 12, 12, 20), (0, 0));
        // 空选区 / 全区间
        assert_eq!(gap_covered_lines(&starts, 12, 10, 10), (0, 0));
        assert_eq!(gap_covered_lines(&starts, 12, 5, 12), (0, 2));
    }

    #[test]
    fn vertical_target_crosses_adjacent_blocks() {
        let spans = [(0, 9), (11, 20)];
        assert_eq!(vertical_target(5, &spans, &[], true), Some(11));
        assert_eq!(vertical_target(15, &spans, &[], false), Some(9));
        // 首块向上 / 末块向下无路
        assert_eq!(vertical_target(5, &spans, &[], false), None);
        assert_eq!(vertical_target(15, &spans, &[], true), None);
    }

    #[test]
    fn vertical_target_walks_gap_lines() {
        // 两根空行（行首 10、11）：空行内按行首逐行，越端点落相邻块
        let spans = [(0, 9), (12, 20)];
        let starts = [10, 11];
        assert_eq!(vertical_target(10, &spans, &starts, true), Some(11));
        assert_eq!(vertical_target(11, &spans, &starts, true), Some(12));
        assert_eq!(vertical_target(11, &spans, &starts, false), Some(10));
        assert_eq!(vertical_target(10, &spans, &starts, false), Some(9));
    }

    #[test]
    fn vertical_target_walks_whitespace_gap_lines() {
        // 空行含空白字符（行首 10 的行占 "   \n"）：行移动按行首跳，不逐字节走
        let spans = [(0, 9), (14, 20)];
        let starts = [10, 13];
        assert_eq!(vertical_target(12, &spans, &starts, true), Some(13));
        assert_eq!(vertical_target(12, &spans, &starts, false), Some(9));
        assert_eq!(vertical_target(13, &spans, &starts, false), Some(10));
        assert_eq!(vertical_target(13, &spans, &starts, true), Some(14));
    }

    #[test]
    fn vertical_target_edges() {
        let spans = [(5, 9)];
        // 文首之前：向下进首块，向上无路
        assert_eq!(vertical_target(2, &spans, &[], true), Some(5));
        assert_eq!(vertical_target(2, &spans, &[], false), None);
        // 文末之后的空行：向上收回块末，向下无路
        assert_eq!(vertical_target(11, &spans, &[10], false), Some(9));
        assert_eq!(vertical_target(11, &spans, &[10], true), None);
    }

    #[test]
    fn vertical_target_enters_edge_blank_regions() {
        let spans = [(5, 9)];
        // 末块向下进文末空白区，区内逐行、末端无路
        assert_eq!(vertical_target(7, &spans, &[10, 11], true), Some(10));
        assert_eq!(vertical_target(10, &spans, &[10, 11], true), Some(11));
        assert_eq!(vertical_target(11, &spans, &[10, 11], true), None);
        // 区内向上逐行收回块末
        assert_eq!(vertical_target(11, &spans, &[10, 11], false), Some(10));
        assert_eq!(vertical_target(10, &spans, &[10, 11], false), Some(9));
        // 首块向上进文首空白区，区内逐行、顶端无路
        assert_eq!(vertical_target(6, &spans, &[0, 1, 2], false), Some(2));
        assert_eq!(vertical_target(2, &spans, &[0, 1, 2], false), Some(1));
        assert_eq!(vertical_target(1, &spans, &[0, 1, 2], false), Some(0));
        assert_eq!(vertical_target(0, &spans, &[0, 1, 2], false), None);
        // 区内向下进首块
        assert_eq!(vertical_target(2, &spans, &[0, 1, 2], true), Some(5));
        assert_eq!(vertical_target(1, &spans, &[0, 1, 2], true), Some(2));
    }

    #[test]
    fn remote_span_converts_note_offsets_to_block() {
        // 前缀剥除：note 偏移 15/17 → 正文 5/7，选区落块 [4, 10) 内
        assert_eq!(remote_span_in_block(17, 15, 10, 4, 10), Some((5, 7, Some(7))));
        // 折叠光标：选区为空，光标条保留
        assert_eq!(remote_span_in_block(16, 16, 10, 4, 10), Some((6, 6, Some(6))));
        // 块末边界光标归本块（端点闭区间）
        assert_eq!(remote_span_in_block(20, 20, 10, 4, 10), Some((10, 10, Some(10))));
        // 选区跨越块界：两端夹回块界，head 在块外则不画光标条
        assert_eq!(remote_span_in_block(30, 1, 10, 4, 10), Some((4, 10, None)));
        // 锚点在 frontmatter 区（< body_start）：归正文零点，伸入块内的部分照画
        assert_eq!(remote_span_in_block(17, 3, 10, 4, 10), Some((4, 7, Some(7))));
        // head 在 frontmatter 区：整条不画
        assert_eq!(remote_span_in_block(8, 20, 10, 0, 10), None);
        // 光标与选区都不触及块
        assert_eq!(remote_span_in_block(22, 24, 10, 4, 10), None);
    }
}