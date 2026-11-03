//! 图标钮用的内置图标：按 24x24 网格定义描边形状，绘制时缩放到目标矩形。
//! 只收插件界面常用的几枚——要用别的形状时插件可以在自绘里直接写指令。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use atelyx_plugin_protocol::paint::{PaintColor, PaintPoint, DrawCommand};

use super::build::BuildCtx;
use super::geom::Rect;
use super::{lucide, svgpath};

/// 内置图标（24x24 网格）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Copy,
    Refresh,
    Eraser,
    Download,
    Search,
    HardDrive,
    Wrench,
    FileText,
    Box,
    ChevronLeft,
    ChevronRight,
    X,
    Trash,
    Minus,
    Square,
    Plus,
    Clock,
}

/// 图标线宽（24x24 网格下的描边宽度）。
const STROKE: f32 = 2.0;

pub(crate) fn draw(ctx: &mut BuildCtx<'_>, icon: Icon, rect: Rect, color: PaintColor) {
    let scale = rect.w / 24.0;
    let width = (STROKE * scale).max(1.0);
    let point = |x: f32, y: f32| PaintPoint { x: rect.x + x * scale, y: rect.y + y * scale };
    let line = |ctx: &mut BuildCtx<'_>, x1: f32, y1: f32, x2: f32, y2: f32| {
        ctx.draw(DrawCommand::Line {
            x1: rect.x + x1 * scale,
            y1: rect.y + y1 * scale,
            x2: rect.x + x2 * scale,
            y2: rect.y + y2 * scale,
            stroke: color,
            width,
            dash: None,
        });
    };
    let poly = |ctx: &mut BuildCtx<'_>, points: &[(f32, f32)], closed: bool| {
        ctx.draw(DrawCommand::Path {
            points: points.iter().map(|(x, y)| point(*x, *y)).collect(),
            closed,
            fill: None,
            stroke: Some(color),
            stroke_width: Some(width),
        });
    };
    match icon {
        // 两层叠放的方框：后一层只画左上折角
        Icon::Copy => {
            ctx.draw(DrawCommand::Rect {
                x: rect.x + 8.0 * scale,
                y: rect.y + 8.0 * scale,
                w: 13.0 * scale,
                h: 13.0 * scale,
                fill: None,
                stroke: Some(color),
                stroke_width: Some(width),
                corner_radius: Some(2.0 * scale),
                shadow: None,
            });
            poly(ctx, &[(4.0, 16.0), (4.0, 4.0), (16.0, 4.0)], false);
        }
        // 圆弧箭头：断口在右上方
        Icon::Refresh => {
            let mut points = Vec::new();
            let mut angle = -50.0_f32;
            while angle <= 250.0 {
                let radians = angle.to_radians();
                points.push((12.0 + 8.5 * radians.cos(), 12.0 + 8.5 * radians.sin()));
                angle += 15.0;
            }
            poly(ctx, &points, false);
            poly(ctx, &[(16.5, 2.0), (20.5, 6.0), (16.5, 10.0)], false);
        }
        // 斜向橡皮：一个斜置方框加一条底边
        Icon::Eraser => {
            poly(ctx, &[(14.0, 4.0), (20.0, 10.0), (12.0, 18.0), (4.0, 18.0), (4.0, 14.0)], true);
            line(ctx, 4.0, 18.0, 12.0, 18.0);
        }
        Icon::Download => {
            line(ctx, 12.0, 3.0, 12.0, 15.0);
            poly(ctx, &[(7.0, 10.0), (12.0, 15.0), (17.0, 10.0)], false);
            line(ctx, 5.0, 21.0, 19.0, 21.0);
        }
        Icon::Search => {
            ctx.draw(DrawCommand::Circle {
                cx: rect.x + 11.0 * scale,
                cy: rect.y + 11.0 * scale,
                r: 7.0 * scale,
                fill: None,
                stroke: Some(color),
                stroke_width: Some(width),
            });
            line(ctx, 16.0, 16.0, 21.0, 21.0);
        }
        // 机箱：一个横置方框 + 指示灯与仓位线
        Icon::HardDrive => {
            ctx.draw(DrawCommand::Rect {
                x: rect.x + 3.0 * scale,
                y: rect.y + 7.0 * scale,
                w: 18.0 * scale,
                h: 10.0 * scale,
                fill: None,
                stroke: Some(color),
                stroke_width: Some(width),
                corner_radius: Some(2.0 * scale),
                shadow: None,
            });
            line(ctx, 3.0, 13.0, 21.0, 13.0);
            ctx.draw(DrawCommand::Circle {
                cx: rect.x + 7.0 * scale,
                cy: rect.y + 16.5 * scale,
                r: 0.9 * scale,
                fill: Some(color),
                stroke: None,
                stroke_width: None,
            });
            line(ctx, 11.0, 16.5, 17.0, 16.5);
        }
        // 扳手：柄 + 两端开口的圆头
        Icon::Wrench => {
            ctx.draw(DrawCommand::Circle {
                cx: rect.x + 17.0 * scale,
                cy: rect.y + 7.0 * scale,
                r: 3.5 * scale,
                fill: None,
                stroke: Some(color),
                stroke_width: Some(width),
            });
            ctx.draw(DrawCommand::Circle {
                cx: rect.x + 7.0 * scale,
                cy: rect.y + 17.0 * scale,
                r: 3.0 * scale,
                fill: None,
                stroke: Some(color),
                stroke_width: Some(width),
            });
            line(ctx, 9.7, 14.3, 14.3, 9.7);
        }
        // 文档：折角纸 + 两行文字
        Icon::FileText => {
            poly(ctx, &[(6.0, 3.0), (14.0, 3.0), (19.0, 8.0), (19.0, 21.0), (6.0, 21.0)], true);
            poly(ctx, &[(14.0, 3.0), (14.0, 8.0), (19.0, 8.0)], false);
            line(ctx, 9.0, 13.0, 16.0, 13.0);
            line(ctx, 9.0, 17.0, 16.0, 17.0);
        }
        // 立方体：顶面菱形 + 三条棱
        Icon::Box => {
            poly(ctx, &[(3.0, 7.0), (12.0, 3.0), (21.0, 7.0), (21.0, 17.0), (12.0, 21.0), (3.0, 17.0)], true);
            poly(ctx, &[(3.0, 7.0), (12.0, 11.0), (21.0, 7.0)], false);
            line(ctx, 12.0, 11.0, 12.0, 21.0);
        }
        Icon::ChevronLeft => poly(ctx, &[(15.0, 5.0), (8.0, 12.0), (15.0, 19.0)], false),
        Icon::ChevronRight => poly(ctx, &[(9.0, 5.0), (16.0, 12.0), (9.0, 19.0)], false),
        Icon::X => {
            line(ctx, 6.0, 6.0, 18.0, 18.0);
            line(ctx, 18.0, 6.0, 6.0, 18.0);
        }
        // 垃圾桶：桶盖横线 + 提手 + 桶身 + 两道竖线
        Icon::Trash => {
            line(ctx, 4.0, 7.0, 20.0, 7.0);
            line(ctx, 9.0, 7.0, 9.0, 4.0);
            line(ctx, 9.0, 4.0, 15.0, 4.0);
            line(ctx, 15.0, 4.0, 15.0, 7.0);
            poly(ctx, &[(6.0, 7.0), (7.0, 20.0), (17.0, 20.0), (18.0, 7.0)], false);
            line(ctx, 10.0, 11.0, 10.0, 16.0);
            line(ctx, 14.0, 11.0, 14.0, 16.0);
        }
        // 窗口控制（最小化横线 / 最大化方框）与建议图标（加号 / 历史时钟）
        Icon::Minus => line(ctx, 5.0, 12.0, 19.0, 12.0),
        Icon::Square => {
            ctx.draw(DrawCommand::Rect {
                x: rect.x + 5.0 * scale,
                y: rect.y + 5.0 * scale,
                w: 14.0 * scale,
                h: 14.0 * scale,
                fill: None,
                stroke: Some(color),
                stroke_width: Some(width),
                corner_radius: None,
                shadow: None,
            });
        }
        Icon::Plus => {
            line(ctx, 12.0, 5.0, 12.0, 19.0);
            line(ctx, 5.0, 12.0, 19.0, 12.0);
        }
        Icon::Clock => {
            ctx.draw(DrawCommand::Circle {
                cx: rect.x + 12.0 * scale,
                cy: rect.y + 12.0 * scale,
                r: 8.0 * scale,
                fill: None,
                stroke: Some(color),
                stroke_width: Some(width),
            });
            line(ctx, 12.0, 7.5, 12.0, 12.0);
            line(ctx, 12.0, 12.0, 16.0, 14.0);
        }
    }
}

