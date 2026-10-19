//! 多维表格视图：canvas 自绘主体（网格 / 文本 / 样式 / 图片 / 选中框 / 把手 / 虚拟窗口）、
//! 顶部工具条、编辑器浮层、格式气泡与灯箱。模型真源 = core `TableFile`；
//! 模型变换走 `table_ops` 纯函数，消息处理与保存链在 `table_actions`。

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;

use iced::widget::canvas::{self, Canvas, LineCap, LineDash, LineJoin, Path, Stroke};
use iced::widget::text_editor;
use iced::widget::{column, container, row, space};
use iced::{alignment, Color, Element, Length, Point, Rectangle, Size, Vector};

use crate::layout_view::estimate_text_width;
use crate::table_text::{TextLine, WrapCache, LINE_FACTOR};
use crate::ui::icon::polylines;
use crate::ui::palette as pal;
use crate::ui::text;
use crate::Message;

use atelyx_core::table_ops::{self, CellStyle, OptState, TableSelection};
use atelyx_core::vault::{TableField, TableFile, TableRow};

pub use atelyx_core::table_ops::TableSnapshot;

const GUTTER_W: f32 = 40.0;
const ADD_COL_W: f32 = 48.0;
const HEADER_H: f32 = 32.0;
const STATUS_H: f32 = 32.0;
const NEW_ROW_H: f32 = 32.0;
const TOOLBAR_H: f32 = 30.0;
pub const MIN_ZOOM: f32 = 0.5;
pub const MAX_ZOOM: f32 = 2.0;

const CELL_PAD_X: f32 = 6.0;
const CELL_PAD_Y: f32 = 5.0;
fn cell_font() -> f32 {
    pal::fs_ui()
}
fn num_font() -> f32 {
    pal::fs_caption()
}
/// 单元格正文字体（样式族 + 粗斜体）：画布绘制、断行测量与编辑浮层同口径。
fn cell_text_font(st: &CellStyle) -> iced::Font {
    iced::Font {
        weight: if st.b { iced::font::Weight::Bold } else { iced::font::Weight::Normal },
        style: if st.i { iced::font::Style::Italic } else { iced::font::Style::Normal },
        ..style_font(st)
    }
}

/// 正文可用宽 = 格宽去掉左右内衬（行高、绘制与浮层的换行同式）。
fn wrap_width(cell_w: f32, zoom: f32) -> f32 {
    (cell_w - 2.0 * CELL_PAD_X * zoom).max(8.0)
}

const COL_HANDLE_W: f32 = 8.0;
const ROW_HANDLE_H: f32 = 6.0;
/// 按压转框选 / 行拖拽排序的位移阈值（px）。
pub const DRAG_THRESHOLD: f32 = 5.0;
/// 撤销栈深度（快照式）。
pub const UNDO_DEPTH: usize = 50;
/// 双击判定窗口（毫秒）。
pub const DOUBLE_CLICK_MS: u128 = 350;
const FIELD_BTN_W: f32 = 18.0;
/// 行拖拽把手（grip）中线命中半径（px，随缩放）。
const GRIP_HIT_R: f32 = 9.0;
/// 滚轮一格的滚动距离（px；winit Lines 档换算）。
const WHEEL_LINE_PX: f32 = 40.0;
/// 格式气泡与选区的间距（px）。
const BAR_GAP: f32 = 4.0;
/// 格式气泡估宽余量（px）：估宽按平均字宽，长字体族名可能低估，余量保证钳位后行内仍不折字。
const BAR_W_MARGIN: f32 = 16.0;
/// 格式气泡几何：估宽估高算式与气泡构造共用同一口径，改尺寸须同步两处。
const BAR_PAD_X: f32 = 6.0;
const BAR_PAD_Y: f32 = 4.0;
const BAR_BTN_PAD_X: f32 = 6.0;
const BAR_BTN_PAD_Y: f32 = 3.0;
const BAR_ERASER_PAD_X: f32 = 5.0;
const BAR_SWATCH: f32 = 10.0;
const BAR_SWATCH_GAP: f32 = 3.0;
const BAR_ERASER_ICON: f32 = 13.0;
const BAR_DIVIDER_W: f32 = 1.0;
const BAR_ITEM_GAP: f32 = 4.0;
/// 灯箱图片四周遮罩边距（px；点击遮罩关闭的命中区）。
const LIGHTBOX_MARGIN: f32 = 48.0;
/// 灯箱缩放倍率上限（相对适应窗口）与滚轮指数系数（px → 倍率）。
const LIGHTBOX_MAX_ZOOM: f32 = 8.0;
const LIGHTBOX_ZOOM_RATE: f32 = 0.0035;
/// 双击复位允许的按压位移（px）。
pub const LIGHTBOX_CLICK_SLOP: f32 = 4.0;

// 图片单元格
const IMAGE_CAROUSEL_H: f32 = 96.0;
const IMAGE_QUEUE_H: f32 = 36.0;
const IMAGE_THUMB: f32 = 28.0;
const IMAGE_GAP: f32 = 4.0;
const IMAGE_GRID_WIDE_MIN: f32 = 200.0;
const IMAGE_GRID_SINGLE_MAX: f32 = 160.0;
/// 长按进入拖动排序的等待时长（毫秒）。
pub const IMAGE_LONG_PRESS_MS: u128 = 400;
/// 长按激活前允许的位移（px；超过 = 取消按压）。
pub const IMAGE_PRESS_CANCEL: f32 = 6.0;
/// 滑动翻页阈值（px）。
pub const IMAGE_SWIPE_THRESHOLD: f32 = 48.0;
/// 两端阻尼回弹系数。
pub const IMAGE_EDGE_DAMPING: f32 = 0.35;
/// 单个图片文件导入上限（与服务端单文件写限额一致）。
pub const IMAGE_MAX_BYTES: usize = 50 * 1024 * 1024;

/// 编辑器浮层的 widget id（挂焦点用）。
pub const CELL_EDITOR_ID: &str = "table-cell-editor";

/// 横向滚动条厚度（px；贴表格区底缘）。
pub const HSCROLLBAR_H: f32 = 8.0;
/// 竖向滚动条宽度（px；贴表格区右缘）。
pub const VSCROLLBAR_W: f32 = 8.0;

// ===== 会话状态 =====

/// 打开的表格会话（数据 + 保存基线 + 视图态）。
pub struct TableSession {
    /// 相对仓库根路径。
    pub file: String,
    pub data: TableFile,
    /// 落盘基线（上次保存成功时的字段/行快照；补丁 diff 源）。
    pub saved: TableFile,
    pub dirty: bool,
    pub save_error: Option<String>,
    /// 数据变更代：任何 data 变更自增（布局缓存失效判据）。
    pub version: u64,
    pub view: TableViewState,
}

impl TableSession {
    pub fn new(file: String, data: TableFile) -> Self {
        let mut s = Self {
            file,
            data,
            saved: TableFile {
                schema: String::new(),
                id: String::new(),
                title: String::new(),
                fields: vec![],
                rows: vec![],
                created_at: 0,
                updated_at: 0,
            },
            dirty: false,
            save_error: None,
            version: 1,
            view: TableViewState { zoom: 1.0, ..Default::default() },
        };
        s.saved = s.data.clone();
        s.rebuild_layout();
        s
    }

    /// 数据已变更：置脏 + 排自动保存 + 重建布局缓存；新变更作废重做栈。
    pub fn touch(&mut self) {
        self.dirty = true;
        self.save_error = None;
        self.version += 1;
        self.view.redo.clear();
        self.rebuild_layout();
    }

