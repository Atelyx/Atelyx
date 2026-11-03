//! 原始输入 → 表格交互：按压命中、框选、列宽 / 行高拖拽、行排序、滚动条拖拽、滚轮与缩放、
//! 键盘导航与编辑快捷键。所有状态落在 `Session.view`，返回「要不要重绘 / 排保存 / 开菜单」。

use std::time::Instant;

use atelyx_format::table::ops;
use atelyx_format::table::ops::TableSelection;
use atelyx_plugin_protocol::frame::{FrameInput, Key, Modifiers, MouseButton, NamedKey};

use crate::edit;
use crate::geom;
use crate::model::{
    Hit, PressKind, PressState, RowReorder, Session, HScrollDrag, VScrollDrag, DOUBLE_CLICK_MS,
    MAX_ZOOM, MIN_ZOOM,
};

/// 图片格的干净点按容差：位移超过它就不算点按，不打开灯箱。
const IMAGE_TAP_SLOP: f32 = 6.0;

/// 一次输入处理的结果。
#[derive(Default)]
pub struct Outcome {
    /// 需要重绘。
    pub paint: bool,
    /// 数据有变化，需要排一次防抖保存。
    pub dirty: bool,
    /// 选区变化（需要广播给宿主）。
    pub selection: bool,
    /// 剪贴板动作（由入口层用宿主能力执行）。
    pub clipboard: Option<ClipboardAction>,
    /// 图片导入请求（格位）：入口层弹选图对话框并落盘。
    pub image_import: Option<(String, String)>,
    /// 取色请求（`true` = 底色 / `false` = 字色）。
    pub pick_color: Option<bool>,
    /// 编辑态或脏标记变化：重发面板头部（在途草稿也计入未落盘）。
    pub header: bool,
    /// 手动保存请求（Ctrl+S）。
    pub manual_save: bool,
    /// 灯箱当前图的仓库路径：入口层据此探明能否载入并收起加载占位。
    pub lightbox_image: Option<String>,
}

/// 需要宿主剪贴板的动作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipboardAction {
    Copy,
    Cut,
    Paste,
}

/// 右键菜单请求（由入口层用宿主菜单渲染）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MenuRequest {
    Cell { row: String, field: String },
    Field { field: String },
    Column { field: String },
    Row { row: String },
    Status { field: String },
    Select { row: String, field: String },
    /// 整表：列宽 / 行高自适应与格式
    All,
    /// 样式：字体清单
    StyleFont,
    /// 样式：字号清单
    StyleSize,
    /// 灯箱：复制 / 下载当前图
    Lightbox,
    /// 字段改名（浮层输入）
    RenameField { field_id: String },
    /// 左 / 右插列（浮层输入）
    InsertColumn { field_id: String, side: i32 },
    /// 添加字段（名称 + 类型）
    AddField,
    /// 单选选项编辑
    EditOptions { field_id: String },
}

/// 输入分发：键盘 / 指针 / 滚轮（灯箱打开时优先给灯箱）。
pub fn handle(session: &mut Session, input: &FrameInput) -> (Outcome, Option<MenuRequest>) {
    let mut outcome = Outcome::default();
    let mut menu = None;
    // 指针位置随每次移动更新：远端名签只在悬停其选区时出现，离开面即收起
    if let FrameInput::PointerDown { x, y, .. } | FrameInput::PointerMove { x, y, .. } = input {
        session.view.pointer = Some(crate::model::Point::new(*x, *y));
    } else if matches!(input, FrameInput::PointerLeave) {
        session.view.pointer = None;
        outcome.paint = true;
    }
    if session.view.lightbox.is_some() {
        menu = lightbox_input(session, input, &mut outcome);
        return (outcome, menu);
    }
    if session.view.prompt.is_some() {
        prompt_input(session, input, &mut outcome);
        return (outcome, None);
    }
    match input {
        FrameInput::PointerDown { x, y, button, .. } => {
            session.view.caret_on = true;
            press(session, *x, *y, *button, &mut outcome, &mut menu);
        }
        FrameInput::PointerMove { x, y, .. } => {
            move_cursor(session, *x, *y, &mut outcome);
        }
        FrameInput::PointerUp { x, y: _, .. } => {
            release(session, *x, &mut outcome);
        }
        FrameInput::DoubleClick { x, y, button, .. } if *button == MouseButton::Left => {
            double_click(session, *x, *y, &mut outcome);
        }
        FrameInput::Wheel { delta_x, delta_y, modifiers, .. } => {
            session.view.mods = *modifiers;
            if modifiers.control || modifiers.meta {
                zoom_by_wheel(session, *delta_y, &mut outcome);
            } else {
                wheel(session, *delta_x, *delta_y, &mut outcome);
            }
        }
        FrameInput::Key { key, action, modifiers, text } => {
            // 修饰态随时更新：滚轮缩放判定要用最近一次按键的修饰快照
            session.view.mods = *modifiers;
            if *action == atelyx_plugin_protocol::frame::KeyAction::Press {
                if session.view.editing.is_some() {
                    session.view.caret_on = true;
                }
                key_press(session, key, modifiers, text.as_deref(), &mut outcome);
            }
        }
        FrameInput::Text { text, composing, caret, .. } => {
            session.view.caret_on = true;
            if let Some(edit) = session.view.editing.as_mut() {
                // 组合串只改显示；值在提交时按实际变化置脏
                if edit.editor.apply_ime(text, *composing, *caret) {
                    session.rebuild_layout();
                    outcome.paint = true;
                }
            }
        }
        FrameInput::Focus { focused: false } => {
            // 点到面外：先收尾草稿，避免切文件 / 关面板时丢已输入内容
            session.view.editor_drag = false;
            if edit::commit(session) {
                outcome.dirty = true;
                outcome.header = true;
            }
            outcome.paint = true;
        }
        FrameInput::Outside { .. } => {
            session.view.editor_drag = false;
            if edit::commit(session) {
                outcome.dirty = true;
                outcome.header = true;
            }
            outcome.paint = true;
        }
        _ => {}
    }
    (outcome, menu)
}

