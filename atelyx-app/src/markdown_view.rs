//! Markdown 只读渲染：块规格 → iced 控件（阅读面，不含编辑态与源码标记）。
//! 块间距 = 源码空行数 × 行高（块级不设外边距）；公式与 raw HTML 降级源码文本，
//! 图片以替代文字占位，链接只作样式。

use atelyx_core::markdown::{
    parse_inline, parse_markdown, InlineSpan, LinkForm, MarkdownBlock, MarkdownDocument,
    MarkdownListItem, MarkdownTableCell, ParseOptions,
};
use iced::widget::text::{LineHeight, Rich, Span, Wrapping};
use iced::widget::{
    button, checkbox, column, container, row, scrollable, table, text, Space, Stack,
};
use iced::{Border, Color, Element, Font, Length, Padding};

use crate::Message;

/// 正文基准字号与行高倍数（空行行的间距按此折算；编辑面空行预编辑同源）。
pub(crate) const BASE_SIZE: f32 = 14.0;
pub(crate) const LINE_HEIGHT: f32 = 1.625;
/// 代码字号与代码块最多展示行数（超出转块内纵向滚动）。
const MONO_SIZE: f32 = 12.0;
const CODE_MAX_LINES: usize = 18;
/// 列表标记悬挂列宽（内容与标记分列，嵌套列表随之逐层缩进）。
const LIST_MARKER_W: f32 = 20.0;

/// 编辑档可点的链接目标（外链 / 仓库路径 / wiki），命中后由视图层打开。
#[derive(Clone)]
pub struct LinkTarget {
    pub href: String,
    pub form: LinkForm,
}

/// 一个顶层块的渲染产物：源区间 + 前置空行数 + 控件。
/// 编辑档与只读档由同一入口产出，非活动块据此逐点一致（档位切换不重排）。
pub struct BlockChunk<'a> {
    pub from: usize,
    pub to: usize,
    /// 与上一块之间的源码空行数（首块为 0；块间距 = 空行数 × 行高）。
    pub gap_before: usize,
    /// 源码态排版口径（编辑档回显源码时与该块类型一致）。
    pub source: SourceKind,
    pub element: Element<'a, Message>,
}

/// 源码态排版口径。
#[derive(Clone, Copy)]
pub enum SourceKind {
    /// 散文类块：字号与行高随块类型（标题逐级放大）。
    Prose { size: f32, line_height: f32 },
    /// 代码类块：等宽小字。
    Mono { size: f32 },
}

/// 块渲染共享入口：`interactive` 为真时链接可点、任务框可勾（编辑档）。
pub fn block_chunks<'a>(
    source: &'a str,
    blocks: &'a [MarkdownBlock],
    theme: &iced::Theme,
    interactive: bool,
) -> Vec<BlockChunk<'a>> {
    let render = Render { pal: Palette::from_theme(theme), opts: link_options(), interactive };
    render.chunks(source, blocks)
}

/// 阅读面入口：正文列放进滚动容器。
pub fn document_view<'a>(doc: &'a MarkdownDocument, theme: &iced::Theme) -> Element<'a, Message> {
    let mut body = column![].width(Length::Fill).spacing(0);
    for chunk in block_chunks(&doc.source, &doc.blocks, theme, false) {
        if chunk.gap_before > 0 {
            body = body.push(gap(chunk.gap_before));
        }
        body = body.push(chunk.element);
    }
    scrollable(container(body).width(Length::Fill).padding(16.0))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// 编辑面块元数据。
#[derive(Clone)]
pub struct BlockMeta {
    pub from: usize,
    pub to: usize,
    /// 列表块（Enter 延续项、空项退格用）。
    pub list: bool,
    /// 富渲染与源码渲染几何是否一致（一致才可按源文本精确换算落点）。
    pub exact: bool,
    pub source: SourceKind,
}

/// 富渲染与源码渲染几何是否一致：只有「纯文本段落」（无任何隐藏标记）两者逐点相同——
/// 落点可按源文本精确换算，且揭示不引起重排；其余块富形态带标记或缩进列，只能按块边界吸附。
fn rich_equals_source(source: &str, from: usize, to: usize, block: &MarkdownBlock) -> bool {
    let MarkdownBlock::Paragraph { inline, .. } = block else {
        return false;
    };
    if to <= from || to > source.len() {
        return false;
    }
    let slice = &source[from..to];
    if slice.starts_with(char::is_whitespace) {
        return false;
    }
    let mut len = 0;
    for span in inline {
        match span {
            InlineSpan::Text { text, .. } => len += text.len(),
            _ => return false,
        }
    }
    len == slice.len()
}

