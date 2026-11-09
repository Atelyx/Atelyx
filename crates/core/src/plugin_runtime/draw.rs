//! 同步绘制：宿主在自己的重绘拍里调插件绘制入口，插件当场把这一面画成绘制指令与命中区。
//! 这里给出宿主侧的绘制函数表（量算 / Markdown / 主题都是同一次调用里的同步子调用），
//! 以及 ABI 结构到协议类型的换算——插件侧的镜像在 SDK 里，两侧都只认定序表 `COLOR_TOKENS`。

use std::ffi::c_void;

use atelyx_plugin_abi as abi;
use atelyx_plugin_protocol::frame::{self, FrameRect, HitCursor, HitRegion, SurfaceFrame};
use atelyx_plugin_protocol::method::{
    self, MarkdownBlock, MarkdownSpan, MeasureTextResult, ThemeResult,
};
use atelyx_plugin_protocol::paint::{
    self, ColorToken, DrawCommand, ImageFit, ImageSource, PaintColor, PaintPoint, PaintShadow,
    TextAlign, TextFamily, TextWeight,
};

/// 量算请求：借用插件给出的文本，宿主侧不再复制一份。
pub struct MeasureRequest<'a> {
    pub text: &'a str,
    pub size: f32,
    pub weight: Option<TextWeight>,
    pub family: Option<TextFamily>,
    pub max_width: Option<f32>,
}

/// draw 期间宿主侧的同步能力。由壳实现：core 不认识界面层，量算与取色都交给它。
///
/// 三个方法都在宿主的重绘线程上同步调用，返回前必须给出结果；实现不得阻塞。
pub trait DrawHost: Send + Sync {
    /// 一段文字的排版占位（`max_width` 给定即按该宽度换行）。
    fn measure(&self, request: MeasureRequest<'_>) -> MeasureTextResult;
    /// 把 Markdown 原文解析成结构化块；失败给可读原因（该块按无内容绘制）。
    fn markdown(&self, content: &str) -> Result<Vec<MarkdownBlock>, String>;
    /// 当前主题表。
    fn theme(&self) -> ThemeResult;
}

/// 一次 draw 的结果（壳侧据此决定画新内容还是沿用上一拍）。
pub enum DrawOutcome {
    /// 画成了：本拍内容，同时已记为该面的当前帧。
    Painted(std::sync::Arc<SurfaceFrame>),
    /// 这一拍沿用上一拍：插件未在运行、该面正被输入回调占用，或绘制失败（原因在插件日志里）。
    Skipped,
}

/// 一次 draw 调用在运行时内部的结果。
pub(crate) enum DrawCall {
    /// 画成了：这一拍的内容。
    Painted(SurfaceFrame),
    /// 该面正被输入回调占用：宿主沿用上一拍。
    Busy,
    /// 插件绘制失败或产物不合法：原因给调用方记日志。
    Failed(String),
}

/// 调一次插件绘制入口，收下它推来的指令、命中区与输入声明。
pub(crate) fn invoke(
    draw: abi::DrawFn,
    handle: abi::PluginHandle,
    surface_id: &str,
    width: f32,
    height: f32,
    host: &dyn DrawHost,
    log: &dyn Fn(u32, &str),
) -> DrawCall {
    let mut ctx = DrawCtx::new(host, log);
    let sink = DrawSink::new(&mut ctx);
    let target = abi::DrawTarget {
        surface_id: surface_id.as_ptr(),
        surface_id_len: surface_id.len(),
        width,
        height,
    };
    let code = unsafe { draw(handle, &target, &sink.table) };
    match code {
        abi::DRAW_OK => match ctx.finish() {
            Ok(frame) => DrawCall::Painted(frame),
            Err(reason) => DrawCall::Failed(reason),
        },
        abi::DRAW_BUSY => DrawCall::Busy,
        abi::DRAW_REJECTED => DrawCall::Failed("draw 期间调用了宿主能力面，这一拍作废".into()),
        abi::DRAW_FAILED => DrawCall::Failed("插件绘制失败".into()),
        other => DrawCall::Failed(format!("未知的绘制返回码：{other}")),
    }
}

/// 绘制函数表与它的宿主上下文绑在一起：插件只拿得到表，表里的 `ctx` 指回同一个上下文。
struct DrawSink {
    table: abi::DrawSink,
    /// 上下文只在这一拍内有效；这里持有裸指针只为让函数表里的地址有个明确的来源。
    _ctx: *mut DrawCtx<'static>,
}

impl DrawSink {
    fn new(ctx: &mut DrawCtx<'_>) -> Self {
        let raw = std::ptr::from_mut(ctx).cast::<DrawCtx<'static>>();
        let table = abi::DrawSink {
            ctx: raw as *mut c_void,
            measure: Some(sink_measure),
            markdown: Some(sink_markdown),
            theme: Some(sink_theme),
            command: Some(sink_command),
            hit: Some(sink_hit),
            policy: Some(sink_policy),
            log: Some(sink_log),
        };
        Self { table, _ctx: raw }
    }
}

/// 一次 draw 的收集器：指令、命中区、输入声明与交给插件的临时数据。
struct DrawCtx<'a> {
    host: &'a dyn DrawHost,
    /// 绘制期诊断输出：落插件日志（发布版没有 stderr 可看）。
    log: &'a dyn Fn(u32, &str),
    commands: Vec<DrawCommand>,
    hits: Vec<HitRegion>,
    ime: Option<FrameRect>,
    keys: bool,
    fps: Option<u16>,
    /// 插件侧的第一处违规：整帧按违规丢弃，原因给宿主记日志。
    error: Option<String>,
    arena: Arena,
}

