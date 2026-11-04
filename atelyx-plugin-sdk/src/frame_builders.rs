//! 整面帧构建辅助：绘制指令的薄构造器 + 帧外壳（命中区 / 输入法锚点 / 键盘与帧率声明）。
//! 命中判断由宿主按几何完成，插件只按回发的命中 id 解释语义；坐标一律面本地逻辑像素。

use crate::protocol::frame::{FrameRect, HitCursor, HitRegion, SurfaceFrame};
use crate::protocol::paint::{
    ColorToken, DrawCommand, ImageFit, ImageSource, PaintColor, PaintPoint, PaintShadow, TextAlign,
    TextLayout, TextShaping, TextWeight,
};

/// 主题 token 颜色。
pub fn token(name: ColorToken) -> PaintColor {
    PaintColor::Token { name }
}

/// 自带 RGBA 颜色（`[r, g, b, a]`，各 0~255）。
pub fn rgba(r: u8, g: u8, b: u8, a: u8) -> PaintColor {
    PaintColor::Rgba { rgba: [r, g, b, a] }
}

fn pt(x: f32, y: f32) -> PaintPoint {
    PaintPoint { x, y }
}

pub fn line(x1: f32, y1: f32, x2: f32, y2: f32, stroke: PaintColor) -> DrawCommand {
    DrawCommand::Line { x1, y1, x2, y2, stroke, width: 1.0, dash: None }
}

/// 虚线（`dash` = 实线长 / 空白长，像素）。
pub fn dashed_line(
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    stroke: PaintColor,
    width: f32,
    dash: [f32; 2],
) -> DrawCommand {
    DrawCommand::Line { x1, y1, x2, y2, stroke, width, dash: Some(dash) }
}

/// 矩形：`fill` 与 `stroke` 至少给其一。
pub fn rect(
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    fill: Option<PaintColor>,
    stroke: Option<PaintColor>,
) -> DrawCommand {
    DrawCommand::Rect { x, y, w, h, fill, stroke, stroke_width: None, corner_radius: None, shadow: None }
}

/// 圆角矩形（`radius` 为四角半径）：`fill` 与 `stroke` 至少给其一。
pub fn rounded_rect(
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radius: f32,
    fill: Option<PaintColor>,
    stroke: Option<PaintColor>,
) -> DrawCommand {
    DrawCommand::Rect { x, y, w, h, fill, stroke, stroke_width: None, corner_radius: Some(radius), shadow: None }
}

/// 矩形投影参数：偏移 / 模糊半径 / 颜色。
pub fn shadow(offset_x: f32, offset_y: f32, blur: f32, color: PaintColor) -> PaintShadow {
    PaintShadow { offset_x, offset_y, blur, color }
}

/// 折线 / 多边形：`closed` 闭合末点与起点；`fill` 与 `stroke` 至少给其一。
pub fn path(
    points: Vec<(f32, f32)>,
    closed: bool,
    fill: Option<PaintColor>,
    stroke: Option<PaintColor>,
) -> DrawCommand {
    DrawCommand::Path {
        points: points.into_iter().map(|(x, y)| pt(x, y)).collect(),
        closed,
        fill,
        stroke,
        stroke_width: None,
    }
}

/// 圆：`fill` 与 `stroke` 至少给其一。
pub fn circle(
    cx: f32,
    cy: f32,
    r: f32,
    fill: Option<PaintColor>,
    stroke: Option<PaintColor>,
) -> DrawCommand {
    DrawCommand::Circle { cx, cy, r, fill, stroke, stroke_width: None }
}

/// 文字：`(x, y)` 为左上角。文字与形状按声明顺序绘制。
pub fn text(x: f32, y: f32, content: impl Into<String>, size: f32, color: PaintColor) -> DrawCommand {
    DrawCommand::Text {
        x,
        y,
        content: content.into(),
        size,
        color,
        weight: None,
        family: None,
        family_name: None,
        italic: false,
        max_width: None,
        align: None,
        layout: None,
    }
}

pub fn text_weighted(
    x: f32,
    y: f32,
    content: impl Into<String>,
    size: f32,
    color: PaintColor,
    weight: TextWeight,
) -> DrawCommand {
    DrawCommand::Text {
        x,
        y,
        content: content.into(),
        size,
        color,
        weight: Some(weight),
        family: None,
        family_name: None,
        italic: false,
        max_width: None,
        align: None,
        layout: None,
    }
}

