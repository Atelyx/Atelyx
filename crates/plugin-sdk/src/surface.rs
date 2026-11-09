//! 宿主绘制函数表在插件侧的适配：把控件层的协议类型翻成 ABI 结构推给宿主，并把宿主回的
//! 量算 / Markdown / 主题翻回协议类型。与宿主侧的绘制桥互为镜像，两侧只认 plugin-abi 的常量。

use atelyx_plugin_protocol::frame::{FrameRect, HitCursor, HitRegion};
use atelyx_plugin_protocol::method::{
    FontSizes, MarkdownBlock, MarkdownSpan, MeasureTextResult, Radii,
};
use atelyx_plugin_protocol::paint::{
    ColorToken, DrawCommand, ImageFit, ImageSource, PaintColor, PaintShadow, TextAlign, TextFamily,
    TextWeight,
};

use crate::abi;
use crate::frame_ui::{DrawSurface, Theme};

/// 一次 draw 的宿主函数表：回调都指向宿主，插件当场用完即弃。
pub(crate) struct AbiSurface<'a> {
    sink: &'a abi::DrawSink,
    surface_id: &'a str,
    width: f32,
    height: f32,
}

impl<'a> AbiSurface<'a> {
    /// 绘制期诊断：走宿主函数表落插件日志。
    fn log(&self, level: u32, text: &str) {
        if let Some(log) = self.sink.log {
            unsafe { log(self.sink.ctx, level, text.as_ptr(), text.len()) };
        }
    }

    pub(crate) fn new(
        sink: &'a abi::DrawSink,
        surface_id: &'a str,
        width: f32,
        height: f32,
    ) -> Self {
        Self { sink, surface_id, width, height }
    }
}

