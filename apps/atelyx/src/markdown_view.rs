//! Markdown 只读渲染：块规格 → iced 控件（阅读面，不含编辑态与源码标记）。
//! 块间距 = 源码空行数 × 行高（块级不设外边距）；公式与 raw HTML 降级源码文本，
//! 独立成段的图片按块渲染（句柄由应用层载入，未载入或行内位置以替代文字占位）。

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use atelyx_core::markdown::{
    parse_inline, parse_markdown, InlineSpan, LinkForm, MarkdownBlock, MarkdownDocument,
    MarkdownListItem, MarkdownTableCell, ParseOptions,
};
use iced::alignment;
use iced::widget::text::{LineHeight, Rich, Span, Wrapping};
use iced::widget::{button, column, container, row, scrollable, Space, Stack};
use iced::{Border, Color, Element, Font, Length, Padding, Size};

use crate::markdown_select::{
    ChatSelection, NoteSelection, SelectRun, SelectScope, SelectableRich,
};
use crate::markdown_table::MarkdownTable;
use crate::ui::controls;
use crate::ui::palette as pal;
use crate::ui::text;
use crate::Message;

/// 已载入的图片句柄（key = 图片 src 原文；载入与缓存管理在应用层，渲染只读）。
pub type ImageHandles = HashMap<String, iced::widget::image::Handle>;

/// 正文基准字号（随界面基准字号缩放）与行高倍数（空行行的间距按此折算；编辑面空行预编辑同源）。
pub(crate) fn base_size() -> f32 {
    crate::ui::palette::fs_body()
}

/// 行尾风格残留：正文换行只由 `\n` 决定，`\r` 既不进排版也不计字符位。
/// 富文本排版按双向文本段落切行，`\r` 与 `\n` 各算一个段落分隔符——CRLF 会排出一根
/// 源码里不存在的空行，且该行没有对应字符（光标落不上去）。
pub(crate) fn is_cr(ch: char) -> bool {
    ch == '\r'
}

/// 撇去不占位的行尾 `\r`：进排版的文本一律经此（原地改动，不额外分配）。
pub(crate) fn without_cr(mut text: String) -> String {
    if text.contains(is_cr) {
        text.retain(|c| !is_cr(c));
    }
    text
}

/// 行高倍数（空行行的间距按此折算；编辑面空行预编辑同源）。
pub(crate) const LINE_HEIGHT: f32 = 1.625;
/// 表格单元格内边距（自然宽同口径计入）。
const CELL_PAD_X: f32 = 8.0;
const CELL_PAD_Y: f32 = 4.0;
/// 单元格行内解析缓存的条目上限（超出整表丢弃，按需重建）。
const TABLE_CELL_CACHE_CAP: usize = 4096;
/// 代码字号与代码块最多展示行数（超出转块内纵向滚动）。
fn mono_size() -> f32 {
    crate::ui::palette::fs_caption()
}
const CODE_MAX_LINES: usize = 18;
/// 任务项标记列宽（勾选框所在窄列）。普通项的列宽按源码标记串实测，见 `item_marker_width`。
const LIST_MARKER_W: f32 = 20.0;

/// 可点的链接目标（外链 / 仓库路径 / wiki）。
#[derive(Clone)]
pub struct LinkTarget {
    pub href: String,
    pub form: LinkForm,
}

/// 行内可见片段：可见文本 + 该文本对应的源区间 + 样式。
/// 渲染与编辑面的落点换算（源映射探针）共用同一份走查，避免两处规则分叉。
#[derive(Clone)]
pub struct LineRun {
    /// 可见文本（与进渲染 span 的文本逐字相同）。
    pub text: String,
    /// 可见文本对应的源区间；替换形态（图片 / 角标等）与源长度不等时按整段取边界。
    pub src: (usize, usize),
    /// 样式（字体随加粗 / 斜体 / 等宽变化）。
    pub(crate) st: TextStyle,
    /// 相对字号倍数（脚注角标等）。
    pub(crate) scale: f32,
    link: Option<LinkTarget>,
}

impl LineRun {
    /// 渲染字体（探针排版按同一字体取字宽）。
    pub(crate) fn font(&self) -> Font {
        self.st.font
    }

}

/// 一个顶层块的渲染产物：前置空行数 + 控件。
pub struct BlockChunk<'a> {
    /// 与上一块之间的源码空行数（首块为 0；块间距 = 空行数 × 行高）。
    pub gap_before: usize,
    pub element: Element<'a, Message>,
}

/// 源码态排版口径。
#[derive(Clone, Copy, Debug)]
pub enum SourceKind {
    /// 散文类块：字号与行高随块类型（标题逐级放大）。
    Prose { size: f32, line_height: f32 },
    /// 代码类块：等宽小字。
    Mono { size: f32 },
}

/// 只读面块高度缓存（应用层持有）：块高只在布局期可得，全篇测量完成后按视口截块。
#[derive(Default)]
pub struct BlockMetrics {
    /// 建缓存时的块数（正文重建后整篇重测）。
    pub blocks: usize,
    /// 建缓存时的内容宽（换宽即整篇重测）。
    pub width: f32,
    /// 建缓存时的字体代次（换字体族即整篇重测）。
    pub epoch: u64,
    /// 建缓存时的宽松换行档位（同一宽度下也会改块高）。
    pub soft_break: bool,
    /// 建缓存时的图片句柄代数（图片落位后块高变化）。
    pub image_epoch: u64,
    /// 每块高度（不含块前空行）；未测量的块为 0。
    pub heights: Vec<f32>,
}

impl BlockMetrics {
    /// 正文重建：块数与块高一起作废，下一帧起整篇重测。
    pub fn reset(&mut self) {
        self.blocks = 0;
        self.heights.clear();
    }
}

/// 表格单元格行内解析缓存（应用层持有）：同一文档内源区间唯一，正文重建时整表丢弃。
#[derive(Default)]
pub struct InlineSpanCache {
    spans: HashMap<(usize, usize), Vec<InlineSpan>>,
}

impl InlineSpanCache {
    pub(crate) fn clear(&mut self) {
        self.spans.clear();
    }
}

/// 块高度回写：窗口化按块高截块，而块高只有布局后才知道。
struct BlockProbe<'a> {
    index: usize,
    metrics: Rc<RefCell<BlockMetrics>>,
    /// 块高随宽松换行档位变化（同一宽度下也会整篇失效）。
    soft_break: bool,
    /// 块高随图片句柄入表变化（占位换成实图）。
    image_epoch: u64,
    content: Element<'a, Message>,
}

impl<'a> BlockProbe<'a> {
    fn new(
        content: Element<'a, Message>,
        index: usize,
        metrics: Rc<RefCell<BlockMetrics>>,
        soft_break: bool,
        image_epoch: u64,
    ) -> Self {
        Self { index, metrics, soft_break, image_epoch, content }
    }
}

impl iced::advanced::Widget<Message, iced::Theme, iced::Renderer> for BlockProbe<'_> {
    fn tag(&self) -> iced::advanced::widget::tree::Tag {
        self.content.as_widget().tag()
    }

    fn state(&self) -> iced::advanced::widget::tree::State {
        self.content.as_widget().state()
    }

    fn children(&self) -> Vec<iced::advanced::widget::Tree> {
        self.content.as_widget().children()
    }

    fn diff(&self, tree: &mut iced::advanced::widget::Tree) {
        self.content.as_widget().diff(tree);
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut iced::advanced::widget::Tree,
        renderer: &iced::Renderer,
        limits: &iced::advanced::layout::Limits,
    ) -> iced::advanced::layout::Node {
        let node = self.content.as_widget_mut().layout(tree, renderer, limits);
        let width = limits.max().width;
        let epoch = crate::ui::font::epoch();
        let mut metrics = self.metrics.borrow_mut();
        // 换宽 / 换字体 / 换软换行档位后旧块高全部失效：清缓存让下一帧整篇重测，
        // 避免按旧高算窗口前缀而错位
        if metrics.width > 0.0
            && ((metrics.width - width).abs() > 0.5
                || metrics.epoch != epoch
                || metrics.soft_break != self.soft_break
                || metrics.image_epoch != self.image_epoch)
        {
            metrics.heights.clear();
        }
        metrics.width = width;
        metrics.epoch = epoch;
        metrics.soft_break = self.soft_break;
        metrics.image_epoch = self.image_epoch;
        let blocks = metrics.blocks;
        if metrics.heights.len() != blocks {
            metrics.heights.resize(blocks, 0.0);
        }
        if let Some(slot) = metrics.heights.get_mut(self.index) {
            *slot = node.size().height;
        }
        node
    }

    fn draw(
        &self,
        tree: &iced::advanced::widget::Tree,
        renderer: &mut iced::Renderer,
        theme: &iced::Theme,
        style: &iced::advanced::renderer::Style,
        layout: iced::advanced::Layout<'_>,
        cursor: iced::advanced::mouse::Cursor,
        viewport: &iced::Rectangle,
    ) {
        self.content.as_widget().draw(tree, renderer, theme, style, layout, cursor, viewport);
    }

    fn update(
        &mut self,
        tree: &mut iced::advanced::widget::Tree,
        event: &iced::Event,
        layout: iced::advanced::Layout<'_>,
        cursor: iced::advanced::mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn iced::advanced::Clipboard,
        shell: &mut iced::advanced::Shell<'_, Message>,
        viewport: &iced::Rectangle,
    ) {
        self.content.as_widget_mut().update(
            tree, event, layout, cursor, renderer, clipboard, shell, viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &iced::advanced::widget::Tree,
        layout: iced::advanced::Layout<'_>,
        cursor: iced::advanced::mouse::Cursor,
        viewport: &iced::Rectangle,
        renderer: &iced::Renderer,
    ) -> iced::advanced::mouse::Interaction {
        self.content
            .as_widget()
            .mouse_interaction(tree, layout, cursor, viewport, renderer)
    }

    fn operate(
        &mut self,
        tree: &mut iced::advanced::widget::Tree,
        layout: iced::advanced::Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn iced::advanced::widget::Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(tree, layout, renderer, operation);
    }
}

impl<'a> From<BlockProbe<'a>> for Element<'a, Message> {
    fn from(widget: BlockProbe<'a>) -> Self {
        Element::new(widget)
    }
}

/// 块前空行数（首块为 0；与块序列同一口径）。
fn gap_before_at(source: &str, blocks: &[MarkdownBlock], index: usize) -> usize {
    if index == 0 {
        return 0;
    }
    blank_line_count(source, block_to(&blocks[index - 1]), block_from(&blocks[index]))
}

/// 块高表 + 滚动偏移 → 可见块区间（上下各留 0.75 屏余量）与区间前后高度。
fn block_window(
    heights: &[f32],
    gaps: &[usize],
    gap_h: f32,
    scroll_y: f32,
    viewport_h: f32,
) -> Option<(std::ops::Range<usize>, f32, f32)> {
    if heights.is_empty() || heights.len() != gaps.len() || viewport_h <= 0.0 {
        return None;
    }
    let span = |index: usize| heights[index] + gaps[index] as f32 * gap_h;
    let top_limit = (scroll_y - viewport_h * 0.75).max(0.0);
    let bottom_limit = scroll_y + viewport_h * 1.75;
    let mut y = 0.0;
    let mut start = None;
    let mut end = heights.len();
    for index in 0..heights.len() {
        let next = y + span(index);
        if start.is_none() && next >= top_limit {
            start = Some(index);
        }
        if next >= bottom_limit {
            end = index + 1;
            break;
        }
        y = next;
    }
    let start = start?;
    let prefix: f32 = (0..start).map(span).sum();
    let suffix: f32 = (end..heights.len()).map(span).sum();
    Some((start..end.max(start + 1), prefix, suffix))
}