impl<'a> DrawCtx<'a> {
    fn new(host: &'a dyn DrawHost, log: &'a dyn Fn(u32, &str)) -> Self {
        Self {
            host,
            log,
            commands: Vec::new(),
            hits: Vec::new(),
            ime: None,
            keys: false,
            fps: None,
            error: None,
            arena: Arena::default(),
        }
    }

    /// 记录一次插件侧违规：后面的指令照收，整帧在结束时按违规丢弃。
    fn reject(&mut self, reason: String) {
        if self.error.is_none() {
            self.error = Some(reason);
        }
    }

    fn finish(self) -> Result<SurfaceFrame, String> {
        if let Some(reason) = self.error {
            return Err(reason);
        }
        let frame = SurfaceFrame {
            commands: self.commands,
            hits: self.hits,
            ime: self.ime,
            keys: self.keys,
            fps: self.fps,
        };
        frame.validate().map_err(|err| format!("帧内容不合法：{err}"))?;
        Ok(frame)
    }
}

/// 本次 draw 内交给插件的字符串与结构：插件当场读走，这里只管活到调用返回。
///
/// 每块都是 `Box<[T]>`：外层 Vec 扩容不搬动内层数据，插件拿到的指针一直有效。
#[derive(Default)]
struct Arena {
    bytes: Vec<Box<[u8]>>,
    spans: Vec<Box<[abi::AbiMdSpan]>>,
    rows: Vec<Box<[abi::AbiMdRow]>>,
    cells: Vec<Box<[abi::AbiStr]>>,
    blocks: Vec<Box<[abi::AbiMdBlock]>>,
}

impl Arena {
    fn bytes(&mut self, text: &str) -> abi::AbiStr {
        let buffer = text.as_bytes().to_vec().into_boxed_slice();
        let item = abi::AbiStr { ptr: buffer.as_ptr(), len: buffer.len() };
        self.bytes.push(buffer);
        item
    }

    fn spans(&mut self, spans: &[MarkdownSpan]) -> (*const abi::AbiMdSpan, usize) {
        let mut table = Vec::with_capacity(spans.len());
        for span in spans {
            let text = self.bytes(&span.text);
            let link = match span.link.as_deref() {
                Some(link) => self.bytes(link),
                None => abi::AbiStr::empty(),
            };
            table.push(abi::AbiMdSpan {
                text,
                strong: u32::from(span.strong),
                code: u32::from(span.code),
                link,
            });
        }
        let buffer = table.into_boxed_slice();
        let item = (buffer.as_ptr(), buffer.len());
        self.spans.push(buffer);
        item
    }

