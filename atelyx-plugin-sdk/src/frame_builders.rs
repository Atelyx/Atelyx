//! 整面帧构建辅助：绘制指令的薄构造器 + 帧外壳（命中区 / 输入法锚点 / 键盘与帧率声明）。
//! 命中判断由宿主按几何完成，插件只按回发的命中 id 解释语义；坐标一律面本地逻辑像素。

use crate::protocol::frame::{FrameRect, HitCursor, HitRegion, SurfaceFrame};
use crate::protocol::paint::{
    ColorToken, DrawCommand, ImageFit, ImageSource, PaintColor, PaintPoint, TextAlign, TextWeight,
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
    DrawCommand::Line { x1, y1, x2, y2, stroke, width: 1.0 }
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
    DrawCommand::Rect { x, y, w, h, fill, stroke, stroke_width: None, corner_radius: None }
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
    DrawCommand::Rect { x, y, w, h, fill, stroke, stroke_width: None, corner_radius: Some(radius) }
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

/// 文字：`(x, y)` 为左上角。文字恒绘制在形状之上。
pub fn text(x: f32, y: f32, content: impl Into<String>, size: f32, color: PaintColor) -> DrawCommand {
    DrawCommand::Text {
        x,
        y,
        content: content.into(),
        size,
        color,
        weight: None,
        family: None,
        max_width: None,
        align: None,
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
        max_width: None,
        align: None,
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
        max_width: Some(max_width),
        align,
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
    SurfaceFrame { commands, hits, ime: None, keys: false, fps: None }
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
