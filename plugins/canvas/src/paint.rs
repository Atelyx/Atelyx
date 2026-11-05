//! 画布帧：世界层（网格 / 分组 / 连线 / 卡 / 小地图）与浮层（文本卡正文、对话卡消息与输入区）。
//! 世界层是单块自绘区，命中与拖拽由 `input` 按坐标判定；浮层承载需要控件能力的正文。

use std::sync::Arc;

use atelyx_format::canvas::geometry::{
    self, EdgeRole, display_size, edge_endpoints, edge_hidden, edge_role,
    hidden_by_collapsed,
};
use atelyx_format::canvas::{CanvasFile, CanvasFileNode};
use atelyx_plugin_protocol::paint::{
    ColorToken, DrawCommand::*, ImageFit, ImageSource, PaintColor, PaintPoint, TextWeight,
};
use atelyx_plugin_sdk::frame_ui::{
    BoxStyle, BoxWidth, Canvas, Edges, MentionCandidate, Rect as UiRect, TextRole, TextStyle, Ui,
    icon_commands,
};

use crate::model::{
    COMPACT_CARD_PX, CardChat, GRID_STEP, MINIMAP_H, MINIMAP_MARGIN, MINIMAP_W, RemotePeer, Session,
    TOOLBAR_H, card_head,
};

/// 组色板（六色）。
pub const GROUP_COLORS: [[u8; 3]; 6] = [
    [0x5F, 0xBF, 0x8B],
    [0xE0, 0xA9, 0x4E],
    [0xE8, 0x77, 0x6C],
    [0x64, 0xA8, 0xF0],
    [0xB0, 0x8C, 0xE0],
    [0x64, 0xC8, 0xC8],
];

/// 收起分组的边框虚线（屏幕像素）。
const GROUP_DASH: [f32; 2] = [6.0, 4.0];
/// 世界层文字字号固定为屏幕像素：缩放只改卡片几何，字号不跟着缩小到不可读。
const CARD_TITLE_SIZE: f32 = 12.0;
const CARD_ICON_SIZE: f32 = 12.0;
const GROUP_LABEL_SIZE: f32 = 13.0;
const PILL_SIZE: f32 = 11.0;
const PILL_H: f32 = 18.0;
const PEER_NAME_SIZE: f32 = 11.0;
const PEER_PILL_SIZE: f32 = 10.0;
/// 卡头世界高度：随缩放变化，字与图标固定为屏幕像素。
const CARD_HEAD_H: f32 = 30.0;
/// 低缩放卡片只画卡头与居中图标的图标边长（屏幕像素）。
const COMPACT_ICON_SIZE: f32 = 22.0;
/// 省略量算的宽度余量：量算按常规字重口径，粗体实绘偏宽。
const ELLIPSIZE_WIDTH_GUARD: f32 = 1.06;

/// 低缩放紧凑形态：屏幕宽装不下标题，或卡头高度已装不下固定字号（卡头高度随缩放缩小）。
pub(crate) fn compact_card(zoom: f32, world_w: f32) -> bool {
    world_w * zoom <= COMPACT_CARD_PX || CARD_HEAD_H * zoom < CARD_TITLE_SIZE
}

