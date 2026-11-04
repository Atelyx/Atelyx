//! 白板交互：指针按下 / 拖动 / 抬起 / 滚轮统一换算到世界坐标后驱动选中、拖动与视口。
//! 工具栏在顶部、小地图在右下角；浮层内的输入由控件层处理，不经过本模块。

use atelyx_format::canvas::geometry::{
    self, bbox_of_rects, display_size, hit_card, hit_link_handle, hit_resize_handle, marquee_hit,
    snap_move, snap_size,
};
use atelyx_plugin_protocol::frame::{FrameInput, Modifiers, MouseButton};
use atelyx_plugin_protocol::method::{MenuItem, MenuOpenParams};
use atelyx_plugin_sdk::frame_ui::Rect;

use crate::model::{
    DragKind, DragState, MARQUEE_MIN, MINIMAP_H, MINIMAP_MARGIN, MINIMAP_W, Session, TOOLBAR_H,
};

/// 指针处理结果：是否需要重绘 / 排保存 / 交给宿主或其它模块的副作用。
#[derive(Default)]
pub struct Outcome {
    pub redraw: bool,
    pub dirty: bool,
    pub open_file: Option<String>,
    pub rename_start: Option<String>,
    pub link: Option<(String, String)>,
    pub menu: Option<MenuOpenParams>,
}

/// 面内原始输入入口（只处理白板坐标，不解释控件层事件）。
pub fn handle(
    session: &mut Session,
    input: &FrameInput,
    size: (f32, f32),
    mods: &mut Modifiers,
) -> Outcome {
    let mut out = Outcome::default();
    session.view.viewport = (size.0, size.1);
    match input {
        FrameInput::PointerDown { x, y, button, .. } => {
            out.redraw = press(session, *x, *y, *button, size, mods, &mut out);
        }
        FrameInput::PointerMove { x, y, .. } => {
            out.redraw = moved(session, *x, *y, size);
        }
        FrameInput::PointerUp { .. } => {
            out.redraw = release(session, &mut out);
        }
        FrameInput::Wheel {
            x,
            y,
            delta_x,
            delta_y,
            modifiers,
            ..
        } => {
            *mods = *modifiers;
            out.redraw = wheel(session, *x, *y, *delta_x, *delta_y, modifiers, size);
        }
        FrameInput::DoubleClick {
            x,
            y,
            button: MouseButton::Left,
            ..
        } => {
            out.redraw = double_click(session, *x, *y, size, &mut out);
        }
        FrameInput::PointerLeave => {
            // 指针离开面：悬停态不能留到下次进入
            out.redraw = session.view.hover.take().is_some();
            out.redraw |= session.view.toolbar_hover.take().is_some();
        }
        FrameInput::Key { modifiers, .. } => {
            *mods = *modifiers;
        }
        _ => {}
    }
    out
}

/// 指针相对世界区（工具栏下方）的局部 y。
fn local(y: f32) -> f32 {
    y - TOOLBAR_H
}