/// 编辑面块元数据（按顶层块序）。
pub fn block_meta(source: &str, blocks: &[MarkdownBlock]) -> Vec<BlockMeta> {
    blocks
        .iter()
        .map(|b| {
            let (from, to) = (block_from(b), block_to(b));
            BlockMeta {
                from,
                to,
                list: matches!(b, MarkdownBlock::List { .. }),
                exact: rich_equals_source(source, from, to, b),
                source: block_source_style(b),
            }
        })
        .collect()
}

/// 空文档的空块元数据（编辑档给无块正文一个光标宿主）。
pub fn empty_block_meta() -> BlockMeta {
    BlockMeta {
        from: 0,
        to: 0,
        list: false,
        exact: true,
        source: SourceKind::Prose { size: BASE_SIZE, line_height: LINE_HEIGHT },
    }
}

/// 阅读面解析入口：与渲染面共用同一套链接形态判定。
pub fn parse_document(source: &str) -> MarkdownDocument {
    parse_markdown(source, &link_options())
}

/// 解析选项：非外链地址一律判为仓库内路径（不做存在性判定）。
pub(crate) fn link_options() -> ParseOptions {
    ParseOptions {
        resolve_link: Some(Box::new(|href: &str| {
            if href.trim().is_empty() {
                LinkForm::Plain
            } else {
                LinkForm::Path
            }
        })),
        ..ParseOptions::default()
    }
}

/// 主题配色：正文用文字色，弱化/描边/面板底按文字色透明度派生。
#[derive(Clone, Copy)]
struct Palette {
    text: Color,
    muted: Color,
    accent: Color,
    border: Color,
    panel: Color,
    mark_bg: Color,
}

impl Palette {
    fn from_theme(theme: &iced::Theme) -> Self {
        let p = theme.palette();
        Self {
            text: p.text,
            muted: p.text.scale_alpha(0.6),
            accent: p.primary,
            border: p.text.scale_alpha(0.2),
            panel: p.text.scale_alpha(0.06),
            mark_bg: Color::from_rgba(1.0, 0.84, 0.28, 0.35),
        }
    }
}

/// 行内样式累加器：容器片段（加粗/斜体/删除线/高亮）向下传递，叶子片段据它产出 span。
#[derive(Clone, Copy)]
struct TextStyle {
    font: Font,
    color: Option<Color>,
    strike: bool,
    underline: bool,
    bg: Option<Color>,
    /// 胶囊形态（标签 / mention）：高亮带左右内边距。
    chip: bool,
}

impl TextStyle {
    fn base() -> Self {
        Self {
            font: Font::DEFAULT,
            color: None,
            strike: false,
            underline: false,
            bg: None,
            chip: false,
        }
    }
}

/// 标题字号：按层级逐级放大。
fn heading_size(level: u8) -> f32 {
    BASE_SIZE
        * match level {
            1 => 1.5,
            2 => 1.35,
            3 => 1.2,
            4 => 1.1,
            5 => 1.0,
            _ => 0.9,
        }
}

/// 块的源码态口径：结构块与散文块统一按整块源码回显，字号随块类型。
fn block_source_style(block: &MarkdownBlock) -> SourceKind {
    match block {
        MarkdownBlock::Heading { level, .. } => {
            SourceKind::Prose { size: heading_size(*level), line_height: 1.3 }
        }
        MarkdownBlock::Paragraph { .. }
        | MarkdownBlock::List { .. }
        | MarkdownBlock::Blockquote { .. } => {
            SourceKind::Prose { size: BASE_SIZE, line_height: LINE_HEIGHT }
        }
        MarkdownBlock::FootnoteDef { .. } => {
            SourceKind::Prose { size: BASE_SIZE * 0.9, line_height: LINE_HEIGHT }
        }
        MarkdownBlock::FencedCode { .. }
        | MarkdownBlock::IndentedCode { .. }
        | MarkdownBlock::Table { .. }
        | MarkdownBlock::HtmlBlock { .. }
        | MarkdownBlock::MathBlock { .. }
        | MarkdownBlock::Hr { .. } => SourceKind::Mono { size: MONO_SIZE },
    }
}

struct Render {
    pal: Palette,
    opts: ParseOptions,
    /// 链接可点、任务框可勾（编辑档）；只读档为假。
    interactive: bool,
}

impl Render {
    /// 块序列：每块附源区间与前置空行数（间距由调用方按空行数补行高）。
    fn chunks<'a>(&self, source: &'a str, blocks: &'a [MarkdownBlock]) -> Vec<BlockChunk<'a>> {
        let mut out = Vec::with_capacity(blocks.len());
        let mut prev: Option<&MarkdownBlock> = None;
        for block in blocks {
            let gap_before = prev.map_or(0, |p| {
                blank_line_count(source, block_to(p), block_from(block))
            });
            out.push(BlockChunk {
                from: block_from(block),
                to: block_to(block),
                gap_before,
                source: block_source_style(block),
                element: self.block(source, block),
            });
            prev = Some(block);
        }
        out
    }