/// 按压：先收尾在途编辑，再按命中分类处理。
fn press(
    session: &mut Session,
    x: f32,
    y: f32,
    button: MouseButton,
    outcome: &mut Outcome,
    menu: &mut Option<MenuRequest>,
) {
    // 点在自己正在编辑的格子里 = 编辑器内部交互：不能提交草稿（用户可能在点插入点）
    let inside_editor = session.view.editing.as_ref().is_some_and(|editing| {
        geom::geo(
            &session.view.layout,
            session.view.scroll_x,
            session.view.scroll_y,
            session.view.zoom,
            session.view.canvas_size,
        )
        .is_some_and(|geo| {
            matches!(
                geo.hit_at(x, y),
                Some(Hit::Cell { row, field }) if row == editing.row && field == editing.field
            )
        })
    });
    if inside_editor && button == MouseButton::Left {
        place_caret(session, x, y, outcome);
        return;
    }
    if edit::commit(session) {
        outcome.dirty = true;
        outcome.header = true;
    }
    // 气泡优先：点在气泡上即执行样式动作，不落到网格
    if format_bubble_press(session, x, y, outcome, menu) {
        return;
    }
    session.view.bar_shown = false;
    let zoom = session.view.zoom;
    let Some(geo) = geom::geo(&session.view.layout, session.view.scroll_x, session.view.scroll_y, session.view.zoom, session.view.canvas_size) else {
        return;
    };
    let hit = geo.hit_at(x, y);
    let now = Instant::now();
    let double = session
        .view
        .last_press
        .take()
        .is_some_and(|(at, previous)| {
            now.duration_since(at).as_millis() < DOUBLE_CLICK_MS
                && (previous.x - x).abs() < 4.0
                && (previous.y - y).abs() < 4.0
        });
    // 只有左键参与双击判定：右键开菜单不能被下一次左键误判成双击
    session.view.last_press = (button == MouseButton::Left)
        .then_some((now, crate::model::Point::new(x, y)));
    let Some(hit) = hit else {
        session.view.selection = None;
        outcome.selection = true;
        outcome.paint = true;
        return;
    };
    match hit {
        Hit::Corner => {
            session.view.selection = Some(TableSelection::All);
            if button == MouseButton::Right {
                *menu = Some(MenuRequest::All);
            }
            outcome.selection = true;
        }
        Hit::Header { field } => {
            let Some(field_def) = session.data.fields.get(field) else { return };
            let field_id = field_def.id.clone();
            if button == MouseButton::Right {
                *menu = Some(MenuRequest::Column { field: field_id.clone() });
            }
            session.view.selection = Some(TableSelection::Column(field_id));
            outcome.selection = true;
        }
        Hit::HeaderMenu { field } => {
            if let Some(field_def) = session.data.fields.get(field) {
                *menu = Some(MenuRequest::Field { field: field_def.id.clone() });
            }
        }
        Hit::HeaderResize { field } => {
            if let Some(field_def) = session.data.fields.get(field) {
                let start_w =
                    field_def.width.unwrap_or_else(|| ops::field_default_width(&field_def.name));
                session.view.col_drag = Some(crate::model::ColDrag {
                    field_id: field_def.id.clone(),
                    start_w: start_w as f32,
                    start_x: x,
                    pushed: false,
                });
            }
        }
        Hit::AddFieldCol => {
            *menu = Some(MenuRequest::AddField);
        }
        Hit::Gutter { row } => {
            let Some(table_row) = session.data.rows.get(row) else { return };
            let row_id = table_row.id.clone();
            if button == MouseButton::Right {
                *menu = Some(MenuRequest::Row { row: row_id.clone() });
            }
            session.view.selection = Some(TableSelection::Row(row_id));
            outcome.selection = true;
        }
        Hit::GutterHandle { row } => {
            if let Some(table_row) = session.data.rows.get(row) {
                let natural = geo.row_h(row) / zoom;
                session.view.row_drag = Some(crate::model::RowDrag {
                    row_id: table_row.id.clone(),
                    start_y: y,
                    start_h: table_row.height.map(|height| height as f32).unwrap_or(natural),
                    pushed: false,
                });
            }
        }
        Hit::GutterGrip { row } => {
            if let Some(table_row) = session.data.rows.get(row) {
                session.view.selection = Some(TableSelection::Row(table_row.id.clone()));
                session.view.reorder = Some(RowReorder {
                    row_id: table_row.id.clone(),
                    target: row,
                    start_y: y,
                    moved: false,
                });
                outcome.selection = true;
            }
        }
        Hit::Cell { row, field } => {
            let Some(field_def) = session.data.fields.get(field) else { return };
            let field_id = field_def.id.clone();
            let field_type = field_def.field_type.clone();
            let Some(table_row) = session.data.rows.get(row) else { return };
            let row_id = table_row.id.clone();
            if field_type == "image" {
                if button == MouseButton::Right {
                    *menu = Some(MenuRequest::Cell { row: row_id, field: field_id });
                    outcome.selection = true;
                    return;
                }
                session.view.selection = Some(TableSelection::Cell {
                    row_id: row_id.clone(),
                    field_id: field_id.clone(),
                });
                outcome.selection = true;
                if double {
                    return;
                }
                let value = table_row.values.get(&field_id).cloned();
                let list = crate::images::entries(value.as_ref());
                let key = (table_row.id.clone(), field_id.clone());
                let current = session
                    .view
                    .carousel
                    .get(&key)
                    .copied()
                    .unwrap_or(0)
                    .min(list.len().saturating_sub(1));
                let rect = geo.cell_rect(row, field);
                let layout = crate::images::layout_of(
                    rect,
                    value.as_ref(),
                    session.view.zoom,
                    current,
                );
                let inside = |target: &Option<crate::model::Rect>| {
                    target.as_ref().is_some_and(|target| target.contains(x, y))
                };
                if inside(&layout.mode_btn) {
                    toggle_image_mode(session, &row_id, &field_id);
                    outcome.dirty = true;
                    outcome.paint = true;
                    return;
                }
                if inside(&layout.add_btn) || inside(&layout.empty_add) {
                    outcome.image_import = Some((row_id, field_id));
                    outcome.paint = true;
                    return;
                }
                if inside(&layout.remove_btn) {
                    if remove_image(session, &row_id, &field_id, current) {
                        outcome.dirty = true;
                        outcome.paint = true;
                    }
                    return;
                }
                for (index, thumb) in layout.thumbs.iter().chain(layout.tiles.iter()) {
                    if thumb.contains(x, y) {
                        session.view.carousel.insert(key.clone(), *index);
                        let grid = crate::images::display_mode(value.as_ref()) == "grid";
                        // 九宫格点图 = 直接开灯箱；轮播点缩略图 = 切主图
                        if grid {
                            session.view.lightbox =
                                Some(crate::model::Lightbox::new(row_id, field_id, *index));
                            note_lightbox_probe(session, outcome);
                        } else {
                            // 长按到点进拖拽重排：先记按压，帧拍里按时间判定
                            session.view.image_press = Some(crate::model::ImagePress {
                                row_id: row_id.clone(),
                                field_id: field_id.clone(),
                                index: *index,
                                at: std::time::Instant::now(),
                                start: crate::model::Point::new(x, y),
                                moved: false,
                            });
                        }
                        outcome.paint = true;
                        return;
                    }
                }
                if let Some(main) = layout.main {
                    if main.contains(x, y) && !list.is_empty() {
                        session.view.pending_image_open = Some((row_id, field_id, current));
                        session.view.swipe = Some(crate::model::SwipeState {
                            start: crate::model::Point::new(x, y),
                            moved: false,
                            dx: 0.0,
                        });
                    }
                }
                outcome.paint = true;
                return;
            }
            if field_type == "singleSelect" {
                session.view.selection = Some(TableSelection::Cell { row_id: row_id.clone(), field_id: field_id.clone() });
                *menu = Some(MenuRequest::Select { row: row_id, field: field_id });
                outcome.selection = true;
                return;
            }
            if button == MouseButton::Right {
                let keep = ops::region_of(
                    session.view.selection.as_ref(),
                    &session.data.fields,
                    &session.data.rows,
                )
                .is_some_and(|region| region.contains(row, field));
                if !keep {
                    session.view.selection =
                        Some(TableSelection::Cell { row_id: row_id.clone(), field_id: field_id.clone() });
                }
                *menu = Some(MenuRequest::Cell { row: row_id, field: field_id });
                outcome.selection = true;
                return;
            }
            if double {
                edit::begin(session, row, field, None);
                outcome.header = true;
                outcome.paint = true;
                return;
            }
            // 再点选区内部（含多格 / 整行 / 整列）弹格式气泡
            let already = ops::region_of(
                session.view.selection.as_ref(),
                &session.data.fields,
                &session.data.rows,
            )
            .is_some_and(|region| region.contains(row, field));
            session.view.selection = Some(TableSelection::Cell { row_id, field_id });
            session.view.bar_shown = already;
            session.view.press = Some(PressState {
                origin: (row, field),
                pos: crate::model::Point::new(x, y),
                kind: PressKind::Cell,
            });
            outcome.selection = true;
        }
        Hit::Status { field } => {
            if let Some(field_def) = session.data.fields.get(field) {
                *menu = Some(MenuRequest::Status { field: field_def.id.clone() });
            }
        }
        Hit::NewRow => {
            push_undo(session);
            session.data.rows.push(atelyx_format::table::TableRow {
                id: nanoid::nanoid!(),
                values: serde_json::Map::new(),
                height: None,
                styles: None,
            });
            session.touch();
            outcome.dirty = true;
        }
        Hit::VScrollbar => {
            let (start, len, travel) = geo.v_thumb();
            let local = y - geo.grid_top();
            let grab = if local >= start && local <= start + len {
                local - start
            } else {
                let top = (local - len / 2.0).clamp(0.0, travel);
                session.view.scroll_y =
                    if travel > 0.0 { top / travel * geo.max_scroll_y() } else { 0.0 };
                len / 2.0
            };
            session.view.vscroll_drag = Some(VScrollDrag { grab });
        }
        Hit::HScrollbar => {
            let (start, len, travel) = geo.h_thumb();
            let local = x - geo.gutter_w();
            let grab = if local >= start && local <= start + len {
                local - start
            } else {
                let left = (local - len / 2.0).clamp(0.0, travel);
                session.view.scroll_x =
                    if travel > 0.0 { left / travel * geo.max_scroll_x() } else { 0.0 };
                len / 2.0
            };
            session.view.hscroll_drag = Some(HScrollDrag { grab });
        }
    }
    outcome.paint = true;
}