    fn rows(&mut self, rows: &[Vec<String>]) -> (*const abi::AbiMdRow, usize) {
        let mut table = Vec::with_capacity(rows.len());
        for row in rows {
            let cells: Vec<abi::AbiStr> = row.iter().map(|cell| self.bytes(cell)).collect();
            let buffer = cells.into_boxed_slice();
            table.push(abi::AbiMdRow { cells: buffer.as_ptr(), cell_count: buffer.len() });
            self.cells.push(buffer);
        }
        let buffer = table.into_boxed_slice();
        let item = (buffer.as_ptr(), buffer.len());
        self.rows.push(buffer);
        item
    }

    fn blocks(&mut self, blocks: &[MarkdownBlock]) -> (*const abi::AbiMdBlock, usize) {
        let mut table = Vec::with_capacity(blocks.len());
        for block in blocks {
            let item = self.block(block);
            table.push(item);
        }
        let buffer = table.into_boxed_slice();
        let item = (buffer.as_ptr(), buffer.len());
        self.blocks.push(buffer);
        item
    }

    fn block(&mut self, block: &MarkdownBlock) -> abi::AbiMdBlock {
        let mut out = abi::AbiMdBlock {
            kind: abi::MD_PARAGRAPH,
            level: 0,
            depth: 0,
            ordered: 0,
            index: 0,
            spans: std::ptr::null(),
            span_count: 0,
            text: abi::AbiStr::empty(),
            lang: abi::AbiStr::empty(),
            rows: std::ptr::null(),
            row_count: 0,
        };
        match block {
            MarkdownBlock::Heading { level, depth, spans } => {
                out.kind = abi::MD_HEADING;
                out.level = u32::from(*level);
                out.depth = *depth;
                (out.spans, out.span_count) = self.spans(spans);
            }
            MarkdownBlock::Paragraph { depth, spans } => {
                out.depth = *depth;
                (out.spans, out.span_count) = self.spans(spans);
            }
            MarkdownBlock::ListItem { depth, ordered, index, spans } => {
                out.kind = abi::MD_LIST_ITEM;
                out.depth = *depth;
                out.ordered = u32::from(*ordered);
                out.index = *index;
                (out.spans, out.span_count) = self.spans(spans);
            }
            MarkdownBlock::Quote { depth, spans } => {
                out.kind = abi::MD_QUOTE;
                out.depth = *depth;
                (out.spans, out.span_count) = self.spans(spans);
            }
            MarkdownBlock::Code { depth, lang, content } => {
                out.kind = abi::MD_CODE;
                out.depth = *depth;
                out.text = self.bytes(content);
                out.lang = self.bytes(lang);
            }
            MarkdownBlock::Table { depth, rows } => {
                out.kind = abi::MD_TABLE;
                out.depth = *depth;
                (out.rows, out.row_count) = self.rows(rows);
            }
            MarkdownBlock::Divider => out.kind = abi::MD_DIVIDER,
        }
        out
    }
}

unsafe extern "C" fn sink_measure(
    ctx: *mut c_void,
    item: *const abi::AbiMeasureItem,
) -> abi::AbiMeasureResult {
    let empty = abi::AbiMeasureResult { width: 0.0, height: 0.0, lines: 0 };
    let Some(ctx) = (ctx as *mut DrawCtx<'_>).as_mut() else {
        return empty;
    };
    let Some(item) = item.as_ref() else {
        return empty;
    };
    let text = item.text.read().unwrap_or_default();
    // 字号必须为正：非法值进 iced 排版会给出无意义的占位（与 `query.measure` 的校验同口径）
    if !item.size.is_finite() || item.size <= 0.0 {
        return abi::AbiMeasureResult { width: 0.0, height: 0.0, lines: 1 };
    }
    let measured = ctx.host.measure(MeasureRequest {
        text: &text,
        size: item.size,
        weight: weight_of(item.weight),
        family: family_of(item.family),
        max_width: (item.max_width.is_finite() && item.max_width > 0.0).then_some(item.max_width),
    });
    abi::AbiMeasureResult {
        width: measured.width,
        height: measured.height,
        lines: measured.lines,
    }
}

