//! 对话输入面：自绘多行文本部件（光标 / 选区 / IME / 剪贴板 / 撤销重做）+
//! # 引用标签的选区式高亮与整标签交互；文本真相在编排层 `Draft`。

use iced::advanced::clipboard::Kind as ClipboardKind;
use iced::advanced::input_method::{Event as ImeEvent, InputMethod, Purpose};
use iced::advanced::layout::{self, Layout};
use iced::advanced::mouse;
use iced::advanced::renderer::Quad;
use iced::advanced::text as advanced_text;
use iced::advanced::text::Paragraph as _;
use iced::advanced::text::Span;
use iced::advanced::widget::{self, Widget};
use iced::advanced::{Clipboard, Shell};
use iced::advanced::{Renderer as _, text::Renderer as _};
use iced::alignment;
use iced::event::Event;
use iced::keyboard;
use iced::keyboard::key::Named;
use iced::keyboard::Key;
use iced::window;
use iced::{Font, Length, Point, Rectangle, Size, Theme, Vector};

use std::time::{Duration, Instant};

use atelyx_core::ai::refs::{self, RefEntry};

use crate::markdown_edit::{
    byte_of_char, char_index, line_end_at, line_start_at, selection_rects, visible_count,
    ParagraphT,
};
use crate::markdown_view::is_cr;
use crate::text_preedit::{self, Composed};
use crate::ui::palette as pal;
use crate::Message;

/// 行高倍数。
const LINE_FACTOR: f32 = 1.5;
/// 输入面静止高度（空框与短内容的保底下限，与旧多行输入框一致）。
const MIN_HEIGHT: f32 = 96.0;
/// 输入面纵向可视上限（超出内部滚动）。
const MAX_HEIGHT: f32 = 240.0;
/// 光标闪烁半周期（毫秒）。
const BLINK_MS: u128 = 500;
/// 撤销栈深度上限（全文快照，超限淘汰最旧）。
const UNDO_DEPTH: usize = 200;
/// 双击判定的最大间隔与位移。
const DOUBLE_CLICK_MS: u128 = 350;
const DOUBLE_CLICK_DRIFT: f32 = 4.0;

/// 输入草稿真相（应用层持有；部件按帧投影）。
#[derive(Default, Debug, Clone)]
pub struct Draft {
    pub text: String,
    pub caret: usize,
    pub anchor: usize,
    /// 输入法组合串（组合期恒显示、不进正文）。
    pub preedit: Option<String>,
    pub focused: bool,
    /// 撤销栈（(文本, 光标) 全量快照；文本突变时压入）。
    undo: Vec<(String, usize)>,
    /// 重做栈（撤销时压入；新编辑作废）。
    redo: Vec<(String, usize)>,
}

impl Draft {
    /// 清空草稿（保留焦点态）。
    pub fn clear(&mut self) {
        self.text.clear();
        self.caret = 0;
        self.anchor = 0;
        self.preedit = None;
        self.undo.clear();
        self.redo.clear();
    }

    /// 文本突变落地：文本变化才压撤销快照并作废重做。
    pub fn apply_edited(&mut self, text: String, caret: usize, anchor: usize) {
        if text != self.text {
            self.undo
                .push((std::mem::take(&mut self.text), self.caret));
            if self.undo.len() > UNDO_DEPTH {
                self.undo.remove(0);
            }
            self.redo.clear();
        }
        self.text = text;
        self.caret = caret;
        self.anchor = anchor;
    }

    pub fn undo_step(&mut self) {
        if let Some((text, caret)) = self.undo.pop() {
            self.redo.push((std::mem::take(&mut self.text), self.caret));
            self.text = text;
            self.caret = caret;
            self.anchor = caret;
        }
    }

    pub fn redo_step(&mut self) {
        if let Some((text, caret)) = self.redo.pop() {
            self.undo.push((std::mem::take(&mut self.text), self.caret));
            self.text = text;
            self.caret = caret;
            self.anchor = caret;
        }
    }

    /// 选区文本（两端夹回文本长度）。
    pub fn selected_text(&self) -> String {
        let (a, b) = self.selection();
        self.text[a..b].to_string()
    }

    /// 选区两端（升序）。
    pub fn selection(&self) -> (usize, usize) {
        let a = self.anchor.min(self.caret).min(self.text.len());
        let b = self.anchor.max(self.caret).min(self.text.len());
        (a, b)
    }

    /// 光标前是否存在激活的 # 触发段：`#` 在行首或空白后，且其后到光标无空白
    /// （返回 `#` 的字节位置；浮层开启期间继续键入靠本判定保持激活）。
    pub fn active_mention_at(&self) -> Option<usize> {
        let caret = self.caret.min(self.text.len());
        if !self.text.is_char_boundary(caret) {
            return None;
        }
        let seg = &self.text[..caret];
        let at = seg.rfind('#')?;
        if !refs::is_trigger_position(&seg[..at]) {
            return None;
        }
        seg[at + 1..]
            .chars()
            .all(|c| !c.is_whitespace())
            .then_some(at)
    }
}

/// 部件回发事件（应用层据此更新草稿与浮层）。
#[derive(Debug, Clone)]
pub enum InputEvent {
    /// 文本突变后的完整草稿（应用层整体接管）。
    Edited { text: String, caret: usize, anchor: usize },
    /// 仅光标 / 选区移动（文本不变）。
    Select { caret: usize, anchor: usize },
    /// 部件内按下（取得焦点）。
    Focus,
    /// Enter（无 Shift、非组合期）请求发送。
    Send,
    /// 输入法组合串变化。
    Preedit(Option<String>),
    Undo,
    Redo,
    /// 候选浮层开启时方向键 / Enter / Esc 的转交。
    PickerNav(i32),
    PickerCommit,
    PickerClose,
}

