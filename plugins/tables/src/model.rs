//! 表格会话状态：磁盘真源（`data`）、保存基线（`saved`）、视图态（滚动 / 缩放 / 选区 / 手势）
//! 与布局缓存。模型变换一律经 `atelyx_format::table::ops` 纯函数，本模块只管状态与排版缓存。

use std::collections::{HashMap, HashSet};

use atelyx_format::table::ops::{self, TableSelection};
use atelyx_format::table::{TableField, TableFile, TableRow};

use crate::text::{FontSpec, WrapCache};



/// 单元格左右内衬（px，未缩放）。
pub const CELL_PAD_X: f32 = 6.0;
/// 单元格上下内衬（px，未缩放）。
pub const CELL_PAD_Y: f32 = 5.0;
/// 行号列宽（px，未缩放）。
pub const GUTTER_W: f32 = 40.0;
/// 列尾「加列」占位宽（px，未缩放）。
pub const ADD_COL_W: f32 = 48.0;
/// 表头高（px，未缩放）。
pub const HEADER_H: f32 = 32.0;
/// 新行条高（px，未缩放）。
pub const NEW_ROW_H: f32 = 32.0;
/// 单元格图片缩略图长边上限（px）：与宿主表格图片同一档。
pub const THUMB_MAX_EDGE: u32 = 720;
/// 状态栏高（px，未缩放）。
pub const STATUS_H: f32 = 32.0;
/// 最小行高（px，未缩放）。
pub const MIN_ROW_H: f32 = 32.0;
/// 缩放上下限。
pub const MIN_ZOOM: f32 = 0.5;
pub const MAX_ZOOM: f32 = 2.0;
/// 撤销栈深度（快照式）。
pub const UNDO_DEPTH: usize = 50;
/// 双击判定窗口（毫秒）。
pub const DOUBLE_CLICK_MS: u128 = 350;

/// 数据快照（撤销 / 重做单元）。
pub type Snapshot = (Vec<TableField>, Vec<TableRow>);

/// 表格会话。
pub struct Session {
    /// 仓库相对路径。
    pub file: String,
    /// 会话身份（每次载入自增）：保存回执按它判定是否仍落在同一会话上。
    pub epoch: u64,
    /// 内存模型（载入磁盘为起点，之后由本地编辑与远端补丁改写）。
    pub data: TableFile,
    /// 上次成功落盘的基线（补丁 diff 源）。
    pub saved: TableFile,
    pub dirty: bool,
    pub save_error: Option<String>,
    /// 数据代：任何模型变更自增（布局缓存失效判据）。
    pub version: u64,
    pub view: ViewState,
}

/// 会话身份发号器（进程内唯一；回执只认发起保存时的会话）。
fn next_epoch() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

impl Session {
    pub fn new(file: String, data: TableFile) -> Self {
        let mut session = Self {
            file,
            epoch: next_epoch(),
            saved: data.clone(),
            data,
            dirty: false,
            save_error: None,
            version: 1,
            view: ViewState { zoom: 1.0, ..ViewState::default() },
        };
        session.rebuild_layout();
        session
    }

    /// 数据已变更：置脏 + 重建布局 + 作废重做栈。
    pub fn touch(&mut self) {
        self.mark_dirty();
        self.view.redo.clear();
    }