    /// 布局重建（数据版本或 zoom 变化时调用；zoom 变更后由调用方补调）。
    pub fn rebuild_layout(&mut self) {
        let zoom = self.view.zoom;
        let data = &self.data;
        let col_w: Vec<f32> = data
            .fields
            .iter()
            .map(|f| f.width.unwrap_or_else(|| table_ops::field_default_width(&f.name)) as f32 * zoom)
            .collect();
        // 内容坐标原点 = 行号列右缘（content_x/cell_rect 的 gutter 偏移已承担这层平移）
        let mut col_x = Vec::with_capacity(col_w.len() + 1);
        let mut x = 0.0f32;
        for w in &col_w {
            col_x.push(x);
            x += w;
        }
        col_x.push(x);
        let total_w = x + ADD_COL_W * zoom;

        // 行段缓存随布局滚动：收口时丢掉本轮没再用到的键
        let mut wrap = self.view.layout.take().map(|cache| cache.wrap).unwrap_or_default();
        wrap.begin_pass();
        // 列宽拖拽在途：该列换行宽每帧都变，实测会把每次指针移动变成整表重排。
        // 这列本轮不实测（定高与绘制同走字符估算），松手后的重建回到实测口径
        let dragging = self.view.col_drag.as_ref().map(|d| d.field_id.clone());
        let mut row_h = Vec::with_capacity(data.rows.len());
        let mut row_y = Vec::with_capacity(data.rows.len() + 1);
        let mut y = 0.0f32;
        for row in &data.rows {
            row_y.push(y);
            // 显式行高的行同样实测：绘制这些格的正文要有行段可取
            let mut content = 0.0f32;
            for (i, f) in data.fields.iter().enumerate() {
                let slot = if dragging.as_deref() == Some(f.id.as_str()) { None } else { Some(&mut wrap) };
                content = content.max(cell_content_height(slot, f, row, row.values.get(&f.id), col_w[i], zoom));
            }
            let h = match row.height {
                // 磁盘存量可能小于下限（写入侧钳制不覆盖旧数据）：读取侧同样钳到把手命中区下限
                Some(h) => (h as f32).max(table_ops::MIN_ROW_HEIGHT as f32) * zoom,
                None => (content + 2.0 * CELL_PAD_Y * zoom).max(32.0 * zoom),
            };
            row_h.push(h);
            y += h;
        }
        row_y.push(y);
        wrap.end_pass();
        self.view.layout = Some(LayoutCache {
            version: self.version,
            zoom,
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

#[derive(Default)]
pub struct TableViewState {
    /// 面板内展示的视图：`None` = 内置网格；`Some` = 插件表格视图的壳侧 kind。
    pub active_view: Option<String>,
    pub scroll_x: f32,
    pub scroll_y: f32,
    pub zoom: f32,
    pub selection: Option<TableSelection>,
    /// 编辑器浮层态（None = 无）。
    pub editing: Option<CellEdit>,
    /// 样式工具条可见（单击已选中格弹出；换选 / 进编辑 / 撤销重做即隐藏）。
    pub bar_shown: bool,
    /// 横向滚动条拖拽（grab = 按压点相对滑块左缘的偏移）。
    pub hscroll_drag: Option<HScrollDrag>,
    /// 竖向滚动条拖拽（grab = 按压点相对滑块上缘的偏移）。
    pub vscroll_drag: Option<VScrollDrag>,
    /// 单元格按压会话（框选候选 / 图片手势）。
    pub press: Option<PressState>,
    pub col_drag: Option<ColDrag>,
    pub row_drag: Option<RowDrag>,
    pub reorder: Option<RowReorder>,
    pub img_drag: Option<ImageDrag>,
    pub swipe: Option<SwipeState>,
    /// 光标位置（画布局部；hover 表达用）。
    pub hover: Point,
    pub undo: Vec<TableSnapshot>,
    pub redo: Vec<TableSnapshot>,
    /// 编辑会话（起稿快照 + 单元格）：提交有变化才入撤销栈，取消/无改动直接丢弃。
    pub edit_session: Option<EditSession>,
    /// 轮播当前页（key = (row, field)）。
    pub carousel: HashMap<(String, String), usize>,
    pub images: HashMap<String, CachedImage>,
    pub image_inflight: HashSet<String>,
    pub image_failed: HashSet<String>,
    /// 缩略图纹理常驻登记（绘制时登记，见 [`ThumbPins`]）。
    pub pins: RefCell<ThumbPins>,
    pub lightbox: Option<Lightbox>,
    /// 上次按压（双击判定）。
    pub last_press: Option<(Instant, Point)>,
    /// 画布尺寸（Press/Context 播种；Move/Release 期间的命中几何用）。
    pub canvas_size: Size,
    /// 画布在窗口中的原点与来源窗口（Press/Move 播种）：格式气泡据此把选区偏移换算成窗口锚点。
    /// 窗口不符（切窗 / 撕裂）或画布未收到过事件 = 锚点不成立，气泡不弹。
    pub canvas_origin: Option<(iced::window::Id, Point)>,
    pub layout: Option<LayoutCache>,
    /// 编辑器浮层按压标志（全局 Press 的「点在编辑器内」判定；由编辑器的鼠标动作消息置位）。
    pub editor_inside: bool,
    /// 表格视图持有键盘焦点（画布按压置位、外点清除；键盘分派门控）。
    pub focused: bool,
    /// 本批按压来自表格面（画布或其菜单，不失焦；供全局 Press 区分；同事件批内 widget 消息先于 subscription 消息）。
    pub press_seen: bool,
}

#[derive(Clone)]
pub struct CellEdit {
    pub row_id: String,
    pub field_id: String,
    pub content: text_editor::Content,
    /// 编辑器收到过内容编辑（区分「未输入」（提交 = 保留原值）与「输入后删空」（提交 = 清值））。
    pub touched: bool,
}

/// 编辑会话（起稿快照 + 单元格）：提交有变化才把快照入撤销栈；取消 / 无改动直接丢弃。
#[derive(Clone)]
pub struct EditSession {
    pub row_id: String,
    pub field_id: String,
    pub snapshot: TableSnapshot,
}

/// 横向滚动条拖拽会话。
#[derive(Clone, Copy)]
pub struct HScrollDrag {
    pub grab: f32,
}

/// 竖向滚动条拖拽会话。
#[derive(Clone, Copy)]
pub struct VScrollDrag {
    pub grab: f32,
}

/// 单元格按压会话。
pub struct PressState {
    pub start: Point,
    /// 位移超阈值（已进入框选 / 图片手势）。
    pub active: bool,
    /// 锚点 (row, col)。
    pub anchor: (usize, usize),
    /// image 字段不参与框选。
    pub no_range: bool,
    /// 图片子元素按压（长按 / 滑动 / 队列跳转）。
    pub img: Option<ImagePress>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum ImagePressKind {
    /// 轮播主图区（横向滑动翻页；未拖动松手 = 开灯箱）。
    Main,
    /// 缩略图 / 九宫格方块（长按拖拽排序；未长按松手 = 跳转 / 开灯箱）。
    Thumb(usize),
}

pub struct ImagePress {
    pub row_id: String,
    pub field_id: String,
    pub kind: ImagePressKind,
    pub at: Instant,
    pub start: Point,
}

pub struct ColDrag {
    pub field_id: String,
    pub start_x: f32,
    pub start_w: f64,
    /// 首次实际变化才入撤销栈（整段拖拽 = 一步）。
    pub pushed: bool,
}

pub struct RowDrag {
    pub row_id: String,
    pub start_y: f32,
    pub start_h: Option<f64>,
    pub pushed: bool,
}

pub struct RowReorder {
    pub row_id: String,
    pub from: usize,
    pub insert_at: usize,
    pub active: bool,
    pub start: Point,
}

pub struct ImageDrag {
    pub row_id: String,
    pub field_id: String,
    /// 被拖图的仓库相对路径（影子绘制）。
    pub rel: String,
    pub from: usize,
    pub pos: Point,
}

pub struct SwipeState {
    pub row_id: String,
    pub field_id: String,
    pub index: usize,
    pub count: usize,
    pub start_x: f32,
    pub dx: f32,
}

#[derive(Clone)]
pub struct CachedImage {
    pub fp: (u128, u64),
    /// 原图句柄（灯箱按全分辨率绘制）。
    pub handle: iced::widget::image::Handle,
    /// 单元格缩略图句柄（长边不超过 `THUMB_MAX_EDGE`，绘制即用它）。
    pub thumb: iced::widget::image::Handle,
    /// 原图像素尺寸（contain 缩放与灯箱几何用；与 `handle` 解码所得一致）。
    pub size: (f32, f32),
    /// 缩略图是否已绘制过（纹理已上传）。常驻登记以此为前提：登记只取分配、不触发上传。
    pub drawn: Cell<bool>,
}

impl CachedImage {
    /// 由载入结果构建（缩略图尚未绘制）。
    pub fn new(fp: (u128, u64), loaded: LoadedImage) -> Self {
        let thumb = loaded.thumb;
        Self {
            fp,
            handle: iced::widget::image::Handle::from_bytes(loaded.bytes),
            thumb: iced::widget::image::Handle::from_rgba(thumb.width, thumb.height, thumb.rgba),
            size: loaded.size,
            drawn: Cell::new(false),
        }
    }
}

/// 图片加载结果：原图字节（灯箱按需解码）与单元格缩略图。
#[derive(Clone)]
pub struct LoadedImage {
    pub bytes: Vec<u8>,
    pub size: (f32, f32),
    pub thumb: atelyx_core::image_thumb::Thumb,
}

impl std::fmt::Debug for LoadedImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadedImage")
            .field("size", &self.size)
            .field("bytes", &self.bytes.len())
            .field("thumb", &self.thumb)
            .finish_non_exhaustive()
    }
}

/// 常驻缩略图的显存预算（解码后字节数）。
pub const THUMB_PIN_BUDGET: usize = 192 * 1024 * 1024;

/// 单元格缩略图纹理的常驻登记。
///
/// 渲染层的图片缓存在有新图上传时回收当帧未绘制的纹理；持有 [`Allocation`] 即被保留。
/// 分配只有渲染器能给，所以登记发生在绘制时（见 `pin_visible_thumbs`），按字节预算淘汰
/// 最久未登记的条目。
///
/// [`Allocation`]: iced::advanced::image::Allocation
#[derive(Default)]
pub struct ThumbPins {
    budget: PinBudget,
    allocations: HashMap<String, iced::advanced::image::Allocation>,
}

impl ThumbPins {
    pub fn contains(&self, rel: &str) -> bool {
        self.allocations.contains_key(rel)
    }

    /// 取消登记（同一路径换了一份图：旧分配对应旧句柄，留着只会白占纹理）。
    pub fn unpin(&mut self, rel: &str) {
        if self.allocations.remove(rel).is_some() {
            self.budget.forget(rel);
        }
    }

    /// 登记一张缩略图的分配；超预算的条目在此释放。
    pub fn pin(
        &mut self,
        rel: String,
        allocation: iced::advanced::image::Allocation,
        visible: &HashSet<String>,
    ) {
        if self.allocations.contains_key(&rel) {
            return;
        }
        let size = allocation.size();
        let bytes = size.width as usize * size.height as usize * 4;
        for evicted in self.budget.insert(&rel, bytes, visible) {
            self.allocations.remove(&evicted);
        }
        self.allocations.insert(rel, allocation);
    }
}

/// 常驻预算记账：按登记顺序淘汰，可见区内的条目不淘汰。
#[derive(Debug)]
pub struct PinBudget {
    limit: usize,
    total: usize,
    order: VecDeque<(String, usize)>,
}

impl Default for PinBudget {
    fn default() -> Self {
        Self { limit: THUMB_PIN_BUDGET, total: 0, order: VecDeque::new() }
    }
}

impl PinBudget {
    /// 取消记账（条目已被调用方释放）。
    pub fn forget(&mut self, rel: &str) {
        if let Some(index) = self.order.iter().position(|(key, _)| key == rel) {
            if let Some((_, size)) = self.order.remove(index) {
                self.total = self.total.saturating_sub(size);
            }
        }
    }

    /// 记入一条并返回因超预算被淘汰的键；可见区放不下时宁可超预算（不重传正在看的图）。
    pub fn insert(&mut self, rel: &str, bytes: usize, visible: &HashSet<String>) -> Vec<String> {
        self.order.push_back((rel.to_string(), bytes));
        self.total += bytes;
        let mut evicted = Vec::new();
        while self.total > self.limit {
            let Some(index) = self
                .order
                .iter()
                .position(|(key, _)| key.as_str() != rel && !visible.contains(key))
            else {
                break;
            };
            let Some((key, size)) = self.order.remove(index) else {
                break;
            };
            self.total = self.total.saturating_sub(size);
            evicted.push(key);
        }
        evicted
    }
}

#[derive(Clone)]
pub struct Lightbox {
    pub row_id: String,
    pub field_id: String,
    pub index: usize,
    /// 缩放倍率（1 = 适应窗口）。
    pub zoom: f32,
    /// 图片中心相对可视区中心的平移（px，按显示尺寸夹紧）。
    pub pan: Vector,
    /// 拖动平移的上一光标点（None = 未拖动）。
    pub drag: Option<Point>,
    /// 上次按压（双击复位判定）。
    pub last_press: Option<(Instant, Point)>,
}

impl Lightbox {
    pub fn new(row_id: String, field_id: String, index: usize) -> Self {
        Self { row_id, field_id, index, zoom: 1.0, pan: Vector::ZERO, drag: None, last_press: None }
    }

    /// 回到适应窗口（双击复位 / 翻页）。
    pub fn reset_view(&mut self) {
        self.zoom = 1.0;
        self.pan = Vector::ZERO;
        self.drag = None;
        self.last_press = None;
    }
}

/// 行高 / 列位布局缓存（version 或 zoom 变更即重建）。
pub struct LayoutCache {
    pub version: u64,
    pub zoom: f32,
    pub col_w: Vec<f32>,
    pub col_x: Vec<f32>,
    pub total_w: f32,
    pub row_h: Vec<f32>,
    pub row_y: Vec<f32>,
    pub content_h: f32,
    /// 本轮布局的断行结果（行高与绘制共用）。
    pub wrap: WrapCache,
}

// ===== 消息 =====

/// 表格视图消息（部件上报 + 菜单/命令 + 保存链回执）。
#[derive(Debug, Clone)]
pub enum TableMsg {
    /// 画布内左键按下（画布局部坐标；origin = 画布在窗口中的原点，供弹层锚定单元格）。
    Press { window: iced::window::Id, pos: Point, size: Size, origin: Point },
    /// 画布内右键按下（上下文菜单）。
    Context { window: iced::window::Id, pos: Point, size: Size },
    /// 画布内鼠标移动（携带画布尺寸与画布原点：首帧悬停即播种 canvas_size / canvas_origin）。
    Move { window: iced::window::Id, pos: Point, size: Size, origin: Point },
    Release(Point),
    /// 滚轮（ctrl = 缩放，修饰态由 App.mods 维护）。
    Wheel { delta_y: f32, delta_x: f32 },
    /// 编辑器动作（Enter/Esc/撤销经 key_binding 转义为 Custom 分支）。
    Edited(text_editor::Action),
    /// 菜单条目来源的表格动作：先关弹层再执行（绕过全局键盘守卫——菜单本身即弹层）。
    Menu(Box<TableMsg>),
    /// 打开样式工具气泡（右键菜单「格式」；作用于当前选区）。
    ShowStyleBar,
    /// Enter 提交并下移一行。
    CommitEnter,
    /// 取消编辑（Esc；无改动的会话回退撤销栈顶）。
    CancelEdit,
    /// 选区态打印字符（覆盖原值进入编辑）。
    CharKey(char),
    /// 选区态导航键。
    NavKey(NavDir),
    Copy,
    Cut,
    Paste,
    ClearSelection,
    SelectAll,
    /// 延迟弹样式工具条（仍选中该格且未在编辑才弹）。
    ShowBar { row_id: String, field_id: String },
    Undo,
    Redo,
    // 行列命令
    DuplicateRow(String),
    MoveRow { row_id: String, delta: i32 },
    DeleteRows(Vec<String>),
    AddField { name: String, ftype: String },
    InsertField { index: Option<usize>, name: String },
    DeleteField(String),
    RenameField { field_id: String, name: String },
    RetypeField { field_id: String, ftype: String },
    /// 追加一个单选选项（弹层内逐项维护，保持弹层打开）。
    OptionsAdd { field_id: String, name: String },
    /// 删除指定下标的单选选项。
    OptionsRemove { field_id: String, index: usize },
    ColumnAutoWidth(Option<String>),
    RowHeightAuto(Option<String>),
    SetCalc { field_id: String, calc: Option<String> },
    // 样式
    StyleToggle(StyleKey),
    SetFont(Option<String>),
    SetSize(Option<f64>),
    SetColor { bg: bool, color: Option<String> },
    ClearFormat,
    // 单选
    SelectOption { row_id: String, field_id: String, value: Option<String> },
    // 图片
    ImageImportPick { row_id: String, field_id: String },
    ImagesPicked { row_id: String, field_id: String, files: Vec<(String, Vec<u8>)> },
    ImageLoaded { rel: String, fp: (u128, u64), result: Result<LoadedImage, String> },
    ImagesWritten { row_id: String, field_id: String, rels: Vec<String>, failed: usize },
    LightboxClose,
    LightboxNav(i32),
    LightboxDownload,
    /// 灯箱：复制当前图到系统剪贴板。
    LightboxCopy,
    /// 复制到系统剪贴板的结果。
    Copied(Result<(), String>),
    /// 灯箱滚轮缩放（pos = 灯箱局部坐标，view = 灯箱尺寸）。
    LightboxZoom { pos: Point, view: Size, delta_y: f32 },
    /// 灯箱图片按压（双击复位 / 放大后开始拖动）。
    LightboxPress(Point),
    /// 灯箱拖动平移（Some = 拖动中的光标点，None = 松开）。
    LightboxPan { pos: Option<Point>, view: Size },
    /// 打开灯箱右键菜单（复制 / 下载图片）。
    OpenLightboxMenu(iced::window::Id),
    /// 打开工具条视图切换下拉。
    OpenViewMenu(iced::window::Id),
    Downloaded(Result<std::path::PathBuf, String>),
    // 导出
    ExportXlsxPick,
    ExportPathPicked(Option<std::path::PathBuf>),
    /// 导出 xlsx 完成（Ok = 无法嵌入的图片数）。
    Exported(Result<usize, String>),
    /// 打开 ··· 菜单（历史 / 导出）。
    OpenMoreMenu(iced::window::Id),
    /// 打开格式气泡的下拉（字体 / 字号 / 字色 / 底色）。
    OpenFontMenu(iced::window::Id),
    OpenSizeMenu(iced::window::Id),
    OpenColorMenu { window: iced::window::Id, bg: bool },
    /// 无操作（吞键位）。
    Noop,
    /// 表格弹层内联输入草稿（改名 / 插列 / 添加字段共用）。
    PopupDraft(String),
    // 载入与保存
    Loaded(u64, String, Result<TableFile, String>),
    TableSaved(u64, String, SaveReason, Result<(), String>, TableFile),
    PasteText(String),
    // 插件表格视图
    /// 切换面板内展示的视图（`None` = 内置表格网格；`Some` = 插件表格视图的壳侧 kind）。
    SetView(Option<String>),
    /// 插件写：逐格写（经同一出口：同撤销栈、同置脏与保存链；结果按 `request_id` 回填）。
    PluginSetCell { request_id: u64, row_id: String, field_id: String, value: serde_json::Value },
    /// 插件写：新增行（结果回带新行 id）。
    PluginAddRow { request_id: u64 },
    /// 插件写：删除行。
    PluginRemoveRow { request_id: u64, row_id: String },
    /// 插件写：选中行（与网格联动）。
    PluginSelectRow { request_id: u64, row_id: String },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NavDir {
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StyleKey {
    Bold,
    Italic,
    Underline,
    Strike,
}

/// 表格保存动机（与笔记 SaveReason 同构）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SaveReason {
    Manual,
    AutoSave,
    Flush,
}

// ===== 几何 =====

/// 表格几何：全部尺寸已乘 zoom，坐标为画布局部。
pub struct Geo<'a> {
    pub session: &'a TableSession,
    pub cache: &'a LayoutCache,
    pub width: f32,
    pub height: f32,
    pub z: f32,
}

impl<'a> Geo<'a> {
    pub fn header_h(&self) -> f32 {
        HEADER_H * self.z
    }
    pub fn status_h(&self) -> f32 {
        STATUS_H * self.z
    }
    pub fn gutter_w(&self) -> f32 {
        GUTTER_W * self.z
    }
    pub fn add_col_w(&self) -> f32 {
        ADD_COL_W * self.z
    }
    pub fn rows_bottom(&self) -> f32 {
        self.height - self.status_h()
    }
    /// 当前可见行范围（首/末行索引；绘制与图片播种共用，保证两口径一致）。
    pub fn visible_rows(&self, total_rows: usize) -> (usize, usize) {
        let first = self.row_at(self.content_y(self.header_h())).unwrap_or(0);
        let last = self
            .row_at(self.content_y(self.rows_bottom() - 1.0))
            .unwrap_or(total_rows.saturating_sub(1));
        (first, last)
    }
    pub fn content_y(&self, local_y: f32) -> f32 {
        local_y - self.header_h() + self.session.view.scroll_y
    }
    pub fn content_x(&self, local_x: f32) -> f32 {
        local_x - self.gutter_w() + self.session.view.scroll_x
    }
    pub fn cols_w(&self) -> f32 {
        self.cache.col_w.iter().sum()
    }
    pub fn total_w(&self) -> f32 {
        self.cache.total_w
    }
    pub fn max_scroll_x(&self) -> f32 {
        // total_w 是内容宽（不含行号列），滚动范围需补回 gutter
        (self.total_w() + self.gutter_w() - self.width).max(0.0)
    }
    /// 横向滚动条滑块几何（无横向可滚 = None）：track = 行号列右侧全宽，滑块宽按视口/内容占比。
    pub fn hscrollbar(&self) -> Option<Rectangle> {
        let max = self.max_scroll_x();
        if max <= 0.0 {
            return None;
        }
        let track_x = self.gutter_w();
        let track_w = (self.width - track_x).max(0.0);
        let thumb_w = (track_w * track_w / (track_w + max)).clamp(24.0, track_w.max(24.0));
        let t = (self.session.view.scroll_x / max).clamp(0.0, 1.0);
        Some(rect(track_x + t * (track_w - thumb_w), self.rows_bottom() - HSCROLLBAR_H, thumb_w, HSCROLLBAR_H))
    }
    /// 竖向滚动条滑块几何（无竖向可滚 = None）：track = 表格区右缘（横向条上方），滑块高按占比。
    pub fn vscrollbar(&self) -> Option<Rectangle> {
        let max = self.max_scroll_y();
        if max <= 0.0 {
            return None;
        }
        let track_y = self.header_h();
        let track_h = (self.rows_bottom() - HSCROLLBAR_H - track_y).max(0.0);
        let thumb_h = (track_h * track_h / (track_h + max)).clamp(24.0, track_h.max(24.0));
        let t = (self.session.view.scroll_y / max).clamp(0.0, 1.0);
        Some(rect(
            self.width - VSCROLLBAR_W,
            track_y + t * (track_h - thumb_h),
            VSCROLLBAR_W,
            thumb_h,
        ))
    }
    pub fn max_scroll_y(&self) -> f32 {
        let content = self.cache.content_h + self.header_h();
        (content - (self.height - self.status_h())).max(0.0)
    }
    pub fn col_at(&self, content_x: f32) -> Option<usize> {
        let n = self.cache.col_w.len();
        if content_x < 0.0 || n == 0 {
            return None;
        }
        for c in 0..n {
            let x0 = self.cache.col_x[c];
            let x1 = x0 + self.cache.col_w[c];
            if content_x < x1 {
                return (content_x >= x0).then_some(c);
            }
        }
        None
    }
    pub fn row_at(&self, content_y: f32) -> Option<usize> {
        let n = self.cache.row_h.len();
        if content_y < 0.0 || n == 0 {
            return None;
        }
        for r in 0..n {
            let y0 = self.cache.row_y[r];
            let y1 = y0 + self.cache.row_h[r];
            if content_y < y1 {
                return (content_y >= y0).then_some(r);
            }
        }
        None
    }
}

/// 命中区域。
pub enum Hit {
    None,
    Corner,
    Header(usize),
    HeaderMenu(usize),
    HeaderResize(usize),
    AddFieldCol,
    Gutter(usize),
    /// 行号格底部行高把手。
    GutterHandle(usize),
    /// 行号格拖拽钮。
    GutterGrip(usize),
    Cell(usize, usize),
    Status(usize),
    NewRow,
    /// 底缘横向滚动条。
    HScrollbar,
    /// 右缘竖向滚动条。
    VScrollbar,
}

/// 画布局部坐标 → 命中区域。
pub fn hit_at(g: &Geo, pos: Point) -> Hit {
    let header_h = g.header_h();
    let status_top = g.rows_bottom();
    if pos.y < header_h {
        if pos.x < g.gutter_w() {
            return Hit::Corner;
        }
        let cx = g.content_x(pos.x);
        if cx >= g.cols_w() {
            return Hit::AddFieldCol;
        }
        if let Some(c) = g.col_at(cx) {
            let right = g.cache.col_x[c] + g.cache.col_w[c];
            let local_x = pos.x - g.gutter_w() + g.session.view.scroll_x;
            // 拖宽把手（右缘 8px）先于 ⋮ 菜单（其左侧 10px）判定：两者区间相邻不重叠
            if local_x > right - COL_HANDLE_W * g.z {
                return Hit::HeaderResize(c);
            }
            if local_x > right - FIELD_BTN_W * g.z {
                return Hit::HeaderMenu(c);
            }
            return Hit::Header(c);
        }
        return Hit::None;
    }
    if pos.y >= status_top {
        if pos.x >= g.gutter_w() {
            let cx = g.content_x(pos.x);
            if let Some(c) = g.col_at(cx) {
                return Hit::Status(c);
            }
        }
        return Hit::None;
    }
    // 横向滚动条条带（压在表格区底缘；有横向可滚才命中）
    if pos.y >= status_top - HSCROLLBAR_H && pos.x >= g.gutter_w() && g.hscrollbar().is_some() {
        return Hit::HScrollbar;
    }
    // 竖向滚动条条带（贴表格区右缘；有竖向可滚才命中）
    if pos.x >= g.width - VSCROLLBAR_W && pos.y >= header_h && g.vscrollbar().is_some() {
        return Hit::VScrollbar;
    }
    let cy = g.content_y(pos.y);
    if cy >= g.cache.content_h {
        // 行号列区段属覆盖层，新行条带从其右缘起（命中与绘制同口径）
        if pos.x < g.gutter_w() {
            return Hit::None;
        }
        return Hit::NewRow;
    }
    if pos.x < g.gutter_w() {
        let Some(r) = g.row_at(cy) else { return Hit::None };
        let row_top = g.cache.row_y[r] - g.session.view.scroll_y + header_h;
        let row_bottom = row_top + g.cache.row_h[r];
        if pos.y >= row_bottom - ROW_HANDLE_H * g.z {
            return Hit::GutterHandle(r);
        }
        let mid = row_top + g.cache.row_h[r] / 2.0;
        if (pos.y - mid).abs() <= GRIP_HIT_R * g.z {
            return Hit::GutterGrip(r);
        }
        return Hit::Gutter(r);
    }
    let cx = g.content_x(pos.x);
    if cx >= g.cols_w() {
        return Hit::None;
    }
    let Some(r) = g.row_at(cy) else { return Hit::None };
    let Some(c) = g.col_at(cx) else { return Hit::None };
    Hit::Cell(r, c)
}

/// 单元格矩形（画布局部坐标）。
pub fn cell_rect(g: &Geo, r: usize, c: usize) -> Rectangle {
    let x = g.cache.col_x[c] - g.session.view.scroll_x + g.gutter_w();
    let y = g.cache.row_y[r] - g.session.view.scroll_y + g.header_h();
    Rectangle::new(Point::new(x, y), Size::new(g.cache.col_w[c], g.cache.row_h[r]))
}

/// 单元格内容自然高度（不含上下内衬；image 区自带高度）。字号、字体与换行宽口径与绘制一致：
/// 基准字号 × zoom 再叠加单元格样式字号，行数取编辑浮层同一套实测行段。
/// `wrap` 为 None = 本轮不做实测（该列正在被拖宽），行数改用字符估算。
fn cell_content_height(
    wrap: Option<&mut WrapCache>,
    field: &TableField,
    row: &TableRow,
    v: Option<&serde_json::Value>,
    col_w: f32,
    zoom: f32,
) -> f32 {
    let Some(v) = v else { return 0.0 };
    if field.field_type == "image" {
        return image_area_height(v, col_w, zoom);
    }
    let st = table_ops::style_of(row, &field.id);
    let size = style_size(&st, base_font_size(field) * zoom, zoom).max(1.0);
    let text = match v.as_str() {
        Some(s) => s.to_string(),
        _ if v.is_number() => v.to_string(),
        _ => String::new(),
    };
    if text.is_empty() {
        return 0.0;
    }
    let wrap_w = wrap_width(col_w, zoom);
    let lines = match wrap {
        Some(wrap) => wrap.lines(&text, size, cell_text_font(&st), wrap_w).len(),
        None => estimate_lines(&text, size, wrap_w).len(),
    };
    lines as f32 * size * LINE_FACTOR
}

/// 图片单元格高度（轮播 = 主图区 + 多图队列条；九宫格 = 方块行高累计 + 内衬）。
fn image_area_height(v: &serde_json::Value, col_w: f32, zoom: f32) -> f32 {
    let images = table_ops::image_entries(v);
    if images.is_empty() {
        return 22.0 * zoom;
    }
    if table_ops::image_display(v) == Some("grid") {
        let cols = grid_cols(col_w, zoom, images.len());
        let tile_w = grid_tile_w(col_w, zoom, cols);
        let rows = images.len().div_ceil(cols);
        let tile_h = tile_w.min(IMAGE_GRID_SINGLE_MAX * zoom);
        rows as f32 * tile_h + (rows.saturating_sub(1)) as f32 * IMAGE_GAP * zoom + 8.0 * zoom
    } else {
        IMAGE_CAROUSEL_H * zoom + if images.len() > 1 { IMAGE_QUEUE_H * zoom } else { 0.0 }
    }
}

fn grid_cols(col_w: f32, zoom: f32, count: usize) -> usize {
    if count <= 2 {
        count
    } else if col_w >= IMAGE_GRID_WIDE_MIN * zoom {
        3
    } else {
        2
    }
}

fn grid_tile_w(col_w: f32, zoom: f32, cols: usize) -> f32 {
    let inner = col_w - 2.0 * CELL_PAD_X * zoom - 2.0 * zoom;
    ((inner - (cols.saturating_sub(1)) as f32 * IMAGE_GAP * zoom) / cols as f32).max(24.0 * zoom)
}

// ===== 文本估算（复用 chrome 估宽的单字符口径；单行截断与缺行段时的兜底）=====

fn text_width(text: &str, size: f32) -> f32 {
    text.chars().map(|c| crate::layout_view::estimate_char_w(c, size)).sum()
}

/// 按字符单位贪心断行的行段：本轮布局没覆盖该口径时的兜底，正文照样画得出，
/// 折行点与编辑浮层可能有字符级差异。
fn estimate_lines(text: &str, size: f32, max_w: f32) -> Vec<TextLine> {
    let mut out = Vec::new();
    for line in text.split('\n') {
        let mut cur = String::new();
        let mut w = 0.0f32;
        for ch in line.chars() {
            let cw = crate::layout_view::estimate_char_w(ch, size);
            if w + cw > max_w && w > 0.0 {
                out.push(TextLine { text: std::mem::take(&mut cur), width: w });
                w = 0.0;
            }
            cur.push(ch);
            w += cw;
        }
        out.push(TextLine { text: cur, width: w });
    }
    out
}

/// 字段基准字号：数字 / 时长列小一档。
fn base_font_size(field: &TableField) -> f32 {
    match field.field_type.as_str() {
        "number" | "duration" => num_font(),
        _ => cell_font(),
    }
}

/// 样式字体映射：默认随界面字体族，`serif` / `mono` 走系统通用族，其余值按族名解析。
pub fn style_font(st: &CellStyle) -> iced::Font {
    let base = crate::ui::regular();
    let family = match st.font.as_deref() {
        Some("serif") => iced::font::Family::Serif,
        Some("mono") => iced::font::Family::Monospace,
        Some(value) => named_family(value),
        None => base.family,
    };
    iced::Font { family, ..base }
}

/// 单元格字体值 → 字体族：旧中文预设键（hei/song/kai/fang）先换成对应系统字体名，
/// 再与族名一起按已安装清单解析；解析不到（含旧 `default` / `sans`）回落界面字体族。
fn named_family(value: &str) -> iced::font::Family {
    let name = match value {
        "hei" => "Microsoft YaHei",
        "song" => "SimSun",
        "kai" => "KaiTi",
        "fang" => "FangSong",
        other => other,
    };
    crate::ui::font::lookup(name).map_or_else(crate::ui::family, iced::font::Family::Name)
}

/// hex 颜色解析（#RGB / #RRGGBB；非法 = None）。数据来自 .atb 文件值，先验十六进制再切片。
pub fn parse_hex(s: &str) -> Option<Color> {
    let s = s.trim_start_matches('#');
    if !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let (r, g, b) = match s.len() {
        3 => (
            u8::from_str_radix(&s[0..1].repeat(2), 16).ok()?,
            u8::from_str_radix(&s[1..2].repeat(2), 16).ok()?,
            u8::from_str_radix(&s[2..3].repeat(2), 16).ok()?,
        ),
        6 => (
            u8::from_str_radix(&s[0..2], 16).ok()?,
            u8::from_str_radix(&s[2..4], 16).ok()?,
            u8::from_str_radix(&s[4..6], 16).ok()?,
        ),
        _ => return None,
    };
    Some(Color::from_rgb8(r, g, b))
}

fn style_color(st: &CellStyle) -> Color {
    st.color.as_deref().and_then(parse_hex).unwrap_or(pal::TEXT_PRIMARY)
}

/// 显示字号：基准字号随 zoom，样式字号（数据值）同样乘 zoom 保持同步缩放。
fn style_size(st: &CellStyle, base: f32, zoom: f32) -> f32 {
    st.size.map_or(base, |s| s as f32 * zoom)
}

// ===== 图片单元格布局 =====

/// 图片单元格子元素布局（绘制与命中共用；坐标相对画布）。
pub struct ImageLayout {
    pub main: Option<Rectangle>,
    /// 队列缩略图（真实图片下标、附件相对路径、绘制区）；窗口随当前页滚动，
    /// 故下标不等于切片位置，命中/重排必须用携带的真实下标。
    pub thumbs: Vec<(usize, String, Rectangle)>,
    /// 九宫格图块（真实图片下标、附件相对路径、绘制区）。
    pub tiles: Vec<(usize, String, Rectangle)>,
    pub mode_btn: Option<Rectangle>,
    pub add_btn: Option<Rectangle>,
    pub remove_btn: Option<Rectangle>,
    /// 空值的整格添加热区。
    pub empty_add: Option<Rectangle>,
}

/// `current` = 轮播当前页（队列条据此滚动，保证当前缩略图可见）。
pub fn image_layout_of(cell: Rectangle, v: &serde_json::Value, zoom: f32, current: usize) -> ImageLayout {
    let images = table_ops::image_entries(v);
    let pad_x = CELL_PAD_X * zoom;
    let pad_y = CELL_PAD_Y * zoom;
    let inner = Rectangle::new(
        Point::new(cell.x + pad_x, cell.y + pad_y),
        Size::new((cell.width - 2.0 * pad_x).max(0.0), (cell.height - 2.0 * pad_y).max(0.0)),
    );
    if images.is_empty() {
        return ImageLayout {
            main: None,
            thumbs: vec![],
            tiles: vec![],
            mode_btn: None,
            add_btn: None,
            remove_btn: None,
            empty_add: Some(inner),
        };
    }
    let btn = 22.0 * zoom;
    let multi = images.len() > 1;
    let current = current.min(images.len() - 1);
    if table_ops::image_display(v) == Some("grid") {
        let cols = grid_cols(cell.width, zoom, images.len());
        let tile_w = grid_tile_w(cell.width, zoom, cols);
        let tile_h = tile_w.min(IMAGE_GRID_SINGLE_MAX * zoom);
        let mut tiles = Vec::new();
        for (i, img) in images.iter().enumerate() {
            let r = i / cols;
            let c = i % cols;
            tiles.push((
                i,
                img.to_string(),
                Rectangle::new(
                    Point::new(
                        inner.x + c as f32 * (tile_w + IMAGE_GAP * zoom),
                        inner.y + r as f32 * (tile_h + IMAGE_GAP * zoom),
                    ),
                    Size::new(tile_w, tile_h),
                ),
            ));
        }
        ImageLayout {
            main: None,
            thumbs: vec![],
            tiles,
            mode_btn: multi.then(|| Rectangle::new(inner.position(), Size::new(btn, btn))),
            add_btn: Some(Rectangle::new(
                Point::new(inner.x + btn + IMAGE_GAP * zoom, inner.y),
                Size::new(btn, btn),
            )),
            remove_btn: None,
            empty_add: None,
        }
    } else {
        let queue_h = if multi { IMAGE_QUEUE_H * zoom } else { 0.0 };
        let main = Rectangle::new(inner.position(), Size::new(inner.width, (inner.height - queue_h).max(0.0)));
        // 队列条仅多图时存在（单图 = 主图区铺满，无缩略图行）；
        // 数量超出格宽时截断（canvas 无逐格裁剪，画出去会盖到邻格，命中也落邻格）；
        // 窗口恒以当前页收尾（无状态派生：图片增删后无需窗口钳制，当前页总可见）
        let mut thumbs = Vec::new();
        if multi {
            let thumb_step = (IMAGE_THUMB + IMAGE_GAP) * zoom;
            let capacity = (((inner.width - 4.0 * zoom) / thumb_step).floor() as usize).max(1);
            let start = current.saturating_sub(capacity - 1);
            for (i, img) in images.iter().enumerate().skip(start).take(capacity) {
                let j = i - start;
                thumbs.push((
                    i,
                    img.to_string(),
                    Rectangle::new(
                        Point::new(
                            inner.x + 2.0 * zoom + j as f32 * (IMAGE_THUMB + IMAGE_GAP) * zoom,
                            inner.y + main.height + 4.0 * zoom,
                        ),
                        Size::new(IMAGE_THUMB * zoom, IMAGE_THUMB * zoom),
                    ),
                ));
            }
        }
        ImageLayout {
            main: Some(main),
            thumbs,
            tiles: vec![],
            mode_btn: multi.then(|| Rectangle::new(inner.position(), Size::new(btn, btn))),
            add_btn: Some(Rectangle::new(
                Point::new(inner.x + btn + IMAGE_GAP * zoom, inner.y),
                Size::new(btn, btn),
            )),
            remove_btn: Some(Rectangle::new(
                Point::new(inner.x + inner.width - btn, inner.y),
                Size::new(btn, btn),
            )),
            empty_add: None,
        }
    }
}

// ===== 部件 =====

/// 表格自绘部件（State = ()；全部会话态在 App.table.view）。
pub struct TableView<'a> {
    pub session: &'a TableSession,
    pub window: iced::window::Id,
}

impl canvas::Program<Message> for TableView<'_> {
    type State = ();

    fn update(
        &self,
        _state: &mut Self::State,
        event: &canvas::Event,
        bounds: Rectangle,
        cursor: iced::mouse::Cursor,
    ) -> Option<iced::widget::Action<Message>> {
        use iced::mouse::Event as MouseEv;
        use canvas::Event as Ev;
        let window = self.window;
        match event {
            Ev::Mouse(MouseEv::ButtonPressed(iced::mouse::Button::Left)) => {
                cursor.position_in(bounds).map(|p| {
                    iced::widget::Action::publish(Message::Table(TableMsg::Press {
                        window,
                        pos: p,
                        size: bounds.size(),
                        origin: bounds.position(),
                    }))
                    .and_capture()
                })
            }
            Ev::Mouse(MouseEv::ButtonPressed(iced::mouse::Button::Right)) => {
                cursor.position_in(bounds).map(|p| {
                    iced::widget::Action::publish(Message::Table(TableMsg::Context { window, pos: p, size: bounds.size() }))
                        .and_capture()
                })
            }
            Ev::Mouse(MouseEv::CursorMoved { .. }) => {
                cursor.position_in(bounds).map(|p| {
                    iced::widget::Action::publish(Message::Table(TableMsg::Move {
                        window,
                        pos: p,
                        size: bounds.size(),
                        origin: bounds.position(),
                    }))
                })
            }
            Ev::Mouse(MouseEv::ButtonReleased(iced::mouse::Button::Left)) => {
                cursor
                    .position_in(bounds)
                    .map(|p| iced::widget::Action::publish(Message::Table(TableMsg::Release(p))).and_capture())
            }
            Ev::Mouse(MouseEv::WheelScrolled { delta }) => {
                // iced 把全窗口鼠标事件广播给所有 widget：只认光标落在画布内的滚轮（防跨面板串扰）
                cursor.position_in(bounds)?;
                let (dx, dy) = match delta {
                    iced::mouse::ScrollDelta::Lines { x, y } => (x * WHEEL_LINE_PX, y * WHEEL_LINE_PX),
                    iced::mouse::ScrollDelta::Pixels { x, y } => (*x, *y),
                };
                Some(iced::widget::Action::publish(Message::Table(TableMsg::Wheel { delta_y: dy, delta_x: dx })))
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &iced::Renderer,
        _theme: &iced::Theme,
        bounds: Rectangle,
        cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        pin_visible_thumbs(self.session, renderer, bounds.size());
        draw_table(self.session, &mut frame, bounds.size(), cursor.position_in(bounds));
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        _state: &Self::State,
        bounds: Rectangle,
        cursor: iced::mouse::Cursor,
    ) -> iced::mouse::Interaction {
        let Some(p) = cursor.position_in(bounds) else {
            return iced::mouse::Interaction::default();
        };
        let g = geo(self.session, bounds.size());
        match hit_at(&g, p) {
            Hit::GutterHandle(_) => iced::mouse::Interaction::ResizingRow,
            Hit::HeaderResize(_) => iced::mouse::Interaction::ResizingColumn,
            Hit::GutterGrip(_) => iced::mouse::Interaction::Grab,
            _ => iced::mouse::Interaction::default(),
        }
    }
}

impl<'a> From<TableView<'a>> for Element<'a, Message> {
    fn from(w: TableView<'a>) -> Element<'a, Message> {
        Canvas::new(w).width(Length::Fill).height(Length::Fill).into()
    }
}

/// 表格 chrome 覆盖层：表头 / 状态栏 / 滚动条。canvas 的文字与图片恒渲染在同画布
/// 所有几何之上（iced 层级限制），行内容画在本画布会透底——由本覆盖层作为后续
/// widget 压住。事件全部让位下层画布（命中仍在 body 命中层）。
pub struct TableChrome<'a> {
    pub session: &'a TableSession,
}

impl canvas::Program<Message> for TableChrome<'_> {
    type State = ();

    fn update(
        &self,
        _state: &mut Self::State,
        _event: &canvas::Event,
        _bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Option<iced::widget::Action<Message>> {
        None
    }

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &iced::Renderer,
        _theme: &iced::Theme,
        bounds: Rectangle,
        cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        draw_chrome(self.session, &mut frame, bounds.size(), cursor.position_in(bounds));
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        _state: &Self::State,
        _bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> iced::mouse::Interaction {
        iced::mouse::Interaction::None
    }
}

impl<'a> From<TableChrome<'a>> for Element<'a, Message> {
    fn from(w: TableChrome<'a>) -> Element<'a, Message> {
        Canvas::new(w).width(Length::Fill).height(Length::Fill).into()
    }
}

/// 构建 Geo（布局缓存由 touch/zoom 变更路径保证最新）。
pub fn geo(session: &TableSession, size: Size) -> Geo<'_> {
    let cache = session.view.layout.as_ref().expect("布局缓存未初始化");
    Geo { session, cache, width: size.width, height: size.height, z: session.view.zoom }
}

// ===== 绘制 =====

/// 可见区缩略图纹理的常驻登记（滚动回来时不重传；预算见 [`THUMB_PIN_BUDGET`]）。
///
/// 只登记已经绘制过、纹理已上传的图：登记只取分配，不触发上传。未就绪的留到下一帧。
fn pin_visible_thumbs(session: &TableSession, renderer: &iced::Renderer, size: Size) {
    use iced::advanced::image::Renderer as _;

    if session.view.images.is_empty() || session.view.layout.is_none() {
        return;
    }
    let g = geo(session, size);
    let (first, last) = g.visible_rows(session.data.rows.len());
    let mut visible: HashSet<String> = HashSet::new();
    for r in first..=last {
        let Some(row) = session.data.rows.get(r) else { continue };
        for field in &session.data.fields {
            if field.field_type != "image" {
                continue;
            }
            if let Some(value) = row.values.get(&field.id) {
                visible.extend(table_ops::image_entries(value).into_iter().map(str::to_string));
            }
        }
    }
    for rel in &visible {
        if session.view.pins.borrow().contains(rel) {
            continue;
        }
        let Some(cached) = session.view.images.get(rel) else { continue };
        // 已绘制过 = 纹理已上传：此时取分配不会触发上传（也不阻塞在这一帧）
        if !cached.drawn.get() || renderer.measure_image(&cached.thumb).is_none() {
            continue;
        }
        if let Ok(allocation) = renderer.load_image(&cached.thumb) {
            session.view.pins.borrow_mut().pin(rel.clone(), allocation, &visible);
        }
    }
}

fn draw_table(session: &TableSession, frame: &mut canvas::Frame, size: Size, hover: Option<Point>) {
    let g = geo(session, size);
    let view = &session.view;
    let data = &session.data;
    let w = g.width;
    let h = g.height;

    // 背景（表头 / 状态栏带体在行内容之后再铺：滚入内容被不透明压住，不透底）
    frame.fill_rectangle(Point::ORIGIN, Size::new(w, h), pal::BG_PRIMARY);

    let region = table_ops::region_of(view.selection.as_ref(), &data.fields, &data.rows);

    // 行 / 列选中淡底
    match &view.selection {
        Some(TableSelection::Row(id)) => {
            if let Some(r) = data.rows.iter().position(|x| &x.id == id) {
                let y = g.cache.row_y[r] - view.scroll_y + g.header_h();
                if y + g.cache.row_h[r] > g.header_h() && y < g.rows_bottom() {
                    frame.fill_rectangle(Point::new(0.0, y), Size::new(w, g.cache.row_h[r]), pal::ACCENT_SOFT);
                }
            }
        }
        Some(TableSelection::Column(id)) => {
            if let Some(c) = data.fields.iter().position(|f| &f.id == id) {
                let x = g.cache.col_x[c] - view.scroll_x + g.gutter_w();
                frame.fill_rectangle(
                    Point::new(x, g.header_h()),
                    Size::new(g.cache.col_w[c], g.rows_bottom() - g.header_h()),
                    pal::ACCENT_SOFT,
                );
            }
        }
        _ => {}
    }

    // 可见行范围
    let (first, last) = g.visible_rows(data.rows.len());

    for r in first..=last {
        let Some(row) = data.rows.get(r) else { continue };
        let y = g.cache.row_y[r] - view.scroll_y + g.header_h();
        let rh = g.cache.row_h[r];
        if y + rh < g.header_h() || y > g.rows_bottom() {
            continue;
        }
        // 行边线（行号列区段由 chrome 覆盖层绘制）
        frame.fill_rectangle(Point::new(g.gutter_w(), y + rh - 1.0), Size::new(w - g.gutter_w(), 1.0), pal::BORDER_SUBTLE);

        // 各列
        for (c, field) in data.fields.iter().enumerate() {
            let x = g.cache.col_x[c] - view.scroll_x + g.gutter_w();
            if x > w || x + g.cache.col_w[c] < g.gutter_w() {
                continue;
            }
            let cell = rect(x, y, g.cache.col_w[c], rh);
            let st = table_ops::style_of(row, &field.id);
            if let Some(bg) = st.bg.as_deref().and_then(parse_hex) {
                frame.fill_rectangle(cell.position(), cell.size(), bg);
            }
            draw_cell_content(session, &g, frame, r, c, cell, row, field, &st, hover);
            frame.fill_rectangle(Point::new(x + g.cache.col_w[c] - 1.0, y), Size::new(1.0, rh), pal::BORDER_SUBTLE);
        }
    }

    // 图片拖拽跟随影子（行循环之外绘制，避免被后画单元格覆盖）
    if let Some(drag) = &view.img_drag {
        if let Some(cached) = view.images.get(&drag.rel) {
            let size = IMAGE_THUMB * g.z * 1.12;
            let rect = Rectangle::new(
                Point::new(drag.pos.x - size / 2.0, drag.pos.y - size / 2.0),
                Size::new(size, size),
            );
            let mut img = iced::advanced::image::Image::new(cached.thumb.clone());
            img.opacity = 0.92;
            frame.draw_image(rect, img);
        }
    }

    // 「+ 新行」（行号列区段属于 chrome 覆盖层，条带从行号列右缘起）
    let new_row_y = g.cache.content_h - view.scroll_y + g.header_h();
    if new_row_y < g.rows_bottom() {
        let nr = rect(g.gutter_w(), new_row_y, (g.total_w() - view.scroll_x - g.gutter_w()).max(0.0), NEW_ROW_H * g.z);
        let hovered = hover.is_some_and(|p| nr.contains(p));
        if hovered {
            frame.fill_rectangle(nr.position(), nr.size(), pal::HOVER);
        }
        draw_icon(frame, "plus", Point::new(nr.x + 10.0 * g.z, nr.y + (nr.height - 12.0 * g.z) / 2.0), 12.0 * g.z, pal::TEXT_MUTED);
        fill_text(
            frame,
            "新行",
            Point::new(nr.x + 26.0 * g.z, nr.y + (nr.height - num_font() * g.z * LINE_FACTOR) / 2.0),
            num_font() * g.z,
            ui_font(),
            pal::TEXT_MUTED,
        );
    }

    // 表头 / 状态栏 / 滚动条在 chrome 覆盖层绘制（draw_chrome）

    // 选中框描边（2px 强调色贴格内缘，编辑态边框同用此框）
    if let Some(region) = region {
        let x0 = g.cache.col_x[region.col_start] - view.scroll_x + g.gutter_w();
        let x1 = g.cache.col_x[region.col_end] + g.cache.col_w[region.col_end] - view.scroll_x + g.gutter_w();
        let y0 = g.cache.row_y[region.row_start] - view.scroll_y + g.header_h();
        let y1 = g.cache.row_y[region.row_end] + g.cache.row_h[region.row_end] - view.scroll_y + g.header_h();
        let bw = 2.0 * g.z;
        for line in [
            rect(x0, y0, x1 - x0, bw),
            rect(x0, y1 - bw, x1 - x0, bw),
            rect(x0, y0, bw, y1 - y0),
            rect(x1 - bw, y0, bw, y1 - y0),
        ] {
            frame.fill_rectangle(line.position(), line.size(), pal::ACCENT);
        }
    }

    // 行拖拽插入线
    if let Some(reo) = &view.reorder {
        if reo.active {
            let y = insert_line_y(&g, reo.insert_at);
            frame.fill_rectangle(Point::new(g.gutter_w(), y - 1.0), Size::new(w - g.gutter_w(), 2.0), pal::ACCENT);
        }
    }
}

fn insert_line_y(g: &Geo, at: usize) -> f32 {
    let y = g.cache.row_y.get(at).copied().unwrap_or(g.cache.content_h - NEW_ROW_H * g.z);
    y - g.session.view.scroll_y + g.header_h()
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> Rectangle {
    Rectangle::new(Point::new(x, y), Size::new(w.max(0.0), h.max(0.0)))
}

fn ui_font() -> iced::Font {
    crate::ui::regular()
}

fn mono_font() -> iced::Font {
    crate::ui::mono()
}

/// 基础文本（左上锚定，Wrapping::None——换行由行段测量负责）。
/// 字距恒用 Advanced：与编辑浮层的排版口径一致，切换编辑时同一行文字才不跳位。
fn fill_text(frame: &mut canvas::Frame, content: &str, pos: Point, size: f32, font: iced::Font, color: Color) {
    if content.is_empty() || size <= 0.0 {
        return;
    }
    frame.fill_text(
        canvas::Text {
            content: content.to_string(),
            position: pos,
            max_width: f32::INFINITY,
            color,
            size: size.into(),
            line_height: iced::advanced::text::LineHeight::Relative(LINE_FACTOR),
            font,
            align_x: iced::advanced::text::Alignment::Left,
            align_y: alignment::Vertical::Top,
            shaping: iced::advanced::text::Shaping::Advanced,
        },
    );
}

/// 图标描边（24 单位视框缩放到 size 见方，lucide 2 单位描边随缩放）。
fn draw_icon(frame: &mut canvas::Frame, name: &str, pos: Point, size: f32, color: Color) {
    let Some(subpaths) = polylines(name) else { return };
    let k = size / 24.0;
    let stroke = Stroke {
        style: canvas::Style::Solid(color),
        width: 2.0 * k,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        line_dash: LineDash { segments: &[], offset: 0 },
    };
    frame.with_save(|f| {
        f.translate(Vector::new(pos.x, pos.y));
        f.scale(k);
        for poly in subpaths {
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
}

/// 估算宽截断（列头 / 状态栏单行文本）。
fn ellipsize_text(text: &str, max_w: f32, size: f32) -> String {
    if text_width(text, size) <= max_w {
        return text.to_string();
    }
    let mut out = String::new();
    for ch in text.chars() {
        if text_width(&format!("{out}{ch}…"), size) > max_w {
            break;
        }
        out.push(ch);
    }
    format!("{out}…")
}

/// chrome 覆盖层内容：行号列 / 表头 / 状态栏带体 + 滚动条 + 图片单元格 hover 圆钮
/// （覆盖在同画布行内容之上，滚动不透底）。
fn draw_chrome(session: &TableSession, frame: &mut canvas::Frame, size: Size, hover: Option<Point>) {
    let g = geo(session, size);
    let view = &session.view;
    // 行号列最先画：部分滚出可视区的行会画过表头/状态栏区带，由其后的不透明底色压回
    draw_gutter(session, &g, frame);
    frame.fill_rectangle(Point::ORIGIN, Size::new(g.width, g.header_h()), pal::BG_SECONDARY);
    draw_header(session, &g, frame);
    frame.fill_rectangle(Point::new(0.0, g.rows_bottom()), Size::new(g.width, g.status_h()), pal::BG_SECONDARY);
    draw_status(session, &g, frame);
    // 横向滚动条（无横向可滚 = 不画）
    if let Some(bar) = g.hscrollbar() {
        let track = rect(g.gutter_w(), bar.y, g.width - g.gutter_w(), HSCROLLBAR_H);
        frame.fill_rectangle(track.position(), track.size(), pal::BG_SUNKEN);
        frame.fill_rectangle(Point::new(track.x, track.y), Size::new(track.width, 1.0), pal::BORDER_SUBTLE);
        frame.fill_rectangle(bar.position(), bar.size(), if view.hscroll_drag.is_some() { pal::ACCENT } else { pal::BORDER_STRONG });
    }
    // 竖向滚动条（无竖向可滚 = 不画）
    if let Some(bar) = g.vscrollbar() {
        let track = rect(g.width - VSCROLLBAR_W, g.header_h(), VSCROLLBAR_W, g.rows_bottom() - HSCROLLBAR_H - g.header_h());
        frame.fill_rectangle(track.position(), track.size(), pal::BG_SUNKEN);
        frame.fill_rectangle(bar.position(), bar.size(), if view.vscroll_drag.is_some() { pal::ACCENT } else { pal::BORDER_STRONG });
    }
    draw_image_buttons(session, &g, frame, hover);
}

/// 图片单元格 hover 圆钮（画在覆盖层：主体画布中图片恒渲染在同画布所有几何之上，
/// 圆钮画在主体画布会被图片压住）。
fn draw_image_buttons(session: &TableSession, g: &Geo, frame: &mut canvas::Frame, hover: Option<Point>) {
    let Some(pos) = hover else { return };
    let view = &session.view;
    let data = &session.data;
    let (first, last) = g.visible_rows(data.rows.len());
    for r in first..=last {
        let y = g.cache.row_y[r] - view.scroll_y + g.header_h();
        let rh = g.cache.row_h[r];
        if y + rh < g.header_h() || y > g.rows_bottom() || !(y..y + rh).contains(&pos.y) {
            continue;
        }
        for (c, field) in data.fields.iter().enumerate() {
            if field.field_type != "image" {
                continue;
            }
            let x = g.cache.col_x[c] - view.scroll_x + g.gutter_w();
            if x > g.width || x + g.cache.col_w[c] < g.gutter_w() || !(x..x + g.cache.col_w[c]).contains(&pos.x) {
                continue;
            }
            let empty = serde_json::json!({ "images": [] });
            let v = data.rows[r].values.get(&field.id).unwrap_or(&empty);
            if table_ops::image_entries(v).is_empty() {
                continue;
            }
            let row = &data.rows[r];
            let idx = view
                .carousel
                .get(&(row.id.clone(), field.id.clone()))
                .copied()
                .unwrap_or(0);
            let layout = image_layout_of(rect(x, y, g.cache.col_w[c], rh), v, g.z, idx);
            if let Some(btn) = layout.mode_btn {
                circle_btn(frame, btn, "columns-2");
            }
            if let Some(btn) = layout.add_btn {
                circle_btn(frame, btn, "plus");
            }
            if let Some(btn) = layout.remove_btn {
                circle_btn(frame, btn, "x");
            }
            return;
        }
    }
}

/// 行号列（画在覆盖层：主体画布中文字与图片恒渲染在同画布所有几何之上，
/// 单元格内容压不住主体画布里的行号底色，横向滚动后须由上层覆盖件盖住）。
fn draw_gutter(session: &TableSession, g: &Geo, frame: &mut canvas::Frame) {
    let view = &session.view;
    let data = &session.data;
    let gw = g.gutter_w();
    frame.fill_rectangle(Point::ORIGIN, Size::new(gw, (g.rows_bottom() - g.header_h()).max(0.0)), pal::BG_SECONDARY);
    let (first, last) = g.visible_rows(data.rows.len());
    for r in first..=last {
        let y = g.cache.row_y[r] - view.scroll_y + g.header_h();
        let rh = g.cache.row_h[r];
        if y + rh < g.header_h() || y > g.rows_bottom() {
            continue;
        }
        let selected_row = matches!(&view.selection, Some(TableSelection::Row(id)) if data.rows.get(r).is_some_and(|x| &x.id == id));
        if selected_row {
            frame.fill_rectangle(Point::new(0.0, y), Size::new(gw, rh), pal::BG_TERTIARY);
            frame.fill_rectangle(Point::new(0.0, y), Size::new(2.0, rh), pal::ACCENT);
        }
        let num = (r + 1).to_string();
        // 行号靠左、拖拽钮靠右，恒显示不重叠
        fill_text(
            frame,
            &num,
            Point::new(6.0 * g.z, y + (rh - num_font() * g.z * LINE_FACTOR) / 2.0),
            num_font() * g.z,
            mono_font(),
            pal::TEXT_MUTED,
        );
        draw_icon(frame, "grip-vertical", Point::new(gw - 15.0 * g.z, y + rh / 2.0 - 5.0 * g.z), 10.0 * g.z, if selected_row { pal::TEXT_SECONDARY } else { pal::BORDER_STRONG });
        frame.fill_rectangle(Point::new(0.0, y + rh - 1.0), Size::new(gw, 1.0), pal::BORDER_SUBTLE);
    }
}

/// 列头：角钮 + 各列名 + ⋮ + 拖宽把手 + 添加字段列。
fn draw_header(session: &TableSession, g: &Geo, frame: &mut canvas::Frame) {
    let view = &session.view;
    let h = g.header_h();
    let font_size = num_font() * g.z;
    draw_icon(frame, "maximize-2", Point::new(g.gutter_w() / 2.0 - 5.0 * g.z, (h - 10.0 * g.z) / 2.0), 10.0 * g.z, pal::TEXT_MUTED);
    for (c, field) in session.data.fields.iter().enumerate() {
        let x = g.cache.col_x[c] - view.scroll_x + g.gutter_w();
        let w = g.cache.col_w[c];
        if x > g.width || x + w < g.gutter_w() {
            continue;
        }
        let col_selected = matches!(&view.selection, Some(TableSelection::Column(id)) if &session.data.fields[c].id == id);
        if col_selected {
            frame.fill_rectangle(Point::new(x, 0.0), Size::new(w, h), pal::ACCENT_SOFT);
        }
        let name = ellipsize_text(&field.name, w - 2.0 * CELL_PAD_X * g.z - FIELD_BTN_W * g.z, font_size);
        fill_text(
            frame,
            &name,
            Point::new(x + CELL_PAD_X * g.z, (h - font_size * LINE_FACTOR) / 2.0),
            font_size,
            ui_font(),
            pal::TEXT_SECONDARY,
        );
        // ⋮ 居于菜单命中区（右缘 18px 内、避开 8px 拖宽把手）
        draw_icon(frame, "ellipsis", Point::new(x + w - FIELD_BTN_W * g.z + (FIELD_BTN_W * g.z - 11.0 * g.z) / 2.0 - 4.0 * g.z, (h - 11.0 * g.z) / 2.0), 11.0 * g.z, pal::TEXT_MUTED);
        frame.fill_rectangle(Point::new(x + w - 1.0, 0.0), Size::new(1.0, h), pal::BORDER);
        frame.fill_rectangle(Point::new(x + w - COL_HANDLE_W * g.z, h - 3.0 * g.z), Size::new(COL_HANDLE_W * g.z, 3.0 * g.z), pal::ACCENT.scale_alpha(0.5));
        frame.fill_rectangle(Point::new(x, h - 1.0), Size::new(w, 1.0), pal::BORDER);
    }
    let add_x = g.gutter_w() + g.cols_w() - view.scroll_x;
    if add_x < g.width {
        frame.fill_rectangle(Point::new(add_x, 0.0), Size::new(g.add_col_w(), h), pal::BG_SECONDARY);
        draw_icon(frame, "plus", Point::new(add_x + (g.add_col_w() - 12.0 * g.z) / 2.0, (h - 12.0 * g.z) / 2.0), 12.0 * g.z, pal::TEXT_MUTED);
        frame.fill_rectangle(Point::new(add_x - 1.0, 0.0), Size::new(1.0, h), pal::BORDER);
        frame.fill_rectangle(Point::new(add_x + g.add_col_w() - 1.0, 0.0), Size::new(1.0, h), pal::BORDER);
    }
    frame.fill_rectangle(Point::new(0.0, h - 1.0), Size::new(g.width, 1.0), pal::BORDER);
}

/// 状态栏：Σ 角格 + 各列计算值。
fn draw_status(session: &TableSession, g: &Geo, frame: &mut canvas::Frame) {
    let y = g.rows_bottom();
    let h = g.status_h();
    draw_icon(frame, "sigma", Point::new((g.gutter_w() - 13.0 * g.z) / 2.0, y + (h - 13.0 * g.z) / 2.0), 13.0 * g.z, pal::TEXT_MUTED);
    for (c, field) in session.data.fields.iter().enumerate() {
        let x = g.cache.col_x[c] - session.view.scroll_x + g.gutter_w();
        let w = g.cache.col_w[c];
        if x > g.width || x + w < g.gutter_w() {
            continue;
        }
        let Some(calc) = table_ops::compute_column_calc(field, &session.data.rows) else { continue };
        let label = format!("{} {}", table_ops::calc_type_label(field.calc_type.as_deref().unwrap_or("")), calc);
        let size = num_font() * g.z;
        let text = ellipsize_text(&label, w - 2.0 * CELL_PAD_X * g.z, size);
        fill_text(
            frame,
            &text,
            Point::new(x + CELL_PAD_X * g.z, y + (h - size * LINE_FACTOR) / 2.0),
            size,
            mono_font(),
            pal::TEXT_PRIMARY,
        );
    }
    frame.fill_rectangle(Point::new(0.0, y), Size::new(g.width, 1.0), pal::BORDER);
}

/// 单元格内容（文本 / 数字 / 单选 / 图片）；编辑中的格不再画原值（覆盖阶段草稿为空除外）。
#[allow(clippy::too_many_arguments)]
fn draw_cell_content(
    session: &TableSession,
    g: &Geo,
    frame: &mut canvas::Frame,
    r: usize,
    c: usize,
    cell: Rectangle,
    row: &TableRow,
    field: &TableField,
    st: &CellStyle,
    hover: Option<Point>,
) {
    let view = &session.view;
    let editing_here = view
        .editing
        .as_ref()
        .is_some_and(|e| e.row_id == row.id && e.field_id == field.id);
    if editing_here {
        return;
    }
    let v = row.values.get(&field.id);
    let base = base_font_size(field) * g.z;
    match field.field_type.as_str() {
        "image" => draw_image_cell(session, g, frame, r, c, cell, v, hover),
        "singleSelect" => {
            let text = v.and_then(serde_json::Value::as_str).unwrap_or("未选择");
            let color = if v.is_some() { style_color(st) } else { pal::TEXT_MUTED };
            let size = style_size(st, base, g.z);
            // 右缘常显下拉箭头（选择弹层入口示意），文本避让箭头区
            let chevron_x = cell.x + cell.width - (CELL_PAD_X + 12.0) * g.z;
            let text = ellipsize_text(text, chevron_x - cell.x - CELL_PAD_X * g.z, size);
            fill_text(
                frame,
                &text,
                Point::new(cell.x + CELL_PAD_X * g.z, cell.y + (cell.height - size * LINE_FACTOR) / 2.0),
                size,
                cell_text_font(st),
                color,
            );
            draw_icon(
                frame,
                "chevron-down",
                Point::new(chevron_x, cell.y + (cell.height - 12.0 * g.z) / 2.0),
                12.0 * g.z,
                color,
            );
        }
        "number" | "duration" => {
            let Some(n) = v.and_then(serde_json::Value::as_f64) else { return };
            let text = if field.field_type == "duration" { format!("{n} 秒") } else { format!("{n}") };
            let size = style_size(st, base, g.z);
            let text = ellipsize_text(&text, cell.width - 2.0 * CELL_PAD_X * g.z, size);
            fill_text(
                frame,
                &text,
                Point::new(cell.x + CELL_PAD_X * g.z, cell.y + (cell.height - size * LINE_FACTOR) / 2.0),
                size,
                cell_text_font(st),
                style_color(st),
            );
        }
        _ => {
            let Some(s) = v.and_then(serde_json::Value::as_str) else { return };
            if s.is_empty() {
                return;
            }
            let size = style_size(st, base, g.z);
            let font = cell_text_font(st);
            let wrap_w = wrap_width(cell.width, g.z);
            // 行段来自本轮布局的实测（与编辑浮层同折行口径）；缺键说明该口径没被覆盖，兜底仍按字符估算断行
            let fallback: Vec<TextLine>;
            let lines: &[TextLine] = match g.cache.wrap.peek(s, size, font, wrap_w) {
                Some(lines) => lines,
                None => {
                    fallback = estimate_lines(s, size, wrap_w);
                    &fallback
                }
            };
            let lh = size * LINE_FACTOR;
            let mut y = cell.y + CELL_PAD_Y * g.z;
            for line in lines {
                fill_text(frame, &line.text, Point::new(cell.x + CELL_PAD_X * g.z, y), size, font, style_color(st));
                let w = line.width.min(wrap_w);
                if st.u {
                    frame.fill_rectangle(Point::new(cell.x + CELL_PAD_X * g.z, y + lh - 1.5), Size::new(w, 1.0), style_color(st));
                }
                if st.s {
                    frame.fill_rectangle(Point::new(cell.x + CELL_PAD_X * g.z, y + lh / 2.0), Size::new(w, 1.0), style_color(st));
                }
                y += lh;
            }
        }
    }
}

/// 图片单元格（轮播 / 九宫格 + hover 圆钮 + 拖拽影子）。
#[allow(clippy::too_many_arguments)]
fn draw_image_cell(
    session: &TableSession,
    g: &Geo,
    frame: &mut canvas::Frame,
    r: usize,
    c: usize,
    cell: Rectangle,
    v: Option<&serde_json::Value>,
    hover: Option<Point>,
) {
    let Some(v) = v else { return };
    let view = &session.view;
    let field = &session.data.fields[c];
    let row = &session.data.rows[r];
    let images = table_ops::image_entries(v);
    let idx = view
        .carousel
        .get(&(row.id.clone(), field.id.clone()))
        .copied()
        .unwrap_or(0)
        .min(images.len().saturating_sub(1));
    let layout = image_layout_of(cell, v, g.z, idx);
    let hovered = hover.is_some_and(|p| cell.contains(p));

    if let Some(empty_add) = layout.empty_add {
        if hovered {
            frame.fill_rectangle(empty_add.position(), empty_add.size(), pal::HOVER);
        }
        draw_icon(frame, "image-plus", Point::new(empty_add.x + (empty_add.width - 14.0 * g.z) / 2.0, empty_add.y + (empty_add.height - 14.0 * g.z) / 2.0), 14.0 * g.z, pal::TEXT_MUTED);
        return;
    }

    if let Some(main) = &layout.main {
        // 轮播：当前页；滑动跟手时补画相邻页（canvas 无逐格裁剪，静止不画邻页防溢出单元格）
        let swipe_dx = view
            .swipe
            .as_ref()
            .filter(|s| s.row_id == row.id && s.field_id == field.id && s.dx != 0.0)
            .map(|s| s.dx)
            .unwrap_or(0.0);
        for offset in [-1i32, 0, 1] {
            if offset != 0 && swipe_dx == 0.0 {
                continue;
            }
            let page = idx as i32 + offset;
            if page < 0 || page as usize >= images.len() {
                continue;
            }
            let x_off = if offset == 0 { swipe_dx } else { swipe_dx + offset as f32 * main.width };
            let rect = Rectangle::new(Point::new(main.x + x_off, main.y), main.size());
            if rect.x + rect.width < cell.x || rect.x > cell.x + cell.width {
                continue;
            }
            draw_thumb_contain(session, frame, images[page as usize], rect);
        }
        // 队列条（窗口随当前页滚动，下标取布局携带的真实图片下标）
        for (img_idx, rel, rect) in &layout.thumbs {
            let current = *img_idx == idx;
            frame.fill_rectangle(rect.position(), rect.size(), pal::BG_SUNKEN);
            draw_thumb_contain(session, frame, rel, shrink(*rect, 2.0 * g.z));
            outline(frame, *rect, 2.0 * g.z, if current { pal::ACCENT } else { pal::BORDER });
        }
        // hover 圆钮在 chrome 覆盖层绘制（draw_image_buttons）：同画布几何会被图片压住
    } else {
        for (_, rel, rect) in &layout.tiles {
            frame.fill_rectangle(rect.position(), rect.size(), pal::BG_SUNKEN);
            draw_thumb_contain(session, frame, rel, *rect);
        }
        // 拖拽占位（被拖格描边）
        if let Some(drag) = &view.img_drag {
            if drag.row_id == row.id && drag.field_id == field.id {
                if let Some((_, _, from_rect)) = layout.tiles.get(drag.from) {
                    outline(frame, *from_rect, 1.0, pal::ACCENT);
                }
            }
        }
    }
}

fn shrink(rect: Rectangle, by: f32) -> Rectangle {
    Rectangle::new(
        Point::new(rect.x + by, rect.y + by),
        Size::new((rect.width - 2.0 * by).max(0.0), (rect.height - 2.0 * by).max(0.0)),
    )
}

/// 单元格图片：按相对路径取缩略图并 contain 绘制（未加载 = 不画）。
fn draw_thumb_contain(session: &TableSession, frame: &mut canvas::Frame, rel: &str, rect: Rectangle) {
    let Some(cached) = session.view.images.get(rel) else { return };
    cached.drawn.set(true);
    draw_image_fit(frame, &cached.thumb, cached.size, rect);
}

/// contain 缩放画图（尺寸取原图像素尺寸）。
fn draw_image_fit(
    frame: &mut canvas::Frame,
    handle: &iced::widget::image::Handle,
    size: (f32, f32),
    rect: Rectangle,
) {
    let (w, h) = size;
    let scale = (rect.width / w).min(rect.height / h);
    let dw = (w * scale).min(rect.width);
    let dh = (h * scale).min(rect.height);
    let bounds = Rectangle::new(
        Point::new(rect.x + (rect.width - dw) / 2.0, rect.y + (rect.height - dh) / 2.0),
        Size::new(dw, dh),
    );
    frame.draw_image(bounds, iced::advanced::image::Image::new(handle.clone()));
}

fn outline(frame: &mut canvas::Frame, area: Rectangle, w: f32, color: Color) {
    for line in [
        rect(area.x, area.y, area.width, w),
        rect(area.x, area.y + area.height - w, area.width, w),
        rect(area.x, area.y, w, area.height),
        rect(area.x + area.width - w, area.y, w, area.height),
    ] {
        frame.fill_rectangle(line.position(), line.size(), color);
    }
}

fn circle_btn(frame: &mut canvas::Frame, area: Rectangle, icon_name: &str) {
    let path = Path::circle(Point::new(area.center_x(), area.center_y()), area.width / 2.0);
    frame.fill(&path, pal::SCRIM);
    let icon_size = area.width * 0.55;
    draw_icon(frame, icon_name, Point::new(area.center_x() - icon_size / 2.0, area.center_y() - icon_size / 2.0), icon_size, pal::TEXT_PRIMARY);
}

// ===== 视图组装 =====

/// 表格视图（工具条 + 自绘主体 + 浮层）；未打开文件 = 空态。
pub fn table_view<'a>(app: &'a crate::App, window: iced::window::Id) -> Element<'a, Message> {
    let Some(session) = &app.table else {
        return crate::ui::controls::empty_state("table", "未打开表格", "从文件面板打开一个 .atb 文件", None);
    };

    // 插件表格视图：面板内以插件面替掉网格（工具条保留，「视图」下拉可切回内置网格）
    if let Some(kind) = session.view.active_view.as_deref() {
        let members = collab_members(app, session);
        return column![
            toolbar(&members, session, window),
            crate::plugin_view::surface_view(app, window, kind, "表格视图"),
        ]
        .width(Length::Fill)
        .height(Length::Fill)
        .into();
    }

    let canvas: Element<Message> = TableView { session, window }.into();
    let chrome: Element<Message> = TableChrome { session }.into();
    let mut stack = iced::widget::Stack::new().push(canvas).push(chrome);
    if let Some(overlay) = editor_overlay(session) {
        stack = stack.push(overlay);
    }
    // 在场协作者（名册 presence.file 匹配当前表格的空间成员）
    let collab_members = collab_members(app, session);

    let body = column![
        toolbar(&collab_members, session, window),
        stack.width(Length::Fill).height(Length::Fill),
    ]
    .width(Length::Fill)
    .height(Length::Fill);
    // 灯箱覆盖整个表格视图（含工具条）：预览期间表格收不到鼠标事件
    let mut root = iced::widget::Stack::new().push(body);
    if let Some(lb) = &session.view.lightbox {
        root = root.push(lightbox_overlay(session, lb, window));
    }
    root.width(Length::Fill).height(Length::Fill).into()
}

/// 在场协作者：名册里 presence.file 指向当前表格的空间成员（颜色 + 昵称）。
fn collab_members(app: &crate::App, session: &TableSession) -> Vec<(iced::Color, String)> {
    if !matches!(app.repo_kind, crate::RepoKind::Space(_)) {
        return Vec::new();
    }
    app.space_peers
        .iter()
        .filter(|p| {
            p.presence
                .as_ref()
                .and_then(|pr| pr.get("file").and_then(|f| f.as_str()))
                .is_some_and(|f| f == session.file)
        })
        .map(|p| (crate::layout_view::parse_hex_color(&p.color).unwrap_or(pal::TEXT_MUTED), p.nickname.clone()))
        .collect()
}

/// 顶部工具条：协作者胶囊 + 行列统计 + 缩放 + ··· 菜单。
fn toolbar<'a>(
    collab_members: &[(iced::Color, String)],
    session: &'a TableSession,
    window: iced::window::Id,
) -> Element<'a, Message> {
    let zoom_label = format!("{}%", (session.view.zoom * 100.0).round() as i32);
    let stats = format!("{} 行 · {} 列", session.data.rows.len(), session.data.fields.len());
    let dirty_pill = if session.dirty {
        crate::ui::controls::status_pill(crate::ui::controls::PillStatus::Pending, "未保存")
    } else {
        space().width(Length::Shrink).into()
    };
    let save_state: Element<Message> = if let Some(err) = &session.save_error {
        text(crate::layout_view::ellipsize(err, 220.0, pal::fs_micro()))
            .size(pal::fs_micro())
            .color(pal::DANGER)
            .into()
    } else {
        space().width(Length::Shrink).into()
    };
    let collab_pills: Element<'static, Message> = crate::layout_view::collab_pill_row(collab_members);
    row![
        view_select(window),
        collab_pills,
        dirty_pill,
        space().width(Length::Fill),
        save_state,
        text(zoom_label).size(pal::fs_caption()).color(pal::TEXT_MUTED),
        text(stats).size(pal::fs_caption()).color(pal::TEXT_SECONDARY),
        icon_btn(window, "ellipsis", "更多", Message::Table(TableMsg::OpenMoreMenu(window))),
    ]
    .spacing(10)
    .padding(iced::Padding::ZERO.horizontal(10.0))
    .height(Length::Fixed(TOOLBAR_H))
    .align_y(alignment::Vertical::Center)
    .into()
}

/// 视图切换下拉（内置「表格」；插件表格视图随插件路线接入）。
fn view_select(window: iced::window::Id) -> Element<'static, Message> {
    iced::widget::button(
        row![
            text("视图").size(pal::fs_caption()).color(pal::TEXT_MUTED),
            text("表格").size(pal::fs_caption()).color(pal::TEXT_PRIMARY),
            crate::ui::icon::icon("chevron-down", 11.0, pal::TEXT_MUTED),
        ]
        .spacing(4),
    )
    .padding(iced::Padding::ZERO.horizontal(6.0).vertical(3.0))
    .style(bar_btn_style())
    .on_press(Message::Table(TableMsg::OpenViewMenu(window)))
    .into()
}