/// 阅读面入口：正文列（不含滚动容器——由视图层统一包一层，档位切换时滚动位置随
/// 同一 scrollable 状态保留）。给出滚动偏移与视口高时按块高缓存截块；缓存未就绪
/// （首帧 / 换宽后）整篇构建，块高由布局回写。
#[allow(clippy::too_many_arguments)]
pub fn document_view<'a>(
    doc: &'a MarkdownDocument,
    theme: &iced::Theme,
    images: &'a ImageHandles,
    soft_break: bool,
    selection: Option<&'a NoteSelection>,
    code_expanded: &'a std::collections::HashSet<usize>,
    metrics: &Rc<RefCell<BlockMetrics>>,
    inline_cache: &'a RefCell<InlineSpanCache>,
    scroll: Option<(f32, f32)>,
    image_epoch: u64,
) -> Element<'a, Message> {
    let render = Render::new(
        theme,
        images,
        soft_break,
        1.0,
        SelectMode::Note(selection),
        Some(code_expanded),
        Some(inline_cache),
    );
    let blocks = doc.blocks.len();
    {
        let mut metrics = metrics.borrow_mut();
        if metrics.blocks != blocks {
            metrics.reset();
            metrics.blocks = blocks;
        }
    }
    let gaps: Vec<usize> =
        (0..blocks).map(|index| gap_before_at(&doc.source, &doc.blocks, index)).collect();
    let window = scroll.and_then(|(scroll_y, viewport_h)| {
        let metrics = metrics.borrow();
        if metrics.heights.len() != blocks || metrics.heights.iter().any(|height| *height <= 0.0) {
            return None;
        }
        block_window(&metrics.heights, &gaps, gap_height(render.scale), scroll_y, viewport_h)
    });
    let (range, top, bottom) = match window {
        Some((range, top, bottom)) => (range, top, bottom),
        None => (0..blocks, 0.0, 0.0),
    };
    let mut body = column![].width(Length::Fill).spacing(0);
    if top > 0.0 {
        body = body.push(Space::new().width(Length::Fill).height(Length::Fixed(top)));
    }
    for index in range {
        let chunk = render.chunk_at(&doc.source, &doc.blocks, index);
        if chunk.gap_before > 0 {
            body = body.push(gap(chunk.gap_before, render.scale));
        }
        let probe: Element<'a, Message> =
            BlockProbe::new(chunk.element, index, metrics.clone(), soft_break, image_epoch)
                .into();
        body = body.push(probe);
    }
    if bottom > 0.0 {
        body = body.push(Space::new().width(Length::Fill).height(Length::Fixed(bottom)));
    }
    container(body.width(Length::Fill)).width(Length::Fill).padding(16.0).into()
}

/// 无根留白的正文体，字号按 `scale` 等比缩放（调用方自带边距；画布卡浮层传视口缩放、面板传 1.0）。
pub fn document_body_scaled<'a>(
    doc: &'a MarkdownDocument,
    theme: &iced::Theme,
    images: &'a ImageHandles,
    soft_break: bool,
    scale: f32,
) -> Element<'a, Message> {
    document_body_with(doc, theme, images, soft_break, scale, SelectMode::Off, None)
}

/// 对话正文体（可拖选）：`scope` 标识本消息段，`selection` 为应用层选区（按段隔离）。
pub fn document_body_select<'a>(
    doc: &'a MarkdownDocument,
    theme: &iced::Theme,
    images: &'a ImageHandles,
    soft_break: bool,
    scale: f32,
    scope: SelectScope,
    selection: Option<&'a ChatSelection>,
) -> Element<'a, Message> {
    document_body_with(
        doc,
        theme,
        images,
        soft_break,
        scale,
        SelectMode::Chat { scope, selection },
        None,
    )
}

/// 只读面的拖选档位：笔记只读档整篇一个文档；对话按消息段各持一份选区
/// （只读面其余调用方用纯富文本）。
#[derive(Clone, Copy)]
enum SelectMode<'a> {
    Off,
    Note(Option<&'a NoteSelection>),
    Chat {
        scope: SelectScope,
        selection: Option<&'a ChatSelection>,
    },
}

impl SelectMode<'_> {
    fn selectable(&self) -> bool {
        !matches!(self, Self::Off)
    }
}

fn document_body_with<'a>(
    doc: &'a MarkdownDocument,
    theme: &iced::Theme,
    images: &'a ImageHandles,
    soft_break: bool,
    scale: f32,
    select: SelectMode<'a>,
    code_expanded: Option<&'a std::collections::HashSet<usize>>,
) -> Element<'a, Message> {
    let render = Render::new(theme, images, soft_break, scale, select, code_expanded, None);
    let mut body = column![].width(Length::Fill).spacing(0);
    for chunk in render.chunks(&doc.source, &doc.blocks) {
        if chunk.gap_before > 0 {
            body = body.push(gap(chunk.gap_before, render.scale));
        }
        body = body.push(chunk.element);
    }
    body.width(Length::Fill).into()
}

/// 编辑面块元数据。
#[derive(Clone)]
pub struct BlockMeta {
    pub from: usize,
    pub to: usize,
    /// 列表块（Enter 延续项、空项退格用）。
    pub list: bool,
    pub source: SourceKind,
}

/// 编辑面块元数据（按顶层块序）。
pub fn block_meta(blocks: &[MarkdownBlock]) -> Vec<BlockMeta> {
    blocks
        .iter()
        .map(|b| {
            let (from, to) = (block_from(b), block_to(b));
            BlockMeta {
                from,
                to,
                list: matches!(b, MarkdownBlock::List { .. }),
                source: block_source_style(b, 1.0),
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
        source: SourceKind::Prose { size: base_size(), line_height: LINE_HEIGHT },
    }
}

/// 阅读面解析入口：与渲染面共用同一套链接形态判定。
pub fn parse_document(source: &str) -> MarkdownDocument {
    parse_markdown(source, &link_options())
}

/// 带 mention 候选的解析入口（对话消息引用标签的胶囊渲染；候选命中按 mention 原子输出）。
pub fn parse_document_with_mentions(
    source: &str,
    mentions: Vec<atelyx_core::markdown::MentionCandidate>,
) -> MarkdownDocument {
    let mut opts = link_options();
    opts.mentions = mentions;
    parse_markdown(source, &opts)
}

/// 解析选项：非外链地址一律判为仓库内路径（不做存在性判定）。
pub(crate) fn link_options() -> ParseOptions {
    ParseOptions {
        resolve_link: Some(Box::new(|href: &str| {
            let href = href.trim();
            // 空 href 与纯片段锚点（#标题）都不是仓库路径：不按「文件没找到」提示
            if href.is_empty() || href.starts_with('#') {
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
            mark_bg: pal::MARK_BG,
        }
    }
}

/// 行内样式累加器：容器片段（加粗/斜体/删除线/高亮）向下传递，叶子片段据它产出 span。
#[derive(Clone, Copy)]
pub(crate) struct TextStyle {
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
            font: crate::ui::regular(),
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
    base_size()
        * match level {
            1 => 1.5,
            2 => 1.35,
            3 => 1.2,
            4 => 1.1,
            5 => 1.0,
            _ => 0.9,
        }
}

/// 块的源码态口径：结构块与散文块统一按整块源码回显，字号随块类型（按 `scale` 缩放）。
fn block_source_style(block: &MarkdownBlock, scale: f32) -> SourceKind {
    match block {
        MarkdownBlock::Heading { level, .. } => {
            SourceKind::Prose { size: heading_size(*level) * scale, line_height: 1.3 }
        }
        MarkdownBlock::Paragraph { .. }
        | MarkdownBlock::List { .. }
        | MarkdownBlock::Blockquote { .. } => {
            SourceKind::Prose { size: base_size() * scale, line_height: LINE_HEIGHT }
        }
        MarkdownBlock::FootnoteDef { .. } => {
            SourceKind::Prose { size: base_size() * scale * 0.9, line_height: LINE_HEIGHT }
        }
        MarkdownBlock::FencedCode { .. }
        | MarkdownBlock::IndentedCode { .. }
        | MarkdownBlock::Table { .. }
        | MarkdownBlock::HtmlBlock { .. }
        | MarkdownBlock::MathBlock { .. }
        | MarkdownBlock::Hr { .. } => SourceKind::Mono { size: mono_size() * scale },
    }
}

/// 引用内边距（每层引用 10px）。
const QUOTE_PAD: f32 = 10.0;
/// 引用竖线宽度。
const QUOTE_BAR_W: f32 = 3.0;
/// 脚注竖线宽度。
const FOOTNOTE_BAR_W: f32 = 2.0;

/// 行装饰：外层左内缩与左侧竖线。渲染件与揭示态的源码面共用同一套包装。
#[derive(Clone)]
pub enum RowWrap {
    /// 左内缩（列表子行 / 项内子块）。
    Pad(f32),
    /// 左侧竖线 + 左内边距 + 可选底色（引用 / callout / 脚注）。
    Bar { color: Color, width: f32, pad: f32, bg: Option<Color> },
}

/// 行件包装：按装饰逐层包住内层（`wraps` 外层在前）。
pub fn wrap_row<'a>(wraps: &[RowWrap], inner: Element<'a, Message>) -> Element<'a, Message> {
    let mut el = inner;
    for wrap in wraps.iter().rev() {
        el = match wrap {
            RowWrap::Pad(left) => container(el)
                .width(Length::Fill)
                .padding(Padding { left: *left, ..Padding::default() })
                .into(),
            RowWrap::Bar { color, width, pad, bg } => {
                let bg = *bg;
                Stack::new()
                    .push(
                        container(el)
                            .width(Length::Fill)
                            .padding(Padding { left: *pad, ..Padding::default() })
                            .style(move |_theme: &iced::Theme| container::Style {
                                background: bg.map(Into::into),
                                ..Default::default()
                            }),
                    )
                    .push(bar_line(*color, *width))
                    .width(Length::Fill)
                    .into()
            }
        };
    }
    el
}

/// 装饰累计的可见正文左偏移（行内落点换算与空行自绘光标用）。
fn wrap_indent(wraps: &[RowWrap]) -> f32 {
    wraps
        .iter()
        .map(|w| match w {
            RowWrap::Pad(left) => *left,
            RowWrap::Bar { pad, .. } => *pad,
        })
        .sum()
}

/// 竖线件：撑满所在行高（Stack 覆盖层按收紧限高布局）。
fn bar_line<'a>(color: Color, width: f32) -> Element<'a, Message> {
    container(Space::new())
        .width(Length::Fixed(width))
        .height(Length::Fill)
        .style(move |_theme: &iced::Theme| container::Style {
            background: Some(color.into()),
            ..Default::default()
        })
        .into()
}

/// callout 类型徽标。
fn callout_badge<'a>(name: &str, color: Color, scale: f32) -> Element<'a, Message> {
    container(
        text(name.to_uppercase())
            .size(11.0 * scale)
            .font(crate::ui::bold())
            .color(color),
    )
    .padding([1.0, 6.0])
    .style(move |_theme: &iced::Theme| container::Style {
        background: Some(color.scale_alpha(0.16).into()),
        border: Border { radius: 8.0.into(), ..Border::default() },
        ..Default::default()
    })
    .into()
}

/// 编辑档行类别：揭示单位按类别定——`Block` 整块回显源码、`Unit` 只回显本行、`Gap` / `Deco` 不揭示。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    /// 整块一行（段落 / 标题 / 脚注 / 表格 / 图片 / 分隔线 / 公式 / raw HTML / 围栏代码）。
    Block,
    /// 可独立揭示的一行（列表项内容行、引用内的子块行）。
    Unit,
    /// 空行行（块间与块内）：不揭示，光标与选区由编辑面自绘。
    Gap,
    /// 装饰行（callout 徽标）：命中吸到相邻内容行，不揭示。
    Deco,
}

/// 单个整块的内容件（只读面与编辑档整块行共用）。
struct BlockContent<'a> {
    runs: Vec<LineRun>,
    /// 富渲染件（未含块级行装饰）。
    element: Element<'a, Message>,
    /// 围栏代码正文的源区间（代码正文的行列换算基准）。
    code: Option<(usize, usize)>,
}

/// 编辑档一行：未揭示时的富渲染件 + 源锚 + 可见片段（源映射）与行装饰。
pub struct EditRow<'a> {
    pub kind: RowKind,
    /// 行区间：正文行 = 行内源区间；空行行 = 空行区间（`gap_lines` 为各空行行首）。
    pub from: usize,
    pub to: usize,
    /// 揭示时回显源码的区间（块级 = 整块；行级 = 本行）。
    pub reveal: (usize, usize),
    /// 可见片段（源映射探针）；空行行 / 图片块 / 表格等无逐字映射的为空。
    pub runs: Vec<LineRun>,
    /// 可见片段相对行左缘的横向偏移。
    pub indent: f32,
    /// 探针排版口径（与富渲染同一字号行高）。
    pub source: SourceKind,
    /// 空行行内各空行的行首。
    pub gap_lines: Vec<usize>,
    /// 围栏代码正文的源区间（代码正文的行列换算基准）。
    pub code: Option<(usize, usize)>,
    /// 行装饰（渲染与揭示态共用）。
    pub wraps: Vec<RowWrap>,
    /// 未揭示时的富渲染件；窗口外的行不构建（None）。
    pub element: Option<Element<'a, Message>>,
}