unsafe extern "C" fn sink_markdown(
    ctx: *mut c_void,
    text: abi::AbiStr,
    out: *mut abi::AbiMarkdown,
) -> u32 {
    let Some(ctx) = (ctx as *mut DrawCtx<'_>).as_mut() else {
        return 1;
    };
    let Some(out) = out.as_mut() else {
        return 1;
    };
    let Some(content) = text.read() else {
        return 1;
    };
    if content.len() > method::MAX_MARKDOWN_BYTES {
        ctx.reject(format!("Markdown 文本超过 {} 字节", method::MAX_MARKDOWN_BYTES));
        return 1;
    }
    match ctx.host.markdown(&content) {
        Ok(blocks) => {
            let (table, count) = ctx.arena.blocks(&blocks);
            *out = abi::AbiMarkdown { blocks: table, count };
            0
        }
        Err(_) => {
            *out = abi::AbiMarkdown { blocks: std::ptr::null(), count: 0 };
            1
        }
    }
}

unsafe extern "C" fn sink_theme(ctx: *mut c_void, out: *mut abi::AbiTheme) {
    let Some(ctx) = (ctx as *mut DrawCtx<'_>).as_mut() else {
        return;
    };
    let Some(out) = out.as_mut() else {
        return;
    };
    let result = ctx.host.theme();
    let mut colors = [abi::AbiRgba { rgba: [0; 4] }; abi::THEME_COLOR_COUNT];
    for (index, token) in atelyx_plugin_protocol::paint::COLOR_TOKENS.iter().enumerate() {
        colors[index] = abi::AbiRgba { rgba: rgba(result.colors.get(*token)) };
    }
    *out = abi::AbiTheme {
        colors,
        font_sizes: [
            result.font_sizes.micro,
            result.font_sizes.caption,
            result.font_sizes.ui,
            result.font_sizes.body,
            result.font_sizes.h2,
            result.font_sizes.h1,
        ],
        radii: [result.radii.xs, result.radii.sm, result.radii.md],
    };
}

unsafe extern "C" fn sink_command(ctx: *mut c_void, kind: u32, data: *const c_void) {
    let Some(ctx) = (ctx as *mut DrawCtx<'_>).as_mut() else {
        return;
    };
    if data.is_null() {
        return;
    }
    if ctx.commands.len() >= frame::MAX_COMMANDS {
        ctx.reject(format!("绘制指令超过 {} 条", frame::MAX_COMMANDS));
        return;
    }
    match command_of(kind, data) {
        Ok(command) => ctx.commands.push(command),
        Err(reason) => ctx.reject(reason),
    }
}

unsafe extern "C" fn sink_hit(ctx: *mut c_void, hit: *const abi::AbiHit) {
    let Some(ctx) = (ctx as *mut DrawCtx<'_>).as_mut() else {
        return;
    };
    let Some(hit) = hit.as_ref() else {
        return;
    };
    let Some(id) = hit.id.read() else {
        ctx.reject("命中区 id 不可读".into());
        return;
    };
    ctx.hits.push(HitRegion {
        id,
        x: hit.x,
        y: hit.y,
        w: hit.w,
        h: hit.h,
        cursor: cursor_of(hit.cursor),
    });
}

unsafe extern "C" fn sink_log(ctx: *mut c_void, level: u32, ptr: *const u8, len: usize) {
    let Some(ctx) = (ctx as *mut DrawCtx<'_>).as_ref() else {
        return;
    };
    if ptr.is_null() && len > 0 {
        return;
    }
    let bytes = std::slice::from_raw_parts(ptr, len);
    (ctx.log)(level, &String::from_utf8_lossy(bytes));
}