/// 编辑格内落点 → 显示文本字节位（与绘制同一份行段与字宽估算）。
fn caret_at_point(session: &Session, x: f32, y: f32) -> Option<usize> {
    let geo = geom::geo(
        &session.view.layout,
        session.view.scroll_x,
        session.view.scroll_y,
        session.view.zoom,
        session.view.canvas_size,
    )?;
    let edit = session.view.editing.as_ref()?;
    let (Some(field), Some(table_row)) = (
        session.data.fields.get(edit.field),
        session.data.rows.get(edit.row),
    ) else {
        return None;
    };
    let cell = geo.cell_rect(edit.row, edit.field);
    let zoom = session.view.zoom;
    let style = ops::style_of(table_row, &field.id);
    let font = crate::text::FontSpec::of(&style, field, zoom);
    let pad_x = crate::model::CELL_PAD_X * zoom;
    let pad_y = if field.field_type == "text" {
        crate::model::CELL_PAD_Y * zoom
    } else {
        ((cell.h.max(24.0) - font.size * crate::text::LINE_FACTOR) / 2.0).max(0.0)
    };
    let wrap_w = (cell.w.max(40.0) - 2.0 * pad_x).max(8.0);
    Some(crate::text::caret_at(
        &edit.editor.text,
        font.size,
        wrap_w,
        x - (cell.x + pad_x),
        y - (cell.y + pad_y),
    ))
}

/// 编辑格内按下：收尾输入法组合并把光标与锚点落到按压点，进入拖选。
fn place_caret(session: &mut Session, x: f32, y: f32, outcome: &mut Outcome) {
    let Some(edit) = session.view.editing.as_mut() else { return };
    edit.editor.finish_preedit();
    let Some(caret) = caret_at_point(session, x, y) else { return };
    if let Some(edit) = session.view.editing.as_mut() {
        edit.editor.caret = caret;
        edit.editor.anchor = caret;
    }
    session.view.caret_on = true;
    session.view.editor_drag = true;
    outcome.paint = true;
}

/// 编辑格内拖选：锚点保持按下时落点，焦点随指针移动。
fn drag_caret(session: &mut Session, x: f32, y: f32, outcome: &mut Outcome) {
    let Some(caret) = caret_at_point(session, x, y) else { return };
    if let Some(edit) = session.view.editing.as_mut() {
        edit.editor.caret = caret;
    }
    session.view.caret_on = true;
    outcome.paint = true;
}

/// 移动：悬停 / 框选 / 各类拖拽。
fn move_cursor(session: &mut Session, x: f32, y: f32, outcome: &mut Outcome) {
    session.view.hover = None;
    // 编辑格内按住拖动 = 扩展文本选区（锚点保持按下落点）
    if session.view.editor_drag {
        if session.view.editing.is_some() {
            drag_caret(session, x, y, outcome);
            return;
        }
        session.view.editor_drag = false;
    }
    // 缩略图按压：移动超过阈值即取消长按判定
    if let Some(press) = session.view.image_press.as_mut() {
        let dx = press.start.x - x;
        let dy = press.start.y - y;
        if (dx * dx + dy * dy).sqrt() > crate::images::IMAGE_PRESS_CANCEL * session.view.zoom {
            press.moved = true;
        }
    }
    // 缩略图拖拽重排：落点按当前指针命中的缩略图 / 图块更新
    if session.view.img_drag.is_some() {
        let drag_row = session
            .view
            .img_drag
            .as_ref()
            .map(|drag| (drag.row_id.clone(), drag.field_id.clone()));
        if let Some((row_id, field_id)) = drag_row {
            let row = session.data.rows.iter().position(|row| row.id == row_id);
            let field = session.data.fields.iter().position(|field| field.id == field_id);
            let value = session
                .data
                .rows
                .iter()
                .find(|row| row.id == row_id)
                .and_then(|row| row.values.get(&field_id))
                .cloned();
            if let (Some(row), Some(field)) = (row, field) {
                if let Some(geo) = crate::geom::geo(
                    &session.view.layout,
                    session.view.scroll_x,
                    session.view.scroll_y,
                    session.view.zoom,
                    session.view.canvas_size,
                ) {
                    let rect = geo.cell_rect(row, field);
                    if let Some(drag) = session.view.img_drag.as_ref() {
                        let current = drag.to;
                        let layout = crate::images::layout_of(
                            rect,
                            value.as_ref(),
                            session.view.zoom,
                            current,
                        );
                        let mut target = current;
                        for (index, thumb) in
                            layout.thumbs.iter().chain(layout.tiles.iter())
                        {
                            if thumb.contains(x, y) {
                                target = *index;
                            }
                        }
                        if let Some(drag) = session.view.img_drag.as_mut() {
                            drag.to = target;
                            drag.pos = crate::model::Point::new(x, y);
                        }
                    }
                }
            }
        }
        outcome.paint = true;
    }
    // 轮播横向滑动：记录跟手位移，超过阈值即标记，松手时翻页
    if let Some(swipe) = session.view.swipe.as_mut() {
        swipe.dx = x - swipe.start.x;
        if swipe.dx.abs() >= crate::images::IMAGE_SWIPE_THRESHOLD * session.view.zoom {
            swipe.moved = true;
        }
    }
    let Some(geo) = geom::geo(&session.view.layout, session.view.scroll_x, session.view.scroll_y, session.view.zoom, session.view.canvas_size) else { return };
    let hit = geo.hit_at(x, y);
    session.view.hover = hit;
    if session.view.col_drag.is_some() {
        let (field_id, pushed, width) = {
            let drag = session.view.col_drag.as_ref().expect("已判存在");
            let delta = (x - drag.start_x) / session.view.zoom;
            let width = (drag.start_w + delta)
                .clamp(ops::MIN_COL_WIDTH as f32, ops::MAX_COL_WIDTH as f32);
            (drag.field_id.clone(), drag.pushed, width)
        };
        if pushed
            || width.to_bits() != session.view.col_drag.as_ref().expect("已判存在").start_w.to_bits()
        {
            if !pushed {
                push_undo(session);
                if let Some(drag) = session.view.col_drag.as_mut() {
                    drag.pushed = true;
                }
            }
            if let Some(field_def) = session.data.fields.iter_mut().find(|field| field.id == field_id)
            {
                field_def.width = Some(f64::from(width));
            }
            session.rebuild_layout();
        }
        outcome.paint = true;
        return;
    }
    if session.view.row_drag.is_some() {
        let (row_id, pushed, height) = {
            let drag = session.view.row_drag.as_ref().expect("已判存在");
            let delta = (y - drag.start_y) / session.view.zoom;
            let height = (drag.start_h + delta)
                .clamp(ops::MIN_ROW_HEIGHT as f32, ops::MAX_ROW_HEIGHT as f32);
            (drag.row_id.clone(), drag.pushed, height)
        };
        if pushed
            || height.to_bits() != session.view.row_drag.as_ref().expect("已判存在").start_h.to_bits()
        {
            if !pushed {
                push_undo(session);
                if let Some(drag) = session.view.row_drag.as_mut() {
                    drag.pushed = true;
                }
            }
            if let Some(table_row) = session.data.rows.iter_mut().find(|row| row.id == row_id) {
                table_row.height = Some(f64::from(height));
            }
            session.rebuild_layout();
        }
        outcome.paint = true;
        return;
    }
    if let Some(drag) = &session.view.vscroll_drag {
        let (_start, _len, travel) = geo.v_thumb();
        let top = (y - drag.grab).clamp(0.0, travel);
        session.view.scroll_y = if travel > 0.0 { top / travel * geo.max_scroll_y() } else { 0.0 };
        outcome.paint = true;
        return;
    }
    if let Some(drag) = &session.view.hscroll_drag {
        let (_start, _len, travel) = geo.h_thumb();
        let local = x - geo.gutter_w();
        let left = (local - drag.grab).clamp(0.0, travel);
        session.view.scroll_x = if travel > 0.0 { left / travel * geo.max_scroll_x() } else { 0.0 };
        outcome.paint = true;
        return;
    }
    // 框选：按压落在格上并移出 5px 后开始扩展选区
    if let Some(press) = session.view.press.as_ref() {
        if press.kind == PressKind::Cell {
            let (row, field) = press.origin;
            let distance = ((press.pos.x - x).powi(2) + (press.pos.y - y).powi(2)).sqrt();
            if distance > crate::model::CELL_PAD_X * 2.0 {
                if let Some(Hit::Cell { row: target_row, field: target_field }) = hit {
                    let (anchor_row, anchor_field) = (row, field);
                    if let (Some(anchor), Some(target)) = (
                        session.data.rows.get(anchor_row),
                        session.data.rows.get(target_row),
                    ) {
                        let anchor_row_id = anchor.id.clone();
                        let target_row_id = target.id.clone();
                        let anchor_field_id = session
                            .data
                            .fields
                            .get(anchor_field)
                            .map(|field| field.id.clone());
                        let target_field_id = session
                            .data
                            .fields
                            .get(target_field)
                            .map(|field| field.id.clone());
                        if let (Some(anchor_field_id), Some(target_field_id)) =
                            (anchor_field_id, target_field_id)
                        {
                            session.view.selection = Some(TableSelection::Range {
                                anchor_row_id,
                                anchor_field_id,
                                row_id: target_row_id,
                                field_id: target_field_id,
                            });
                            session.view.bar_shown = false;
                            outcome.selection = true;
                            outcome.paint = true;
                        }
                    }
                }
            }
        }
    }
    // 行排序：拖到目标行（表尾 = 追加）
    if let Some(reorder) = session.view.reorder.as_mut() {
        if (y - reorder.start_y).abs() >= 5.0 * session.view.zoom {
            reorder.moved = true;
        }
    }
    if session.view.reorder.as_ref().is_some_and(|reorder| reorder.moved) {
        if let Some(target) = reorder_target(&geo, session, y) {
            if let Some(updated) = session.view.reorder.as_mut() {
                updated.target = target;
            }
            outcome.paint = true;
        }
    }
    outcome.paint = true;
}