fn press(
    session: &mut Session,
    x: f32,
    y: f32,
    button: MouseButton,
    size: (f32, f32),
    mods: &Modifiers,
    out: &mut Outcome,
) -> bool {
    // 工具条是独立浮层，只认左键
    if button == MouseButton::Left && y < TOOLBAR_H {
        return toolbar_press(session, x, size);
    }
    if button == MouseButton::Middle {
        session.view.drag = Some(DragState {
            kind: DragKind::Pan,
            start: (x, y),
            orig: Default::default(),
            mutated: false,
        });
        return false;
    }
    if button == MouseButton::Right {
        // 工具条与小地图都不落到世界层；拖拽中也不弹菜单
        if session.view.drag.is_some()
            || y < TOOLBAR_H
            || minimap_area(size).contains(x, y)
        {
            return false;
        }
        let world = session.view.world(x, local(y));
        session.view.menu_at = Some(world);
        out.menu = context_menu(session, world);
        return true;
    }
    if button != MouseButton::Left {
        return false;
    }
    if let Some((wx, wy)) = minimap_world(session, x, y, size) {
        session.view.pan = (
            size.0 / 2.0 - wx as f32 * session.view.zoom,
            (size.1 - TOOLBAR_H) / 2.0 - wy as f32 * session.view.zoom,
        );
        session.view.fit_pending = false;
        session.view.drag = Some(DragState {
            kind: DragKind::Minimap,
            start: (x, y),
            orig: Default::default(),
            mutated: false,
        });
        return true;
    }
    let world = session.view.world(x, local(y));
    if let Some(id) = resize_handle(session, world) {
        session.push_undo();
        session.view.selection = vec![id.clone()];
        let orig = session
            .node(&id)
            .map(|node| {
                let (w, h) = display_size(node);
                (node.x, node.y, w, h)
            })
            .into_iter()
            .map(|rect| (id.clone(), rect))
            .collect();
        session.view.drag = Some(DragState {
            kind: DragKind::Resize(id),
            start: (x, y),
            orig,
            mutated: false,
        });
        return true;
    }
    if let Some(id) = link_handle(session, world) {
        session.view.selection = vec![id.clone()];
        session.view.drag = Some(DragState {
            kind: DragKind::Link(id),
            start: (x, y),
            orig: Default::default(),
            mutated: false,
        });
        session.view.link_to = Some((x, local(y)));
        return true;
    }
    if let Some(index) = hit_card(&session.data, world) {
        let id = session.data.nodes[index].id.clone();
        if mods.shift {
            if let Some(pos) = session
                .view
                .selection
                .iter()
                .position(|selected| selected == &id)
            {
                session.view.selection.remove(pos);
            } else {
                session.view.selection.push(id);
            }
            return true;
        }
        if !session
            .view
            .selection
            .iter()
            .any(|selected| selected == &id)
        {
            session.view.selection = vec![id.clone()];
        }
        if crate::model::over_card_body(&session.data.nodes[index], world) {
            return true;
        }
        session.push_undo();
        let orig = session.drag_geometry();
        session.view.drag = Some(DragState {
            kind: DragKind::Card,
            start: (x, y),
            orig,
            mutated: false,
        });
        return true;
    }
    let base = if mods.shift {
        std::mem::take(&mut session.view.selection)
    } else {
        session.view.selection.clear();
        Vec::new()
    };
    session.view.guides.clear();
    session.view.marquee_to = Some((x, y));
    session.view.drag = Some(DragState {
        kind: DragKind::Marquee { base },
        start: (x, y),
        orig: Default::default(),
        mutated: false,
    });
    true
}

fn toolbar_press(session: &mut Session, x: f32, size: (f32, f32)) -> bool {
    let y = TOOLBAR_H / 2.0;
    for (name, (bx, by, bw, bh)) in crate::model::zoom_buttons(size.0) {
        if !Rect::new(bx, by, bw, bh).contains(x, y) {
            continue;
        }
        match name {
            "zoom-in" => zoom_at(session, size.0 / 2.0, (size.1 - TOOLBAR_H) / 2.0, 1.12),
            "zoom-out" => zoom_at(
                session,
                size.0 / 2.0,
                (size.1 - TOOLBAR_H) / 2.0,
                1.0 / 1.12,
            ),
            _ => session.request_fit(),
        }
        return true;
    }
    false
}

fn minimap_area(size: (f32, f32)) -> Rect {
    Rect::new(
        size.0 - MINIMAP_W - MINIMAP_MARGIN,
        size.1 - MINIMAP_H - MINIMAP_MARGIN,
        MINIMAP_W,
        MINIMAP_H,
    )
}

fn minimap_world(session: &Session, x: f32, y: f32, size: (f32, f32)) -> Option<(f64, f64)> {
    let area = minimap_area(size);
    if !area.contains(x, y) {
        return None;
    }
    let (scale, ox, oy) = crate::model::minimap_transform(&session.data.nodes)?;
    Some((
        ox + ((x - area.x) / scale) as f64,
        oy + ((y - area.y) / scale) as f64,
    ))
}