unsafe extern "C" fn sink_policy(ctx: *mut c_void, ime: *const abi::AbiRect, keys: u32, fps: u32) {
    let Some(ctx) = (ctx as *mut DrawCtx<'_>).as_mut() else {
        return;
    };
    ctx.ime = ime.as_ref().map(|rect| FrameRect { x: rect.x, y: rect.y, w: rect.w, h: rect.h });
    ctx.keys = keys != 0;
    // 越界按上限收：静默丢弃与整帧拒收对插件作者是同一种「画面停了」，夹取至少能跑
    ctx.fps = u16::try_from(fps.min(u32::from(paint::MAX_FPS))).ok().filter(|fps| *fps > 0);
}

/// 按 kind 读一条绘制指令。
///
/// # Safety
/// `data` 须指向与 `kind` 对应的 ABI 结构。
unsafe fn command_of(kind: u32, data: *const c_void) -> Result<DrawCommand, String> {
    let command = match kind {
        abi::CMD_LINE => {
            let line = &*data.cast::<abi::AbiLine>();
            DrawCommand::Line {
                x1: line.x1,
                y1: line.y1,
                x2: line.x2,
                y2: line.y2,
                stroke: required_color(line.stroke, "线描边")?,
                width: line.width,
            }
        }
        abi::CMD_RECT => {
            let rect = &*data.cast::<abi::AbiRectShape>();
            let shadow = if rect.shadow.kind == 0 {
                None
            } else {
                Some(PaintShadow {
                    offset_x: rect.shadow.offset_x,
                    offset_y: rect.shadow.offset_y,
                    blur: rect.shadow.blur,
                    color: required_color(rect.shadow.color, "投影颜色")?,
                })
            };
            DrawCommand::Rect {
                x: rect.x,
                y: rect.y,
                w: rect.w,
                h: rect.h,
                fill: color(rect.fill)?,
                stroke: color(rect.stroke)?,
                stroke_width: has_stroke(rect.stroke).then_some(rect.stroke_width),
                corner_radius: (rect.corner_radius >= 0.0).then_some(rect.corner_radius),
                shadow,
            }
        }
        abi::CMD_PATH => {
            let path = &*data.cast::<abi::AbiPath>();
            let points: Vec<PaintPoint> = if path.points.is_null() {
                Vec::new()
            } else {
                std::slice::from_raw_parts(path.points, path.len)
                    .iter()
                    .map(|point| PaintPoint { x: point.x, y: point.y })
                    .collect()
            };
            DrawCommand::Path {
                points,
                closed: path.closed != 0,
                fill: color(path.fill)?,
                stroke: color(path.stroke)?,
                stroke_width: has_stroke(path.stroke).then_some(path.stroke_width),
            }
        }
        abi::CMD_CIRCLE => {
            let circle = &*data.cast::<abi::AbiCircle>();
            DrawCommand::Circle {
                cx: circle.cx,
                cy: circle.cy,
                r: circle.r,
                fill: color(circle.fill)?,
                stroke: color(circle.stroke)?,
                stroke_width: has_stroke(circle.stroke).then_some(circle.stroke_width),
            }
        }
        abi::CMD_TEXT => {
            let text = &*data.cast::<abi::AbiText>();
            DrawCommand::Text {
                x: text.x,
                y: text.y,
                content: text.content.read().unwrap_or_default(),
                size: text.size,
                color: required_color(text.color, "文字颜色")?,
                weight: weight_of(text.weight),
                family: family_of(text.family),
                max_width: (text.max_width > 0.0).then_some(text.max_width),
                align: align_of(text.align),
            }
        }
        abi::CMD_IMAGE => {
            let image = &*data.cast::<abi::AbiImage>();
            let path = image.path.read().unwrap_or_default();
            let url = image.url.read().unwrap_or_default();
            let source = match image.source {
                abi::IMAGE_PRIVATE => ImageSource::Private { path },
                abi::IMAGE_VAULT => ImageSource::Vault { path },
                abi::IMAGE_URL => ImageSource::Url { url, local: (image.local != 0).then_some(true) },
                abi::IMAGE_FILE => ImageSource::File { path },
                other => return Err(format!("未知图片源：{other}")),
            };
            DrawCommand::Image {
                source,
                x: image.x,
                y: image.y,
                w: image.w,
                h: image.h,
                fit: fit_of(image.fit),
            }
        }
        other => return Err(format!("未知绘制指令：{other}")),
    };
    Ok(command)
}