fn icon_btn(
    window: iced::window::Id,
    name: &'static str,
    tip: &'static str,
    on_press: Message,
) -> Element<'static, Message> {
    let btn = iced::widget::button(crate::ui::icon::icon(name, 13.0, pal::TEXT_SECONDARY))
        .padding(4.0)
        .style(|_t, status| iced::widget::button::Style {
            background: matches!(status, iced::widget::button::Status::Hovered)
                .then_some(pal::HOVER.into()),
            ..Default::default()
        })
        .on_press(on_press);
    let tip_label = tip.to_string();
    iced::widget::mouse_area(btn)
        .on_enter(Message::Ui(crate::ui::UiMsg::HoverTip {
            window,
            label: tip_label.clone(),
            box_px: 21.0,
        }))
        .on_exit(Message::Ui(crate::ui::UiMsg::HoverTipEnd { window, label: tip_label }))
        .into()
}

/// 编辑器浮层（覆盖阶段画布仍显示原值；可见编辑 = 画布停画原值、浮层即单元格本身）。
fn editor_overlay<'a>(session: &'a TableSession) -> Option<Element<'a, Message>> {
    let edit = session.view.editing.as_ref()?;
    let (ri, ci) = session.cell_index(&edit.row_id, &edit.field_id)?;
    // 伪尺寸取几何：只用于 cell_rect 等纯位置换算，不得触 max_scroll/rows_bottom（依赖真实宽高）
    let g = geo(session, Size::new(f32::MAX, f32::MAX));
    let cell = cell_rect(&g, ri, ci);
    let field = &session.data.fields[ci];
    let st = table_ops::style_of(&session.data.rows[ri], &field.id);
    let is_text = !matches!(field.field_type.as_str(), "number" | "duration" | "image" | "singleSelect");
    let zoom = session.view.zoom;
    let size = style_size(&st, base_font_size(field) * zoom, zoom);
    // 浮层内衬 = 画布绘制内衬；数字/时长格画布垂直居中，等值上内衬复现同一落点
    let pad_y = if is_text {
        CELL_PAD_Y * zoom
    } else {
        ((cell.height - size * LINE_FACTOR) / 2.0).max(0.0)
    };
    let style_color_value = style_color(&st);
    let editor = text_editor(&edit.content)
        .id(iced::widget::Id::new(CELL_EDITOR_ID))
        .on_action(move |a| Message::Table(TableMsg::Edited(a)))
        .key_binding(move |kp| table_key_binding(&kp, is_text))
        .size(size)
        .font(cell_text_font(&st))
        // 行高与折行同画布实测口径（行段测量用这一套参数）
        .line_height(iced::advanced::text::LineHeight::Relative(LINE_FACTOR))
        .wrapping(iced::advanced::text::Wrapping::WordOrGlyph)
        .padding(iced::Padding::ZERO.horizontal(CELL_PAD_X * zoom).vertical(pad_y))
        .width(cell.width.max(40.0))
        .height(cell.height.max(24.0))
        .style(move |_t, _s| text_editor::Style {
            background: iced::Background::Color(Color::TRANSPARENT),
            // 编辑框不另画边框：选中框（2px 强调色贴格缘）即编辑态边框（与单元格编辑同款式）
            border: iced::Border { color: Color::TRANSPARENT, width: 0.0, radius: 0.0.into() },
            placeholder: pal::TEXT_MUTED,
            value: style_color_value,
            selection: pal::SELECTION,
        });
    // 编辑框不另包 mouse_area：编辑器内按压必被 text_editor 捕获（外层观察不到），
    // 「按压在编辑器内」由 Edited 的鼠标动作（Click/Drag 等）作信号上报
    Some(
        container(editor)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(alignment::Horizontal::Left)
            .align_y(alignment::Vertical::Top)
            .padding(iced::Padding::ZERO.left(cell.x).top(cell.y))
            .into(),
    )
}

