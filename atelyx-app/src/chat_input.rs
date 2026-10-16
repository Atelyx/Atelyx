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
    byte_of_char, caret_xy, char_at_point, char_index, hit_offset, line_end_at, line_start_at,
    selection_rects, ParagraphT,
};
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
    size: f32,
    width: f32,
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
    fn stale(&self, content: &str, size: f32, width: f32) -> bool {
        self.paragraph.is_none()
            || self.content.as_str() != content
            || self.size != size
            || (self.width - width).abs() > 0.5
    }
}

/// 逐字符 span 段落（`span_bounds` 按字符给几何——命中测试与光标定位的前提，
/// 与笔记编辑面同口径）；组合期布局。
fn build_paragraph(content: &str, size: f32, width: f32) -> ParagraphT {
    let spans: Vec<Span<'static, (), Font>> =
        content.chars().map(|ch| Span::new(ch.to_string())).collect();
    ParagraphT::with_spans(advanced_text::Text {
        content: spans.as_slice(),
        bounds: Size::new(width, f32::INFINITY),
        size: size.into(),
        line_height: advanced_text::LineHeight::Relative(LINE_FACTOR),
        font: Font::DEFAULT,
        align_x: advanced_text::Alignment::Left,
        align_y: alignment::Vertical::Top,
        shaping: advanced_text::Shaping::Advanced,
        wrapping: advanced_text::Wrapping::default(),
    })
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
            let state = tree.state.downcast_mut::<InputState>();
            if state.stale(text, size, width) {
                state.paragraph = Some(build_paragraph(text, size, width));
                state.content = text.clone();
                state.size = size;
                state.width = width;
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
        let count = state.content.chars().count();
        let origin = bounds.position() + Vector::new(0.0, -state.scroll_y);
        let text_bounds = Rectangle::new(origin, Size::new(bounds.width, f32::INFINITY));

        // # 引用标签高亮（选区样式；正文底下）
        for hit in refs::scan_hits(&self.draft.text, self.refs) {
            let (lo, hi) = (hit.start, hit.start + hit.len);
            for r in selection_rects(
                p,
                char_index(&state.content, lo),
                char_index(&state.content, hi),
            ) {
                renderer.fill_quad(
                    Quad {
                        bounds: Rectangle::new(
                            origin + Vector::new(r.x, r.y + r.height * 0.15),
                            Size::new(r.width, r.height * 0.85),
                        ),
                        border: iced::border::Border { radius: 2.0.into(), ..Default::default() },
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
                char_index(&state.content, sel_a),
                char_index(&state.content, sel_b),
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
            viewport,
        );

        // 占位符
        if self.draft.text.is_empty() && self.draft.preedit.is_none() {
            renderer.fill_text(
                advanced_text::Text {
                    content: self.placeholder.to_string(),
                    bounds: Size::new(bounds.width, line_px),
                    size: state.size.into(),
                    line_height: advanced_text::LineHeight::Relative(LINE_FACTOR),
                    font: Font::DEFAULT,
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
        let caret = self.draft.caret.clamp(0, self.draft.text.len());
        let caret_char = char_index(&state.content, caret);
        let (x, y, h) = caret_xy(p, count, caret_char, line_px);
        // 组合串画在光标处（组合期恒可见）
        if let Some(pre) = self.draft.preedit.as_deref().filter(|s| !s.is_empty()) {
            renderer.fill_text(
                advanced_text::Text {
                    content: pre.to_string(),
                    bounds: Size::new(f32::INFINITY, h),
                    size: state.size.into(),
                    line_height: advanced_text::LineHeight::Relative(LINE_FACTOR),
                    font: Font::DEFAULT,
                    align_x: advanced_text::Alignment::Left,
                    align_y: alignment::Vertical::Top,
                    shaping: advanced_text::Shaping::Auto,
                    wrapping: advanced_text::Wrapping::None,
                },
                origin + Vector::new(x, y),
                pal::TEXT_SECONDARY,
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
                bounds: Rectangle::new(
                    origin + Vector::new(x, y),
                    Size::new(1.5, h.max(line_px)),
                ),
                ..Quad::default()
            },
            pal::TEXT_PRIMARY,
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
        cursor: mouse::Cursor,
        _renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let state = tree.state.downcast_mut::<InputState>();
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
                self.publish(shell, InputEvent::Focus);
                state.dragging = true;
                let local = Point::new(pos.x, pos.y + state.scroll_y);
                // 双击 = 选词
                if let Some((t, p)) = state.last_click {
                    if t.elapsed().as_millis() <= DOUBLE_CLICK_MS
                        && { let d = p - pos; d.x * d.x + d.y * d.y <= DOUBLE_CLICK_DRIFT * DOUBLE_CLICK_DRIFT }
                    {
                        let count = state.content.chars().count();
                        let byte = state.paragraph.as_ref().map_or(0, |p| {
                            byte_of_char(&state.content, char_at_point(p, count, local))
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
                    hit_offset(p, &state.content, local)
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
                    let caret = hit_offset(p, &state.content, local);
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
                let count = state.content.chars().count();
                let caret = self.draft.caret.clamp(0, self.draft.text.len());
                let caret_char = char_index(&state.content, caret);
                let line_px = state.size * LINE_FACTOR;
                if let Some(p) = state.paragraph.as_ref() {
                    let (x, y, h) = caret_xy(p, count, caret_char, line_px);
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
        let Some(p) = state.paragraph.as_ref() else {
            return;
        };
        let line_px = state.size * LINE_FACTOR;
        let count = state.content.chars().count();
        let caret = self.draft.caret.clamp(0, self.draft.text.len());
        let idx = char_index(&state.content, caret);
        let (x, y, _) = caret_xy(p, count, idx, line_px);
        let up = matches!(key, Key::Named(Named::ArrowUp));
        let target_y = if up { y - line_px } else { y + line_px };
        let content_h = p.min_bounds().height;
        let next = if target_y < 0.0 || target_y >= content_h {
            if up {
                0
            } else {
                self.draft.text.len()
            }
        } else {
            let ci = char_at_point(p, count, Point::new(x, target_y + line_px / 2.0));
            byte_of_char(&state.content, ci)
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