fn has_stroke(color: abi::AbiColor) -> bool {
    color.kind != abi::COLOR_NONE
}

fn color(value: abi::AbiColor) -> Result<Option<PaintColor>, String> {
    match value.kind {
        abi::COLOR_NONE => Ok(None),
        abi::COLOR_TOKEN => match ColorToken::from_index(value.token) {
            Some(name) => Ok(Some(PaintColor::Token { name })),
            None => Err(format!("未知颜色 token：{}", value.token)),
        },
        abi::COLOR_RGBA => Ok(Some(PaintColor::Rgba { rgba: value.rgba })),
        other => Err(format!("未知颜色类型：{other}")),
    }
}

fn required_color(value: abi::AbiColor, what: &str) -> Result<PaintColor, String> {
    color(value)?.ok_or_else(|| format!("{what}不可省略"))
}

fn weight_of(code: u32) -> Option<TextWeight> {
    match code {
        abi::WEIGHT_NORMAL => Some(TextWeight::Normal),
        abi::WEIGHT_MEDIUM => Some(TextWeight::Medium),
        abi::WEIGHT_SEMIBOLD => Some(TextWeight::SemiBold),
        _ => None,
    }
}

fn family_of(code: u32) -> Option<TextFamily> {
    match code {
        abi::FAMILY_UI => Some(TextFamily::Ui),
        abi::FAMILY_MONO => Some(TextFamily::Mono),
        _ => None,
    }
}

fn align_of(code: u32) -> Option<TextAlign> {
    match code {
        abi::ALIGN_START => Some(TextAlign::Start),
        abi::ALIGN_CENTER => Some(TextAlign::Center),
        abi::ALIGN_END => Some(TextAlign::End),
        _ => None,
    }
}

fn fit_of(code: u32) -> Option<ImageFit> {
    match code {
        abi::FIT_FILL => Some(ImageFit::Fill),
        abi::FIT_CONTAIN => Some(ImageFit::Contain),
        abi::FIT_COVER => Some(ImageFit::Cover),
        _ => None,
    }
}

fn cursor_of(code: u32) -> Option<HitCursor> {
    match code {
        abi::CURSOR_POINTER => Some(HitCursor::Pointer),
        abi::CURSOR_TEXT => Some(HitCursor::Text),
        abi::CURSOR_GRAB => Some(HitCursor::Grab),
        abi::CURSOR_GRABBING => Some(HitCursor::Grabbing),
        abi::CURSOR_CROSSHAIR => Some(HitCursor::Crosshair),
        abi::CURSOR_NOT_ALLOWED => Some(HitCursor::NotAllowed),
        abi::CURSOR_RESIZE_HORIZONTAL => Some(HitCursor::ResizeHorizontal),
        abi::CURSOR_RESIZE_VERTICAL => Some(HitCursor::ResizeVertical),
        _ => None,
    }
}

/// 十六进制颜色（`#rrggbb` 或 `#rrggbbaa`）→ RGBA；解析不出按全透明。
fn rgba(hex: &str) -> [u8; 4] {
    let digits = hex.strip_prefix('#').unwrap_or(hex);
    let channel = |range: std::ops::Range<usize>| {
        digits.get(range).and_then(|text| u8::from_str_radix(text, 16).ok()).unwrap_or(0)
    };
    match digits.len() {
        6 => [channel(0..2), channel(2..4), channel(4..6), u8::MAX],
        8 => [channel(0..2), channel(2..4), channel(4..6), channel(6..8)],
        _ => [0, 0, 0, 0],
    }
}