/// 自绘输入面部件。
pub struct Input<'a> {
    pub draft: &'a Draft,
    /// 已登记引用（标签高亮与整标签交互按此扫描）。
    pub refs: &'a [RefEntry],
    /// 候选浮层开启：方向 / Enter / Esc 转交浮层。
    pub picker_open: bool,
    pub placeholder: &'a str,
    pub handler: fn(InputEvent) -> Message,
}

impl<'a> Input<'a> {
    pub fn new(draft: &'a Draft, refs: &'a [RefEntry], handler: fn(InputEvent) -> Message) -> Self {
        Self {
            draft,
            refs,
            picker_open: false,
            placeholder: "",
            handler,
        }
    }

    pub fn picker_open(mut self, open: bool) -> Self {
        self.picker_open = open;
        self
    }

    pub fn placeholder(mut self, value: &'a str) -> Self {
        self.placeholder = value;
        self
    }

    fn publish(&self, shell: &mut Shell<'_, Message>, event: InputEvent) {
        shell.publish((self.handler)(event));
        shell.capture_event();
        shell.request_redraw();
    }
}

#[derive(Default)]
struct InputState {
    paragraph: Option<ParagraphT>,
    content: String,
    /// 建段时的排版文本（组合串已插在光标处）。
    layout: String,
    /// 建段时的组合串（`None` = 未组合）。
    preedit: Option<String>,
    /// 建段时的组合串插入点（正文字符下标）。
    insert: usize,
    size: f32,
    width: f32,
    /// 建段时的界面字体（换族后旧行段与命中几何即失效）。
    font: iced::Font,
    /// 可视行表（随段落重建；命中与光标几何的纵向基准）。
    rows: Vec<Row>,
    blink_anchor: Option<Instant>,
    seen_caret: usize,
    /// 纵向滚动偏移（内容超可视上限时）。
    scroll_y: f32,
    /// 按压拖选进行中。
    dragging: bool,
    /// 双击判定（上次按压时刻与位置）。
    last_click: Option<(Instant, Point)>,
}

impl InputState {
    fn composed(&self) -> Composed {
        Composed::new(self.insert, self.preedit.as_deref())
    }

    fn stale(
        &self,
        content: &str,
        preedit: Option<&str>,
        insert: usize,
        size: f32,
        width: f32,
        font: iced::Font,
    ) -> bool {
        self.paragraph.is_none()
            || self.content.as_str() != content
            || self.preedit.as_deref() != preedit
            || (preedit.is_some() && self.insert != insert)
            || self.size != size
            || self.font != font
            || (self.width - width).abs() > 0.5
    }
}

/// 逐字符 span 段落（`span_bounds` 按字符给几何——命中测试与光标定位的前提，
/// 与笔记编辑面同口径）；组合串按弱化色排在插入点。`\r` 不占位（见 is_cr）。
fn build_paragraph(content: &str, size: f32, width: f32, at: Composed) -> ParagraphT {
    let mut spans: Vec<Span<'static, (), Font>> = Vec::with_capacity(content.chars().count());
    let mut k = 0;
    for ch in content.chars() {
        if is_cr(ch) {
            continue;
        }
        let span = Span::new(ch.to_string());
        spans.push(if at.span().contains(&k) { span.color(pal::TEXT_SECONDARY) } else { span });
        k += 1;
    }
    ParagraphT::with_spans(advanced_text::Text {
        content: spans.as_slice(),
        bounds: Size::new(width, f32::INFINITY),
        size: size.into(),
        line_height: advanced_text::LineHeight::Relative(LINE_FACTOR),
        font: crate::ui::regular(),
        align_x: advanced_text::Alignment::Left,
        align_y: alignment::Vertical::Top,
        shaping: advanced_text::Shaping::Advanced,
        wrapping: advanced_text::Wrapping::default(),
    })
}

/// 可视行：字形行或推算的无字形行（空行 / 文末虚拟行）。
#[derive(Clone, Copy)]
struct Row {
    /// 行带顶（字形矩形 y 口径；空行按相邻字形行网格推算）。
    y: f32,
    /// 行首字节偏移。
    start: usize,
    kind: RowKind,
}

#[derive(Clone, Copy)]
enum RowKind {
    /// 字形行：行内字符区间（换行段必含字形，空行只会由连续换行产生）。
    Glyph { first: usize, last: usize },
    /// 无字形行：光标恒落行首。
    Empty,
}