/// 编辑器键位重排：Enter = 提交下移（Shift+Enter 仅文本换行）、Esc = 取消；
/// Ctrl+Z/Y 全部吞掉（表格撤销统一走全局订阅路径，编辑器不内建文本撤销）。
fn table_key_binding(kp: &text_editor::KeyPress, is_text: bool) -> Option<text_editor::Binding<Message>> {
    use iced::keyboard::{key::Named, Key, Modifiers};
    use text_editor::Binding as B;
    let m: &Modifiers = &kp.modifiers;
    match &kp.key {
        Key::Named(Named::Enter) if !m.shift() => Some(B::Custom(Message::Table(TableMsg::CommitEnter))),
        Key::Named(Named::Enter) if m.shift() && is_text => Some(B::Enter),
        Key::Named(Named::Enter) => Some(B::Custom(Message::Table(TableMsg::Noop))),
        Key::Named(Named::Escape) => Some(B::Custom(Message::Table(TableMsg::CancelEdit))),
        Key::Character(c)
            if (c.eq_ignore_ascii_case("z") || c.eq_ignore_ascii_case("y")) && m.control() =>
        {
            Some(B::Custom(Message::Table(TableMsg::Noop)))
        }
        _ => text_editor::Binding::from_key_press(kp.clone()),
    }
}

