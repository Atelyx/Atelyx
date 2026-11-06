//! 画布纯几何：视口变换、卡尺寸与落点、分组归属、框选、吸附、适应视图、命中与连线语义。
//! 全部无 I/O、无 UI 依赖，宿主与插件共用同一份判定。

use serde_json::Value;

use super::{CanvasFile, CanvasFileEdge, CanvasFileNode};

/// 适应视图的视口内边距（屏幕像素）。
pub const FIT_PAD: f32 = 48.0;
pub const MIN_ZOOM: f32 = 0.1;
pub const MAX_ZOOM: f32 = 3.0;
/// 网格吸附步长（世界坐标）。
pub const SNAP_GRID: f64 = 16.0;
/// 对齐参考线吸附阈值（世界坐标）。
pub const ALIGN_TOL: f64 = 6.0;
/// resize 把手命中容差（屏幕像素，按缩放反推世界容差）。
pub const HANDLE_SIZE: f64 = 9.0;
pub const CARD_MIN_W: f64 = 120.0;
pub const CARD_MIN_H: f64 = 80.0;
/// 收起分组的折叠小卡尺寸（磁盘几何保持原值，展开即恢复）。
pub const COLLAPSED_GROUP_W: f64 = 180.0;
pub const COLLAPSED_GROUP_H: f64 = 46.0;

/// 生效视口：世界 ↔ 屏幕的线性变换（screen = world × zoom + pan）。
#[derive(Clone, Copy)]
pub struct View {
    pub zoom: f32,
    pub pan: (f32, f32),
}

impl View {
    pub fn screen(&self, x: f64, y: f64) -> (f32, f32) {
        (x as f32 * self.zoom + self.pan.0, y as f32 * self.zoom + self.pan.1)
    }

    pub fn world(&self, x: f32, y: f32) -> (f64, f64) {
        (
            ((x - self.pan.0) / self.zoom) as f64,
            ((y - self.pan.1) / self.zoom) as f64,
        )
    }
}

/// 屏幕 → 世界坐标。
pub fn world_from_screen(view: &View, x: f32, y: f32) -> (f64, f64) {
    view.world(x, y)
}

/// 卡类型缺省尺寸。
pub fn default_size(node_type: &str) -> (f64, f64) {
    match node_type {
        "conversation" => (520.0, 420.0),
        "table" => (300.0, 110.0),
        "link" => (300.0, 100.0),
        "group" => (400.0, 300.0),
        _ => (360.0, 240.0),
    }
}

/// 卡显示尺寸（width/height 缺省按类型回落）。
pub fn node_size(n: &CanvasFileNode) -> (f64, f64) {
    let (dw, dh) = default_size(&n.node_type);
    (n.width.unwrap_or(dw), n.height.unwrap_or(dh))
}

/// 卡显示尺寸：收起分组 = 折叠小卡尺寸，其余 = 磁盘几何。
/// 收起只是显示形态——width/height 不改磁盘，展开即恢复原尺寸。
pub fn display_size(n: &CanvasFileNode) -> (f64, f64) {
    if group_collapsed(n) {
        (COLLAPSED_GROUP_W, COLLAPSED_GROUP_H)
    } else {
        node_size(n)
    }
}

/// 从起始点向下搜索不与现有卡相交的落点（世界坐标；分组矩形也参与避让）。
pub fn find_free_spot(
    nodes: &[CanvasFileNode],
    start: (f64, f64),
    size: (f64, f64),
    step: f64,
) -> (f64, f64) {
    let overlaps = |x: f64, y: f64| {
        nodes.iter().any(|n| {
            let (w, h) = node_size(n);
            x < n.x + w && x + size.0 > n.x && y < n.y + h && y + size.1 > n.y
        })
    };
    let mut y = start.1;
    while overlaps(start.0, y) {
        y += step;
    }
    (start.0, y)
}

/// 分组是否收起（data.collapsed，缺省展开）。
pub fn group_collapsed(g: &CanvasFileNode) -> bool {
    g.node_type == "group" && g.data.get("collapsed").and_then(Value::as_bool) == Some(true)
}