impl<'a> EditRow<'a> {
    /// 揭示态的行件：源码面替换本行内容，行装饰照旧。
    pub fn revealed(&self, inner: Element<'a, Message>) -> Element<'a, Message> {
        wrap_row(&self.wraps, inner)
    }

    /// 行的左内缩：富渲染行的可见片段让开标记列；揭示态源码面自带标记，只余行装饰。
    pub fn indent_for(&self, revealed: bool) -> f32 {
        if revealed {
            wrap_indent(&self.wraps)
        } else {
            self.indent
        }
    }
}

/// 编辑档行清单入口（富渲染 + 源锚 + 可见片段）。
#[cfg(test)]
pub fn edit_rows<'a>(
    source: &'a str,
    blocks: &'a [MarkdownBlock],
    theme: &iced::Theme,
    images: &'a ImageHandles,
) -> Vec<EditRow<'a>> {
    edit_rows_window(source, blocks, theme, images, None)
}

/// 编辑档行清单（可只构建窗口内的富渲染件）。窗口外的行只保留源锚与空行信息，
/// `EditRow::element` 为 None；调用方按窗口切片后交给编辑面。
pub fn edit_rows_window<'a>(
    source: &'a str,
    blocks: &'a [MarkdownBlock],
    theme: &iced::Theme,
    images: &'a ImageHandles,
    window: Option<std::ops::Range<usize>>,
) -> Vec<EditRow<'a>> {
    let render = Render {
        pal: Palette::from_theme(theme),
        opts: link_options(),
        interactive: true,
        linkable: true,
        code_expanded: None,
        images,
        soft_break: true,
        scale: 1.0,
        select: SelectMode::Off,
        window,
        cell_cache: None,
    };
    render.rows(source, blocks)
}

struct Render<'a> {
    pal: Palette,
    opts: ParseOptions,
    /// 任务框可勾（编辑档）；只读档为假。
    interactive: bool,
    /// 链接片段挂触点（编辑档命中测试与只读档点击打开都要）。
    linkable: bool,
    /// 已展开的代码块（只读档 footer；键 = 块起始源偏移）。
    code_expanded: Option<&'a std::collections::HashSet<usize>>,
    images: &'a ImageHandles,
    /// 宽松换行（false = 段内单个换行渲染为空格；编辑档恒 true，见 block_chunks）。
    soft_break: bool,
    /// 字号与行高倍数（画布卡浮层按视口缩放；面板与编辑面传 1.0）。
    scale: f32,
    /// 行内段落拖选档位（含应用层选区；随档位下发给段落）。
    select: SelectMode<'a>,
    /// 编辑档只构建该行窗口内的元素（None = 全量；只读面恒 None）。
    window: Option<std::ops::Range<usize>>,
    /// 单元格行内解析缓存（只读面传入；其余路径每次现解析）。
    cell_cache: Option<&'a RefCell<InlineSpanCache>>,
}

impl<'a> Render<'a> {
    fn new(
        theme: &iced::Theme,
        images: &'a ImageHandles,
        soft_break: bool,
        scale: f32,
        select: SelectMode<'a>,
        code_expanded: Option<&'a std::collections::HashSet<usize>>,
        cell_cache: Option<&'a RefCell<InlineSpanCache>>,
    ) -> Self {
        Self {
            pal: Palette::from_theme(theme),
            opts: link_options(),
            interactive: false,
            // 笔记只读档的链接可点（点击由选择部件按落点判定）；其余只读面仍是纯样式
            linkable: matches!(select, SelectMode::Note(_)),
            code_expanded,
            images,
            soft_break,
            scale,
            select,
            window: None,
            cell_cache,
        }
    }
}