/// 按宽度换行并给对齐区域的文字（`align` 只在给了 `max_width` 时成立）。
pub fn text_wrapped(
    x: f32,
    y: f32,
    content: impl Into<String>,
    size: f32,
    color: PaintColor,
    max_width: f32,
    align: Option<TextAlign>,
) -> DrawCommand {
    DrawCommand::Text {
        x,
        y,
        content: content.into(),
        size,
        color,
        weight: None,
        family: None,
        family_name: None,
        italic: false,
        max_width: Some(max_width),
        align,
        layout: None,
    }
}

/// 带排版口径的文字（行高倍数 / 换行 / 整形）：与 `query.measure` 的同名口径一致，
/// 量出来的行高与折行点才对得上绘制结果。
pub fn text_aligned(
    x: f32,
    y: f32,
    content: impl Into<String>,
    size: f32,
    color: PaintColor,
    weight: Option<TextWeight>,
    family: Option<crate::protocol::paint::TextFamily>,
    italic: bool,
    max_width: Option<f32>,
    align: Option<TextAlign>,
    layout: Option<crate::protocol::paint::TextLayout>,
) -> DrawCommand {
    DrawCommand::Text {
        x,
        y,
        content: content.into(),
        size,
        color,
        weight,
        family,
        family_name: None,
        italic,
        max_width,
        align,
        layout,
    }
}

/// 图片贴片，按给定矩形绘制（缺省拉伸填满，比例由插件自己保持）。
pub fn image_tile(source: ImageSource, x: f32, y: f32, w: f32, h: f32) -> DrawCommand {
    DrawCommand::Image { source, x, y, w, h, fit: None }
}

/// 图片贴片，按 `fit` 在矩形内等比适配（`contain` 留边 / `cover` 裁切）。
pub fn image_tile_fit(
    source: ImageSource,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    fit: ImageFit,
) -> DrawCommand {
    DrawCommand::Image { source, x, y, w, h, fit: Some(fit) }
}

/// 命中区（光标缺省 = 系统箭头）。
pub fn hit(id: impl Into<String>, x: f32, y: f32, w: f32, h: f32) -> HitRegion {
    HitRegion { id: id.into(), x, y, w, h, cursor: None }
}

/// 带光标形状的命中区（悬停时宿主据此换指针）。
pub fn hit_with_cursor(
    id: impl Into<String>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    cursor: HitCursor,
) -> HitRegion {
    HitRegion { id: id.into(), x, y, w, h, cursor: Some(cursor) }
}

/// 输入法光标 / 候选窗锚点矩形（面本地逻辑像素）。
pub fn ime_rect(x: f32, y: f32, w: f32, h: f32) -> FrameRect {
    FrameRect { x, y, w, h }
}

/// 帧外壳：指令与命中区，不声明按键 / 输入法 / 帧率。
pub fn frame(commands: Vec<DrawCommand>, hits: Vec<HitRegion>) -> SurfaceFrame {
    SurfaceFrame { size: None, commands, hits, ime: None, keys: false, fps: None, popup: None }
}

/// 声明本面收键盘：指针按下取焦，随后全部按键（含修饰键组合）经 `FrameInput::Key` 回发。
pub fn keys(mut frame: SurfaceFrame) -> SurfaceFrame {
    frame.keys = true;
    frame
}

/// 声明本面收输入法：组合串与提交文本经 `FrameInput::Text` 回发，`rect` 定位光标与候选窗。
pub fn ime(mut frame: SurfaceFrame, rect: FrameRect) -> SurfaceFrame {
    frame.ime = Some(rect);
    frame
}

/// 声明帧率（1–120）：宿主按该节拍回发 `host:frame.tick`。
pub fn fps(mut frame: SurfaceFrame, fps: u16) -> SurfaceFrame {
    frame.fps = Some(fps);
    frame
}

/// 空态图标边长。
pub const EMPTY_ICON: f32 = 64.0;
/// 空态图标 / 标题 / 说明之间的间距。
pub const EMPTY_GAP: f32 = 16.0;

/// 空态文字档：字号与行高由调用方按主题取（宿主量算值），SDK 不自估。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EmptyStateText {
    pub size: f32,
    pub line: f32,
}

impl EmptyStateText {
    /// 宿主绘制用的行高倍数（与 `line` 一致，避免排版与绘制各行其是）。
    fn line_factor(&self) -> f32 {
        if self.size > 0.0 { (self.line / self.size).max(0.0) } else { 1.2 }
    }
}

/// 空态内容块高度（图标 + 标题 + 说明）：调用方要在块下挂按钮等附加内容时按它让位。
pub fn empty_state_height(title_line: f32, desc_line: f32) -> f32 {
    EMPTY_ICON + EMPTY_GAP * 2.0 + title_line + desc_line
}

