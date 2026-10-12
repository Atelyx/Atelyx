//! 矢量图标部件：lucide 形状（24 单位视框）在 iced Canvas 上描边绘制。
//!
//! 展平在 `init` 时一次完成并缓存；绘制帧只做坐标缩放与描边，
//! 描边宽度 2 单位随帧缩放，等价 lucide 的矢量缩放（14px 时 ≈1.17px 线宽）。

use std::collections::HashMap;
use std::sync::OnceLock;

use iced::widget::canvas::{self, Canvas, LineCap, LineDash, LineJoin, Path, Stroke};
use iced::{Color, Element, Length, Point, Rectangle, Vector};

use super::icons::{self, Shape};
use super::svgpath;

/// 图标几何缓存：kebab 名 → 展平子路径（24 单位坐标）。
static CACHE: OnceLock<HashMap<&'static str, Vec<svgpath::Polyline>>> = OnceLock::new();

// 每帧重绘（State = ()）：canvas::Cache 只按尺寸失效，挂实例 State 会把
// 变色/换形图标画到陈旧几何；展平结果已在静态缓存，每帧只剩描边。

/// 启动时展平全部图标形状（一次性，微秒级）。
pub fn init() {
    CACHE.get_or_init(|| {
        let mut map = HashMap::new();
        for &name in icons::NAMES {
            map.insert(name, flatten_icon(name));
        }
        map
    });
}

fn flatten_icon(name: &str) -> Vec<svgpath::Polyline> {
    let Some(shapes) = icons::icon(name) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for shape in shapes {
        match *shape {
            Shape::Path(d) => out.extend(svgpath::flatten_path(d)),
            Shape::Circle(cx, cy, r) => {
                // 圆用 48 段折线逼近（描边后与圆弧无可见差）
                let segs = 48;
                let mut poly = Vec::with_capacity(segs + 1);
                for i in 0..=segs {
                    let a = (i as f32) * std::f32::consts::TAU / segs as f32;
                    poly.push([cx + r * a.cos(), cy + r * a.sin()]);
                }
                out.push(poly);
            }
            Shape::Rect(x, y, w, h, rx) => {
                if rx > 0.0 {
                    out.extend(rounded_rect(x, y, w, h, rx));
                } else {
                    out.push(vec![[x, y], [x + w, y], [x + w, y + h], [x, y + h], [x, y]]);
                }
            }
            Shape::Line([x1, y1, x2, y2]) => out.push(vec![[x1, y1], [x2, y2]]),
        }
    }
    out
}

/// 圆角矩形（rx 统一圆角）单条闭合折线：四段 90° 角弧 + 直边连线。
fn rounded_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Vec<svgpath::Polyline> {
    let r = r.min(w / 2.0).min(h / 2.0);
    const SEG: usize = 8;
    let arc = |cx: f32, cy: f32, a0: f32| -> Vec<[f32; 2]> {
        (0..=SEG)
            .map(|i| {
                let a = a0 + (i as f32 / SEG as f32) * std::f32::consts::FRAC_PI_2;
                [cx + r * a.cos(), cy + r * a.sin()]
            })
            .collect()
    };
    // 右上 → 右下 → 左下 → 左上；相邻弧端点间由直线段自然连接（即四条直边）
    let mut poly = arc(x + w - r, y + r, -std::f32::consts::FRAC_PI_2);
    poly.extend(arc(x + w - r, y + h - r, 0.0));
    poly.extend(arc(x + r, y + h - r, std::f32::consts::FRAC_PI_2));
    poly.extend(arc(x + r, y + r, std::f32::consts::PI));
    // 闭合回起点（首点 = (x, y)）
    poly.push(poly[0]);
    vec![poly]
}

/// 图标展平折线（画布内自绘用）：kebab 名 → 24 单位坐标子路径；未展平（init 前）返回 None。
pub fn polylines(name: &str) -> Option<&'static [svgpath::Polyline]> {
    let name: &'static str = icons::NAMES.iter().copied().find(|n| *n == name)?;
    CACHE.get().and_then(|m| m.get(name)).map(|v| v.as_slice())
}

/// 图标部件：绘制以实际 bounds 为准（视框边长在 icon_stroke 构造 Canvas 宽高时给定）。
pub struct Icon {
    pub name: &'static str,
    pub color: Color,
    /// 描边宽度（24 单位视框坐标；lucide 缺省 2）。
    pub stroke_units: f32,
}

impl<Message> canvas::Program<Message> for Icon
where
    Message: 'static,
{
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &iced::Renderer,
        _theme: &iced::Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let Some(subpaths) = CACHE.get().and_then(|m| m.get(self.name)) else {
            return vec![];
        };
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        // 24 单位视框 → size 见方，居中
        let k = frame.width() / 24.0;
        let offset = Vector::new(
            (frame.width() - 24.0 * k) / 2.0,
            (frame.height() - 24.0 * k) / 2.0,
        );
        let stroke = Stroke {
            style: canvas::Style::Solid(self.color),
            width: self.stroke_units,
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            line_dash: LineDash { segments: &[], offset: 0 },
        };
        frame.with_save(|f| {
            f.translate(offset);
            f.scale(k);
            for poly in subpaths.iter() {
                if poly.len() < 2 {
                    continue;
                }
                let path = Path::new(|p| {
                    p.move_to(Point::new(poly[0][0], poly[0][1]));
                    for pt in &poly[1..] {
                        p.line_to(Point::new(pt[0], pt[1]));
                    }
                });
                f.stroke(&path, stroke);
            }
        });
        vec![frame.into_geometry()]
    }
}

/// 图标元素（`size` 见方、缺省 2 单位描边；未知图标名画空）。
pub fn icon<Message: 'static>(name: &str, size: f32, color: Color) -> Element<'static, Message> {
    icon_stroke(name, size, color, 2.0)
}

/// 图标元素（自定描边宽度；lucide strokeWidth 1.5 档等价 stroke_units = 1.5）。
pub fn icon_stroke<Message: 'static>(
    name: &str,
    size: f32,
    color: Color,
    stroke_units: f32,
) -> Element<'static, Message> {
    let name: &'static str = icons::NAMES
        .iter()
        .copied()
        .find(|n| *n == name)
        .unwrap_or("");
    Canvas::new(Icon { name, color, stroke_units })
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .into()
}