    /// 子块列表 → 列（块间按源码空行数补行高间距）。
    fn blocks_column<'a>(&self, source: &'a str, blocks: &'a [MarkdownBlock]) -> Element<'a, Message> {
        let mut col = column![].width(Length::Fill).spacing(0);
        for chunk in self.chunks(source, blocks) {
            if chunk.gap_before > 0 {
                col = col.push(gap(chunk.gap_before));
            }
            col = col.push(chunk.element);
        }
        col.into()
    }

    fn block<'a>(&self, source: &'a str, block: &'a MarkdownBlock) -> Element<'a, Message> {
        match block {
            MarkdownBlock::Heading { level, inline, .. } => {
                let mut st = TextStyle::base();
                st.font.weight = iced::font::Weight::Semibold;
                rich(
                    self.spans(source, inline, st),
                    heading_size(*level),
                    1.3,
                    None,
                    iced::alignment::Horizontal::Left,
                )
            }
            MarkdownBlock::Paragraph { inline, .. } => rich(
                self.spans(source, inline, TextStyle::base()),
                BASE_SIZE,
                LINE_HEIGHT,
                None,
                iced::alignment::Horizontal::Left,
            ),
            MarkdownBlock::Blockquote { callout, children, .. } => {
                self.blockquote(source, callout.as_deref(), children)
            }
            MarkdownBlock::List { ordered, items, .. } => self.list(source, *ordered, items),
            MarkdownBlock::FencedCode { lang, code, .. } => self.code_block(lang, code),
            MarkdownBlock::IndentedCode { code, .. } => self.code_block("", code),
            MarkdownBlock::Hr { .. } => self.hr(),
            MarkdownBlock::Table { header, aligns, rows, .. } => {
                self.table(source, header, aligns, rows)
            }
            MarkdownBlock::MathBlock { tex, .. } => self.source_block(format!("$$\n{tex}\n$$")),
            MarkdownBlock::HtmlBlock { html, .. } => self.source_block(html.clone()),
            MarkdownBlock::FootnoteDef { label, inline, .. } => {
                self.footnote(source, label, inline)
            }
        }
    }