/// 行表构建：字形按 y 聚带；首带前（前导换行）、带间（连续换行）、
/// 末带后（结尾换行的文末虚拟行）按行高网格推算。行表按视觉序、行首字节升序。
fn rows_of(p: &ParagraphT, content: &str, line_px: f32) -> Vec<Row> {
    let count = visible_count(content);
    let byte_at = |i: usize| byte_of_char(content, i.min(count));
    let mut bands: Vec<(f32, usize, usize)> = Vec::new();
    for i in 0..count {
        let Some(r) = p.span_bounds(i).into_iter().next() else {
            continue;
        };
        match bands.last_mut() {
            Some((y, _, last)) if (r.y - *y).abs() < 0.5 => *last = i,
            _ => bands.push((r.y, i, i)),
        }
    }
    let Some(&(y0, first0, _)) = bands.first() else {
        // 无字形：空文本或纯换行 —— 行号即字符下标（含文末虚拟行）
        return (0..=count)
            .map(|k| Row { y: k as f32 * line_px, start: byte_at(k), kind: RowKind::Empty })
            .collect();
    };
    let mut rows: Vec<Row> = Vec::new();
    for k in 0..first0 {
        let start = byte_at(k);
        rows.push(Row {
            y: y0 - (first0 - k) as f32 * line_px,
            start,
            kind: RowKind::Empty,
        });
    }
    for (i, &(y, first, last)) in bands.iter().enumerate() {
        rows.push(Row { y, start: byte_at(first), kind: RowKind::Glyph { first, last } });
        let n_empty = if i + 1 == bands.len() {
            count - (last + 1)
        } else {
            let next_y = bands[i + 1].0;
            (((next_y - y) / line_px).round() as i32 - 1).max(0) as usize
        };
        for j in 1..=n_empty {
            let start = byte_at(last + 1 + j);
            rows.push(Row { y: y + j as f32 * line_px, start, kind: RowKind::Empty });
        }
    }
    rows
}

/// 纵向定位：点所在行（行盒 `[y, y + line_px)` 分箱，越界夹回首末行）。
fn row_at(rows: &[Row], y: f32, line_px: f32) -> &Row {
    let idx = rows.partition_point(|r| r.y + line_px <= y);
    rows.get(idx).unwrap_or_else(|| rows.last().expect("行表非空"))
}

/// 命中 → 字节偏移：行表定位纵向（空行 / 文末虚拟行落行首），
/// 字形行内取 x 最近字符并以字形中点分左右。
fn hit_offset_rows(
    rows: &[Row],
    p: &ParagraphT,
    content: &str,
    point: Point,
    line_px: f32,
) -> usize {
    let count = visible_count(content);
    let row = row_at(rows, point.y, line_px);
    let RowKind::Glyph { first, last } = row.kind else {
        return row.start;
    };
    let mut best: Option<(usize, f32, bool)> = None; // (字符, |dx|, 越过中点)
    for i in first..=last {
        let Some(r) = p.span_bounds(i).into_iter().next() else {
            continue;
        };
        let dx = if point.x < r.x {
            r.x - point.x
        } else if point.x > r.x + r.width {
            point.x - (r.x + r.width)
        } else {
            0.0
        };
        let past = point.x > r.x + r.width / 2.0;
        if best.map_or(true, |(_, bdx, _)| dx < bdx) {
            best = Some((i, dx, past));
        }
    }
    match best {
        Some((i, _, past)) => byte_of_char(content, (if past { i + 1 } else { i }).min(count)),
        None => row.start,
    }
}

/// 光标几何（行表口径）：字形位取字形矩形；行尾（终止换行前）、
/// 空行与文末虚拟行按行表落位。返回 (x, y, h)。
fn caret_row_pos(
    rows: &[Row],
    p: &ParagraphT,
    content: &str,
    idx: usize,
    line_px: f32,
) -> (f32, f32, f32) {
    let count = visible_count(content);
    let idx = idx.min(count);
    if idx < count {
        if let Some(r) = p.span_bounds(idx).into_iter().next() {
            return (r.x, r.y, r.height);
        }
    }
    let byte = byte_of_char(content, idx);
    let ri = rows.partition_point(|r| r.start <= byte).saturating_sub(1);
    let Some(row) = rows.get(ri) else {
        return (0.0, 0.0, line_px);
    };
    match row.kind {
        RowKind::Empty => (0.0, row.y, line_px),
        RowKind::Glyph { last, .. } => match p.span_bounds(last).into_iter().next() {
            Some(r) => (r.x + r.width, r.y, r.height),
            None => (0.0, row.y, line_px),
        },
    }
}

/// 词边界：`backward` = 跳过尾部空白后回退到词首；否则前进到词尾。
fn word_edge(text: &str, mut i: usize, backward: bool) -> usize {
    i = i.min(text.len());
    while !text.is_char_boundary(i) {
        i = if backward { i - 1 } else { i + 1 };
    }
    if backward {
        while let Some((b, c)) = text[..i].char_indices().next_back() {
            if c.is_whitespace() {
                i = b;
            } else {
                break;
            }
        }
        while let Some((b, c)) = text[..i].char_indices().next_back() {
            if !c.is_whitespace() {
                i = b;
            } else {
                break;
            }
        }
    } else {
        while text[i..].chars().next().is_some_and(|c| c.is_whitespace()) {
            i += text[i..].chars().next().map_or(1, char::len_utf8);
        }
        while text[i..]
            .chars()
            .next()
            .is_some_and(|c| !c.is_whitespace())
        {
            i += text[i..].chars().next().map_or(1, char::len_utf8);
        }
    }
    i
}