    /// 数据已变更但保留重做栈（撤销 / 重做回放用）。
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
        self.save_error = None;
        self.version += 1;
        self.rebuild_layout();
    }

    /// 重建布局缓存（列宽 / 行高 / 换行行段）。
    pub fn rebuild_layout(&mut self) {
        let zoom = self.view.zoom;
        let col_w: Vec<f32> = self
            .data
            .fields
            .iter()
            .map(|field| {
                field.width.unwrap_or_else(|| ops::field_default_width(&field.name)) as f32 * zoom
            })
            .collect();
        let mut col_x = Vec::with_capacity(col_w.len() + 1);
        let mut x = 0.0f32;
        for width in &col_w {
            col_x.push(x);
            x += width;
        }
        col_x.push(x);
        let total_w = x + ADD_COL_W * zoom;
        let mut wrap = self.view.layout.take().map(|cache| cache.wrap).unwrap_or_default();
        wrap.begin_pass();
        // 列宽拖拽在途：该列换行宽每帧都变，本轮不实测（定高与绘制同走字符估算）
        let dragging = self.view.col_drag.as_ref().map(|drag| drag.field_id.clone());
        // 在途草稿也算内容：它可能比已提交值多出软换行，行高要跟着撑开
        let draft = self.view.editing.as_ref().map(|edit| {
            (edit.row, edit.field, edit.editor.display_text())
        });
        let mut row_h = Vec::with_capacity(self.data.rows.len());
        let mut row_y = Vec::with_capacity(self.data.rows.len() + 1);
        let mut y = 0.0f32;
        for (row_index, row) in self.data.rows.iter().enumerate() {
            row_y.push(y);
            let mut content = 0.0f32;
            let draft_value = draft
                .as_ref()
                .filter(|(row, _, _)| *row == row_index)
                .map(|(_, _, text)| serde_json::Value::String(text.clone()));
            for (index, field) in self.data.fields.iter().enumerate() {
                let slot = if dragging.as_deref() == Some(field.id.as_str()) {
                    None
                } else {
                    Some(&mut wrap)
                };
                let value = match (&draft, &draft_value) {
                    (Some((_, field, _)), Some(value)) if *field == index => Some(value),
                    _ => row.values.get(&field.id),
                };
                content = content.max(cell_content_height(
                    slot,
                    field,
                    row,
                    value,
                    col_w[index],
                    zoom,
                ));
            }
            let height = match row.height {
                Some(height) => (height as f32).max(ops::MIN_ROW_HEIGHT as f32) * zoom,
                None => (content + 2.0 * CELL_PAD_Y * zoom).max(MIN_ROW_H * zoom),
            };
            row_h.push(height);
            y += height;
        }
        row_y.push(y);
        wrap.end_pass();
        self.view.layout = Some(LayoutCache {
            col_w,
            col_x,
            total_w,
            row_h,
            row_y,
            content_h: y + NEW_ROW_H * zoom,
            wrap,
        });
    }
}

/// 表格视图态。
#[derive(Default)]
pub struct ViewState {
    pub scroll_x: f32,
    pub scroll_y: f32,
    pub zoom: f32,
    pub selection: Option<TableSelection>,
    /// 单元格编辑器浮层（None = 未在编辑）。
    pub editing: Option<crate::edit::CellEdit>,
    /// 格式气泡可见（单击已选中格弹出；换选 / 进编辑 / 撤销重做即隐藏）。
    pub bar_shown: bool,
    pub hscroll_drag: Option<HScrollDrag>,
    pub vscroll_drag: Option<VScrollDrag>,
    pub press: Option<PressState>,
    pub col_drag: Option<ColDrag>,
    pub row_drag: Option<RowDrag>,
    pub reorder: Option<RowReorder>,
    pub img_drag: Option<ImageDrag>,
    pub swipe: Option<SwipeState>,
    pub hover: Option<Hit>,
    pub undo: Vec<Snapshot>,
    pub redo: Vec<Snapshot>,
    /// 起稿快照（进入编辑时压栈；提交有改动才保留）。
    pub edit_session: Option<Snapshot>,
    /// 图片轮播起始下标（多图单元格）。
    pub carousel: HashMap<(String, String), usize>,
    pub lightbox: Option<Lightbox>,
    /// 上次按下的时刻与位置（双击判定）。
    pub last_press: Option<(std::time::Instant, Point)>,
    /// 面尺寸（最近一次 resize 上报）。
    pub canvas_size: Size,
    /// 布局缓存（数据代 / 缩放变化时重建）。
    pub layout: Option<LayoutCache>,
    /// 最近一次按键的修饰态快照（滚轮缩放判定用）。
    pub mods: atelyx_plugin_protocol::frame::Modifiers,
    /// 主图按下后等待松手开灯箱的格位。
    pub pending_image_open: Option<(String, String, usize)>,
    /// 缩略图长按判定中的按压。
    pub image_press: Option<ImagePress>,
    /// 浮层输入（字段改名 / 插列 / 添加字段 / 选项编辑）。
    pub prompt: Option<crate::prompt::Prompt>,
}

/// 面尺寸。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Size {
    pub width: f32,
    pub height: f32,
}

/// 矩形（面本地坐标）。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x <= self.right() && y >= self.y && y <= self.bottom()
    }
}

/// 点位（与 iced 的 Point 同构，插件不依赖 iced）。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// 布局缓存：列宽 / 列起点 / 行高 / 行起点 / 内容总高 + 断行缓存。
pub struct LayoutCache {
    pub col_w: Vec<f32>,
    pub col_x: Vec<f32>,
    pub total_w: f32,
    pub row_h: Vec<f32>,
    pub row_y: Vec<f32>,
    pub content_h: f32,
    pub wrap: WrapCache,
}