/// 格式气泡的窗口落位（矩形为窗口坐标；宽高均为估算，渲染尺寸由内容自适应）。
struct BarPlacement {
    rect: Rectangle,
    summary: table_ops::StyleSummary,
}

/// 格式气泡落位（单击已选中格弹出；编辑态 / 换选即隐藏）：锚 = 选区左上角，
/// 上方放不下翻到选区下缘，两端按窗口视口钳制——气泡叠在窗口覆盖层，不受面板边界约束。
fn bar_placement(
    session: &TableSession,
    window: iced::window::Id,
    viewport: Size,
) -> Option<BarPlacement> {
    let view = &session.view;
    // 面板内替掉网格的形态（灯箱全屏遮罩、插件表格面）期间不出气泡：气泡在窗口层会盖过它们
    if view.editing.is_some()
        || view.lightbox.is_some()
        || view.active_view.is_some()
        || !view.bar_shown
        || view.selection.is_none()
    {
        return None;
    }
    let (_, origin) = view.canvas_origin.filter(|(w, _)| *w == window)?;
    let region = table_ops::region_of(view.selection.as_ref(), &session.data.fields, &session.data.rows)?;
    // 伪尺寸取几何：只用于 cell_rect 等纯位置换算，不得触 max_scroll/rows_bottom（依赖真实宽高）
    let g = geo(session, Size::new(f32::MAX, f32::MAX));
    let x0 = (g.cache.col_x[region.col_start] - view.scroll_x + g.gutter_w()).max(8.0);
    let y0 = g.cache.row_y[region.row_start] - view.scroll_y + g.header_h();
    let y1 = g.cache.row_y[region.row_end] + g.cache.row_h[region.row_end] - view.scroll_y + g.header_h();
    let summary = table_ops::style_summary(&session.data.fields, &session.data.rows, &region);
    let w = bar_est_width(&bar_font_label(&summary), &bar_size_label(&summary));
    let h = bar_est_height();
    // 竖向留在网格区（画布矩形）内：上方贴不进（首行、或选区已滚出网格顶缘）就翻到选区下缘，
    // 越界则贴网格上下缘——气泡只横向越出面板，不压宿主标题栏与状态栏
    let grid_top = origin.y;
    let grid_bottom = (origin.y + view.canvas_size.height).min(viewport.height);
    let above = grid_top + y0 - h - BAR_GAP;
    let want_y = if above >= grid_top { above } else { grid_top + y1 + BAR_GAP };
    let max_y = (grid_bottom - h - pal::VIEWPORT_GAP).max(grid_top);
    // 横向钳位用估宽：真实气泡不大于估宽，钳后左侧起点仍留得住完整一条
    let max_x = (viewport.width - w - pal::VIEWPORT_GAP).max(pal::VIEWPORT_GAP);
    Some(BarPlacement {
        rect: Rectangle::new(
            Point::new((origin.x + x0).clamp(pal::VIEWPORT_GAP, max_x), want_y.clamp(grid_top, max_y)),
            Size::new(w, h),
        ),
        summary,
    })
}