/// 双击选词：命中位在词内或词边界取相邻词（右侧优先），空白段取该空白段。
fn word_range_at(text: &str, i: usize) -> (usize, usize) {
    let i = i.min(text.len());
    let mut lo = i;
    while !text.is_char_boundary(lo) {
        lo -= 1;
    }
    let mut hi = lo;
    let fwd_nonws = text[hi..].chars().next().is_some_and(|c| !c.is_whitespace());
    let back_nonws = text[..lo].chars().next_back().is_some_and(|c| !c.is_whitespace());
    if !fwd_nonws && !back_nonws {
        // 空白段：向两侧扩展空白
        while text[hi..].chars().next().is_some_and(|c| c.is_whitespace()) {
            hi += text[hi..].chars().next().map_or(1, char::len_utf8);
        }
        while let Some((b, c)) = text[..lo].char_indices().next_back() {
            if c.is_whitespace() {
                lo = b;
            } else {
                break;
            }
        }
        return (lo, hi);
    }
    if fwd_nonws {
        while text[hi..].chars().next().is_some_and(|c| !c.is_whitespace()) {
            hi += text[hi..].chars().next().map_or(1, char::len_utf8);
        }
    }
    if back_nonws {
        while let Some((b, c)) = text[..lo].char_indices().next_back() {
            if !c.is_whitespace() {
                lo = b;
            } else {
                break;
            }
        }
    }
    (lo, hi)
}

/// 在光标处插入文本（替换选区），返回 (新文本, 新光标)。
fn insert_at(text: &str, caret: usize, anchor: usize, insert: &str) -> (String, usize) {
    let (a, b) = {
        let lo = anchor.min(caret).min(text.len());
        let hi = anchor.max(caret).min(text.len());
        (lo, hi)
    };
    let mut out = String::with_capacity(text.len() - (b - a) + insert.len());
    out.push_str(&text[..a]);
    out.push_str(insert);
    out.push_str(&text[b..]);
    let caret = a + insert.len();
    (out, caret)
}