    /// 引用 / callout：左侧竖线 + 缩进；callout 另加类型徽标与底色。
    /// 竖线走 Stack 覆盖层：滚动容器内 `Fill` 高度会被压缩，而 Stack 的非基础层
    /// 用收紧限高重新布局，`Fill` 高度即内容高度。
    fn blockquote<'a>(
        &self,
        source: &'a str,
        callout: Option<&str>,
        children: &'a [MarkdownBlock],
    ) -> Element<'a, Message> {
        let line = callout.map(|name| self.callout_color(name)).unwrap_or(self.pal.border);
        let content = self.blocks_column(source, children);
        let content = match callout {
            Some(name) => {
                let badge = container(
                    text(name.to_uppercase())
                        .size(11)
                        .font(Font { weight: iced::font::Weight::Semibold, ..Font::DEFAULT })
                        .color(line),
                )
                .padding([1.0, 6.0])
                .style(move |_theme: &iced::Theme| container::Style {
                    background: Some(line.scale_alpha(0.16).into()),
                    border: Border { radius: 8.0.into(), ..Border::default() },
                    ..Default::default()
                });
                column![badge, content].width(Length::Fill).spacing(4.0).into()
            }
            None => content,
        };

        let bar = container(Space::new())
            .width(Length::Fixed(3.0))
            .height(Length::Fill)
            .style(move |_theme: &iced::Theme| container::Style {
                background: Some(line.into()),
                ..Default::default()
            });
        let bg = callout.map(|_| self.pal.text.scale_alpha(0.04));
        let inner = container(content)
            .width(Length::Fill)
            .padding(Padding { left: 10.0, ..Padding::default() })
            .style(move |_theme: &iced::Theme| container::Style {
                background: bg.map(Into::into),
                ..Default::default()
            });
        Stack::new().push(inner).push(bar).width(Length::Fill).into()
    }

    fn list<'a>(
        &self,
        source: &'a str,
        ordered: bool,
        items: &'a [MarkdownListItem],
    ) -> Element<'a, Message> {
        let mut col = column![].width(Length::Fill).spacing(0);
        for (index, item) in items.iter().enumerate() {
            if index > 0 {
                let n = blank_line_count(source, items[index - 1].to, item.from);
                if n > 0 {
                    col = col.push(gap(n));
                }
            }
            col = col.push(self.list_item(source, ordered, item));
        }
        col.into()
    }

    fn list_item<'a>(
        &self,
        source: &'a str,
        ordered: bool,
        item: &'a MarkdownListItem,
    ) -> Element<'a, Message> {
        let raw = &source[item.from..item.to];
        let (marker_len, ordinal) = item_marker(raw);
        let content_from = item.from + marker_len;
        // 首段内容止于第一个嵌套子块起点（项内后续段落等归 children）
        let content_end = item.children.first().map(block_from).unwrap_or(item.to);
        let by = content_end.max(content_from);
        let content_to = content_from + source[content_from..by].trim_end().len();

        // 项区间跨引用块时续行带 `>` 前缀：剥掉后按独立区间解析（阅读面不依赖源偏移）
        let content = strip_quote_marks(&source[content_from..content_to]);
        let spans = parse_inline(&content, 0, content.len(), &self.opts);
        let mut body = column![].width(Length::Fill).spacing(0);
        if !spans.is_empty() {
            body = body.push(rich(
                self.spans(&content, &spans, TextStyle::base()),
                BASE_SIZE,
                LINE_HEIGHT,
                None,
                iced::alignment::Horizontal::Left,
            ));
        }
        body = body.push(self.blocks_column(source, &item.children));

        // 任务框取代圆点/序号；有序项沿用源序号文本
        let marker: Element<'_, Message> = if item.task {
            let box_widget = checkbox(item.checked);
            let box_widget = if self.interactive {
                box_widget.on_toggle(move |_| Message::ToggleTask(item.from))
            } else {
                box_widget
            };
            container(box_widget)
                .width(Length::Fixed(LIST_MARKER_W))
                .padding(Padding { top: 2.0, ..Padding::default() })
                .into()
        } else {
            let label = if ordered { ordinal } else { "•".to_string() };
            container(text(label).size(BASE_SIZE).color(self.pal.muted))
                .width(Length::Fixed(LIST_MARKER_W))
                .into()
        };
        row![marker, body].width(Length::Fill).into()
    }

    /// 围栏 / 缩进代码块：语言标签 + 复制按钮 + 等宽正文（不换行，超宽横向滚动）。
    fn code_block<'a>(&self, lang: &str, code: &str) -> Element<'a, Message> {
        let panel = self.pal.panel;
        let border = self.pal.border;
        let label = if lang.trim().is_empty() { "代码".to_string() } else { lang.to_string() };
        let head = container(
            row![
                text(label).size(11).font(Font::MONOSPACE).color(self.pal.muted),
                Space::new().width(Length::Fill),
                button(text("复制").size(11))
                    .padding([2.0, 6.0])
                    .style(button::text)
                    .on_press(Message::CopyCode(code.to_string())),
            ]
            .width(Length::Fill)
            .spacing(8.0)
            .align_y(iced::alignment::Vertical::Center),
        )
        .width(Length::Fill)
        .padding([3.0, 6.0])
        .style(move |_theme: &iced::Theme| container::Style {
            background: Some(panel.into()),
            ..Default::default()
        });

        let lines = code.trim_end_matches('\n').lines().count().max(1);
        let height = lines.min(CODE_MAX_LINES) as f32 * MONO_SIZE * 1.4 + 16.0;
        let body = scrollable(
            container(text(code.to_string()).size(MONO_SIZE).font(Font::MONOSPACE).wrapping(Wrapping::None))
                .padding(8.0),
        )
        .direction(scrollable::Direction::Both {
            vertical: scrollable::Scrollbar::new().width(6.0),
            horizontal: scrollable::Scrollbar::new().width(6.0),
        })
        .width(Length::Fill)
        .height(Length::Fixed(height));

        container(column![head, body].width(Length::Fill))
            .width(Length::Fill)
            .style(move |_theme: &iced::Theme| container::Style {
                background: Some(panel.into()),
                border: Border { color: border, width: 1.0, radius: 6.0.into() },
                ..Default::default()
            })
            .into()
    }

    /// 表格：交给 iced 的 table 部件——分隔线由表格按列/行边界绘制（单元格不各自描边），
    /// 同一行的单元格高度不齐也不会把边框画到行中间。
    fn table<'a>(
        &self,
        source: &'a str,
        header: &[MarkdownTableCell],
        aligns: &'a [String],
        rows: &'a [Vec<MarkdownTableCell>],
    ) -> Element<'a, Message> {
        let cols = header
            .len()
            .max(aligns.len())
            .max(rows.iter().map(Vec::len).max().unwrap_or(0))
            .max(1);
        let border = self.pal.border;
        let pal = self.pal;
        let columns = (0..cols).map(|i| {
            let head = self.cell(source, header.get(i), aligns.get(i), true);
            // 单元格生成器随行闭包独立持有调色板与解析选项，不借用 self
            let cells = Render { pal, opts: link_options(), interactive: self.interactive };
            table::column(head, move |row: usize| {
                let cell = rows.get(row).and_then(|r| r.get(i));
                cells.cell(source, cell, aligns.get(i), false)
            })
            .width(Length::Fill)
        });
        let grid = table::table(columns, 0..rows.len())
            .width(Length::Fill)
            .padding(0.0)
            .separator(1.0);
        container(grid)
            .width(Length::Fill)
            .style(move |_theme: &iced::Theme| container::Style {
                border: Border { color: border, width: 1.0, ..Border::default() },
                ..Default::default()
            })
            .into()
    }

    fn cell<'a>(
        &self,
        source: &str,
        cell: Option<&MarkdownTableCell>,
        align: Option<&String>,
        header: bool,
    ) -> Element<'a, Message> {
        let spans = match cell {
            Some(c) if c.to > c.from => parse_inline(source, c.from, c.to, &self.opts),
            _ => Vec::new(),
        };
        let bg = if header { Some(self.pal.panel) } else { None };
        let border = self.pal.border;
        let alignment = match align.map(String::as_str) {
            Some("center") => iced::alignment::Horizontal::Center,
            Some("right") => iced::alignment::Horizontal::Right,
            _ => iced::alignment::Horizontal::Left,
        };
        container(rich(
            self.spans(source, &spans, TextStyle::base()),
            BASE_SIZE,
            LINE_HEIGHT,
            None,
            alignment,
        ))
        .width(Length::Fill)
        .padding([4.0, 8.0])
        .style(move |_theme: &iced::Theme| container::Style {
            background: bg.map(Into::into),
            border: Border { color: border, width: 1.0, ..Border::default() },
            ..Default::default()
        })
        .into()
    }

    /// 公式 / raw HTML 的降级形态：源码文本放进等宽底块。
    fn source_block<'a>(&self, content: String) -> Element<'a, Message> {
        let panel = self.pal.panel;
        let border = self.pal.border;
        container(
            text(content).size(MONO_SIZE).font(Font::MONOSPACE).color(self.pal.muted),
        )
        .width(Length::Fill)
        .padding(8.0)
        .style(move |_theme: &iced::Theme| container::Style {
            background: Some(panel.into()),
            border: Border { color: border, width: 1.0, radius: 4.0.into() },
            ..Default::default()
        })
        .into()
    }

    fn hr<'a>(&self) -> Element<'a, Message> {
        let color = self.pal.border;
        let line = container(Space::new().height(Length::Fixed(1.0)))
            .width(Length::Fill)
            .style(move |_theme: &iced::Theme| container::Style {
                background: Some(color.into()),
                ..Default::default()
            });
        let half = (gap_height() - 1.0) / 2.0;
        container(line)
            .width(Length::Fill)
            .padding(Padding { top: half, bottom: half, ..Padding::default() })
            .into()
    }

    fn footnote<'a>(&self, source: &str, label: &str, inline: &[InlineSpan]) -> Element<'a, Message> {
        let st = TextStyle { color: Some(self.pal.muted), ..TextStyle::base() };
        let mut spans = vec![
            self.leaf(
                format!("[{label}]"),
                TextStyle { color: Some(self.pal.accent), ..TextStyle::base() },
            ),
            self.leaf(" ".to_string(), st),
        ];
        spans.extend(self.spans(source, inline, st));
        let line = self.pal.border;
        let bar = container(Space::new())
            .width(Length::Fixed(2.0))
            .height(Length::Fill)
            .style(move |_theme: &iced::Theme| container::Style {
                background: Some(line.into()),
                ..Default::default()
            });
        let content = container(rich(
            spans,
            BASE_SIZE * 0.9,
            LINE_HEIGHT,
            Some(self.pal.muted),
            iced::alignment::Horizontal::Left,
        ))
        .width(Length::Fill)
        .padding(Padding { left: 8.0, ..Padding::default() });
        Stack::new().push(content).push(bar).width(Length::Fill).into()
    }

    fn callout_color(&self, name: &str) -> Color {
        callout_color(name).unwrap_or(self.pal.accent)
    }

    // ===== 行内 =====

    fn spans<'a>(&self, source: &str, list: &[InlineSpan], st: TextStyle) -> Vec<Span<'a, LinkTarget, Font>> {
        let mut out = Vec::new();
        for span in list {
            self.push_span(source, span, st, &mut out);
        }
        out
    }

    fn push_span<'a>(
        &self,
        source: &str,
        span: &InlineSpan,
        st: TextStyle,
        out: &mut Vec<Span<'a, LinkTarget, Font>>,
    ) {
        match span {
            InlineSpan::Text { text, .. } => out.push(self.leaf(text.clone(), st)),
            InlineSpan::Strong { children, .. } => {
                let inner = TextStyle { font: Font { weight: iced::font::Weight::Bold, ..st.font }, ..st };
                for c in children {
                    self.push_span(source, c, inner, out);
                }
            }
            InlineSpan::Em { children, .. } => {
                let inner = TextStyle { font: Font { style: iced::font::Style::Italic, ..st.font }, ..st };
                for c in children {
                    self.push_span(source, c, inner, out);
                }
            }
            InlineSpan::Strike { children, .. } => {
                let inner = TextStyle { strike: true, ..st };
                for c in children {
                    self.push_span(source, c, inner, out);
                }
            }
            InlineSpan::Highlight { children, .. } => {
                let inner = TextStyle { bg: Some(self.pal.mark_bg), ..st };
                for c in children {
                    self.push_span(source, c, inner, out);
                }
            }
            InlineSpan::Code { text, .. } => {
                let inner = TextStyle { font: Font::MONOSPACE, bg: Some(self.pal.panel), ..st };
                out.push(self.leaf(text.clone(), inner));
            }
            // 公式降级为源码文本
            InlineSpan::MathInline { from, to, .. } => {
                let inner = TextStyle { color: Some(self.pal.muted), ..st };
                out.push(self.leaf(source[*from..*to].to_string(), TextStyle { font: Font { style: iced::font::Style::Italic, ..inner.font }, ..inner }));
            }
            InlineSpan::Comment { .. } => {}
            InlineSpan::Link { from, to, label, form, href, .. } => match form {
                LinkForm::Plain => out.push(self.leaf(source[*from..*to].to_string(), st)),
                LinkForm::External | LinkForm::Path | LinkForm::Wiki => {
                    let inner = TextStyle { color: Some(self.pal.accent), underline: true, ..st };
                    let leaf = self.leaf(label.clone(), inner);
                    out.push(self.linked(leaf, href.clone(), *form));
                }
                LinkForm::Create => {
                    let inner = TextStyle { color: Some(self.pal.muted), underline: true, ..st };
                    out.push(self.leaf(label.clone(), inner));
                }
            },
            InlineSpan::Autolink { href, label, .. } => {
                let inner = TextStyle { color: Some(self.pal.accent), underline: true, ..st };
                let leaf = self.leaf(label.clone(), inner);
                out.push(self.linked(leaf, href.clone(), LinkForm::External));
            }
            InlineSpan::Wiki { target, label, .. } => {
                let inner = TextStyle { color: Some(self.pal.accent), underline: true, ..st };
                let leaf = self.leaf(label.clone(), inner);
                out.push(self.linked(leaf, target.clone(), LinkForm::Wiki));
            }
            // 图片以替代文字占位（不加载图片字节）
            InlineSpan::Image { src, alt, .. } => {
                let name = if alt.is_empty() { src } else { alt };
                let inner = TextStyle { color: Some(self.pal.muted), ..st };
                out.push(self.leaf(format!("[图片：{name}]"), TextStyle { font: Font { style: iced::font::Style::Italic, ..inner.font }, ..inner }));
            }
            InlineSpan::Tag { tag, .. } => {
                let inner = TextStyle {
                    color: Some(self.pal.accent),
                    bg: Some(self.pal.accent.scale_alpha(0.14)),
                    chip: true,
                    ..st
                };
                out.push(self.leaf(format!("#{tag}"), inner));
            }
            InlineSpan::FootnoteRef { label, .. } => {
                let inner = TextStyle { color: Some(self.pal.accent), ..st };
                out.push(self.leaf(format!("[{label}]"), inner).size(BASE_SIZE * 0.85));
            }
            InlineSpan::Mention { key: _, label, trigger, .. } => {
                let inner = TextStyle {
                    color: Some(self.pal.accent),
                    bg: Some(self.pal.accent.scale_alpha(0.18)),
                    chip: true,
                    ..st
                };
                out.push(self.leaf(format!("{trigger}{label}"), inner));
            }
            // raw HTML 降级为源码文本
            InlineSpan::Html { from, to, .. } => {
                let inner = TextStyle { color: Some(self.pal.muted), ..st };
                out.push(self.leaf(source[*from..*to].to_string(), inner));
            }
            InlineSpan::HardBreak { .. } => out.push(self.leaf("\n".to_string(), st)),
        }
    }

    /// 交互档给链接片段挂触点（只读档不挂，链接仅作样式）。
    fn linked<'a>(
        &self,
        leaf: Span<'a, LinkTarget, Font>,
        href: String,
        form: LinkForm,
    ) -> Span<'a, LinkTarget, Font> {
        if self.interactive {
            leaf.link(LinkTarget { href, form })
        } else {
            leaf
        }
    }

    fn leaf<'a>(&self, content: String, st: TextStyle) -> Span<'a, LinkTarget, Font> {
        let mut span = Span::new(content).font(st.font).strikethrough(st.strike);
        if let Some(color) = st.color {
            span = span.color(color);
        }
        if let Some(bg) = st.bg {
            span = span.background(bg);
            if st.chip {
                span = span.padding([0.0, 3.0]);
            }
        }
        if st.underline {
            span = span.underline(true);
        }
        span
    }
}