impl DrawSurface for AbiSurface<'_> {
    fn surface_id(&self) -> &str {
        self.surface_id
    }

    fn size(&self) -> (f32, f32) {
        (self.width, self.height)
    }

    fn measure(
        &self,
        text: &str,
        size: f32,
        weight: Option<TextWeight>,
        family: Option<TextFamily>,
        max_width: Option<f32>,
    ) -> MeasureTextResult {
        let Some(measure) = self.sink.measure else {
            return MeasureTextResult { width: 0.0, height: size, lines: 1 };
        };
        let item = abi::AbiMeasureItem {
            text: abi::AbiStr::of(text),
            size,
            weight: weight_code(weight),
            family: family_code(family),
            max_width: max_width.unwrap_or(-1.0),
        };
        let measured = unsafe { measure(self.sink.ctx, &item) };
        MeasureTextResult {
            width: measured.width,
            height: measured.height,
            lines: measured.lines,
        }
    }

    fn markdown(&self, content: &str) -> Vec<MarkdownBlock> {
        let Some(markdown) = self.sink.markdown else {
            return Vec::new();
        };
        let mut out = abi::AbiMarkdown { blocks: std::ptr::null(), count: 0 };
        let code = unsafe { markdown(self.sink.ctx, abi::AbiStr::of(content), &mut out) };
        if code != 0 || out.blocks.is_null() {
            // 失败按空块画，但留下原因：绘制跑在宿主线程上，stderr 在发布版看不见
            self.log(abi::LOG_ERROR, "Markdown 解析失败，按空块绘制这一块");
            return Vec::new();
        }
        let blocks = unsafe { std::slice::from_raw_parts(out.blocks, out.count) };
        blocks.iter().map(markdown_block).collect()
    }

    fn theme(&self) -> Theme {
        let Some(theme) = self.sink.theme else {
            return Theme::default();
        };
        let mut out = unsafe { std::mem::zeroed::<abi::AbiTheme>() };
        unsafe { theme(self.sink.ctx, &mut out) };
        let accent = ColorToken::Accent.index() as usize;
        Theme {
            sizes: FontSizes {
                micro: out.font_sizes[0],
                caption: out.font_sizes[1],
                ui: out.font_sizes[2],
                body: out.font_sizes[3],
                h2: out.font_sizes[4],
                h1: out.font_sizes[5],
            },
            radii: Radii { xs: out.radii[0], sm: out.radii[1], md: out.radii[2] },
            accent: out.colors.get(accent).map(|color| color.rgba),
        }
    }

    fn command(&self, command: &DrawCommand) {
        let Some(push) = self.sink.command else {
            return;
        };
        let ctx = self.sink.ctx;
        unsafe {
            match command {
                DrawCommand::Line { x1, y1, x2, y2, stroke, width } => {
                    let line = abi::AbiLine {
                        x1: *x1,
                        y1: *y1,
                        x2: *x2,
                        y2: *y2,
                        stroke: color(*stroke),
                        width: *width,
                    };
                    push(ctx, abi::CMD_LINE, (&line as *const abi::AbiLine).cast());
                }
                DrawCommand::Rect {
                    x,
                    y,
                    w,
                    h,
                    fill,
                    stroke,
                    stroke_width,
                    corner_radius,
                    shadow,
                } => {
                    let rect = abi::AbiRectShape {
                        x: *x,
                        y: *y,
                        w: *w,
                        h: *h,
                        fill: optional_color(*fill),
                        stroke: optional_color(*stroke),
                        stroke_width: stroke_width.unwrap_or(1.0),
                        corner_radius: corner_radius.unwrap_or(-1.0),
                        shadow: shadow_code(*shadow),
                    };
                    push(ctx, abi::CMD_RECT, (&rect as *const abi::AbiRectShape).cast());
                }
                DrawCommand::Path { points, closed, fill, stroke, stroke_width } => {
                    let table: Vec<abi::AbiPoint> =
                        points.iter().map(|p| abi::AbiPoint { x: p.x, y: p.y }).collect();
                    let path = abi::AbiPath {
                        points: table.as_ptr(),
                        len: table.len(),
                        closed: u32::from(*closed),
                        fill: optional_color(*fill),
                        stroke: optional_color(*stroke),
                        stroke_width: stroke_width.unwrap_or(1.0),
                    };
                    push(ctx, abi::CMD_PATH, (&path as *const abi::AbiPath).cast());
                }
                DrawCommand::Circle { cx, cy, r, fill, stroke, stroke_width } => {
                    let circle = abi::AbiCircle {
                        cx: *cx,
                        cy: *cy,
                        r: *r,
                        fill: optional_color(*fill),
                        stroke: optional_color(*stroke),
                        stroke_width: stroke_width.unwrap_or(1.0),
                    };
                    push(ctx, abi::CMD_CIRCLE, (&circle as *const abi::AbiCircle).cast());
                }
                DrawCommand::Text {
                    x,
                    y,
                    content,
                    size,
                    color: text_color,
                    weight,
                    family,
                    max_width,
                    align,
                } => {
                    let text = abi::AbiText {
                        x: *x,
                        y: *y,
                        size: *size,
                        content: abi::AbiStr::of(content),
                        color: color(*text_color),
                        weight: weight_code(*weight),
                        family: family_code(*family),
                        align: align_code(*align),
                        max_width: max_width.unwrap_or(-1.0),
                    };
                    push(ctx, abi::CMD_TEXT, (&text as *const abi::AbiText).cast());
                }
                DrawCommand::Image { source, x, y, w, h, fit } => {
                    let mut image = abi::AbiImage {
                        x: *x,
                        y: *y,
                        w: *w,
                        h: *h,
                        source: abi::IMAGE_PRIVATE,
                        path: abi::AbiStr::empty(),
                        url: abi::AbiStr::empty(),
                        local: 0,
                        fit: fit_code(*fit),
                    };
                    match source {
                        ImageSource::Private { path } => {
                            image.path = abi::AbiStr::of(path);
                        }
                        ImageSource::Vault { path } => {
                            image.source = abi::IMAGE_VAULT;
                            image.path = abi::AbiStr::of(path);
                        }
                        ImageSource::Url { url, local } => {
                            image.source = abi::IMAGE_URL;
                            image.url = abi::AbiStr::of(url);
                            image.local = u32::from(*local == Some(true));
                        }
                        ImageSource::File { path } => {
                            image.source = abi::IMAGE_FILE;
                            image.path = abi::AbiStr::of(path);
                        }
                    }
                    push(ctx, abi::CMD_IMAGE, (&image as *const abi::AbiImage).cast());
                }
            }
        }
    }

    fn hit(&self, hit: &HitRegion) {
        let Some(push) = self.sink.hit else {
            return;
        };
        let item = abi::AbiHit {
            id: abi::AbiStr::of(&hit.id),
            x: hit.x,
            y: hit.y,
            w: hit.w,
            h: hit.h,
            cursor: cursor_code(hit.cursor),
        };
        unsafe { push(self.sink.ctx, &item) };
    }

    fn policy(&self, ime: Option<FrameRect>, keys: bool, fps: Option<u16>) {
        let Some(push) = self.sink.policy else {
            return;
        };
        let rect = ime.map(|ime| abi::AbiRect { x: ime.x, y: ime.y, w: ime.w, h: ime.h });
        let pointer = rect.as_ref().map_or(std::ptr::null(), |rect| rect as *const abi::AbiRect);
        unsafe { push(self.sink.ctx, pointer, u32::from(keys), u32::from(fps.unwrap_or(0))) };
    }
}