impl Widget<Message, Theme, iced::Renderer> for Input<'_> {
    fn tag(&self) -> widget::tree::Tag {
        widget::tree::Tag::of::<InputState>()
    }

    fn state(&self) -> widget::tree::State {
        widget::tree::State::new(InputState::default())
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
            let size = pal::fs_body();
            let text = &self.draft.text;
            let line_px = size * LINE_FACTOR;
            let state = tree.state.downcast_mut::<InputState>();
            let font = crate::ui::regular();
            let preedit = if self.draft.focused {
                self.draft.preedit.as_deref().filter(|s| !s.is_empty())
            } else {
                None
            };
            let insert = char_index(text, self.draft.caret.min(text.len()));
            if state.stale(text, preedit, insert, size, width, font) {
                let at = Composed::new(insert, preedit);
                let laid = text_preedit::compose(text, at, preedit);
                state.paragraph = Some(build_paragraph(&laid, size, width, at));
                state.content = text.to_string();
                state.layout = laid.into_owned();
                state.preedit = preedit.map(str::to_string);
                state.insert = insert;
                state.size = size;
                state.font = font;
                state.width = width;
                let p = state.paragraph.as_ref().expect("段落已建");
                state.rows = rows_of(p, &state.layout, line_px);
            }
            let line_px = size * LINE_FACTOR;
            let content_h = state
                .paragraph
                .as_ref()
                .map_or(line_px, |p| p.min_bounds().height);
            let height = content_h.clamp(MIN_HEIGHT, MAX_HEIGHT);
            // 滚动夹回可视区
            if let Some(p) = state.paragraph.as_ref() {
                let max_scroll = (p.min_bounds().height - height).max(0.0);
                state.scroll_y = state.scroll_y.clamp(0.0, max_scroll);
            }
            Size::new(width, height)
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
        let state = tree.state.downcast_ref::<InputState>();
        let Some(p) = state.paragraph.as_ref() else {
            return;
        };
        let line_px = state.size * LINE_FACTOR;
        let origin = bounds.position() + Vector::new(0.0, -state.scroll_y);
        let text_bounds = Rectangle::new(origin, Size::new(bounds.width, f32::INFINITY));
        let clip = bounds.intersection(viewport).unwrap_or(bounds);
        let at = state.composed();

        // 全部内容画进按 bounds 裁剪的层：滚动时超出可视区的部分不外溢
        renderer.with_layer(clip, |renderer| {
            // # 引用标签高亮（选区样式；正文底下）
            for hit in refs::scan_hits(&self.draft.text, self.refs) {
                let (lo, hi) = (hit.start, hit.start + hit.len);
                for r in selection_rects(
                    p,
                    at.index(char_index(&state.content, lo)),
                    at.index(char_index(&state.content, hi)),
                ) {
                    renderer.fill_quad(
                        Quad {
                            bounds: Rectangle::new(
                                origin + Vector::new(r.x, r.y + r.height * 0.15),
                                Size::new(r.width, r.height * 0.85),
                            ),
                            border: iced::border::Border {
                                radius: 2.0.into(),
                                ..Default::default()
                            },
                            ..Quad::default()
                        },
                        pal::ACCENT_SOFT,
                    );
                }
            }

            // 选区（压正文下，覆盖标签高亮更醒目）
            let (sel_a, sel_b) = self.draft.selection();
            if sel_b > sel_a {
                for r in selection_rects(
                    p,
                    at.index(char_index(&state.content, sel_a)),
                    at.index(char_index(&state.content, sel_b)),
                ) {
                    renderer.fill_quad(
                        Quad {
                            bounds: Rectangle::new(origin + Vector::new(r.x, r.y), r.size()),
                            ..Quad::default()
                        },
                        pal::SELECTION,
                    );
                }
            }

            // 正文（纵向滚动 = bounds 平移，可视区裁剪）
            iced::advanced::widget::text::draw(
                renderer,
                style,
                text_bounds,
                p,
                iced::advanced::widget::text::Style { color: None },
                &clip,
            );

            // 占位符
            if self.draft.text.is_empty() && self.draft.preedit.is_none() {
                renderer.fill_text(
                    advanced_text::Text {
                        content: self.placeholder.to_string(),
                        bounds: Size::new(bounds.width, line_px),
                        size: state.size.into(),
                        line_height: advanced_text::LineHeight::Relative(LINE_FACTOR),
                        font: crate::ui::regular(),
                        align_x: advanced_text::Alignment::Left,
                        align_y: alignment::Vertical::Top,
                        shaping: advanced_text::Shaping::Auto,
                        wrapping: advanced_text::Wrapping::None,
                    },
                    bounds.position(),
                    pal::TEXT_MUTED,
                    bounds,
                );
            }

            if !self.draft.focused {
                return;
            }
            // 组合串已排进段落，光标竖条落在串尾
            let caret = self.draft.caret.clamp(0, self.draft.text.len());
            let caret_char = char_index(&state.content, caret);
            let (x, y, h) =
                caret_row_pos(&state.rows, p, &state.layout, at.caret(caret_char), line_px);
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
                    bounds: Rectangle::new(
                        origin + Vector::new(x, y),
                        Size::new(1.5, h.max(line_px)),
                    ),
                    ..Quad::default()
                },
                pal::TEXT_PRIMARY,
            );
        });

        // 滚动条（内容超出可视区时）
        let content_h = p.min_bounds().height;
        if content_h > bounds.height + 0.5 {
            let max_scroll = content_h - bounds.height;
            let track = bounds.height - 8.0;
            let thumb_h = (track * bounds.height / content_h).max(24.0).min(track);
            let thumb_y =
                bounds.y + 4.0 + (state.scroll_y / max_scroll) * (track - thumb_h);
            renderer.fill_quad(
                Quad {
                    bounds: Rectangle::new(
                        Point::new(bounds.x + bounds.width - 6.0, thumb_y),
                        Size::new(3.0, thumb_h),
                    ),
                    border: iced::border::Border {
                        radius: 1.5.into(),
                        ..Default::default()
                    },
                    ..Quad::default()
                },
                pal::BORDER,
            );
        }
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
        cursor: mouse::Cursor,
        _renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let state = tree.state.downcast_mut::<InputState>();
        let line_px = state.size * LINE_FACTOR;
        if state.seen_caret != self.draft.caret {
            state.seen_caret = self.draft.caret;
            state.blink_anchor = Some(Instant::now());
        }

        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let Some(pos) = cursor.position_in(bounds) else {
                    return;
                };
                // IME 组合期点击会中断组合会话，光标换算暂不接管
                if self.draft.preedit.is_some() {
                    shell.capture_event();
                    return;
                }
                // 建段态仍含组合串（组合刚结束的一帧）：行表按排版文本，正文下标换算不成立
                if state.preedit.is_some() {
                    shell.capture_event();
                    return;
                }
                self.publish(shell, InputEvent::Focus);
                state.dragging = true;
                let local = Point::new(pos.x, pos.y + state.scroll_y);
                // 双击 = 选词
                if let Some((t, p)) = state.last_click {
                    if t.elapsed().as_millis() <= DOUBLE_CLICK_MS
                        && { let d = p - pos; d.x * d.x + d.y * d.y <= DOUBLE_CLICK_DRIFT * DOUBLE_CLICK_DRIFT }
                    {
                        let byte = state.paragraph.as_ref().map_or(0, |p| {
                            hit_offset_rows(&state.rows, p, &state.content, local, line_px)
                        });
                        let (lo, hi) = word_range_at(&self.draft.text, byte);
                        state.last_click = None;
                        self.publish(
                            shell,
                            InputEvent::Select { caret: hi, anchor: lo },
                        );
                        return;
                    }
                }
                state.last_click = Some((Instant::now(), pos));
                let caret = state.paragraph.as_ref().map_or(0, |p| {
                    hit_offset_rows(&state.rows, p, &state.content, local, line_px)
                });
                // 点击落在标签内 = 整标签选中
                if let Some(tag) = self.tag_at(caret) {
                    self.publish(
                        shell,
                        InputEvent::Select { caret: tag.1, anchor: tag.0 },
                    );
                    return;
                }
                self.publish(shell, InputEvent::Select { caret, anchor: caret });
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                if !state.dragging || self.draft.preedit.is_some() {
                    return;
                }
                let Some(pos) = cursor.position_in(bounds) else {
                    return;
                };
                let local = Point::new(pos.x, pos.y + state.scroll_y);
                if let Some(p) = state.paragraph.as_ref() {
                    let caret =
                        hit_offset_rows(&state.rows, p, &state.content, local, line_px);
                    self.publish(
                        shell,
                        InputEvent::Select { caret, anchor: self.draft.anchor },
                    );
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                state.dragging = false;
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                // 内容未超出可视区不接管（滚轮穿透给外层消息流）
                let Some(p) = state.paragraph.as_ref() else {
                    return;
                };
                let max_scroll = (p.min_bounds().height - bounds.height).max(0.0);
                if max_scroll <= 0.0 {
                    return;
                }
                if !cursor.is_over(bounds) {
                    return;
                }
                let step = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => y * state.size * LINE_FACTOR,
                    mouse::ScrollDelta::Pixels { y, .. } => *y,
                };
                state.scroll_y = (state.scroll_y - step).clamp(0.0, max_scroll);
                shell.capture_event();
                shell.request_redraw();
            }
            _ => {}
        }

        if !self.draft.focused {
            return;
        }

        match event {
            Event::InputMethod(ime) => match ime {
                ImeEvent::Closed => self.publish(shell, InputEvent::Preedit(None)),
                ImeEvent::Preedit(content, _) => {
                    self.publish(shell, InputEvent::Preedit(Some(content.clone())))
                }
                ImeEvent::Commit(content) => {
                    let (text, caret) = insert_at(
                        &self.draft.text,
                        self.draft.caret,
                        self.draft.anchor,
                        content,
                    );
                    self.publish(shell, InputEvent::Preedit(None));
                    self.publish(shell, InputEvent::Edited { text, caret, anchor: caret });
                }
                ImeEvent::Opened => {}
            },
            Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, text, .. })
                if self.draft.preedit.is_none() =>
            {
                self.handle_key(key, *modifiers, text.as_deref(), state, clipboard, shell);
            }
            Event::Window(window::Event::RedrawRequested(_)) => {
                let caret = self.draft.caret.clamp(0, self.draft.text.len());
                let caret_char = char_index(&state.content, caret);
                let at = state.composed();
                if let Some(p) = state.paragraph.as_ref() {
                    // 组合期报组合起点：候选窗贴串首，串尾由本端自绘
                    let (x, y, h) = caret_row_pos(
                        &state.rows,
                        p,
                        &state.layout,
                        at.index(caret_char),
                        line_px,
                    );
                    // 光标滚入可视区
                    let view_h = bounds.height;
                    if y < state.scroll_y {
                        state.scroll_y = (y).max(0.0);
                    } else if y + h > state.scroll_y + view_h {
                        state.scroll_y = y + h - view_h;
                    }
                    let max_scroll = (p.min_bounds().height - view_h).max(0.0);
                    state.scroll_y = state.scroll_y.clamp(0.0, max_scroll);
                    let ime: InputMethod<&str> = InputMethod::Enabled {
                        cursor: Rectangle::new(
                            bounds.position()
                                + Vector::new(x, y - state.scroll_y),
                            Size::new(1.0, h.max(line_px)),
                        ),
                        purpose: Purpose::Normal,
                        preedit: None,
                    };
                    shell.request_input_method(&ime);
                }
                if let Some(anchor) = state.blink_anchor {
                    let phase = anchor.elapsed().as_millis() % BLINK_MS;
                    if phase < BLINK_MS {
                        shell.request_redraw_at(
                            Instant::now() + Duration::from_millis((BLINK_MS - phase) as u64),
                        );
                    }
                }
            }
            _ => {}
        }
    }
}