enum CachedShape {
    Polyline(Vec<[f32; 2]>),
    Circle(f32, f32, f32),
    Rect(f32, f32, f32, f32, f32),
    Line([f32; 4]),
}

/// 图标路径展平结果按名称缓存：同一图标只解析一次，绘制期不再解析 path。
fn cached(name: &str) -> Option<Arc<Vec<CachedShape>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<Vec<CachedShape>>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(shapes) = cache.lock().unwrap().get(name) {
        return Some(Arc::clone(shapes));
    }
    let shapes = lucide::icon(name)?;
    let mut out = Vec::new();
    for shape in shapes {
        match shape {
            lucide::Shape::Path(data) => {
                for polyline in svgpath::flatten_path(data) {
                    if polyline.len() >= 2 {
                        out.push(CachedShape::Polyline(polyline));
                    }
                }
            }
            lucide::Shape::Circle(cx, cy, r) => out.push(CachedShape::Circle(*cx, *cy, *r)),
            lucide::Shape::Rect(x, y, w, h, radius) => {
                out.push(CachedShape::Rect(*x, *y, *w, *h, *radius));
            }
            lucide::Shape::Line(line) => out.push(CachedShape::Line(*line)),
        }
    }
    let out = Arc::new(out);
    cache.lock().unwrap().insert(name.to_string(), Arc::clone(&out));
    Some(out)
}