/// 行拖拽落点：按行中线判定插入位置，越过末行 = 追加到表尾。
fn reorder_target(geo: &geom::Geo<'_>, session: &Session, y: f32) -> Option<usize> {
    let count = session.data.rows.len();
    for row in 0..count {
        let rect = geo.cell_rect(row, 0);
        if y < rect.y + rect.h / 2.0 {
            return Some(row);
        }
    }
    Some(count)
}

/// 松开：结算各类拖拽。
fn release(session: &mut Session, x: f32, outcome: &mut Outcome) {
    session.view.hover = None;
    session.view.editor_drag = false;
    session.view.image_press = None;
    if let Some(drag) = session.view.img_drag.take() {
        if commit_image_drag(session, drag) {
            outcome.dirty = true;
        }
        outcome.paint = true;
        return;
    }
    if let Some(swipe) = session.view.swipe.take() {
        if swipe.moved {
            if let Some((row_id, field_id, index)) = session.view.pending_image_open.take() {
                let count = session
                    .data
                    .rows
                    .iter()
                    .find(|row| row.id == row_id)
                    .and_then(|row| row.values.get(&field_id))
                    .map(|value| crate::images::entries(Some(value)).len())
                    .unwrap_or(0);
                let threshold = crate::images::IMAGE_SWIPE_THRESHOLD * session.view.zoom;
                let next = if x < swipe.start.x - threshold {
                    (index + 1).min(count.saturating_sub(1))
                } else {
                    index.saturating_sub(1)
                };
                session.view.carousel.insert((row_id, field_id), next);
                outcome.paint = true;
                return;
            }
        }
        // 拖过一点但没到翻页阈值：算拖动而非点按，不能顺手打开灯箱
        if swipe.dx.abs() > IMAGE_TAP_SLOP * session.view.zoom {
            session.view.pending_image_open = None;
            outcome.paint = true;
            return;
        }
    }
    if let Some((row_id, field_id, index)) = session.view.pending_image_open.take() {
        session.view.lightbox = Some(crate::model::Lightbox::new(row_id, field_id, index));
        note_lightbox_probe(session, outcome);
        outcome.paint = true;
        return;
    }
    if session.view.vscroll_drag.take().is_some() || session.view.hscroll_drag.take().is_some() {
        outcome.paint = true;
        return;
    }
    if let Some(drag) = session.view.col_drag.take() {
        if drag.pushed {
            session.rebuild_layout();
            session.touch();
            outcome.dirty = true;
        }
        outcome.paint = true;
        return;
    }
    if let Some(drag) = session.view.row_drag.take() {
        if drag.pushed {
            session.rebuild_layout();
            session.touch();
            outcome.dirty = true;
        }
        outcome.paint = true;
        return;
    }
    if let Some(reorder) = session.view.reorder.take() {
        // 位移未超过阈值 = 只是点了把手：不产生撤销单元
        if !reorder.moved {
            outcome.paint = true;
            return;
        }
        push_undo(session);
        if ops::move_row(&mut session.data, &reorder.row_id, reorder.target) {
            session.touch();
            outcome.dirty = true;
        } else {
            session.view.undo.pop();
        }
        outcome.paint = true;
        return;
    }
    let Some(press) = session.view.press.take() else { return };
    if press.kind == PressKind::Cell {
        outcome.paint = true;
    }
}

/// 双击：编辑格内选词；文本类格进编辑。
fn double_click(session: &mut Session, x: f32, y: f32, outcome: &mut Outcome) {
    // 已在编辑格内双击 = 选中落点处的词，并接着可拖选（重开编辑会丢掉草稿）
    if session.view.editing.is_some() {
        if let Some(edit) = session.view.editing.as_mut() {
            edit.editor.finish_preedit();
        }
        if let Some(caret) = caret_at_point(session, x, y) {
            if let Some(edit) = session.view.editing.as_mut() {
                let (start, end) = crate::editor::word_range(&edit.editor.text, caret);
                edit.editor.anchor = start;
                edit.editor.caret = end;
            }
            session.view.editor_drag = true;
            session.view.caret_on = true;
            outcome.paint = true;
        }
        return;
    }
    let Some(geo) = geom::geo(&session.view.layout, session.view.scroll_x, session.view.scroll_y, session.view.zoom, session.view.canvas_size) else { return };
    if let Some(Hit::Cell { row, field }) = geo.hit_at(x, y) {
        if let Some(field_def) = session.data.fields.get(field) {
            if matches!(field_def.field_type.as_str(), "text" | "number" | "duration") {
                edit::begin(session, row, field, None);
                outcome.header = true;
                outcome.paint = true;
            }
        }
    }
}