fn resize_handle(session: &Session, world: (f64, f64)) -> Option<String> {
    hit_resize_handle(
        &session.data,
        &session.view.selection,
        session.view.zoom,
        world,
    )
}

fn link_handle(session: &Session, world: (f64, f64)) -> Option<String> {
    hit_link_handle(
        &session.data,
        session.view.hover.as_deref(),
        &session.view.selection,
        session.view.zoom,
        world,
    )
}

fn moved(session: &mut Session, x: f32, y: f32, size: (f32, f32)) -> bool {
    // 已有拖拽时不再按工具栏拦截：指针经过顶部仍继续拖动
    if session.view.drag.is_none() && y < TOOLBAR_H {
        let hover = toolbar_hover(size, x);
        session.view.toolbar_hover = hover;
        return false;
    }
    session.view.toolbar_hover = None;
    // 小地图是独立浮层：悬停不命中它下面的世界层卡片
    if session.view.drag.is_none() && minimap_area(size).contains(x, y) {
        return session.view.hover.take().is_some();
    }
    let world = session.view.world(x, local(y));
    let Some(drag) = session.view.drag.clone() else {
        let hover = hit_card(&session.data, world)
            .map(|index| session.data.nodes[index].id.clone())
            .filter(|id| {
                session
                    .data
                    .nodes
                    .iter()
                    .any(|n| &n.id == id && n.node_type != "group")
            });
        if session.view.hover != hover {
            session.view.hover = hover;
            return true;
        }
        return false;
    };
    match drag.kind {
        DragKind::Pan => {
            session.view.pan.0 += x - drag.start.0;
            session.view.pan.1 += y - drag.start.1;
            if let Some(active) = session.view.drag.as_mut() {
                active.start = (x, y);
            }
            true
        }
        DragKind::Minimap => {
            if let Some((wx, wy)) = minimap_world(session, x, y, size) {
                session.view.pan = (
                    size.0 / 2.0 - wx as f32 * session.view.zoom,
                    (size.1 - TOOLBAR_H) / 2.0 - wy as f32 * session.view.zoom,
                );
                session.view.fit_pending = false;
                return true;
            }
            false
        }
        DragKind::Marquee { base } => {
            let start = session.view.world(drag.start.0, local(drag.start.1));
            let end = session.view.world(x, local(y));
            let rect = (
                start.0.min(end.0),
                start.1.min(end.1),
                (end.0 - start.0).abs(),
                (end.1 - start.1).abs(),
            );
            let mut selection = base;
            for id in marquee_hit(&session.data.nodes, rect) {
                if !selection.contains(&id) {
                    selection.push(id);
                }
            }
            session.view.selection = selection;
            session.view.marquee_to = Some((x, y));
            true
        }
        DragKind::Card => {
            let dx = (x - drag.start.0) as f64 / session.view.zoom as f64;
            let dy = (y - drag.start.1) as f64 / session.view.zoom as f64;
            if dx == 0.0 && dy == 0.0 {
                return false;
            }
            let moving: Vec<(f64, f64, f64, f64)> = drag
                .orig
                .values()
                .map(|(a, b, c, d)| (*a + dx, *b + dy, *c, *d))
                .collect();
            let bbox = bbox_of_rects(&moving);
            let statics: Vec<(f64, f64, f64, f64)> = session
                .data
                .nodes
                .iter()
                .filter(|n| !drag.orig.contains_key(&n.id))
                .filter(|n| !geometry::hidden_by_collapsed(&session.data.nodes, &n.id))
                .map(|n| {
                    let (w, h) = display_size(n);
                    (n.x, n.y, w, h)
                })
                .collect();
            let snap = snap_move(&statics, bbox);
            session.view.guides = snap.guides.clone();
            for (id, (ox, oy, _, _)) in &drag.orig {
                if let Some(node) = session.node_mut(id) {
                    node.x = ox + dx + snap.dx;
                    node.y = oy + dy + snap.dy;
                }
            }
            // 几何一变就置脏：拖拽中途来远端补丁 / 自动保存也能认出本端有改动
            session.touch();
            if let Some(active) = session.view.drag.as_mut() {
                active.mutated = true;
            }
            true
        }
        DragKind::Resize(id) => {
            let Some((ox, oy, ow, oh)) = drag.orig.get(&id).copied() else {
                return false;
            };
            let dx = (x - drag.start.0) as f64 / session.view.zoom as f64;
            let dy = (y - drag.start.1) as f64 / session.view.zoom as f64;
            if dx == 0.0 && dy == 0.0 {
                return false;
            }
            let (w, h) = snap_size(ow + dx, oh + dy);
            if let Some(node) = session.node_mut(&id) {
                node.x = ox;
                node.y = oy;
                node.width = Some(w);
                node.height = Some(h);
            }
            session.touch();
            if let Some(active) = session.view.drag.as_mut() {
                active.mutated = true;
            }
            true
        }
        DragKind::Link(_) => {
            session.view.link_to = Some((x, local(y)));
            true
        }
    }
}