impl Input<'_> {
    /// 光标处的标签区间（命中范围内 = 整标签交互对象）。
    fn tag_at(&self, caret: usize) -> Option<(usize, usize)> {
        refs::scan_hits(&self.draft.text, self.refs).into_iter().find_map(|h| {
            let end = h.start + h.len;
            (caret > h.start && caret < end).then_some((h.start, end))
        })
    }

    fn handle_key(
        &self,
        key: &Key,
        modifiers: keyboard::Modifiers,
        text: Option<&str>,
        state: &mut InputState,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
    ) {
        let caret = self.draft.caret.clamp(0, self.draft.text.len());
        let anchor = self.draft.anchor.clamp(0, self.draft.text.len());
        let extend = modifiers.shift();
        let (sel_a, sel_b) = self.draft.selection();
        let has_sel = sel_b > sel_a;

        // 组合键
        if modifiers.control() || modifiers.logo() {
            if let Key::Character(c) = &key {
                match c.to_lowercase().as_str() {
                    "c" => {
                        if has_sel {
                            clipboard.write(ClipboardKind::Standard, self.draft.selected_text());
                        }
                        shell.capture_event();
                        return;
                    }
                    "x" => {
                        if has_sel {
                            clipboard.write(ClipboardKind::Standard, self.draft.selected_text());
                            let (t, c) = cut_selection(&self.draft.text, caret, anchor);
                            self.publish(shell, InputEvent::Edited { text: t, caret: c, anchor: c });
                        }
                        shell.capture_event();
                        return;
                    }
                    "v" => {
                        if let Some(t) = clipboard.read(ClipboardKind::Standard) {
                            let (t, c) = insert_at(&self.draft.text, caret, anchor, &t);
                            self.publish(shell, InputEvent::Edited { text: t, caret: c, anchor: c });
                        }
                        shell.capture_event();
                        return;
                    }
                    "a" => {
                        let len = self.draft.text.len();
                        self.publish(shell, InputEvent::Select { caret: len, anchor: 0 });
                        return;
                    }
                    "z" => {
                        self.publish(
                            shell,
                            if extend { InputEvent::Redo } else { InputEvent::Undo },
                        );
                        return;
                    }
                    "y" => {
                        self.publish(shell, InputEvent::Redo);
                        return;
                    }
                    _ => {}
                }
            }
            if matches!(key, Key::Named(Named::Home)) {
                self.publish(shell, InputEvent::Select { caret: 0, anchor: if extend { anchor } else { 0 } });
                return;
            }
            if matches!(key, Key::Named(Named::End)) {
                let len = self.draft.text.len();
                self.publish(
                    shell,
                    InputEvent::Select { caret: len, anchor: if extend { anchor } else { len } },
                );
                return;
            }
            return;
        }

        // 候选浮层开启：方向 / Enter / Esc 转交
        if self.picker_open {
            match key {
                Key::Named(Named::ArrowUp) => {
                    self.publish(shell, InputEvent::PickerNav(-1));
                    return;
                }
                Key::Named(Named::ArrowDown) => {
                    self.publish(shell, InputEvent::PickerNav(1));
                    return;
                }
                Key::Named(Named::Enter) => {
                    self.publish(shell, InputEvent::PickerCommit);
                    return;
                }
                Key::Named(Named::Escape) => {
                    self.publish(shell, InputEvent::PickerClose);
                    return;
                }
                _ => {}
            }
        }

        match key {
            Key::Named(Named::ArrowLeft) => {
                let next = if modifiers.control() {
                    word_edge(&self.draft.text, caret, true)
                } else {
                    text_prev(&self.draft.text, caret)
                };
                self.publish(
                    shell,
                    InputEvent::Select {
                        caret: next,
                        anchor: if extend { anchor } else { next },
                    },
                );
            }
            Key::Named(Named::ArrowRight) => {
                let next = if modifiers.control() {
                    word_edge(&self.draft.text, caret, false)
                } else {
                    text_next(&self.draft.text, caret)
                };
                self.publish(
                    shell,
                    InputEvent::Select {
                        caret: next,
                        anchor: if extend { anchor } else { next },
                    },
                );
            }
            Key::Named(Named::ArrowUp) | Key::Named(Named::ArrowDown) => {
                self.move_line(key, state, extend, shell);
            }
            Key::Named(Named::Home) => {
                let next = line_start_at(&self.draft.text, caret);
                self.publish(
                    shell,
                    InputEvent::Select {
                        caret: next,
                        anchor: if extend { anchor } else { next },
                    },
                );
            }
            Key::Named(Named::End) => {
                let next = line_end_at(&self.draft.text, caret);
                self.publish(
                    shell,
                    InputEvent::Select {
                        caret: next,
                        anchor: if extend { anchor } else { next },
                    },
                );
            }
            Key::Named(Named::Backspace) => {
                if has_sel {
                    let (t, c) = cut_selection(&self.draft.text, caret, anchor);
                    self.publish(shell, InputEvent::Edited { text: t, caret: c, anchor: c });
                } else if let Some(tag) = self.tag_backspace_target(caret) {
                    // 标签边界退格 = 整删标签
                    let (t, c) = cut_range(&self.draft.text, tag.0, tag.1);
                    self.publish(shell, InputEvent::Edited { text: t, caret: c, anchor: c });
                } else {
                    let prev = text_prev(&self.draft.text, caret);
                    if prev < caret {
                        let (t, c) = cut_range(&self.draft.text, prev, caret);
                        self.publish(shell, InputEvent::Edited { text: t, caret: c, anchor: c });
                    }
                }
            }
            Key::Named(Named::Delete) => {
                if has_sel {
                    let (t, c) = cut_selection(&self.draft.text, caret, anchor);
                    self.publish(shell, InputEvent::Edited { text: t, caret: c, anchor: c });
                } else if let Some(tag) = self.tag_forward_target(caret) {
                    let (t, c) = cut_range(&self.draft.text, tag.0, tag.1);
                    self.publish(shell, InputEvent::Edited { text: t, caret: c, anchor: c });
                } else {
                    let next = text_next(&self.draft.text, caret);
                    if next > caret {
                        let (t, c) = cut_range(&self.draft.text, caret, next);
                        self.publish(shell, InputEvent::Edited { text: t, caret: c, anchor: c });
                    }
                }
            }
            Key::Named(Named::Enter) => {
                if extend {
                    let (t, c) = insert_at(&self.draft.text, caret, anchor, "\n");
                    self.publish(shell, InputEvent::Edited { text: t, caret: c, anchor: c });
                } else {
                    self.publish(shell, InputEvent::Send);
                }
            }
            Key::Named(Named::Space) => {
                let t = text.filter(|t| !t.is_empty()).unwrap_or(" ").to_string();
                let (nt, c) = insert_at(&self.draft.text, caret, anchor, &t);
                self.publish(shell, InputEvent::Edited { text: nt, caret: c, anchor: c });
            }
            Key::Character(_) => {
                if let Some(t) = text.filter(|t| !t.is_empty()) {
                    let (nt, c) = insert_at(&self.draft.text, caret, anchor, t);
                    self.publish(shell, InputEvent::Edited { text: nt, caret: c, anchor: c });
                }
            }
            _ => {}
        }
    }

    /// 上 / 下移一行：按光标横坐标在相邻行取位（首尾行 = 移到行首 / 行尾）。
    fn move_line(
        &self,
        key: &Key,
        state: &mut InputState,
        extend: bool,
        shell: &mut Shell<'_, Message>,
    ) {
        let line_px = state.size * LINE_FACTOR;
        let caret = self.draft.caret.clamp(0, self.draft.text.len());
        let up = matches!(key, Key::Named(Named::ArrowUp));
        // 建段态仍含组合串（组合刚结束的一帧）：行表按排版文本，退回行首 / 行尾
        if state.preedit.is_some() {
            let next = if up {
                line_start_at(&self.draft.text, caret)
            } else {
                line_end_at(&self.draft.text, caret)
            };
            self.publish(
                shell,
                InputEvent::Select { caret: next, anchor: if extend { self.draft.anchor } else { next } },
            );
            return;
        }
        let Some(p) = state.paragraph.as_ref() else {
            return;
        };
        let idx = char_index(&state.content, caret);
        let (x, y, _) = caret_row_pos(&state.rows, p, &state.content, idx, line_px);
        let target_y = if up { y - line_px } else { y + line_px };
        let content_h = p.min_bounds().height;
        let next = if target_y < 0.0 || target_y >= content_h {
            if up {
                0
            } else {
                self.draft.text.len()
            }
        } else {
            hit_offset_rows(
                &state.rows,
                p,
                &state.content,
                Point::new(x, target_y + line_px / 2.0),
                line_px,
            )
        };
        self.publish(
            shell,
            InputEvent::Select {
                caret: next,
                anchor: if extend { self.draft.anchor } else { next },
            },
        );
    }

    /// 退格整删目标：光标恰在标签末尾（无选区）时返回标签区间。
    fn tag_backspace_target(&self, caret: usize) -> Option<(usize, usize)> {
        if self.draft.anchor != caret {
            return None;
        }
        refs::scan_hits(&self.draft.text, self.refs).into_iter().find_map(|h| {
            let end = h.start + h.len;
            (caret == end).then_some((h.start, end))
        })
    }

    /// Del 整删目标：光标恰在标签开头（无选区）时返回标签区间。
    fn tag_forward_target(&self, caret: usize) -> Option<(usize, usize)> {
        if self.draft.anchor != caret {
            return None;
        }
        refs::scan_hits(&self.draft.text, self.refs).into_iter().find_map(|h| {
            (caret == h.start).then_some((h.start, h.start + h.len))
        })
    }
}

