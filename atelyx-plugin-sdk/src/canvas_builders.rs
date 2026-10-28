//! 画布指令清单构建辅助：薄构造器收拢字段默认值，插件只写关心的项。
//! 命中判断由插件按自己提交的指令几何自算——坐标口径与指令一致（节点本地逻辑像素）。

use crate::protocol::canvas::{CanvasColor, CanvasPoint, DrawCommand};
use crate::protocol::view::{ColorToken, ImageFit, ImageSource, TextWeight};

/// 主题 token 颜色。
pub fn token(name: ColorToken) -> CanvasColor {
    CanvasColor::Token { name }
}

/// 自带 RGBA 颜色（`[r, g, b, a]`，各 0~255）。
pub fn rgba(r: u8, g: u8, b: u8, a: u8) -> CanvasColor {
    CanvasColor::Rgba { rgba: [r, g, b, a] }
}

fn pt(x: f32, y: f32) -> CanvasPoint {
    CanvasPoint { x, y }
}

pub fn line(x1: f32, y1: f32, x2: f32, y2: f32, stroke: CanvasColor) -> DrawCommand {
    DrawCommand::Line { x1, y1, x2, y2, stroke, width: 1.0 }
}

/// 矩形：`fill` 与 `stroke` 至少给其一。
pub fn rect(
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    fill: Option<CanvasColor>,
    stroke: Option<CanvasColor>,
) -> DrawCommand {
    DrawCommand::Rect { x, y, w, h, fill, stroke, stroke_width: None, corner_radius: None }
}

/// 折线 / 多边形：`closed` 闭合末点与起点；`fill` 与 `stroke` 至少给其一。
pub fn path(
    points: Vec<(f32, f32)>,
    closed: bool,
    fill: Option<CanvasColor>,
    stroke: Option<CanvasColor>,
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
    fill: Option<CanvasColor>,
    stroke: Option<CanvasColor>,
) -> DrawCommand {
    DrawCommand::Circle { cx, cy, r, fill, stroke, stroke_width: None }
}

/// 文字（(x, y) 为左上角；宿主 canvas 文字永远绘制在所有图层之上）。
/// 名字带 `draw_` 前缀，避免与 [`crate::view_builders::text`] 撞名。
pub fn draw_text(
    x: f32,
    y: f32,
    content: impl Into<String>,
    size: f32,
    color: CanvasColor,
) -> DrawCommand {
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

pub fn draw_text_weighted(
    x: f32,
    y: f32,
    content: impl Into<String>,
    size: f32,
    color: CanvasColor,
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