/// 滚轮：Shift / Alt 横滚；其余纵向。`delta` 已按协议（正 = 向下 / 向右）。
fn wheel(session: &mut Session, delta_x: f32, delta_y: f32, outcome: &mut Outcome) {
    let Some(geo) = geom::geo(&session.view.layout, session.view.scroll_x, session.view.scroll_y, session.view.zoom, session.view.canvas_size) else { return };
    let (max_x, max_y) = (geo.max_scroll_x(), geo.max_scroll_y());
    let (dx, dy) = if session.view.mods.shift || session.view.mods.alt {
        (delta_x + delta_y, 0.0)
    } else {
        (delta_x, delta_y)
    };
    session.view.scroll_y = (session.view.scroll_y + dy).clamp(0.0, max_y);
    session.view.scroll_x = (session.view.scroll_x + dx).clamp(0.0, max_x);
    outcome.paint = true;
}

/// Ctrl+滚轮缩放（`delta_y` 正 = 向下 = 缩小）。
pub fn zoom_by_wheel(session: &mut Session, delta_y: f32, outcome: &mut Outcome) {
    let zoom = (session.view.zoom * (-delta_y * 0.0015f32).exp()).clamp(MIN_ZOOM, MAX_ZOOM);
    session.view.zoom = (zoom * 100.0).round() / 100.0;
    session.rebuild_layout();
    geom::clamp_scroll(session);
    outcome.paint = true;
}

/// 键盘：编辑态优先，其次是表格级快捷键。
fn key_press(
    session: &mut Session,
    key: &Key,
    modifiers: &Modifiers,
    text: Option<&str>,
    outcome: &mut Outcome,
) {
    if session.view.editing.is_some() {
        // 编辑框内的复制 / 剪切 / 粘贴交给宿主剪贴板，文本操作在入口层做
        if let Key::Character { text: typed } = key {
            let control = modifiers.control || modifiers.meta;
            if control && typed.eq_ignore_ascii_case("s") {
                outcome.manual_save = true;
                return;
            }
            if control && typed.eq_ignore_ascii_case("c") {
                outcome.clipboard = Some(ClipboardAction::Copy);
                return;
            }
            if control && typed.eq_ignore_ascii_case("x") {
                outcome.clipboard = Some(ClipboardAction::Cut);
                return;
            }
            if control && typed.eq_ignore_ascii_case("v") {
                outcome.clipboard = Some(ClipboardAction::Paste);
                return;
            }
        }
        let multiline = session
            .view
            .editing
            .as_ref()
            .map(|edit| edit::multiline(session, edit))
            .unwrap_or(false);
        let result = session
            .view
            .editing
            .as_mut()
            .map(|edit| edit.editor.apply_key(key, modifiers, multiline))
            .unwrap_or_default();
        outcome.header = true;
        if result.cancel {
            edit::cancel(session);
            session.rebuild_layout();
        } else if result.commit {
            let has_next = selection_cell(session)
                .is_some_and(|(row, _)| row + 1 < session.data.rows.len());
            if edit::commit(session) {
                outcome.dirty = true;
            }
            session.rebuild_layout();
            // 与是否改值无关：录入节奏只取决于下方还有没有行
            if has_next {
                move_selection(session, NavDir::Down);
            }
        } else if result.edited {
            session.rebuild_layout();
        }
        outcome.paint = true;
        return;
    }
    let control = modifiers.control || modifiers.meta;
    if control && matches!(key, Key::Character { text } if text.eq_ignore_ascii_case("s")) {
        outcome.manual_save = true;
        return;
    }
    match key {
        Key::Named { name } => match name {
            NamedKey::ArrowUp => move_selection(session, NavDir::Up),
            NamedKey::ArrowDown => move_selection(session, NavDir::Down),
            NamedKey::ArrowLeft => move_selection(session, NavDir::Left),
            NamedKey::ArrowRight => move_selection(session, NavDir::Right),
            NamedKey::Home => move_selection(session, NavDir::Home),
            NamedKey::End => move_selection(session, NavDir::End),
            NamedKey::Enter => {
                // 只有可直接输入的字段才起稿：图片 / 单选走各自交互，避免把字符串写进图片值
                if let Some((row, field)) = selection_cell(session) {
                    let typeable = session
                        .data
                        .fields
                        .get(field)
                        .is_some_and(crate::text::can_type_into);
                    if typeable {
                        edit::begin(session, row, field, None);
                        outcome.header = true;
                    }
                }
            }
            NamedKey::Escape => {
                session.view.bar_shown = false;
                session.view.selection = None;
                outcome.selection = true;
            }
            NamedKey::Delete | NamedKey::Backspace => {
                if clear_selection(session) {
                    outcome.dirty = true;
                }
            }
            _ => {}
        },
        Key::Character { text: typed } => {
            if control && typed.eq_ignore_ascii_case("z") {
                let changed = if modifiers.shift { redo(session) } else { undo(session) };
                if changed {
                    outcome.dirty = true;
                    outcome.paint = true;
                }
                return;
            }
            if control && typed.eq_ignore_ascii_case("y") {
                if redo(session) {
                    outcome.dirty = true;
                    outcome.paint = true;
                }
                return;
            }
            if control && typed.eq_ignore_ascii_case("c") {
                outcome.clipboard = Some(ClipboardAction::Copy);
                return;
            }
            if control && typed.eq_ignore_ascii_case("x") {
                outcome.clipboard = Some(ClipboardAction::Cut);
                return;
            }
            if control && typed.eq_ignore_ascii_case("v") {
                outcome.clipboard = Some(ClipboardAction::Paste);
                return;
            }
            if control && typed.eq_ignore_ascii_case("a") {
                session.view.selection = Some(TableSelection::All);
                outcome.selection = true;
                outcome.paint = true;
                return;
            }
            if control {
                return;
            }
            // 选中格直接敲字符：覆盖原值起稿
            let Some((row, field)) = selection_cell(session) else { return };
            let Some(field_def) = session.data.fields.get(field) else { return };
            if !crate::text::can_type_into(field_def) {
                return;
            }
            let seed = text.map(str::to_string).unwrap_or_else(|| typed.clone());
            edit::begin(session, row, field, Some(seed));
            session.view.editing.as_mut().map(|edit| {
                edit.editor.touched = true;
            });
            outcome.header = true;
            outcome.paint = true;
        }
    }
}

/// 选区移动方向。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavDir {
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
}

/// 当前选区为单格时返回其下标（区域 / 整行 / 整列 = None，与旧键盘语义一致）。
pub fn selection_cell(session: &Session) -> Option<(usize, usize)> {
    match session.view.selection.as_ref()? {
        TableSelection::Cell { row_id, field_id } => Some((
            session.data.rows.iter().position(|row| row.id == *row_id)?,
            session.data.fields.iter().position(|field| field.id == *field_id)?,
        )),
        _ => None,
    }
}

/// 键盘移动选区（仅单格选区响应；不滚动，与旧导航一致）。
fn move_selection(session: &mut Session, dir: NavDir) {
    let Some((row, field)) = selection_cell(session) else { return };
    let last_row = session.data.rows.len().saturating_sub(1);
    let last_field = session.data.fields.len().saturating_sub(1);
    let (row, field) = match dir {
        NavDir::Up => (row.saturating_sub(1), field),
        NavDir::Down => ((row + 1).min(last_row), field),
        NavDir::Left => (row, field.saturating_sub(1)),
        NavDir::Right => (row, (field + 1).min(last_field)),
        NavDir::Home => (row, 0),
        NavDir::End => (row, last_field),
    };
    let (Some(table_row), Some(field_def)) =
        (session.data.rows.get(row), session.data.fields.get(field))
    else {
        return;
    };
    session.view.selection = Some(TableSelection::Cell {
        row_id: table_row.id.clone(),
        field_id: field_def.id.clone(),
    });
    session.view.bar_shown = false;
}