/// 卡所属分组的下标（成员中心点落在组矩形内；多组重叠取后声明者）。
/// 分组自身不属于任何组；成员判定按磁盘几何（展开矩形），收起只改显示形态。
pub fn group_of(nodes: &[CanvasFileNode], node_id: &str) -> Option<usize> {
    let n = nodes.iter().find(|n| n.id == node_id)?;
    if n.node_type == "group" {
        return None;
    }
    let (w, h) = display_size(n);
    let (cx, cy) = (n.x + w / 2.0, n.y + h / 2.0);
    nodes.iter().rposition(|g| {
        g.node_type == "group" && {
            let (w, h) = node_size(g);
            cx >= g.x && cx <= g.x + w && cy >= g.y && cy <= g.y + h
        }
    })
}

/// 分组的成员 id 列表（成员判定 = 中心点在组矩形内，纯空间关系不落盘）。
pub fn group_members(nodes: &[CanvasFileNode], group_id: &str) -> Vec<String> {
    nodes
        .iter()
        .filter(|n| {
            n.node_type != "group"
                && group_of(nodes, &n.id).is_some_and(|i| nodes[i].id == group_id)
        })
        .map(|n| n.id.clone())
        .collect()
}

/// 卡是否被收起分组隐藏（隐藏 = 不绘制、不命中、浮层不建）。
pub fn hidden_by_collapsed(nodes: &[CanvasFileNode], node_id: &str) -> bool {
    group_of(nodes, node_id).is_some_and(|i| group_collapsed(&nodes[i]))
}

/// 框选命中：显示矩形与选择矩形相交的卡（不含分组）。
pub fn marquee_hit(nodes: &[CanvasFileNode], rect: (f64, f64, f64, f64)) -> Vec<String> {
    let (rx, ry, rw, rh) = rect;
    nodes
        .iter()
        .filter(|n| {
            if n.node_type == "group" || hidden_by_collapsed(nodes, &n.id) {
                return false;
            }
            let (w, h) = display_size(n);
            n.x < rx + rw && n.x + w > rx && n.y < ry + rh && n.y + h > ry
        })
        .map(|n| n.id.clone())
        .collect()
}

/// 对齐参考线（世界坐标的竖线 / 横线）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Guide {
    pub vertical: bool,
    pub at: f64,
}

/// 拖动吸附结果：位移修正量与生效参考线。
#[derive(Debug, Clone, PartialEq)]
pub struct SnapOutcome {
    pub dx: f64,
    pub dy: f64,
    pub guides: Vec<Guide>,
}

/// 拖动吸附：移动集包围盒的左 / 中 / 右对静态节点的边 / 中心线全配对做参考线吸附
/// （阈值内取最近，两轴独立），该轴无参考线时坐标取整到网格。
pub fn snap_move(statics: &[(f64, f64, f64, f64)], bbox: (f64, f64, f64, f64)) -> SnapOutcome {
    let (bx, by, bw, bh) = bbox;
    let (m_xs, m_ys) = ([bx, bx + bw / 2.0, bx + bw], [by, by + bh / 2.0, by + bh]);
    let mut best_x: Option<(f64, f64)> = None;
    let mut best_y: Option<(f64, f64)> = None;
    for &(sx, sy, sw, sh) in statics {
        let s_xs = [sx, sx + sw / 2.0, sx + sw];
        let s_ys = [sy, sy + sh / 2.0, sy + sh];
        for &m in &m_xs {
            for &s in &s_xs {
                let d = s - m;
                if d.abs() <= ALIGN_TOL && best_x.is_none_or(|(bd, _)| d.abs() < bd.abs()) {
                    best_x = Some((d, s));
                }
            }
        }
        for &m in &m_ys {
            for &s in &s_ys {
                let d = s - m;
                if d.abs() <= ALIGN_TOL && best_y.is_none_or(|(bd, _)| d.abs() < bd.abs()) {
                    best_y = Some((d, s));
                }
            }
        }
    }
    let mut guides = Vec::new();
    let dx = match best_x {
        Some((d, line)) => {
            guides.push(Guide { vertical: true, at: line });
            d
        }
        None => (bx / SNAP_GRID).round() * SNAP_GRID - bx,
    };
    let dy = match best_y {
        Some((d, line)) => {
            guides.push(Guide { vertical: false, at: line });
            d
        }
        None => (by / SNAP_GRID).round() * SNAP_GRID - by,
    };
    SnapOutcome { dx, dy, guides }
}

/// resize 网格取整（钳最小尺寸后取整到网格）。
pub fn snap_size(w: f64, h: f64) -> (f64, f64) {
    let snap = |v: f64, min: f64| ((v / SNAP_GRID).round() * SNAP_GRID).max(min);
    (snap(w, CARD_MIN_W), snap(h, CARD_MIN_H))
}