impl Render<'_> {
    fn selectable(&self) -> bool {
        self.select.selectable()
    }

    /// 当前行是否在构建窗口内（None = 全量）。
    fn row_visible(&self, index: usize) -> bool {
        self.window.as_ref().is_none_or(|window| window.contains(&index))
    }
    // 字号助手：渲染链上的字号一律经此，随 scale 等比。
    fn base(&self) -> f32 {
        base_size() * self.scale
    }

    fn mono(&self) -> f32 {
        mono_size() * self.scale
    }

    fn heading(&self, level: u8) -> f32 {
        heading_size(level) * self.scale
    }

    /// 行内基础样式：正文字体、颜色随主题语义。
    fn text_style(&self) -> TextStyle {
        TextStyle { font: crate::ui::regular(), ..TextStyle::base() }
    }

    /// 块序列：每块附源区间与前置空行数（间距由调用方按空行数补行高）。
    fn chunks<'a>(&self, source: &'a str, blocks: &'a [MarkdownBlock]) -> Vec<BlockChunk<'a>> {
        (0..blocks.len()).map(|index| self.chunk_at(source, blocks, index)).collect()
    }

    /// 单块渲染产物（只读面窗口化按块构建）。块前空行按源码相邻块计算，与整篇构建同口径。
    fn chunk_at<'a>(
        &self,
        source: &'a str,
        blocks: &'a [MarkdownBlock],
        index: usize,
    ) -> BlockChunk<'a> {
        BlockChunk {
            gap_before: gap_before_at(source, blocks, index),
            element: self.block(source, &blocks[index]),
        }
    }

    /// 子块列表 → 列（块间与首个子块之前的源码空行各补一行高间距）。
    /// `lead_from` = 首个子块之前的源码位置（列表项内容末尾 / 引用起点），
    /// 用于把「项内容 ↔ 首个子块」「引用起点 ↔ 首个段落」之间的空行也渲染成行。
    fn blocks_column<'a>(
        &self,
        source: &'a str,
        lead_from: Option<usize>,
        blocks: &'a [MarkdownBlock],
    ) -> Element<'a, Message> {
        let mut col = column![].width(Length::Fill).spacing(0);
        for (pos, block) in blocks.iter().enumerate() {
            let lead = if pos == 0 { lead_from } else { Some(block_to(&blocks[pos - 1])) };
            if let Some(lead) = lead {
                let n = blank_line_count(source, lead, block_from(block));
                if n > 0 {
                    col = col.push(gap(n, self.scale));
                }
            }
            col = col.push(self.block(source, block));
        }
        col.into()
    }

    fn block<'a>(&self, source: &'a str, block: &'a MarkdownBlock) -> Element<'a, Message> {
        match block {
            MarkdownBlock::Blockquote { from, callout, children, .. } => {
                self.blockquote(source, *from, callout.as_deref(), children)
            }
            MarkdownBlock::List { ordered, items, .. } => self.list(source, *ordered, items),
            _ => {
                let content = self.block_content(source, block, false);
                wrap_row(&self.block_wraps(block), content.element)
            }
        }
    }

    /// 单个整块的内容件：可见片段、内层元素、围栏代码正文的源区间一次算出。
    /// 只读面与编辑档整块行都走这里，两面只差行包裹与外层参数。
    /// `code_cursor` = 给围栏代码正文挂编辑面源码光标。
    fn block_content<'a>(
        &self,
        source: &'a str,
        block: &'a MarkdownBlock,
        code_cursor: bool,
    ) -> BlockContent<'a> {
        let mut runs = Vec::new();
        let mut code = None;
        let element = match block {
            MarkdownBlock::Heading { level, inline, .. } => {
                let mut st = self.text_style();
                st.font.weight = iced::font::Weight::Bold;
                runs = self.runs(source, inline, st);
                self.rich_runs(
                    &runs,
                    self.heading(*level),
                    1.3,
                    None,
                    iced::alignment::Horizontal::Left,
                )
            }
            MarkdownBlock::Paragraph { inline, .. } => {
                if let Some(InlineSpan::Image { src, alt, width, height, .. }) = image_only(inline) {
                    self.image_block(src, alt, width.as_deref(), height.as_deref())
                } else {
                    runs = self.runs(source, inline, self.text_style());
                    self.rich_runs(
                        &runs,
                        self.base(),
                        LINE_HEIGHT,
                        None,
                        iced::alignment::Horizontal::Left,
                    )
                }
            }
            MarkdownBlock::FencedCode { lang, code: body, content_from, content_to, .. } => {
                code = Some((*content_from, *content_to));
                let cursor = code_cursor.then(|| ((*content_from, *content_to), self.mono()));
                self.code_block(block_from(block), lang, body, cursor)
            }
            MarkdownBlock::IndentedCode { code: body, .. } => {
                self.code_block(block_from(block), "", body, None)
            }
            MarkdownBlock::Hr { .. } => self.hr(),
            MarkdownBlock::Table { header, aligns, rows, .. } => {
                self.table(source, header, aligns, rows)
            }
            MarkdownBlock::MathBlock { tex, .. } => self.source_block(format!("$$\n{tex}\n$$")),
            MarkdownBlock::HtmlBlock { html, .. } => self.source_block(html.clone()),
            MarkdownBlock::FootnoteDef { from, label, inline, .. } => {
                runs = self.footnote_runs(source, *from, label, inline);
                self.rich_runs(
                    &runs,
                    self.base() * 0.9,
                    LINE_HEIGHT,
                    Some(self.pal.muted),
                    iced::alignment::Horizontal::Left,
                )
            }
            MarkdownBlock::List { .. } | MarkdownBlock::Blockquote { .. } => {
                unreachable!("列表与引用按行拆开，不产生整块内容")
            }
        };
        BlockContent { runs, element, code }
    }

    /// 整块行的块级行装饰（脚注左侧竖线）：只读面与编辑面共用同一处定义。
    fn block_wraps(&self, block: &MarkdownBlock) -> Vec<RowWrap> {
        match block {
            MarkdownBlock::FootnoteDef { .. } => vec![RowWrap::Bar {
                color: self.pal.border,
                width: FOOTNOTE_BAR_W,
                pad: 8.0,
                bg: None,
            }],
            _ => Vec::new(),
        }
    }

    // ===== 编辑档行清单 =====
    //
    // 编辑面按「行」排布与命中：行 = 可揭示单位（整块 / 列表项 / 引用内子块）+ 空行行。
    // 行清单复用本模块同一批渲染函数，与只读档逐点一致；只读档不经此入口。

    /// 编辑档行清单：顶层块序列 → 行序列（含块间 / 块内空行行与嵌套缩进）。
    fn rows<'a>(&self, source: &'a str, blocks: &'a [MarkdownBlock]) -> Vec<EditRow<'a>> {
        let mut rows = Vec::new();
        let Some(first) = blocks.first() else {
            return rows;
        };
        let first_from = block_from(first);
        if first_from > 0 {
            let lines = crate::markdown_edit::edge_blank_starts(source, 0, first_from);
            if !lines.is_empty() {
                rows.push(self.gap_row(lines, 0, first_from, Vec::new(), self.row_visible(rows.len())));
            }
        }
        for (index, block) in blocks.iter().enumerate() {
            if index > 0 {
                let prev_to = block_to(&blocks[index - 1]);
                self.push_blank_rows(source, prev_to, block_from(block), &[], &mut rows);
            }
            self.push_block_rows(source, block, Vec::new(), &mut rows);
        }
        let last_to = block_to(blocks.last().unwrap());
        if source.len() > last_to {
            let lines = crate::markdown_edit::tail_blank_starts(source, last_to);
            if !lines.is_empty() {
                rows.push(self.gap_row(lines, last_to, source.len() + 1, Vec::new(), self.row_visible(rows.len())));
            }
        }
        rows
    }

    /// 空行行：一根源码空行一行高；光标由编辑面自绘（不揭示）。
    fn gap_row<'a>(
        &self,
        lines: Vec<usize>,
        from: usize,
        to: usize,
        wraps: Vec<RowWrap>,
        render: bool,
    ) -> EditRow<'a> {
        let height = lines.len().max(1);
        EditRow {
            kind: RowKind::Gap,
            from,
            to,
            reveal: (from, to),
            runs: Vec::new(),
            indent: wrap_indent(&wraps),
            source: SourceKind::Prose { size: self.base(), line_height: LINE_HEIGHT },
            gap_lines: lines,
            code: None,
            element: render.then(|| wrap_row(&wraps, gap(height, self.scale))),
            wraps,
        }
    }

    /// 区间内的空行行（块间与块内同口径：区间里全是空行才成行）。
    fn push_blank_rows<'a>(
        &self,
        source: &str,
        from: usize,
        to: usize,
        wraps: &[RowWrap],
        rows: &mut Vec<EditRow<'a>>,
    ) {
        if to <= from || blank_line_count(source, from, to) == 0 {
            return;
        }
        let mut lines = crate::markdown_edit::gap_line_starts(source, from, to);
        // 区间末尾未以换行收尾时，末段是下一行行首的装饰（缩进 / 引用标记）：去掉这根伪行
        if !source[from..to].ends_with('\n') {
            lines.pop();
        }
        if lines.is_empty() {
            return;
        }
        rows.push(self.gap_row(lines, from, to, wraps.to_vec(), self.row_visible(rows.len())));
    }

    fn push_block_rows<'a>(
        &self,
        source: &'a str,
        block: &'a MarkdownBlock,
        wraps: Vec<RowWrap>,
        rows: &mut Vec<EditRow<'a>>,
    ) {
        let render = self.row_visible(rows.len());
        match block {
            MarkdownBlock::List { ordered, items, .. } => {
                self.push_list_rows(source, *ordered, items, wraps, rows)
            }
            MarkdownBlock::Blockquote { from, callout, children, .. } => {
                self.push_quote_rows(source, *from, callout.as_deref(), children, wraps, rows)
            }
            _ => rows.push(self.block_row(source, block, wraps, render)),
        }
    }

    /// 单个整块行（段落 / 标题 / 脚注 / 表格 / 图片 / 分隔线 / 公式 / raw HTML / 代码块）。
    fn block_row<'a>(
        &self,
        source: &'a str,
        block: &'a MarkdownBlock,
        wraps: Vec<RowWrap>,
        render: bool,
    ) -> EditRow<'a> {
        let (from, to) = (block_from(block), block_to(block));
        let mut row_wraps = wraps.clone();
        row_wraps.extend(self.block_wraps(block));
        // 窗口外的行不构建内容件，只留源锚与行装饰
        let content = render.then(|| self.block_content(source, block, true));
        let (runs, code, element) = match content {
            Some(content) => (
                content.runs,
                content.code,
                Some(wrap_row(&row_wraps, content.element)),
            ),
            None => (Vec::new(), None, None),
        };
        EditRow {
            kind: RowKind::Block,
            from,
            to,
            reveal: (from, to),
            runs,
            indent: wrap_indent(&row_wraps),
            source: block_source_style(block, 1.0),
            gap_lines: Vec::new(),
            code,
            element,
            wraps: row_wraps,
        }
    }

    /// 列表行：每项一行（标记列 + 内容），项间空行、项内容与首个子块之间的空行、
    /// 子块之间空行各自成空行行，嵌套子块按本项标记列宽递进缩进。
    fn push_list_rows<'a>(
        &self,
        source: &'a str,
        ordered: bool,
        items: &'a [MarkdownListItem],
        wraps: Vec<RowWrap>,
        rows: &mut Vec<EditRow<'a>>,
    ) {
        for (pos, item) in items.iter().enumerate() {
            if pos > 0 {
                self.push_blank_rows(source, items[pos - 1].to, item.from, &wraps, rows);
            }
            let render = self.row_visible(rows.len());
            let (marker_len, ordinal) = item_marker(&source[item.from..item.to]);
            // 窗口从本项的子块开始时要保留缩进列宽；完全在窗口外的项不必实测字宽
            let marker_w = if render
                || self.window.as_ref().is_some_and(|window| window.end > rows.len() + 1)
            {
                item_marker_width(source, item, marker_len, self.scale)
            } else {
                0.0
            };
            let (runs, content) = if render {
                let (runs, content) = self.item_content_row(source, item);
                (runs, Some(content))
            } else {
                (Vec::new(), None)
            };
            let (_, content_to) = item_content_range(source, item);
            let element = content.map(|content| {
                let marker = self.item_marker_cell(item, ordered, &ordinal, marker_w);
                wrap_row(&wraps, row![marker, content].width(Length::Fill).into())
            });
            rows.push(EditRow {
                kind: RowKind::Unit,
                from: item.from,
                to: content_to,
                reveal: (item.from, content_to),
                runs,
                indent: wrap_indent(&wraps) + marker_w,
                source: SourceKind::Prose { size: self.base(), line_height: LINE_HEIGHT },
                gap_lines: Vec::new(),
                code: None,
                element,
                wraps: wraps.clone(),
            });
            if item.children.is_empty() {
                continue;
            }
            let mut child_wraps = wraps.clone();
            child_wraps.push(RowWrap::Pad(marker_w));
            for (pos, child) in item.children.iter().enumerate() {
                let lead = if pos == 0 {
                    content_to
                } else {
                    block_to(&item.children[pos - 1])
                };
                self.push_blank_rows(source, lead, block_from(child), &child_wraps, rows);
                self.push_block_rows(source, child, child_wraps.clone(), rows);
            }
        }
    }

    /// 引用 / callout 行：内部子块各自成行，行间空行成空行行；每行带竖线（callout 另有底色）。
    /// 类型徽标成一条装饰行（命中吸到内容首行；随该行揭示让位）。
    fn push_quote_rows<'a>(
        &self,
        source: &'a str,
        from: usize,
        callout: Option<&str>,
        children: &'a [MarkdownBlock],
        wraps: Vec<RowWrap>,
        rows: &mut Vec<EditRow<'a>>,
    ) {
        let color = callout.map(|name| self.callout_color(name)).unwrap_or(self.pal.border);
        let bg = callout.map(|_| self.pal.text.scale_alpha(0.04));
        let mut inner_wraps = wraps;
        inner_wraps.push(RowWrap::Bar { color, width: QUOTE_BAR_W, pad: QUOTE_PAD, bg });
        if let Some(name) = callout {
            let render = self.row_visible(rows.len());
            let badge = render.then(|| callout_badge(name, color, self.scale));
            rows.push(EditRow {
                kind: RowKind::Deco,
                from,
                to: from,
                reveal: (from, from),
                runs: Vec::new(),
                indent: wrap_indent(&inner_wraps),
                source: SourceKind::Prose { size: self.base(), line_height: LINE_HEIGHT },
                gap_lines: Vec::new(),
                code: None,
                element: badge.map(|badge| {
                    wrap_row(
                        &inner_wraps,
                        container(badge).padding(Padding { bottom: 4.0, ..Padding::default() }).into(),
                    )
                }),
                wraps: inner_wraps.clone(),
            });
        }
        for (pos, child) in children.iter().enumerate() {
            let lead = if pos == 0 { from } else { block_to(&children[pos - 1]) };
            self.push_blank_rows(source, lead, block_from(child), &inner_wraps, rows);
            let start = rows.len();
            self.push_block_rows(source, child, inner_wraps.clone(), rows);
            // 引用内的子块按行揭示：整块行降为行级（列表项行本就是行级）
            for row in &mut rows[start..] {
                if row.kind == RowKind::Block {
                    row.kind = RowKind::Unit;
                }
            }
        }
    }

    /// 列表项内容件：可见片段 + 富渲染件（图片项走图片块、空项占一行高）。
    /// 项区间跨引用块时渲染按剥前缀后的文本（既有口径），源映射回落行级。
    fn item_content_row<'a>(
        &self,
        source: &'a str,
        item: &'a MarkdownListItem,
    ) -> (Vec<LineRun>, Element<'a, Message>) {
        let (content_from, content_to) = item_content_range(source, item);
        let raw = &source[content_from..content_to];
        let stripped = atelyx_core::markdown::strip_quote_marks(raw);
        if stripped == raw {
            let inline = parse_inline(source, content_from, content_to, &self.opts);
            if let Some(InlineSpan::Image { src, alt, width, height, .. }) = image_only(&inline) {
                return (Vec::new(), self.image_block(src, alt, width.as_deref(), height.as_deref()));
            }
            let runs = self.runs(source, &inline, self.text_style());
            if runs.is_empty() {
                return (Vec::new(), self.empty_item_filler());
            }
            let content = self.rich_runs(
                &runs,
                self.base(),
                LINE_HEIGHT,
                None,
                iced::alignment::Horizontal::Left,
            );
            return (runs, content);
        }
        let inline = parse_inline(&stripped, 0, stripped.len(), &self.opts);
        if let Some(InlineSpan::Image { src, alt, width, height, .. }) = image_only(&inline) {
            return (Vec::new(), self.image_block(src, alt, width.as_deref(), height.as_deref()));
        }
        if inline.is_empty() {
            return (Vec::new(), self.empty_item_filler());
        }
        // 剥引用前缀后的文本与源区间不再逐字节对应：选择按整段落项内容区间
        let mut runs = self.runs(&stripped, &inline, self.text_style());
        if self.selectable() {
            for run in &mut runs {
                run.src = (content_from, content_to);
            }
        }
        let content = self.rich_runs(
            &runs,
            self.base(),
            LINE_HEIGHT,
            None,
            iced::alignment::Horizontal::Left,
        );
        (Vec::new(), content)
    }

    /// 空项的内容位：零高占位（行高由标记列决定）。
    fn empty_item_filler<'a>(&self) -> Element<'a, Message> {
        Space::new().width(Length::Fill).into()
    }

    /// 列表项标记位：任务框取代圆点/序号；有序项沿用源序号文本。
    fn item_marker_cell<'a>(
        &self,
        item: &MarkdownListItem,
        ordered: bool,
        ordinal: &str,
        width: f32,
    ) -> Element<'a, Message> {
        if item.task {
            let from = item.from;
            // 任务框走 ui 自绘（iced 内建 checkbox 的勾是内嵌图标字体的字形，不满足全应用自绘图标）
            let state =
                if item.checked { controls::CheckState::On } else { controls::CheckState::Off };
            // 只读档也可切换任务（写回正文走保存链）；编辑档同一条路
            let toggle = self.interactive || matches!(self.select, SelectMode::Note(_));
            let box_widget = controls::checkbox(state, toggle.then_some(Message::ToggleTask(from)));
            container(box_widget)
                .width(Length::Fixed(width))
                .padding(Padding { top: 2.0, ..Padding::default() })
                .into()
        } else {
            let label = if ordered { ordinal.to_string() } else { "•".to_string() };
            container(text(label).size(self.base()).color(self.pal.muted))
                .width(Length::Fixed(width))
                .into()
        }
    }

    /// 脚注定义的可见片段：定义标记（原子，落定义起点）+ 正文片段。
    fn footnote_runs(&self, source: &str, from: usize, label: &str, inline: &[InlineSpan]) -> Vec<LineRun> {
        let mut runs = vec![LineRun {
            text: format!("[{label}] "),
            src: (from, from),
            st: TextStyle { color: Some(self.pal.accent), ..self.text_style() },
            scale: 1.0,
            link: None,
        }];
        runs.extend(self.runs(source, inline, TextStyle { color: Some(self.pal.muted), ..self.text_style() }));
        runs
    }

    /// 引用 / callout：左侧竖线 + 缩进；callout 另加类型徽标与底色。
    /// 竖线走 Stack 覆盖层：滚动容器内 `Fill` 高度会被压缩，而 Stack 的非基础层
    /// 用收紧限高重新布局，`Fill` 高度即内容高度。
    fn blockquote<'a>(
        &self,
        source: &'a str,
        from: usize,
        callout: Option<&str>,
        children: &'a [MarkdownBlock],
    ) -> Element<'a, Message> {
        let line = callout.map(|name| self.callout_color(name)).unwrap_or(self.pal.border);
        let content = self.blocks_column(source, Some(from), children);
        let content: Element<'a, Message> = match callout {
            Some(name) => column![callout_badge(name, line, self.scale), content]
                .width(Length::Fill)
                .spacing(4.0)
                .into(),
            None => content,
        };
        let bg = callout.map(|_| self.pal.text.scale_alpha(0.04));
        wrap_row(
            &[RowWrap::Bar { color: line, width: QUOTE_BAR_W, pad: QUOTE_PAD, bg }],
            content,
        )
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
                    col = col.push(gap(n, self.scale));
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
        let (marker_len, ordinal) = item_marker(&source[item.from..item.to]);
        let marker_w = item_marker_width(source, item, marker_len, self.scale);
        let (_, content) = self.item_content_row(source, item);
        let marker = self.item_marker_cell(item, ordered, &ordinal, marker_w);
        let (_, content_to) = item_content_range(source, item);
        let body = column![content]
            .width(Length::Fill)
            .spacing(0)
            .push(self.blocks_column(source, Some(content_to), &item.children));
        row![marker, body].width(Length::Fill).into()
    }

    /// 围栏 / 缩进代码块：语言标签 + 复制按钮 + 等宽正文（不换行，超宽横向滚动）。
    /// `hit` = 编辑档的正文源区间与等宽字号（正文内落点换算基准）。
    fn code_block<'a>(
        &self,
        from: usize,
        lang: &str,
        code: &str,
        hit: Option<((usize, usize), f32)>,
    ) -> Element<'a, Message> {
        let panel = self.pal.panel;
        let border = self.pal.border;
        let label = if lang.trim().is_empty() { "代码".to_string() } else { lang.to_string() };
        let head = container(
            row![
                text(label).size(11.0 * self.scale).font(crate::ui::mono()).color(self.pal.muted),
                Space::new().width(Length::Fill),
                button(text("复制").size(11.0 * self.scale))
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
        // 首屏折叠只作用于只读面：长代码块给「展开全部 / 收起」footer，复制按钮始终取全文
        let collapsible = matches!(self.select, SelectMode::Note(_)) && lines > CODE_MAX_LINES;
        let expanded = collapsible && self.code_expanded.is_some_and(|set| set.contains(&from));
        let visible = if expanded { lines } else { lines.min(CODE_MAX_LINES) };
        let height = visible as f32 * self.mono() * 1.4 + 16.0;
        // 双向滚动条保持浮层：iced 的 Both 方向不为滚动条预留布局空间（上游限制），
        // 内嵌基元只覆盖单向；厚度随 scale 缩放（画布卡内与正文同比例，面板 1× 口径不变）
        let bar_w = (6.0 * self.scale).max(2.0);
        let thumb_w = (10.0 * self.scale).max(2.0);
        let body = scrollable(
            container(Self::code_body(code, hit, self.scale)).padding(8.0),
        )
        .direction(scrollable::Direction::Both {
            vertical: scrollable::Scrollbar::new().width(bar_w).scroller_width(thumb_w),
            horizontal: scrollable::Scrollbar::new().width(bar_w).scroller_width(thumb_w),
        })
        .width(Length::Fill)
        .height(Length::Fixed(height));

        let mut panel_col = column![head, body].width(Length::Fill);
        if collapsible {
            panel_col = panel_col.push(crate::ui::controls::h_divider_subtle());
            panel_col = panel_col.push(
                container(
                    button(
                        text(if expanded { "收起" } else { "展开全部" })
                            .size(11.0 * self.scale)
                            .color(self.pal.muted),
                    )
                    .padding([2.0, 6.0])
                    .style(button::text)
                    .on_press(Message::ToggleCodeBlock(from)),
                )
                .width(Length::Fill)
                .padding([2.0, 6.0])
                .style(move |_theme: &iced::Theme| container::Style {
                    background: Some(panel.into()),
                    ..Default::default()
                }),
            );
        }
        container(panel_col)
            .width(Length::Fill)
            .style(move |_theme: &iced::Theme| container::Style {
                background: Some(panel.into()),
                border: Border { color: border, width: 1.0, radius: 6.0.into() },
                ..Default::default()
            })
            .into()
    }

    /// 等宽代码正文件：编辑档另包一层局部坐标上报（正文内落点换算需要正文自己的原点）。
fn code_body<'a>(code: &str, hit: Option<((usize, usize), f32)>, scale: f32) -> Element<'a, Message> {
    let text: Element<'a, Message> = text(code.to_string())
        .size(mono_size() * scale)
        .font(crate::ui::mono())
        .wrapping(Wrapping::None)
        .into();
    match hit {
        Some((content, size)) => crate::markdown_edit::CodeBody::new(text, content, size).into(),
        None => text,
    }
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
        // 表头列数即表宽（GFM：数据行多出的格忽略、缺格渲染为空）
        let cols = header.len().max(1);
        let mut cells = Vec::with_capacity((rows.len() + 1) * cols);
        let mut natural = vec![0.0f32; cols];
        for column in 0..cols {
            let (cell, width) = self.table_cell(source, header.get(column), aligns.get(column), true);
            natural[column] = natural[column].max(width);
            cells.push(cell);
        }
        for row in rows {
            for column in 0..cols {
                let (cell, width) =
                    self.table_cell(source, row.get(column), aligns.get(column), false);
                natural[column] = natural[column].max(width);
                cells.push(cell);
            }
        }
        let border = self.pal.border;
        container(MarkdownTable::new(cols, cells, natural, border, self.pal.panel, 1.0))
            .width(Length::Fill)
            .style(move |_theme: &iced::Theme| container::Style {
                border: Border { color: border, width: 1.0, ..Border::default() },
                ..Default::default()
            })
            .into()
    }

    /// 单元格：可见片段、自然宽（文字宽 + 左右内边距）与富渲染件。
    /// 空单元格按下限宽占位，行高由同行的非空单元格撑起。
    fn table_cell<'a>(
        &self,
        source: &str,
        cell: Option<&MarkdownTableCell>,
        align: Option<&String>,
        header: bool,
    ) -> (Element<'a, Message>, f32) {
        let mut style = self.text_style();
        if header {
            style.font = crate::ui::bold();
        }
        let runs = match cell {
            Some(c) if c.to > c.from => self.cell_runs(source, c, style),
            _ => Vec::new(),
        };
        let size = self.base();
        let text_width: f32 = runs
            .iter()
            .map(|run| crate::ui::text::measure(&run.text, size * run.scale, run.font()))
            .sum();
        let natural = text_width + CELL_PAD_X * 2.0;
        let alignment = match align.map(String::as_str) {
            Some("center") => iced::alignment::Horizontal::Center,
            Some("right") => iced::alignment::Horizontal::Right,
            _ => iced::alignment::Horizontal::Left,
        };
        let content: Element<'a, Message> = if runs.is_empty() {
            Space::new().width(Length::Fill).height(Length::Fixed(size * LINE_HEIGHT)).into()
        } else {
            self.rich_runs_with(
                &runs,
                size,
                LINE_HEIGHT,
                None,
                alignment,
                Wrapping::WordOrGlyph,
            )
        };
        // 表头底色由表格整行铺（同一行单元格高度不齐也覆盖），单元格自身不叠底色
        let cell = container(content)
            .width(Length::Fill)
            .padding([CELL_PAD_Y, CELL_PAD_X])
            .into();
        (cell, natural)
    }

    /// 单元格行内片段：只读面按（起, 止）复用解析产物（同一文档内区间唯一）；
    /// 其余路径每次现解析。
    fn cell_runs(&self, source: &str, cell: &MarkdownTableCell, style: TextStyle) -> Vec<LineRun> {
        let Some(cache) = self.cell_cache else {
            let inline = parse_inline(source, cell.from, cell.to, &self.opts);
            return self.runs(source, &inline, style);
        };
        let mut cache = cache.borrow_mut();
        if cache.spans.len() >= TABLE_CELL_CACHE_CAP {
            cache.spans.clear();
        }
        let inline = cache
            .spans
            .entry((cell.from, cell.to))
            .or_insert_with(|| parse_inline(source, cell.from, cell.to, &self.opts));
        self.runs(source, inline, style)
    }

    /// 公式 / raw HTML 的降级形态：源码文本放进等宽底块。
    fn source_block<'a>(&self, content: String) -> Element<'a, Message> {
        let panel = self.pal.panel;
        let border = self.pal.border;
        container(
            text(content).size(self.mono()).font(crate::ui::mono()).color(self.pal.muted),
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
        let half = (gap_height(self.scale) - 1.0) / 2.0;
        container(line)
            .width(Length::Fill)
            .padding(Padding { top: half, bottom: half, ..Padding::default() })
            .into()
    }

    fn callout_color(&self, name: &str) -> Color {
        callout_color(name).unwrap_or(self.pal.accent)
    }

    // ===== 行内 =====

    /// 行内可见片段表（渲染与编辑面源映射的唯一走查）。
    fn runs(&self, source: &str, list: &[InlineSpan], st: TextStyle) -> Vec<LineRun> {
        let mut out = Vec::new();
        for span in list {
            self.push_run(source, span, st, &mut out);
        }
        out
    }

    /// 可见文本与其源区间的对齐：文本在源区间内原样出现时取其精确位置
    /// （代码 / 链接标签等被定界符包裹的形态），否则整段取边界。
    fn aligned_src(source: &str, from: usize, to: usize, text: &str) -> (usize, usize) {
        if text.len() == to.saturating_sub(from) {
            return (from, to);
        }
        match source.get(from..to).and_then(|s| s.find(text)) {
            Some(k) => (from + k, from + k + text.len()),
            None => (from, to),
        }
    }

    fn push_run(&self, source: &str, span: &InlineSpan, st: TextStyle, out: &mut Vec<LineRun>) {
        match span {
            InlineSpan::Text { from, to, text } => {
                // 宽松换行折叠：CRLF 先归一为 LF，孤 \r 同样按换行折叠为空格
                if !self.soft_break {
                    let text = text.replace("\r\n", "\n").replace(['\r', '\n'], " ");
                    out.push(LineRun { text, src: (*from, *to), st, scale: 1.0, link: None });
                    return;
                }
                // LF 文本一行一片段即可；只有 CRLF 需要逐行剥离 \r 并单独记换行符
                if !text.contains('\r') && source.get(*from..*to) == Some(text.as_str()) {
                    out.push(LineRun { text: text.clone(), src: (*from, *to), st, scale: 1.0, link: None });
                    return;
                }
                if source.get(*from..*to) != Some(text.as_str()) {
                    out.push(LineRun {
                        text: without_cr(text.clone()),
                        src: (*from, *to),
                        st,
                        scale: 1.0,
                        link: None,
                    });
                    return;
                }
                // 逐源码行拆片段：行尾 \r 是行尾风格填充，不进文本也不计入映射区间，
                // 换行符单独成原子片段（CRLF 文档的点击/选区才与可见字符逐字对齐）。
                let mut pos = *from;
                for segment in text.split_inclusive('\n') {
                    let end = pos + segment.len();
                    let line = segment.strip_suffix('\n').unwrap_or(segment);
                    let line = line.strip_suffix('\r').unwrap_or(line);
                    // 行中孤立 \r 同样不进排版：拆成不带 \r 的片段并同步收窄源区间，
                    // 否则整段退化成不可逐字的粗粒度单元
                    let mut line_start = pos;
                    for piece in line.split('\r') {
                        if !piece.is_empty() {
                            out.push(LineRun {
                                text: piece.to_string(),
                                src: (line_start, line_start + piece.len()),
                                st,
                                scale: 1.0,
                                link: None,
                            });
                        }
                        line_start += piece.len() + 1;
                    }
                    if segment.ends_with('\n') {
                        out.push(LineRun {
                            text: "\n".to_string(),
                            src: (pos + line.len(), end),
                            st,
                            scale: 1.0,
                            link: None,
                        });
                    }
                    pos = end;
                }
            }
            InlineSpan::Strong { children, .. } => {
                let inner = TextStyle { font: Font { weight: iced::font::Weight::Bold, ..st.font }, ..st };
                for c in children {
                    self.push_run(source, c, inner, out);
                }
            }
            InlineSpan::Em { children, .. } => {
                let inner = TextStyle { font: Font { style: iced::font::Style::Italic, ..st.font }, ..st };
                for c in children {
                    self.push_run(source, c, inner, out);
                }
            }
            InlineSpan::Strike { children, .. } => {
                let inner = TextStyle { strike: true, ..st };
                for c in children {
                    self.push_run(source, c, inner, out);
                }
            }
            InlineSpan::Highlight { children, .. } => {
                let inner = TextStyle { bg: Some(self.pal.mark_bg), ..st };
                for c in children {
                    self.push_run(source, c, inner, out);
                }
            }
            InlineSpan::Code { from, to, text } => {
                let inner = TextStyle { font: crate::ui::mono(), bg: Some(self.pal.panel), ..st };
                let src = Self::aligned_src(source, *from, *to, text);
                out.push(LineRun { text: text.clone(), src, st: inner, scale: 1.0, link: None });
            }
            // 公式降级为源码文本
            InlineSpan::MathInline { from, to, .. } => {
                let inner = TextStyle { color: Some(self.pal.muted), ..st };
                out.push(LineRun {
                    text: source[*from..*to].to_string(),
                    src: (*from, *to),
                    st: TextStyle { font: Font { style: iced::font::Style::Italic, ..inner.font }, ..inner },
                    scale: 1.0,
                    link: None,
                });
            }
            InlineSpan::Comment { .. } => {}
            InlineSpan::Link { from, to, href, label, form, .. } => {
                let src = Self::aligned_src(source, *from, *to, label);
                match form {
                    LinkForm::Plain => out.push(LineRun {
                        text: source[*from..*to].to_string(),
                        src: (*from, *to),
                        st,
                        scale: 1.0,
                        link: None,
                    }),
                    LinkForm::External | LinkForm::Path | LinkForm::Wiki => {
                        let inner = TextStyle { color: Some(self.pal.accent), underline: true, ..st };
                        out.push(LineRun {
                            text: label.clone(),
                            src,
                            st: inner,
                            scale: 1.0,
                            link: Some(LinkTarget { href: href.clone(), form: *form }),
                        });
                    }
                    LinkForm::Create => {
                        let inner = TextStyle { color: Some(self.pal.muted), underline: true, ..st };
                        out.push(LineRun { text: label.clone(), src, st: inner, scale: 1.0, link: None });
                    }
                }
            }
            InlineSpan::Autolink { from, to, href, label } => {
                let inner = TextStyle { color: Some(self.pal.accent), underline: true, ..st };
                out.push(LineRun {
                    text: label.clone(),
                    src: Self::aligned_src(source, *from, *to, label),
                    st: inner,
                    scale: 1.0,
                    link: Some(LinkTarget { href: href.clone(), form: LinkForm::External }),
                });
            }
            InlineSpan::Wiki { from, to, target, label } => {
                let inner = TextStyle { color: Some(self.pal.accent), underline: true, ..st };
                out.push(LineRun {
                    text: label.clone(),
                    src: Self::aligned_src(source, *from, *to, label),
                    st: inner,
                    scale: 1.0,
                    link: Some(LinkTarget { href: target.clone(), form: LinkForm::Wiki }),
                });
            }
            // 图片以替代文字占位（不加载图片字节）
            InlineSpan::Image { from, to, src: path, alt, .. } => {
                let name = if alt.is_empty() { path } else { alt };
                let inner = TextStyle { color: Some(self.pal.muted), ..st };
                out.push(LineRun {
                    text: format!("[图片：{name}]"),
                    src: (*from, *to),
                    st: TextStyle { font: Font { style: iced::font::Style::Italic, ..inner.font }, ..inner },
                    scale: 1.0,
                    link: None,
                });
            }
            InlineSpan::Tag { from, to, tag } => {
                let inner = TextStyle {
                    color: Some(self.pal.accent),
                    bg: Some(self.pal.accent.scale_alpha(0.14)),
                    chip: true,
                    ..st
                };
                let text = format!("#{tag}");
                out.push(LineRun {
                    src: Self::aligned_src(source, *from, *to, &text),
                    text,
                    st: inner,
                    scale: 1.0,
                    link: None,
                });
            }
            InlineSpan::FootnoteRef { from, to, label } => {
                let inner = TextStyle { color: Some(self.pal.accent), ..st };
                let text = format!("[{label}]");
                out.push(LineRun {
                    src: Self::aligned_src(source, *from, *to, &text),
                    text,
                    st: inner,
                    scale: 0.85,
                    link: None,
                });
            }
            InlineSpan::Mention { from, to, label, trigger, .. } => {
                let inner = TextStyle {
                    color: Some(self.pal.accent),
                    bg: Some(self.pal.accent.scale_alpha(0.18)),
                    chip: true,
                    ..st
                };
                let text = format!("{trigger}{label}");
                out.push(LineRun {
                    src: Self::aligned_src(source, *from, *to, &text),
                    text,
                    st: inner,
                    scale: 1.0,
                    link: None,
                });
            }
            // raw HTML 降级为源码文本
            InlineSpan::Html { from, to, .. } => {
                let inner = TextStyle { color: Some(self.pal.muted), ..st };
                out.push(LineRun {
                    text: source[*from..*to].to_string(),
                    src: (*from, *to),
                    st: inner,
                    scale: 1.0,
                    link: None,
                });
            }
            InlineSpan::HardBreak { from, to } => {
                out.push(LineRun {
                    text: "\n".to_string(),
                    src: Self::aligned_src(source, *from, *to, "\n"),
                    st,
                    scale: 1.0,
                    link: None,
                });
            }
        }
    }

    /// 单个片段 → 渲染 span（与 {@link Render::runs} 同一份走查的渲染侧）。
    fn run_span<'a>(&self, run: LineRun) -> Span<'a, LinkTarget, Font> {
        let mut leaf = self.leaf(run.text, run.st);
        if run.scale != 1.0 {
            leaf = leaf.size(self.base() * run.scale);
        }
        match run.link {
            Some(target) => self.linked(leaf, target.href, target.form),
            None => leaf,
        }
    }

    /// 片段表 → 渲染 span。
    fn run_spans<'a>(&self, runs: Vec<LineRun>) -> Vec<Span<'a, LinkTarget, Font>> {
        runs.into_iter().map(|run| self.run_span(run)).collect()
    }

    /// 片段表 → 富文本件：可拖选档带源区间与选区，其余走只读富文本。
    fn rich_runs<'a>(
        &self,
        runs: &[LineRun],
        size: f32,
        line_height: f32,
        color: Option<Color>,
        align: alignment::Horizontal,
    ) -> Element<'a, Message> {
        self.rich_runs_with(runs, size, line_height, color, align, Wrapping::default())
    }

    /// 与 [`Self::rich_runs`] 同源，可指定折行策略（表格单元格按词/字形兜底）。
    fn rich_runs_with<'a>(
        &self,
        runs: &[LineRun],
        size: f32,
        line_height: f32,
        color: Option<Color>,
        align: alignment::Horizontal,
        wrapping: Wrapping,
    ) -> Element<'a, Message> {
        if !self.selectable() {
            return rich_with(self.run_spans(runs.to_vec()), size, line_height, color, align, wrapping);
        }
        let mut spans = Vec::with_capacity(runs.len());
        let mut srcs = Vec::with_capacity(runs.len());
        for run in runs {
            spans.push(self.run_span(run.clone()));
            srcs.push(SelectRun::new(run.src, run.text.len()));
        }
        let mut widget = SelectableRich::new(spans, srcs)
            .size(size)
            .line_height(line_height)
            .align_x(align)
            .color_maybe(color)
            .wrapping(wrapping);
        match self.select {
            SelectMode::Off => {}
            SelectMode::Note(selection) => {
                if let Some(sel) = selection {
                    widget = widget.dragging(sel.dragging);
                    let (from, to) = sel.range();
                    if from < to {
                        widget = widget.selection((from, to));
                    }
                }
            }
            SelectMode::Chat { scope, selection } => {
                widget = widget.chat_selection(scope, selection);
            }
        }
        widget.into()
    }

    /// 交互档给链接片段挂触点（纯只读档不挂，链接仅作样式）。
    fn linked<'a>(
        &self,
        leaf: Span<'a, LinkTarget, Font>,
        href: String,
        form: LinkForm,
    ) -> Span<'a, LinkTarget, Font> {
        if self.linkable {
            leaf.link(LinkTarget { href, form })
        } else {
            leaf
        }
    }

    /// 图片块：句柄已载入 → image 部件（`|W` / `|WxH` 尺寸语法生效，缺省自然尺寸且
    /// 不超出面板宽）；载入中/失败回落替代文字占位。
    fn image_block<'a>(
        &self,
        src: &str,
        alt: &str,
        width: Option<&str>,
        height: Option<&str>,
    ) -> Element<'a, Message> {
        let name = if alt.is_empty() { src } else { alt };
        match self.images.get(src) {
            Some(handle) => {
                let mut img = iced::widget::image(handle.clone());
                // 声明尺寸随面缩放：画布卡浮层是按视口缩放渲染的，尺寸不跟着缩会撑破卡片
                if let Some(w) = parse_px(width) {
                    img = img.width(Length::Fixed(w * self.scale));
                }
                if let Some(h) = parse_px(height) {
                    img = img.height(Length::Fixed(h * self.scale));
                }
                container(img).width(Length::Fill).into()
            }
            None => {
                let muted = self.pal.muted;
                container(
                    text(format!("[图片：{name}]"))
                        .size(self.base())
                        .font(crate::ui::italic())
                        .color(muted),
                )
                .width(Length::Fill)
                .into()
            }
        }
    }

    fn leaf<'a>(&self, content: String, st: TextStyle) -> Span<'a, LinkTarget, Font> {
        // 行尾风格不进排版（见 is_cr）：一个 CRLF 才只算一个换行
        let mut span = Span::new(without_cr(content)).font(st.font).strikethrough(st.strike);
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

/// 段落/列表项内容是否「仅一张图」（忽略纯空白文本片段）：是则返回该图片片段。
/// 行内图片（图文混排）不按块渲染，保持替代文字占位。
fn image_only(inline: &[InlineSpan]) -> Option<&InlineSpan> {
    let mut found: Option<&InlineSpan> = None;
    for span in inline {
        if matches!(span, InlineSpan::Text { text, .. } if text.trim().is_empty()) {
            continue;
        }
        if !matches!(span, InlineSpan::Image { .. }) || found.is_some() {
            return None;
        }
        found = Some(span);
    }
    found
}

/// 尺寸语法取值（`|W` / `|WxH` 的 W/H）：正的有限数值才生效，超宽由布局钳到面板内。
fn parse_px(value: Option<&str>) -> Option<f32> {
    value
        .and_then(|s| s.trim().parse::<f32>().ok())
        .filter(|n| n.is_finite() && *n > 0.0)
}

/// 列表项首段内容的源区间：剥标记（止于第一个嵌套子块起点，去掉尾部空白）。
fn item_content_range(source: &str, item: &MarkdownListItem) -> (usize, usize) {
    let raw = &source[item.from..item.to];
    let (marker_len, _) = item_marker(raw);
    let content_from = item.from + marker_len;
    let content_end = item.children.first().map(block_from).unwrap_or(item.to);
    let by = content_end.max(content_from);
    let content_to = content_from + source[content_from..by].trim_end().len();
    (content_from, content_to)
}

/// 列表项首段内容文本：剥标记与引用前缀（项区间跨引用块时续行带 `>`），
/// 止于第一个嵌套子块起点（项内后续段落归 children）。渲染与图片收集共用。
fn item_content(source: &str, item: &MarkdownListItem) -> String {
    let (from, to) = item_content_range(source, item);
    atelyx_core::markdown::strip_quote_marks(&source[from..to])
}

/// 收集文档中按块渲染的图片 src（图片段落与列表项图片内容；行内图片不收集不载入）。
/// 供应用层发起按需载入；与渲染的块级图片判定同源。
pub fn collect_image_srcs(source: &str, blocks: &[MarkdownBlock]) -> Vec<String> {
    let opts = link_options();
    let mut out = Vec::new();
    collect_walk(source, &opts, blocks, &mut out);
    out
}

fn collect_walk(source: &str, opts: &ParseOptions, blocks: &[MarkdownBlock], out: &mut Vec<String>) {
    for block in blocks {
        match block {
            MarkdownBlock::Paragraph { inline, .. } => {
                if let Some(InlineSpan::Image { src, .. }) = image_only(inline) {
                    out.push(src.clone());
                }
            }
            MarkdownBlock::Blockquote { children, .. } => collect_walk(source, opts, children, out),
            MarkdownBlock::List { items, .. } => {
                for item in items {
                    let content = item_content(source, item);
                    let spans = parse_inline(&content, 0, content.len(), opts);
                    if let Some(InlineSpan::Image { src, .. }) = image_only(&spans) {
                        out.push(src.clone());
                    }
                    collect_walk(source, opts, &item.children, out);
                }
            }
            _ => {}
        }
    }
}

/// 选区可见文本（文档顺序）：块间换行、表格单元格制表符；非逐字节对应的片段
/// （代码 / 公式 / 占位等）按整段取。
pub fn selected_text(
    doc: &MarkdownDocument,
    theme: &iced::Theme,
    images: &ImageHandles,
    soft_break: bool,
    range: (usize, usize),
) -> String {
    if range.0 >= range.1 {
        return String::new();
    }
    let render = Render {
        pal: Palette::from_theme(theme),
        opts: link_options(),
        interactive: false,
        linkable: false,
        code_expanded: None,
        images,
        soft_break,
        scale: 1.0,
        select: SelectMode::Off,
        window: None,
        cell_cache: None,
    };
    let mut pieces = Vec::new();
    render.collect_text(&doc.source, &doc.blocks, range, &mut pieces);
    let mut out = String::new();
    for (index, piece) in pieces.iter().enumerate() {
        if index > 0 {
            out.push_str(&pieces[index - 1].sep);
        }
        out.push_str(&piece.text);
    }
    out.trim_end().to_string()
}

/// 复制文本片段：`sep` = 与下一段的间隔（含块间源码空行，跨块复制不压平空行）。
struct TextPiece {
    text: String,
    sep: String,
}

fn push_text(out: &mut Vec<TextPiece>, text: String, sep: String) {
    if !text.is_empty() {
        out.push(TextPiece { text, sep });
    }
}

/// 块间隔：块本身换行 + 源码里两块的空行数（与渲染的块间距同源）。
fn block_sep(blank_lines: usize) -> String {
    "\n".repeat(1 + blank_lines)
}

/// 片段表在选区内的可见文本：逐字节对齐的片段取相交部分，替换形态整段取。
fn selected_runs_text(runs: &[LineRun], range: (usize, usize)) -> String {
    let mut out = String::new();
    for run in runs {
        let (from, to) = run.src;
        if from == to {
            // 零宽源（脚注标记等）：落点覆盖到该位置即整段取
            if range.0 <= from && from < range.1 {
                out.push_str(&run.text);
            }
            continue;
        }
        if to <= range.0 || range.1 <= from {
            continue;
        }
        if to - from == run.text.len() {
            let start = range.0.max(from) - from;
            let end = range.1.min(to) - from;
            if let Some(slice) = run.text.get(start..end) {
                out.push_str(slice);
            }
        } else {
            out.push_str(&run.text);
        }
    }
    out
}

fn selected_atomic_text(text: &str, src: (usize, usize), range: (usize, usize)) -> String {
    let hit = if src.0 == src.1 {
        range.0 <= src.0 && src.0 < range.1
    } else {
        src.0 < range.1 && range.0 < src.1
    };
    if hit { text.to_string() } else { String::new() }
}

impl Render<'_> {
    /// 按块序收集选区可见文本（与渲染同一份行内走查）。
    fn collect_text(
        &self,
        source: &str,
        blocks: &[MarkdownBlock],
        range: (usize, usize),
        out: &mut Vec<TextPiece>,
    ) {
        // 块与块之间按源码空行数补空行：只压一个换行会把「段 + 空行 + 段」复制成一整段。
        // 间隔属于前一块的末片段（拼接时用的是 pieces[i-1].sep）
        let mut prev_to: Option<usize> = None;
        for block in blocks {
            let from = block_from(block);
            if let (Some(prev), Some(last)) = (prev_to, out.last_mut()) {
                last.sep = block_sep(blank_line_count(source, prev, from));
            }
            prev_to = Some(block_to(block));
            match block {
                MarkdownBlock::Heading { inline, .. } | MarkdownBlock::Paragraph { inline, .. } => {
                    let runs = self.runs(source, inline, self.text_style());
                    // 单片段块的块间隔统一由循环末尾的末片段改写补上
                    push_text(out, selected_runs_text(&runs, range), block_sep(0));
                }
                MarkdownBlock::Blockquote { children, .. } => {
                    // 引用内的块各自成段（引用标记不进复制文本）
                    self.collect_text(source, children, range, out);
                }
                MarkdownBlock::List { items, .. } => {
                    for item in items {
                        let (content_from, content_to) = item_content_range(source, item);
                        let stripped = atelyx_core::markdown::strip_quote_marks(&source[content_from..content_to]);
                        if stripped == source[content_from..content_to] {
                            let inline = parse_inline(source, content_from, content_to, &self.opts);
                            let runs = self.runs(source, &inline, self.text_style());
                            push_text(out, selected_runs_text(&runs, range), block_sep(0));
                        } else {
                            // 剥前缀后与源区间不再逐字节对应：整段落项内容区间按可见文本取
                            let inline = parse_inline(&stripped, 0, stripped.len(), &self.opts);
                            let text: String = self
                                .runs(&stripped, &inline, self.text_style())
                                .iter()
                                .map(|run| run.text.as_str())
                                .collect();
                            push_text(
                                out,
                                selected_atomic_text(&text, (content_from, content_to), range),
                                block_sep(0),
                            );
                        }
                        self.collect_text(source, &item.children, range, out);
                    }
                }
                MarkdownBlock::FencedCode { code, content_from, content_to, .. } => {
                    push_text(
                        out,
                        selected_atomic_text(code, (*content_from, *content_to), range),
                        block_sep(0),
                    );
                }
                MarkdownBlock::IndentedCode { code, from, to, .. } => {
                    push_text(out, selected_atomic_text(code, (*from, *to), range), block_sep(0));
                }
                MarkdownBlock::Hr { .. } => {}
                MarkdownBlock::Table { header, rows, .. } => {
                    for row in std::iter::once(header).chain(rows.iter()) {
                        // 行不在选区内整行跳过：保留下来的空单元格会变成一串制表符
                        let row_from = row.iter().map(|cell| cell.from).min().unwrap_or(0);
                        let row_to = row.iter().map(|cell| cell.to).max().unwrap_or(row_from);
                        if row_to <= range.0 || range.1 <= row_from {
                            continue;
                        }
                        // 空单元格照样占一列：过滤掉会把后面的列左移（列数错位）
                        let cells: Vec<String> = row
                            .iter()
                            .map(|cell| {
                                if cell.to <= cell.from {
                                    return String::new();
                                }
                                let inline = parse_inline(source, cell.from, cell.to, &self.opts);
                                let runs = self.runs(source, &inline, self.text_style());
                                selected_runs_text(&runs, range)
                            })
                            .collect();
                        // 行内单换行；表格与下一块的空行由循环末尾的末片段改写补上
                        push_text(out, cells.join("\t"), block_sep(0));
                    }
                }
                MarkdownBlock::MathBlock { tex, from, to } => {
                    push_text(
                        out,
                        selected_atomic_text(&format!("$$\n{tex}\n$$"), (*from, *to), range),
                        block_sep(0),
                    );
                }
                MarkdownBlock::HtmlBlock { html, from, to } => {
                    push_text(out, selected_atomic_text(html, (*from, *to), range), block_sep(0));
                }
                MarkdownBlock::FootnoteDef { from, label, inline, .. } => {
                    let runs = self.footnote_runs(source, *from, label, inline);
                    push_text(out, selected_runs_text(&runs, range), block_sep(0));
                }
            }
        }
    }
}