/// 宿主回的块表 → 协议块。字符串都由宿主在本次 draw 内持有，这里当场复制走。
fn markdown_block(block: &abi::AbiMdBlock) -> MarkdownBlock {
    let depth = block.depth;
    match block.kind {
        abi::MD_HEADING => MarkdownBlock::Heading {
            level: block.level.min(u32::from(u8::MAX)) as u8,
            depth,
            spans: markdown_spans(block),
        },
        abi::MD_LIST_ITEM => MarkdownBlock::ListItem {
            depth,
            ordered: block.ordered != 0,
            index: block.index,
            spans: markdown_spans(block),
        },
        abi::MD_QUOTE => MarkdownBlock::Quote { depth, spans: markdown_spans(block) },
        abi::MD_CODE => MarkdownBlock::Code {
            depth,
            lang: unsafe { block.lang.read() }.unwrap_or_default(),
            content: unsafe { block.text.read() }.unwrap_or_default(),
        },
        abi::MD_TABLE => {
            let rows = if block.rows.is_null() {
                Vec::new()
            } else {
                unsafe { std::slice::from_raw_parts(block.rows, block.row_count) }
                    .iter()
                    .map(|row| {
                        if row.cells.is_null() {
                            return Vec::new();
                        }
                        unsafe { std::slice::from_raw_parts(row.cells, row.cell_count) }
                            .iter()
                            .map(|cell| unsafe { cell.read() }.unwrap_or_default())
                            .collect()
                    })
                    .collect()
            };
            MarkdownBlock::Table { depth, rows }
        }
        abi::MD_DIVIDER => MarkdownBlock::Divider,
        _ => MarkdownBlock::Paragraph { depth, spans: markdown_spans(block) },
    }
}

fn markdown_spans(block: &abi::AbiMdBlock) -> Vec<MarkdownSpan> {
    if block.spans.is_null() {
        return Vec::new();
    }
    unsafe { std::slice::from_raw_parts(block.spans, block.span_count) }
        .iter()
        .map(|span| MarkdownSpan {
            text: unsafe { span.text.read() }.unwrap_or_default(),
            strong: span.strong != 0,
            code: span.code != 0,
            link: unsafe { span.link.read() },
        })
        .collect()
}

fn color(value: PaintColor) -> abi::AbiColor {
    match value {
        PaintColor::Token { name } => abi::AbiColor {
            kind: abi::COLOR_TOKEN,
            token: name.index(),
            rgba: [0; 4],
        },
        PaintColor::Rgba { rgba } => {
            abi::AbiColor { kind: abi::COLOR_RGBA, token: 0, rgba }
        }
    }
}

fn optional_color(value: Option<PaintColor>) -> abi::AbiColor {
    value.map(color).unwrap_or_else(abi::AbiColor::none)
}

fn shadow_code(value: Option<PaintShadow>) -> abi::AbiShadow {
    match value {
        Some(shadow) => abi::AbiShadow {
            kind: 1,
            offset_x: shadow.offset_x,
            offset_y: shadow.offset_y,
            blur: shadow.blur,
            color: color(shadow.color),
        },
        None => abi::AbiShadow {
            kind: 0,
            offset_x: 0.0,
            offset_y: 0.0,
            blur: 0.0,
            color: abi::AbiColor::none(),
        },
    }
}

fn weight_code(weight: Option<TextWeight>) -> u32 {
    match weight {
        Some(TextWeight::Normal) => abi::WEIGHT_NORMAL,
        Some(TextWeight::Medium) => abi::WEIGHT_MEDIUM,
        Some(TextWeight::SemiBold) => abi::WEIGHT_SEMIBOLD,
        None => 0,
    }
}

fn family_code(family: Option<TextFamily>) -> u32 {
    match family {
        Some(TextFamily::Ui) => abi::FAMILY_UI,
        Some(TextFamily::Mono) => abi::FAMILY_MONO,
        None => 0,
    }
}

fn align_code(align: Option<TextAlign>) -> u32 {
    match align {
        Some(TextAlign::Start) => abi::ALIGN_START,
        Some(TextAlign::Center) => abi::ALIGN_CENTER,
        Some(TextAlign::End) => abi::ALIGN_END,
        None => 0,
    }
}

fn fit_code(fit: Option<ImageFit>) -> u32 {
    match fit {
        Some(ImageFit::Fill) => abi::FIT_FILL,
        Some(ImageFit::Contain) => abi::FIT_CONTAIN,
        Some(ImageFit::Cover) => abi::FIT_COVER,
        None => 0,
    }
}

fn cursor_code(cursor: Option<HitCursor>) -> u32 {
    match cursor {
        Some(HitCursor::Pointer) => abi::CURSOR_POINTER,
        Some(HitCursor::Text) => abi::CURSOR_TEXT,
        Some(HitCursor::Grab) => abi::CURSOR_GRAB,
        Some(HitCursor::Grabbing) => abi::CURSOR_GRABBING,
        Some(HitCursor::Crosshair) => abi::CURSOR_CROSSHAIR,
        Some(HitCursor::NotAllowed) => abi::CURSOR_NOT_ALLOWED,
        Some(HitCursor::ResizeHorizontal) => abi::CURSOR_RESIZE_HORIZONTAL,
        Some(HitCursor::ResizeVertical) => abi::CURSOR_RESIZE_VERTICAL,
        Some(HitCursor::Default) | None => abi::CURSOR_DEFAULT,
    }
}