/// 格式气泡估高：按钮字阶行高（12px 档 = 16，与菜单条目同口径）随界面基准字号放大 + 上下内衬。
/// 驱动窗口内的翻转与钳制判据，渲染高度仍由内容自适应（不做实测回填）。
fn bar_est_height() -> f32 {
    pal::fs_caption() * (16.0 / 12.0) + 2.0 * (BAR_PAD_Y + BAR_BTN_PAD_Y)
}

/// 格式气泡估宽：当前标签的估字宽 + 固定件之和 + 余量（仅供钳制与命中判定，不参与渲染）。
fn bar_est_width(font_label: &str, size_label: &str) -> f32 {
    let fs = pal::fs_caption();
    let chip = |label: &str| estimate_text_width(label, fs) + 2.0 * BAR_BTN_PAD_X;
    2.0 * BAR_PAD_X
        + chip(font_label)
        + chip(size_label)
        + 4.0 * chip("B")
        + 2.0 * (estimate_text_width("字", fs) + BAR_SWATCH + BAR_SWATCH_GAP + 2.0 * BAR_BTN_PAD_X)
        + (BAR_ERASER_ICON + 2.0 * BAR_ERASER_PAD_X)
        + 3.0 * BAR_DIVIDER_W
        + 11.0 * BAR_ITEM_GAP
        + BAR_W_MARGIN
}