/// 行内富文本（默认字色与行内对齐可选，供引用/脚注的弱化文本与表格单元格对齐用）。
/// 链接触点常驻：只读档的行内片段不带链接，处理器不会触发。
fn rich<'a>(
    spans: Vec<Span<'a, LinkTarget, Font>>,
    size: f32,
    line_height: f32,
    color: Option<Color>,
    align: iced::alignment::Horizontal,
) -> Element<'a, Message> {
    let mut text = Rich::<'a, LinkTarget, Message>::with_spans(spans)
        .size(size)
        .line_height(LineHeight::Relative(line_height))
        .align_x(align)
        .width(Length::Fill)
        .on_link_click(|target: LinkTarget| Message::OpenLink(target.href, target.form));
    if let Some(c) = color {
        text = text.color(c);
    }
    text.into()
}

/// 空行间距元素：一个源码空行 = 一行高。
/// 宽度须撑满：零宽行与视口的相交恒为空，编辑面会整行跳过空行的光标与选区底色。
pub fn gap<'a>(lines: usize) -> Element<'a, Message> {
    Space::new()
        .width(Length::Fill)
        .height(Length::Fixed(lines as f32 * gap_height()))
        .into()
}

fn gap_height() -> f32 {
    BASE_SIZE * LINE_HEIGHT
}