/// 主题 token 数必须与 ABI 的定长数组一致：两边漂移会在 extern "C" 回调里越界。
const _: () = assert!(
    abi::THEME_COLOR_COUNT == atelyx_plugin_protocol::paint::COLOR_TOKENS.len(),
    "ABI 主题 token 数与协议表不一致"
);

#[cfg(test)]
mod tests {
    use super::*;
    use atelyx_plugin_protocol::method::{FontSizes, Radii, ThemeColors};

    /// 绘制期诊断的落点：测试里只看有没有人调，不关心内容。
    fn quiet(_level: u32, _text: &str) {}

    /// 只回答固定值的宿主动作面：量算 / Markdown / 主题都按可断言的常量返回。
    struct FixedHost;

    impl DrawHost for FixedHost {
        fn measure(&self, request: MeasureRequest<'_>) -> MeasureTextResult {
            MeasureTextResult {
                width: request.text.chars().count() as f32 * request.size,
                height: request.size,
                lines: 1,
            }
        }

        fn markdown(&self, content: &str) -> Result<Vec<MarkdownBlock>, String> {
            Ok(vec![
                MarkdownBlock::Heading {
                    level: 2,
                    depth: 0,
                    spans: vec![MarkdownSpan {
                        text: content.into(),
                        strong: true,
                        code: false,
                        link: None,
                    }],
                },
                MarkdownBlock::Table { depth: 1, rows: vec![vec!["a".into(), "b".into()]] },
            ])
        }

        fn theme(&self) -> ThemeResult {
            let hex = |text: &str| text.to_string();
            ThemeResult {
                colors: ThemeColors {
                    text: hex("#111111"),
                    text_secondary: hex("#222222"),
                    text_muted: hex("#333333"),
                    accent: hex("#444444"),
                    accent_soft: hex("#555555"),
                    danger: hex("#666666"),
                    danger_soft: hex("#777777"),
                    success: hex("#888888"),
                    success_soft: hex("#999999"),
                    warning: hex("#aaaaaa"),
                    warning_soft: hex("#bbbbbb"),
                    surface: hex("#cccccc"),
                    surface_muted: hex("#dddddd"),
                    surface_sunken: hex("#eeeeee"),
                    hover: hex("#123456"),
                    border: hex("#654321"),
                    border_strong: hex("#0f0f0f"),
                    transparent: hex("#00000000"),
                },
                font_sizes: FontSizes {
                    micro: 10.0,
                    caption: 11.0,
                    ui: 12.0,
                    body: 13.0,
                    h2: 18.0,
                    h1: 24.0,
                },
                radii: Radii { xs: 2.0, sm: 4.0, md: 6.0 },
                event_colors: vec![hex("#000001")],
            }
        }
    }