/// 行内富文本（可指定折行策略，表格单元格用 WordOrGlyph）。
fn rich_with<'a>(
    spans: Vec<Span<'a, LinkTarget, Font>>,
    size: f32,
    line_height: f32,
    color: Option<Color>,
    align: iced::alignment::Horizontal,
    wrapping: Wrapping,
) -> Element<'a, Message> {
    let mut text = Rich::<'a, LinkTarget, Message>::with_spans(spans)
        .font(crate::ui::regular())
        .size(size)
        .line_height(LineHeight::Relative(line_height))
        .align_x(align)
        .width(Length::Fill)
        .wrapping(wrapping)
        .on_link_click(link_message);
    if let Some(c) = color {
        text = text.color(c);
    }
    text.into()
}

/// 链接点击：按目标形态打开。
fn link_message(target: LinkTarget) -> Message {
    Message::OpenLink(target.href, target.form)
}

/// 空行间距元素：一个源码空行 = 一行高（行高按 `scale` 缩放）。
/// 宽度须撑满：零宽行与视口的相交恒为空，编辑面会整行跳过空行的光标与选区底色。
pub fn gap<'a>(lines: usize, scale: f32) -> Element<'a, Message> {
    Space::new()
        .width(Length::Fill)
        .height(Length::Fixed(lines as f32 * gap_height(scale)))
        .into()
}