/// 键盘把选区移进视口。
pub fn scroll_into_view(session: &mut Session, row: usize, field: usize) {
    let Some(geo) = geom::geo(&session.view.layout, session.view.scroll_x, session.view.scroll_y, session.view.zoom, session.view.canvas_size) else { return };
    let rect = geo.cell_rect(row, field);
    if rect.y < geo.grid_top() {
        session.view.scroll_y = (session.view.scroll_y - (geo.grid_top() - rect.y)).max(0.0);
    } else if rect.bottom() > geo.grid_top() + geo.viewport_h() {
        session.view.scroll_y += rect.bottom() - (geo.grid_top() + geo.viewport_h());
    }
    let left = geo.grid_left();
    if rect.x < left {
        session.view.scroll_x = (session.view.scroll_x - (left - rect.x)).max(0.0);
    } else if rect.right() > left + geo.viewport_w() {
        session.view.scroll_x += rect.right() - (left + geo.viewport_w());
    }
    session.view.scroll_x = session.view.scroll_x.clamp(0.0, geo.max_scroll_x());
    session.view.scroll_y = session.view.scroll_y.clamp(0.0, geo.max_scroll_y());
}

/// 删行 / 删列后选区可能指向已不存在的实体：失效即收起。
pub fn clear_invalid_selection(session: &mut Session) {
    if ops::region_of(
        session.view.selection.as_ref(),
        &session.data.fields,
        &session.data.rows,
    )
    .is_none()
    {
        session.view.selection = None;
    }
}

/// 清空选中格的正文（图片格不动）。
fn clear_selection(session: &mut Session) -> bool {
    let Some(region) = ops::region_of(
        session.view.selection.as_ref(),
        &session.data.fields,
        &session.data.rows,
    ) else {
        return false;
    };
    push_undo(session);
    let mut changed = false;
    for row in region.row_start..=region.row_end {
        for field in region.col_start..=region.col_end {
            let Some(field_def) = session.data.fields.get(field) else { continue };
            if field_def.field_type == "image" {
                continue;
            }
            let field_id = field_def.id.clone();
            if let Some(table_row) = session.data.rows.get_mut(row) {
                changed |= ops::set_cell_value(table_row, &field_id, None);
            }
        }
    }
    if changed {
        session.touch();
    } else {
        session.view.undo.pop();
    }
    changed
}

/// 入撤销栈（数据变更前调用）。
pub fn push_undo(session: &mut Session) {
    session
        .view
        .undo
        .push((session.data.fields.clone(), session.data.rows.clone()));
    if session.view.undo.len() > crate::model::UNDO_DEPTH {
        session.view.undo.remove(0);
    }
    session.view.redo.clear();
}

/// 撤销：当前态压入重做栈，回退到栈顶快照。编辑会话中不动数据：
/// 提交草稿会先压入「编辑前快照」，紧接着弹栈会把用户刚敲的内容吞掉。
pub fn undo(session: &mut Session) -> bool {
    if session.view.editing.is_some() {
        return false;
    }
    let Some(snapshot) = session.view.undo.pop() else { return false };
    session
        .view
        .redo
        .push((session.data.fields.clone(), session.data.rows.clone()));
    session.data.fields = snapshot.0;
    session.data.rows = snapshot.1;
    session.view.bar_shown = false;
    session.mark_dirty();
    clear_invalid_selection(session);
    crate::geom::clamp_scroll(session);
    true
}

/// 重做：当前态压回撤销栈，恢复重做栈顶快照。编辑会话中不做，避免数据与草稿错位。
pub fn redo(session: &mut Session) -> bool {
    if session.view.editing.is_some() {
        return false;
    }
    let Some(snapshot) = session.view.redo.pop() else { return false };
    session
        .view
        .undo
        .push((session.data.fields.clone(), session.data.rows.clone()));
    session.data.fields = snapshot.0;
    session.data.rows = snapshot.1;
    session.view.bar_shown = false;
    session.mark_dirty();
    clear_invalid_selection(session);
    crate::geom::clamp_scroll(session);
    true
}

/// 选区导出为 TSV（复制 / 剪切用）。
pub fn copy_tsv(session: &Session) -> Option<String> {
    let region = ops::region_of(
        session.view.selection.as_ref(),
        &session.data.fields,
        &session.data.rows,
    )?;
    Some(ops::build_region_tsv(&session.data.fields, &session.data.rows, &region))
}

/// 剪切：导出后清空选区（图片格保留）。
pub fn cut_tsv(session: &mut Session) -> Option<String> {
    let text = copy_tsv(session)?;
    clear_selection_public(session);
    Some(text)
}

/// 粘贴 TSV：以选区左上角为锚点落格（表格不够宽时按需扩列）。
pub fn paste_tsv(session: &mut Session, text: &str) -> bool {
    let Some((row, field)) = selection_cell(session) else { return false };
    let grid = ops::parse_tsv(text);
    if grid.is_empty() {
        return false;
    }
    push_undo(session);
    let changed = ops::apply_paste_grid(
        &mut session.data.fields,
        &mut session.data.rows,
        row,
        field,
        &grid,
        || nanoid::nanoid!(),
    );
    if changed {
        session.touch();
    } else {
        session.view.undo.pop();
    }
    changed
}

/// 清空选区内容（菜单与 Delete 共用入口）。
pub fn clear_selection_public(session: &mut Session) -> bool {
    clear_selection(session)
}

/// 灯箱输入：滚轮缩放、拖动平移、双击复位、左右翻页、Esc / 点外关闭。
fn lightbox_input(
    session: &mut Session,
    input: &FrameInput,
    outcome: &mut Outcome,
) -> Option<MenuRequest> {
    use crate::model::Point;
    let size = session.view.canvas_size;
    match input {
        FrameInput::Wheel { x, y, delta_y, .. } => {
            let Some(lightbox) = session.view.lightbox.as_mut() else { return None };
            crate::lightbox::zoom_at(lightbox, size, Point::new(*x, *y), *delta_y);
            outcome.paint = true;
        }
        FrameInput::PointerDown { x, y, button, .. } => {
            let count = session
                .view
                .lightbox
                .as_ref()
                .map(|lightbox| crate::lightbox::image_count(session, lightbox))
                .unwrap_or(0);
            // 翻页钮在图片显示框之外：先按按钮命中处理，不能落进「点外关闭」
            if count > 1 {
                for dir in [-1, 1] {
                    if !crate::lightbox::nav_button(size, dir).contains(*x, *y) {
                        continue;
                    }
                    match *button {
                        MouseButton::Left => {
                            if let Some(lightbox) = session.view.lightbox.as_mut() {
                                crate::lightbox::navigate(lightbox, count, dir);
                            }
                            note_lightbox_probe(session, outcome);
                            outcome.paint = true;
                        }
                        MouseButton::Right => {
                            outcome.paint = true;
                            return Some(MenuRequest::Lightbox);
                        }
                        _ => {}
                    }
                    return None;
                }
            }
            let Some(lightbox) = session.view.lightbox.as_mut() else { return None };
            let image = crate::lightbox::display_rect(
                crate::lightbox::inner_rect(size),
                lightbox.zoom,
                lightbox.pan,
            );
            let inside = image.contains(*x, *y);
            if !inside {
                session.view.lightbox = None;
                outcome.paint = true;
                return None;
            }
            if *button == MouseButton::Right {
                outcome.paint = true;
                return Some(MenuRequest::Lightbox);
            }
            if *button == MouseButton::Left {
                lightbox.drag = Some((Point::new(*x, *y), lightbox.pan));
            }
            outcome.paint = true;
        }
        FrameInput::PointerMove { x, y, .. } => {
            let Some(lightbox) = session.view.lightbox.as_mut() else { return None };
            if let Some((start, pan)) = lightbox.drag {
                lightbox.pan = Point::new(pan.x + (*x - start.x), pan.y + (*y - start.y));
                crate::lightbox::clamp_pan(lightbox, size);
                outcome.paint = true;
            }
        }
        FrameInput::PointerUp { .. } => {
            if let Some(lightbox) = session.view.lightbox.as_mut() {
                lightbox.drag = None;
            }
            outcome.paint = true;
        }
        FrameInput::DoubleClick { .. } => {
            if let Some(lightbox) = session.view.lightbox.as_mut() {
                lightbox.reset_zoom();
            }
            outcome.paint = true;
        }
        FrameInput::Key { key, action, .. }
            if *action == atelyx_plugin_protocol::frame::KeyAction::Press =>
        {
            match key {
                Key::Named { name: NamedKey::Escape } => {
                    session.view.lightbox = None;
                    outcome.paint = true;
                }
                Key::Named { name: NamedKey::ArrowLeft } | Key::Named { name: NamedKey::ArrowRight } => {
                    let dir = if matches!(key, Key::Named { name: NamedKey::ArrowLeft }) { -1 } else { 1 };
                    let count = session
                        .view
                        .lightbox
                        .as_ref()
                        .map(|lightbox| crate::lightbox::image_count(session, lightbox))
                        .unwrap_or(0);
                    if count > 1 {
                        if let Some(lightbox) = session.view.lightbox.as_mut() {
                            crate::lightbox::navigate(lightbox, count, dir);
                        }
                        note_lightbox_probe(session, outcome);
                        outcome.paint = true;
                    }
                }
                _ => {}
            }
        }
        FrameInput::Outside { .. } => {
            session.view.lightbox = None;
            outcome.paint = true;
        }
        _ => {}
    }
    None
}

