//! 矢量图标部件：形状表（24 单位视框）在 iced Canvas 上描边绘制。
//!
//! 展平在 `init` 时一次完成并缓存；绘制帧只做坐标缩放与描边。
//! 描边按视框单位随尺寸缩放（2 单位在 14px ≈ 1.17px），但绘制宽度不低于
//! 1 逻辑像素——小图标再细会发灰。

use std::collections::HashMap;
use std::sync::OnceLock;

use iced::widget::canvas::{self, Canvas, LineCap, LineDash, LineJoin, Path, Stroke};
use iced::{Color, Element, Length, Point, Rectangle, Vector};

use super::icons::{self, Shape};

/// 一条展平后的子路径（24 单位坐标）。
pub type Polyline = Vec<[f32; 2]>;

/// 图标几何缓存：kebab 名 → 展平子路径（24 单位坐标）。
static CACHE: OnceLock<HashMap<&'static str, Vec<Polyline>>> = OnceLock::new();

// 每帧重绘（State = ()）：canvas::Cache 只按尺寸失效，挂实例 State 会把
// 变色/换形图标画到陈旧几何；展平结果已在静态缓存，每帧只剩描边。

/// 启动时展平全部图标形状（一次性，微秒级）。
pub fn init() {
    CACHE.get_or_init(|| {
        let mut map = HashMap::new();
        for name in icons::names() {
            map.insert(name, flatten_icon(name));
        }
        map
    });
}

fn flatten_icon(name: &str) -> Vec<Polyline> {
    let Some(shapes) = icons::icon(name) else {
        return Vec::new();
    };
    shapes.iter().flat_map(flatten_shape).collect()
}

fn flatten_shape(shape: &Shape) -> Vec<Polyline> {
    match *shape {
        Shape::Line([x1, y1, x2, y2]) => vec![vec![[x1, y1], [x2, y2]]],
        Shape::Polyline { points, closed } => {
            let mut poly = points.to_vec();
            if closed {
                poly.push(points[0]);
            }
            vec![poly]
        }
        Shape::Circle { cx, cy, r } => {
            // 圆按 48 段折线逼近（描边后与圆弧无可见差）
            const SEG: usize = 48;
            vec![(0..=SEG)
                .map(|i| {
                    let a = (i as f32) * std::f32::consts::TAU / SEG as f32;
                    [cx + r * a.cos(), cy + r * a.sin()]
                })
                .collect()]
        }
        Shape::Arc { cx, cy, r, from, to } => vec![arc(cx, cy, r, from, to)],
        Shape::Rect { x, y, w, h, radius } => {
            if radius > 0.0 {
                rounded_rect(x, y, w, h, radius)
            } else {
                vec![vec![[x, y], [x + w, y], [x + w, y + h], [x, y + h], [x, y]]]
            }
        }
    }
}

/// 圆弧折线：每 6° 一段（最小 2 段），端点落在给定角度上。
fn arc(cx: f32, cy: f32, r: f32, from: f32, to: f32) -> Polyline {
    let sweep = (to - from).to_radians();
    let segs = ((sweep.abs().to_degrees() / 6.0).ceil() as usize).max(2);
    (0..=segs)
        .map(|i| {
            let a = from.to_radians() + sweep * (i as f32 / segs as f32);
            [cx + r * a.cos(), cy + r * a.sin()]
        })
        .collect()
}

/// 圆角矩形（radius 统一圆角）单条闭合折线：四段 90° 角弧 + 直边连线。
fn rounded_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Vec<Polyline> {
    let r = r.min(w / 2.0).min(h / 2.0);
    const SEG: usize = 8;
    let corner = |cx: f32, cy: f32, a0: f32| -> Vec<[f32; 2]> {
        (0..=SEG)
            .map(|i| {
                let a = a0 + (i as f32 / SEG as f32) * std::f32::consts::FRAC_PI_2;
                [cx + r * a.cos(), cy + r * a.sin()]
            })
            .collect()
    };
    // 右上 → 右下 → 左下 → 左上；相邻弧端点间由直线段自然连接（即四条直边）
    let mut poly = corner(x + w - r, y + r, -std::f32::consts::FRAC_PI_2);
    poly.extend(corner(x + w - r, y + h - r, 0.0));
    poly.extend(corner(x + r, y + h - r, std::f32::consts::FRAC_PI_2));
    poly.extend(corner(x + r, y + r, std::f32::consts::PI));
    // 闭合回起点（首点 = (x, y)）
    poly.push(poly[0]);
    vec![poly]
}

/// 图标展平折线（画布内自绘用）：kebab 名 → 24 单位坐标子路径；未展平（init 前）返回 None。
pub fn polylines(name: &str) -> Option<&'static [Polyline]> {
    let name: &'static str = icons::names().find(|n| *n == name)?;
    CACHE.get().and_then(|m| m.get(name)).map(|v| v.as_slice())
}

/// 图标部件：绘制以实际 bounds 为准（视框边长在 icon_stroke 构造 Canvas 宽高时给定）。
pub struct Icon {
    pub name: &'static str,
    pub color: Color,
    /// 描边宽度（24 单位视框坐标；缺省 2）。
    pub stroke_units: f32,
    /// 旋转角（弧度；0 = 不转，转圈动画用）。
    pub rotation: f32,
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
        let missing;
        let subpaths = match CACHE.get().and_then(|m| m.get(self.name)) {
            Some(found) => found,
            None => {
                // 名字未命中（写错）：画方框占位而不是留白——
                // 空白会让「名字写错」看起来像「控件没渲染」，排查方向完全错位
                missing = rounded_rect(4.0, 4.0, 16.0, 16.0, 3.0);
                &missing
            }
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
            // 视框单位 → 逻辑像素；小图标缩完不足 1px 时钉在下限，避免细线发灰。
            width: (self.stroke_units * k).max(1.0),
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            line_dash: LineDash { segments: &[], offset: 0 },
        };
        frame.with_save(|f| {
            f.translate(offset);
            f.scale(k);
            if self.rotation != 0.0 {
                // 旋转轴 = 视框中心（rotate 绕当前原点，须先平移中心再转回）
                f.translate(Vector::new(12.0, 12.0));
                f.rotate(self.rotation);
                f.translate(Vector::new(-12.0, -12.0));
            }
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

/// 全部图标名（kebab-case）：文档、校验与开发期工具按它枚举。
pub fn names() -> impl Iterator<Item = &'static str> {
    icons::names()
}

/// 图标名归一：命中形状表返回 'static 名，未知 / 空串返回空（画空图标）。
pub fn icon_name(name: &str) -> &'static str {
    icons::names().find(|n| *n == name).unwrap_or("")
}

/// 图标元素（`size` 见方、缺省 2 单位描边；未知图标名画空）。
pub fn icon<Message: 'static>(name: &str, size: f32, color: Color) -> Element<'static, Message> {
    icon_stroke(name, size, color, 2.0)
}

/// 图标元素（自定描边宽度；stroke_units 用 24 单位视框坐标）。
pub fn icon_stroke<Message: 'static>(
    name: &str,
    size: f32,
    color: Color,
    stroke_units: f32,
) -> Element<'static, Message> {
    icon_spin(name, size, color, stroke_units, 0.0)
}

/// 旋转图标（转圈动画：调用方按节拍递增 rotation）。
pub fn icon_spin<Message: 'static>(
    name: &str,
    size: f32,
    color: Color,
    stroke_units: f32,
    rotation: f32,
) -> Element<'static, Message> {
    Canvas::new(Icon { name: icon_name(name), color, stroke_units, rotation })
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .into()
}