/// 本窗口是否渲染表格视图，及其格式气泡落位（面板已切走 / 不满足弹出条件 = None）。
fn bar_in_window<'a>(
    app: &'a crate::App,
    window: iced::window::Id,
    viewport: Size,
) -> Option<(&'a TableSession, BarPlacement)> {
    // 接管面与设置整页把整窗内容换掉（布局真源不动），此时网格不在场
    if app.plugin_takeovers.contains_key(&window)
        || (app.main_id == Some(window) && app.settings_open)
    {
        return None;
    }
    if !crate::layout_view::window_shows_table(app, window) {
        return None;
    }
    let session = app.table.as_ref()?;
    let placement = bar_placement(session, window, viewport)?;
    Some((session, placement))
}

/// 格式气泡的矩形（窗口坐标，宽高为估算）：供全局按压判定「点在气泡上 = 表格面按压」。
pub fn format_bar_rect(app: &crate::App, window: iced::window::Id, viewport: Size) -> Option<Rectangle> {
    bar_in_window(app, window, viewport).map(|(_, p)| p.rect)
}

/// 格式气泡覆盖层：由窗口覆盖层作为追加子叠在面板之上（显隐不重建面板子树）。
pub fn format_bar_overlay<'a>(
    app: &'a crate::App,
    window: iced::window::Id,
    viewport: Size,
) -> Option<Element<'a, Message>> {
    let (_session, placement) = bar_in_window(app, window, viewport)?;
    let summary = &placement.summary;
    let bar = row![
        font_pick(app, window, summary),
        size_pick(window, summary),
        divider_el(),
        tri_btn("B", StyleKey::Bold, &summary.bold),
        tri_btn("I", StyleKey::Italic, &summary.italic),
        tri_btn("U", StyleKey::Underline, &summary.underline),
        tri_btn("S", StyleKey::Strike, &summary.strike),
        divider_el(),
        color_btn(app, window, false, &summary.color),
        color_btn(app, window, true, &summary.bg),
        divider_el(),
        clear_btn(),
    ]
    .spacing(BAR_ITEM_GAP)
    .padding(iced::Padding::ZERO.horizontal(BAR_PAD_X).vertical(BAR_PAD_Y));
    let bar = container(bar).style(|_t| iced::widget::container::Style {
        background: Some(pal::BG_OVERLAY.into()),
        border: iced::border::rounded(pal::RADIUS_SM).color(pal::BORDER_STRONG).width(1.0),
        ..Default::default()
    });
    Some(
        container(bar)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(alignment::Horizontal::Left)
            .align_y(alignment::Vertical::Top)
            .padding(iced::Padding::ZERO.left(placement.rect.x).top(placement.rect.y))
            .into(),
    )
}

fn divider_el() -> Element<'static, Message> {
    container(space())
        .width(Length::Fixed(BAR_DIVIDER_W))
        .height(Length::Fixed(18.0))
        .style(|_t| iced::widget::container::Style {
            background: Some(pal::BORDER.into()),
            ..Default::default()
        })
        .into()
}

/// 字体按钮标签（估宽与渲染同一取词口径）。
fn bar_font_label(summary: &table_ops::StyleSummary) -> String {
    match &summary.font {
        OptState::Mixed => "字体".to_string(),
        OptState::Value(None) => "默认".to_string(),
        OptState::Value(Some(f)) => table_ops_font_label(f),
    }
}

/// 字号按钮标签（估宽与渲染同一取词口径）。
fn bar_size_label(summary: &table_ops::StyleSummary) -> String {
    match &summary.size {
        OptState::Value(Some(s)) => format!("{}", *s as i32),
        _ => "字号".to_string(),
    }
}

fn font_pick<'a>(app: &'a crate::App, window: iced::window::Id, summary: &table_ops::StyleSummary) -> Element<'a, Message> {
    let btn = iced::widget::button(text(bar_font_label(summary)).size(pal::fs_caption()).color(pal::TEXT_PRIMARY))
        .padding(iced::Padding::ZERO.horizontal(BAR_BTN_PAD_X).vertical(BAR_BTN_PAD_Y))
        .style(bar_btn_style())
        .on_press_maybe(app.table.as_ref().map(|_| Message::Table(TableMsg::OpenFontMenu(window))));
    btn.into()
}

fn size_pick(window: iced::window::Id, summary: &table_ops::StyleSummary) -> Element<'static, Message> {
    iced::widget::button(text(bar_size_label(summary)).size(pal::fs_caption()).color(pal::TEXT_PRIMARY))
        .padding(iced::Padding::ZERO.horizontal(BAR_BTN_PAD_X).vertical(BAR_BTN_PAD_Y))
        .style(bar_btn_style())
        .on_press(Message::Table(TableMsg::OpenSizeMenu(window)))
        .into()
}

/// 单元格字体值的菜单标签：旧中文预设键保留中文名，其余值即已安装族名，原样显示。
fn table_ops_font_label(key: &str) -> String {
    match key {
        "default" => "默认",
        "sans" => "无衬线",
        "serif" => "衬线",
        "mono" => "等宽",
        "hei" => "黑体",
        "song" => "宋体",
        "kai" => "楷体",
        "fang" => "仿宋",
        other => return other.to_string(),
    }
    .to_string()
}

fn bar_btn_style() -> impl Fn(&iced::Theme, iced::widget::button::Status) -> iced::widget::button::Style {
    |_t, status| iced::widget::button::Style {
        background: matches!(status, iced::widget::button::Status::Hovered).then_some(pal::HOVER.into()),
        ..Default::default()
    }
}

/// 布尔三态按钮：全开 = 强调实底，mixed = 淡强调底。
fn tri_btn(label: &'static str, key: StyleKey, state: &OptState<bool>) -> Element<'static, Message> {
    let (on, mixed) = match state {
        OptState::Value(Some(true)) => (true, false),
        OptState::Mixed => (false, true),
        _ => (false, false),
    };
    iced::widget::button(text(label).size(pal::fs_caption()).color(if on { pal::ACCENT } else { pal::TEXT_PRIMARY }))
        .padding(iced::Padding::ZERO.horizontal(BAR_BTN_PAD_X).vertical(BAR_BTN_PAD_Y))
        .style(move |_t, status| iced::widget::button::Style {
            background: (on || mixed || matches!(status, iced::widget::button::Status::Hovered))
                .then_some(if on { pal::ACCENT_SOFT } else { pal::HOVER }.into()),
            ..Default::default()
        })
        .on_press(Message::Table(TableMsg::StyleToggle(key)))
        .into()
}

fn color_btn<'a>(app: &'a crate::App, window: iced::window::Id, bg: bool, state: &OptState<String>) -> Element<'a, Message> {
    let current = match state {
        OptState::Value(Some(c)) => parse_hex(c),
        OptState::Value(None) => None,
        OptState::Mixed => None,
    };
    let swatch = container(space())
        .width(Length::Fixed(BAR_SWATCH))
        .height(Length::Fixed(BAR_SWATCH))
        .style(move |_t| iced::widget::container::Style {
            background: Some(current.unwrap_or(pal::TEXT_PRIMARY).into()),
            border: iced::border::rounded(pal::RADIUS_XS).color(pal::BORDER_STRONG).width(1.0),
            ..Default::default()
        });
    let label = if bg { "底" } else { "字" };
    let btn = iced::widget::button(
        row![text(label).size(pal::fs_caption()).color(pal::TEXT_PRIMARY), swatch].spacing(BAR_SWATCH_GAP),
    )
    .padding(iced::Padding::ZERO.horizontal(BAR_BTN_PAD_X).vertical(BAR_BTN_PAD_Y))
    .style(bar_btn_style())
    .on_press_maybe(
        app.table
            .as_ref()
            .map(|_| Message::Table(TableMsg::OpenColorMenu { window, bg })),
    );
    btn.into()
}

fn clear_btn() -> Element<'static, Message> {
    iced::widget::button(crate::ui::icon::icon("eraser", BAR_ERASER_ICON, pal::TEXT_PRIMARY))
        .padding(iced::Padding::ZERO.horizontal(BAR_ERASER_PAD_X).vertical(BAR_BTN_PAD_Y))
        .style(bar_btn_style())
        .on_press(Message::Table(TableMsg::ClearFormat))
        .into()
}

// ===== 灯箱几何与交互 =====

/// 灯箱可视区（四周留遮罩边距）。
fn lightbox_inner(view: Size) -> Rectangle {
    Rectangle::new(
        Point::new(LIGHTBOX_MARGIN, LIGHTBOX_MARGIN),
        Size::new(
            (view.width - 2.0 * LIGHTBOX_MARGIN).max(1.0),
            (view.height - 2.0 * LIGHTBOX_MARGIN).max(1.0),
        ),
    )
}

/// 当前灯箱图片的缓存（索引越界或未加载 = None）。
pub fn lightbox_image<'a>(session: &'a TableSession, lb: &Lightbox) -> Option<&'a CachedImage> {
    let images = session
        .data
        .rows
        .iter()
        .find(|r| r.id == lb.row_id)?
        .values
        .get(&lb.field_id)
        .map(table_ops::image_entries)?;
    let index = lb.index.min(images.len().checked_sub(1)?);
    session.view.images.get(images[index])
}

/// 适应可视区并乘以倍率后的显示尺寸。
fn lightbox_display(view: Size, img: (f32, f32), zoom: f32) -> Size {
    let inner = lightbox_inner(view);
    let scale = (inner.width / img.0).min(inner.height / img.1) * zoom;
    Size::new(img.0 * scale, img.1 * scale)
}

/// 平移量按显示尺寸夹紧：放得下的方向居中，放不下的方向不露白边。
pub fn lightbox_clamp_pan(view: Size, img: (f32, f32), zoom: f32, pan: Vector) -> Vector {
    let inner = lightbox_inner(view);
    let d = lightbox_display(view, img, zoom);
    let max_x = ((d.width - inner.width) / 2.0).max(0.0);
    let max_y = ((d.height - inner.height) / 2.0).max(0.0);
    Vector::new(pan.x.clamp(-max_x, max_x), pan.y.clamp(-max_y, max_y))
}

/// 图片显示矩形（可视区居中 + 夹紧后的平移）。
fn lightbox_rect(view: Size, img: (f32, f32), zoom: f32, pan: Vector) -> Rectangle {
    let inner = lightbox_inner(view);
    let d = lightbox_display(view, img, zoom);
    let pan = lightbox_clamp_pan(view, img, zoom, pan);
    let c = inner.center();
    Rectangle::new(Point::new(c.x + pan.x - d.width / 2.0, c.y + pan.y - d.height / 2.0), d)
}

/// 滚轮缩放后的倍率（指数曲线；winit 滚上为正 → 放大）。
pub fn lightbox_zoom_level(zoom: f32, delta_y: f32) -> f32 {
    (zoom * (delta_y * LIGHTBOX_ZOOM_RATE).exp()).clamp(1.0, LIGHTBOX_MAX_ZOOM)
}

/// 以光标为锚点缩放：缩放前后光标下的图片点保持不动（两端平移均夹紧）。
pub fn lightbox_zoom_pan(view: Size, img: (f32, f32), zoom: f32, next: f32, pan: Vector, cursor: Point) -> Vector {
    let c = lightbox_inner(view).center();
    let cv = Vector::new(cursor.x - c.x, cursor.y - c.y);
    let pan = lightbox_clamp_pan(view, img, zoom, pan);
    lightbox_clamp_pan(view, img, next, cv - (cv - pan) * (next / zoom))
}

/// 灯箱交互画布：绘制当前图片，接管滚轮缩放、拖动平移、双击复位与遮罩点击关闭。
/// 覆盖整个表格视图（含工具条），预览期间表格收不到鼠标事件。
struct LightboxCanvas<'a> {
    session: &'a TableSession,
    lb: &'a Lightbox,
    window: iced::window::Id,
}

impl LightboxCanvas<'_> {
    fn image_rect(&self, view: Size) -> Option<Rectangle> {
        let cached = lightbox_image(self.session, self.lb)?;
        let (w, h) = cached.size;
        (w > 0.0 && h > 0.0).then(|| lightbox_rect(view, (w, h), self.lb.zoom, self.lb.pan))
    }
}