/// 单行省略：超出 `max_w` 时按字符二分截断并补省略号。世界层是整块自绘画布，
/// 指令集没有逐项裁剪，越界文字必须在这里收住。
fn ellipsize(canvas: &mut Canvas, text: &str, size: f32, max_w: f32) -> String {
    if text.is_empty() || max_w <= 0.0 {
        return String::new();
    }
    // 粗体实绘偏宽：量算放大一档，避免省略后仍贴到卡框边缘
    let measure_size = size * ELLIPSIZE_WIDTH_GUARD;
    let (full_w, _) = canvas.measure(text, measure_size);
    if full_w <= max_w {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = (lo + hi + 1) / 2;
        let mut candidate: String = chars[..mid].iter().collect();
        candidate.push('…');
        let (width, _) = canvas.measure(&candidate, measure_size);
        if width <= max_w {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    if lo == 0 {
        return String::new();
    }
    let mut out: String = chars[..lo].iter().collect();
    out.push('…');
    out
}

/// 对话卡头右侧控件占宽（屏幕像素，含与标题的间隙）：生成中胶囊或压缩钮。
fn header_reserve(canvas: &mut Canvas, s: &Snapshot, node: &CanvasFileNode) -> f32 {
    if node.node_type != "conversation" {
        return 0.0;
    }
    let streaming = s.cards.get(&node.id).is_some_and(|chat| chat.streaming);
    if streaming {
        let (tw, _) = canvas.measure("生成中", PILL_SIZE);
        tw + 18.0 + 10.0 * s.zoom
    } else {
        32.0 * s.zoom
    }
}

fn token(name: ColorToken) -> PaintColor {
    PaintColor::Token { name }
}

fn rgba(rgb: [u8; 3], alpha: u8) -> PaintColor {
    PaintColor::Rgba {
        rgba: [rgb[0], rgb[1], rgb[2], alpha],
    }
}

fn with_alpha(color: PaintColor, alpha: f32) -> PaintColor {
    match color {
        PaintColor::Rgba { rgba } => PaintColor::Rgba {
            rgba: [rgba[0], rgba[1], rgba[2], (alpha * 255.0) as u8],
        },
        PaintColor::Token { name } => {
            // 组色 / 主题色按 alpha 叠加时退化成该色低饱和底：主题 token 不在这里解析，
            // 直接用中性灰会丢语义，因此保留 token 名由宿主叠透明度。
            let _ = name;
            PaintColor::Rgba {
                rgba: [128, 128, 128, (alpha * 255.0) as u8],
            }
        }
    }
}

/// 一帧的只读快照：自绘闭包要求 `'static`，数据先克隆出来再画。
pub struct Snapshot {
    pub data: CanvasFile,
    pub zoom: f32,
    pub pan: (f32, f32),
    pub selection: Vec<String>,
    pub hover: Option<String>,
    pub link_to: Option<(f32, f32)>,
    pub marquee: Option<((f32, f32), (f32, f32))>,
    pub guides: Vec<geometry::Guide>,
    pub viewport: (f32, f32),
    pub refs: std::collections::HashMap<String, crate::model::RefContent>,
    pub cards: std::collections::HashMap<String, CardChat>,
    /// 只含当前画布上的对端（同一空间里对端可能开着别的画布）。
    pub remote: Vec<RemotePeer>,
    pub path: String,
    /// 本端持有的编辑锁（nodeId → 占锁时刻毫秒）与本端 peer id：只读判定与交互侧同一套规则。
    pub locks: std::collections::HashMap<String, i64>,
    pub self_id: Option<String>,
    pub rename: Option<(String, String)>,
    pub dirty: bool,
    pub save_error: Option<String>,
    pub toolbar_hover: Option<String>,
}

impl Snapshot {
    pub fn of(session: &Session) -> Self {
        let marquee = match (&session.view.drag, session.view.marquee_to) {
            (Some(drag), Some(to))
                if matches!(drag.kind, crate::model::DragKind::Marquee { .. }) =>
            {
                Some((drag.start, to))
            }
            _ => None,
        };
        Self {
            data: session.data.clone(),
            zoom: session.view.zoom,
            pan: session.view.pan,
            selection: session.view.selection.clone(),
            hover: session.view.hover.clone(),
            link_to: session.view.link_to,
            marquee,
            guides: session.view.guides.clone(),
            viewport: session.view.viewport,
            refs: session.view.refs.clone(),
            cards: session.view.card_chat.clone(),
            remote: session
                .view
                .remote
                .iter()
                .filter(|peer| peer.path == session.file)
                .cloned()
                .collect(),
            path: session.file.clone(),
            locks: session.view.locks.clone(),
            self_id: session.view.self_id.clone(),
            rename: session.view.group_rename.clone(),
            dirty: session.dirty,
            save_error: session.save_error.clone(),
            toolbar_hover: session.view.toolbar_hover.clone(),
        }
    }

    fn view(&self) -> geometry::View {
        geometry::View {
            zoom: self.zoom,
            pan: self.pan,
        }
    }

    fn screen(&self, x: f64, y: f64) -> (f32, f32) {
        let (sx, sy) = self.view().screen(x, y);
        (sx, sy + TOOLBAR_H)
    }

    fn node(&self, id: &str) -> Option<&CanvasFileNode> {
        self.data.nodes.iter().find(|n| n.id == id)
    }

    /// 编辑锁的远端持有者：本端持有更早的锁时不显示他人「编辑中」。
    fn locked_by_peer(&self, node_id: &str) -> Option<&RemotePeer> {
        crate::model::lock_owner(
            self.locks.get(node_id).copied(),
            &self.remote,
            &self.path,
            self.self_id.as_deref(),
            node_id,
        )
    }
}

/// 构建整面帧：自绘世界层 + 每张卡正文浮层。
/// 一帧可选的控件候选（Agent / 模型清单）。
pub struct Choices<'a> {
    pub agents: &'a [atelyx_plugin_protocol::method::AiAgent],
    pub default_pick: Option<&'a atelyx_plugin_protocol::method::AiModelPick>,
}

/// 未打开画布：整页空态（与其它面板同一份居中排版）。
pub fn empty(ui: &mut Ui) {
    ui.empty_state("pencil", "打开画布", "在文件面板或搜索面板单击一个 .atlx 画布开始编辑。");
}

pub fn frame(ui: &mut Ui, session: &Session, size: (f32, f32), choices: Choices<'_>) {
    let snapshot = Arc::new(Snapshot::of(session));
    let (w, h) = size;
    let body_h = (h - TOOLBAR_H).max(0.0);
    let paint = snapshot.clone();
    ui.canvas_sized(w, h, move |canvas| {
        draw_world(canvas, &paint, w, h, body_h);
    });
    card_overlays(ui, &snapshot, &choices);
}

fn draw_world(canvas: &mut Canvas, s: &Snapshot, w: f32, h: f32, body_h: f32) {
    draw_grid(canvas, s, w, body_h);
    for node in s.data.nodes.iter().filter(|n| n.node_type == "group") {
        draw_group(canvas, s, node);
    }
    for edge in &s.data.edges {
        if edge_hidden(&s.data, edge) {
            continue;
        }
        let Some((a, b)) = edge_endpoints(&s.data, edge) else {
            continue;
        };
        draw_edge(canvas, s, edge, a, b);
    }
    for node in s.data.nodes.iter().filter(|n| n.node_type != "group") {
        if hidden_by_collapsed(&s.data.nodes, &node.id) {
            continue;
        }
        draw_card(canvas, s, node);
    }
    draw_remote(canvas, s);
    if let Some(((sx, sy), (ex, ey))) = s.marquee {
        canvas.draw(Rect {
            x: sx.min(ex),
            y: sy.min(ey),
            w: ((ex - sx).abs()).max(0.0),
            h: ((ey - sy).abs()).max(0.0),
            fill: Some(accent_alpha(0.08)),
            stroke: Some(token(ColorToken::Accent)),
            stroke_width: Some(1.0),
            corner_radius: None,
            shadow: None,
        });
    }
    for guide in &s.guides {
        if guide.vertical {
            let (x, _) = s.screen(guide.at, 0.0);
            canvas.draw(Line {
                x1: x,
                y1: TOOLBAR_H,
                x2: x,
                y2: body_h,
                stroke: token(ColorToken::Accent),
                width: 1.0,
                dash: Some([4.0, 3.0]),
            });
        } else {
            let (_, y) = s.screen(0.0, guide.at);
            canvas.draw(Line {
                x1: 0.0,
                y1: y,
                x2: w,
                y2: y,
                stroke: token(ColorToken::Accent),
                width: 1.0,
                dash: Some([4.0, 3.0]),
            });
        }
    }
    if let (Some(from), Some(to)) = (link_source(s), s.link_to) {
        let (sx, sy) = s.screen(from.0, from.1);
        canvas.draw(Line {
            x1: sx,
            y1: sy,
            x2: to.0,
            y2: to.1 + TOOLBAR_H,
            stroke: token(ColorToken::Accent),
            width: 1.6,
            dash: Some([4.0, 3.0]),
        });
    }
    draw_minimap(canvas, s);
    draw_toolbar(canvas, s, w, h);
}

fn link_source(s: &Snapshot) -> Option<(f64, f64)> {
    if s.link_to.is_none() {
        return None;
    }
    let id = s.selection.first()?;
    let node = s.node(id)?;
    let (w, h) = display_size(node);
    Some((node.x + w, node.y + h / 2.0))
}

fn accent_alpha(alpha: f32) -> PaintColor {
    PaintColor::Rgba {
        rgba: [59, 130, 246, (alpha * 255.0) as u8],
    }
}

fn draw_grid(canvas: &mut Canvas, s: &Snapshot, w: f32, h: f32) {
    let step = GRID_STEP as f32 * s.zoom;
    if step < 12.0 {
        return;
    }
    let mut x = s.pan.0.rem_euclid(step);
    while x < w {
        canvas.draw(Line {
            x1: x,
            y1: TOOLBAR_H,
            x2: x,
            y2: h,
            stroke: token(ColorToken::BorderSubtle),
            width: 1.0,
            dash: None,
        });
        x += step;
    }
    let mut y = TOOLBAR_H + s.pan.1.rem_euclid(step);
    while y < h {
        canvas.draw(Line {
            x1: 0.0,
            y1: y,
            x2: w,
            y2: y,
            stroke: token(ColorToken::BorderSubtle),
            width: 1.0,
            dash: None,
        });
        y += step;
    }
}

fn group_color(node: &CanvasFileNode) -> PaintColor {
    let index = node
        .data
        .get("color")
        .and_then(serde_json::Value::as_str)
        .and_then(|text| text.parse::<usize>().ok())
        .filter(|index| (1..=6).contains(index));
    match index {
        Some(index) => rgba(GROUP_COLORS[index - 1], 255),
        None => token(ColorToken::TextMuted),
    }
}

fn draw_group(canvas: &mut Canvas, s: &Snapshot, node: &CanvasFileNode) {
    let (w, h) = display_size(node);
    let (sx, sy) = s.screen(node.x, node.y);
    let (sw, sh) = (w as f32 * s.zoom, h as f32 * s.zoom);
    let color = group_color(node);
    let collapsed = geometry::group_collapsed(node);
    let selected = s.selection.iter().any(|id| id == &node.id);
    canvas.draw(Rect {
        x: sx,
        y: sy,
        w: (sw).max(0.0),
        h: (sh).max(0.0),
        fill: Some(with_alpha(color, if collapsed { 0.25 } else { 0.12 })),
        stroke: if collapsed {
            None
        } else {
            Some(if selected {
                token(ColorToken::Accent)
            } else {
                color
            })
        },
        stroke_width: Some(if selected { 2.0 } else { 1.5 }),
        corner_radius: Some(14.0 * s.zoom),
        shadow: None,
    });
    if collapsed {
        // 收起组只留虚线边框与居中标题：仍占位、仍可点开
        for (x1, y1, x2, y2) in [
            (sx, sy, sx + sw, sy),
            (sx + sw, sy, sx + sw, sy + sh),
            (sx + sw, sy + sh, sx, sy + sh),
            (sx, sy + sh, sx, sy),
        ] {
            canvas.draw(Line {
                x1,
                y1,
                x2,
                y2,
                stroke: color,
                width: if selected { 2.0 } else { 1.5 },
                dash: Some(GROUP_DASH),
            });
        }
        if sw >= 40.0 && sh >= GROUP_LABEL_SIZE {
            let (text, text_color) = group_label(s, node);
            let text = ellipsize(canvas, &text, GROUP_LABEL_SIZE, sw - 16.0 * s.zoom);
            let (tw, _) = canvas.measure(&text, GROUP_LABEL_SIZE);
            canvas.draw(Text {
                x: sx + ((sw - tw) / 2.0).max(0.0),
                y: sy + ((sh - GROUP_LABEL_SIZE) / 2.0).max(0.0),
                content: text,
                size: GROUP_LABEL_SIZE,
                color: text_color,
                weight: Some(TextWeight::Bold),
                family: None,
                italic: false,
                family_name: None,
                max_width: None,
                align: None,
                layout: None,
            });
        }
        return;
    }
    canvas.draw(Rect {
        x: sx,
        y: sy,
        w: (sw).max(0.0),
        h: (30.0 * s.zoom).max(0.0),
        fill: Some(with_alpha(color, 0.25)),
        stroke: None,
        stroke_width: None,
        corner_radius: Some(14.0 * s.zoom),
        shadow: None,
    });
    let (text, color) = group_label(s, node);
    if 30.0 * s.zoom >= GROUP_LABEL_SIZE {
        let text = ellipsize(canvas, &text, GROUP_LABEL_SIZE, sw - 20.0 * s.zoom);
        canvas.draw(Text {
            x: sx + 10.0 * s.zoom,
            y: sy + (30.0 * s.zoom - GROUP_LABEL_SIZE) / 2.0,
            content: text,
            size: GROUP_LABEL_SIZE,
            color,
            weight: Some(TextWeight::Bold),
            family: None,
            italic: false,
            family_name: None,
            max_width: None,
            align: None,
            layout: None,
        });
    }
}

/// 分组标题：改名草稿优先，其次节点标签，缺省「分组」。
fn group_label(s: &Snapshot, node: &CanvasFileNode) -> (String, PaintColor) {
    let label = s
        .rename
        .as_ref()
        .filter(|(id, _)| id == &node.id)
        .map(|(_, draft)| draft.clone())
        .or_else(|| {
            node.data
                .get("label")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        });
    match label.as_deref() {
        Some(label) if !label.is_empty() => (label.to_string(), token(ColorToken::Text)),
        _ => ("分组".to_string(), token(ColorToken::TextMuted)),
    }
}

fn draw_edge(
    canvas: &mut Canvas,
    s: &Snapshot,
    edge: &atelyx_format::canvas::CanvasFileEdge,
    a: (f64, f64),
    b: (f64, f64),
) {
    let (sx, sy) = s.screen(a.0, a.1);
    let (ex, ey) = s.screen(b.0, b.1);
    let (color, width, dash) = match edge_role(&s.data, edge) {
        EdgeRole::Association => (token(ColorToken::BorderStrong), 1.2, None),
        EdgeRole::Reference { consumed: false } => {
            (token(ColorToken::Accent), 1.4, Some([3.0, 4.0]))
        }
        EdgeRole::Reference { consumed: true } => (token(ColorToken::Accent), 1.8, None),
        EdgeRole::Lineage => (token(ColorToken::Accent), 1.6, None),
        EdgeRole::Flow => (token(ColorToken::Accent), 1.4, None),
    };
    let out = ((ex - sx).hypot(ey - sy) / 2.0).min(90.0 * s.zoom);
    let (ax, ay) = side_normal(edge.source_handle.as_deref(), (sx, sy), (ex, ey), out);
    let (bx, by) = side_normal(edge.target_handle.as_deref(), (ex, ey), (sx, sy), out);
    let points = bezier((sx, sy), (ax, ay), (bx, by), (ex, ey), 28);
    if let Some(dash) = dash {
        dashed(canvas, &points, color, width, dash);
    } else {
        canvas.draw(Path {
            points: points
                .iter()
                .map(|(x, y)| PaintPoint { x: *x, y: *y })
                .collect(),
            closed: false,
            fill: None,
            stroke: Some(color),
            stroke_width: Some(width),
        });
    }
    // 数据流一个箭头指向目标；关联按 linkMode 单 / 双头；其余（引用 / 血缘）不画箭头
    let arrows = if geometry::is_dataflow(edge) {
        1
    } else if matches!(edge_role(&s.data, edge), EdgeRole::Association) {
        match edge.link_mode.as_deref() {
            Some("double" | "both") => 2,
            Some("single") => 1,
            _ => 0,
        }
    } else {
        0
    };
    if arrows >= 1 {
        arrow(
            canvas,
            points[points.len() - 2],
            points[points.len() - 1],
            color,
        );
    }
    if arrows == 2 {
        arrow(canvas, points[1], points[0], color);
    }
}

fn side_normal(handle: Option<&str>, from: (f32, f32), to: (f32, f32), out: f32) -> (f32, f32) {
    let side = handle.and_then(|handle| {
        if handle.starts_with("left") {
            Some(0)
        } else if handle.starts_with("right") {
            Some(1)
        } else if handle.starts_with("top") {
            Some(2)
        } else if handle.starts_with("bottom") {
            Some(3)
        } else {
            None
        }
    });
    match side.unwrap_or_else(|| {
        let (dx, dy) = (to.0 - from.0, to.1 - from.1);
        if dx.abs() >= dy.abs() {
            if dx >= 0.0 { 1 } else { 0 }
        } else if dy >= 0.0 {
            3
        } else {
            2
        }
    }) {
        0 => (from.0 - out, from.1),
        1 => (from.0 + out, from.1),
        2 => (from.0, from.1 - out),
        _ => (from.0, from.1 + out),
    }
}

fn bezier(
    a: (f32, f32),
    c1: (f32, f32),
    c2: (f32, f32),
    b: (f32, f32),
    n: usize,
) -> Vec<(f32, f32)> {
    (0..=n)
        .map(|i| {
            let t = i as f32 / n as f32;
            let u = 1.0 - t;
            let x =
                u * u * u * a.0 + 3.0 * u * u * t * c1.0 + 3.0 * u * t * t * c2.0 + t * t * t * b.0;
            let y =
                u * u * u * a.1 + 3.0 * u * u * t * c1.1 + 3.0 * u * t * t * c2.1 + t * t * t * b.1;
            (x, y)
        })
        .collect()
}

fn dashed(
    canvas: &mut Canvas,
    points: &[(f32, f32)],
    color: PaintColor,
    width: f32,
    dash: [f32; 2],
) {
    let (on, off) = (dash[0], dash[1]);
    let mut remaining = on;
    let mut drawing = true;
    for pair in points.windows(2) {
        let (mut x1, mut y1) = pair[0];
        let (x2, y2) = pair[1];
        let mut length = (x2 - x1).hypot(y2 - y1);
        while length > 0.0 {
            let step = remaining.min(length);
            let ratio = step / length;
            let nx = x1 + (x2 - x1) * ratio;
            let ny = y1 + (y2 - y1) * ratio;
            if drawing {
                canvas.draw(Line {
                    x1,
                    y1,
                    x2: nx,
                    y2: ny,
                    stroke: color,
                    width,
                    dash: None,
                });
            }
            x1 = nx;
            y1 = ny;
            length -= step;
            remaining -= step;
            if remaining <= 0.0 {
                drawing = !drawing;
                remaining = if drawing { on } else { off };
            }
        }
    }
}

fn arrow(canvas: &mut Canvas, from: (f32, f32), tip: (f32, f32), color: PaintColor) {
    let angle = (tip.1 - from.1).atan2(tip.0 - from.0);
    let (len, half) = (9.0, 4.0);
    let left = (
        tip.0 - len * angle.cos() + half * angle.sin(),
        tip.1 - len * angle.sin() - half * angle.cos(),
    );
    let right = (
        tip.0 - len * angle.cos() - half * angle.sin(),
        tip.1 - len * angle.sin() + half * angle.cos(),
    );
    for end in [left, right] {
        canvas.draw(Line {
            x1: tip.0,
            y1: tip.1,
            x2: end.0,
            y2: end.1,
            stroke: color,
            width: 1.5,
            dash: None,
        });
    }
}

fn draw_card(canvas: &mut Canvas, s: &Snapshot, node: &CanvasFileNode) {
    let (w, h) = display_size(node);
    let (sx, sy) = s.screen(node.x, node.y);
    let (sw, sh) = (w as f32 * s.zoom, h as f32 * s.zoom);
    let selected = s.selection.iter().any(|id| id == &node.id);
    canvas.draw(Rect {
        x: sx,
        y: sy,
        w: (sw).max(0.0),
        h: (sh).max(0.0),
        fill: Some(token(ColorToken::SurfaceMuted)),
        stroke: Some(if selected {
            token(ColorToken::Accent)
        } else {
            token(ColorToken::Border)
        }),
        stroke_width: Some(if selected { 2.0 } else { 1.0 }),
        corner_radius: Some(10.0 * s.zoom),
        shadow: None,
    });
    let head = CARD_HEAD_H * s.zoom;
    canvas.draw(Rect {
        x: sx,
        y: sy,
        w: (sw).max(0.0),
        h: (head).max(0.0),
        fill: Some(token(ColorToken::SurfaceSunken)),
        stroke: None,
        stroke_width: None,
        corner_radius: Some(10.0 * s.zoom),
        shadow: None,
    });
    canvas.draw(Rect {
        x: sx,
        y: sy + head - 6.0 * s.zoom,
        w: (sw).max(0.0),
        h: (6.0 * s.zoom).max(0.0),
        fill: Some(token(ColorToken::SurfaceSunken)),
        stroke: None,
        stroke_width: None,
        corner_radius: None,
        shadow: None,
    });
    let (icon, title) = card_head(node);
    let compact = compact_card(s.zoom, w as f32);
    if compact {
        // 低缩放只留卡头与居中图标：正文预览在这个尺寸读不出来，字号也不再跟着缩小
        let (cx, cy) = (sx + sw / 2.0, sy + sh / 2.0);
        for command in icon_commands(
            icon,
            UiRect::new(
                cx - COMPACT_ICON_SIZE / 2.0,
                cy - COMPACT_ICON_SIZE / 2.0,
                COMPACT_ICON_SIZE,
                COMPACT_ICON_SIZE,
            ),
            token(ColorToken::TextMuted),
        ) {
            canvas.draw(command);
        }
    } else {
        for command in icon_commands(
            icon,
            UiRect::new(
                sx + 9.0 * s.zoom,
                sy + (head - CARD_ICON_SIZE) / 2.0,
                CARD_ICON_SIZE,
                CARD_ICON_SIZE,
            ),
            token(ColorToken::TextMuted),
        ) {
            canvas.draw(command);
        }
        // 标题省略到卡头右侧控件之前：世界层不裁剪，越界会盖到按钮或溢出卡框
        let title_limit = (sw - 36.0 * s.zoom - header_reserve(canvas, s, node)).max(0.0);
        let title = ellipsize(canvas, &title, CARD_TITLE_SIZE, title_limit);
        canvas.draw(Text {
            x: sx + 26.0 * s.zoom,
            y: sy + (head - CARD_TITLE_SIZE) / 2.0,
            content: title,
            size: CARD_TITLE_SIZE,
            color: token(ColorToken::Text),
            weight: Some(TextWeight::Bold),
            family: None,
            italic: false,
            family_name: None,
            max_width: None,
            align: None,
            layout: None,
        });
    }
    let linked = s.hover.as_deref() == Some(node.id.as_str()) || selected;
    if linked && !matches!(node.node_type.as_str(), "group" | "conversation") {
        canvas.draw(Circle {
            cx: sx + sw,
            cy: sy + sh / 2.0,
            r: 4.0 * s.zoom,
            fill: Some(token(ColorToken::Accent)),
            stroke: None,
            stroke_width: None,
        });
    }
    if selected && s.selection.len() == 1 {
        canvas.draw(Rect {
            x: sx + sw - 4.5 * s.zoom,
            y: sy + sh - 4.5 * s.zoom,
            w: (9.0 * s.zoom).max(0.0),
            h: (9.0 * s.zoom).max(0.0),
            fill: Some(token(ColorToken::Accent)),
            stroke: None,
            stroke_width: None,
            corner_radius: Some(1.0),
            shadow: None,
        });
    }
    if !compact
        && head >= PILL_H
        && node.node_type == "conversation"
        && s.cards.get(&node.id).is_some_and(|chat| chat.streaming)
    {
        let label = "生成中";
        let (tw, _) = canvas.measure(label, PILL_SIZE);
        let pill_w = tw + 18.0;
        let pill_x = sx + sw - pill_w - 6.0 * s.zoom;
        let pill_y = sy + (head - PILL_H) / 2.0;
        canvas.draw(Rect {
            x: pill_x,
            y: pill_y,
            w: (pill_w).max(0.0),
            h: PILL_H,
            fill: Some(token(ColorToken::SurfaceMuted)),
            stroke: None,
            stroke_width: None,
            corner_radius: Some(PILL_H / 2.0),
            shadow: None,
        });
        canvas.draw(Circle {
            cx: pill_x + 8.0,
            cy: pill_y + PILL_H / 2.0,
            r: 2.5,
            fill: Some(token(ColorToken::Accent)),
            stroke: None,
            stroke_width: None,
        });
        canvas.draw(Text {
            x: pill_x + 14.0,
            y: pill_y + 3.5,
            content: label.to_string(),
            size: PILL_SIZE,
            color: token(ColorToken::TextSecondary),
            weight: None,
            family: None,
            italic: false,
            family_name: None,
            max_width: None,
            align: None,
            layout: None,
        });
    }
    if node.node_type == "media" && !compact {
        draw_media(canvas, s, node);
    }
    if node.node_type == "link" && !compact {
        if let Some(url) = node.data.get("url").and_then(serde_json::Value::as_str) {
            let url = ellipsize(canvas, url, CARD_TITLE_SIZE, sw - 20.0 * s.zoom);
            canvas.draw(Text {
                x: sx + 10.0 * s.zoom,
                y: sy + head + 10.0 * s.zoom,
                content: url,
                size: CARD_TITLE_SIZE,
                color: token(ColorToken::Accent),
                weight: None,
                family: None,
                italic: false,
                family_name: None,
                max_width: None,
                align: None,
                layout: None,
            });
        }
    }
    if node.node_type == "table" && !compact {
        if let Some(path) = node.data.get("file").and_then(serde_json::Value::as_str) {
            // 没有可用的表格内容也要给提示，不留空白卡
            let (hint, color) = match s.refs.get(path) {
                Some(crate::model::RefContent::Table { summary, .. }) => (
                    format!("双击打开表格 · {summary}"),
                    ColorToken::TextSecondary,
                ),
                Some(crate::model::RefContent::Missing) => {
                    ("引用表格缺失".to_string(), ColorToken::Danger)
                }
                Some(_) => ("引用表格缺失".to_string(), ColorToken::Danger),
                None => ("加载中…".to_string(), ColorToken::TextMuted),
            };
            let text = ellipsize(canvas, &hint, CARD_TITLE_SIZE, sw - 20.0 * s.zoom);
            canvas.draw(Text {
                x: sx + 10.0 * s.zoom,
                y: sy + head + 10.0 * s.zoom,
                content: text,
                size: CARD_TITLE_SIZE,
                color: token(color),
                weight: None,
                family: None,
                italic: false,
                family_name: None,
                max_width: None,
                align: None,
                layout: None,
            });
        }
    }
}

fn draw_media(canvas: &mut Canvas, s: &Snapshot, node: &CanvasFileNode) {
    let Some(path) = node.data.get("file").and_then(serde_json::Value::as_str) else {
        return;
    };
    let (w, h) = display_size(node);
    let (sx, sy) = s.screen(node.x, node.y);
    // 图片还没解析 / 附件找不到时出提示，避免只剩一张空白卡
    let hint = match s.refs.get(path) {
        Some(crate::model::RefContent::Image) => None,
        Some(_) => Some(("附件缺失", ColorToken::Danger)),
        None => Some(("图片加载中…", ColorToken::TextMuted)),
    };
    if let Some((hint, color)) = hint {
        canvas.draw(Text {
            x: sx + 10.0 * s.zoom,
            y: sy + 40.0 * s.zoom + 10.0 * s.zoom,
            content: hint.to_string(),
            size: CARD_TITLE_SIZE,
            color: token(color),
            weight: None,
            family: None,
            italic: false,
            family_name: None,
            max_width: Some((w as f32 - 20.0) * s.zoom),
            align: None,
            layout: None,
        });
        return;
    }
    canvas.draw(Image {
        source: ImageSource::Vault {
            path: path.to_string(),
            max_edge: Some(1024),
        },
        x: sx + 10.0 * s.zoom,
        y: sy + 40.0 * s.zoom,
        w: (w as f32 - 20.0) * s.zoom,
        h: (h as f32 - 50.0) * s.zoom,
        fit: Some(ImageFit::Contain),
    });
}

fn draw_remote(canvas: &mut Canvas, s: &Snapshot) {
    for peer in &s.remote {
        let color = rgba([peer.color.0, peer.color.1, peer.color.2], 255);
        let mut tagged = false;
        for id in &peer.selected {
            let Some(node) = s.node(id) else { continue };
            let (w, h) = display_size(node);
            let (x, y) = s.screen(node.x, node.y);
            canvas.draw(Rect {
                x: x - 3.0,
                y: y - 3.0,
                w: (w as f32 * s.zoom + 6.0).max(0.0),
                h: (h as f32 * s.zoom + 6.0).max(0.0),
                fill: None,
                stroke: Some(color),
                stroke_width: Some(2.0),
                corner_radius: Some(12.0 * s.zoom),
                shadow: None,
            });
            let compact = compact_card(s.zoom, w as f32);
            if !compact && peer.streaming.iter().any(|item| item == id) {
                let label = "生成中";
                let (tw, _) = canvas.measure(label, PEER_PILL_SIZE);
                let (px, py) = (x + 6.0 * s.zoom, y + 6.0 * s.zoom);
                canvas.draw(Rect {
                    x: px,
                    y: py,
                    w: (tw + 12.0).max(0.0),
                    h: 16.0,
                    fill: Some(with_alpha(color, 0.9)),
                    stroke: None,
                    stroke_width: None,
                    corner_radius: Some(8.0),
                    shadow: None,
                });
                canvas.draw(Text {
                    x: px + 6.0,
                    y: py + 2.0,
                    content: label.to_string(),
                    size: PEER_PILL_SIZE,
                    color: PaintColor::Rgba {
                        rgba: [255, 255, 255, 235],
                    },
                    weight: None,
                    family: None,
                    italic: false,
                    family_name: None,
                    max_width: None,
                    align: None,
                    layout: None,
                });
            }
            if !compact && !tagged && !peer.name.is_empty() {
                let size = PEER_NAME_SIZE;
                // 名签不越出卡宽：名字可用宽度扣掉胶囊内边距与卡两侧留白
                let name = ellipsize(
                    canvas,
                    &peer.name,
                    size,
                    (w as f32 * s.zoom - 8.0 * s.zoom - 12.0).max(0.0),
                );
                if !name.is_empty() {
                    tagged = true;
                    let (tw, th) = canvas.measure(&name, size);
                    let (bx, by) = (x + 4.0 * s.zoom, (y - th - 4.0).max(TOOLBAR_H));
                    canvas.draw(Rect {
                        x: bx,
                        y: by,
                        w: (tw + 12.0).max(0.0),
                        h: (th + 4.0).max(0.0),
                        fill: Some(color),
                        stroke: None,
                        stroke_width: None,
                        corner_radius: Some((th + 4.0) / 2.0),
                        shadow: None,
                    });
                    canvas.draw(Text {
                        x: bx + 6.0,
                        y: by + 2.0,
                        content: name,
                        size,
                        color: PaintColor::Rgba {
                            rgba: [255, 255, 255, 235],
                        },
                        weight: None,
                        family: None,
                        italic: false,
                        family_name: None,
                        max_width: None,
                        align: None,
                        layout: None,
                    });
                }
            }
        }
    }
}

fn draw_minimap(canvas: &mut Canvas, s: &Snapshot) {
    let (mw, mx, my) = (
        MINIMAP_W,
        s.viewport.0 - MINIMAP_W - MINIMAP_MARGIN,
        s.viewport.1 - MINIMAP_H - MINIMAP_MARGIN,
    );
    canvas.draw(Rect {
        x: mx,
        y: my,
        w: (mw).max(0.0),
        h: (MINIMAP_H).max(0.0),
        fill: Some(token(ColorToken::SurfaceOverlay)),
        stroke: Some(token(ColorToken::Border)),
        stroke_width: Some(1.0),
        corner_radius: Some(6.0),
        shadow: None,
    });
    let Some((scale, ox, oy)) = crate::model::minimap_transform(&s.data.nodes) else {
        return;
    };
    for node in s.data.nodes.iter().filter(|n| n.node_type == "group") {
        let (w, h) = display_size(node);
        canvas.draw(Rect {
            x: (node.x - ox) as f32 * scale + mx,
            y: (node.y - oy) as f32 * scale + my,
            w: (w as f32 * scale).max(0.0),
            h: (h as f32 * scale).max(0.0),
            fill: Some(with_alpha(group_color(node), 0.45)),
            stroke: None,
            stroke_width: None,
            corner_radius: Some(3.0),
            shadow: None,
        });
    }
    for node in s.data.nodes.iter().filter(|n| n.node_type != "group") {
        if hidden_by_collapsed(&s.data.nodes, &node.id) {
            continue;
        }
        let (w, h) = display_size(node);
        let selected = s.selection.iter().any(|id| id == &node.id);
        canvas.draw(Rect {
            x: (node.x - ox) as f32 * scale + mx,
            y: (node.y - oy) as f32 * scale + my,
            w: ((w as f32 * scale).max(1.0)).max(0.0),
            h: ((h as f32 * scale).max(1.0)).max(0.0),
            fill: None,
            stroke: Some(if selected {
                token(ColorToken::Accent)
            } else {
                token(ColorToken::BorderStrong)
            }),
            stroke_width: Some(1.0),
            corner_radius: None,
            shadow: None,
        });
    }
    let world_x = -s.pan.0 as f64 / s.zoom as f64;
    let world_y = -s.pan.1 as f64 / s.zoom as f64;
    let (vw, vh) = s.viewport;
    canvas.draw(Rect {
        x: (world_x - ox) as f32 * scale + mx,
        y: (world_y - oy) as f32 * scale + my,
        w: ((vw / s.zoom) as f32 * scale).max(0.0),
        h: (((vh - TOOLBAR_H) / s.zoom) as f32 * scale).max(0.0),
        fill: None,
        stroke: Some(token(ColorToken::Accent)),
        stroke_width: Some(1.5),
        corner_radius: None,
        shadow: None,
    });
}

fn draw_toolbar(canvas: &mut Canvas, s: &Snapshot, w: f32, _h: f32) {
    canvas.draw(Line {
        x1: 0.0,
        y1: TOOLBAR_H,
        x2: w,
        y2: TOOLBAR_H,
        stroke: token(ColorToken::Border),
        width: 1.0,
        dash: None,
    });
    let mut left = 12.0;
    if s.dirty {
        let (tw, _) = canvas.measure("未保存", 11.0);
        canvas.draw(Rect {
            x: left,
            y: 10.0,
            w: (tw + 14.0).max(0.0),
            h: 16.0_f32.max(0.0),
            fill: Some(PaintColor::Rgba {
                rgba: [230, 192, 60, 40],
            }),
            stroke: None,
            stroke_width: None,
            corner_radius: Some(8.0),
            shadow: None,
        });
        canvas.draw(Text {
            x: left + 7.0,
            y: 11.5,
            content: "未保存".to_string(),
            size: 11.0,
            color: token(ColorToken::TextSecondary),
            weight: None,
            family: None,
            italic: false,
            family_name: None,
            max_width: None,
            align: None,
            layout: None,
        });
        left += tw + 22.0;
    }
    if let Some(error) = &s.save_error {
        // 工具条只留 220px 单行省略；悬停该区域时在工具条下方给出全文
        let short = ellipsize(canvas, error, 11.0, 220.0);
        canvas.draw(Text {
            x: left,
            y: 11.5,
            content: short,
            size: 11.0,
            color: token(ColorToken::Danger),
            weight: None,
            family: None,
            italic: false,
            family_name: None,
            max_width: Some(220.0),
            align: None,
            layout: None,
        });
        if s.toolbar_hover.as_deref() == Some("save-error") {
            let max_w = (w - 24.0).max(120.0);
            let full = error.clone();
            let (text_w, _) = canvas.measure(&full, 11.0);
            let box_w = (text_w + 16.0).min(max_w);
            canvas.draw(Rect {
                x: 12.0,
                y: TOOLBAR_H + 4.0,
                w: box_w,
                h: 22.0,
                fill: Some(token(ColorToken::SurfaceMuted)),
                stroke: Some(token(ColorToken::Border)),
                stroke_width: Some(1.0),
                corner_radius: Some(5.0),
                shadow: None,
            });
            canvas.draw(Text {
                x: 20.0,
                y: TOOLBAR_H + 10.0,
                content: full,
                size: 11.0,
                color: token(ColorToken::Danger),
                weight: None,
                family: None,
                italic: false,
                family_name: None,
                max_width: Some(box_w - 16.0),
                align: None,
                layout: None,
            });
        }
    }
    let buttons = crate::model::zoom_buttons(w);
    let left_edge = buttons
        .iter()
        .map(|(_, (x, _, _, _))| *x)
        .fold(w, f32::min);
    let zoom_text = format!("{}%", (s.zoom * 100.0).round() as i32);
    let (tw, _) = canvas.measure(&zoom_text, 12.0);
    canvas.draw(Text {
        x: left_edge - tw - 8.0,
        y: 9.0,
        content: zoom_text,
        size: 12.0,
        color: token(ColorToken::TextSecondary),
        weight: None,
        family: None,
        italic: false,
        family_name: None,
        max_width: None,
        align: None,
        layout: None,
    });
    for (name, (bx, by, bw, bh)) in buttons {
        let hovered = s.toolbar_hover.as_deref() == Some(name);
        if hovered {
            canvas.draw(Rect {
                x: bx,
                y: by,
                w: bw,
                h: bh,
                fill: Some(token(ColorToken::SurfaceMuted)),
                stroke: None,
                stroke_width: None,
                corner_radius: Some(5.0),
                shadow: None,
            });
        }
        let icon = match name {
            "zoom-in" => "plus",
            "zoom-out" => "minus",
            _ => "maximize",
        };
        for command in icon_commands(
            icon,
            UiRect::new(bx + 4.0, by + 4.0, 16.0, 16.0),
            token(ColorToken::TextSecondary),
        ) {
            canvas.draw(command);
        }
    }
    if let Some((name, (bx, by, bw, _))) = buttons
        .iter()
        .find(|(name, _)| s.toolbar_hover.as_deref() == Some(*name))
    {
        let label = match *name {
            "zoom-in" => "放大",
            "zoom-out" => "缩小",
            _ => "适应视图",
        };
        let (tw, _) = canvas.measure(label, 11.0);
        let tx = (bx + bw / 2.0 - tw / 2.0).clamp(4.0, (w - tw - 4.0).max(4.0));
        canvas.draw(Rect {
            x: tx - 6.0,
            y: by + 28.0,
            w: (tw + 12.0).max(0.0),
            h: 20.0,
            fill: Some(token(ColorToken::SurfaceMuted)),
            stroke: Some(token(ColorToken::Border)),
            stroke_width: Some(1.0),
            corner_radius: Some(4.0),
            shadow: None,
        });
        canvas.draw(Text {
            x: tx,
            y: by + 31.5,
            content: label.to_string(),
            size: 11.0,
            color: token(ColorToken::TextSecondary),
            weight: None,
            family: None,
            italic: false,
            family_name: None,
            max_width: None,
            align: None,
            layout: None,
        });
    }
    let stats = stats_text(s);
    let (sw, _) = canvas.measure(&stats, 12.0);
    canvas.draw(Text {
        x: (left_edge - tw - sw - 20.0).max(10.0),
        y: 9.0,
        content: stats,
        size: 12.0,
        color: token(ColorToken::TextMuted),
        weight: None,
        family: None,
        italic: false,
        family_name: None,
        max_width: None,
        align: None,
        layout: None,
    });
}

/// 工具条统计：分组也计入卡数。
fn stats_text(s: &Snapshot) -> String {
    format!(
        "{} 卡 · {} 连线{}",
        s.data.nodes.len(),
        s.data.edges.len(),
        if s.selection.is_empty() {
            String::new()
        } else {
            format!(" · 已选 {}", s.selection.len())
        }
    )
}

fn card_overlays(ui: &mut Ui, s: &Snapshot, choices: &Choices<'_>) {
    for node in &s.data.nodes {
        if hidden_by_collapsed(&s.data.nodes, &node.id) {
            continue;
        }
        let (w, h) = display_size(node);
        let (sx, sy) = s.screen(node.x, node.y);
        let (sw, sh) = (w as f32 * s.zoom, h as f32 * s.zoom);
        if sx + sw < 0.0 || sy + sh < 0.0 || sx > s.viewport.0 || sy > s.viewport.1 {
            continue;
        }
        if node.node_type == "group" {
            if let Some((id, draft)) = &s.rename {
                if id == &node.id {
                    let rect = UiRect::new(sx + 6.0, sy + 3.0, (sw - 12.0).max(1.0), 24.0);
                    let value = draft.clone();
                    ui.overlay(rect, move |ui| {
                        ui.text_input("canvas-rename-input", value, "分组名");
                    });
                }
            }
            continue;
        }
        // 低缩放卡片没有正文与输入区：浮层控件一并省掉，命中与双击仍走世界层判定
        if compact_card(s.zoom, w as f32) {
            continue;
        }
        if node.node_type == "conversation" {
            let compacting = s.cards.get(&node.id).is_some_and(|chat| chat.compacting);
            let streaming = s.cards.get(&node.id).is_some_and(|chat| chat.streaming);
            let locked = s.locked_by_peer(&node.id).is_some();
            let rect = UiRect::new(
                sx + sw - 28.0 * s.zoom,
                sy + 3.0 * s.zoom,
                24.0 * s.zoom,
                24.0 * s.zoom,
            );
            let id = node.id.clone();
            ui.overlay(rect, move |ui| {
                if compacting {
                    ui.icon_button_named(
                        &format!("canvas-compact-{id}"),
                        "loader-circle",
                        13.0,
                        24.0,
                        ColorToken::TextMuted,
                        false,
                    );
                } else {
                    ui.icon_button_named(
                        &format!("canvas-compact-{id}"),
                        "layers",
                        13.0,
                        24.0,
                        ColorToken::TextMuted,
                        !streaming && !locked,
                    );
                    ui.hint(
                        &format!("canvas-compact-{id}"),
                        if locked { "对方编辑中" } else { "压缩对话" },
                    );
                }
            });
        }
        let pad = 10.0 * s.zoom;
        let body = UiRect::new(
            sx + pad,
            sy + 40.0 * s.zoom,
            (sw - 2.0 * pad).max(1.0),
            (sh - 50.0 * s.zoom).max(1.0),
        );
        match node.node_type.as_str() {
            "text" => {
                let text = text_body(s, node);
                let status = node
                    .data
                    .get("file")
                    .and_then(serde_json::Value::as_str)
                    .map(|path| s.refs.get(path));
                let hint = match status {
                    Some(Some(crate::model::RefContent::Missing)) => {
                        Some(("引用文件缺失", ColorToken::Danger))
                    }
                    Some(Some(crate::model::RefContent::Binary)) => {
                        Some(("二进制附件", ColorToken::TextMuted))
                    }
                    Some(Some(crate::model::RefContent::Image)) => {
                        Some(("引用内容非文本", ColorToken::TextMuted))
                    }
                    Some(Some(crate::model::RefContent::Table { .. })) => {
                        Some(("引用内容非文本", ColorToken::TextMuted))
                    }
                    Some(None) => Some(("加载中…", ColorToken::TextMuted)),
                    _ => None,
                };
                ui.overlay(body, move |ui| {
                    ui.scroll(format!("canvas-text-{}", node.id), 0.0, |ui| {
                        if let Some((hint, color)) = hint {
                            ui.text_styled(hint, TextStyle::new(TextRole::Caption).color(color));
                        } else {
                            match text.as_deref() {
                                Some(text) if !text.is_empty() => ui.markdown(text),
                                _ => ui.text_styled(
                                    "（空）",
                                    TextStyle::new(TextRole::Caption).color(ColorToken::TextMuted),
                                ),
                            }
                        }
                    });
                });
            }
            "conversation" => conversation_overlay(ui, s, node, body, choices),
            _ => {}
        }
    }
}

fn text_body(s: &Snapshot, node: &CanvasFileNode) -> Option<String> {
    if let Some(path) = node.data.get("file").and_then(serde_json::Value::as_str) {
        return match s.refs.get(path) {
            Some(crate::model::RefContent::Text(text)) => {
                Some(atelyx_format::frontmatter::parse(text).body)
            }
            _ => None,
        };
    }
    node.data
        .get("bodyMd")
        .and_then(serde_json::Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn conversation_overlay(
    ui: &mut Ui,
    s: &Snapshot,
    node: &CanvasFileNode,
    rect: UiRect,
    choices: &Choices<'_>,
) {
    let messages = node
        .data
        .get("messages")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let chat = s.cards.get(&node.id).cloned().unwrap_or_default();
    let id = node.id.clone();
    let locked = s.locked_by_peer(&node.id).map(|peer| peer.name.clone());
    let compaction = node.data.get("compaction").cloned();
    let expanded = chat.expanded.clone();
    let streaming = chat.streaming;
    let rollback_confirm = chat.rollback_confirm.clone();
    let selected_agent = node
        .data
        .get("agentId")
        .and_then(serde_json::Value::as_str)
        .and_then(|agent| choices.agents.iter().find(|item| item.id == agent))
        .map(|agent| agent.name.clone())
        .unwrap_or_else(|| "默认".to_string());
    let selected_model = node
        .data
        .get("model")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .or_else(|| choices.default_pick.map(|pick| pick.model.clone()))
        .unwrap_or_else(|| "模型".to_string());
    // 模型标签带推理等级：与供应商菜单里的「推理等级」二级项一致
    let selected_model = format!(
        "{selected_model} · {}",
        effort_label(
            node.data
                .get("reasoningEffort")
                .and_then(serde_json::Value::as_str)
        )
    );
    ui.overlay(rect, move |ui| {
        ui.fill_column(6.0, |ui| {
            // 错误条固定在消息区顶部，不随消息滚动
            if let Some(error) = chat.error.clone() {
                let clear = format!("canvas-error-clear-{id}");
                ui.row(4.0, |ui| {
                    ui.icon_named("circle-alert", 12.0, ColorToken::Danger);
                    ui.text_styled(
                        error,
                        TextStyle::new(TextRole::Micro)
                            .color(ColorToken::Danger)
                            .wrap(),
                    );
                    ui.grow();
                    ui.icon_button_named(&clear, "x", 12.0, 20.0, ColorToken::TextMuted, true);
                    ui.hint(&clear, "清除");
                });
            }
            ui.scroll_follow(format!("canvas-chat-{id}"), 8.0, chat.follow_bottom, |ui| {
                if messages.is_empty() {
                    ui.text_styled(
                        "输入消息开始对话",
                        TextStyle::new(TextRole::Caption).color(ColorToken::TextMuted),
                    );
                }
                let last_assistant = messages.iter().rposition(|message| {
                    message.get("role").and_then(serde_json::Value::as_str) != Some("user")
                });
                let marker = compaction_index(&messages, compaction.as_ref());
                for (index, message) in messages.iter().enumerate() {
                    if Some(index) == marker {
                        if let Some(compaction) = compaction.as_ref() {
                            compaction_marker(ui, compaction);
                        }
                    }
                    render_message(
                        ui,
                        message,
                        &expanded,
                        streaming && last_assistant == Some(index),
                        &id,
                        last_assistant == Some(index),
                        rollback_confirm.as_deref() == Some(message_id_of(message)),
                        rect.w,
                    );
                }
                // 折叠覆盖到末尾（刚压缩完的常态）：标记落在消息列表最后
                if marker == Some(messages.len()) {
                    if let Some(compaction) = compaction.as_ref() {
                        compaction_marker(ui, compaction);
                    }
                }
            });
            if let Some(name) = &locked {
                ui.text_styled(
                    format!("{name} 编辑中"),
                    TextStyle::new(TextRole::Micro).color(ColorToken::TextMuted),
                );
                return;
            }
            if !chat.follow_bottom && !messages.is_empty() {
                ui.row(0.0, |ui| {
                    ui.click_row(
                        &format!("canvas-bottom-{id}"),
                        atelyx_plugin_sdk::frame_ui::Edges::all(4.0),
                        4.0,
                        false,
                        true,
                        |ui| {
                            ui.text_styled(
                                "回到底部",
                                TextStyle::new(TextRole::Micro).color(ColorToken::Accent),
                            );
                        },
                    );
                });
            }
            if let Some(picker) = &chat.mention_picker {
                ui.scroll_max(format!("canvas-picker-{id}"), 160.0, 0.0, |ui| {
                    for (index, candidate) in picker.candidates.iter().enumerate() {
                        let label = crate::chat::candidate_label(candidate);
                        let row_id = format!("canvas-pick-{id}-{index}");
                        ui.click_row(
                            &row_id,
                            atelyx_plugin_sdk::frame_ui::Edges::all(4.0),
                            4.0,
                            picker.selected == index,
                            true,
                            |ui| {
                                ui.text_styled(label, TextStyle::new(TextRole::Caption));
                            },
                        );
                    }
                });
            }
            if !chat.pending_images.is_empty() {
                ui.row(6.0, |ui| {
                    for (index, image) in chat.pending_images.iter().enumerate() {
                        let row_id = format!("canvas-remove-image-{id}-{index}");
                        let label = format!("{} ×", image.file_name);
                        ui.click_row(
                            &row_id,
                            atelyx_plugin_sdk::frame_ui::Edges::all(4.0),
                            4.0,
                            false,
                            true,
                            |ui| {
                                ui.chip(label, ColorToken::SurfaceMuted);
                            },
                        );
                    }
                });
            }
            ui.row(6.0, |ui| {
                ui.icon_button_named(
                    &format!("canvas-attach-{id}"),
                    "plus",
                    12.0,
                    22.0,
                    ColorToken::TextMuted,
                    true,
                );
                ui.click_row(
                    &format!("canvas-agent-{id}"),
                    atelyx_plugin_sdk::frame_ui::Edges::all(4.0),
                    4.0,
                    false,
                    true,
                    |ui| {
                        ui.text_styled(
                            selected_agent,
                            TextStyle::new(TextRole::Micro).color(ColorToken::TextMuted),
                        );
                    },
                );
                ui.click_row(
                    &format!("canvas-provider-{id}"),
                    atelyx_plugin_sdk::frame_ui::Edges::all(4.0),
                    4.0,
                    false,
                    true,
                    |ui| {
                        ui.text_styled(
                            selected_model,
                            TextStyle::new(TextRole::Micro).color(ColorToken::TextMuted),
                        );
                    },
                );
            });
            ui.row(6.0, |ui| {
                ui.text_area_submit_auto(
                    &format!("canvas-input-{id}"),
                    chat.draft.clone(),
                    "输入消息…",
                    28.0,
                    120.0,
                    13.0,
                    atelyx_plugin_sdk::frame_ui::Edges::all(6.0),
                    crate::chat::marks(&chat),
                );
                if chat.streaming {
                    ui.icon_button_filled(
                        &format!("canvas-stop-{id}"),
                        "square",
                        12.0,
                        26.0,
                        ColorToken::Accent,
                        ColorToken::Text,
                        true,
                    );
                } else {
                    let enabled =
                        !chat.draft.trim().is_empty() || !chat.pending_images.is_empty();
                    ui.icon_button_filled(
                        &format!("canvas-send-{id}"),
                        "arrow-up",
                        12.0,
                        26.0,
                        ColorToken::Accent,
                        ColorToken::Text,
                        enabled,
                    );
                }
            });
        });
    });
}

/// 对话消息缩略图边长与折叠块摘要字符上限（与面板消息流同一口径）。
const CHAT_THUMB: f32 = 160.0;
const CHAT_SUMMARY_CHARS: usize = 56;

fn message_id_of(message: &serde_json::Value) -> &str {
    message
        .get("id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
}

fn role_of(message: &serde_json::Value) -> &str {
    message
        .get("role")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
}

/// 用户消息正文：`displayContent` 优先（可为空），缺失回落 `content`（旧消息）。
fn display_of(message: &serde_json::Value) -> &str {
    message
        .get("displayContent")
        .and_then(serde_json::Value::as_str)
        .filter(|text| !text.is_empty())
        .or_else(|| message.get("content").and_then(serde_json::Value::as_str))
        .unwrap_or_default()
}

fn content_of(message: &serde_json::Value) -> &str {
    message
        .get("content")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
}

/// 压缩标记的插入位（消息数组下标；= 长度表示落在列表末尾）；无注解 / 锚点失配 = None。
fn compaction_index(
    messages: &[serde_json::Value],
    compaction: Option<&serde_json::Value>,
) -> Option<usize> {
    let compaction = compaction.filter(|value| !value.is_null())?;
    let anchor = compaction
        .get("upToMessageId")
        .and_then(serde_json::Value::as_str)?;
    Some(
        messages
            .iter()
            .position(|message| message_id_of(message) == anchor)
            .map(|index| index + 1)
            .unwrap_or(messages.len()),
    )
}

fn compaction_marker(ui: &mut Ui, compaction: &serde_json::Value) {
    let summary = compaction
        .get("summary")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let count = compaction
        .get("messageCount")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    ui.boxed(
        BoxWidth::Fill,
        BoxStyle::new(None, Some(ColorToken::Border), 10.0).hairline(),
        Edges::all(8.0),
        4.0,
        |card| {
            card.text_styled(
                format!("上下文已压缩 · 以上 {count} 条已折叠为摘要"),
                TextStyle::new(TextRole::Micro),
            );
            if !summary.is_empty() {
                card.text_styled(
                    summary.to_string(),
                    TextStyle::new(TextRole::Micro)
                        .color(ColorToken::TextSecondary)
                        .wrap(),
                );
            }
        },
    );
}

fn render_message(
    ui: &mut Ui,
    message: &serde_json::Value,
    expanded: &std::collections::HashSet<String>,
    streaming: bool,
    node: &str,
    last_assistant: bool,
    rollback_pending: bool,
    width: f32,
) {
    let message_id = message_id_of(message).to_string();
    if role_of(message) == "user" {
        render_user(
            ui,
            message,
            &message_id,
            node,
            rollback_pending,
            width,
        );
    } else {
        render_assistant(
            ui,
            message,
            expanded,
            streaming,
            &message_id,
            node,
            last_assistant,
            rollback_pending,
        );
    }
}

/// 用户气泡：按内容收缩、纯文本不套 markdown，引用走胶囊，图片附件出缩略图行。
fn render_user(
    ui: &mut Ui,
    message: &serde_json::Value,
    message_id: &str,
    node: &str,
    rollback_pending: bool,
    width: f32,
) {
    let display = display_of(message);
    let plain = is_plain_line(display);
    let max_width = 560.0_f32.min((width - 24.0).max(80.0));
    ui.column(4.0, |column| {
        if !display.is_empty() {
            let bubble_width = if plain {
                (column.text_width(display, TextRole::Body) + 20.0).min(max_width)
            } else {
                max_width
            };
            column.row(0.0, |line| {
                line.grow();
                line.boxed(
                    BoxWidth::Fixed(bubble_width.max(1.0)),
                    BoxStyle::new(Some(ColorToken::Surface), Some(ColorToken::Border), 10.0)
                        .hairline(),
                    Edges::all(10.0),
                    0.0,
                    |bubble| {
                        if plain {
                            bubble.text_styled(
                                display.to_string(),
                                TextStyle::new(TextRole::Body).wrap(),
                            );
                        } else {
                            bubble.markdown_with(display.to_string(), mentions_of(message));
                        }
                    },
                );
            });
        }
        let images: Vec<&str> = message
            .get("attachments")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter(|attachment| {
                attachment.get("kind").and_then(serde_json::Value::as_str) == Some("image")
            })
            .filter_map(|attachment| {
                attachment
                    .get("file")
                    .and_then(serde_json::Value::as_str)
            })
            .collect();
        if !images.is_empty() {
            column.row(6.0, |line| {
                line.grow();
                for file in images {
                    line.image_sized(
                        ImageSource::Vault {
                            path: file.to_string(),
                            max_edge: None,
                        },
                        CHAT_THUMB,
                        CHAT_THUMB,
                        Some(ImageFit::Contain),
                    );
                }
            });
        }
        message_actions(
            column,
            node,
            message_id,
            "user",
            display,
            false,
            false,
            rollback_pending,
        );
    });
}

/// assistant 块（无气泡全宽）：步骤块（思考 / 工具可折叠）+ 最终正文 + 操作行。
fn render_assistant(
    ui: &mut Ui,
    message: &serde_json::Value,
    expanded: &std::collections::HashSet<String>,
    streaming: bool,
    message_id: &str,
    node: &str,
    last_assistant: bool,
    rollback_pending: bool,
) {
    let content = content_of(message);
    let steps = message.get("steps").and_then(serde_json::Value::as_array);
    let mut has_tools = false;
    ui.column(6.0, |column| {
        if let Some(steps) = steps {
            let last = steps.len().saturating_sub(1);
            for (idx, step) in steps.iter().enumerate() {
                let key = format!("{message_id}:{idx}");
                match step.get("kind").and_then(serde_json::Value::as_str) {
                    Some("reasoning") => reasoning_block(
                        column,
                        expanded,
                        step,
                        streaming && idx == last,
                        node,
                        &key,
                    ),
                    Some("text") => {
                        if let Some(text) = step.get("text").and_then(serde_json::Value::as_str) {
                            if !text.is_empty() {
                                column.markdown(text.to_string());
                            }
                        }
                    }
                    Some("tool") => {
                        has_tools = true;
                        tool_block(column, expanded, step, node, &key);
                    }
                    _ => {}
                }
            }
        }
        if has_tools && !content.is_empty() {
            column.rule();
        }
        if !content.is_empty() {
            column.markdown(content.to_string());
        } else if streaming && steps.map(|steps| steps.is_empty()).unwrap_or(true) {
            column.text_styled(
                "生成中…",
                TextStyle::new(TextRole::Caption).color(ColorToken::TextMuted),
            );
        }
        message_actions(
            column,
            node,
            message_id,
            "assistant",
            content,
            last_assistant,
            streaming,
            rollback_pending,
        );
    });
}

/// 思考块：折叠态一行摘要（流式取最新一行、完成后取首行），展开态全文。
fn reasoning_block(
    ui: &mut Ui,
    expanded: &std::collections::HashSet<String>,
    step: &serde_json::Value,
    streaming: bool,
    node: &str,
    key: &str,
) {
    let full = step
        .get("text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let summary = if streaming {
        full.lines()
            .rfind(|line| !line.trim().is_empty())
            .unwrap_or("")
            .trim()
    } else {
        full.lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("")
            .trim()
    };
    let open = expanded.contains(key);
    details_block(
        ui,
        node,
        key,
        open,
        vec![("brain", ColorToken::TextMuted)],
        "思考过程".to_string(),
        summary.to_string(),
        false,
        full.to_string(),
        "思考内容为空",
    );
}

/// 工具块：折叠态 = 状态色图示 + 工具名 + 等宽参数/结果摘要；展开态 = 参数与结果分节。
fn tool_block(
    ui: &mut Ui,
    expanded: &std::collections::HashSet<String>,
    step: &serde_json::Value,
    node: &str,
    key: &str,
) {
    let run = step.get("run").cloned().unwrap_or(serde_json::Value::Null);
    let name = run
        .get("name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("tool");
    let status = run
        .get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("running");
    let args_summary = run
        .get("argsSummary")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let result_summary = run
        .get("resultSummary")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let open = expanded.contains(key);
    // 参数与结果是任意长的工具输出：只有展开时才格式化
    let detail = if open {
        let args_json = run.get("args").map(pretty_json).unwrap_or_default();
        let result = run
            .get("result")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        format!(
            "参数\n{}\n\n结果\n{}",
            if args_json.is_empty() {
                "—".to_string()
            } else {
                args_json
            },
            if status == "running" {
                "进行中…".to_string()
            } else if result.is_empty() {
                result_summary.to_string()
            } else {
                result.to_string()
            }
        )
    } else {
        String::new()
    };
    let summary = match (args_summary.is_empty(), result_summary.is_empty()) {
        (true, true) => String::new(),
        (true, false) => result_summary.to_string(),
        (false, true) => args_summary.to_string(),
        (false, false) => format!("{args_summary} · {result_summary}"),
    };
    let status_icon = match status {
        "error" => ("circle-x", ColorToken::Danger),
        "done" => ("check", ColorToken::Success),
        _ => ("loader-circle", ColorToken::TextMuted),
    };
    details_block(
        ui,
        node,
        key,
        open,
        vec![(tool_icon(name), ColorToken::TextMuted), status_icon],
        tool_label(name),
        summary,
        true,
        detail,
        "",
    );
}

/// 可折叠步骤块（思考 / 工具共用）：折叠态一行，展开态正文（超高滚动）。
#[allow(clippy::too_many_arguments)]
fn details_block(
    ui: &mut Ui,
    node: &str,
    key: &str,
    open: bool,
    leading: Vec<(&str, ColorToken)>,
    title: String,
    summary: String,
    mono_summary: bool,
    body: String,
    empty_hint: &str,
) {
    ui.boxed(
        BoxWidth::Fill,
        BoxStyle::new(Some(ColorToken::SurfaceSunken), Some(ColorToken::Border), 6.0).hairline(),
        Edges::all(0.0),
        0.0,
        |card| {
            card.click_row(
                format!("step|{node}|{key}"),
                Edges::symmetric(0.0, 5.0),
                4.0,
                false,
                true,
                |head| {
                    head.row(6.0, |line| {
                        line.icon_named(
                            if open { "chevron-down" } else { "chevron-right" },
                            12.0,
                            ColorToken::TextMuted,
                        );
                        for (name, color) in &leading {
                            line.icon_named(name, 12.0, *color);
                        }
                        line.text_styled_fit(
                            title.clone(),
                            TextStyle::new(TextRole::Micro).color(ColorToken::TextSecondary),
                        );
                        if !summary.is_empty() {
                            let mut style = TextStyle::new(TextRole::Micro);
                            if mono_summary {
                                style = style.mono();
                            }
                            line.text_styled(one_line(&summary, CHAT_SUMMARY_CHARS), style);
                        }
                    });
                },
            );
            if open {
                if !body.is_empty() {
                    card.rule();
                    card.pad(0.0, 6.0, |body_ui| {
                        body_ui.scroll_sized(
                            format!("step-body:{key}"),
                            160.0,
                            0.0,
                            false,
                            |scroll| {
                                scroll.text_styled(
                                    body.clone(),
                                    TextStyle::new(TextRole::Micro)
                                        .color(ColorToken::TextSecondary)
                                        .wrap(),
                                );
                            },
                        );
                    });
                } else if !empty_hint.is_empty() {
                    card.pad(0.0, 6.0, |hint| {
                        hint.text_styled(empty_hint, TextStyle::new(TextRole::Micro));
                    });
                }
            }
        },
    );
}

#[allow(clippy::too_many_arguments)]
fn message_actions(
    ui: &mut Ui,
    node: &str,
    message_id: &str,
    role: &str,
    content: &str,
    last_assistant: bool,
    streaming: bool,
    rollback_pending: bool,
) {
    let action_id = |action| crate::chat::message_action_id(action, node, Some(message_id));
    ui.row(2.0, |line| {
        let copy = action_id(crate::chat::MessageAction::Copy);
        line.icon_button_named(&copy, "copy", 12.0, 22.0, ColorToken::TextMuted, true);
        line.hint(&copy, "复制");
        if role != "user" && !streaming && !content.starts_with("[错误]") {
            let rollback = action_id(crate::chat::MessageAction::Rollback);
            line.icon_button_named(
                &rollback,
                "corner-up-left",
                12.0,
                22.0,
                ColorToken::TextMuted,
                true,
            );
            line.hint(&rollback, "回到此处（该消息之后转为新话题）");
            let branch = action_id(crate::chat::MessageAction::Branch);
            line.icon_button_named(&branch, "columns-2", 12.0, 22.0, ColorToken::TextMuted, true);
            line.hint(&branch, "分支为新对话卡");
        }
        if role != "user" && last_assistant && !streaming {
            let regen = action_id(crate::chat::MessageAction::Regen);
            line.icon_button_named(&regen, "refresh-cw", 12.0, 22.0, ColorToken::TextMuted, true);
            line.hint(&regen, "重新生成回复");
        }
    });
    if rollback_pending {
        let confirm = action_id(crate::chat::MessageAction::RollbackConfirm);
        let cancel =
            crate::chat::message_action_id(crate::chat::MessageAction::CancelRollback, node, None);
        ui.row(6.0, |line| {
            line.button(&confirm, "确认回滚？");
            line.button(&cancel, "取消");
        });
    }
}

/// 消息里的引用 → 解析器的引用候选：标签文本与草稿里插入的那段字面量逐字一致，胶囊才画得准。
fn mentions_of(message: &serde_json::Value) -> Vec<MentionCandidate> {
    message
        .get("refs")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|reference| {
            let path = reference.get("path").and_then(serde_json::Value::as_str)?;
            let label = reference.get("label").and_then(serde_json::Value::as_str)?;
            let prefix = match reference.get("refKind").and_then(serde_json::Value::as_str) {
                Some("table") => "表格",
                Some("canvas") => "画布",
                Some("dir") => "文件夹",
                _ => "笔记",
            };
            Some(MentionCandidate {
                key: path.to_string(),
                label: format!("{prefix}：{label}"),
            })
        })
        .collect()
}

fn is_plain_line(source: &str) -> bool {
    !source.contains('\n')
        && source.chars().count() <= 200
        && !source.contains(['*', '_', '`', '[', ']', '#', '>', '|', '\\', '~'])
}

fn one_line(source: &str, cap: usize) -> String {
    let mut out: String = source
        .chars()
        .filter(|ch| !ch.is_control())
        .take(cap + 1)
        .collect();
    if out.chars().count() > cap {
        out = out.chars().take(cap).collect();
        out.push('\u{2026}');
    }
    out
}

fn pretty_json(value: &serde_json::Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

fn tool_icon(name: &str) -> &'static str {
    match name {
        "web_search" => "globe",
        "web_fetch" => "link",
        "read_file" | "read_history" => "file-text",
        "glob" | "grep" => "search",
        "edit_file" => "pen-line",
        "write_file" | "append_file" => "file-plus",
        _ => "wrench",
    }
}

fn tool_label(id: &str) -> String {
    match id {
        "web_search" => "联网搜索",
        "web_fetch" => "抓取网页",
        "read_file" => "读取文件",
        "glob" => "查找文件",
        "grep" => "搜索内容",
        "list_dir" => "列出目录",
        "read_history" => "读取历史",
        "edit_file" => "编辑文件",
        "append_file" => "追加内容",
        "write_file" => "写入文件",
        "rename_file" => "重命名文件",
        "move_file" => "移动文件",
        "delete_file" => "删除文件",
        "delete_dir" => "删除目录",
        "todo_write" => "任务清单",
        other => other,
    }
    .to_string()
}

/// 推理等级的展示文案（`off` / `low` / `medium` / `high`；缺省 = 默认）。
fn effort_label(effort: Option<&str>) -> &'static str {
    match effort {
        Some("off") => "关闭",
        Some("low") => "低",
        Some("medium") => "中",
        Some("high") => "高",
        _ => "默认",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atelyx_format::canvas::{CANVAS_SCHEMA, CanvasFile, CanvasFileNode};

    const GROUP_NODE: &str = r#"{"id":"g1","type":"group","x":0.0,"y":0.0,"data":{}}"#;

    /// 工具条统计口径：分组与普通卡一起计数。
    #[test]
    fn stats_count_groups() {
        let nodes = vec![
            CanvasFileNode {
                id: "n1".into(),
                node_type: "text".into(),
                x: 0.0,
                y: 0.0,
                width: None,
                height: None,
                data: serde_json::json!({}),
            },
            serde_json::from_str(GROUP_NODE).unwrap(),
        ];
        let session = Session::new(
            "a.atlx".into(),
            CanvasFile {
                schema: CANVAS_SCHEMA.into(),
                id: "c".into(),
                title: "a".into(),
                nodes,
                edges: Vec::new(),
                created_at: 0,
                updated_at: 0,
            },
            1,
        );
        assert_eq!(stats_text(&Snapshot::of(&session)), "2 卡 · 0 连线");
    }

    /// 压缩标记插在锚点消息之后；锚点失配时落在末尾，空注解不画标记。
    #[test]
    fn compaction_index_anchor() {
        let messages = vec![
            serde_json::json!({ "id": "m1", "role": "user", "content": "a" }),
            serde_json::json!({ "id": "m2", "role": "assistant", "content": "b" }),
        ];
        let compaction = serde_json::json!({ "upToMessageId": "m1", "messageCount": 1 });
        assert_eq!(compaction_index(&messages, Some(&compaction)), Some(1));
        let unknown = serde_json::json!({ "upToMessageId": "gone", "messageCount": 1 });
        assert_eq!(compaction_index(&messages, Some(&unknown)), Some(2));
        assert_eq!(compaction_index(&messages, None), None);
        assert_eq!(
            compaction_index(&messages, Some(&serde_json::Value::Null)),
            None
        );
    }

    /// 引用胶囊的标签与草稿插入字面量一致（前缀按 refKind 区分）。
    #[test]
    fn mention_labels_follow_ref_kind() {
        let message = serde_json::json!({
            "refs": [
                { "path": "note.md", "label": "说明", "refKind": "note" },
                { "path": "t.csv", "label": "清单", "refKind": "table" },
                { "path": "dir", "label": "资料", "refKind": "dir" },
                { "label": "缺路径" }
            ]
        });
        let labels: Vec<String> = mentions_of(&message)
            .into_iter()
            .map(|candidate| candidate.label)
            .collect();
        assert_eq!(labels, vec!["笔记：说明", "表格：清单", "文件夹：资料"]);
    }

    /// 折叠摘要单行：超长补省略号、控制字符剔除。
    #[test]
    fn one_line_truncates_and_strips_controls() {
        assert_eq!(one_line("短句", 56), "短句");
        assert_eq!(one_line("a\nb", 56), "ab");
        let long: String = std::iter::repeat('字').take(60).collect();
        let short = one_line(&long, 56);
        assert_eq!(short.chars().count(), 57);
        assert!(short.ends_with('\u{2026}'));
    }

    /// 消息正文优先级：displayContent 非空优先，否则回落 content。
    #[test]
    fn display_falls_back_to_content() {
        let message = serde_json::json!({ "content": "原始", "displayContent": "" });
        assert_eq!(display_of(&message), "原始");
        let message = serde_json::json!({ "content": "原始", "displayContent": "显示" });
        assert_eq!(display_of(&message), "显示");
    }

    /// 推理等级标签：已知值映射中文，缺省 / 未知回落「默认」。
    #[test]
    fn effort_labels() {
        assert_eq!(effort_label(None), "默认");
        assert_eq!(effort_label(Some("off")), "关闭");
        assert_eq!(effort_label(Some("medium")), "中");
        assert_eq!(effort_label(Some("unknown")), "默认");
    }
}