/// 单根源码空行的行高随 `scale` 缩放。
fn gap_height(scale: f32) -> f32 {
    base_size() * scale * LINE_HEIGHT
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

/// 列表项标记列宽：任务项维持勾选框窄列，其余按源码标记串（标记符与其后空白）的实测字宽——
/// 编辑档揭示本行时正文列落在标记串之后，列宽取自源码字宽才能与渲染态重合、切揭示不横移。
fn item_marker_width(source: &str, item: &MarkdownListItem, marker_len: usize, scale: f32) -> f32 {
    if item.task {
        return LIST_MARKER_W;
    }
    marker_column_width(&source[item.from..item.from + marker_len], scale)
}

/// 标记串字宽：与源码面同一逐字符 span 排版实测（列宽与揭示态对齐才不横移）。
/// 字宽表由 ui::text 统一缓存，长列表每帧不逐项重排。
fn marker_column_width(marker: &str, scale: f32) -> f32 {
    crate::ui::text::measure_per_char(marker, base_size() * scale, crate::ui::regular())
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
    let color = match name.to_ascii_lowercase().as_str() {
        "note" => pal::CALLOUT_NOTE,
        "abstract" | "summary" | "tldr" | "info" | "todo" => pal::CALLOUT_INFO,
        "tip" | "success" | "check" | "done" => pal::CALLOUT_TIP,
        "question" | "help" | "faq" => pal::CALLOUT_QUESTION,
        "warning" | "caution" => pal::CALLOUT_WARNING,
        "failure" | "fail" | "missing" | "danger" | "error" | "bug" => pal::CALLOUT_DANGER,
        "important" => pal::CALLOUT_IMPORTANT,
        "example" => pal::CALLOUT_EXAMPLE,
        "quote" | "cite" => pal::CALLOUT_QUOTE,
        _ => return None,
    };
    Some(color)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 行尾风格不进排版：撇去 `\r` 后一个 CRLF 只算一个换行。
    #[test]
    fn without_cr_drops_line_ending_fill() {
        assert_eq!(without_cr("甲\r\n乙".to_string()), "甲\n乙");
        assert_eq!(without_cr("甲\n乙".to_string()), "甲\n乙");
        assert_eq!(without_cr(String::new()), "");
    }

    /// 段落行的渲染文本只保留源码里的换行：CRLF 不得在渲染侧多出一个换行。
    #[test]
    fn edit_rows_window_only_builds_visible_rows() {
        let source = "甲\n\n乙\n\n丙\n\n丁\n";
        let doc = parse_document(source);
        let images = ImageHandles::default();
        let theme = iced::Theme::Dark;
        let all = edit_rows_window(source, &doc.blocks, &theme, &images, None);
        let windowed = edit_rows_window(source, &doc.blocks, &theme, &images, Some(2..6));
        assert_eq!(all.len(), windowed.len());
        assert!(windowed[..2].iter().all(|row| row.element.is_none()));
        assert!(windowed[2..6].iter().all(|row| row.element.is_some()));
        assert!(windowed[6..].iter().all(|row| row.element.is_none()));
    }

    /// 整块内容分派覆盖全部整块类型，块级行装饰只来自同一处：两面不会再各长一份分支。
    #[test]
    fn block_content_covers_every_whole_block() {
        let source = concat!(
            "# 标题\n\n正文 abc\n\n```rs\ncode\n```\n\n    indented\n\n---\n\n",
            "$$\nx\n$$\n\n<div>html</div>\n\n[^1]: 脚注\n",
        );
        let doc = parse_document(source);
        let images = ImageHandles::default();
        let theme = iced::Theme::Dark;
        let render = Render {
            pal: Palette::from_theme(&theme),
            opts: link_options(),
            interactive: true,
            linkable: true,
            code_expanded: None,
            images: &images,
            soft_break: true,
            scale: 1.0,
            select: SelectMode::Off,
            window: None,
            cell_cache: None,
        };
        let mut seen = 0;
        for block in &doc.blocks {
            assert!(
                !matches!(block, MarkdownBlock::List { .. } | MarkdownBlock::Blockquote { .. }),
                "样本里只有整块"
            );
            seen += 1;
            let content = render.block_content(source, block, true);
            match block {
                MarkdownBlock::FencedCode { content_from, content_to, .. } => {
                    assert_eq!(content.code, Some((*content_from, *content_to)));
                    assert!(render.block_wraps(block).is_empty());
                }
                MarkdownBlock::FootnoteDef { .. } => {
                    assert!(!content.runs.is_empty(), "脚注正文片段进源映射");
                    assert_eq!(render.block_wraps(block).len(), 1, "脚注竖线只由 block_wraps 给出");
                }
                _ => assert!(render.block_wraps(block).is_empty()),
            }
        }
        assert!(seen >= 7, "样本覆盖标题/段落/围栏/缩进/分隔线/公式/HTML/脚注");
        // 编辑档整块行同样从这里取片段、代码区间与行装饰
        let rows = render.rows(source, &doc.blocks);
        let footnote = rows
            .iter()
            .find(|row| !row.wraps.is_empty())
            .expect("脚注行带竖线");
        assert_eq!(footnote.kind, RowKind::Block);
        assert!(rows.iter().any(|row| row.code.is_some()));
    }

    /// 行中孤立回车符不占排版也不占选区区间：片段文本与源区间逐字对齐。
    #[test]
    fn lone_cr_run_keeps_src_aligned() {
        let source = "甲\r乙";
        let doc = parse_document(source);
        let images = ImageHandles::default();
        let rows = edit_rows(source, &doc.blocks, &iced::Theme::Dark, &images);
        let runs: Vec<(String, (usize, usize))> = rows
            .iter()
            .flat_map(|row| row.runs.iter())
            .map(|run| (run.text.clone(), run.src))
            .collect();
        assert_eq!(
            runs,
            vec![("甲".to_string(), (0, 3)), ("乙".to_string(), (4, 7))]
        );
    }

    #[test]
    fn crlf_row_keeps_one_break_per_source_line() {
        let source = "甲乙\r\n丙丁\r\n";
        let doc = parse_document(source);
        let images = ImageHandles::default();
        let rows = edit_rows(source, &doc.blocks, &iced::Theme::Dark, &images);
        let row = rows.iter().find(|r| !r.runs.is_empty()).expect("段落在编辑档有可见片段");
        let laid: String = row.runs.iter().map(|r| without_cr(r.text.clone())).collect();
        assert_eq!(laid, "甲乙\n丙丁");
        // 内容片段源区间不含行尾 \r：点击/选区按可见字符逐字对齐
        for run in &row.runs {
            if run.text != "\n" {
                assert!(!source[run.src.0..run.src.1].contains('\r'));
            }
        }
    }

    /// 复制走文档可见文本：块间按源码空行补空行、列表不带标记、表格单元格制表符。
    #[test]
    fn selected_text_walks_visible_content() {
        let source = "# 标题\n\n正文 abc 结束\n\n- 列表项\n\n| 甲 | 乙 |\n| --- | --- |\n| 1 | 2 |\n";
        let doc = parse_document(source);
        let images = ImageHandles::default();
        let theme = iced::Theme::Dark;
        let all = selected_text(&doc, &theme, &images, true, (0, source.len()));
        assert_eq!(
            all,
            "标题\n\n正文 abc 结束\n\n列表项\n\n甲\t乙\n1\t2"
        );
        let start = source.find("abc").expect("选区目标");
        assert_eq!(selected_text(&doc, &theme, &images, true, (start, start + 3)), "abc");
    }

    /// 列表项跨引用前缀：复制取渲染可见文本（剥引用标记，不含强调定界符）。
    #[test]
    fn selected_text_strips_quote_prefix_and_marks() {
        let source = "> - **加粗** 项\n>   续行\n";
        let doc = parse_document(source);
        let images = ImageHandles::default();
        let theme = iced::Theme::Dark;
        assert_eq!(
            selected_text(&doc, &theme, &images, true, (0, source.len())),
            "加粗 项\n  续行"
        );
    }

    /// 跨块复制保留源码空行；表格空单元格占位不丢列（列数错位会打乱表格结构）。
    #[test]
    fn copy_keeps_block_blank_lines_and_table_columns() {
        let source = "标题段\n\n正文\n\n| 甲 |  | 乙 |\n| --- | --- | --- |\n| 1 |  | 3 |\n";
        let doc = parse_document(source);
        let images = ImageHandles::default();
        let theme = iced::Theme::Dark;
        assert_eq!(
            selected_text(&doc, &theme, &images, true, (0, source.len())),
            "标题段\n\n正文\n\n甲\t\t乙\n1\t\t3"
        );
    }

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

    /// 块窗口：按块高（含块前空行）截出可见区间与区间前后高度。
    #[test]
    fn block_window_slices_by_block_heights() {
        let heights = [100.0, 100.0, 100.0, 100.0];
        let gaps = [0usize, 0, 0, 0];
        let (range, top, bottom) =
            block_window(&heights, &gaps, 10.0, 0.0, 100.0).expect("顶部有窗口");
        assert_eq!((range.start, range.end), (0, 2));
        assert_eq!(top, 0.0);
        assert_eq!(bottom, 200.0);
        let (range, top, bottom) =
            block_window(&heights, &gaps, 10.0, 250.0, 100.0).expect("中段有窗口");
        assert_eq!((range.start, range.end), (1, 4));
        assert_eq!(top, 100.0);
        assert_eq!(bottom, 0.0);
    }

    /// 块前空行计入块间距：窗口后缀高度必须包含跳过的块与其空行。
    #[test]
    fn block_window_counts_gap_rows() {
        let heights = [50.0, 50.0];
        let gaps = [0usize, 3];
        let (range, top, bottom) =
            block_window(&heights, &gaps, 10.0, 0.0, 10.0).expect("顶部有窗口");
        assert_eq!((range.start, range.end), (0, 1));
        assert_eq!(top, 0.0);
        assert_eq!(bottom, 80.0);
    }

    /// 表格单元格行内解析按源区间复用：同区间再取不重解析。
    #[test]
    fn table_cell_inline_parse_is_cached() {
        let source = "| 甲 | 乙 |\n| --- | --- |\n| **粗** | 2 |\n";
        let doc = parse_document(source);
        let images = ImageHandles::default();
        let theme = iced::Theme::Dark;
        let cache = RefCell::new(InlineSpanCache::default());
        let render = Render::new(&theme, &images, true, 1.0, SelectMode::Off, None, Some(&cache));
        let MarkdownBlock::Table { header, .. } = &doc.blocks[0] else {
            panic!("首块是表格");
        };
        let cell = &header[0];
        let first = render.cell_runs(&doc.source, cell, render.text_style());
        assert_eq!(cache.borrow().spans.len(), 1);
        let second = render.cell_runs(&doc.source, cell, render.text_style());
        assert_eq!(cache.borrow().spans.len(), 1, "同区间命中缓存，不新增条目");
        let texts = |runs: &[LineRun]| runs.iter().map(|r| r.text.clone()).collect::<Vec<_>>();
        assert_eq!(texts(&first), vec!["甲".to_string()]);
        assert_eq!(texts(&first), texts(&second));
    }

    #[test]
    fn callout_color_covers_known_types() {
        assert!(callout_color("NOTE").is_some());
        assert_eq!(callout_color("warning"), Some(pal::CALLOUT_WARNING));
        assert!(callout_color("unknown-type").is_none());
    }

    #[test]
    fn edit_rows_make_in_block_blank_lines_rows() {
        // 松散列表：项行 / 项间空行行 / 项行 / 文末虚拟空行行
        let source = "- 甲\n\n- 乙\n";
        let doc = parse_document(source);
        let images = ImageHandles::default();
        let rows = edit_rows(source, &doc.blocks, &iced::Theme::Dark, &images);
        let kinds: Vec<RowKind> = rows.iter().map(|r| r.kind).collect();
        assert_eq!(kinds, vec![RowKind::Unit, RowKind::Gap, RowKind::Unit, RowKind::Gap]);
        // 项间空行的行首（"- 甲\n" 之后的那根空行）
        assert_eq!(rows[1].gap_lines, vec![6]);
        // 项行只揭示自己那一行（标记入内、块间空行不入内）
        assert_eq!(rows[0].reveal, (0, 5));
        assert_eq!(rows[2].reveal, (7, 12));
        // 可见片段与源偏移一一对应（内容不含标记列）
        assert_eq!(rows[0].runs.len(), 1);
        assert_eq!(rows[0].runs[0].src, (2, 5));
        // 标记列宽 = 源码标记串实测字宽（揭示本行时正文列不横移）
        assert_eq!(rows[0].indent, marker_column_width("- ", 1.0));
    }

    #[test]
    fn marker_column_width_follows_source_marker() {
        // 标记后的空白计入列宽（揭示态正文紧跟其后）；序号位数越多列越宽
        assert!(marker_column_width("- ", 1.0) > marker_column_width("-", 1.0));
        assert!(marker_column_width("12) ", 1.0) > marker_column_width("1. ", 1.0));
        // 任务项维持勾选框窄列
        let source = "- [ ] 甲\n";
        let doc = parse_document(source);
        let images = ImageHandles::default();
        let rows = edit_rows(source, &doc.blocks, &iced::Theme::Dark, &images);
        assert_eq!(rows[0].indent, LIST_MARKER_W);
    }

    /// 揭示态源码面自带标记列：行内缩只余行装饰，否则落点与光标整体右移一个标记列
    /// （嵌套列表越深偏得越多）。
    #[test]
    fn revealed_rows_inset_only_counts_decorations() {
        let source = "- 甲\n  - 乙\n    - 丙\n";
        let doc = parse_document(source);
        let images = ImageHandles::default();
        let rows = edit_rows(source, &doc.blocks, &iced::Theme::Dark, &images);
        let items: Vec<&EditRow<'_>> =
            rows.iter().filter(|row| row.kind == RowKind::Unit).collect();
        assert_eq!(items.len(), 3);
        let marker = marker_column_width("- ", 1.0);
        for (depth, row) in items.iter().enumerate() {
            let decorations = wrap_indent(&row.wraps);
            assert_eq!(decorations, marker * depth as f32, "第 {depth} 层的装饰内缩");
            assert_eq!(row.indent, decorations + marker, "富渲染行让出本项标记列");
            assert_eq!(row.indent_for(false), row.indent);
            assert_eq!(row.indent_for(true), decorations);
        }
    }

    #[test]
    fn edit_rows_keep_quote_children_and_gaps_apart() {
        // 引用内两段之间有空行：段行 / 空行行 / 段行，行装饰带竖线
        let source = "> 甲\n>\n> 乙\n";
        let doc = parse_document(source);
        let images = ImageHandles::default();
        let rows = edit_rows(source, &doc.blocks, &iced::Theme::Dark, &images);
        let kinds: Vec<RowKind> = rows.iter().map(|r| r.kind).collect();
        assert_eq!(kinds, vec![RowKind::Unit, RowKind::Gap, RowKind::Unit, RowKind::Gap]);
        assert_eq!(rows[1].gap_lines, vec![6]);
        assert!(matches!(rows[0].wraps.as_slice(), [RowWrap::Bar { pad, .. }] if *pad == QUOTE_PAD));
    }


    fn paragraph_image(src: &str) -> Option<InlineSpan> {
        let doc = parse_markdown(src, &link_options());
        let MarkdownBlock::Paragraph { inline, .. } = doc.blocks.first()? else { return None };
        image_only(inline).cloned()
    }

    #[test]
    fn image_only_matches_standalone_images() {
        // 独立图片段（含尾随空白）命中；尺寸语法剥离后 src 纯净
        let span = paragraph_image("![封面](pic.png)").expect("独立图片应命中");
        assert!(matches!(&span, InlineSpan::Image { src, .. } if src == "pic.png"));
        assert!(paragraph_image("![封面](pic.png) ").is_some());
        assert!(paragraph_image("![图|100x50](pic.png)").is_some());
        // 图文混排与纯文本不命中
        assert!(paragraph_image("看这张 ![图](pic.png)").is_none());
        assert!(paragraph_image("普通段落").is_none());
        // 多张图不命中
        assert!(paragraph_image("![a](1.png)![b](2.png)").is_none());
    }

    #[test]
    fn parse_px_accepts_positive_numbers_only() {
        assert_eq!(parse_px(Some("100")), Some(100.0));
        assert_eq!(parse_px(Some(" 12.5 ")), Some(12.5));
        assert_eq!(parse_px(Some("0")), None);
        assert_eq!(parse_px(Some("-3")), None);
        assert_eq!(parse_px(Some("abc")), None);
        assert_eq!(parse_px(None), None);
    }

    #[test]
    fn collect_image_srcs_covers_block_positions() {
        let src = "![a](p1.png)\n\n> ![b](p2.png)\n\n- ![c](p3.png)\n- 文字 ![d](p4.png)\n\n行内 ![e](p5.png) 混排";
        let doc = parse_markdown(src, &link_options());
        assert_eq!(
            collect_image_srcs(&doc.source, &doc.blocks),
            vec!["p1.png".to_string(), "p2.png".to_string(), "p3.png".to_string()]
        );
    }
}
