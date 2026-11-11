//! 图标形状表：24×24 视框内按 2 单位描边手写的几何，按用途分组。
//!
//! 坐标落在 24 格内、图形内缩 2 单位（描边后不触边）；端点圆角与连接由绘制端给定。

mod edit;
mod file;
mod layout;
mod nav;
mod status;

/// 一个图标的组成图元（描边绘制，不填充）。
#[derive(Clone, Copy)]
pub enum Shape {
    /// 直线段。
    Line([f32; 4]),
    /// 折线：按 `points` 顺序连线，`closed` 时末点回连首点。
    Polyline { points: &'static [[f32; 2]], closed: bool },
    /// 整圆。
    Circle { cx: f32, cy: f32, r: f32 },
    /// 圆弧：角度按度计（0 = 右、90 = 下），从 `from` 扫到 `to`。
    Arc { cx: f32, cy: f32, r: f32, from: f32, to: f32 },
    /// 圆角矩形（`radius` 统一四角）。
    Rect { x: f32, y: f32, w: f32, h: f32, radius: f32 },
}

impl Shape {
    /// 直线段。
    pub const fn line(x1: f32, y1: f32, x2: f32, y2: f32) -> Self {
        Self::Line([x1, y1, x2, y2])
    }

    /// 开放折线。
    pub const fn poly(points: &'static [[f32; 2]]) -> Self {
        Self::Polyline { points, closed: false }
    }

    /// 闭合折线（多边形轮廓）。
    pub const fn area(points: &'static [[f32; 2]]) -> Self {
        Self::Polyline { points, closed: true }
    }

    /// 整圆。
    pub const fn circle(cx: f32, cy: f32, r: f32) -> Self {
        Self::Circle { cx, cy, r }
    }

    /// 圆弧（度；0 = 右、90 = 下，按 from→to 角度扫）。
    pub const fn arc(cx: f32, cy: f32, r: f32, from: f32, to: f32) -> Self {
        Self::Arc { cx, cy, r, from, to }
    }

    /// 圆角矩形（`radius` = 0 即直角）。
    pub const fn rect(x: f32, y: f32, w: f32, h: f32, radius: f32) -> Self {
        Self::Rect { x, y, w, h, radius }
    }
}

/// 分组表拼成的全集。
fn groups() -> [&'static [(&'static str, &'static [Shape])]; 5] {
    [nav::ICONS, layout::ICONS, file::ICONS, edit::ICONS, status::ICONS]
}

/// 全部可用图标名（kebab-case）。
pub fn names() -> impl Iterator<Item = &'static str> {
    groups().into_iter().flat_map(|group| group.iter().map(|(name, _)| *name))
}

/// 图标名 -> 形状数据；未登记的名为 `None`。
pub fn icon(name: &str) -> Option<&'static [Shape]> {
    groups()
        .into_iter()
        .find_map(|group| group.iter().find(|(n, _)| *n == name).map(|(_, shapes)| *shapes))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 名字唯一：重复名会让后一组的同名片静默失效。
    #[test]
    fn names_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for name in names() {
            assert!(seen.insert(name), "重复的图标名：{name}");
        }
    }

    /// 每个名字都有几何，点落在 24 视框的容差内，折线至少两点。
    #[test]
    fn every_name_has_geometry_inside_viewbox() {
        for name in names() {
            let shapes = icon(name).unwrap_or_else(|| panic!("{name} 没有形状"));
            assert!(!shapes.is_empty(), "{name} 形状为空");
            for (index, shape) in shapes.iter().enumerate() {
                for point in points(shape) {
                    assert!(
                        (-1.0..=25.0).contains(&point[0]) && (-1.0..=25.0).contains(&point[1]),
                        "{name} 第 {index} 个图元超出视框：{point:?}"
                    );
                }
                if let Shape::Polyline { points, .. } = shape {
                    assert!(points.len() >= 2, "{name} 第 {index} 个折线少于两点");
                }
            }
        }
    }

    /// 圆角矩形的圆角不超过半边长（否则绘制端钳制，形状与表里写的不同）。
    #[test]
    fn rect_radius_fits_shape() {
        for name in names() {
            for shape in icon(name).unwrap_or_default() {
                if let Shape::Rect { w, h, radius, .. } = *shape {
                    assert!(radius <= w / 2.0 && radius <= h / 2.0, "{name} 圆角过大");
                }
            }
        }
    }

    fn points(shape: &Shape) -> Vec<[f32; 2]> {
        match *shape {
            Shape::Line([x1, y1, x2, y2]) => vec![[x1, y1], [x2, y2]],
            Shape::Polyline { points, .. } => points.to_vec(),
            Shape::Circle { cx, cy, r } => vec![[cx - r, cy - r], [cx + r, cy + r]],
            // 弧只取扫过的点：整圆包围盒会把只占上缘的弧误判成越界
            Shape::Arc { cx, cy, r, from, to } => (0..=16)
                .map(|i| {
                    let a = (from + (to - from) * (i as f32 / 16.0)).to_radians();
                    [cx + r * a.cos(), cy + r * a.sin()]
                })
                .collect(),
            Shape::Rect { x, y, w, h, .. } => vec![[x, y], [x + w, y + h]],
        }
    }
}