/// 区间内的完整空行数（首尾两个片段不算完整行）：行内只有空白或引用标记 `>` 才算空行。
fn blank_line_count(source: &str, from: usize, to: usize) -> usize {
    if to <= from {
        return 0;
    }
    let lines: Vec<&str> = source[from..to].split('\n').collect();
    if lines.len() < 3 {
        return 0;
    }
    lines[1..lines.len() - 1]
        .iter()
        .filter(|line| line.chars().all(|c| c.is_whitespace() || c == '>'))
        .count()
}

/// 剥掉每行行首的引用标记（可嵌套的 `>` 前缀，宽度口径与内核一致）。
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

/// 单层引用前缀宽度（缩进 ≤3 + `>` + 至多一个空格）；非引用行返回 None。
fn quote_prefix_len(text: &str) -> Option<usize> {
    let indent = text.len() - text.trim_start_matches([' ', '\t']).len();
    if indent > 3 || !text[indent..].starts_with('>') {
        return None;
    }
    let after = &text[indent + 1..];
    let spaces = after.len() - after.trim_start_matches([' ', '\t']).len();
    Some(indent + 1 + spaces.min(1))
}

/// 列表项标记（含任务框）的字节长度与有序序号文本；非列表标记返回 (0, "")。
fn item_marker(raw: &str) -> (usize, String) {
    let bytes = raw.as_bytes();
    let mut i = 0;
    while matches!(bytes.get(i), Some(b' ') | Some(b'\t')) {
        i += 1;
    }
    let mut ordinal = String::new();
    match bytes.get(i) {
        Some(b'-') | Some(b'+') | Some(b'*') => i += 1,
        Some(c) if c.is_ascii_digit() => {
            let start = i;
            while matches!(bytes.get(i), Some(c) if c.is_ascii_digit()) {
                i += 1;
            }
            if !matches!(bytes.get(i), Some(b'.') | Some(b')')) {
                return (0, String::new());
            }
            ordinal = raw[start..=i].to_string();
            i += 1;
        }
        _ => return (0, String::new()),
    }
    while matches!(bytes.get(i), Some(b' ') | Some(b'\t')) {
        i += 1;
    }
    if bytes.get(i) == Some(&b'[')
        && matches!(bytes.get(i + 1), Some(b' ') | Some(b'x') | Some(b'X'))
        && bytes.get(i + 2) == Some(&b']')
    {
        i += 3;
        while matches!(bytes.get(i), Some(b' ') | Some(b'\t')) {
            i += 1;
        }
    }
    (i, ordinal)
}