/// 矩形集包围盒 (x, y, w, h)；空集 = 原点零尺寸。
pub fn bbox_of_rects(rects: &[(f64, f64, f64, f64)]) -> (f64, f64, f64, f64) {
    let Some(&(x0, y0, _, _)) = rects.first() else {
        return (0.0, 0.0, 0.0, 0.0);
    };
    let (bx0, by0, bx1, by1) = rects
        .iter()
        .fold((x0, y0, x0, y0), |(ax0, ay0, ax1, ay1), (x, y, w, h)| {
            (ax0.min(*x), ay0.min(*y), ax1.max(x + w), ay1.max(y + h))
        });
    (bx0, by0, bx1 - bx0, by1 - by0)
}

/// 适应视图变换：全部可见内容卡包围盒装入视口（分组与收起组隐藏成员不计），无卡 = 复位原点。
pub fn fit_transform(
    nodes: &[CanvasFileNode],
    card_size: impl Fn(&CanvasFileNode) -> (f64, f64),
    vw: f32,
    vh: f32,
) -> (f32, (f32, f32)) {
    let mut bounds: Option<(f64, f64, f64, f64)> = None;
    for n in nodes {
        if n.node_type == "group" || hidden_by_collapsed(nodes, &n.id) {
            continue;
        }
        let (w, h) = card_size(n);
        let b = bounds.get_or_insert((n.x, n.y, n.x, n.y));
        b.0 = b.0.min(n.x);
        b.1 = b.1.min(n.y);
        b.2 = b.2.max(n.x + w);
        b.3 = b.3.max(n.y + h);
    }
    let zoom = match bounds {
        Some((x0, y0, x1, y1)) if x1 > x0 && y1 > y0 => {
            let zx = (vw - FIT_PAD * 2.0) / (x1 - x0) as f32;
            let zy = (vh - FIT_PAD * 2.0) / (y1 - y0) as f32;
            zx.min(zy).clamp(MIN_ZOOM, 1.0)
        }
        _ => 1.0,
    };
    let (cx, cy) = bounds
        .map(|(x0, y0, x1, y1)| ((x0 + x1) / 2.0, (y0 + y1) / 2.0))
        .unwrap_or((0.0, 0.0));
    (zoom, (vw / 2.0 - cx as f32 * zoom, vh / 2.0 - cy as f32 * zoom))
}

/// 顶层卡命中（世界坐标）：非分组卡优先（组矩形内的卡不被组遮挡），收起组隐藏成员不命中。
pub fn hit_card(canvas: &CanvasFile, world: (f64, f64)) -> Option<usize> {
    let contains = |n: &CanvasFileNode| {
        let (w, h) = display_size(n);
        world.0 >= n.x && world.0 <= n.x + w && world.1 >= n.y && world.1 <= n.y + h
    };
    canvas
        .nodes
        .iter()
        .rposition(|n| n.node_type != "group" && !hidden_by_collapsed(&canvas.nodes, &n.id) && contains(n))
        .or_else(|| canvas.nodes.iter().rposition(|n| n.node_type == "group" && contains(n)))
}

/// 右下角 resize 把手命中（仅单选；世界容差按缩放反推屏幕像素）。
pub fn hit_resize_handle(
    canvas: &CanvasFile,
    selection: &[String],
    zoom: f32,
    world: (f64, f64),
) -> Option<String> {
    if selection.len() != 1 {
        return None;
    }
    let tol = HANDLE_SIZE / zoom as f64;
    for n in canvas.nodes.iter().rev() {
        if n.node_type == "group" || !selection.iter().any(|s| s == &n.id) {
            continue;
        }
        let (w, h) = display_size(n);
        let (hx, hy) = (n.x + w, n.y + h);
        if world.0 >= hx - tol && world.0 <= hx + tol && world.1 >= hy - tol && world.1 <= hy + tol {
            return Some(n.id.clone());
        }
    }
    None
}

/// 右侧连接点命中（仅悬停 / 选中的非分组、非对话卡）。
pub fn hit_link_handle(
    canvas: &CanvasFile,
    hover: Option<&str>,
    selection: &[String],
    zoom: f32,
    world: (f64, f64),
) -> Option<String> {
    let tol = 6.0 / zoom as f64;
    for n in canvas.nodes.iter().rev() {
        if matches!(n.node_type.as_str(), "group" | "conversation")
            || hidden_by_collapsed(&canvas.nodes, &n.id)
        {
            continue;
        }
        let active = hover == Some(n.id.as_str()) || selection.iter().any(|s| s == &n.id);
        if !active {
            continue;
        }
        let (w, h) = display_size(n);
        let (cx, cy) = (n.x + w, n.y + h / 2.0);
        if (world.0 - cx).abs() <= tol && (world.1 - cy).abs() <= tol {
            return Some(n.id.clone());
        }
    }
    None
}