/// 记下灯箱当前图：入口层据此探明能否载入（收起加载占位 / 显示失败文案）。
fn note_lightbox_probe(session: &Session, outcome: &mut Outcome) {
    outcome.lightbox_image = session
        .view
        .lightbox
        .as_ref()
        .and_then(|lightbox| crate::lightbox::current_image(session, lightbox));
}

/// 图片展示方式切换（轮播 ↔ 九宫格）。
fn toggle_image_mode(session: &mut Session, row_id: &str, field_id: &str) {
    let Some(row) = session.data.rows.iter().find(|row| row.id == row_id) else { return };
    let Some(value) = row.values.get(field_id) else { return };
    let mut next = value.clone();
    let grid = crate::images::display_mode(Some(value)) == "grid";
    if let Some(object) = next.as_object_mut() {
        object.insert(
            "display".to_string(),
            serde_json::Value::String(if grid { "carousel".into() } else { "grid".into() }),
        );
    } else {
        // 旧形态（纯数组）升级为新形态，保留图片引用
        next = serde_json::json!({
            "images": value.clone(),
            "display": if grid { "carousel" } else { "grid" },
        });
    }
    push_undo(session);
    let Some(row) = session.data.rows.iter_mut().find(|row| row.id == row_id) else { return };
    let changed = ops::set_cell_value(row, field_id, Some(next));
    if changed {
        session.touch();
    } else {
        session.view.undo.pop();
    }
}

/// 删除当前页图片（整格重写引用列表）。
fn remove_image(session: &mut Session, row_id: &str, field_id: &str, index: usize) -> bool {
    let Some(row) = session.data.rows.iter().find(|row| row.id == row_id) else { return false };
    let Some(value) = row.values.get(field_id) else { return false };
    let mut list = crate::images::entries(Some(value));
    if index >= list.len() {
        return false;
    }
    list.remove(index);
    let next = if list.is_empty() {
        None
    } else {
        Some(serde_json::json!({ "images": list, "display": crate::images::display_mode(Some(value)) }))
    };
    push_undo(session);
    let Some(row) = session.data.rows.iter_mut().find(|row| row.id == row_id) else { return false };
    let changed = ops::set_cell_value(row, field_id, next);
    if changed {
        session.touch();
    } else {
        session.view.undo.pop();
    }
    changed
}

/// 气泡按压：命中条目则执行（切换 / 清除直接生效，字体 / 字号开菜单，颜色开取色器）。
fn format_bubble_press(
    session: &mut Session,
    x: f32,
    y: f32,
    outcome: &mut Outcome,
    menu: &mut Option<MenuRequest>,
) -> bool {
    let size = session.view.canvas_size;
    let Some(hit) = crate::format::hit(session, size, crate::model::Point::new(x, y)) else {
        return false;
    };
    use crate::format::FormatHit;
    match hit {
        FormatHit::Font => *menu = Some(MenuRequest::StyleFont),
        FormatHit::Size => *menu = Some(MenuRequest::StyleSize),
        FormatHit::Color => outcome.pick_color = Some(false),
        FormatHit::Background => outcome.pick_color = Some(true),
        FormatHit::Clear => {
            if crate::format::clear(session) {
                outcome.dirty = true;
            }
        }
        other => {
            if crate::format::toggle(session, other) {
                outcome.dirty = true;
            }
        }
    }
    outcome.paint = true;
    true
}

/// 浮层输入：编辑键走 `Editor`，回车提交、Esc / 点外取消，点类型行切换字段类型。
fn prompt_input(session: &mut Session, input: &FrameInput, outcome: &mut Outcome) {
    let size = session.view.canvas_size;
    let multiline = session
        .view
        .prompt
        .as_ref()
        .map(crate::prompt::multiline)
        .unwrap_or(false);
    match input {
        FrameInput::Key { key, action, modifiers, .. }
            if *action == atelyx_plugin_protocol::frame::KeyAction::Press =>
        {
            let result = session
                .view
                .prompt
                .as_mut()
                .map(|prompt| prompt.editor.apply_key(key, modifiers, multiline))
                .unwrap_or_default();
            if result.cancel {
                session.view.prompt = None;
            } else if result.commit {
                if crate::prompt::submit(session) {
                    outcome.dirty = true;
                }
            }
            outcome.paint = true;
        }
        FrameInput::Text { text, composing, caret, .. } => {
            if let Some(prompt) = session.view.prompt.as_mut() {
                if prompt.editor.apply_ime(text, *composing, *caret) {
                    outcome.paint = true;
                }
            }
        }
        FrameInput::PointerDown { x, y, .. } => {
            let Some(hit) = session
                .view
                .prompt
                .as_ref()
                .map(|prompt| crate::prompt::hit(prompt, size, crate::model::Point::new(*x, *y)))
                .unwrap_or(None)
            else {
                session.view.prompt = None;
                outcome.paint = true;
                return;
            };
            match hit {
                Some(kind) => {
                    if let Some(prompt) = session.view.prompt.as_mut() {
                        if let crate::prompt::PromptKind::AddField { field_type } = &mut prompt.kind {
                            *field_type = kind.to_string();
                        }
                    }
                }
                None => {
                    // 点在输入框内：按横坐标落光标（多行输入只处理首行）
                    let area = session
                        .view
                        .prompt
                        .as_ref()
                        .map(|prompt| crate::prompt::rect(prompt, size));
                    if let Some(area) = area {
                        let input_x = area.x + 8.0 + 6.0;
                        if let Some(prompt) = session.view.prompt.as_mut() {
                            let index = crate::prompt::caret_at(
                                &prompt.editor.text,
                                crate::theme::font_caption(),
                                x - input_x,
                            );
                            prompt.editor.caret = index;
                            prompt.editor.anchor = index;
                        }
                    }
                }
            }
            outcome.paint = true;
        }
        FrameInput::Outside { .. } => {
            session.view.prompt = None;
            outcome.paint = true;
        }
        _ => {}
    }
}