impl canvas::Program<Message> for LightboxCanvas<'_> {
    type State = ();

    fn update(
        &self,
        _state: &mut Self::State,
        event: &canvas::Event,
        bounds: Rectangle,
        cursor: iced::mouse::Cursor,
    ) -> Option<iced::widget::Action<Message>> {
        use canvas::Event as Ev;
        use iced::mouse::Event as MouseEv;
        match event {
            Ev::Mouse(MouseEv::WheelScrolled { delta }) => {
                let pos = cursor.position_in(bounds)?;
                let dy = match delta {
                    iced::mouse::ScrollDelta::Lines { y, .. } => y * WHEEL_LINE_PX,
                    iced::mouse::ScrollDelta::Pixels { y, .. } => *y,
                };
                Some(
                    iced::widget::Action::publish(Message::Table(TableMsg::LightboxZoom {
                        pos,
                        view: bounds.size(),
                        delta_y: dy,
                    }))
                    .and_capture(),
                )
            }
            Ev::Mouse(MouseEv::ButtonPressed(iced::mouse::Button::Left)) => {
                let pos = cursor.position_in(bounds)?;
                let inside = self.image_rect(bounds.size()).is_some_and(|r| r.contains(pos));
                let msg = if inside { TableMsg::LightboxPress(pos) } else { TableMsg::LightboxClose };
                Some(iced::widget::Action::publish(Message::Table(msg)).and_capture())
            }
            Ev::Mouse(MouseEv::ButtonPressed(iced::mouse::Button::Right)) => {
                let pos = cursor.position_in(bounds)?;
                let inside = self.image_rect(bounds.size()).is_some_and(|r| r.contains(pos));
                if inside {
                    return Some(
                        iced::widget::Action::publish(Message::Table(TableMsg::OpenLightboxMenu(self.window)))
                            .and_capture(),
                    );
                }
                Some(iced::widget::Action::capture())
            }
            Ev::Mouse(MouseEv::CursorMoved { .. }) => {
                let pos = cursor.position_in(bounds)?;
                // 预览为模态：预览层内吞掉全部移动，下层工具条不产生悬停提示
                if self.lb.drag.is_none() {
                    return Some(iced::widget::Action::capture());
                }
                Some(
                    iced::widget::Action::publish(Message::Table(TableMsg::LightboxPan {
                        pos: Some(pos),
                        view: bounds.size(),
                    }))
                    .and_capture(),
                )
            }
            Ev::Mouse(MouseEv::ButtonReleased(iced::mouse::Button::Left)) => {
                self.lb.drag?;
                let msg = TableMsg::LightboxPan { pos: None, view: bounds.size() };
                Some(iced::widget::Action::publish(Message::Table(msg)).and_capture())
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &iced::Renderer,
        _theme: &iced::Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let Some(cached) = lightbox_image(self.session, self.lb) else { return Vec::new() };
        let (w, h) = cached.size;
        if w <= 0.0 || h <= 0.0 {
            return Vec::new();
        }
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let rect = lightbox_rect(bounds.size(), (w, h), self.lb.zoom, self.lb.pan);
        // 原图解码未完成时先铺缩略图（同矩形；原图就绪后覆盖其上）
        for handle in [cached.thumb.clone(), cached.handle.clone()] {
            frame.draw_image(rect, iced::advanced::image::Image::new(handle));
        }
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        _state: &Self::State,
        bounds: Rectangle,
        cursor: iced::mouse::Cursor,
    ) -> iced::mouse::Interaction {
        if self.lb.zoom <= 1.0 {
            return iced::mouse::Interaction::default();
        }
        if self.lb.drag.is_some() {
            return iced::mouse::Interaction::Grabbing;
        }
        let Some(pos) = cursor.position_in(bounds) else {
            return iced::mouse::Interaction::default();
        };
        if self.image_rect(bounds.size()).is_some_and(|r| r.contains(pos)) {
            iced::mouse::Interaction::Grab
        } else {
            iced::mouse::Interaction::default()
        }
    }
}

impl<'a> From<LightboxCanvas<'a>> for Element<'a, Message> {
    fn from(w: LightboxCanvas<'a>) -> Element<'a, Message> {
        Canvas::new(w).width(Length::Fill).height(Length::Fill).into()
    }
}

/// 灯箱：画布接管滚轮缩放 / 拖动平移 / 双击复位；点击图片外关闭；右键图片弹下载菜单；
/// 多图显示左右翻页与计数。
fn lightbox_overlay<'a>(
    session: &'a TableSession,
    lb: &'a Lightbox,
    window: iced::window::Id,
) -> Element<'a, Message> {
    let images = session
        .data
        .rows
        .iter()
        .find(|r| r.id == lb.row_id)
        .and_then(|r| r.values.get(&lb.field_id))
        .map(table_ops::image_entries)
        .unwrap_or_default();
    let empty = images.is_empty();
    let index = lb.index.min(images.len().saturating_sub(1));
    let multi = !empty && images.len() > 1;
    let scrim = container(space())
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_t| iced::widget::container::Style {
            background: Some(iced::Background::Color(pal::SCRIM_STRONG)),
            ..Default::default()
        });
    let canvas: Element<'_, Message> = LightboxCanvas { session, lb, window }.into();
    let mut stack = iced::widget::Stack::new().push(scrim).push(canvas);
    // 图片未就绪时由文字层提示；文字层不吃事件，滚轮与按压仍由画布接管
    if empty {
        stack = stack.push(lightbox_placeholder("图片已清空"));
    } else if lightbox_image(session, lb).is_none() {
        stack = stack.push(lightbox_placeholder("图片加载中"));
    }
    if multi {
        let nav_btn = |name: &'static str, msg: Message| -> Element<'static, Message> {
            iced::widget::button(circle_btn_element(name))
                .padding(0.0)
                .style(|_t, _s| iced::widget::button::Style::default())
                .on_press(msg)
                .into()
        };
        stack = stack.push(
            container(nav_btn("chevron-left", Message::Table(TableMsg::LightboxNav(-1))))
                .width(Length::Fill)
                .height(Length::Fill)
                .align_y(alignment::Vertical::Center)
                .align_x(alignment::Horizontal::Left)
                .padding(8.0),
        );
        stack = stack.push(
            container(nav_btn("chevron-right", Message::Table(TableMsg::LightboxNav(1))))
                .width(Length::Fill)
                .height(Length::Fill)
                .align_y(alignment::Vertical::Center)
                .align_x(alignment::Horizontal::Right)
                .padding(8.0),
        );
        stack = stack.push(
            container(text(format!("{}/{}", index + 1, images.len())).size(pal::fs_caption()).color(pal::TEXT_PRIMARY))
                .width(Length::Fill)
                .height(Length::Fill)
                .align_y(alignment::Vertical::Bottom)
                .align_x(alignment::Horizontal::Center)
                .padding(8.0),
        );
    }
    stack.width(Length::Fill).height(Length::Fill).into()
}

fn lightbox_placeholder(label: &'static str) -> Element<'static, Message> {
    container(text(label).size(pal::fs_caption()).color(pal::TEXT_MUTED))
        .width(Length::Fill)
        .height(Length::Fill)
        .center(iced::Length::Fill)
        .into()
}

/// 灯箱翻页圆钮（组件库外自绘口径，与图片单元格 hover 圆钮同款）。
fn circle_btn_element(name: &'static str) -> Element<'static, Message> {
    container(crate::ui::icon::icon(name, 16.0, pal::TEXT_PRIMARY))
        .padding(8.0)
        .style(|_t| iced::widget::container::Style {
            background: Some(iced::Background::Color(pal::SCRIM)),
            border: iced::border::rounded(16.0),
            ..Default::default()
        })
        .into()
}

impl TableSession {
    pub fn cell_index(&self, row_id: &str, field_id: &str) -> Option<(usize, usize)> {
        let r = self.data.rows.iter().position(|r| r.id == row_id)?;
        let c = self.data.fields.iter().position(|f| f.id == field_id)?;
        Some((r, c))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::{TableField, TableFile, TableRow};

    fn text_session(values: &[&str]) -> TableSession {
        let field = TableField {
            id: "f1".to_string(),
            name: "正文".to_string(),
            field_type: "text".to_string(),
            options: None,
            width: Some(120.0),
            calc_type: None,
        };
        let rows = values
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let mut cell = serde_json::Map::new();
                cell.insert("f1".to_string(), serde_json::Value::String(v.to_string()));
                TableRow { id: format!("r{i}"), values: cell, height: None, styles: None }
            })
            .collect();
        TableSession::new(
            "t.atb".to_string(),
            TableFile {
                schema: String::new(),
                id: "t".to_string(),
                title: String::new(),
                fields: vec![field],
                rows,
                created_at: 0,
                updated_at: 0,
            },
        )
    }

    /// 自动行高按实测行段累加：一行放不下的正文把行撑高（同批行段供画布绘制与编辑浮层共用）。
    #[test]
    fn auto_row_height_follows_measured_lines() {
        let long = "需要折行的很长很长很长很长很长很长很长很长很长的一段中文正文";
        let s = text_session(&["短", long]);
        let layout = s.view.layout.as_ref().expect("布局缓存在载入时建立");
        assert_eq!(layout.row_h[0], 32.0, "单行正文停在行高下限");

        let size = cell_font();
        let lines = layout
            .wrap
            .peek(long, size, cell_text_font(&CellStyle::default()), wrap_width(120.0, 1.0))
            .expect("布局实测后行段可取");
        assert!(lines.len() > 1, "放不进一行的正文要折出多行");
        assert_eq!(layout.row_h[1], lines.len() as f32 * size * LINE_FACTOR + 2.0 * CELL_PAD_Y);
    }

    /// 1× 时图片按可视区等比铺满并居中（四周遮罩边距不变）。
    #[test]
    fn lightbox_fit_is_centered_in_inner_area() {
        let view = Size::new(1000.0, 800.0);
        let rect = lightbox_rect(view, (4000.0, 2000.0), 1.0, Vector::ZERO);
        let inner = lightbox_inner(view);
        assert!((rect.width - inner.width).abs() < 0.01, "长边贴满可视区宽度");
        assert!((rect.center().x - inner.center().x).abs() < 0.01);
        assert!((rect.center().y - inner.center().y).abs() < 0.01);
    }

    /// 平移只在图片超出可视区的方向生效，且边缘不露白。
    #[test]
    fn lightbox_pan_clamps_to_visible_edges() {
        let view = Size::new(1000.0, 800.0);
        let img = (4000.0, 2000.0);
        assert_eq!(lightbox_clamp_pan(view, img, 1.0, Vector::new(100.0, -100.0)), Vector::ZERO, "放得下时居中");
        let d = lightbox_display(view, img, 2.0);
        let inner = lightbox_inner(view);
        let max_x = (d.width - inner.width) / 2.0;
        let max_y = (d.height - inner.height) / 2.0;
        let clamped = lightbox_clamp_pan(view, img, 2.0, Vector::new(1000.0, -1000.0));
        assert!((clamped.x - max_x).abs() < 0.01);
        assert!((clamped.y + max_y).abs() < 0.01);
    }

    /// 光标锚点缩放：光标下的图片点在缩放前后保持不动。
    #[test]
    fn lightbox_zoom_keeps_cursor_anchor() {
        let view = Size::new(1000.0, 800.0);
        let img = (4000.0, 2000.0);
        let cursor = Point::new(700.0, 300.0);
        let c = lightbox_inner(view).center();
        let old_pan = Vector::new(10.0, -20.0);
        let new_pan = lightbox_zoom_pan(view, img, 2.0, 3.0, old_pan, cursor);
        let before = Vector::new(cursor.x - c.x, cursor.y - c.y) - old_pan;
        let after = Vector::new(cursor.x - c.x, cursor.y - c.y) - new_pan;
        assert!((before.x / 2.0 - after.x / 3.0).abs() < 0.01);
        assert!((before.y / 2.0 - after.y / 3.0).abs() < 0.01);
    }

    /// 滚轮倍率夹紧在 [适应窗口, 上限]。
    #[test]
    fn lightbox_zoom_level_is_clamped() {
        assert_eq!(lightbox_zoom_level(1.0, -100.0), 1.0);
        assert_eq!(lightbox_zoom_level(8.0, 100.0), 8.0);
        assert!(lightbox_zoom_level(1.0, 40.0) > 1.0);
    }

    /// 选中首列某格的格式气泡会话（origin = 画布在窗口中的原点，None = 画布未收到过事件）。
    fn bar_session(window: iced::window::Id, origin: Option<Point>, row_id: &str) -> TableSession {
        let mut s = text_session(&["短", "短"]);
        s.view.selection = Some(TableSelection::Cell {
            row_id: row_id.to_string(),
            field_id: "f1".to_string(),
        });
        s.view.bar_shown = true;
        s.view.canvas_origin = origin.map(|p| (window, p));
        // 画布尺寸与原点同源播种（气泡竖向留在网格区内）
        s.view.canvas_size = Size::new(600.0, 300.0);
        s
    }

    /// 网格几何里的选区上下缘换算到窗口坐标（zoom 1 时 header_h 与行高均为 32）。
    fn sel_edges(s: &TableSession, origin: Point, row: usize) -> (f32, f32) {
        let g = geo(s, Size::new(f32::MAX, f32::MAX));
        let top = origin.y + g.cache.row_y[row] - s.view.scroll_y + g.header_h();
        let bottom = origin.y + g.cache.row_y[row] + g.cache.row_h[row] - s.view.scroll_y + g.header_h();
        (top, bottom)
    }

    /// 靠右窗边的选区：气泡按窗口视口钳位、整条留在窗口内（钳制基准是窗口，不是面板）。
    #[test]
    fn format_bar_placement_clamps_inside_window() {
        let window = iced::window::Id::unique();
        let viewport = Size::new(900.0, 500.0);
        let origin = Point::new(700.0, 200.0);
        let s = bar_session(window, Some(origin), "r1");
        let p = bar_placement(&s, window, viewport).expect("锚点成立即落位");
        assert!(p.rect.x < 700.0, "越出右缘时向左收拢");
        assert!(
            p.rect.x + p.rect.width <= viewport.width - pal::VIEWPORT_GAP + 0.01,
            "估宽范围内整条不越出窗口"
        );
        let (sel_top, _) = sel_edges(&s, origin, 1);
        assert!(p.rect.y + p.rect.height <= sel_top, "上方有余量时贴在选区之上");
    }

    /// 选区首行上方贴不进网格区（再往上是本面板工具条与标签条）= 翻到选区下缘落位。
    #[test]
    fn format_bar_placement_flips_below_selection() {
        let window = iced::window::Id::unique();
        let viewport = Size::new(900.0, 500.0);
        let origin = Point::new(100.0, 200.0);
        let s = bar_session(window, Some(origin), "r0");
        let (_, sel_bottom) = sel_edges(&s, origin, 0);
        let p = bar_placement(&s, window, viewport).expect("上方无余量仍要落位");
        assert!(p.rect.y >= sel_bottom, "翻到选区下方");
        assert!(p.rect.y >= origin.y, "不越到网格区上方压工具条");
        assert!(p.rect.y + p.rect.height <= origin.y + s.view.canvas_size.height, "仍在网格区内");
    }

    /// 选区滚出网格下缘（下方无容身处）= 贴网格下缘，不越出去压宿主状态栏。
    #[test]
    fn format_bar_placement_clamps_to_canvas_bottom() {
        let window = iced::window::Id::unique();
        let viewport = Size::new(900.0, 500.0);
        let origin = Point::new(100.0, 200.0);
        let mut s = bar_session(window, Some(origin), "r1");
        s.view.canvas_size = Size::new(600.0, 40.0);
        s.view.scroll_y = 40.0;
        let p = bar_placement(&s, window, viewport).expect("选区滚出网格仍要落位");
        assert!(p.rect.y >= origin.y, "不越到网格区上方压工具条");
        assert!(p.rect.y + p.rect.height <= origin.y + s.view.canvas_size.height, "贴网格下缘不越出");
    }

    /// 锚点基准：画布原点未播种或来自别的窗口 = 落位不成立（气泡不弹）。
    #[test]
    fn format_bar_placement_requires_matching_canvas_origin() {
        let window = iced::window::Id::unique();
        let viewport = Size::new(900.0, 500.0);
        let s = bar_session(window, None, "r0");
        assert!(bar_placement(&s, window, viewport).is_none(), "画布未收到过事件即不弹");
        let s = bar_session(window, Some(Point::new(100.0, 200.0)), "r0");
        assert!(bar_placement(&s, iced::window::Id::unique(), viewport).is_none(), "换窗后旧原点不作数");
    }

    /// 面板内替掉网格的形态（编辑态 / 灯箱 / 插件表格面）期间不弹气泡：
    /// 气泡在窗口层，叠上去会盖过这些形态。
    #[test]
    fn format_bar_placement_hidden_by_covering_states() {
        let window = iced::window::Id::unique();
        let viewport = Size::new(900.0, 500.0);
        let origin = Some(Point::new(100.0, 200.0));
        let mut s = bar_session(window, origin, "r0");
        s.view.lightbox = Some(Lightbox {
            row_id: "r0".to_string(),
            field_id: "f1".to_string(),
            index: 0,
            zoom: 1.0,
            pan: Vector::ZERO,
            drag: None,
            last_press: None,
        });
        assert!(bar_placement(&s, window, viewport).is_none(), "灯箱预览期间不弹");
        let mut s = bar_session(window, origin, "r0");
        s.view.active_view = Some("plugin:view".to_string());
        assert!(bar_placement(&s, window, viewport).is_none(), "插件表格面在场时不弹");
    }

    fn rels(keys: &[&str]) -> HashSet<String> {
        keys.iter().map(|key| (*key).to_string()).collect()
    }

    #[test]
    fn pin_budget_evicts_oldest_outside_view() {
        let mut budget = PinBudget::default();
        let half = THUMB_PIN_BUDGET / 2;
        assert!(budget.insert("a", half, &rels(&["a"])).is_empty());
        assert!(budget.insert("b", half, &rels(&["b"])).is_empty());
        assert_eq!(budget.insert("c", 1, &rels(&["c"])), vec!["a".to_string()]);
    }

    #[test]
    fn pin_budget_keeps_viewed_entries_over_limit() {
        let mut budget = PinBudget::default();
        let half = THUMB_PIN_BUDGET / 2;
        budget.insert("a", half, &rels(&["a"]));
        budget.insert("b", half, &rels(&["b"]));
        // 可见区放不下：宁可超预算，也不让正在看的图重传
        assert!(budget.insert("c", THUMB_PIN_BUDGET, &rels(&["a", "b", "c"])).is_empty());
    }

    #[test]
    fn pin_budget_never_evicts_the_new_entry() {
        let mut budget = PinBudget::default();
        budget.insert("a", THUMB_PIN_BUDGET, &rels(&[]));
        assert_eq!(budget.insert("b", THUMB_PIN_BUDGET, &rels(&[])), vec!["a".to_string()]);
    }
}