/// 卡指定侧的连线锚点（世界坐标）。side = left/right/top/bottom。
fn anchor_of(x: f64, y: f64, w: f64, h: f64, side: &str) -> (f64, f64) {
    match side {
        "left" => (x, y + h / 2.0),
        "right" => (x + w, y + h / 2.0),
        "top" => (x + w / 2.0, y),
        _ => (x + w / 2.0, y + h),
    }
}

/// 从旧句柄名（如 "right-source"）取侧名；缺失 = None（回落最近侧）。
fn handle_side(handle: &Option<String>) -> Option<&str> {
    let side = handle.as_deref()?.split('-').next()?;
    matches!(side, "left" | "right" | "top" | "bottom").then_some(side)
}

/// 无句柄时的最近侧回落（按两端卡中心连线方向）。
fn nearest_side(x: f64, y: f64, w: f64, h: f64, toward: (f64, f64)) -> &'static str {
    let (cx, cy) = (x + w / 2.0, y + h / 2.0);
    let (dx, dy) = (toward.0 - cx, toward.1 - cy);
    if dx.abs() >= dy.abs() {
        if dx >= 0.0 { "right" } else { "left" }
    } else if dy >= 0.0 {
        "bottom"
    } else {
        "top"
    }
}

/// 连线两端锚点：端点藏在收起组内时聚到组边缘，句柄缺失时按对端方向取最近侧。
pub fn edge_endpoints(canvas: &CanvasFile, e: &CanvasFileEdge) -> Option<((f64, f64), (f64, f64))> {
    let s = canvas.nodes.iter().find(|n| n.id == e.source)?;
    let t = canvas.nodes.iter().find(|n| n.id == e.target)?;
    let endpoint = |n: &CanvasFileNode, other: &CanvasFileNode, handle: &Option<String>| {
        if let Some(gi) = group_of(&canvas.nodes, &n.id) {
            let g = &canvas.nodes[gi];
            if group_collapsed(g) {
                let (ow, oh) = display_size(other);
                let toward = (other.x + ow / 2.0, other.y + oh / 2.0);
                let (w, h) = display_size(g);
                return anchor_of(g.x, g.y, w, h, nearest_side(g.x, g.y, w, h, toward));
            }
        }
        let (w, h) = display_size(n);
        let side = handle_side(handle)
            .map(str::to_string)
            .unwrap_or_else(|| {
                let (ow, oh) = display_size(other);
                nearest_side(n.x, n.y, w, h, (other.x + ow / 2.0, other.y + oh / 2.0)).to_string()
            });
        anchor_of(n.x, n.y, w, h, &side)
    };
    Some((endpoint(s, t, &e.source_handle), endpoint(t, s, &e.target_handle)))
}

/// 连线两端是否都藏在同一收起组内（绘制时整条跳过）。
pub fn edge_hidden(canvas: &CanvasFile, e: &CanvasFileEdge) -> bool {
    match (group_of(&canvas.nodes, &e.source), group_of(&canvas.nodes, &e.target)) {
        (Some(si), Some(ti)) => si == ti && group_collapsed(&canvas.nodes[si]),
        _ => false,
    }
}

/// 连线语义：关联 / 资产引用（虚实表达消费）/ 产出血缘 / 其它有向边。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeRole {
    Association,
    Reference { consumed: bool },
    Lineage,
    Flow,
}

/// 判定一条连线的语义（磁盘 `directed` 缺省 = 数据流）。
pub fn edge_role(canvas: &CanvasFile, e: &CanvasFileEdge) -> EdgeRole {
    if e.directed == Some(false) {
        return EdgeRole::Association;
    }
    let source_is_conversation = canvas
        .nodes
        .iter()
        .find(|n| n.id == e.source)
        .is_some_and(|n| n.node_type == "conversation");
    let target = canvas.nodes.iter().find(|n| n.id == e.target);
    match target {
        Some(t) if t.node_type == "conversation" && !source_is_conversation => {
            EdgeRole::Reference { consumed: reference_consumed(canvas, t, e) }
        }
        _ if source_is_conversation => EdgeRole::Lineage,
        _ => EdgeRole::Flow,
    }
}