    /// 一次 draw 的假入口：用绘制函数表把预定指令推回去，模拟 SDK 侧的行为。
    unsafe extern "C" fn paint(
        _handle: abi::PluginHandle,
        target: *const abi::DrawTarget,
        sink: *const abi::DrawSink,
    ) -> u32 {
        let target = &*target;
        let sink = &*sink;
        let id = std::slice::from_raw_parts(target.surface_id, target.surface_id_len);
        assert_eq!(std::str::from_utf8(id).unwrap(), "panel");
        assert_eq!(target.width, 320.0);
        let item = abi::AbiMeasureItem {
            text: abi::AbiStr::of("abc"),
            size: 10.0,
            weight: 0,
            family: 0,
            max_width: -1.0,
        };
        let measured = (sink.measure.unwrap())(sink.ctx, &item);
        assert_eq!(measured.width, 30.0);

        let mut theme = std::mem::zeroed::<abi::AbiTheme>();
        (sink.theme.unwrap())(sink.ctx, &mut theme);
        assert_eq!(theme.colors[0].rgba, [0x11, 0x11, 0x11, 0xff]);
        assert_eq!(theme.font_sizes[3], 13.0);

        let mut markdown = std::mem::zeroed::<abi::AbiMarkdown>();
        assert_eq!((sink.markdown.unwrap())(sink.ctx, abi::AbiStr::of("# hi"), &mut markdown), 0);
        assert_eq!(markdown.count, 2);

        let rect = abi::AbiRectShape {
            x: 1.0,
            y: 2.0,
            w: 3.0,
            h: 4.0,
            fill: abi::AbiColor {
                kind: abi::COLOR_TOKEN,
                token: ColorToken::Accent.index(),
                rgba: [0; 4],
            },
            stroke: abi::AbiColor::none(),
            stroke_width: 1.0,
            corner_radius: -1.0,
            shadow: abi::AbiShadow {
                kind: 0,
                offset_x: 0.0,
                offset_y: 0.0,
                blur: 0.0,
                color: abi::AbiColor::none(),
            },
        };
        let data = (&rect as *const abi::AbiRectShape).cast();
        (sink.command.unwrap())(sink.ctx, abi::CMD_RECT, data);

        let hit = abi::AbiHit {
            id: abi::AbiStr::of("inc"),
            x: 0.0,
            y: 0.0,
            w: 50.0,
            h: 20.0,
            cursor: abi::CURSOR_POINTER,
        };
        (sink.hit.unwrap())(sink.ctx, &hit);
        (sink.policy.unwrap())(sink.ctx, std::ptr::null(), 1, 30);
        abi::DRAW_OK
    }

    #[test]
    fn a_draw_call_collects_commands_hits_and_policy() {
        let call = invoke(paint, std::ptr::null_mut(), "panel", 320.0, 200.0, &FixedHost, &quiet);
        let DrawCall::Painted(frame) = call else {
            panic!("应画成");
        };
        assert_eq!(frame.commands.len(), 1);
        assert_eq!(frame.hits.len(), 1);
        assert_eq!(frame.hits[0].id, "inc");
        assert_eq!(frame.hits[0].cursor, Some(HitCursor::Pointer));
        assert!(frame.keys);
        assert_eq!(frame.fps, Some(30));
        assert_eq!(frame.hit_at(10.0, 10.0), Some("inc"));
    }

    #[test]
    fn invalid_hits_discard_the_whole_frame() {
        unsafe extern "C" fn paint_broken(
            _handle: abi::PluginHandle,
            _target: *const abi::DrawTarget,
            sink: *const abi::DrawSink,
        ) -> u32 {
            let sink = &*sink;
            let hit = abi::AbiHit {
                id: abi::AbiStr::of(""),
                x: 0.0,
                y: 0.0,
                w: 1.0,
                h: 1.0,
                cursor: 0,
            };
            (sink.hit.unwrap())(sink.ctx, &hit);
            abi::DRAW_OK
        }
        assert!(matches!(
            invoke(paint_broken, std::ptr::null_mut(), "panel", 10.0, 10.0, &FixedHost, &quiet),
            DrawCall::Failed(_)
        ));
    }

    #[test]
    fn busy_is_reported_distinctly_from_failure() {
        unsafe extern "C" fn busy(
            _handle: abi::PluginHandle,
            _target: *const abi::DrawTarget,
            _sink: *const abi::DrawSink,
        ) -> u32 {
            abi::DRAW_BUSY
        }
        assert!(matches!(
            invoke(busy, std::ptr::null_mut(), "panel", 10.0, 10.0, &FixedHost, &quiet),
            DrawCall::Busy
        ));
    }

    #[test]
    fn hex_colors_parse_to_rgba() {
        assert_eq!(rgba("#0f1011"), [0x0f, 0x10, 0x11, 0xff]);
        assert_eq!(rgba("#0f101180"), [0x0f, 0x10, 0x11, 0x80]);
        assert_eq!(rgba("bad"), [0, 0, 0, 0]);
    }
}