fn block_from(block: &MarkdownBlock) -> usize {
    match block {
        MarkdownBlock::Heading { from, .. }
        | MarkdownBlock::Paragraph { from, .. }
        | MarkdownBlock::Blockquote { from, .. }
        | MarkdownBlock::List { from, .. }
        | MarkdownBlock::FencedCode { from, .. }
        | MarkdownBlock::IndentedCode { from, .. }
        | MarkdownBlock::Hr { from, .. }
        | MarkdownBlock::Table { from, .. }
        | MarkdownBlock::MathBlock { from, .. }
        | MarkdownBlock::HtmlBlock { from, .. }
        | MarkdownBlock::FootnoteDef { from, .. } => *from,
    }
}

fn block_to(block: &MarkdownBlock) -> usize {
    match block {
        MarkdownBlock::Heading { to, .. }
        | MarkdownBlock::Paragraph { to, .. }
        | MarkdownBlock::Blockquote { to, .. }
        | MarkdownBlock::List { to, .. }
        | MarkdownBlock::FencedCode { to, .. }
        | MarkdownBlock::IndentedCode { to, .. }
        | MarkdownBlock::Hr { to, .. }
        | MarkdownBlock::Table { to, .. }
        | MarkdownBlock::MathBlock { to, .. }
        | MarkdownBlock::HtmlBlock { to, .. }
        | MarkdownBlock::FootnoteDef { to, .. } => *to,
    }
}