/// 空态图标在给定宽度内的水平位置（块内左缘）。
fn icon_rect(x: f32, y: f32, w: f32) -> crate::frame_ui::Rect {
    crate::frame_ui::Rect::new(x + (w - EMPTY_ICON) / 2.0, y, EMPTY_ICON, EMPTY_ICON)
}

/// 空态整块（图标 + 标题 + 说明）：在 `area` 内水平 / 垂直居中，坐标面本地。
/// 图标按 lucide 名称取；标题与说明以面宽为对齐区域居中。
pub fn empty_state(
    area: (f32, f32, f32, f32),
    icon: &str,
    title: &str,
    desc: &str,
    title_text: EmptyStateText,
    desc_text: EmptyStateText,
) -> Vec<DrawCommand> {
    let (x, y, w, h) = area;
    let block = empty_state_height(title_text.line, desc_text.line);
    let top = y + (h - block) / 2.0;
    let title_y = top + EMPTY_ICON + EMPTY_GAP;
    let desc_y = title_y + title_text.line + EMPTY_GAP;
    let mut out =
        crate::frame_ui::icon_commands(icon, icon_rect(x, top, w), token(ColorToken::TextMuted));
    let title = text_aligned(
        x,
        title_y,
        title,
        title_text.size,
        token(ColorToken::Text),
        Some(TextWeight::SemiBold),
        None,
        false,
        Some(w),
        Some(TextAlign::Center),
        Some(TextLayout {
            line_height: Some(title_text.line_factor()),
            wrap: None,
            shaping: Some(TextShaping::Advanced),
        }),
    );
    let desc = text_aligned(
        x,
        desc_y,
        desc,
        desc_text.size,
        token(ColorToken::TextMuted),
        None,
        None,
        false,
        Some(w),
        Some(TextAlign::Center),
        Some(TextLayout {
            line_height: Some(desc_text.line_factor()),
            wrap: None,
            shaping: Some(TextShaping::Advanced),
        }),
    );
    out.push(title);
    out.push(desc);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text<'a>(commands: &'a [DrawCommand], content: &str) -> &'a DrawCommand {
        commands
            .iter()
            .find(|command| matches!(command, DrawCommand::Text { content: text, .. } if text == content))
            .expect("空态缺少文字指令")
    }

    // 面 400×300、标题 20/24、说明 12/14.4：图标左缘 = (400-64)/2，整块上下沿中心 = 150。
    #[test]
    fn empty_state_block_is_centered_in_area() {
        let title = EmptyStateText { size: 20.0, line: 24.0 };
        let desc = EmptyStateText { size: 12.0, line: 14.4 };
        let block = empty_state_height(title.line, desc.line);
        let top = (300.0 - block) / 2.0;
        let commands = empty_state(
            (0.0, 0.0, 400.0, 300.0),
            "file-text",
            "打开笔记",
            "在文件面板或搜索面板单击一个 .md 笔记开始编辑。",
            title,
            desc,
        );

        let icon = icon_rect(0.0, top, 400.0);
        assert_eq!(icon.x, 168.0);
        assert_eq!(icon.y, top);
        assert_eq!(icon.w, 64.0);
        assert_eq!(icon.h, 64.0);

        for content in ["打开笔记", "在文件面板或搜索面板单击一个 .md 笔记开始编辑。"] {
            let DrawCommand::Text { x, max_width, align, .. } = text(&commands, content) else {
                unreachable!()
            };
            assert_eq!(*x, 0.0);
            assert_eq!(*max_width, Some(400.0));
            assert_eq!(*align, Some(TextAlign::Center));
        }

        let title_y = match text(&commands, "打开笔记") {
            DrawCommand::Text { y, .. } => *y,
            _ => unreachable!(),
        };
        let desc_y = match text(&commands, "在文件面板或搜索面板单击一个 .md 笔记开始编辑。") {
            DrawCommand::Text { y, .. } => *y,
            _ => unreachable!(),
        };
        assert!((title_y - (top + EMPTY_ICON + EMPTY_GAP)).abs() < 0.5);
        assert!((desc_y - (title_y + title.line + EMPTY_GAP)).abs() < 0.5);
        let block_top = title_y - EMPTY_ICON - EMPTY_GAP;
        let block_bottom = desc_y + desc.line;
        assert!(((block_top + block_bottom) / 2.0 - 150.0).abs() < 0.5);
    }
}