/// 资产是否已被目标对话消费：消息 refs 的 nodeId、附件的 sourceNodeId，
/// 或 Agent 实际读取过来源卡引用的文件（read_file 工具步命中）。
fn reference_consumed(canvas: &CanvasFile, target: &CanvasFileNode, e: &CanvasFileEdge) -> bool {
    let Some(messages) = target.data.get("messages").and_then(Value::as_array) else {
        return false;
    };
    let source_file = canvas
        .nodes
        .iter()
        .find(|n| n.id == e.source)
        .and_then(|n| n.data.get("file").and_then(Value::as_str));
    messages.iter().any(|m| {
        let by_ref = m
            .get("refs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|r| r.get("nodeId").and_then(Value::as_str) == Some(e.source.as_str()));
        let by_attachment = m
            .get("attachments")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|a| a.get("sourceNodeId").and_then(Value::as_str) == Some(e.source.as_str()));
        let by_read = source_file.is_some_and(|file| message_read_file(m, file));
        by_ref || by_attachment || by_read
    })
}

fn message_read_file(message: &Value, file: &str) -> bool {
    message
        .get("steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|s| s.get("kind").and_then(Value::as_str) == Some("tool"))
        .any(|s| {
            let run = s.get("run");
            run.and_then(|r| r.get("name")).and_then(Value::as_str) == Some("read_file")
                && run
                    .and_then(|r| r.get("args"))
                    .and_then(|a| a.get("path"))
                    .and_then(Value::as_str)
                    == Some(file)
        })
}

/// 连线是否为数据流连线（directed 缺省 = 数据流）。
pub fn is_dataflow(e: &CanvasFileEdge) -> bool {
    e.directed != Some(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(id: &str, node_type: &str, x: f64, y: f64) -> CanvasFileNode {
        CanvasFileNode {
            id: id.into(),
            node_type: node_type.into(),
            x,
            y,
            width: None,
            height: None,
            data: json!({}),
        }
    }

    #[test]
    fn group_membership_uses_center_point() {
        let nodes = vec![
            node("g", "group", 0.0, 0.0),
            node("a", "text", 10.0, 10.0),
            node("b", "text", 500.0, 500.0),
        ];
        assert_eq!(group_of(&nodes, "a"), Some(0));
        assert_eq!(group_of(&nodes, "b"), None);
        assert_eq!(group_members(&nodes, "g"), ["a"]);
        assert!(!hidden_by_collapsed(&nodes, "a"));
    }

    #[test]
    fn snap_prefers_alignment_then_grid() {
        let snap = snap_move(&[(100.0, 100.0, 50.0, 50.0)], (96.0, 33.0, 20.0, 20.0));
        assert_eq!(snap.dx, 4.0);
        assert_eq!(snap.guides, [Guide { vertical: true, at: 100.0 }]);
        assert_eq!(snap.dy, -1.0, "33 取整到 32");
    }

    #[test]
    fn fit_ignores_groups_and_collapsed_members() {
        let mut collapsed = node("g", "group", 0.0, 0.0);
        collapsed.data = json!({ "collapsed": true });
        let nodes = vec![collapsed, node("a", "text", 10.0, 10.0), node("b", "text", 400.0, 400.0)];
        let (zoom, pan) = fit_transform(&nodes, display_size, 800.0, 600.0);
        assert!(zoom > 0.0);
        assert!(pan.0.is_finite());
    }

    #[test]
    fn reference_edge_consumed_by_message_ref() {
        let mut target = node("c", "conversation", 0.0, 0.0);
        target.data = json!({ "messages": [{ "refs": [{ "nodeId": "s" }] }] });
        let canvas = CanvasFile {
            schema: super::super::CANVAS_SCHEMA.into(),
            id: "c1".into(),
            title: "t".into(),
            nodes: vec![node("s", "text", 0.0, 0.0), target],
            edges: vec![],
            created_at: 0,
            updated_at: 0,
        };
        let edge = CanvasFileEdge {
            id: "e".into(),
            source: "s".into(),
            target: "c".into(),
            source_handle: None,
            target_handle: None,
            directed: None,
            link_mode: None,
            created_at: 0,
        };
        assert_eq!(edge_role(&canvas, &edge), EdgeRole::Reference { consumed: true });
    }
}