fn toolbar_hover(size: (f32, f32), x: f32) -> Option<String> {
    let y = TOOLBAR_H / 2.0;
    crate::model::zoom_buttons(size.0)
        .into_iter()
        .find(|(_, (bx, by, bw, bh))| Rect::new(*bx, *by, *bw, *bh).contains(x, y))
        .map(|(name, _)| name.to_string())
}

fn release(session: &mut Session, out: &mut Outcome) -> bool {
    let Some(drag) = session.view.drag.take() else {
        return false;
    };
    let end = session.view.marquee_to.take();
    let link_to = session.view.link_to.take();
    session.view.guides.clear();
    match drag.kind {
        DragKind::Marquee { base } => {
            if let Some((ex, ey)) = end {
                // 两个轴都没超过最小位移才算点击：只看斜边会把横向轻拖误判成点击
                if (ex - drag.start.0).abs() < MARQUEE_MIN
                    && (ey - drag.start.1).abs() < MARQUEE_MIN
                {
                    session.view.selection = base;
                }
            }
            true
        }
        DragKind::Card | DragKind::Resize(_) => {
            if drag.mutated {
                session.touch();
                out.dirty = true;
            } else {
                session.view.undo.pop();
            }
            true
        }
        DragKind::Link(source) => {
            let Some(link_to) = link_to else { return true };
            let target = session.view.world(link_to.0, link_to.1);
            if let Some(index) = hit_card(&session.data, target) {
                let node = &session.data.nodes[index];
                if node.node_type == "conversation" && node.id != source {
                    out.link = Some((source, node.id.clone()));
                }
            }
            true
        }
        DragKind::Pan | DragKind::Minimap => false,
    }
}

fn wheel(
    session: &mut Session,
    x: f32,
    y: f32,
    delta_x: f32,
    delta_y: f32,
    modifiers: &Modifiers,
    size: (f32, f32),
) -> bool {
    // 小地图是独立浮层：落在矩形内一律消费，空画布时也不能透传成缩放
    if minimap_area(size).contains(x, y) {
        return false;
    }
    // 还没有真实视口时不算适应：否则会按 1×1 把视口钉在缩放下限
    if session.view.fit_pending && size.0 > 1.0 && size.1 > 1.0 {
        session.apply_fit(size.0, size.1);
    }
    // 协议增量正 = 向下 / 向右（宿主已按 Web 惯例换算）
    if modifiers.control || modifiers.meta {
        session.view.pan.1 -= if delta_y != 0.0 { delta_y } else { delta_x };
        return true;
    }
    if modifiers.alt {
        session.view.pan.0 -= if delta_x != 0.0 { delta_x } else { delta_y };
        return true;
    }
    let factor = (-delta_y * 0.0025).exp();
    zoom_at(session, x, local(y), factor);
    true
}

pub fn zoom_at(session: &mut Session, x: f32, y: f32, factor: f32) {
    let next = (session.view.zoom * factor).clamp(0.1, 3.0);
    let k = next / session.view.zoom;
    session.view.pan = (
        x - (x - session.view.pan.0) * k,
        y - (y - session.view.pan.1) * k,
    );
    session.view.zoom = next;
    session.view.fit_pending = false;
}