/// 按压会话（框选候选 / 图片手势）。
pub struct PressState {
    pub origin: (usize, usize),
    pub pos: Point,
    pub kind: PressKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PressKind {
    Cell,
}

/// 缩略图按压：长按到点即进拖拽（移动超过阈值则取消）。
#[derive(Clone)]
pub struct ImagePress {
    pub row_id: String,
    pub field_id: String,
    pub index: usize,
    pub at: std::time::Instant,
    pub start: Point,
    pub moved: bool,
}

pub struct HScrollDrag {
    pub grab: f32,
}

pub struct VScrollDrag {
    pub grab: f32,
}

pub struct ColDrag {
    pub field_id: String,
    pub start_w: f32,
    pub start_x: f32,
    /// 已按实际变化入过撤销栈（按下未拖动不产生撤销点）。
    pub pushed: bool,
}

pub struct RowDrag {
    pub row_id: String,
    pub start_h: f32,
    pub start_y: f32,
    /// 已按实际变化入过撤销栈。
    pub pushed: bool,
}

#[derive(Clone)]
pub struct RowReorder {
    pub row_id: String,
    pub target: usize,
    /// 按压起点（纵坐标，判定 5px 启动阈值）。
    pub start_y: f32,
    /// 位移是否已超过阈值（未超过 = 只是点了把手，不结算）。
    pub moved: bool,
}

pub struct ImageDrag {
    pub row_id: String,
    pub field_id: String,
    pub from: usize,
    pub to: usize,
    pub pos: Point,
}

pub struct SwipeState {
    pub start: Point,
    pub moved: bool,
    /// 当前指针相对按压点的横向位移（负 = 向左拖），松手随会话一起丢弃。
    pub dx: f32,
}

/// 灯箱（图片全屏预览）。
#[derive(Clone)]
pub struct Lightbox {
    pub row_id: String,
    pub field_id: String,
    pub index: usize,
    /// 缩放（1 = 适应窗口）。
    pub zoom: f32,
    /// 平移（放大后可拖动）。
    pub pan: Point,
    pub drag: Option<(Point, Point)>,
}

impl Lightbox {
    pub fn new(row_id: String, field_id: String, index: usize) -> Self {
        Self {
            row_id,
            field_id,
            index,
            zoom: 1.0,
            pan: Point::default(),
            drag: None,
        }
    }

    pub fn reset_zoom(&mut self) {
        self.zoom = 1.0;
        self.pan = Point::default();
    }
}

/// 命中区（面本地坐标下的语义分类）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Corner,
    Header { field: usize },
    HeaderMenu { field: usize },
    HeaderResize { field: usize },
    AddFieldCol,
    Gutter { row: usize },
    GutterHandle { row: usize },
    GutterGrip { row: usize },
    Cell { row: usize, field: usize },
    Status { field: usize },
    NewRow,
    HScrollbar,
    VScrollbar,
}

/// 单元格内容高（行高下限之外的正文占位）：换行行段与图片区取最大。
fn cell_content_height(
    wrap: Option<&mut WrapCache>,
    field: &TableField,
    row: &TableRow,
    value: Option<&serde_json::Value>,
    col_w: f32,
    zoom: f32,
) -> f32 {
    let wrap_w = (col_w - 2.0 * CELL_PAD_X * zoom).max(8.0);
    if field.field_type == "image" {
        return crate::images::image_area_height(value, col_w, zoom);
    }
    let text = cell_text(value);
    if text.is_empty() {
        return 0.0;
    }
    let text = text.as_ref();
    let style = ops::style_of(row, &field.id);
    let font = FontSpec::of(&style, field, zoom);
    let lines = match wrap {
        Some(cache) => cache.lines(text, &font, wrap_w).len(),
        // 拖列宽在途：不实测，按字符估算行高（与绘制同口径）
        None => crate::text::estimate_lines(text, font.size, wrap_w),
    };
    lines as f32 * font.size * crate::text::LINE_FACTOR
}

/// 单元格文本（非文本字段取它的显示串）。
pub fn cell_text(value: Option<&serde_json::Value>) -> std::borrow::Cow<'_, str> {
    match value {
        Some(serde_json::Value::String(text)) => std::borrow::Cow::Borrowed(text.as_str()),
        Some(serde_json::Value::Number(number)) => {
            std::borrow::Cow::Owned(number.to_string())
        }
        Some(serde_json::Value::Bool(flag)) => {
            std::borrow::Cow::Borrowed(if *flag { "true" } else { "false" })
        }
        _ => std::borrow::Cow::Borrowed(""),
    }
}

/// 选中区域涉及的字段与行下标集合（绘制淡底与状态栏统计用）。
pub fn selection_sets(
    fields: &[TableField],
    rows: &[TableRow],
    selection: Option<&TableSelection>,
) -> (HashSet<usize>, HashSet<usize>) {
    let region = ops::region_of(selection, fields, rows);
    match region {
        Some(region) => (
            (region.col_start..=region.col_end).collect(),
            (region.row_start..=region.row_end).collect(),
        ),
        None => (HashSet::new(), HashSet::new()),
    }
}