/// 提交缩略图重排：把 `from` 移到 `to`（按最终下标落位，一次撤销）。
fn commit_image_drag(session: &mut Session, drag: crate::model::ImageDrag) -> bool {
    if drag.from == drag.to {
        return false;
    }
    let Some(row) = session.data.rows.iter().find(|row| row.id == drag.row_id) else {
        return false;
    };
    let Some(value) = row.values.get(&drag.field_id) else { return false };
    let mut list = crate::images::entries(Some(value));
    if drag.from >= list.len() {
        return false;
    }
    let display = crate::images::display_mode(Some(value)).to_string();
    let item = list.remove(drag.from);
    let to = drag.to.min(list.len());
    list.insert(to, item);
    let next = serde_json::json!({ "images": list, "display": display });
    push_undo(session);
    let Some(row) = session.data.rows.iter_mut().find(|row| row.id == drag.row_id) else {
        session.view.undo.pop();
        return false;
    };
    let changed = ops::set_cell_value(row, &drag.field_id, Some(next));
    if changed {
        session.touch();
    } else {
        session.view.undo.pop();
    }
    changed
}

/// 长按到点：进拖拽重排（帧拍里调用）。
pub fn tick_image_press(session: &mut Session) -> bool {
    let Some(press) = session.view.image_press.as_ref() else { return false };
    if press.moved || press.at.elapsed().as_millis() < crate::images::IMAGE_LONG_PRESS_MS {
        return false;
    }
    let press = session.view.image_press.take().expect("已判存在");
    session.view.img_drag = Some(crate::model::ImageDrag {
        row_id: press.row_id,
        field_id: press.field_id,
        from: press.index,
        to: press.index,
        pos: press.start,
    });
    true
}

/// 编辑光标闪烁：帧拍里翻转显隐（未编辑 = 不动）。
pub fn tick_caret(session: &mut Session) -> bool {
    if session.view.editing.is_none() {
        return false;
    }
    session.view.caret_on = !session.view.caret_on;
    true
}

#[cfg(test)]
mod tests {
    use atelyx_format::table::{TableField, TableFile, TableRow};
    use atelyx_plugin_protocol::frame::{FrameInput, MouseButton};

    use super::{handle, tick_caret};
    use crate::model::{Lightbox, Session, Size};

    fn table(field_type: &str, value: serde_json::Value) -> TableFile {
        let mut values = serde_json::Map::new();
        values.insert("f".into(), value);
        TableFile {
            schema: "atelyx-table/v1".into(),
            id: "t".into(),
            title: "T".into(),
            fields: vec![TableField {
                id: "f".into(),
                name: "A".into(),
                field_type: field_type.into(),
                options: None,
                width: None,
                calc_type: None,
            }],
            rows: vec![TableRow {
                id: "r".into(),
                values,
                height: None,
                styles: None,
            }],
            created_at: 0,
            updated_at: 0,
        }
    }

    fn session(field_type: &str, value: serde_json::Value) -> Session {
        let mut session = Session::new("t.atb".into(), table(field_type, value));
        session.view.canvas_size = Size { width: 400.0, height: 300.0 };
        session
    }

    #[test]
    fn click_inside_editing_cell_places_caret() {
        let mut session = session("text", serde_json::json!("abcdef"));
        crate::edit::begin(&mut session, 0, 0, None);
        let cell = crate::geom::geo(
            &session.view.layout,
            session.view.scroll_x,
            session.view.scroll_y,
            session.view.zoom,
            session.view.canvas_size,
        )
        .expect("已建布局")
        .cell_rect(0, 0);
        let x = cell.x + crate::model::CELL_PAD_X + 3.0 * 14.0 * 0.55;
        let y = cell.y + crate::model::CELL_PAD_Y;
        let (outcome, _) = handle(
            &mut session,
            &FrameInput::PointerDown { x, y, button: MouseButton::Left, hit: None },
        );
        let edit = session.view.editing.as_ref().expect("仍应处于编辑态");
        assert_eq!(edit.editor.caret, 3);
        assert_eq!(edit.editor.anchor, 3);
        assert!(session.view.caret_on);
        assert!(outcome.paint);
    }

    #[test]
    fn caret_blinks_only_while_editing() {
        let mut session = session("text", serde_json::json!("abc"));
        assert!(!tick_caret(&mut session));
        crate::edit::begin(&mut session, 0, 0, None);
        let before = session.view.caret_on;
        assert!(tick_caret(&mut session));
        assert_ne!(session.view.caret_on, before);
    }

    #[test]
    fn dragging_inside_editor_extends_text_selection() {
        let mut session = session("text", serde_json::json!("abcdef"));
        crate::edit::begin(&mut session, 0, 0, None);
        let cell = cell_rect(&session);
        let char_w = crate::theme::font_ui() * 0.55;
        let y = cell.y + crate::model::CELL_PAD_Y;
        let press_x = cell.x + crate::model::CELL_PAD_X + 2.0 * char_w;
        handle(
            &mut session,
            &FrameInput::PointerDown { x: press_x, y, button: MouseButton::Left, hit: None },
        );
        assert!(session.view.editor_drag, "按下编辑格应进入拖选");
        assert_eq!(
            session.view.editing.as_ref().map(|edit| edit.editor.selection()),
            Some((2, 2)),
            "单击不产生选区"
        );
        let drag_x = cell.x + crate::model::CELL_PAD_X + 5.0 * char_w;
        handle(
            &mut session,
            &FrameInput::PointerMove { x: drag_x, y, hit: None },
        );
        assert_eq!(
            session.view.editing.as_ref().map(|edit| edit.editor.selection()),
            Some((2, 5)),
            "拖动应扩展选区"
        );
        handle(
            &mut session,
            &FrameInput::PointerUp { x: drag_x, y, button: MouseButton::Left, hit: None },
        );
        assert!(!session.view.editor_drag, "松开后结束拖选");
        assert_eq!(
            session.view.editing.as_ref().map(|edit| edit.editor.selection()),
            Some((2, 5)),
            "松开保留选区"
        );
    }

    #[test]
    fn double_click_inside_editor_selects_word() {
        let mut session = session("text", serde_json::json!("hello world"));
        crate::edit::begin(&mut session, 0, 0, None);
        let cell = cell_rect(&session);
        let char_w = crate::theme::font_ui() * 0.55;
        let x = cell.x + crate::model::CELL_PAD_X + 8.0 * char_w;
        let y = cell.y + crate::model::CELL_PAD_Y;
        handle(
            &mut session,
            &FrameInput::DoubleClick { x, y, button: MouseButton::Left, hit: None },
        );
        assert_eq!(
            session.view.editing.as_ref().map(|edit| edit.editor.selection()),
            Some((6, 11)),
            "双击应选中落点所在的词"
        );
        assert!(session.view.editor_drag, "双击后应可接着拖选");
    }

    /// 编辑格矩形（与绘制同口径）。
    fn cell_rect(session: &Session) -> crate::model::Rect {
        crate::geom::geo(
            &session.view.layout,
            session.view.scroll_x,
            session.view.scroll_y,
            session.view.zoom,
            session.view.canvas_size,
        )
        .expect("已建布局")
        .cell_rect(0, 0)
    }

    #[test]
    fn lightbox_nav_buttons_switch_image_without_closing() {
        let mut session = session("image", serde_json::json!(["a.png", "b.png"]));
        session.view.lightbox = Some(Lightbox::new("r".into(), "f".into(), 0));
        let size = session.view.canvas_size;
        let right = crate::lightbox::nav_button(size, 1);
        let (outcome, menu) = handle(
            &mut session,
            &FrameInput::PointerDown {
                x: right.x + right.w / 2.0,
                y: right.y + right.h / 2.0,
                button: MouseButton::Left,
                hit: None,
            },
        );
        assert!(session.view.lightbox.is_some(), "点翻页钮不能退出预览");
        assert_eq!(session.view.lightbox.as_ref().map(|lightbox| lightbox.index), Some(1));
        assert_eq!(outcome.lightbox_image.as_deref(), Some("b.png"));
        assert_eq!(menu, None);
    }
}