fn double_click(session: &mut Session, x: f32, y: f32, size: (f32, f32), out: &mut Outcome) -> bool {
    // 工具条与小地图不落到世界层
    if y < TOOLBAR_H || minimap_area(size).contains(x, y) {
        return false;
    }
    let world = session.view.world(x, local(y));
    let Some(index) = hit_card(&session.data, world) else {
        return false;
    };
    let node = &session.data.nodes[index];
    let id = node.id.clone();
    if node.node_type == "group" {
        if world.1 <= node.y + 30.0 {
            session.view.group_rename = Some((
                id.clone(),
                node.data
                    .get("label")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            ));
            out.rename_start = Some(id);
        } else {
            crate::actions::toggle_collapse(session, &id);
            session.touch();
            out.dirty = true;
        }
        return true;
    }
    if matches!(node.node_type.as_str(), "table" | "text") {
        if let Some(path) = node.data.get("file").and_then(serde_json::Value::as_str) {
            out.open_file = Some(path.to_string());
        }
    }
    true
}

/// 卡正文浮层矩形（世界层命中时用来判断「按下落在正文上」）。
pub fn card_bodies(session: &Session) -> Vec<(String, Rect)> {
    session
        .data
        .nodes
        .iter()
        .filter(|n| matches!(n.node_type.as_str(), "text" | "conversation"))
        .filter(|n| !geometry::hidden_by_collapsed(&session.data.nodes, &n.id))
        // 占位卡没有正文浮层：不过滤会让浮层命中吞掉整张卡上的按下与滚轮
        .filter(|n| !crate::paint::compact_card(session.view.zoom, display_size(n).0 as f32))
        .map(|n| {
            let (w, h) = display_size(n);
            let (sx, sy) = session.view.screen(n.x, n.y);
            let zoom = session.view.zoom;
            (
                n.id.clone(),
                Rect::new(
                    sx + 10.0 * zoom,
                    TOOLBAR_H + sy + 40.0 * zoom,
                    (w as f32 - 20.0) * zoom,
                    (h as f32 - 50.0) * zoom,
                ),
            )
        })
        .collect()
}