/// 单字符回退（字符边界安全）。
fn text_prev(text: &str, caret: usize) -> usize {
    let mut i = caret.min(text.len());
    while !text.is_char_boundary(i) {
        i -= 1;
    }
    text[..i].chars().next_back().map_or(i, |c| i - c.len_utf8())
}

/// 单字符前进。
fn text_next(text: &str, caret: usize) -> usize {
    let mut i = caret.min(text.len());
    while !text.is_char_boundary(i) {
        i += 1;
    }
    text[i..].chars().next().map_or(i, |c| i + c.len_utf8())
}

/// 删除选区（两端排序后切除），返回 (新文本, 新光标)。
fn cut_selection(text: &str, caret: usize, anchor: usize) -> (String, usize) {
    let lo = anchor.min(caret).min(text.len());
    let hi = anchor.max(caret).min(text.len());
    cut_range(text, lo, hi)
}

/// 删除 [lo, hi) 区间。
fn cut_range(text: &str, lo: usize, hi: usize) -> (String, usize) {
    let mut out = String::with_capacity(text.len() - (hi - lo));
    out.push_str(&text[..lo]);
    out.push_str(&text[hi.min(text.len())..]);
    (out, lo)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(text: &str, caret: usize) -> Draft {
        Draft {
            text: text.to_string(),
            caret,
            anchor: caret,
            ..Draft::default()
        }
    }

    #[test]
    fn apply_edited_pushes_undo_and_clears_redo() {
        let mut d = draft("你好", 6);
        d.apply_edited("你好世界".into(), 12, 12);
        assert_eq!(d.text, "你好世界");
        assert_eq!(d.undo.len(), 1);
        d.undo_step();
        assert_eq!(d.text, "你好");
        assert_eq!(d.caret, 6);
        d.redo_step();
        assert_eq!(d.text, "你好世界");
        // 选区未变文本不压栈
        let undo_len = d.undo.len();
        d.apply_edited("你好世界".into(), 6, 0);
        assert_eq!(d.undo.len(), undo_len);
    }

    #[test]
    fn active_mention_requires_line_start_or_whitespace() {
        assert_eq!(draft("#", 1).active_mention_at(), Some(0));
        assert_eq!(draft("问一下 #", 11).active_mention_at(), Some(10));
        // 继续键入不丢触发
        assert_eq!(draft("#笔记", 7).active_mention_at(), Some(0));
        assert_eq!(draft("abc#", 4).active_mention_at(), None);
        // 触发段含空白 = 失活
        assert_eq!(draft("#笔 记", 9).active_mention_at(), None);
        assert_eq!(draft("", 0).active_mention_at(), None);
    }

    #[test]
    fn word_edges_stop_at_whitespace() {
        assert_eq!(word_edge("hello world", 11, true), 6);
        assert_eq!(word_edge("hello world", 0, false), 5);
        // 词中位置先拼回边界；词首（空格后）回退到上一词首
        assert_eq!(word_edge("中文 词", 9, true), 0);
        assert_eq!(word_edge("中文 词", 10, true), 7);
        assert_eq!(word_edge("中文 词", 7, false), 10);
    }

    #[test]
    fn word_range_selects_token_on_double_click() {
        assert_eq!(word_range_at("你好 世界", 0), (0, 6));
        // 词中与词尾都取该词；词首取右侧词
        assert_eq!(word_range_at("你好 世界", 3), (0, 6));
        assert_eq!(word_range_at("你好 世界", 6), (0, 6));
        assert_eq!(word_range_at("你好 世界", 7), (7, 13));
        assert_eq!(word_range_at("你好 世界", 13), (7, 13));
    }

    #[test]
    fn insert_replaces_selection() {
        let (t, c) = insert_at("abcdef", 4, 2, "X");
        assert_eq!(t, "abXef");
        assert_eq!(c, 3);
    }

    #[test]
    fn line_bounds_handle_multibyte_and_newlines() {
        assert_eq!(line_start_at("ab\ncd", 4), 3);
        assert_eq!(line_end_at("ab\ncd", 0), 2);
        assert_eq!(text_prev("好", 3), 0);
        assert_eq!(text_next("好", 0), 3);
    }
}