/// callout 类型色板（未知类型返回 None，由调用方回落强调色）。
fn callout_color(name: &str) -> Option<Color> {
    let rgb = match name.to_ascii_lowercase().as_str() {
        "note" => 0x448aff,
        "abstract" | "summary" | "tldr" | "info" | "todo" => 0x00b0ff,
        "tip" | "success" | "check" | "done" => 0x00b0a6,
        "question" | "help" | "faq" => 0xeb984e,
        "warning" | "caution" => 0xff9500,
        "failure" | "fail" | "missing" | "danger" | "error" | "bug" => 0xff4d4d,
        "important" => 0xff6680,
        "example" => 0xa855f7,
        "quote" | "cite" => 0x808080,
        _ => return None,
    };
    Some(Color::from_rgb8(
        ((rgb >> 16) & 0xff) as u8,
        ((rgb >> 8) & 0xff) as u8,
        (rgb & 0xff) as u8,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_line_count_counts_inner_only() {
        // "甲\n\n乙"：只夹一个空行
        assert_eq!(blank_line_count("甲\n\n乙", 3, 5), 1);
        // "甲\n\n\n乙"：夹两个空行
        assert_eq!(blank_line_count("甲\n\n\n乙", 3, 6), 2);
        // 相邻无空行
        assert_eq!(blank_line_count("甲\n乙", 3, 4), 0);
        // 引用的空延续行（">"）算空行
        assert_eq!(blank_line_count("甲\n>\n乙", 3, 6), 1);
    }

    #[test]
    fn item_marker_reads_bullet_ordinal_and_task() {
        assert_eq!(item_marker("- 甲"), (2, String::new()));
        assert_eq!(item_marker("1. 甲"), (3, "1.".to_string()));
        assert_eq!(item_marker("12) 甲"), (4, "12)".to_string()));
        assert_eq!(item_marker("- [x] 甲"), (6, String::new()));
        assert_eq!(item_marker("- [ ] "), (6, String::new()));
        assert_eq!(item_marker("普通段落"), (0, String::new()));
    }

    #[test]
    fn callout_color_covers_known_types() {
        assert!(callout_color("NOTE").is_some());
        assert_eq!(callout_color("warning"), Some(Color::from_rgb8(0xff, 0x95, 0x00)));
        assert!(callout_color("unknown-type").is_none());
    }

    #[test]
    fn strip_quote_marks_drops_line_prefixes() {
        // 续行的引用标记剥掉，标记后的缩进保留
        assert_eq!(strip_quote_marks("- item\n>   cont"), "- item\n  cont");
        // 嵌套引用逐层剥
        assert_eq!(strip_quote_marks("a\n> > b"), "a\nb");
        // 无引用标记的行（含缩进）不动
        assert_eq!(strip_quote_marks("a\n  lazy"), "a\n  lazy");
    }

    #[test]
    fn block_meta_exact_only_for_plain_paragraphs() {
        // 富渲染与源码渲染几何一致才可精确换算落点：仅纯文本段落成立
        for (src, exact) in [
            ("普通段落", true),
            ("first line\nsecond line", true),
            ("## 标题", false),
            ("- 甲", false),
            ("> 引用", false),
            ("*斜*体", false),
            ("**粗**体", false),
            ("[链接](a.md)", false),
            ("`code`", false),
        ] {
            let doc = parse_markdown(src, &link_options());
            let meta = block_meta(&doc.source, &doc.blocks);
            assert_eq!(meta.first().map(|m| m.exact), Some(exact), "src = {src}");
        }
    }
}