/// 右键菜单：按命中与选中构造宿主菜单（条目 id 由 lib 分发）。
fn context_menu(session: &mut Session, world: (f64, f64)) -> Option<MenuOpenParams> {
    let hit = hit_card(&session.data, world).map(|index| session.data.nodes[index].clone());
    if let Some(node) = &hit {
        if !session.view.selection.iter().any(|id| id == &node.id) {
            session.view.selection = vec![node.id.clone()];
        }
    }
    let action = |id: &str, label: &str, icon: &str, danger: bool| MenuItem::Action {
        id: format!("canvas-menu-{id}"),
        label: label.to_string(),
        icon: Some(icon.to_string()),
        danger,
        disabled: false,
        note: None,
        hint: None,
        pane: 0,
    };
    let (items, width) = if hit.is_some() && session.view.selection.len() > 1 {
        (
            vec![
                action("copy", "复制", "copy", false),
                action("group-create", "创建分组", "layers", false),
                MenuItem::Divider { pane: 0 },
                action("delete", "删除", "trash-2", true),
            ],
            200.0,
        )
    } else if let Some(node) = &hit {
        if node.node_type == "group" {
            let current = node
                .data
                .get("color")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("1");
            let mut items = vec![MenuItem::Submenu {
                label: "颜色".to_string(),
                icon: Some("palette".to_string()),
                disabled: false,
                pane: 0,
                target: 1,
            }];
            for index in 1..=6 {
                let label = if current == index.to_string() {
                    format!("色 {index}（当前）")
                } else {
                    format!("色 {index}")
                };
                let item = MenuItem::Action {
                    id: format!("canvas-menu-color-{index}"),
                    label,
                    icon: Some("circle".to_string()),
                    danger: false,
                    disabled: false,
                    note: None,
                    hint: None,
                    pane: 1,
                };
                items.push(item);
            }
            items.push(action("group-rename", "重命名", "pencil", false));
            let collapsed = node
                .data
                .get("collapsed")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            items.push(if collapsed {
                action("group-expand", "展开", "chevrons-down-up", false)
            } else {
                action("group-collapse", "收起", "chevrons-up-down", false)
            });
            items.push(MenuItem::Divider { pane: 0 });
            items.push(action("delete", "删除分组", "trash-2", true));
            (items, 200.0)
        } else {
            let mut items = vec![action("copy", "复制", "copy", false)];
            let file = node.data.get("file").and_then(serde_json::Value::as_str);
            if file.is_some() && node.node_type == "table" {
                items.push(action("open", "打开表格", "external-link", false));
            } else if file.is_some() && node.node_type == "text" {
                items.push(action("open", "打开笔记", "external-link", false));
            }
            items.push(MenuItem::Divider { pane: 0 });
            items.push(action("delete", "删除", "trash-2", true));
            (items, 200.0)
        }
    } else {
        let mut items = vec![
            action("new-conversation", "新建对话卡", "message-square", false),
            action("new-text", "新建文本卡", "align-left", false),
            action("new-group", "新建分组", "layers", false),
        ];
        if !session.view.selection.is_empty() {
            items.push(action("copy", "复制", "copy", false));
        }
        items.push(action("paste", "粘贴", "clipboard-paste", false));
        (items, 200.0)
    };
    Some(MenuOpenParams {
        items,
        width: Some(width),
        search: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use atelyx_format::canvas::{CANVAS_SCHEMA, CanvasFile, CanvasFileNode};

    fn session_with(nodes: Vec<CanvasFileNode>) -> Session {
        Session::new(
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
        )
    }

    fn text_card(id: &str, w: f64, h: f64) -> CanvasFileNode {
        CanvasFileNode {
            id: id.into(),
            node_type: "text".into(),
            x: 100.0,
            y: 100.0,
            width: Some(w),
            height: Some(h),
            data: serde_json::json!({}),
        }
    }

    fn wheel_at(session: &mut Session, x: f32, y: f32, size: (f32, f32)) {
        let mut mods = Modifiers::default();
        handle(
            session,
            &FrameInput::Wheel {
                x,
                y,
                delta_x: 0.0,
                delta_y: -40.0,
                modifiers: Modifiers::default(),
                hit: None,
            },
            size,
            &mut mods,
        );
    }

    fn minimap_center(size: (f32, f32)) -> (f32, f32) {
        (
            size.0 - MINIMAP_W - MINIMAP_MARGIN + MINIMAP_W / 2.0,
            size.1 - MINIMAP_H - MINIMAP_MARGIN + MINIMAP_H / 2.0,
        )
    }

    /// 小地图矩形内滚轮一律消费：空画布取不到映射也不能透传成画布缩放。
    #[test]
    fn wheel_over_minimap_never_zooms_empty_canvas() {
        let size = (900.0, 600.0);
        let mut session = session_with(Vec::new());
        let before = session.view.zoom;
        let (x, y) = minimap_center(size);
        wheel_at(&mut session, x, y, size);
        assert_eq!(session.view.zoom, before, "小地图上的滚轮不应缩放");
    }

    /// 小地图外的滚轮仍然缩放。
    #[test]
    fn wheel_outside_minimap_zooms() {
        let size = (900.0, 600.0);
        let mut session = session_with(Vec::new());
        let before = session.view.zoom;
        wheel_at(&mut session, 600.0, 300.0, size);
        assert!(session.view.zoom > before, "空白区滚轮应缩放");
    }

    /// 占位卡没有正文浮层：正文矩形集合必须剔除它，否则命中会吞掉卡上的按下与滚轮。
    #[test]
    fn card_bodies_skip_compact_cards() {
        let card = text_card("n1", 360.0, 240.0);
        let mut session = session_with(vec![card]);
        session.view.zoom = 0.3;
        assert!(card_bodies(&session).is_empty(), "占位卡不应有正文浮层");
        session.view.zoom = 0.5;
        assert_eq!(card_bodies(&session).len(), 1, "非占位卡保留正文浮层");
    }
}