/// 按 lucide 名称绘制（24×24 视框、2 单位描边）：插件用字符串取图标，
/// 不必靠扩展枚举；表外的名称不画。
pub(crate) fn draw_named(ctx: &mut BuildCtx<'_>, name: &str, rect: Rect, color: PaintColor) {
    let Some(shapes) = cached(name) else {
        return;
    };
    let scale = rect.w / 24.0;
    let width = (STROKE * scale).max(1.0);
    let point = |x: f32, y: f32| PaintPoint { x: rect.x + x * scale, y: rect.y + y * scale };
    for shape in shapes.iter() {
        match shape {
            CachedShape::Polyline(polyline) => {
                ctx.draw(DrawCommand::Path {
                    points: polyline.iter().map(|[x, y]| point(*x, *y)).collect(),
                    closed: false,
                    fill: None,
                    stroke: Some(color),
                    stroke_width: Some(width),
                });
            }
            CachedShape::Circle(cx, cy, r) => {
                ctx.draw(DrawCommand::Circle {
                    cx: rect.x + cx * scale,
                    cy: rect.y + cy * scale,
                    r: r * scale,
                    fill: None,
                    stroke: Some(color),
                    stroke_width: Some(width),
                });
            }
            CachedShape::Rect(x, y, w, h, radius) => {
                ctx.draw(DrawCommand::Rect {
                    x: rect.x + x * scale,
                    y: rect.y + y * scale,
                    w: w * scale,
                    h: h * scale,
                    fill: None,
                    stroke: Some(color),
                    stroke_width: Some(width),
                    corner_radius: Some(radius * scale),
                    shadow: None,
                });
            }
            CachedShape::Line([x1, y1, x2, y2]) => {
                ctx.draw(DrawCommand::Line {
                    x1: rect.x + x1 * scale,
                    y1: rect.y + y1 * scale,
                    x2: rect.x + x2 * scale,
                    y2: rect.y + y2 * scale,
                    stroke: color,
                    width,
                    dash: None,
                });
            }
        }
    }
}
